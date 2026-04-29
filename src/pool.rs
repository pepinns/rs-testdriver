//! A pool of pre-started nodes for use in integration tests.
//!
//! Spinning up a fresh [`Node`] per test is slow because initialization can be costly.
//! [`NodePool`] amortizes that cost by pre-starting N nodes and lending them to tests on
//! demand. A test calls [`NodePool::checkout`] to
//! receive a [`NodeGuard`]; when the guard is dropped the node is reset and returned to the
//! pool, ready for the next test. If all nodes are currently checked out, `checkout` blocks
//! until one becomes available.
//!
//! # Reset strategies
//!
//! [`ResetStrategy`] controls what happens to a node when it is returned to the pool:
//!
//! - [`ResetStrategy::FullReinit`] *(default)* — stop the node, wipe its state, and
//!   reinitialize from scratch. Each test gets a completely clean node. Slower but fully
//!   isolated.
//! - [`ResetStrategy::DropTables`] — keep the node running and drop only user-created
//!   tables/schemas. Faster, but retains other node-level state.
//! - [`ResetStrategy::None`] — return the node as-is. The caller is responsible for any
//!   cleanup. Use when tests manage their own transactions or truncations.

use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, OnceCell};

#[derive(Clone)]
pub enum ResetStrategy {
    /// Stop the node, wipe its state, and reinitialize from scratch. Fully isolated.
    FullReinit,
    /// Return node as-is; caller is responsible for any cleanup.
    None,
    /// Drop user-created tables/schemas and recreate them. Faster than full reinit.
    DropTables,
}

pub trait NodeLike: Send {
    type CONFIG: Default = ();

    fn connection_string(&self) -> String;
    fn wait_for_ready(
        &mut self,
        timeout: Duration,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
    fn reset(
        &mut self,
        strategy: &ResetStrategy,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
    fn config(&self) -> Self::CONFIG {
        Default::default()
    }
}

/// Extends [`NodeLike`] with the ability to construct a node from options.
/// Implementing this trait makes a type compatible with [`NodePoolBuilder`].
pub trait NodeFactory: NodeLike + Sized + 'static {
    type Options: Default + Clone;
    fn create(opts: Self::Options) -> Self;
}

pub struct NodePool<T: NodeLike> {
    inner: Arc<NodePoolInner<T>>,
}

impl<T: NodeLike> Clone for NodePool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct NodePoolInner<T: NodeLike> {
    sender: mpsc::Sender<T>,
    receiver: tokio::sync::Mutex<mpsc::Receiver<T>>,
    reset_strategy: Arc<ResetStrategy>,
}

pub struct NodeGuard<T: NodeLike + 'static> {
    node: Option<T>,
    sender: mpsc::Sender<T>,
    reset_strategy: Arc<ResetStrategy>,
}

impl<T: NodeLike + 'static> NodePool<T> {
    pub async fn new<F>(
        count: usize,
        factory: F,
        ready_timeout: Duration,
        reset_strategy: ResetStrategy,
    ) -> anyhow::Result<Self>
    where
        F: Fn() -> T,
    {
        let (sender, receiver) = mpsc::channel(count);

        for _ in 0..count {
            let mut node = factory();
            // New nodes are already clean — just wait until the node is ready.
            node.wait_for_ready(ready_timeout).await?;
            sender.send(node).await.expect("channel has capacity");
        }

        Ok(Self {
            inner: Arc::new(NodePoolInner {
                sender,
                receiver: tokio::sync::Mutex::new(receiver),
                reset_strategy: Arc::new(reset_strategy),
            }),
        })
    }

    pub async fn checkout(&self) -> NodeGuard<T> {
        let node = self
            .inner
            .receiver
            .lock()
            .await
            .recv()
            .await
            .expect("NodePool channel closed unexpectedly");

        NodeGuard {
            node: Some(node),
            sender: self.inner.sender.clone(),
            reset_strategy: Arc::clone(&self.inner.reset_strategy),
        }
    }
}

