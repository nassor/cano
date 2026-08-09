//! Reliability contracts of [`SteppedTask`] under critical scenarios.
//!
//! Black-box integration tests (public API only, engine-driven `register_stepped`
//! path) documenting how the stepped processing model behaves when a step fails:
//!
//! | Critical scenario | Contract |
//! |-------------------|----------|
//! | transient step error | per-step retry re-runs the **same cursor** — no restart from `None` |
//! | hard step failure → `resume_from` | resume continues at the last persisted cursor; earlier steps never re-run |
//!
//! The SIGKILL (process-death) variant of cursor resume is covered separately by
//! `stepped_resume_e2e.rs`; these tests exercise the in-process failure paths.

mod support;

use cano::prelude::*;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use support::{Flow, MemStore};

// ===========================================================================
// A transient step error retries the SAME cursor
// ===========================================================================

/// Pages 0→5 via `More(n+1)`; the step at a scripted cursor fails a scripted
/// number of times before succeeding.
struct PagedMigration {
    flaky_cursor: u32,
    failures_remaining: Arc<AtomicU32>,
    calls: Arc<Mutex<Vec<Option<u32>>>>,
}

#[task::stepped]
impl SteppedTask<Flow> for PagedMigration {
    type Cursor = u32;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal().with_fixed_retry(2, Duration::from_millis(1)) // 3 attempts/step
    }

    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, Flow>, CanoError> {
        self.calls.lock().push(cursor);
        if cursor == Some(self.flaky_cursor) && self.failures_remaining.load(Ordering::SeqCst) > 0 {
            self.failures_remaining.fetch_sub(1, Ordering::SeqCst);
            return Err(CanoError::task_execution("page fetch flaked"));
        }
        let n = cursor.unwrap_or(0);
        if n >= 5 {
            Ok(StepOutcome::Done(TaskResult::Single(Flow::Done)))
        } else {
            Ok(StepOutcome::More(n + 1))
        }
    }
}

/// Contract: the retry budget wraps each *step call*, not the whole loop. A
/// transient error re-invokes `step` with the SAME cursor — progress made by
/// earlier steps is never redone, and the loop does not restart from `None`.
#[tokio::test]
async fn stepped_transient_error_retries_same_cursor_without_restarting() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let workflow = Workflow::bare()
        .register_stepped(
            Flow::Work,
            PagedMigration {
                flaky_cursor: 3,
                failures_remaining: Arc::new(AtomicU32::new(2)),
                calls: calls.clone(),
            },
        )
        .add_exit_state(Flow::Done);

    let result = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Flow::Done);
    assert_eq!(
        *calls.lock(),
        vec![
            None,
            Some(1),
            Some(2),
            Some(3), // fails
            Some(3), // retry 1 — same cursor
            Some(3), // retry 2 — same cursor, succeeds
            Some(4),
            Some(5),
        ],
        "retries pin the cursor; earlier pages are never refetched"
    );
}

// ===========================================================================
// Hard failure, then resume from the persisted cursor
// ===========================================================================

/// Counts 0→6; the step at `blocked_at` fails until `unblocked` flips (a
/// dependency outage that is later repaired).
struct BlockingMigration {
    blocked_at: u32,
    unblocked: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<Option<u32>>>>,
}

#[task::stepped]
impl SteppedTask<Flow> for BlockingMigration {
    type Cursor = u32;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal() // one attempt per step: fail fast to the workflow
    }

    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, Flow>, CanoError> {
        self.calls.lock().push(cursor);
        if cursor == Some(self.blocked_at) && !self.unblocked.load(Ordering::SeqCst) {
            return Err(CanoError::task_execution("downstream outage"));
        }
        let n = cursor.unwrap_or(0);
        if n >= 6 {
            Ok(StepOutcome::Done(TaskResult::Single(Flow::Done)))
        } else {
            Ok(StepOutcome::More(n + 1))
        }
    }
}

/// Contract: every `More` persists its cursor BEFORE the next step runs, so a
/// hard mid-loop failure leaves an exact restart position. `resume_from`
/// re-enters the stepped state at that cursor — completed steps never re-run.
#[tokio::test]
async fn stepped_hard_failure_resumes_from_last_persisted_cursor() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let unblocked = Arc::new(AtomicBool::new(false));
    let store = Arc::new(MemStore::default());

    let workflow = Workflow::bare()
        .register_stepped(
            Flow::Work,
            BlockingMigration {
                blocked_at: 4,
                unblocked: unblocked.clone(),
                calls: calls.clone(),
            },
        )
        .add_exit_state(Flow::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id("stepped-outage");

    // Run 1: advances through cursors 1..=4, then the step at 4 hits the outage.
    let err = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap_err();
    assert_eq!(err.category(), "task_execution", "got: {err}");
    assert_eq!(
        *calls.lock(),
        vec![None, Some(1), Some(2), Some(3), Some(4)]
    );
    assert_eq!(
        store.last_cursor("stepped-outage", "Work"),
        Some(4),
        "the cursor reached before the failure is durably persisted"
    );

    // Outage repaired; resume continues AT cursor 4 — not from None.
    unblocked.store(true, Ordering::SeqCst);
    let result = workflow
        .resume_from("stepped-outage", CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Flow::Done);

    let all = calls.lock().clone();
    assert_eq!(
        all[5..],
        [Some(4), Some(5), Some(6)],
        "resume re-runs only the failed step and the remainder"
    );
    assert_eq!(
        all.iter().filter(|c| c.is_none()).count(),
        1,
        "the loop never restarted from the beginning"
    );
    assert!(
        store.rows("stepped-outage").is_empty(),
        "cleared on success"
    );
}
