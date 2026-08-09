//! Reliability contracts of [`StreamTask`] under critical scenarios.
//!
//! Black-box integration tests (public API only, engine-driven `register_stream`
//! path) documenting how the stream processing model behaves when things go wrong
//! mid-flight:
//!
//! | Critical scenario | Contract |
//! |-------------------|----------|
//! | crash mid-window → `resume_from` | loss is bounded to the uncommitted window; replay is at-least-once |
//! | cancel mid-window → drain → resume | in-flight window is flushed + committed ⇒ resume produces **no duplicates** |
//! | flaky source items under `RetryOnError` | error *budget*: failed items are dropped (never re-pulled), counter resets on success |
//! | `process_item` hangs | `attempt_timeout` is the liveness ceiling; surfaces as `timeout` |

mod support;

use cano::prelude::*;
use futures_util::Stream;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{Flow, MemStore, boxed};

// ===========================================================================
// Crash mid-window, then resume from the committed cursor
// ===========================================================================

/// Emits `cursor+1 ..= 10`. Fails exactly once at a scripted item (FailFast policy),
/// recording every `process_item` call, every flushed window, and every `open` cursor.
struct MeterFeed {
    fail_once_at: Arc<Mutex<Option<u64>>>,
    processed: Arc<Mutex<Vec<u64>>>,
    flushed: Arc<Mutex<Vec<Vec<u64>>>>,
    opened_with: Arc<Mutex<Vec<Option<u64>>>>,
}

#[task::stream]
impl StreamTask<Flow> for MeterFeed {
    type Item = u64;
    type Output = u64;
    type Cursor = u64;

    fn window(&self) -> StreamWindow {
        StreamWindow::Count(3)
    }

    async fn open(
        &self,
        _res: &Resources,
        cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
        self.opened_with.lock().push(cursor);
        Ok(boxed(cursor.map_or(1, |c| c + 1)..=10))
    }

    async fn process_item(&self, _res: &Resources, item: u64) -> Result<(u64, u64), CanoError> {
        self.processed.lock().push(item);
        let mut fail = self.fail_once_at.lock();
        if *fail == Some(item) {
            *fail = None; // fail exactly once, like a transient poison read
            return Err(CanoError::task_execution(format!("meter {item} corrupt")));
        }
        Ok((item, item)) // output = item; cursor = position of this item
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<u64>,
    ) -> Result<WindowSignal<Flow>, CanoError> {
        self.flushed.lock().push(outputs);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        _reason: CloseReason,
    ) -> Result<TaskResult<Flow>, CanoError> {
        Ok(TaskResult::Single(Flow::Done))
    }
}

/// Contract: a crash between window flushes loses only the *uncommitted* buffer.
/// The engine persists the cursor after each flushed window, so `resume_from`
/// re-opens the source right after the last committed window and replays the
/// partially-consumed one — at-least-once, window-granular.
#[tokio::test]
async fn stream_crash_loses_only_uncommitted_window_and_resumes_from_cursor() {
    let processed = Arc::new(Mutex::new(Vec::new()));
    let flushed = Arc::new(Mutex::new(Vec::new()));
    let opened_with = Arc::new(Mutex::new(Vec::new()));
    let task = MeterFeed {
        fail_once_at: Arc::new(Mutex::new(Some(8))),
        processed: processed.clone(),
        flushed: flushed.clone(),
        opened_with: opened_with.clone(),
    };

    let store = Arc::new(MemStore::default());
    let workflow = Workflow::bare()
        .register_stream(Flow::Work, task)
        .add_exit_state(Flow::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id("stream-crash");

    // Run 1: windows [1,2,3] and [4,5,6] flush and commit; 7 is buffered; 8 errors.
    let err = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap_err();
    assert_eq!(err.category(), "task_execution", "got: {err}");
    assert_eq!(*flushed.lock(), vec![vec![1, 2, 3], vec![4, 5, 6]]);
    // The buffered-but-unflushed item 7 is NOT committed: the cursor stays at 6.
    assert_eq!(store.last_cursor("stream-crash", "Work"), Some(6));

    // Run 2: resume re-opens the source *after* the committed cursor.
    let result = workflow
        .resume_from("stream-crash", CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Flow::Done);
    assert_eq!(*opened_with.lock(), vec![None, Some(6)]);
    assert_eq!(
        *flushed.lock(),
        vec![vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9], vec![10]],
        "resume replays the uncommitted window and finishes the tail"
    );

    // At-least-once accounting: 7 and 8 were consumed in both runs (the replayed
    // window), everything else exactly once. This is why `process_item` must be
    // idempotent.
    let mut per_item: HashMap<u64, usize> = HashMap::new();
    for item in processed.lock().iter() {
        *per_item.entry(*item).or_default() += 1;
    }
    for item in 1..=6u64 {
        assert_eq!(per_item[&item], 1, "committed item {item} must not replay");
    }
    assert_eq!(per_item[&7], 2, "uncommitted item 7 replays on resume");
    assert_eq!(per_item[&8], 2, "failed item 8 replays on resume");
    for item in 9..=10u64 {
        assert_eq!(per_item[&item], 1);
    }

    // A successful run clears its checkpoint log.
    assert!(store.rows("stream-crash").is_empty());
}

