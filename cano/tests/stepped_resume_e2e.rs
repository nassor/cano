//! End-to-end crash-recovery test for [`SteppedTask`] cursor persistence.
//!
//! A child process runs a checkpointed `Crunch → Done` stepped workflow.
//! The parent reads stdout until it observes a `STEP cursor=N` line with `N ≥ 3`
//! (meaning the redb store already contains cursor blob `N`), then SIGKILLs the
//! child. It reads the redb checkpoint file to find the highest persisted cursor
//! value (`n_persisted`), then restarts the child in `resume` mode. The restarted
//! child must print `RESUMED cursor=<n_persisted>` on its first step call —
//! proving the cursor was correctly rehydrated from the store rather than
//! restarting from `None`.
//!
//! Unix-only (relies on `Child::kill` delivering SIGKILL) and requires the
//! `recovery` feature (the child binary uses `RedbCheckpointStore`).
#![cfg(all(feature = "recovery", unix))]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cano::RedbCheckpointStore;
use cano::recovery::{CheckpointStore, RowKind};

const BIN: &str = env!("CARGO_BIN_EXE_stepped_resume");
const WORKFLOW_ID: &str = "stepped-run";

/// Parse `"STEP cursor=N"` or `"RESUMED cursor=N"`, returning `N`.
fn parse_cursor_line(line: &str) -> Option<u32> {
    let rest = line
        .strip_prefix("STEP cursor=")
        .or_else(|| line.strip_prefix("RESUMED cursor="))?;
    rest.parse().ok()
}

#[test]
fn sigkill_then_resume_continues_from_persisted_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("stepped.redb");

    // --- Run 1: start fresh; kill once we see cursor ≥ 3 in stdout. ---
    let mut child = Command::new(BIN)
        .arg(&db)
        .arg("run")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fresh stepped_resume child");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last_seen: Option<u32> = None;

    while Instant::now() < deadline {
        match lines.next() {
            Some(Ok(ref line)) => {
                if let Some(n) = parse_cursor_line(line) {
                    last_seen = Some(n);
                    if n >= 3 {
                        break;
                    }
                }
            }
            Some(Err(e)) => panic!("reading child stdout: {e}"),
            None => break, // child exited before reaching cursor 3 — unexpected
        }
    }

    assert!(
        last_seen.is_some_and(|n| n >= 3),
        "fresh child did not reach cursor ≥ 3 before the timeout (last seen: {last_seen:?})"
    );

    // Kill the child and wait for it to exit so the redb file lock is released.
    child.kill().expect("SIGKILL stepped_resume fresh child");
    let status = child.wait().expect("await killed child");
    assert!(!status.success(), "SIGKILLed child should not exit Ok");

    // --- Read the redb store: find the highest persisted StepCursor value. ---
    let n_persisted: u32 = {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let store = Arc::new(RedbCheckpointStore::new(&db).expect("open redb after kill"));
            let rows = store
                .load_run(WORKFLOW_ID)
                .await
                .expect("load_run after kill");

            rows.into_iter()
                .filter(|r| r.kind == RowKind::StepCursor)
                .filter_map(|r| {
                    r.output_blob
                        .as_deref()
                        .and_then(|b| serde_json::from_slice::<u32>(b).ok())
                })
                .max()
                .expect("at least one StepCursor row must be present after kill")
        })
    };

    assert!(
        n_persisted >= 3,
        "expected at least cursor 3 to be persisted in redb, got {n_persisted}"
    );

    // --- Run 2: resume; the first step must receive exactly `n_persisted`. ---
    let out = Command::new(BIN)
        .arg(&db)
        .arg("resume")
        .stdout(Stdio::piped())
        .output()
        .expect("spawn resume stepped_resume child");

    let resume_stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "resume run failed (status {:?}):\n{resume_stdout}",
        out.status
    );

    let expected_resumed = format!("RESUMED cursor={n_persisted}");
    assert!(
        resume_stdout.contains(&expected_resumed),
        "expected marker `{expected_resumed}` in resume stdout, got:\n{resume_stdout}"
    );

    assert!(
        resume_stdout.contains("RESUME COMPLETE final=Done"),
        "expected completion marker `RESUME COMPLETE final=Done`, got:\n{resume_stdout}"
    );
}
