//! End-to-end workflow integration tests for `TimerTask`.
//!
//! Duration-based timers run under a paused clock (`#[tokio::test(start_paused = true)]`),
//! so virtual time auto-advances to the scheduled wake-up and the tests assert no real wall
//! time is spent. `Until`-based timers use tiny *real* instants without pausing the clock —
//! mixing a frozen tokio clock with the real `std::Instant` that `TimerOutcome::Until` carries
//! is fragile, so those tests stay on the real clock with short (tens-of-ms) deadlines.

use cano::prelude::*;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Wait,
    Process,
    Done,
}

// A plain follow-on task so timers route into a real subsequent state.
struct Process;

#[task(state = Step)]
impl Process {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Duration timer fires after the expected (virtual) delay
// ---------------------------------------------------------------------------

struct ThirtySecondTimer;

#[task::timer(state = Step)]
impl ThirtySecondTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::from_secs(30)))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Process))
    }
}

#[tokio::test(start_paused = true)]
async fn duration_timer_fires_after_expected_delay() {
    let start = tokio::time::Instant::now();

    let workflow = Workflow::bare()
        .register(Step::Wait, ThirtySecondTimer)
        .register(Step::Process, Process)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Wait).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result, Step::Done);
    // The engine schedules a single 30s sleep; virtual time advances exactly once.
    assert!(
        elapsed >= Duration::from_secs(30),
        "timer should wait the full duration, virtual elapsed = {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(31),
        "timer should wake exactly once, not loop, virtual elapsed = {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Instant timer fires at (around) the requested wall-clock instant
// ---------------------------------------------------------------------------

struct NearFutureInstantTimer;

#[task::timer(state = Step)]
impl NearFutureInstantTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Until(
            Instant::now() + Duration::from_millis(40),
        ))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Process))
    }
}

#[tokio::test]
async fn instant_timer_fires_at_correct_instant() {
    let start = Instant::now();

    let result = Workflow::bare()
        .register(Step::Wait, NearFutureInstantTimer)
        .register(Step::Process, Process)
        .add_exit_state(Step::Done)
        .orchestrate(Step::Wait)
        .await
        .unwrap();

    let elapsed = start.elapsed();
    assert_eq!(result, Step::Done);
    // `sleep_until` never fires early, so we waited at least (most of) the deadline.
    assert!(
        elapsed >= Duration::from_millis(30),
        "Until timer fired too early: {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// attempt_timeout cancels a long timer
// ---------------------------------------------------------------------------

struct OneHourTimer;

#[task::timer(state = Step)]
impl OneHourTimer {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(50))
    }
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::from_secs(3600)))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Process))
    }
}

#[tokio::test(start_paused = true)]
async fn attempt_timeout_cancels_long_timer() {
    // The 50ms attempt timeout fires before the 1h sleep; auto-advance jumps to the
    // earliest deadline (the timeout), producing CanoError::Timeout.
    let err = Workflow::bare()
        .register(Step::Wait, OneHourTimer)
        .register(Step::Process, Process)
        .add_exit_state(Step::Done)
        .orchestrate(Step::Wait)
        .await
        .unwrap_err();

    // The FSM wraps the failure with state context; `.inner()` peels one layer.
    assert!(
        matches!(err.inner(), CanoError::Timeout(_)),
        "expected CanoError::Timeout, got: {err:?}"
    );
}
