//! Reliability contracts of [`BatchTask`] under critical scenarios.
//!
//! Black-box integration tests (public API only) documenting how the batch
//! processing model behaves when items or the aggregation step fail:
//!
//! | Critical scenario | Contract |
//! |-------------------|----------|
//! | poisoned + flaky items, bounded concurrency | slot isolation, per-item retry budget, input-order slots, bulkhead bound |
//! | `finish` fails → outer retry | the **whole** load→process→finish cycle replays (at-least-once per item) |

mod support;

use cano::prelude::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;
use support::Flow;

// ===========================================================================
// Slot isolation, per-item retries, order, bulkhead
// ===========================================================================

/// 12 uploads, concurrency 3. Item 7 is permanently poisoned; item 4 succeeds on
/// its 3rd attempt. Records per-item attempts and the in-flight high-water mark.
struct Uploads {
    attempts: Arc<Mutex<HashMap<u32, u32>>>,
    in_flight: Arc<AtomicUsize>,
    high_water: Arc<AtomicUsize>,
    slots: Arc<Mutex<Vec<Option<u32>>>>, // finish snapshot: None = Err slot
}

#[task::batch]
impl BatchTask<Flow> for Uploads {
    type Item = u32;
    type ItemOutput = u32;

    fn concurrency(&self) -> usize {
        3
    }

    fn item_retry(&self) -> RetryMode {
        RetryMode::fixed(2, Duration::from_millis(1)) // 3 attempts per item
    }

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal() // no outer retry: isolate per-item semantics
    }

    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok((0..12).collect())
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.high_water.fetch_max(now, Ordering::SeqCst);
        let attempt = {
            let mut map = self.attempts.lock();
            let n = map.entry(*item).or_insert(0);
            *n += 1;
            *n
        };
        // Varied durations shuffle completion order — the order test below is real.
        tokio::time::sleep(Duration::from_millis(u64::from(*item % 3) * 4 + 1)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        if *item == 7 {
            return Err(CanoError::task_execution("upload 7: bucket gone"));
        }
        if *item == 4 && attempt < 3 {
            return Err(CanoError::task_execution("upload 4: flaky link"));
        }
        Ok(*item * 10)
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Flow>, CanoError> {
        *self.slots.lock() = outputs.into_iter().map(|r| r.ok()).collect();
        // Partial-failure policy lives HERE: one bad upload is acceptable.
        Ok(TaskResult::Single(Flow::Done))
    }
}