// ===========================================================================
// Cooperative cancel: drain flushes + commits, resume dedups
// ===========================================================================

/// Emits `cursor+1 ..= 12` and fires its own cancellation handle after processing
/// a scripted item — deterministic (no timing races): cancellation is observed at
/// the next item boundary via the engine's biased select.
struct TickerFeed {
    cancel_at: u64,
    handle: Mutex<Option<CancellationHandle>>,
    processed: Arc<Mutex<Vec<u64>>>,
    flushed: Arc<Mutex<Vec<Vec<u64>>>>,
    closes: Arc<Mutex<Vec<String>>>,
}

#[task::stream]
impl StreamTask<Flow> for TickerFeed {
    type Item = u64;
    type Output = u64;
    type Cursor = u64;

    fn window(&self) -> StreamWindow {
        StreamWindow::Count(5)
    }

    async fn open(
        &self,
        _res: &Resources,
        cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
        Ok(boxed(cursor.map_or(1, |c| c + 1)..=12))
    }

    async fn process_item(&self, _res: &Resources, item: u64) -> Result<(u64, u64), CanoError> {
        self.processed.lock().push(item);
        if item == self.cancel_at
            && let Some(handle) = self.handle.lock().take()
        {
            handle.cancel(); // e.g. operator-initiated shutdown mid-window
        }
        Ok((item, item))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<u64>,
    ) -> Result<WindowSignal<Flow>, CanoError> {
        self.flushed.lock().push(outputs);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        reason: CloseReason,
    ) -> Result<TaskResult<Flow>, CanoError> {
        self.closes.lock().push(format!("{reason:?}"));
        Ok(TaskResult::Single(Flow::Done))
    }
}

/// Contract: cancel means "stop cleanly + resumable", not "transition onward".
/// The in-flight partial window is flushed, its cursor IS committed, and
/// `on_close(Cancelled)` runs — so the later resume continues right after the
/// drained window with **zero duplicates** (contrast with the crash test above,
/// where the uncommitted window replays).
#[tokio::test]
async fn stream_cancel_drains_partial_window_commits_cursor_and_resume_dedups() {
    let processed = Arc::new(Mutex::new(Vec::new()));
    let flushed = Arc::new(Mutex::new(Vec::new()));
    let closes = Arc::new(Mutex::new(Vec::new()));

    let (handle, token) = CancellationToken::new();
    let task = TickerFeed {
        cancel_at: 7,
        handle: Mutex::new(Some(handle)),
        processed: processed.clone(),
        flushed: flushed.clone(),
        closes: closes.clone(),
    };

    let store = Arc::new(MemStore::default());
    let workflow = Workflow::bare()
        .register_stream(Flow::Work, task)
        .add_exit_state(Flow::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id("stream-cancel");

    // Run 1: [1..=5] flushes normally; cancel fires inside item 7; the drain
    // flushes the partial [6,7] and commits cursor 7 before surfacing Cancelled.
    let err = workflow.orchestrate(Flow::Work, token).await.unwrap_err();
    assert_eq!(err.category(), "cancelled", "got: {err}");
    assert_eq!(
        *flushed.lock(),
        vec![vec![1, 2, 3, 4, 5], vec![6, 7]],
        "cancel drain must flush the in-flight partial window"
    );
    assert_eq!(*closes.lock(), vec!["Cancelled"]);
    assert_eq!(
        store.last_cursor("stream-cancel", "Work"),
        Some(7),
        "the drained window's cursor is committed — cancel is resumable"
    );

    // Run 2: resume picks up at 8; the run completes and nothing is reprocessed.
    let result = workflow
        .resume_from("stream-cancel", CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Flow::Done);
    assert_eq!(
        *flushed.lock(),
        vec![vec![1, 2, 3, 4, 5], vec![6, 7], vec![8, 9, 10, 11, 12]]
    );
    assert_eq!(*closes.lock(), vec!["Cancelled", "Exhausted"]);

    // Exactly-once effective delivery for the cancel path: the committed drain
    // means the resume introduces no duplicates.
    let all = processed.lock().clone();
    assert_eq!(all, (1..=12).collect::<Vec<_>>());
}

// ===========================================================================
// RetryOnError is an error *budget*, not a re-read
// ===========================================================================

/// Fails deterministically on a scripted set of items.
struct FlakyFeed {
    fail_items: Vec<u64>,
    budget: u32,
    flushed: Arc<Mutex<Vec<Vec<u64>>>>,
}

#[task::stream]
impl StreamTask<Flow> for FlakyFeed {
    type Item = u64;
    type Output = u64;
    type Cursor = u64;

    fn window(&self) -> StreamWindow {
        StreamWindow::Count(3)
    }

    fn on_item_error(&self) -> StreamErrorPolicy {
        StreamErrorPolicy::RetryOnError {
            max_errors: self.budget,
        }
    }

    async fn open(
        &self,
        _res: &Resources,
        cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
        Ok(boxed(cursor.map_or(1, |c| c + 1)..=10))
    }

    async fn process_item(&self, _res: &Resources, item: u64) -> Result<(u64, u64), CanoError> {
        if self.fail_items.contains(&item) {
            return Err(CanoError::task_execution(format!("record {item} rotten")));
        }
        Ok((item, item))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<u64>,
    ) -> Result<WindowSignal<Flow>, CanoError> {
        self.flushed.lock().push(outputs);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        _reason: CloseReason,
    ) -> Result<TaskResult<Flow>, CanoError> {
        Ok(TaskResult::Single(Flow::Done))
    }
}

/// Contract: a stream cannot re-pull a consumed item, so `RetryOnError` does NOT
/// retry the item — it *tolerates* up to `max_errors` consecutive failures, each
/// failed item being dropped, and the counter resets on every success. The run
/// dies on the (budget+1)-th consecutive failure, discarding the in-flight buffer.
#[tokio::test]
async fn stream_error_budget_drops_failed_items_resets_on_success_and_kills_on_burst() {
    // Bursts of 1 (item 2) and 2 (items 5,6) both fit budget=2: the run survives,
    // the rotten items are simply missing from the output.
    let flushed = Arc::new(Mutex::new(Vec::new()));
    let workflow = Workflow::bare()
        .register_stream(
            Flow::Work,
            FlakyFeed {
                fail_items: vec![2, 5, 6],
                budget: 2,
                flushed: flushed.clone(),
            },
        )
        .add_exit_state(Flow::Done);
    let result = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Flow::Done);
    assert_eq!(
        *flushed.lock(),
        vec![vec![1, 3, 4], vec![7, 8, 9], vec![10]],
        "failed items are dropped, not retried; survivors keep flowing"
    );

    // A burst of 3 consecutive failures (5,6,7) exceeds budget=2: the third error
    // propagates and the buffered-but-unflushed item 4 is discarded with the run.
    let flushed = Arc::new(Mutex::new(Vec::new()));
    let workflow = Workflow::bare()
        .register_stream(
            Flow::Work,
            FlakyFeed {
                fail_items: vec![5, 6, 7],
                budget: 2,
                flushed: flushed.clone(),
            },
        )
        .add_exit_state(Flow::Done);
    let err = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap_err();
    assert_eq!(err.category(), "task_execution");
    assert!(err.to_string().contains("record 7 rotten"), "got: {err}");
    assert_eq!(
        *flushed.lock(),
        vec![vec![1, 2, 3]],
        "only the fully-flushed window survives; the partial buffer is lost"
    );
}

