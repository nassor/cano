//! # Saga / Compensation — roll back a multi-step payment when a step fails
//!
//! Run with: `cargo run --example saga_payment`
//!
//! `Reserve → Charge → Ship → Done`. `Reserve`, `Charge`, and `Ship` are
//! [`CompensatableTask`]s: each forward step returns an `Output` describing what it did,
//! and a matching `compensate` undoes it. If a later step fails, the engine drains the
//! per-run compensation stack in reverse and runs each `compensate`.
//!
//! Here `Charge` always fails (declined card), so the run stops. `Charge` produced no
//! output (it failed before committing), so it isn't on the stack; only `Reserve` is, so
//! its compensation runs — releasing the reservation. `Ship` is never reached. A clean
//! rollback like this returns the *original* error from `orchestrate`. Watch the observer
//! output to follow the forward steps, the failure, and the rollback.

use std::sync::Arc;

use cano::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Charge,
    Ship,
    Done,
}

/// Output of `ReserveInventory::run`, handed back to its `compensate`.
#[derive(Debug, Serialize, Deserialize)]
struct Reservation {
    order_id: String,
    sku: String,
    qty: u32,
}

/// Output `ChargeCard::run` would produce on success.
#[derive(Debug, Serialize, Deserialize)]
struct Charge {
    order_id: String,
    amount_cents: u64,
}

struct ReserveInventory;
struct ChargeCard;
struct ShipOrder;

#[compensatable_task]
impl CompensatableTask<Step> for ReserveInventory {
    type Output = Reservation;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        let reservation = Reservation {
            order_id: "ord-1001".into(),
            sku: "WIDGET-7".into(),
            qty: 3,
        };
        println!(
            "  ReserveInventory: reserved {} × {} for {}",
            reservation.qty, reservation.sku, reservation.order_id
        );
        Ok((TaskResult::Single(Step::Charge), reservation))
    }
    async fn compensate(&self, _res: &Resources, output: Reservation) -> Result<(), CanoError> {
        println!(
            "  ReserveInventory::compensate: releasing {} × {} for {}",
            output.qty, output.sku, output.order_id
        );
        Ok(())
    }
}

#[compensatable_task]
impl CompensatableTask<Step> for ChargeCard {
    type Output = Charge;
    // No retries — a declined card is a declined card; surface it straight away.
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Charge), CanoError> {
        println!("  ChargeCard: charging $42.00 ... declined!");
        Err(CanoError::task_execution("card declined"))
        // On success this would be:
        //   Ok((TaskResult::Single(Step::Ship),
        //       Charge { order_id: "ord-1001".into(), amount_cents: 4_200 }))
    }
    async fn compensate(&self, _res: &Resources, output: Charge) -> Result<(), CanoError> {
        println!(
            "  ChargeCard::compensate: refunding {} cents for {}",
            output.amount_cents, output.order_id
        );
        Ok(())
    }
}

#[compensatable_task]
impl CompensatableTask<Step> for ShipOrder {
    type Output = (); // a compensatable task that needs no output data
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, ()), CanoError> {
        println!("  ShipOrder: shipping");
        Ok((TaskResult::Single(Step::Done), ()))
    }
    async fn compensate(&self, _res: &Resources, _output: ()) -> Result<(), CanoError> {
        println!("  ShipOrder::compensate: recalling shipment");
        Ok(())
    }
}

struct Tracer;
impl WorkflowObserver for Tracer {
    fn on_state_enter(&self, state: &str) {
        println!("→ {state}");
    }
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        println!("✗ {task_id}: {err}");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = Workflow::bare()
        .register_with_compensation(Step::Reserve, ReserveInventory)
        .register_with_compensation(Step::Charge, ChargeCard)
        .register_with_compensation(Step::Ship, ShipOrder)
        .add_exit_state(Step::Done)
        .with_observer(Arc::new(Tracer));

    match workflow.orchestrate(Step::Reserve).await {
        Ok(s) => println!("\nCompleted at {s:?}"),
        Err(e) => println!("\nWorkflow failed and rolled back: {e}"),
    }
    Ok(())
}
