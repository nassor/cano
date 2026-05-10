//! End-to-end recovery test.
//!
//! A child process runs a checkpointed `Start → Work → Done` workflow. The
//! parent waits until `Work` has recorded its side effect (by which point the
//! `Work` checkpoint is already durable), SIGKILLs the child, then restarts it
//! with the same workflow id in `resume` mode. The restarted child must pick up
//! at `Work`, re-run it, and finish at `Done` — without re-running `Start`
//! (which is before the resume point).
//!
//! Unix-only (relies on `Child::kill` delivering SIGKILL) and requires the
//! `recovery` feature (the child binary uses `RedbCheckpointStore`).
#![cfg(all(feature = "recovery", unix))]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_recovery_resume");

fn read_side_effects(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn sigkill_then_resume_completes_without_duplicating_prior_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ckpt.redb");
    let sidefx = dir.path().join("sidefx.log");
    let wf_id = "e2e-run";

    // --- Run 1: start fresh; kill once `Work` has run (and its checkpoint is durable). ---
    let mut child = Command::new(BIN)
        .arg(&db)
        .arg(wf_id)
        .arg("fresh")
        .arg(&sidefx)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fresh child");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut work_ran = false;
    while Instant::now() < deadline {
        match lines.next() {
            Some(Ok(line)) if line == "WORK_SIDEFX_WRITTEN" => {
                work_ran = true;
                break;
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => panic!("reading child stdout: {e}"),
            None => break, // child exited before reaching Work — unexpected
        }
    }
    assert!(
        work_ran,
        "fresh child did not reach `Work` before the timeout"
    );

    // The child is now parked inside WorkTask; the `Work` checkpoint is committed.
    child.kill().expect("SIGKILL fresh child");
    let status = child.wait().expect("await killed child");
    assert!(!status.success(), "SIGKILLed child should not exit Ok");

    assert_eq!(
        read_side_effects(&sidefx),
        ["Start", "Work"],
        "after run 1: Start ran, then Work ran once before the kill"
    );

    // --- Run 2: resume with the same id; must re-run Work and finish at Done. ---
    let out = Command::new(BIN)
        .arg(&db)
        .arg(wf_id)
        .arg("resume")
        .arg(&sidefx)
        .stdout(Stdio::piped())
        .output()
        .expect("spawn resume child");
    let resume_stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "resume run failed (status {:?}):\n{resume_stdout}",
        out.status
    );
    assert!(
        resume_stdout.contains(&format!("RESUME {wf_id} 1")),
        "expected resume marker `RESUME {wf_id} 1`, got:\n{resume_stdout}"
    );
    assert!(
        resume_stdout.contains("DONE Done"),
        "expected completion marker `DONE Done`, got:\n{resume_stdout}"
    );

    assert_eq!(
        read_side_effects(&sidefx),
        ["Start", "Work", "Work"],
        "after resume: Start NOT re-run (before resume point); Work re-ran (idempotency contract)"
    );
}