impl<T: NodeFactory> NodePool<T> {
    /// Start building a pool. Call `.start().await` when ready.
    pub fn with_count(count: usize) -> NodePoolBuilder<T> {
        NodePoolBuilder {
            count,
            opts: T::Options::default(),
            ready_timeout: Duration::from_secs(60),
            reset_strategy: ResetStrategy::FullReinit,
            _marker: std::marker::PhantomData,
        }
    }
}

pub struct NodePoolBuilder<T: NodeFactory> {
    count: usize,
    opts: T::Options,
    ready_timeout: Duration,
    reset_strategy: ResetStrategy,
    _marker: std::marker::PhantomData<T>,
}

impl<T: NodeFactory> NodePoolBuilder<T> {
    pub fn with_ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    pub fn with_reset_strategy(mut self, strategy: ResetStrategy) -> Self {
        self.reset_strategy = strategy;
        self
    }

    pub fn with_node_opts(mut self, opts: T::Options) -> Self {
        self.opts = opts;
        self
    }

    pub async fn start(self) -> anyhow::Result<NodePool<T>> {
        NodePool::new(
            self.count,
            || T::create(self.opts.clone()),
            self.ready_timeout,
            self.reset_strategy,
        )
        .await
    }
}

impl<T: NodeLike + 'static> NodeGuard<T> {
    pub fn node(&self) -> &T {
        self.node.as_ref().expect("NodeGuard already consumed")
    }

    pub fn node_mut(&mut self) -> &mut T {
        self.node.as_mut().expect("NodeGuard already consumed")
    }
}

impl<T: NodeLike + 'static> Drop for NodeGuard<T> {
    fn drop(&mut self) {
        if let Some(mut node) = self.node.take() {
            let sender = self.sender.clone();
            let strategy = Arc::clone(&self.reset_strategy);
            // tokio::spawn requires an active runtime; always present in #[tokio::test]
            tokio::spawn(async move {
                if let Err(e) = node.reset(&strategy).await {
                    tracing::warn!(err=?e, "NodeGuard: reset failed, node discarded from pool");
                    return;
                }
                let _ = sender.send(node).await;
            });
        }
    }
}

/// A lazily-initialized, process-wide [`NodePool`] intended for integration
/// tests. Declare one `static` per test binary and share it across all tests
/// to avoid paying node startup cost more than once.
///
/// # Example
///
/// ```rust,ignore
/// use testdriver::{NodePool, ResetStrategy, SharedPool, pool_size_from_env};
///
/// static POOL: SharedPool<MyNode> = SharedPool::new();
///
/// async fn get_pool() -> &'static NodePool<MyNode> {
///     POOL.get_or_init(|| {
///         NodePool::<MyNode>::with_count(pool_size_from_env("MY_TEST_POOL_SIZE", 3))
///             .with_reset_strategy(ResetStrategy::DropTables)
///             .start()
///     }).await
/// }
/// ```
pub struct SharedPool<T: NodeFactory> {
    cell: OnceCell<NodePool<T>>,
}

impl<T: NodeFactory> SharedPool<T> {
    /// Create a new, uninitialized `SharedPool`. Suitable for `static` context.
    pub const fn new() -> Self {
        Self {
            cell: OnceCell::const_new(),
        }
    }

