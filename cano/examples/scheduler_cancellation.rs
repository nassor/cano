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
    async fn run(&self, _res: &Resources) -> Result<(Step, u32), CanoError> {
        println!("reserve  : holding inventory (ticket #42)");
        Ok((Step::Charge, 42))
    }
    async fn compensate(&self, _res: &Resources, ticket: u32) -> Result<(), CanoError> {
        println!("reserve  : releasing ticket #{ticket}  (rollback)");
        Ok(())
    }
}

#[saga::task(state = Step)]
impl Charge {
    type Output = String;
    async fn run(&self, _res: &Resources) -> Result<(Step, String), CanoError> {
        println!("charge   : capturing $42.00 (auth auth-XYZ)");
        Ok((Step::Ship, "auth-XYZ".to_string()))
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
    async fn run(&self) -> Result<Step, CanoError> {
        println!("ship     : dispatching shipment…  (cancel_flow will stop this)");
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(10)).await;
        println!("ship     : this line should never print");
        Ok(Step::Done)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    let ship_started = Arc::new(AtomicBool::new(false));

    let workflow = Workflow::bare()
        .register(Step::Reserve, Reserve)
        .register(Step::Charge, Charge)
        .register(
            Step::Ship,
            Ship {
                started: ship_started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let running = Scheduler::new()
        .manual("order", workflow, Step::Reserve)
        .start()
        .await?;

    // Kick off the saga, then cancel it once Ship is in flight.
    running.trigger("order").await?;
    while !ship_started.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    println!("\n>>> cancelling the in-flight flow…\n");
    running.cancel_flow("order")?;

    // Wait for the cancelled run to settle (saga drained, status back to Idle).
    if let Some(info) = running.wait_idle("order").await {
        println!(
            "\norder flow status after cancel: {:?}  (Idle — not a failure)",
            info.status
        );
    }

    running.stop().await?;
    Ok(())
}
