//! # SteppedTask — Resumable-Iterative Processing Model
//!
//! Run with: `cargo run --example stepped_task --features recovery`
//!
//! Demonstrates [`SteppedTask`] cursor persistence with a [`RedbCheckpointStore`] attached.
//! A `Crunch` state processes items in discrete steps; each `StepOutcome::More` persists the cursor
//! so a crash mid-loop can resume from the last saved position rather than restarting from
//! the beginning.
//!
//! Workflow shape:
//!
//! ```text
//!   Crunch ──(step loop)──► Report ──► Done
//! ```
//!
//! The example shows the most common `SteppedTask` shape:
//! - A small `Progress` cursor struct tracks how many items have been processed.
//! - Each `step` call processes one item, prints progress, and returns `StepOutcome::More(cursor)`
//!   until all items are done, then `StepOutcome::Done(Stage::Report)`.
//! - A plain `#[task]` at `Report` summarises the run and transitions to `Done`.
//! - `RedbCheckpointStore` attached so cursors are persisted to disk after each step.
//!
//! On a **successful** run the engine clears the checkpoint log. To see cursor resumption
//! in action (crash mid-loop + restart), see `tests/stepped_resume_e2e.rs` and the
//! `stepped_resume` binary.

use std::sync::Arc;

use cano::RedbCheckpointStore;
use cano::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

/// Workflow states for this example.
///
/// Named `Stage` here; `Step` would also be fine since `cano::StepOutcome` no longer
/// collides with a plain identifier `Step`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage {
    /// Step-loop state: processes items one by one, advancing the cursor.
    Crunch,
    /// Post-processing state: summarises the completed run.
    Report,
    /// Terminal exit state.
    Done,
}

// ---------------------------------------------------------------------------
// Cursor — tracks progress across steps
// ---------------------------------------------------------------------------

/// Serialisable position marker threaded through each `step` call.
///
/// The engine persists this struct as a `CheckpointRow` of `kind == RowKind::StepCursor`
/// after every `StepOutcome::More`, so a crash mid-loop can resume from `processed` rather than
/// from zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Progress {
    processed: u32,
    total: u32,
}

// ---------------------------------------------------------------------------
// Crunch — the stepped task
// ---------------------------------------------------------------------------

/// Processes `total` items one at a time, advancing `Progress` with each step.
struct Cruncher {
    total: u32,
}

#[task::stepped(state = Stage)]
impl Cruncher {
    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<Progress>,
    ) -> Result<StepOutcome<Progress, Stage>, CanoError> {
        let p = cursor.unwrap_or(Progress {
            processed: 0,
            total: self.total,
        });

        println!("crunch  : item {}/{} processed", p.processed + 1, p.total);

        let next = Progress {
            processed: p.processed + 1,
            total: p.total,
        };

        if next.processed >= next.total {
            Ok(StepOutcome::Done(TaskResult::Single(Stage::Report)))
        } else {
            Ok(StepOutcome::More(next))
        }
    }
}

// ---------------------------------------------------------------------------
// Report — summary task
// ---------------------------------------------------------------------------

struct Reporter {
    total: u32,
}

#[task(state = Stage)]
impl Reporter {
    async fn run_bare(&self) -> Result<TaskResult<Stage>, CanoError> {
        println!(
            "report  : all {} items processed — transitioning to Done",
            self.total
        );
        Ok(TaskResult::Single(Stage::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    const TOTAL: u32 = 8;
    let run_id = "stepped-demo";

    // Create a temporary directory for the checkpoint database.
    let dir = tempfile::tempdir()?;
    let store = Arc::new(RedbCheckpointStore::new(dir.path().join("stepped.redb"))?);

    println!("=== stepped_task example ({TOTAL} items) ===\n");

    let workflow = Workflow::bare()
        .register_stepped(Stage::Crunch, Cruncher { total: TOTAL })
        .register(Stage::Report, Reporter { total: TOTAL })
        .add_exit_state(Stage::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id(run_id);

    let result = workflow.orchestrate(Stage::Crunch).await?;
    assert_eq!(result, Stage::Done);

    println!("\ncompleted at {result:?}");

    // On a successful run the engine clears the checkpoint log.
    let rows = store.load_run(run_id).await?;
    println!(
        "\ncheckpoint log after successful run: {} row(s) (cleared on success)",
        rows.len()
    );
    assert!(
        rows.is_empty(),
        "checkpoint log should be empty after a successful run"
    );

    println!("\nNote: to observe cursor persistence and mid-loop resumption,");
    println!("run `cargo test --features recovery --test stepped_resume_e2e`.");

    // The temp directory (and the redb file) is cleaned up when `dir` drops here.
    Ok(())
}
