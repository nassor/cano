//! # TimerTask — Wait-Then-Transition Processing Model
//!
//! Run with: `cargo run --example timer_task`
//!
//! Demonstrates [`TimerTask`] as a "pause, then continue" processing unit. The `CoolDown`
//! timer sleeps for a fixed [`Duration`] (the engine schedules a *single* `tokio::time::sleep`,
//! not a polling loop), then transitions to the processing step.
//!
//! Workflow shape:
//!
//! ```text
//!   CoolDown ──(sleep 50ms)──► Process ──► Done
//! ```
//!
//! Contrast with [`PollTask`](cano::PollTask): a poll task re-evaluates a condition on every
//! wake-up, whereas a timer wakes exactly once. Reach for a `TimerTask` when you just need a
//! deliberate delay — a cool-down between phases, a debounce, or resuming after a known delay via
//! a monotonic [`Instant`](std::time::Instant) and [`TimerOutcome::Until`].
//!
//! To cap the wait, `config()` can return `TaskConfig::minimal().with_attempt_timeout(dur)`; the
//! engine enforces it and produces `CanoError::Timeout` if the timer hasn't fired in time.
//!
//! Note: a `TimerTask` is checkpointed like any single-task state, so a resumed run re-runs
//! `wait()` and the delay restarts from scratch after a crash.

use std::time::Duration;

use cano::prelude::*;

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    /// Timer state: pauses for a fixed cool-down before continuing.
    CoolDown,
    /// Processing state: runs after the cool-down elapses.
    Process,
    /// Terminal exit state.
    Done,
}

// ---------------------------------------------------------------------------
// Timer task — waits a fixed cool-down, then transitions
// ---------------------------------------------------------------------------

/// Waits a fixed duration before letting the workflow continue.
struct CoolDown {
    delay: Duration,
}

#[task::timer(state = Step)]
impl CoolDown {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        println!(
            "cool down  : sleeping for {:?} (single scheduled wake-up)",
            self.delay
        );
        Ok(TimerOutcome::Duration(self.delay))
    }

    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        // `wait` can also return `TimerOutcome::Until(instant)`; see the module docs.
        println!("cool down  : elapsed — continuing");
        Ok(TaskResult::Single(Step::Process))
    }
}

// ---------------------------------------------------------------------------
// Processing task — runs after the cool-down
// ---------------------------------------------------------------------------

/// Simple post-cool-down processor that transitions to the exit state.
struct Process;

#[task(state = Step)]
impl Process {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("process    : handling work after the cool-down");
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("=== timer_task example ===\n");

    let workflow = Workflow::bare()
        .register(
            Step::CoolDown,
            CoolDown {
                delay: Duration::from_millis(50),
            },
        )
        .register(Step::Process, Process)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::CoolDown).await?;
    assert_eq!(result, Step::Done);
    println!("\ncompleted at {result:?}");

    Ok(())
}
