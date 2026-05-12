//! # Custom CheckpointStore — In-Memory HashMap Backend
//!
//! Run with: `cargo run --example custom_checkpoint_store`
//!
//! Implements a minimal [`CheckpointStore`] backed by
//! `Arc<Mutex<HashMap<String, Vec<CheckpointRow>>>>` using the
//! `#[cano::checkpoint_store]` macro on an inherent `impl`. Shows:
//!
//! - Writing the three required methods (`append`, `load_run`, `clear`).
//! - Enforcing the **no-duplicate-(workflow_id, sequence)** contract with an `Err`.
//! - Attaching the store to a workflow via [`Workflow::with_checkpoint_store`] +
//!   [`Workflow::with_workflow_id`].
//! - Inspecting persisted rows after a first run that ends in error (the checkpoint
//!   log is kept on failure so `resume_from` can continue).
//! - Calling [`Workflow::resume_from`] to continue from the last checkpoint.
//!
//! Workflow shape:
//!
//! ```text
//!   Init ──► Process ──► Finalize ──► Done
//! ```
//!
//! `Process` fails on the first call (simulating a crash) and succeeds on the
//! second — demonstrating that `resume_from` re-runs the checkpointed state.

use cano::prelude::*;
use cano::recovery::CheckpointRow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Custom CheckpointStore
// ---------------------------------------------------------------------------

/// A minimal in-memory checkpoint store for demonstration and testing.
///
/// The inner `Mutex<HashMap<workflow_id, rows>>` is the only state. It is cheap
/// to clone because `Arc` provides shared ownership.
#[derive(Default, Clone)]
struct MapStore(Arc<Mutex<HashMap<String, Vec<CheckpointRow>>>>);

// `#[cano::checkpoint_store]` on an inherent `impl` synthesises the
// `impl CheckpointStore for MapStore` header — no boilerplate needed.
#[cano::checkpoint_store]
impl MapStore {
    /// Append `row` for `workflow_id`. Rejects a duplicate `(workflow_id, sequence)`.
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let mut map = self.0.lock().expect("MapStore mutex poisoned");
        let rows = map.entry(workflow_id.to_string()).or_default();
        // Contract: duplicate (workflow_id, sequence) must be rejected.
        if rows.iter().any(|r| r.sequence == row.sequence) {
            return Err(CanoError::checkpoint_store(format!(
                "duplicate checkpoint: workflow={workflow_id:?} sequence={}",
                row.sequence
            )));
        }
        rows.push(row);
        Ok(())
    }

    /// Load all rows for `workflow_id`, sorted ascending by `sequence`.
    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let mut rows = self
            .0
            .lock()
            .expect("MapStore mutex poisoned")
            .get(workflow_id)
            .cloned()
            .unwrap_or_default();
        rows.sort_by_key(|r| r.sequence);
        Ok(rows)
    }

    /// Remove all rows for `workflow_id`. No-op if the id is unknown.
    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.0
            .lock()
            .expect("MapStore mutex poisoned")
            .remove(workflow_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Init,
    Process,
    Finalize,
    Done,
}

// ---------------------------------------------------------------------------
// Crash-once resource — makes Process fail exactly once.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CrashOnce {
    attempts: AtomicU32,
}

#[resource]
impl Resource for CrashOnce {}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

struct InitTask;

#[task(state = Step)]
impl InitTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("  init: preparing workflow");
        Ok(TaskResult::Single(Step::Process))
    }
}

struct ProcessTask;

#[task(state = Step)]
impl ProcessTask {
    fn config(&self) -> TaskConfig {
        // No retries — the simulated crash should bubble straight out of `orchestrate`.
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let n = res
            .get::<CrashOnce, _>("crash")?
            .attempts
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        if n == 1 {
            println!("  process: attempt {n} — simulated crash");
            return Err(CanoError::task_execution("ProcessTask: simulated crash"));
        }
        println!("  process: attempt {n} — ok");
        Ok(TaskResult::Single(Step::Finalize))
    }
}

struct FinalizeTask;

#[task(state = Step)]
impl FinalizeTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("  finalize: wrapping up");
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(MapStore::default());
    let run_id = "demo-custom-store";

    let workflow = Workflow::new(Resources::new().insert("crash", CrashOnce::default()))
        .register(Step::Init, InitTask)
        .register(Step::Process, ProcessTask)
        .register(Step::Finalize, FinalizeTask)
        .add_exit_state(Step::Done)
        .with_checkpoint_store(store.clone() as Arc<dyn CheckpointStore>)
        .with_workflow_id(run_id);

    // --- First run: Process crashes; the checkpoint log is kept. -------
    println!("=== run 1: Process will crash ===");
    match workflow.orchestrate(Step::Init).await {
        Ok(s) => println!("  completed at {s:?} (unexpected)"),
        Err(e) => println!("  stopped with error: {e}"),
    }

    println!("\nCheckpoint log after run 1 (crash left it intact):");
    for row in store.load_run(run_id).await? {
        println!(
            "  seq={:<3} state={:<9} task={:<20} kind={:?}",
            row.sequence, row.state, row.task_id, row.kind
        );
    }

    // Verify the duplicate-sequence rejection.
    let dup_result = store
        .append(run_id, CheckpointRow::new(0, "Init", "InitTask"))
        .await;
    assert!(
        dup_result.is_err(),
        "store must reject a duplicate (workflow_id, sequence)"
    );
    println!(
        "\nDuplicate-sequence rejection: {}",
        dup_result.unwrap_err()
    );

    // --- Second run: resume from last checkpoint (Process, attempt 2). ---
    println!("\n=== run 2: resume_from ===");
    let final_state = workflow.resume_from(run_id).await?;
    println!("  reached {final_state:?}");
    assert_eq!(final_state, Step::Done);

    println!("\nCheckpoint log after successful run (cleared by engine):");
    let remaining = store.load_run(run_id).await?;
    if remaining.is_empty() {
        println!("  (empty — engine cleared it on success)");
    } else {
        for row in &remaining {
            println!("  seq={} state={}", row.sequence, row.state);
        }
    }

    println!("\n=== Done ===");
    Ok(())
}