    /// Returns the shared pool, initializing it on the first call.
    /// Panics if `init` returns an error.
    pub async fn get_or_init<F, Fut>(&'static self, init: F) -> &'static NodePool<T>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = anyhow::Result<NodePool<T>>> + Send,
    {
        self.cell
            .get_or_init(
                || async move { init().await.expect("SharedPool: failed to initialize pool") },
            )
            .await
    }
}

/// Returns a pool size from an environment variable, falling back to `default`.
pub fn pool_size_from_env(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_node::MockNode;
    use std::sync::atomic::Ordering;

    // Demonstrates the CONFIG associated type: typed connection details are
    // available from the checked-out node without parsing a connection string.
    #[tokio::test]
    async fn test_node_config_accessible_after_checkout() {
        let pool = NodePool::<MockNode>::with_count(1)
            .start()
            .await
            .expect("should create pool");

        let guard = pool.checkout().await;
        let cfg = guard.node().config();
        assert_eq!(cfg.connection_string, guard.node().connection_string());
    }

    #[tokio::test]
    async fn test_nodegroup_can_checkout_and_use_node() {
        let pool = NodePool::<MockNode>::with_count(1)
            .start()
            .await
            .expect("should create pool");

        let guard = pool.checkout().await;
        guard.node().create_table("smoke_test");
        assert!(guard.node().has_table("smoke_test"));
    }

    #[tokio::test]
    async fn test_nodegroup_fullreinit_clears_data_on_return() {
        let pool = NodePool::<MockNode>::with_count(1)
            .start()
            .await
            .expect("should create pool");

        {
            let guard = pool.checkout().await;
            guard.node().create_table("leftover");
        }

        let guard = pool.checkout().await;
        assert!(
            !guard.node().has_table("leftover"),
            "table should not exist after FullReinit"
        );
    }

    #[tokio::test]
    async fn test_nodegroup_droptables_clears_data_on_return() {
        let pool = NodePool::<MockNode>::with_count(1)
            .with_reset_strategy(ResetStrategy::DropTables)
            .start()
            .await
            .expect("should create pool");

        {
            let guard = pool.checkout().await;
            guard.node().create_table("leftover");
        }

        let guard = pool.checkout().await;
        assert!(
            !guard.node().has_table("leftover"),
            "table should not exist after DropTables"
        );
    }

    #[tokio::test]
    async fn test_nodegroup_none_preserves_data_on_return() {
        let pool = NodePool::<MockNode>::with_count(1)
            .with_reset_strategy(ResetStrategy::None)
            .start()
            .await
            .expect("should create pool");

        {
            let guard = pool.checkout().await;
            guard.node().create_table("preserved");
            guard.node().insert("preserved", 1);
        }

        let guard = pool.checkout().await;
        assert!(
            guard.node().has_table("preserved"),
            "table should persist after None reset"
        );
        assert_eq!(guard.node().get_values("preserved"), vec![1]);
    }

    #[tokio::test]
    async fn test_nodegroup_checkout_blocks_when_pool_exhausted() {
        let pool = NodePool::<MockNode>::with_count(2)
            .start()
            .await
            .expect("should create pool");

        let guard1 = pool.checkout().await;
        let guard2 = pool.checkout().await;

        let pool_clone = pool.clone();
        let blocked = tokio::time::timeout(Duration::from_millis(500), pool_clone.checkout()).await;
        assert!(
            blocked.is_err(),
            "checkout should block while pool is exhausted"
        );

        drop(guard2);

        let _guard3 = tokio::time::timeout(Duration::from_secs(60), pool.checkout())
            .await
            .expect("checkout should succeed after a node is returned to the pool");

        drop(guard1);
        drop(_guard3);
    }

    // N tasks all racing to checkout from a pool smaller than N.
    // Verifies no double-checkout (each task gets exclusive access) and that
    // every task eventually succeeds — no nodes are lost or deadlocked.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_checkouts_no_double_borrow() {
        const POOL_SIZE: usize = 3;
        const TASKS: usize = 12;

        let pool = NodePool::<MockNode>::with_count(POOL_SIZE)
            .with_reset_strategy(ResetStrategy::None)
            .start()
            .await
            .expect("should create pool");

        let barrier = Arc::new(tokio::sync::Barrier::new(TASKS));
        let mut handles = Vec::with_capacity(TASKS);

        for _ in 0..TASKS {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let guard = tokio::time::timeout(Duration::from_secs(120), pool.checkout())
                    .await
                    .expect("checkout timed out");

                let was_in_use = guard.node().in_use.swap(true, Ordering::SeqCst);
                assert!(
                    !was_in_use,
                    "node was already in use — double-checkout detected"
                );

                tokio::time::sleep(Duration::from_millis(10)).await;
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }
    }

