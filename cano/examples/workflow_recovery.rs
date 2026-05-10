//! # Crash Recovery — checkpoint a workflow and resume after a "crash"
//!
//! Run with: `cargo run --example workflow_recovery --features recovery`
//!
//! This example wires a [`RedbCheckpointStore`](cano::RedbCheckpointStore) into a
//! `Start → Process → Finalize → Done` workflow. The `Process` task fails the
//! first time it runs (standing in for a crash), so the initial `orchestrate`
//! returns an error. Because every state entry is checkpointed *before* its task
//! runs, the durable log already contains the `Process` row — so a second call,
//! [`Workflow::resume_from`](cano::Workflow::resume_from), picks up at `Process`,
//! re-runs it (this time it succeeds), and the workflow finishes at `Done`.
//!
//! Key points it demonstrates:
//! - `Workflow::with_checkpoint_store(..)` + `with_workflow_id(..)` to opt in.
//! - One [`CheckpointRow`](cano::CheckpointRow) per state entered, in sequence.
//! - `resume_from` re-runs the task at the resume point — tasks there must be
//!   idempotent (here `Process` is made idempotent with a one-shot failure flag).
//! - An observer seeing `on_checkpoint` / `on_resume`.

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

/// Simulates a transient failure: the first `Process` attempt errors (the
/// "crash"); every later attempt succeeds. Shared across runs via `Resources`.
#[derive(Default)]
struct CrashOnce {
    attempts: AtomicU32,
}
#[cano::resource]
impl Resource for CrashOnce {}

/// Prints the checkpoint / resume lifecycle so you can watch the FSM persist.
struct ProgressObserver;
impl WorkflowObserver for ProgressObserver {
    fn on_state_enter(&self, state: &str) {
        println!("  → entered state {state}");
    }
    fn on_checkpoint(&self, workflow_id: &str, sequence: u64) {
        println!("  ✓ checkpoint #{sequence} committed for {workflow_id:?}");
    }
    fn on_resume(&self, workflow_id: &str, sequence: u64) {
        println!("  ↺ resuming {workflow_id:?} from checkpoint #{sequence}");
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
    async fn run(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        println!("  StartTask: kicking things off");
        Ok(TaskResult::Single(Step::Process))
    }
}

#[task(state = Step)]
impl ProcessTask {
    // No automatic retries — we want the first failure to bubble all the way out
    // of `orchestrate`, the way a real crash would, so `resume_from` does the work.
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let crash = res.get::<CrashOnce, _>("crash")?;
        let attempt = crash.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == 1 {
            println!("  ProcessTask: attempt {attempt} — simulated crash!");
            return Err(CanoError::task_execution("simulated crash in ProcessTask"));
        }
        println!("  ProcessTask: attempt {attempt} — succeeded");
        Ok(TaskResult::Single(Step::Finalize))
    }
}

#[task(state = Step)]
impl FinalizeTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        println!("  FinalizeTask: wrapping up");
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("recovery.redb");
    let workflow_id = "demo-run-42";

    // One checkpoint store, one workflow definition — reused across both runs.
    let store = Arc::new(RedbCheckpointStore::new(&db_path)?);
    let resources = Resources::new().insert("crash", CrashOnce::default());
    let workflow = Workflow::new(resources)
        .register(Step::Start, StartTask)
        .register(Step::Process, ProcessTask)
        .register(Step::Finalize, FinalizeTask)
        .add_exit_state(Step::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id(workflow_id)
        .with_observer(Arc::new(ProgressObserver));

    println!("── Run 1: orchestrate (will crash inside ProcessTask) ──");
    match workflow.orchestrate(Step::Start).await {
        Ok(final_state) => println!("Run 1 unexpectedly finished at {final_state:?}"),
        Err(e) => println!("Run 1 stopped: {e}\n"),
    }

    println!("Checkpoint log so far:");
    for row in store.load_run(workflow_id).await? {
        println!(
            "  #{:<2} state={:<10} task_id={:?}",
            row.sequence, row.state, row.task_id
        );
    }

    println!("\n── Run 2: resume_from (picks up at the last checkpointed state) ──");
    let final_state = workflow.resume_from(workflow_id).await?;
    println!("\nRun 2 completed at {final_state:?}");

    println!("\nFinal checkpoint log:");
    for row in store.load_run(workflow_id).await? {
        println!(
            "  #{:<2} state={:<10} task_id={:?}",
            row.sequence, row.state, row.task_id
        );
    }
    assert_eq!(final_state, Step::Done);
    Ok(())
}
