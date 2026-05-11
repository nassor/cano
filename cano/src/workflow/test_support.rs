//! Shared task fixtures for the `workflow` module's unit tests.
//!
//! `TestState` and the three trivial tasks below are used by the test modules in
//! `workflow.rs`, `workflow/execution.rs` and `workflow/compensation.rs`. They
//! live here so each of those `#[cfg(test)] mod tests` can `use` them rather than
//! redefining the same fixtures three times.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cano_macros::task;

use crate::error::CanoError;
use crate::resource::Resources;
use crate::store::MemoryStore;
use crate::task::{Task, TaskResult};

/// Test workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TestState {
    Start,
    Process,
    Split,
    Join,
    Complete,
    #[allow(dead_code)]
    Error,
}

/// Simple task that returns a single state
#[derive(Clone)]
pub(crate) struct SimpleTask {
    next_state: TestState,
    counter: Arc<AtomicU32>,
}

impl SimpleTask {
    pub(crate) fn new(next_state: TestState) -> Self {
        Self {
            next_state,
            counter: Arc::new(AtomicU32::new(0)),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn count(&self) -> u32 {
        self.counter.load(Ordering::SeqCst)
    }
}

#[task]
impl Task<TestState> for SimpleTask {
    async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

/// Task that stores data using a `MemoryStore` from resources
#[derive(Clone)]
pub(crate) struct DataTask {
    key: String,
    value: String,
    next_state: TestState,
}

impl DataTask {
    pub(crate) fn new(key: &str, value: &str, next_state: TestState) -> Self {
        Self {
            key: key.to_string(),
            value: value.to_string(),
            next_state,
        }
    }
}

#[task]
impl Task<TestState> for DataTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        let store: Arc<MemoryStore> = res.get("store")?;
        store.put(&self.key, self.value.clone())?;
        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

/// Task that fails on demand
#[derive(Clone)]
pub(crate) struct FailTask {
    should_fail: bool,
}

impl FailTask {
    pub(crate) fn new(should_fail: bool) -> Self {
        Self { should_fail }
    }
}

#[task]
impl Task<TestState> for FailTask {
    async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
        if self.should_fail {
            Err(CanoError::task_execution("Task intentionally failed"))
        } else {
            Ok(TaskResult::Single(TestState::Complete))
        }
    }
}
