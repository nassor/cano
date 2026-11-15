//! Integration tests for `#[cano::task::timer]` — both the inherent form (which emits
//! `::cano::` paths) and the trait-impl form. These run outside the `cano` crate so
//! `::cano::` paths resolve correctly.

use cano::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Wait,
    Next,
    Done,
}

// ---------------------------------------------------------------------------
// Inherent form: `#[task::timer(state = S)] impl T { wait + after_wait }`
// ---------------------------------------------------------------------------

struct InherentTimer;

#[task::timer(state = Step)]
impl InherentTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::from_millis(30)))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn inherent_timer_has_default_minimal_config() {
    assert_eq!(
        TimerTask::<Step>::config(&InherentTimer)
            .retry_mode
            .max_attempts(),
        1,
        "TimerTask default config must be minimal (no retries)"
    );
}

#[test]
fn inherent_timer_has_default_name() {
    assert!(
        TimerTask::<Step>::name(&InherentTimer).contains("InherentTimer"),
        "default name should contain the type name"
    );
}

#[tokio::test(start_paused = true)]
async fn inherent_timer_duration_waits_then_transitions() {
    // Under the paused clock, the runtime auto-advances virtual time to the next
    // timer, so the 30ms sleep completes without real waiting — and virtual elapsed
    // time reflects the requested duration.
    let start = tokio::time::Instant::now();
    let res = Resources::new();
    let result = Task::run(&InherentTimer, &res).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result, TaskResult::Single(Step::Done));
    assert!(
        elapsed >= Duration::from_millis(30),
        "timer should sleep at least the requested duration, virtual elapsed = {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Inherent form: Until a past instant completes immediately
// ---------------------------------------------------------------------------

struct PastInstantTimer;

#[task::timer(state = Step)]
impl PastInstantTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Until(
            Instant::now() - Duration::from_secs(60),
        ))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn inherent_timer_until_past_instant_completes_immediately() {
    let res = Resources::new();
    let start = Instant::now();
    let result = Task::run(&PastInstantTimer, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "a past Until instant must not sleep"
    );
}

// ---------------------------------------------------------------------------
// after_wait can return Split
// ---------------------------------------------------------------------------

struct SplitTimer;

#[task::timer(state = Step)]
impl SplitTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::ZERO))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Split(vec![Step::Wait, Step::Next]))
    }
}

#[tokio::test]
async fn inherent_timer_after_wait_transition() {
    let res = Resources::new();
    let result = Task::run(&SplitTimer, &res).await.unwrap();
    assert_eq!(result, TaskResult::Split(vec![Step::Wait, Step::Next]));
}

// ---------------------------------------------------------------------------
// Inherent form with config/name overrides
// ---------------------------------------------------------------------------

struct InherentCustomTimer;

#[task::timer(state = Step)]
impl InherentCustomTimer {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal().with_attempt_timeout(Duration::from_secs(5))
    }
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("my-inherent-timer")
    }
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::ZERO))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn inherent_custom_config_override() {
    let cfg = TimerTask::<Step>::config(&InherentCustomTimer);
    assert_eq!(cfg.retry_mode.max_attempts(), 1);
    assert_eq!(cfg.attempt_timeout, Some(Duration::from_secs(5)));
}

#[test]
fn inherent_custom_name_override() {
    assert_eq!(
        TimerTask::<Step>::name(&InherentCustomTimer),
        "my-inherent-timer"
    );
}

#[test]
fn inherent_companion_task_forwards_config() {
    let cfg = Task::config(&InherentCustomTimer);
    assert_eq!(cfg.attempt_timeout, Some(Duration::from_secs(5)));
}

#[test]
fn inherent_companion_task_forwards_name() {
    assert_eq!(Task::name(&InherentCustomTimer), "my-inherent-timer");
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[task::timer] impl TimerTask<S> for T { ... }`
// ---------------------------------------------------------------------------

struct TraitTimer;

#[task::timer]
impl TimerTask<Step> for TraitTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::ZERO))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn trait_form_timer_works() {
    let res = Resources::new();
    let result = Task::run(&TraitTimer, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// ---------------------------------------------------------------------------
// Dynamic dispatch: cast to Arc<dyn Task<Step>>
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inherent_timer_as_dyn_task() {
    let timer: Arc<dyn Task<Step>> = Arc::new(SplitTimer);
    let res = Resources::new();
    let result = Task::run(timer.as_ref(), &res).await.unwrap();
    assert_eq!(result, TaskResult::Split(vec![Step::Wait, Step::Next]));
}

// ---------------------------------------------------------------------------
// Workflow integration via register
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inherent_timer_integrates_with_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Wait, TraitTimer)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Wait).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ---------------------------------------------------------------------------
// Error propagation from wait() / after_wait()
// ---------------------------------------------------------------------------

struct WaitErrorTimer;

#[task::timer(state = Step)]
impl WaitErrorTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Err(CanoError::task_execution("wait failed deliberately"))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn wait_error_propagates_from_task_run() {
    let res = Resources::new();
    let err = Task::run(&WaitErrorTimer, &res).await.unwrap_err();
    assert!(matches!(err, CanoError::TaskExecution(_)));
}

struct AfterWaitErrorTimer;

#[task::timer(state = Step)]
impl AfterWaitErrorTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::ZERO))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Err(CanoError::task_execution("after_wait failed deliberately"))
    }
}

#[tokio::test]
async fn after_wait_error_propagates_from_task_run() {
    let res = Resources::new();
    let err = Task::run(&AfterWaitErrorTimer, &res).await.unwrap_err();
    assert!(matches!(err, CanoError::TaskExecution(_)));
}
