//! # Cooperative cancellation — saga rollback on cancel
//!
//! Demonstrates [`Workflow::orchestrate_with_cancel`](cano::Workflow::orchestrate_with_cancel):
//! a 3-step saga `Reserve → Charge → Ship → Done` where a sibling task fires a
//! [`CancellationHandle`](cano::CancellationHandle) once `Ship` is in flight. The in-flight
//! task is aborted at its next await point, the saga compensation stack drains in reverse
//! (`Charge` then `Reserve`), and the call returns
//! [`CanoError::Cancelled`](cano::CanoError::Cancelled).
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_cancellation
//! ```
//!
//! Expected output (timings will vary):
//! ```text
//! reserve  : holding inventory (ticket #42)
//! charge   : capturing $42.00 (auth auth-XYZ)
//! ship     : dispatching shipment…  (a sibling task will cancel this)
//! charge   : refunding auth auth-XYZ  (rollback)
//! reserve  : releasing ticket #42  (rollback)
//! workflow cancelled, rolled back: state=Ship attempt=0 path=[Reserve, Charge, Ship] caused by: Workflow cancelled
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cano::CancellationToken;
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
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, u32), CanoError> {
        let ticket = 42;
        println!("reserve  : holding inventory (ticket #{ticket})");
        Ok((TaskResult::Single(Step::Charge), ticket))
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
        let auth = "auth-XYZ".to_string();
        println!("charge   : capturing $42.00 (auth {auth})");
        Ok((TaskResult::Single(Step::Ship), auth))
    }
    async fn compensate(&self, _res: &Resources, auth: String) -> Result<(), CanoError> {
        println!("charge   : refunding auth {auth}  (rollback)");
        Ok(())
    }
}

// Plain (non-compensatable) long-running task. It flips `started` so the sibling
// canceller fires deterministically while this task is parked in its sleep.
struct Ship {
    started: Arc<AtomicBool>,
}
#[task(state = Step)]
impl Ship {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("ship     : dispatching shipment…  (a sibling task will cancel this)");
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(2)).await;
        println!("ship     : this line should never print");
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::main]
async fn main() {
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

    let (handle, token) = CancellationToken::new();

    // Sibling task: cancel as soon as `Ship` is in flight.
    let canceller = tokio::spawn(async move {
        while !ship_started.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        handle.cancel();
    });

    match workflow.orchestrate_with_cancel(Step::Reserve, token).await {
        Ok(state) => println!("\nworkflow completed at {state:?}"),
        Err(error) => println!("\nworkflow cancelled, rolled back: {error}"),
    }

    canceller.await.expect("canceller task panicked");
}