    // Many tasks check in and out in tight interleaved loops. After all tasks
    // finish we drain the pool and verify exactly POOL_SIZE nodes come back —
    // none lost, none duplicated.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_checkin_checkout_pool_count_stable() {
        const POOL_SIZE: usize = 3;
        const TASKS: usize = 9;
        const ROUNDS: usize = 3;

        let pool = NodePool::<MockNode>::with_count(POOL_SIZE)
            .with_reset_strategy(ResetStrategy::None)
            .start()
            .await
            .expect("should create pool");

        let mut handles = Vec::with_capacity(TASKS);
        for _ in 0..TASKS {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..ROUNDS {
                    let _guard = tokio::time::timeout(Duration::from_secs(120), pool.checkout())
                        .await
                        .expect("checkout timed out");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }

        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut guards = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let g = tokio::time::timeout(Duration::from_secs(5), pool.checkout())
                .await
                .expect("should drain pool node without blocking");
            guards.push(g);
        }
        let overflow = tokio::time::timeout(Duration::from_millis(200), pool.checkout()).await;
        assert!(
            overflow.is_err(),
            "pool should be empty after draining {POOL_SIZE} nodes"
        );
    }

    // The pool is dropped while a checked-out guard is still alive. When the
    // guard is later dropped the background reset task should complete without
    // panicking even though the pool's receiver is gone (the send just fails
    // silently and the node is discarded).
    #[tokio::test]
    async fn test_guard_dropped_after_pool_dropped_does_not_panic() {
        let pool = NodePool::<MockNode>::with_count(1)
            .with_reset_strategy(ResetStrategy::None)
            .start()
            .await
            .expect("should create pool");

        let guard = pool.checkout().await;
        drop(pool);

        drop(guard);

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    static TEST_SHARED_POOL: SharedPool<MockNode> = SharedPool::new();

    #[tokio::test]
    async fn test_shared_pool_returns_a_working_pool() {
        let pool = TEST_SHARED_POOL
            .get_or_init(|| NodePool::<MockNode>::with_count(1).start())
            .await;

        let guard = pool.checkout().await;
        guard.node().create_table("shared_smoke");
        assert!(guard.node().has_table("shared_smoke"));
    }

    #[tokio::test]
    async fn test_shared_pool_is_same_instance_on_repeated_calls() {
        let p1 = TEST_SHARED_POOL
            .get_or_init(|| NodePool::<MockNode>::with_count(1).start())
            .await;
        let p2 = TEST_SHARED_POOL
            .get_or_init(|| NodePool::<MockNode>::with_count(1).start())
            .await;
        assert!(
            std::ptr::eq(p1, p2),
            "get_or_init should return the same pool instance on repeated calls"
        );
    }

    #[test]
    fn test_pool_size_from_env_uses_default_when_var_absent() {
        // Use a name unlikely to be set in CI.
        std::env::remove_var("RS_TESTDRIVER_POOL_SIZE_ABSENT");
        assert_eq!(pool_size_from_env("RS_TESTDRIVER_POOL_SIZE_ABSENT", 7), 7);
    }

    #[test]
    fn test_pool_size_from_env_reads_var_when_set() {
        std::env::set_var("RS_TESTDRIVER_POOL_SIZE_SET", "5");
        assert_eq!(pool_size_from_env("RS_TESTDRIVER_POOL_SIZE_SET", 1), 5);
        std::env::remove_var("RS_TESTDRIVER_POOL_SIZE_SET");
    }

    #[test]
    fn test_pool_size_from_env_uses_default_for_invalid_value() {
        std::env::set_var("RS_TESTDRIVER_POOL_SIZE_BAD", "not_a_number");
        assert_eq!(pool_size_from_env("RS_TESTDRIVER_POOL_SIZE_BAD", 3), 3);
        std::env::remove_var("RS_TESTDRIVER_POOL_SIZE_BAD");
    }
}
