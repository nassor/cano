//! # Crash Recovery — checkpoint a workflow and resume after a "crash"
//!
//! Run with: `cargo run --example workflow_recovery --features recovery`
//!
//! A `Start → Process → Finalize → Done` workflow with a [`RedbCheckpointStore`](cano::RedbCheckpointStore)
//! attached. `Process` fails the first time it runs (standing in for a crash), so the
//! initial `orchestrate` returns an error — but the checkpoint log already has a `Process`
//! row (state entries are checkpointed *before* their task runs), so
//! [`Workflow::resume_from`](cano::Workflow::resume_from) picks up at `Process`, re-runs it
//! (this time it succeeds), and finishes at `Done`. Tasks at and after the resume point must
//! be idempotent — here `Process` is, via a one-shot failure flag in a shared resource. On a
//! successful run the engine clears the checkpoint log.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cano::RedbCheckpointStore;
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Start,
    Process,
    Finalize,
    Done,
}

/// Makes `Process` fail exactly once (the "crash") and succeed thereafter. Shared across the
/// `orchestrate` and `resume_from` calls via `Resources`.
#[derive(Default)]
struct CrashOnce {
    attempts: AtomicU32,
}
#[cano::resource]
impl Resource for CrashOnce {}

/// Prints each checkpoint as it's committed, and the resume point — watch the FSM persist.
///
/// Exercises [`WorkflowObserver::on_checkpoint`] and [`WorkflowObserver::on_resume`] so
/// checkpoint and resume events are visible alongside the workflow output.
struct Watcher;
impl WorkflowObserver for Watcher {
    fn on_checkpoint(&self, workflow_id: &str, seq: u64) {
        println!("  ✓ checkpoint  workflow_id={workflow_id}  sequence={seq}");
    }
    fn on_resume(&self, workflow_id: &str, seq: u64) {
        println!("  ↺ resuming    workflow_id={workflow_id}  from_sequence={seq}");
    }
}

#[derive(Clone)]
struct StartTask;
#[derive(Clone)]
struct ProcessTask;
#[derive(Clone)]
struct FinalizeTask;

#[task(state = Step)]
impl StartTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("  start");
        Ok(TaskResult::Single(Step::Process))
    }
}

#[task(state = Step)]
impl ProcessTask {
    // No retries — let the first failure (the "crash") bubble out of `orchestrate`.
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let n = res
            .get::<CrashOnce, _>("crash")?
            .attempts
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        if n == 1 {
            println!("  process: attempt {n} — crash!");
            return Err(CanoError::task_execution("simulated crash in ProcessTask"));
        }
        println!("  process: attempt {n} — ok");
        Ok(TaskResult::Single(Step::Finalize))
    }
}

#[task(state = Step)]
impl FinalizeTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("  finalize");
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(RedbCheckpointStore::new(dir.path().join("recovery.redb"))?);
    let run_id = "demo-run-42";

    // One workflow definition, one store — reused across both calls below.
    let workflow = Workflow::new(Resources::new().insert("crash", CrashOnce::default()))
        .register(Step::Start, StartTask)
        .register(Step::Process, ProcessTask)
        .register(Step::Finalize, FinalizeTask)
        .add_exit_state(Step::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id(run_id)
        .with_observer(Arc::new(Watcher));

    println!("── run 1: orchestrate (Process will crash) ──");
    if let Err(e) = workflow.orchestrate(Step::Start).await {
        println!("  stopped: {e}");
    }
    println!("checkpoint log after run 1 (the crash left it intact):");
    for row in store.load_run(run_id).await? {
        println!("  #{:<2} {:<9} {}", row.sequence, row.state, row.task_id);
    }

    println!("\n── run 2: resume_from ──");
    let final_state = workflow.resume_from(run_id).await?;
    println!("  reached {final_state:?} — checkpoint log cleared on success");
    assert_eq!(final_state, Step::Done);
    assert!(store.load_run(run_id).await?.is_empty());

    Ok(())
}