/// Contract: a failing item fills its own `finish` slot with `Err` after its retry
/// budget — it neither aborts siblings nor the batch. Slots arrive in *input*
/// order regardless of completion order, and at most `concurrency` items are ever
/// in flight (the data-level bulkhead).
#[tokio::test]
async fn batch_isolates_poison_items_retries_per_item_and_bounds_concurrency() {
    let attempts = Arc::new(Mutex::new(HashMap::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let high_water = Arc::new(AtomicUsize::new(0));
    let slots = Arc::new(Mutex::new(Vec::new()));

    let workflow = Workflow::bare()
        .register(
            Flow::Work,
            Uploads {
                attempts: attempts.clone(),
                in_flight: in_flight.clone(),
                high_water: high_water.clone(),
                slots: slots.clone(),
            },
        )
        .add_exit_state(Flow::Done);

    let result = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(
        result,
        Flow::Done,
        "one poisoned item must not fail the batch"
    );

    // Input-order slots: slot i belongs to item i even though completion order was
    // shuffled by the varied per-item durations.
    let slots = slots.lock().clone();
    assert_eq!(slots.len(), 12);
    for (i, slot) in slots.iter().enumerate() {
        if i == 7 {
            assert_eq!(*slot, None, "poisoned item lands as an Err slot");
        } else {
            assert_eq!(*slot, Some(i as u32 * 10), "slot {i} out of order");
        }
    }

    // Per-item retry accounting: the poisoned item exhausts its 3 attempts; the
    // flaky one recovers on attempt 3; healthy items run once.
    let attempts = attempts.lock().clone();
    assert_eq!(attempts[&7], 3, "poisoned item exhausts its retry budget");
    assert_eq!(attempts[&4], 3, "flaky item recovered on the 3rd attempt");
    for i in (0..12u32).filter(|i| *i != 4 && *i != 7) {
        assert_eq!(attempts[&i], 1, "healthy item {i} must not be retried");
    }

    // Bulkhead: never more than `concurrency` in flight — and genuinely parallel.
    let high = high_water.load(Ordering::SeqCst);
    assert!(high <= 3, "concurrency bound violated: {high} in flight");
    assert!(high >= 2, "expected real overlap, saw high-water {high}");
}

// ===========================================================================
// Outer retry replays the entire load → process → finish cycle
// ===========================================================================

/// Loads 4 records; `finish` fails a scripted number of times (a downstream
/// commit outage), counting every `load`/`process_item`/`finish` invocation.
struct CommitBatch {
    finish_failures_remaining: Arc<AtomicU32>,
    load_calls: Arc<AtomicU32>,
    item_calls: Arc<Mutex<HashMap<u32, u32>>>,
    finish_calls: Arc<AtomicU32>,
}

#[task::batch]
impl BatchTask<Flow> for CommitBatch {
    type Item = u32;
    type ItemOutput = u32;

    fn config(&self) -> TaskConfig {
        // Outer retry of the WHOLE cycle: 1 retry → 2 attempts.
        TaskConfig::minimal().with_fixed_retry(1, Duration::from_millis(1))
    }

    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        self.load_calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![1, 2, 3, 4])
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        *self.item_calls.lock().entry(*item).or_insert(0) += 1;
        Ok(*item)
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Flow>, CanoError> {
        self.finish_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(outputs.len(), 4);
        if self.finish_failures_remaining.load(Ordering::SeqCst) > 0 {
            self.finish_failures_remaining
                .fetch_sub(1, Ordering::SeqCst);
            return Err(CanoError::task_execution("commit endpoint 503"));
        }
        Ok(TaskResult::Single(Flow::Done))
    }
}

/// A `CommitBatch` plus the shared counters the test inspects after the run.
struct CommitProbe {
    task: CommitBatch,
    load_calls: Arc<AtomicU32>,
    item_calls: Arc<Mutex<HashMap<u32, u32>>>,
    finish_calls: Arc<AtomicU32>,
}

fn commit_batch(finish_failures: u32) -> CommitProbe {
    let load_calls = Arc::new(AtomicU32::new(0));
    let item_calls = Arc::new(Mutex::new(HashMap::new()));
    let finish_calls = Arc::new(AtomicU32::new(0));
    let task = CommitBatch {
        finish_failures_remaining: Arc::new(AtomicU32::new(finish_failures)),
        load_calls: load_calls.clone(),
        item_calls: item_calls.clone(),
        finish_calls: finish_calls.clone(),
    };
    CommitProbe {
        task,
        load_calls,
        item_calls,
        finish_calls,
    }
}

/// Contract: only `load` or `finish` failures trip the batch's *outer* retry, and
/// that retry replays the ENTIRE load → process → finish cycle. There is no
/// partial memoization: every item is reprocessed, so item side effects must be
/// idempotent. When the budget runs out the workflow reports `RetryExhausted`
/// with the true attempt count.
#[tokio::test]
async fn batch_outer_retry_replays_the_entire_cycle() {
    // finish fails once, second attempt lands.
    let probe = commit_batch(1);
    let workflow = Workflow::bare()
        .register(Flow::Work, probe.task)
        .add_exit_state(Flow::Done);
    let result = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Flow::Done);
    assert_eq!(probe.load_calls.load(Ordering::SeqCst), 2, "load re-ran");
    assert_eq!(probe.finish_calls.load(Ordering::SeqCst), 2);
    for (item, calls) in probe.item_calls.lock().iter() {
        assert_eq!(
            *calls, 2,
            "item {item} must be reprocessed by the outer retry"
        );
    }

    // finish fails on both attempts: the retry budget (2 attempts) is exhausted.
    let probe = commit_batch(2);
    let workflow = Workflow::bare()
        .register(Flow::Work, probe.task)
        .add_exit_state(Flow::Done);
    let err = workflow
        .orchestrate(Flow::Work, CancellationToken::disabled())
        .await
        .unwrap_err();
    // The engine adds state/path context around the failure; `category()` sees
    // through that wrapper to the RetryExhausted underneath.
    assert_eq!(err.category(), "retry_exhausted", "got: {err}");
    assert!(
        err.to_string().contains("after 2 attempt(s)"),
        "expected the true attempt count in the report, got: {err}"
    );
    assert_eq!(probe.load_calls.load(Ordering::SeqCst), 2, "no third cycle");
}
