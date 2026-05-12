//! Subprocess driver for the SIGKILL recovery integration test
//! (`tests/recovery_e2e.rs`). Not meant to be run by hand.
//!
//! Usage: `recovery_resume <db_path> <workflow_id> <mode> <sidefx_path>`
//! where `<mode>` is `fresh` (run the workflow from the start; park forever
//! inside `Work` after recording its side effect, so the parent can SIGKILL it)
//! or `resume` (continue a previously checkpointed run to completion).
//!
//! The workflow is `Start → Work → Done`; each task appends a line to
//! `<sidefx_path>` when it runs. Stdout markers (`CHECKPOINT <state> <seq>`,
//! `RESUME <id> <seq>`, `WORK_SIDEFX_WRITTEN`, `DONE <state>`) let the parent
//! synchronize and assert.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cano::RedbCheckpointStore;
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Start,
    Work,
    Done,
}

/// File the tasks append to so the parent can inspect side effects across runs.
#[derive(cano::Resource)]
struct SideEffects {
    path: PathBuf,
}

impl SideEffects {
    fn record(&self, what: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .expect("open side-effects file");
        writeln!(f, "{what}").expect("append side-effect line");
    }
}

/// Whether `Work` should park forever after recording its side effect (the
/// "fresh" run does; the "resume" run runs straight through).
#[derive(cano::Resource)]
struct PauseAfterWork(bool);

#[derive(Clone)]
struct StartTask;
#[derive(Clone)]
struct WorkTask;

#[task(state = Step)]
impl StartTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        res.get::<SideEffects, _>("sidefx")?.record("Start");
        Ok(TaskResult::Single(Step::Work))
    }
}

#[task(state = Step)]
impl WorkTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        res.get::<SideEffects, _>("sidefx")?.record("Work");
        println!("WORK_SIDEFX_WRITTEN");
        let _ = std::io::stdout().flush();
        if res.get::<PauseAfterWork, _>("pause")?.0 {
            // Park until the parent SIGKILLs us — the `Work` checkpoint is
            // already durable (it's written on state entry, before this task).
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        Ok(TaskResult::Single(Step::Done))
    }
}

/// Prints checkpoint/resume markers so the parent can follow progress. Tracks
/// the last entered state (which immediately precedes its checkpoint) so the
/// `CHECKPOINT` line can name the state.
struct Tracer {
    last_state: Mutex<String>,
}

impl WorkflowObserver for Tracer {
    fn on_state_enter(&self, state: &str) {
        *self.last_state.lock().unwrap() = state.to_string();
    }
    fn on_checkpoint(&self, _workflow_id: &str, sequence: u64) {
        println!("CHECKPOINT {} {sequence}", self.last_state.lock().unwrap());
        let _ = std::io::stdout().flush();
    }
    fn on_resume(&self, workflow_id: &str, sequence: u64) {
        println!("RESUME {workflow_id} {sequence}");
        let _ = std::io::stdout().flush();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: recovery_resume <db_path> <workflow_id> <fresh|resume> <sidefx_path>");
        std::process::exit(2);
    }
    let db_path = &args[1];
    let workflow_id = args[2].clone();
    let mode = args[3].as_str();
    let sidefx = PathBuf::from(&args[4]);

    let store = Arc::new(RedbCheckpointStore::new(db_path)?);
    let resources = Resources::new()
        .insert("sidefx", SideEffects { path: sidefx })
        .insert("pause", PauseAfterWork(mode == "fresh"));

    let workflow = Workflow::new(resources)
        .register(Step::Start, StartTask)
        .register(Step::Work, WorkTask)
        .add_exit_state(Step::Done)
        .with_checkpoint_store(store)
        .with_workflow_id(workflow_id.clone())
        .with_observer(Arc::new(Tracer {
            last_state: Mutex::new(String::new()),
        }));

    let final_state = match mode {
        "resume" => workflow.resume_from(workflow_id).await?,
        _ => workflow.orchestrate(Step::Start).await?,
    };
    println!("DONE {final_state:?}");
    let _ = std::io::stdout().flush();
    Ok(())
}
