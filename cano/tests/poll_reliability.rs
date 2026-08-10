//! Reliability contracts of [`PollTask`] under critical scenarios.
//!
//! Black-box integration tests (public API only) documenting how the wait-until
//! processing model behaves against a flaky or dead dependency:
//!
//! | Critical scenario | Contract |
//! |-------------------|----------|
//! | flaky dependency under `RetryOnError` | budget counts *consecutive* errors; `Pending` resets it; budget+1 kills the run |
//! | condition never becomes true | `attempt_timeout` bounds the otherwise-infinite loop |

mod support;

use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use support::Flow;

// ===========================================================================
// Consecutive-error budget, reset by Pending
// ===========================================================================

#[derive(Clone, Copy)]
enum Probe {
    Fail,
    Pending,
    Ready,
}

/// Replays a fixed script, one entry per `poll` call.
struct ScriptedProbe {
    script: Vec<Probe>,
    calls: Arc<AtomicU32>,
}

#[task::poll]
impl PollTask<Flow> for ScriptedProbe {
    fn on_poll_error(&self) -> PollErrorPolicy {
        PollErrorPolicy::RetryOnError { max_errors: 2 }
    }

    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Flow>, CanoError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) as usize;
        match self.script.get(n).copied().unwrap_or(Probe::Ready) {
            Probe::Fail => Err(CanoError::task_execution(format!("probe {n} refused"))),
            Probe::Pending => Ok(PollOutcome::Pending { delay_ms: 0 }),
            Probe::Ready => Ok(PollOutcome::Ready(TaskResult::Single(Flow::Done))),
        }
    }
}

/// Contract: unlike the stream policy, poll's `RetryOnError` really does re-ask
/// (the condition is re-checked next iteration). The budget counts *consecutive*
/// errors only — a successful `Pending` resets it — and the run dies when a burst
/// reaches budget+1, surfacing the underlying error unwrapped (poll defaults to
/// no outer retry).
#[tokio::test]
async fn poll_error_budget_resets_on_pending_and_kills_on_burst() {
    // Two bursts of exactly max_errors=2, separated by a Pending: survives.
    let calls = Arc::new(AtomicU32::new(0));
    let workflow = Workflow::bare()
        .register(
            Flow::Work,
            ScriptedProbe {
                script: vec![
                    Probe::Fail,
                    Probe::Fail,
                    Probe::Pending, // resets the consecutive-error counter
                    Probe::Fail,
                    Probe::Fail,
                    Probe::Ready,
                ],
                calls: calls.clone(),
            },
        )
        .add_exit_state(Flow::Done);
    let result = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Flow::Done);
    assert_eq!(calls.load(Ordering::SeqCst), 6);

    // Three consecutive errors exceed the budget: the third one propagates.
    let calls = Arc::new(AtomicU32::new(0));
    let workflow = Workflow::bare()
        .register(
            Flow::Work,
            ScriptedProbe {
                script: vec![Probe::Fail, Probe::Fail, Probe::Fail],
                calls: calls.clone(),
            },
        )
        .add_exit_state(Flow::Done);
    let err = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap_err();
    assert_eq!(err.category(), "task_execution", "got: {err}");
    assert!(err.to_string().contains("probe 2 refused"), "got: {err}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "no fourth poll after the budget"
    );
}

// ===========================================================================
// An eternally-pending condition is bounded by attempt_timeout
// ===========================================================================

/// The watched condition never becomes true.
struct StuckProbe;

#[task::poll]
impl PollTask<Flow> for StuckProbe {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(200))
    }

    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Flow>, CanoError> {
        Ok(PollOutcome::Pending { delay_ms: 5 })
    }
}

/// Contract: there is no built-in iteration cap — an always-Pending poller loops
/// forever unless `attempt_timeout` bounds the whole loop. The engine then fails
/// the state with category `timeout` near the configured budget.
#[tokio::test]
async fn poll_eternal_pending_is_bounded_by_attempt_timeout() {
    let workflow = Workflow::bare()
        .register(Flow::Work, StuckProbe)
        .add_exit_state(Flow::Done);

    let started = Instant::now();
    let err = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(err.category(), "timeout", "got: {err}");
    assert!(
        elapsed >= Duration::from_millis(200),
        "fired early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "liveness: took {elapsed:?}"
    );
}
