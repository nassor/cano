//! # Panic Safety — panics caught by the engine, compensation still runs
//!
//! Run with: `cargo run --example panic_safety`
//!
//! Cano wraps every task execution in `catch_unwind`, converting a panic into
//! `CanoError::TaskExecution("panic: <message>")` rather than unwinding the caller.
//! The program continues and no data is corrupted.
//!
//! This example shows two things:
//!
//! 1. **Plain panic** — a task that `panic!`s; the workflow returns `Err` with the panic
//!    message embedded, and the program continues past the `orchestrate` call.
//!
//! 2. **Panic with compensation** — a compensatable step runs before the panicking task.
//!    When the panic propagates as an error the compensation stack is drained LIFO, so
//!    the compensatable step's `compensate` runs and the original error is returned.
//!    No checkpoint store is required — compensation works in-memory.
//!
//! Workflow shape for scenario 2:
//!
//! ```text
//! Reserve (compensatable) → PanicTask → Done
//!                                   ↓ (panic)
//!                          compensate(Reserve)
//! ```

use cano::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    /// A compensatable step that runs first.
    Reserve,
    /// A plain task that panics.
    PanicTask,
    /// Never reached in these examples.
    Done,
}

// ---------------------------------------------------------------------------
// Scenario 1: plain task that panics
// ---------------------------------------------------------------------------

struct Panicker;

#[task(state = Step)]
impl Panicker {
    fn config(&self) -> TaskConfig {
        // No retries — let the panic error bubble immediately.
        TaskConfig::minimal()
    }

    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        panic!("something went terribly wrong");
    }
}

// ---------------------------------------------------------------------------
// Scenario 2: compensatable step followed by a panicking plain task
// ---------------------------------------------------------------------------

/// The output produced by `ReserveSlot::run` — passed back to `compensate` for rollback.
#[derive(Debug, Serialize, Deserialize)]
struct Reservation {
    slot_id: u32,
    held_by: String,
}

struct ReserveSlot;

#[saga::task(state = Step)]
impl ReserveSlot {
    type Output = Reservation;

    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        let r = Reservation {
            slot_id: 42,
            held_by: "order-7001".into(),
        };
        println!("  reserve : holding slot {} for {}", r.slot_id, r.held_by);
        Ok((TaskResult::Single(Step::PanicTask), r))
    }

    async fn compensate(&self, _res: &Resources, r: Reservation) -> Result<(), CanoError> {
        println!(
            "  reserve : releasing slot {} for {}  (compensation run after panic)",
            r.slot_id, r.held_by
        );
        Ok(())
    }
}

struct PanicAfterReserve;

#[task(state = Step)]
impl PanicAfterReserve {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        panic!("panic after reservation — should trigger compensation");
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Panic Safety Demo ===\n");

    // -----------------------------------------------------------------------
    // Scenario 1: plain panic caught as CanoError::TaskExecution.
    //
    // The workflow returns Err without the process unwinding. The program
    // continues past the `match` and runs scenario 2.
    // -----------------------------------------------------------------------
    println!("-- Scenario 1: task panics, engine catches it --");
    {
        let workflow = Workflow::bare()
            .register(Step::PanicTask, Panicker)
            .add_exit_state(Step::Done);

        match workflow.orchestrate(Step::PanicTask).await {
            Ok(s) => println!("  outcome: Ok({s:?})  (unexpected)"),
            Err(e) => {
                println!("  outcome: Err(\"{e}\")");
                // Confirm the error variant and that the panic message is preserved.
                assert!(
                    matches!(&e, CanoError::TaskExecution(msg) if msg.contains("panic")),
                    "expected TaskExecution with 'panic' in message, got: {e:?}"
                );
                println!("  confirmed: CanoError::TaskExecution carrying the panic message");
            }
        }
        println!("  program continues normally after the panic\n");
    }

    // -----------------------------------------------------------------------
    // Scenario 2: compensatable step runs, then a panic occurs.
    //
    // The engine catches the panic, converts it to Err, then drains the
    // compensation stack LIFO: `ReserveSlot::compensate` runs, printing the
    // release message. The original error is returned (clean rollback).
    // -----------------------------------------------------------------------
    println!("-- Scenario 2: compensation runs after a downstream panic --");
    {
        let workflow = Workflow::bare()
            .register_with_compensation(Step::Reserve, ReserveSlot)
            .register(Step::PanicTask, PanicAfterReserve)
            .add_exit_state(Step::Done);

        match workflow.orchestrate(Step::Reserve).await {
            Ok(s) => println!("  outcome: Ok({s:?})  (unexpected)"),
            Err(e) => {
                println!("  outcome: Err(\"{e}\")");
                assert!(
                    matches!(&e, CanoError::TaskExecution(msg) if msg.contains("panic")),
                    "expected TaskExecution, got: {e:?}"
                );
                println!("  confirmed: original panic error returned after clean compensation");
            }
        }
    }

    println!("\n=== Done ===");
    Ok(())
}
