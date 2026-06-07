//! # Workflow total timeout — saga rollback on budget exhaustion
//!
//! Demonstrates [`Workflow::with_total_timeout`](cano::Workflow::with_total_timeout):
//! a 3-step saga `Reserve → Charge → Ship → Done` where `Ship` sleeps past the
//! wall-clock budget. When the budget elapses, the in-flight task is aborted at
//! its next await point, the saga compensation stack drains in reverse
//! (`Charge` then `Reserve`), and `orchestrate` returns
//! [`CanoError::WorkflowTimeout`](cano::CanoError::WorkflowTimeout).
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_total_timeout
//! ```
//!
//! Expected output (timings will vary):
//! ```text
//! reserve  : holding inventory (ticket #42)
//! charge   : capturing $42.00 (auth auth-XYZ)
//! ship     : dispatching shipment (this will overrun the 200ms budget)…
//! charge   : refunding auth auth-XYZ  (rollback)
//! reserve  : releasing ticket #42  (rollback)
//! workflow failed, rolled back: workflow total timeout exceeded: elapsed=...ms, limit=200ms
//! ```

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
struct Ship;

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

// Plain (non-compensatable) task. It sleeps far past the workflow's total
// budget, so the engine aborts it at the `sleep` await point and triggers
// rollback of every compensatable step that ran before it.
#[task(state = Step)]
impl Ship {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("ship     : dispatching shipment (this will overrun the 200ms budget)…");
        tokio::time::sleep(Duration::from_secs(2)).await;
        println!("ship     : this line should never print");
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::main]
async fn main() {
    let workflow = Workflow::bare()
        .with_total_timeout(Duration::from_millis(200))
        .register_with_compensation(Step::Reserve, Reserve)
        .register_with_compensation(Step::Charge, Charge)
        .register(Step::Ship, Ship)
        .add_exit_state(Step::Done);

    match workflow
        .orchestrate(Step::Reserve, CancellationToken::disabled())
        .await
    {
        Ok(state) => println!("\nworkflow completed at {state:?}"),
        Err(error) => println!("\nworkflow failed, rolled back: {error}"),
    }
}
