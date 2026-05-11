//! Integration tests for `#[cano::task::poll]` — both the inherent form (which emits
//! `::cano::` paths) and the trait-impl form. These run outside the `cano` crate so
//! `::cano::` paths resolve correctly.

use cano::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Poll,
    Next,
    Done,
}

// ---------------------------------------------------------------------------
// Inherent form: `#[poll_task(state = S)] impl T { async fn poll(...) }`
// ---------------------------------------------------------------------------

struct InherentPoller;

#[task::poll(state = Step)]
impl InherentPoller {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        Ok(PollOutcome::Ready(TaskResult::Single(Step::Done)))
    }
}

#[test]
fn inherent_poller_has_default_minimal_config() {
    assert_eq!(
        PollTask::<Step>::config(&InherentPoller)
            .retry_mode
            .max_attempts(),
        1,
        "PollTask default config must be minimal (no retries)"
    );
}

#[test]
fn inherent_poller_has_default_name() {
    assert!(
        PollTask::<Step>::name(&InherentPoller).contains("InherentPoller"),
        "default name should contain the type name"
    );
}

#[tokio::test]
async fn inherent_poller_poll_works() {
    let res = Resources::new();
    let result = PollTask::poll(&InherentPoller, &res).await.unwrap();
    assert_eq!(result, PollOutcome::Ready(TaskResult::Single(Step::Done)));
}

#[tokio::test]
async fn inherent_poller_task_run_works() {
    let res = Resources::new();
    let result = Task::run(&InherentPoller, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// ---------------------------------------------------------------------------
// Inherent form with config/name overrides
// ---------------------------------------------------------------------------

struct InherentCustomPoller;

#[task::poll(state = Step)]
impl InherentCustomPoller {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("my-inherent-poller")
    }

    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        Ok(PollOutcome::Ready(TaskResult::Single(Step::Done)))
    }
}

#[test]
fn inherent_custom_config_override() {
    assert_eq!(
        PollTask::<Step>::config(&InherentCustomPoller)
            .retry_mode
            .max_attempts(),
        1
    );
}

#[test]
fn inherent_custom_name_override() {
    assert_eq!(
        PollTask::<Step>::name(&InherentCustomPoller),
        "my-inherent-poller"
    );
}

#[test]
fn inherent_companion_task_forwards_config() {
    assert_eq!(
        Task::config(&InherentCustomPoller)
            .retry_mode
            .max_attempts(),
        1
    );
}

#[test]
fn inherent_companion_task_forwards_name() {
    assert_eq!(Task::name(&InherentCustomPoller), "my-inherent-poller");
}

// ---------------------------------------------------------------------------
// Inherent form with multiple polls (Pending → Ready)
// ---------------------------------------------------------------------------

struct InherentCountingPoller {
    target: u32,
    count: AtomicU32,
}

#[task::poll(state = Step)]
impl InherentCountingPoller {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= self.target {
            Ok(PollOutcome::Ready(TaskResult::Single(Step::Done)))
        } else {
            Ok(PollOutcome::Pending { delay_ms: 0 })
        }
    }
}

#[tokio::test]
async fn inherent_poller_multiple_polls() {
    let poller = InherentCountingPoller {
        target: 3,
        count: AtomicU32::new(0),
    };
    let res = Resources::new();
    let result = Task::run(&poller, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
    assert_eq!(poller.count.load(Ordering::Relaxed), 3);
}

// ---------------------------------------------------------------------------
// Inherent form returning Split
// ---------------------------------------------------------------------------

struct InherentSplitPoller;

#[task::poll(state = Step)]
impl InherentSplitPoller {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        Ok(PollOutcome::Ready(TaskResult::Split(vec![
            Step::Poll,
            Step::Next,
        ])))
    }
}

