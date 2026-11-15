//! # PollTask with RetryOnError Policy
//!
//! Run with: `cargo run --example poll_retry_on_error`
//!
//! Demonstrates [`PollErrorPolicy::RetryOnError`] — a [`PollTask`] policy that tolerates
//! a limited run of consecutive `Err` returns from [`PollTask::poll`] before failing the
//! loop.
//!
//! Two scenarios side-by-side:
//!
//! **Scenario A — survives the streak:**
//! The poller produces 3 consecutive errors (the cap), then succeeds. Because
//! `max_errors = 3` allows up to 3 consecutive errors before failing, the loop
//! survives and the workflow completes normally.
//!
//! **Scenario B — exceeds the cap:**
//! The poller produces 4 consecutive errors. On the 4th the counter exceeds
//! `max_errors` and the loop returns `Err`, failing the workflow.
//!
//! Key rule: `PollOutcome::Pending` resets the consecutive-error counter.
//! Errors must be *consecutive* to accumulate toward the cap.

use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Poll,
    Done,
}

// ---------------------------------------------------------------------------
// Configurable poller — errors for the first `error_streak` calls, then succeeds
// ---------------------------------------------------------------------------

/// A [`PollTask`] that fails its first `error_streak` poll calls, then succeeds.
///
/// This makes the scenario completely deterministic: no real sleeps, no external state.
struct FlakyPoller {
    error_streak: u32,
    calls: Arc<AtomicU32>,
}

impl FlakyPoller {
    fn new(error_streak: u32) -> Self {
        Self {
            error_streak,
            calls: Arc::new(AtomicU32::new(0)),
        }
    }
}

#[task::poll(state = Step)]
impl FlakyPoller {
    fn config(&self) -> TaskConfig {
        // No outer retry wrapper — the PollErrorPolicy IS the resilience mechanism.
        TaskConfig::minimal()
    }

    fn on_poll_error(&self) -> PollErrorPolicy {
        // Allow up to 3 consecutive errors before the loop gives up.
        // The counter resets to zero on any PollOutcome::Pending.
        PollErrorPolicy::RetryOnError { max_errors: 3 }
    }

    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst) + 1;

        if call_number <= self.error_streak {
            println!(
                "  poll #{call_number}: Err (consecutive errors: {call_number}/{})  <- transient failure",
                self.error_streak
            );
            Err(CanoError::task_execution(format!(
                "transient error on call {call_number}"
            )))
        } else {
            println!(
                "  poll #{call_number}: Ready  <- success after {}/{} errors",
                call_number - 1,
                self.error_streak
            );
            Ok(PollOutcome::Ready(TaskResult::Single(Step::Done)))
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== PollTask RetryOnError Demo ===");
    println!("max_errors = 3  (up to 3 consecutive errors tolerated)\n");

    // -----------------------------------------------------------------------
    // Scenario A: 3 consecutive errors, then success — survives.
    //
    // error_streak=3 means calls 1,2,3 return Err and call 4 returns Ready.
    // The consecutive-error counter hits 3 (== max_errors) after call 3.
    // The loop checks `consecutive_errors > max_errors`, i.e. `3 > 3 = false`,
    // so it continues. Call 4 succeeds.
    // -----------------------------------------------------------------------
    println!("-- Scenario A: 3 errors then success (should succeed) --");
    {
        let poller = FlakyPoller::new(3);

        let workflow = Workflow::bare()
            .register(Step::Poll, poller)
            .add_exit_state(Step::Done);

        match workflow.orchestrate(Step::Poll).await {
            Ok(state) => println!("  result: Ok({state:?})  -- loop tolerated the streak\n"),
            Err(e) => println!("  result: Err({e})  -- unexpected failure\n"),
        }
    }

    // -----------------------------------------------------------------------
    // Scenario B: 4 consecutive errors — exceeds the cap, loop fails.
    //
    // error_streak=4 means calls 1,2,3,4 return Err. After call 4 the counter
    // is 4 and `4 > max_errors (3)` is true, so the loop propagates the error.
    // -----------------------------------------------------------------------
    println!("-- Scenario B: 4 consecutive errors (should fail) --");
    {
        let poller = FlakyPoller::new(4);

        let workflow = Workflow::bare()
            .register(Step::Poll, poller)
            .add_exit_state(Step::Done);

        match workflow.orchestrate(Step::Poll).await {
            Ok(state) => println!("  result: Ok({state:?})  -- unexpected success\n"),
            Err(e) => println!("  result: Err(\"{e}\")  -- loop aborted after streak > cap\n"),
        }
    }

    // -----------------------------------------------------------------------
    // Scenario C: 2 errors, 1 Pending tick, 2 more errors — never consecutive.
    //
    // We demonstrate that a Pending tick resets the counter: 2 errors, then a
    // success-Pending pair resets to 0, then 2 more errors. Since the max
    // consecutive run is 2, which never exceeds max_errors=3, the loop succeeds.
    // -----------------------------------------------------------------------
    println!("-- Scenario C: interleaved errors with Pending resets counter --");
    {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct InterleavedPoller {
            calls: Arc<AtomicU32>,
        }

        #[task::poll(state = Step)]
        impl InterleavedPoller {
            fn on_poll_error(&self) -> PollErrorPolicy {
                PollErrorPolicy::RetryOnError { max_errors: 3 }
            }

            async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                // call sequence: Err, Err, Pending(resets counter), Err, Err, Ready
                match n {
                    1 | 2 => {
                        println!("  poll #{n}: Err (streak so far: {n})");
                        Err(CanoError::task_execution(format!("error #{n}")))
                    }
                    3 => {
                        println!("  poll #{n}: Pending  <- resets consecutive-error counter to 0");
                        Ok(PollOutcome::Pending { delay_ms: 0 })
                    }
                    4 | 5 => {
                        println!("  poll #{n}: Err (streak so far: {})", n - 3);
                        Err(CanoError::task_execution(format!("error #{n}")))
                    }
                    _ => {
                        println!("  poll #{n}: Ready");
                        Ok(PollOutcome::Ready(TaskResult::Single(Step::Done)))
                    }
                }
            }
        }

        let workflow = Workflow::bare()
            .register(
                Step::Poll,
                InterleavedPoller {
                    calls: Arc::new(AtomicU32::new(0)),
                },
            )
            .add_exit_state(Step::Done);

        match workflow.orchestrate(Step::Poll).await {
            Ok(state) => println!("  result: Ok({state:?})  -- Pending reset the counter\n"),
            Err(e) => println!("  result: Err({e})  -- unexpected failure\n"),
        }
    }

    println!("=== Done ===");
    Ok(())
}
