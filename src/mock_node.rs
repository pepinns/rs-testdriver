//! A lightweight in-memory [`NodeLike`] + [`NodeFactory`] implementation for use in tests.
//!
//! [`MockNode`] has no external dependencies and initializes instantly. It stores
//! tables and values in memory, making it suitable for verifying pool behaviour
//! (reset strategies, concurrent checkout, pool draining, etc.) without spinning
//! up any real process.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::pool::{NodeFactory, NodeLike, ResetStrategy};

static MOCK_NODE_ID: AtomicUsize = AtomicUsize::new(0);

struct MockNodeState {
    tables: HashSet<String>,
    values: HashMap<String, Vec<i32>>,
}

impl MockNodeState {
    fn new() -> Self {
        Self {
            tables: HashSet::new(),
            values: HashMap::new(),
        }
    }
}

pub struct MockNode {
    id: usize,
    /// Toggled true by a test when it holds the node; `reset()` always clears it.
    /// Used to detect concurrent double-checkout of the same node.
    pub in_use: Arc<AtomicBool>,
    state: Arc<Mutex<MockNodeState>>,
}

impl MockNode {
    pub fn new() -> Self {
        Self {
            id: MOCK_NODE_ID.fetch_add(1, Ordering::Relaxed),
            in_use: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(MockNodeState::new())),
        }
    }

    pub fn create_table(&self, name: &str) {
        self.state.lock().unwrap().tables.insert(name.to_string());
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.state.lock().unwrap().tables.contains(name)
    }

    pub fn insert(&self, table: &str, val: i32) {
        self.state
            .lock()
            .unwrap()
            .values
            .entry(table.to_string())
            .or_default()
            .push(val);
    }

    pub fn get_values(&self, table: &str) -> Vec<i32> {
        self.state
            .lock()
            .unwrap()
            .values
            .get(table)
            .cloned()
            .unwrap_or_default()
    }
}

impl NodeLike for MockNode {
    fn connection_string(&self) -> String {
        format!("mock://node-{}", self.id)
    }

    async fn wait_for_ready(&mut self, _timeout: Duration) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reset(&mut self, strategy: &ResetStrategy) -> anyhow::Result<()> {
        // Always clear the exclusivity flag regardless of strategy.
        self.in_use.store(false, Ordering::SeqCst);
        let mut state = self.state.lock().unwrap();
        match strategy {
            ResetStrategy::FullReinit | ResetStrategy::DropTables => {
                state.tables.clear();
                state.values.clear();
            }
            ResetStrategy::None => {}
        }
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct MockNodeOptions;

impl NodeFactory for MockNode {
    type Options = MockNodeOptions;
    fn create(_opts: Self::Options) -> Self {
        MockNode::new()
    }
}
