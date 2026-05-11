//! # Saga / Compensation — mixing compensatable and plain steps
//!
//! Run with: `cargo run --example saga_payment`
//!
//! `Reserve → Validate → Charge → Ship → Done`. `Reserve` and `Charge` are *compensatable*
//! (`#[saga::compensatable_task(state = Step)]`): each forward step returns an `Output`, and a
//! matching `compensate` undoes it. `Validate` and `Ship` are *plain* (`#[task(state = Step)]`,
//! registered with `register`) — they have nothing to roll back, so they never appear on the
//! compensation stack.
//!
//! `Ship` fails (courier down). *Any* task failing — plain or compensatable — drains the
//! per-run compensation stack in reverse: `Charge` refunds, then `Reserve` releases. The plain
//! steps (`Validate`, and the `Ship` that failed) aren't on the stack, so they're left alone.
//! A clean rollback like this returns the *original* error from `orchestrate`.

use cano::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Validate,
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

/// What `ChargeCard::run` produced — handed back to its `compensate` (a refund).
#[derive(Debug, Serialize, Deserialize)]
struct Charge {
    order_id: String,
    txn_id: String,
    amount_cents: u64,
}

struct ReserveInventory;
struct ValidateOrder;
struct ChargeCard;
struct ShipOrder;

#[saga::compensatable_task(state = Step)]
impl ReserveInventory {
    type Output = Reservation;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        let r = Reservation {
            order_id: "ord-1001".into(),
            sku: "WIDGET-7".into(),
            qty: 3,
        };
        println!("reserve  : holding {} × {}", r.qty, r.sku);
        Ok((TaskResult::Single(Step::Validate), r))
    }
    async fn compensate(&self, _res: &Resources, r: Reservation) -> Result<(), CanoError> {
        println!("reserve  : releasing {} × {}  (rollback)", r.qty, r.sku);
        Ok(())
    }
}

// A plain task: registered with `register`, not `register_with_compensation`. It has no
// side effect to undo, so it never gets a compensation-stack entry and is left alone if a
// later step fails.
#[task(state = Step)]
impl ValidateOrder {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("validate : order ok");
        Ok(TaskResult::Single(Step::Charge))
    }
}

#[saga::compensatable_task(state = Step)]
impl ChargeCard {
    type Output = Charge;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Charge), CanoError> {
        let charge = Charge {
            order_id: "ord-1001".into(),
            txn_id: "tx-7788".into(),
            amount_cents: 4_200,
        };
        println!("charge   : $42.00 charged ({})", charge.txn_id);
        Ok((TaskResult::Single(Step::Ship), charge))
    }
    async fn compensate(&self, _res: &Resources, c: Charge) -> Result<(), CanoError> {
        println!(
            "charge   : refunding {} cents (tx {})  (rollback)",
            c.amount_cents, c.txn_id
        );
        Ok(())
    }
}

// Another plain task — and this one fails. Its failure triggers the rollback of every
// compensatable step that ran before it: `Charge`, then `Reserve`.
#[task(state = Step)]
impl ShipOrder {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal() // fail-fast — let the original error escape `orchestrate`
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("ship     : dispatching → courier unavailable");
        Err(CanoError::task_execution("courier unavailable"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = Workflow::bare()
        .register_with_compensation(Step::Reserve, ReserveInventory)
        .register(Step::Validate, ValidateOrder) // plain — no compensation
        .register_with_compensation(Step::Charge, ChargeCard)
        .register(Step::Ship, ShipOrder) // plain — and it fails
        .add_exit_state(Step::Done);

    match workflow.orchestrate(Step::Reserve).await {
        Ok(state) => println!("\ncompleted at {state:?}"),
        Err(error) => println!("\nfailed, rolled back: {error}"),
    }
    Ok(())
}
