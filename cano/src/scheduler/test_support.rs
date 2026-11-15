//! Shared fixtures for the scheduler module's unit tests: a trivial counting
//! task and a single-state workflow built from it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cano_macros::task;

use crate::error::CanoError;
use crate::task::{Task, TaskResult};
use crate::workflow::Workflow;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TestState {
    Start,
    Complete,
    Error,
}

#[derive(Clone)]
pub(crate) struct TestTask {
    pub(crate) execution_count: Arc<AtomicU32>,
}

impl TestTask {
    pub(crate) fn new() -> Self {
        Self {
            execution_count: Arc::new(AtomicU32::new(0)),
        }
    }
}

#[task]
impl Task<TestState> for TestTask {
    async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
        self.execution_count.fetch_add(1, Ordering::Relaxed);
        Ok(TaskResult::Single(TestState::Complete))
    }
}

pub(crate) fn create_test_workflow() -> Workflow<TestState> {
    Workflow::bare()
        .register(TestState::Start, TestTask::new())
        .add_exit_state(TestState::Complete)
        .add_exit_state(TestState::Error)
}
