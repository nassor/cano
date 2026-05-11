//! The "custom software" the end-to-end tests drive: a small Cano app that runs the saga
//! workflow against a Postgres-backed [`CheckpointStore`](cano::CheckpointStore).
//!
//! Usage:
//! ```text
//! cano_workflow_app <postgres-dsn> <workflow-id> <run|resume> [fail_at=PHASE] [pause_at=PHASE]
//! ```
//! It prints stdout markers (`READY`, `CHECKPOINT <state> <seq>`, `RESUME <id> <seq>`,
//! `PAUSED <phase>`, `DONE <state>`, `FAILED <err>`) so a test harness can synchronize,
//! and exits non-zero on failure. The container image (see `../../Dockerfile`) has this as
//! its entrypoint; it can also be run directly against a local Postgres for a smoke test.

use std::str::FromStr;
use std::sync::Arc;

use cano_e2e::{Faults, Phase, PostgresCheckpointStore, StdoutTracer, build_workflow};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        anyhow::bail!(
            "usage: cano_workflow_app <dsn> <workflow_id> <run|resume> [fail_at=PHASE] [pause_at=PHASE]"
        );
    }
    let dsn = args[1].clone();
    let workflow_id = args[2].clone();
    let mode = args[3].clone();

    let mut faults = Faults::default();
    for arg in &args[4..] {
        if let Some(p) = arg.strip_prefix("fail_at=") {
            faults.fail_at = Some(Phase::from_str(p)?);
        } else if let Some(p) = arg.strip_prefix("pause_at=") {
            faults.pause_at = Some(Phase::from_str(p)?);
        } else {
            anyhow::bail!("unknown argument {arg:?}");
        }
    }

    // Postgres may still be coming up — retry the first connect for a while.
    let store = {
        let mut store = None;
        let mut last_err = None;
        for _ in 0..40 {
            match PostgresCheckpointStore::connect(&dsn).await {
                Ok(s) => {
                    store = Some(s);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    cano_e2e::sleep(500).await;
                }
            }
        }
        store.ok_or_else(|| last_err.expect("at least one connect attempt"))?
    };

    let workflow =
        build_workflow(store, &workflow_id, faults).with_observer(Arc::new(StdoutTracer::new()));
    emit(&format!("READY {workflow_id} {mode}"));

    let result = match mode.as_str() {
        "resume" => workflow.resume_from(workflow_id.clone()).await,
        "run" => workflow.orchestrate(Phase::Reserve).await,
        other => anyhow::bail!("unknown mode {other:?}"),
    };
    match result {
        Ok(state) => {
            emit(&format!("DONE {state:?}"));
            Ok(())
        }
        Err(e) => {
            emit(&format!("FAILED {e}"));
            std::process::exit(1);
        }
    }
}

fn emit(line: &str) {
    use std::io::Write as _;
    println!("{line}");
    let _ = std::io::stdout().flush();
}
