//! A lightweight in-memory [`NodeLike`] + [`NodeFactory`] implementation for use in tests.
//!
//! [`MockNode`] has no external dependencies and initializes instantly, making it
//! suitable for testing pool behaviour without spinning up any real process.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::pool::{NodeFactory, NodeLike, ResetStrategy};

static MOCK_NODE_ID: AtomicUsize = AtomicUsize::new(0);

/// Typed configuration returned by [`MockNode::config`].
#[derive(Clone, Default, PartialEq, Debug)]
pub struct MockNodeConfig {
    pub id: usize,
    pub connection_string: String,
}

#[derive(Default)]
struct MockNodeState {
    tables: HashSet<String>,
    values: HashMap<String, Vec<i32>>,
}

pub struct MockNode {
    id: usize,
    /// Set to `true` while checked out; cleared by `reset()`. Detects double-checkout.
    pub in_use: AtomicBool,
    state: Mutex<MockNodeState>,
}

impl MockNode {
    pub fn new() -> Self {
        Self {
            id: MOCK_NODE_ID.fetch_add(1, Ordering::Relaxed),
            in_use: AtomicBool::new(false),
            state: Mutex::new(MockNodeState::default()),
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
    type CONFIG = MockNodeConfig;

    fn connection_string(&self) -> String {
        format!("mock://node-{}", self.id)
    }

    fn config(&self) -> MockNodeConfig {
        MockNodeConfig {
            id: self.id,
            connection_string: self.connection_string(),
        }
    }

    async fn wait_for_ready(&mut self, _timeout: Duration) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reset(&mut self, strategy: &ResetStrategy) -> anyhow::Result<()> {
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

impl NodeFactory for MockNode {
    type Options = ();
    fn create(_opts: ()) -> Self {
        MockNode::new()
    }
}
