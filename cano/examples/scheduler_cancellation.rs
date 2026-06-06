#![cfg(feature = "scheduler")]
//! # Scheduler cooperative cancellation
//!
//! Demonstrates [`RunningScheduler::cancel_flow`](cano::RunningScheduler::cancel_flow):
//! a manually-triggered saga `Reserve → Charge → Ship → Done` whose `Ship` step
//! runs long. A sibling task calls `cancel_flow` once `Ship` is in flight; the
//! engine aborts it at its next await, the saga compensation stack drains in
//! reverse (`Charge` then `Reserve`), and the flow returns to `Idle` — a
//! deliberate cancel is **not** counted as a backoff failure. Graceful `stop()`
//! cancels in-flight flows the same way.
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_cancellation --features scheduler
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cano::prelude::*;
use cano::scheduler::Status;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Charge,
    Ship,
    Done,
}

struct Reserve;
struct Charge;

#[saga::task(state = Step)]
impl Reserve {
    type Output = u32;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, u32), CanoError> {
        println!("reserve  : holding inventory (ticket #42)");
        Ok((TaskResult::Single(Step::Charge), 42))
    }
    async fn compensate(&self, _res: &Resources, ticket: u32) -> Result<(), CanoError> {
        println!("reserve  : releasing ticket #{ticket}  (rollback)");
        Ok(())
    }
}

#[saga::task(state = Step)]
impl Charge {
    type Output = String;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, String), CanoError> {
        println!("charge   : capturing $42.00 (auth auth-XYZ)");
        Ok((TaskResult::Single(Step::Ship), "auth-XYZ".to_string()))
    }
    async fn compensate(&self, _res: &Resources, auth: String) -> Result<(), CanoError> {
        println!("charge   : refunding auth {auth}  (rollback)");
        Ok(())
    }
}

/// Long-running, non-compensatable step. Flips `started` so the sibling
/// canceller fires deterministically while this task is parked in its sleep.
struct Ship {
    started: Arc<AtomicBool>,
}
#[task(state = Step)]
impl Ship {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("ship     : dispatching shipment…  (cancel_flow will stop this)");
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(10)).await;
        println!("ship     : this line should never print");
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    let ship_started = Arc::new(AtomicBool::new(false));

    let workflow = Workflow::bare()
        .register_with_compensation(Step::Reserve, Reserve)
        .register_with_compensation(Step::Charge, Charge)
        .register(
            Step::Ship,
            Ship {
                started: ship_started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let mut scheduler = Scheduler::new();
    scheduler.manual("order", workflow, Step::Reserve)?;
    let running = scheduler.start().await?;

    // Kick off the saga, then cancel it once Ship is in flight.
    running.trigger("order").await?;
    while !ship_started.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    println!("\n>>> cancelling the in-flight flow…\n");
    running.cancel_flow("order").await?;

    // Wait for the cancelled run to settle (saga drained, status back to Idle).
    loop {
        let status = running.status("order").await.map(|i| i.status);
        if status != Some(Status::Running) {
            println!("\norder flow status after cancel: {status:?}  (Idle — not a failure)");
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    running.stop().await?;
    Ok(())
}
