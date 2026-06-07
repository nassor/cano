//! State-coverage assertions (`assert_all_states_entered` /
//! `assert_registered_states_entered`) exercised across router hops, dead states, and
//! explicit state lists.
//!
//! Requires the `testing` feature.
#![cfg(feature = "testing")]

use cano::prelude::*;
use cano::testing::RecordingObserver;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum S {
    Start,
    Route,
    Worker,
    Orphan,
    Done,
}

#[derive(Clone)]
struct Go(S);
#[task]
impl Task<S> for Go {
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        Ok(TaskResult::Single(self.0.clone()))
    }
}

struct RouteTo(S);
#[task::router]
impl RouterTask<S> for RouteTo {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn route(&self, _res: &Resources) -> Result<TaskResult<S>, CanoError> {
        Ok(TaskResult::Single(self.0.clone()))
    }
}

#[tokio::test]
async fn router_hop_is_counted() {
    let observer = Arc::new(RecordingObserver::new());
    let wf = Workflow::bare()
        .register_router(S::Start, RouteTo(S::Route))
        .register_router(S::Route, RouteTo(S::Worker))
        .register(S::Worker, Go(S::Done))
        .add_exit_state(S::Done)
        .with_observer(observer.clone());
    wf.orchestrate(S::Start, CancellationToken::disabled())
        .await
        .unwrap();
    observer
        .assert_registered_states_entered(&wf)
        .expect("router hops counted");
}

#[tokio::test]
async fn unregistered_routed_state_returns_err_does_not_panic() {
    let observer = Arc::new(RecordingObserver::new());
    let wf = Workflow::bare()
        .register(S::Start, Go(S::Done))
        .register(S::Orphan, Go(S::Done))
        .add_exit_state(S::Done)
        .with_observer(observer.clone());
    wf.orchestrate(S::Start, CancellationToken::disabled())
        .await
        .unwrap();
    let missing = observer.assert_registered_states_entered(&wf).unwrap_err();
    assert!(missing.contains(&"Orphan".to_string()), "got: {missing:?}");
}

#[tokio::test]
async fn multi_state_with_explicit_list() {
    let observer = Arc::new(RecordingObserver::new());
    let wf = Workflow::bare()
        .register(S::Start, Go(S::Done))
        .add_exit_state(S::Done)
        .with_observer(observer.clone());
    wf.orchestrate(S::Start, CancellationToken::disabled())
        .await
        .unwrap();
    let missing = observer
        .assert_all_states_entered(&[S::Start, S::Route, S::Worker])
        .unwrap_err();
    assert_eq!(missing, vec!["Route".to_string(), "Worker".to_string()]);
}
