//! Shared fixtures for the scheduler module's unit tests: a trivial counting
//! task and helpers that build ok / failing single-state workflows from it.

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
    should_fail: bool,
}

impl TestTask {
    pub(crate) fn new() -> Self {
        Self {
            execution_count: Arc::new(AtomicU32::new(0)),
            should_fail: false,
        }
    }

    pub(crate) fn new_failing() -> Self {
        Self {
            execution_count: Arc::new(AtomicU32::new(0)),
            should_fail: true,
        }
    }
}

#[task]
impl Task<TestState> for TestTask {
    async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
        self.execution_count.fetch_add(1, Ordering::Relaxed);
        if self.should_fail {
            Err(CanoError::TaskExecution("Test failure".to_string()))
        } else {
            Ok(TaskResult::Single(TestState::Complete))
        }
    }
}

pub(crate) fn create_test_workflow() -> Workflow<TestState> {
    Workflow::bare()
        .register(TestState::Start, TestTask::new())
        .add_exit_state(TestState::Complete)
        .add_exit_state(TestState::Error)
}

pub(crate) fn create_failing_workflow() -> Workflow<TestState> {
    Workflow::bare()
        .register(TestState::Start, TestTask::new_failing())
        .add_exit_state(TestState::Complete)
        .add_exit_state(TestState::Error)
}
