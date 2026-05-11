//! Shared fixtures for the scheduler module's unit tests: a trivial counting
//! node and helpers that build ok / failing single-state workflows from it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::error::{CanoError, CanoResult};
use crate::resource::Resources;
use crate::task;
use crate::task::node::Node;
use crate::workflow::Workflow;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TestState {
    Start,
    Complete,
    Error,
}

#[derive(Clone)]
pub(crate) struct TestNode {
    pub(crate) execution_count: Arc<AtomicU32>,
    should_fail: bool,
}

impl TestNode {
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

#[task::node]
impl Node<TestState> for TestNode {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _res: &Resources) -> CanoResult<()> {
        Ok(())
    }

    async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
        self.execution_count.fetch_add(1, Ordering::Relaxed);
    }

    async fn post(&self, _res: &Resources, _exec_res: Self::ExecResult) -> CanoResult<TestState> {
        if self.should_fail {
            Err(CanoError::NodeExecution("Test failure".to_string()))
        } else {
            Ok(TestState::Complete)
        }
    }
}

pub(crate) fn create_test_workflow() -> Workflow<TestState> {
    Workflow::bare()
        .register(TestState::Start, TestNode::new())
        .add_exit_state(TestState::Complete)
        .add_exit_state(TestState::Error)
}

pub(crate) fn create_failing_workflow() -> Workflow<TestState> {
    Workflow::bare()
        .register(TestState::Start, TestNode::new_failing())
        .add_exit_state(TestState::Complete)
        .add_exit_state(TestState::Error)
}
