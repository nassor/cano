//! # Saga + Recovery — Compensatable Tasks with a Persistent Checkpoint Log
//!
//! Run with: `cargo run --example saga_recovery --features recovery`
//!
//! Combines [`CompensatableTask`] (saga) with [`RedbCheckpointStore`] (recovery) to show
//! what happens when a workflow fails partway through a chain of compensatable steps:
//!
//! - Compensations drain the stack **LIFO** — the most recent step rolls back first.
//! - A *clean* rollback (all `compensate` calls succeed) returns the **original error**
//!   and clears the checkpoint log — no recovery log left behind.
//!
//! Workflow shape:
//!
//! ```text
//!   Reserve ──► Authorize ──► Charge ──► Done
//! ```
//!
//! `Reserve` and `Authorize` are compensatable (`#[saga::task]`). `Charge` is a plain
//! task that always fails (simulating an external outage). The failure triggers LIFO
//! compensation: `Authorize.compensate` runs first, then `Reserve.compensate`.
//!
//! Demo (b): after the clean rollback, `store.load_run(id)` returns an empty vec —
//! the engine cleared the log on success of the compensation chain.

use std::sync::Arc;

use cano::RedbCheckpointStore;
use cano::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Authorize,
    Charge,
    Done,
}

// ---------------------------------------------------------------------------
// Output types — handed to the matching `compensate` to undo the step.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct Reservation {
    item_id: String,
    qty: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Authorization {
    auth_code: String,
    amount_cents: u64,
}

// ---------------------------------------------------------------------------
// Task structs
// ---------------------------------------------------------------------------

struct ReserveItem;
struct AuthorizePayment;
struct ChargePayment;

// ---------------------------------------------------------------------------
// Compensatable task: ReserveItem
// ---------------------------------------------------------------------------

#[saga::task(state = Step)]
impl ReserveItem {
    type Output = Reservation;

    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        let r = Reservation {
            item_id: "SKU-42".into(),
            qty: 2,
        };
        println!("reserve    : holding {} x {}", r.qty, r.item_id);
        Ok((TaskResult::Single(Step::Authorize), r))
    }

    async fn compensate(&self, _res: &Resources, r: Reservation) -> Result<(), CanoError> {
        println!(
            "reserve    : releasing {} x {}  (rollback)",
            r.qty, r.item_id
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Compensatable task: AuthorizePayment
// ---------------------------------------------------------------------------

#[saga::task(state = Step)]
impl AuthorizePayment {
    type Output = Authorization;

    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Authorization), CanoError> {
        let a = Authorization {
            auth_code: "AUTH-9988".into(),
            amount_cents: 5_000,
        };
        println!(
            "authorize  : pre-auth {} cents ({})",
            a.amount_cents, a.auth_code
        );
        Ok((TaskResult::Single(Step::Charge), a))
    }

    async fn compensate(&self, _res: &Resources, a: Authorization) -> Result<(), CanoError> {
        println!(
            "authorize  : voiding auth {} ({} cents)  (rollback)",
            a.auth_code, a.amount_cents
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Plain task: ChargePayment — always fails to trigger compensation.
// ---------------------------------------------------------------------------

#[task(state = Step)]
impl ChargePayment {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal() // no retries — let the error escape immediately
    }

    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("charge     : payment gateway unreachable — failing");
        Err(CanoError::task_execution("payment gateway unreachable"))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a unique path in the OS temp dir so multiple runs don't collide.
    let db_path =
        std::env::temp_dir().join(format!("cano_saga_recovery_{}.redb", std::process::id()));
    let store = Arc::new(RedbCheckpointStore::new(&db_path)?);
    let run_id = "saga-recovery-demo";

    println!("=== Saga + Recovery Demo ===");
    println!("checkpoint db: {}", db_path.display());
    println!();

    let workflow = Workflow::bare()
        .register_with_compensation(Step::Reserve, ReserveItem)
        .register_with_compensation(Step::Authorize, AuthorizePayment)
        .register(Step::Charge, ChargePayment) // plain — always fails
        .add_exit_state(Step::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id(run_id);

    // --- (a) First run: Charge fails; compensations drain LIFO. ----------
    println!("--- run: Reserve → Authorize → Charge (fails) → compensate LIFO ---\n");
    match workflow.orchestrate(Step::Reserve).await {
        Ok(s) => println!("\ncompleted at {s:?} (unexpected)"),
        Err(e) => println!("\nfailed + rolled back: {e}"),
    }

    // --- (b) Checkpoint log cleared on clean rollback. -------------------
    println!("\nCheckpoint log after clean rollback:");
    let rows = store.load_run(run_id).await?;
    if rows.is_empty() {
        println!("  (empty — engine cleared the log after a clean compensation run)");
    } else {
        for row in &rows {
            println!(
                "  seq={} state={} kind={:?}",
                row.sequence, row.state, row.kind
            );
        }
    }

    // Clean up the temp file — `RedbCheckpointStore` is not a `tempfile`.
    std::fs::remove_file(&db_path).ok();

    println!("\n=== Done ===");
    Ok(())
}