#[tokio::test]
async fn inherent_poller_can_return_split() {
    let res = Resources::new();
    let result = Task::run(&InherentSplitPoller, &res).await.unwrap();
    assert_eq!(result, TaskResult::Split(vec![Step::Poll, Step::Next]));
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[task::poll] impl PollTask<S> for T { ... }`
// ---------------------------------------------------------------------------

struct TraitPoller;

#[task::poll]
impl PollTask<Step> for TraitPoller {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        Ok(PollOutcome::Ready(TaskResult::Single(Step::Done)))
    }
}

#[tokio::test]
async fn trait_form_poll_works() {
    let res = Resources::new();
    let result = PollTask::poll(&TraitPoller, &res).await.unwrap();
    assert_eq!(result, PollOutcome::Ready(TaskResult::Single(Step::Done)));
}

#[tokio::test]
async fn trait_form_companion_task_run_works() {
    let res = Resources::new();
    let result = Task::run(&TraitPoller, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// ---------------------------------------------------------------------------
// Dynamic dispatch: cast to Arc<dyn Task<Step>>
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inherent_poller_as_dyn_task() {
    let poller: Arc<dyn Task<Step>> = Arc::new(InherentPoller);
    let res = Resources::new();
    let result = Task::run(poller.as_ref(), &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// ---------------------------------------------------------------------------
// Workflow integration: register via Workflow::register
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inherent_poller_integrates_with_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Poll, InherentPoller)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Poll).await.unwrap();
    assert_eq!(result, Step::Done);
}

#[tokio::test]
async fn trait_poller_integrates_with_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Poll, TraitPoller)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Poll).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ---------------------------------------------------------------------------
// Error propagation from poll()
// ---------------------------------------------------------------------------

struct ErrorPoller;

#[task::poll(state = Step)]
impl ErrorPoller {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        Err(CanoError::task_execution("poll failed deliberately"))
    }
}

#[tokio::test]
async fn poll_error_propagates_from_task_run() {
    let res = Resources::new();
    let err = Task::run(&ErrorPoller, &res).await.unwrap_err();
    assert!(matches!(err, CanoError::TaskExecution(_)));
}

// ---------------------------------------------------------------------------
// on_poll_error: default is FailFast (inherent form injection)
// ---------------------------------------------------------------------------

#[test]
fn inherent_poller_default_on_poll_error_is_fail_fast() {
    // InherentPoller does not define on_poll_error — macro should inject the default.
    assert_eq!(
        PollTask::<Step>::on_poll_error(&InherentPoller),
        PollErrorPolicy::FailFast,
    );
}

// ---------------------------------------------------------------------------
// on_poll_error: user override is forwarded (inherent form)
// ---------------------------------------------------------------------------

struct InherentResilientPoller {
    count: AtomicU32,
    errors_before_ready: u32,
}

#[task::poll(state = Step)]
impl InherentResilientPoller {
    fn on_poll_error(&self) -> PollErrorPolicy {
        PollErrorPolicy::RetryOnError { max_errors: 2 }
    }

    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        if n < self.errors_before_ready {
            Err(CanoError::task_execution("transient"))
        } else {
            Ok(PollOutcome::Ready(TaskResult::Single(Step::Done)))
        }
    }
}

#[test]
fn inherent_on_poll_error_override_forwarded() {
    let poller = InherentResilientPoller {
        count: AtomicU32::new(0),
        errors_before_ready: 0,
    };
    assert_eq!(
        PollTask::<Step>::on_poll_error(&poller),
        PollErrorPolicy::RetryOnError { max_errors: 2 },
    );
}

#[tokio::test]
async fn inherent_retry_on_error_within_budget_succeeds() {
    // 2 errors then Ready — max_errors = 2 so both errors are tolerated.
    let poller = InherentResilientPoller {
        count: AtomicU32::new(0),
        errors_before_ready: 2,
    };
    let res = Resources::new();
    let result = Task::run(&poller, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
    assert_eq!(poller.count.load(Ordering::Relaxed), 3); // 2 errors + 1 Ready
}
