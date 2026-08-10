//! Reliability contract of a multi-model pipeline under a mid-run crash.
//!
//! Black-box integration test (public API only) composing three processing models
//! — [`PollTask`] → [`BatchTask`] → [`StreamTask`] — in one checkpointed workflow:
//!
//! | Critical scenario | Contract |
//! |-------------------|----------|
//! | crash in a late state → `resume_from` | only the crashed state re-enters; completed states are never re-executed |

mod support;

use cano::prelude::*;
use futures_util::Stream;
use parking_lot::Mutex;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use support::{MemStore, boxed};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Pipe {
    Wait,
    Crunch,
    Pump,
    Done,
}

/// Poll stage: ready on the 3rd check.
struct Waiter {
    poll_calls: Arc<AtomicU32>,
}

#[task::poll]
impl PollTask<Pipe> for Waiter {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Pipe>, CanoError> {
        let n = self.poll_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= 3 {
            Ok(PollOutcome::Ready(TaskResult::Single(Pipe::Crunch)))
        } else {
            Ok(PollOutcome::Pending { delay_ms: 1 })
        }
    }
}

/// Batch stage: squares 1..=4 and records the aggregate.
struct Cruncher {
    load_calls: Arc<AtomicU32>,
    sums: Arc<Mutex<Vec<u32>>>,
}

#[task::batch]
impl BatchTask<Pipe> for Cruncher {
    type Item = u32;
    type ItemOutput = u32;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        self.load_calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![1, 2, 3, 4])
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        Ok(item * item)
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Pipe>, CanoError> {
        let sum = outputs.into_iter().filter_map(|r| r.ok()).sum();
        self.sums.lock().push(sum);
        Ok(TaskResult::Single(Pipe::Pump))
    }
}

/// Stream stage: consumes 1..=6 in windows of 2; item 5 is corrupt exactly once.
struct Pumper {
    fail_once_at: Arc<Mutex<Option<u64>>>,
    flushed: Arc<Mutex<Vec<Vec<u64>>>>,
    opened_with: Arc<Mutex<Vec<Option<u64>>>>,
}

#[task::stream]
impl StreamTask<Pipe> for Pumper {
    type Item = u64;
    type Output = u64;
    type Cursor = u64;

    fn window(&self) -> StreamWindow {
        StreamWindow::Count(2)
    }

    async fn open(
        &self,
        _res: &Resources,
        cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
        self.opened_with.lock().push(cursor);
        Ok(boxed(cursor.map_or(1, |c| c + 1)..=6))
    }

    async fn process_item(&self, _res: &Resources, item: u64) -> Result<(u64, u64), CanoError> {
        let mut fail = self.fail_once_at.lock();
        if *fail == Some(item) {
            *fail = None;
            return Err(CanoError::task_execution(format!(
                "pump item {item} corrupt"
            )));
        }
        Ok((item, item))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<u64>,
    ) -> Result<WindowSignal<Pipe>, CanoError> {
        self.flushed.lock().push(outputs);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        _reason: CloseReason,
    ) -> Result<TaskResult<Pipe>, CanoError> {
        Ok(TaskResult::Single(Pipe::Done))
    }
}

/// Contract: the engine checkpoints one row per state entry, so `resume_from`
/// jumps straight into the state that crashed — the poll and batch stages that
/// already completed are NOT re-executed. Only the crashed stream state re-runs,
/// and it re-opens from its own persisted cursor. This is what makes a multi-model
/// pipeline restartable without double-charging earlier side effects.
#[tokio::test]
async fn pipeline_resume_reenters_only_the_crashed_state() {
    let poll_calls = Arc::new(AtomicU32::new(0));
    let load_calls = Arc::new(AtomicU32::new(0));
    let sums = Arc::new(Mutex::new(Vec::new()));
    let flushed = Arc::new(Mutex::new(Vec::new()));
    let opened_with = Arc::new(Mutex::new(Vec::new()));

    let store = Arc::new(MemStore::default());
    let workflow = Workflow::bare()
        .register(
            Pipe::Wait,
            Waiter {
                poll_calls: poll_calls.clone(),
            },
        )
        .register(
            Pipe::Crunch,
            Cruncher {
                load_calls: load_calls.clone(),
                sums: sums.clone(),
            },
        )
        .register_stream(
            Pipe::Pump,
            Pumper {
                fail_once_at: Arc::new(Mutex::new(Some(5))),
                flushed: flushed.clone(),
                opened_with: opened_with.clone(),
            },
        )
        .add_exit_state(Pipe::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id("pipeline");

    // Run 1: Wait and Crunch complete; Pump flushes [1,2] and [3,4], then crashes
    // on the corrupt item 5.
    let err = workflow
        .orchestrate(Pipe::Wait, CancellationToken::disabled())
        .await
        .unwrap_err();
    assert_eq!(err.category(), "task_execution", "got: {err}");
    assert_eq!(poll_calls.load(Ordering::SeqCst), 3);
    assert_eq!(load_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*sums.lock(), vec![30]);
    assert_eq!(*flushed.lock(), vec![vec![1, 2], vec![3, 4]]);
    assert_eq!(store.last_cursor("pipeline", "Pump"), Some(4));

    // Run 2: resume enters at Pump directly.
    let result = workflow
        .resume_from("pipeline", CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Pipe::Done);

    // Completed stages did not re-execute…
    assert_eq!(
        poll_calls.load(Ordering::SeqCst),
        3,
        "the poll stage must not re-run on resume"
    );
    assert_eq!(
        load_calls.load(Ordering::SeqCst),
        1,
        "the batch stage must not re-run on resume"
    );
    assert_eq!(*sums.lock(), vec![30], "no double aggregation");

    // …while the crashed stream stage resumed from its own cursor.
    assert_eq!(*opened_with.lock(), vec![None, Some(4)]);
    assert_eq!(*flushed.lock(), vec![vec![1, 2], vec![3, 4], vec![5, 6]]);
    assert!(store.rows("pipeline").is_empty(), "cleared on success");
}
