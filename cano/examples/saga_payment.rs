//! # Saga / Compensation — roll back a multi-step payment when a step fails
//!
//! Run with: `cargo run --example saga_payment`
//!
//! `Reserve → Charge → Ship → Done`, all three steps compensatable. `Charge` declines, so
//! the run stops; `Charge` produced no output (it failed before committing) so it isn't on
//! the compensation stack — only `Reserve` is, so its `compensate` runs, releasing the
//! reservation. A clean rollback like this returns the *original* error from `orchestrate`.
//!
//! Each step uses `#[task(state = Step, compensatable)]`: like a plain `#[task(state = …)]`,
//! but `run` also returns an `Output` and there's a `compensate` that undoes the step given
//! that `Output`. (`#[compensatable_task(state = Step)]` is the same thing.)

use cano::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Charge,
    Ship,
    Done,
}

/// What `ReserveInventory::run` produced — handed back to its `compensate` to undo it.
#[derive(Debug, Serialize, Deserialize)]
struct Reservation {
    order_id: String,
    sku: String,
    qty: u32,
}

/// What `ChargeCard::run` would produce on success.
#[derive(Debug, Serialize, Deserialize)]
struct Charge {
    order_id: String,
    amount_cents: u64,
}

struct ReserveInventory;
struct ChargeCard;
struct ShipOrder;

#[task(state = Step, compensatable)]
impl ReserveInventory {
    type Output = Reservation;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        let r = Reservation {
            order_id: "ord-1001".into(),
            sku: "WIDGET-7".into(),
            qty: 3,
        };
        println!("reserve  : holding {} × {}", r.qty, r.sku);
        Ok((TaskResult::Single(Step::Charge), r))
    }
    async fn compensate(&self, _res: &Resources, r: Reservation) -> Result<(), CanoError> {
        println!("reserve  : releasing {} × {}  (rollback)", r.qty, r.sku);
        Ok(())
    }
}

#[task(state = Step, compensatable)]
impl ChargeCard {
    type Output = Charge;
    // No retries — a declined card won't un-decline; let the error escape `orchestrate`.
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Charge), CanoError> {
        println!("charge   : $42.00 → declined");
        Err(CanoError::task_execution("card declined"))
        // On success: Ok((TaskResult::Single(Step::Ship),
        //                 Charge { order_id: "ord-1001".into(), amount_cents: 4_200 }))
    }
    async fn compensate(&self, _res: &Resources, c: Charge) -> Result<(), CanoError> {
        println!("charge   : refunding {} cents  (rollback)", c.amount_cents);
        Ok(())
    }
}

#[task(state = Step, compensatable)]
impl ShipOrder {
    type Output = (); // a compensatable task that needs no data to undo itself
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, ()), CanoError> {
        println!("ship     : dispatching");
        Ok((TaskResult::Single(Step::Done), ()))
    }
    async fn compensate(&self, _res: &Resources, _: ()) -> Result<(), CanoError> {
        println!("ship     : recalling shipment  (rollback)");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = Workflow::bare()
        .register_with_compensation(Step::Reserve, ReserveInventory)
        .register_with_compensation(Step::Charge, ChargeCard)
        .register_with_compensation(Step::Ship, ShipOrder)
        .add_exit_state(Step::Done);

    match workflow.orchestrate(Step::Reserve).await {
        Ok(state) => println!("\ncompleted at {state:?}"),
        Err(error) => println!("\nfailed, rolled back: {error}"),
    }
    Ok(())
}