// ===========================================================================
// A hung process_item is bounded by attempt_timeout
// ===========================================================================

/// Item 3 hangs "forever"; `attempt_timeout` must convert that into an item error.
struct StallFeed {
    flushed: Arc<Mutex<Vec<Vec<u64>>>>,
}

#[task::stream]
impl StreamTask<Flow> for StallFeed {
    type Item = u64;
    type Output = u64;
    type Cursor = u64;

    fn window(&self) -> StreamWindow {
        StreamWindow::Count(2)
    }

    fn config(&self) -> TaskConfig {
        // For streams only attempt_timeout is honored — as a per-item bound. This
        // is also what bounds cancel latency when items can hang.
        TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(50))
    }

    async fn open(
        &self,
        _res: &Resources,
        cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
        Ok(boxed(cursor.map_or(1, |c| c + 1)..=5))
    }

    async fn process_item(&self, _res: &Resources, item: u64) -> Result<(u64, u64), CanoError> {
        if item == 3 {
            tokio::time::sleep(Duration::from_secs(3600)).await; // dead upstream call
        }
        Ok((item, item))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<u64>,
    ) -> Result<WindowSignal<Flow>, CanoError> {
        self.flushed.lock().push(outputs);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        _reason: CloseReason,
    ) -> Result<TaskResult<Flow>, CanoError> {
        Ok(TaskResult::Single(Flow::Done))
    }
}

/// Contract: without `attempt_timeout` a hung item wedges the stream forever
/// (cancellation is only observed between items). With it, the hang becomes an
/// ordinary item error routed through the error policy — FailFast here, so the
/// run fails promptly with category `timeout`.
#[tokio::test]
async fn stream_hung_item_is_bounded_by_attempt_timeout() {
    let flushed = Arc::new(Mutex::new(Vec::new()));
    let workflow = Workflow::bare()
        .register_stream(
            Flow::Work,
            StallFeed {
                flushed: flushed.clone(),
            },
        )
        .add_exit_state(Flow::Done);

    let started = Instant::now();
    let err = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(err.category(), "timeout", "got: {err}");
    assert!(
        elapsed < Duration::from_secs(30),
        "liveness: the run must fail near the 50ms budget, took {elapsed:?}"
    );
    assert_eq!(
        *flushed.lock(),
        vec![vec![1, 2]],
        "work committed before the hang is preserved"
    );
}
