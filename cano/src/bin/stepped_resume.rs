//! Subprocess driver for the SIGKILL stepped-task recovery integration test
//! (`tests/stepped_resume_e2e.rs`). Not meant to be run by hand.
//!
//! Usage: `stepped_resume <db_path> <mode>`
//! where `<mode>` is `run` (drive the stepped workflow; the parent SIGKILLs it
//! mid-loop) or `resume` (continue from the last persisted cursor to completion).
//!
//! The workflow is a single `Crunch → Done` state with a `SteppedTask` that
//! counts from 0 to `TARGET - 1`. Each `step` call prints a marker line and
//! flushes stdout so the parent can observe progress. The 50 ms sleep gives the
//! parent a comfortable window to SIGKILL the process after a few cursors have
//! been persisted.
//!
//! Stdout markers:
//! - `STEP cursor=N`   — received cursor N (printed on every step in either mode)
//! - `RESUMED cursor=N` — first step call after resume, receiving Some(N)
//! - `RUN COMPLETE final={state:?}` — fresh run finished (won't occur; parent kills first)
//! - `RESUME COMPLETE final={state:?}` — resume run finished successfully

use std::io::Write as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use cano::RedbCheckpointStore;
use cano::prelude::*;

const TARGET: u32 = 10;
const WORKFLOW_ID: &str = "stepped-run";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State {
    Crunch,
    Done,
}

/// The `SteppedTask` that counts from 0 to `TARGET - 1`.
///
/// In `run` mode it pauses 50 ms each step so the parent can SIGKILL mid-loop.
/// In `resume` mode it also pauses (same code), but prints `RESUMED cursor=N`
/// on the first call that receives `Some(N)` to let the parent verify the cursor
/// was correctly rehydrated.
struct Cruncher {
    is_resume: bool,
    first_call: AtomicBool,
}

impl Cruncher {
    fn new(is_resume: bool) -> Self {
        Self {
            is_resume,
            first_call: AtomicBool::new(true),
        }
    }
}

#[stepped_task(state = State)]
impl Cruncher {
    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, State>, CanoError> {
        let n = cursor.unwrap_or(0);

        // Print RESUMED on the first call in resume mode when the cursor is Some.
        if self.is_resume && cursor.is_some() && self.first_call.swap(false, Ordering::SeqCst) {
            println!("RESUMED cursor={n}");
            let _ = std::io::stdout().flush();
        } else {
            // Always print the current cursor so the parent can track progress.
            println!("STEP cursor={n}");
            let _ = std::io::stdout().flush();
        }

        // Sleep to widen the kill window.
        tokio::time::sleep(Duration::from_millis(50)).await;

        if n + 1 >= TARGET {
            Ok(StepOutcome::Done(TaskResult::Single(State::Done)))
        } else {
            Ok(StepOutcome::More(n + 1))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: stepped_resume <db_path> <run|resume>");
        std::process::exit(2);
    }
    let db_path = &args[1];
    let mode = args[2].as_str();

    let store = Arc::new(RedbCheckpointStore::new(db_path)?);
    let is_resume = mode == "resume";

    let workflow = Workflow::bare()
        .register_stepped(State::Crunch, Cruncher::new(is_resume))
        .add_exit_state(State::Done)
        .with_checkpoint_store(store)
        .with_workflow_id(WORKFLOW_ID);

    let final_state = match mode {
        "resume" => {
            let result = workflow.resume_from(WORKFLOW_ID).await?;
            println!("RESUME COMPLETE final={result:?}");
            let _ = std::io::stdout().flush();
            result
        }
        _ => {
            let result = workflow.orchestrate(State::Crunch).await?;
            println!("RUN COMPLETE final={result:?}");
            let _ = std::io::stdout().flush();
            result
        }
    };

    let _ = final_state;
    Ok(())
}
