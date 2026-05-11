//! # BatchTask — Fan-Out-Over-Data Processing Model
//!
//! A [`BatchTask`] loads a `Vec` of work items, processes them concurrently (up to
//! [`BatchTask::concurrency`] parallel calls), and then aggregates the results in
//! **input order** via [`BatchTask::finish`]. The partial-failure policy lives entirely
//! in `finish` — returning `Err` from `process_item` puts an `Err` into the
//! corresponding slot of `finish`'s `outputs` argument; it does **not** abort the
//! whole batch or the outer task.
//!
//! ## When to use `BatchTask`
//!
//! - **Fan-out over data**: process N independent records (URLs to fetch, rows to
//!   transform, files to upload) with bounded concurrency.
//! - **Partial-failure tolerance**: decide in `finish` whether one failing item is
//!   acceptable (skip it, count it, collect errors) or fatal (return `Err` to abort).
//! - **Data-level parallelism in one state**: contrast with [`TaskResult::Split`], which
//!   fans out over FSM *states* (re-entering the workflow engine); `BatchTask` fans out
//!   over *data*, all within a single state transition.
//!
//! Every [`BatchTask`] automatically implements [`Task`](crate::task::Task) via a
//! per-impl-site companion `impl Task<S> for T` emitted by the `#[task::batch]` macro.
//! This means you can register a `BatchTask` with
//! [`Workflow::register`](crate::workflow::Workflow::register) exactly like any other task.
//!
//! ## Error semantics
//!
//! | Error origin | Propagation |
//! |---|---|
//! | `load` returns `Err` | Propagated immediately; `finish` is never called |
//! | `process_item` returns `Err` | Placed in the corresponding `outputs` slot; `finish` is called with that slot being `Err` |
//! | `finish` returns `Err` | Propagated to the workflow as a failed task run |
//!
//! The whole batch only `Err`s — triggering the workflow's outer retry of the entire
//! `load → process → finish` cycle — when `load` or `finish` returns `Err`.
//!
//! ## Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Process, Done }
//!
//! struct CsvProcessor;
//!
//! #[task::batch(state = Step)]
//! impl CsvProcessor {
//!     fn concurrency(&self) -> usize { 4 }
//!
//!     async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
//!         Ok(vec![1, 2, 3, 4, 5])
//!     }
//!
//!     async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
//!         Ok(item * 2)
//!     }
//!
//!     async fn finish(
//!         &self,
//!         _res: &Resources,
//!         outputs: Vec<Result<u32, CanoError>>,
//!     ) -> Result<TaskResult<Step>, CanoError> {
//!         let sum: u32 = outputs.into_iter().filter_map(|r| r.ok()).sum();
//!         assert_eq!(sum, 30); // 2+4+6+8+10
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let workflow = Workflow::bare()
//!     .register(Step::Process, CsvProcessor)
//!     .add_exit_state(Step::Done);
//!
//! let result = workflow.orchestrate(Step::Process).await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```
//!
//! ## Partial-failure tolerance
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Process, Done }
//!
//! struct TolerantProcessor;
//!
//! #[task::batch(state = Step)]
//! impl TolerantProcessor {
//!     async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
//!         Ok(vec![1, 2, 3])
//!     }
//!
//!     async fn process_item(&self, item: &u32) -> Result<String, CanoError> {
//!         if *item == 2 {
//!             Err(CanoError::task_execution("item 2 failed"))
//!         } else {
//!             Ok(format!("ok-{}", item))
//!         }
//!     }
//!
//!     async fn finish(
//!         &self,
//!         _res: &Resources,
//!         outputs: Vec<Result<String, CanoError>>,
//!     ) -> Result<TaskResult<Step>, CanoError> {
//!         // Partial failure: collect successes, skip errors.
//!         let successes: Vec<_> = outputs.into_iter().filter_map(|r| r.ok()).collect();
//!         assert_eq!(successes, vec!["ok-1", "ok-3"]);
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let workflow = Workflow::bare()
//!     .register(Step::Process, TolerantProcessor)
//!     .add_exit_state(Step::Done);
//! let result = workflow.orchestrate(Step::Process).await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```
//!
//! ## Per-item retries
//!
//! ```rust
//! use cano::prelude::*;
//! use std::time::Duration;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Process, Done }
//!
//! struct RetryingProcessor;
//!
//! #[task::batch(state = Step)]
//! impl RetryingProcessor {
//!     fn item_retry(&self) -> RetryMode {
//!         RetryMode::fixed(2, Duration::from_millis(1))
//!     }
//!
//!     async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
//!         Ok(vec![42])
//!     }
//!
//!     async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
//!         Ok(*item)
//!     }
//!
//!     async fn finish(
//!         &self,
//!         _res: &Resources,
//!         outputs: Vec<Result<u32, CanoError>>,
//!     ) -> Result<TaskResult<Step>, CanoError> {
//!         assert_eq!(outputs.len(), 1);
//!         assert!(outputs[0].is_ok());
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//! ```
//!
//! ## Contrast with `TaskResult::Split`
//!
//! | | `BatchTask` | `TaskResult::Split` |
//! |---|---|---|
//! | Fans out over | data items | FSM states |
//! | Items processed | within one state | each item re-enters the FSM |
//! | Result collected in | `finish` (same state) | `JoinConfig` at the split state's join |
//! | Order guaranteed | yes — inputs order preserved | no — split tasks may settle in any order |

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{RetryMode, TaskConfig, TaskResult};
use cano_macros::batch_task;
use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;

// ---------------------------------------------------------------------------
// Default associated-type aliases
// ---------------------------------------------------------------------------

/// Default type alias for `DynBatchTask` associated type `Item`.
///
/// Pins the `Item` associated type to a heap-erased value so the trait is object-safe.
pub type DefaultBatchItem = Box<dyn std::any::Any + Send + Sync>;

/// Default type alias for `DynBatchTask` associated type `ItemOutput`.
///
/// Pins the `ItemOutput` associated type to a heap-erased value so the trait is object-safe.
pub type DefaultBatchItemOutput = Box<dyn std::any::Any + Send>;

// ---------------------------------------------------------------------------
// BatchTask trait
// ---------------------------------------------------------------------------

/// A fan-out-over-data processing model that loads items, processes them with bounded
/// concurrency, and aggregates results in input order.
///
/// # Generic Types
///
/// - **`TState`**: The state enum used by the workflow (`Clone + Debug + Send + Sync`).
/// - **`TResourceKey`**: The key type used to look up resources (defaults to
///   [`Cow<'static, str>`]).
///
/// # Error semantics
///
/// - `load` returning `Err` aborts the batch immediately.
/// - `process_item` returning `Err` fills that slot in `finish`'s `outputs` argument
///   with `Err`; the rest of the batch continues unaffected.
/// - `finish` returning `Err` propagates to the workflow as a failed task.
///
/// # Inherent form
///
/// Prefer the `#[task::batch(state = S)]` inherent form:
///
/// ```rust
/// use cano::prelude::*;
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// enum Step { Process, Done }
///
/// struct FileUploader;
///
/// #[task::batch(state = Step)]
/// impl FileUploader {
///     fn concurrency(&self) -> usize { 8 }
///
///     async fn load(&self, _res: &Resources) -> Result<Vec<String>, CanoError> {
///         Ok(vec!["a.txt".into(), "b.txt".into()])
///     }
///
///     async fn process_item(&self, item: &String) -> Result<usize, CanoError> {
///         Ok(item.len()) // simulated upload size
///     }
///
///     async fn finish(
///         &self,
///         _res: &Resources,
///         outputs: Vec<Result<usize, CanoError>>,
///     ) -> Result<TaskResult<Step>, CanoError> {
///         let total: usize = outputs.into_iter().filter_map(|r| r.ok()).sum();
///         let _ = total;
///         Ok(TaskResult::Single(Step::Done))
///     }
/// }
/// ```
#[batch_task]
pub trait BatchTask<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// One work item produced by [`load`](BatchTask::load) and consumed by
    /// [`process_item`](BatchTask::process_item).
    type Item: Send + Sync + 'static;

    /// The per-item result produced by [`process_item`](BatchTask::process_item) and
    /// aggregated by [`finish`](BatchTask::finish).
    type ItemOutput: Send + 'static;

    /// Maximum number of concurrent [`process_item`](BatchTask::process_item) calls.
    ///
    /// Defaults to `1` (sequential). Clamped to `1` minimum internally so passing `0`
    /// is safe and will run items sequentially.
    fn concurrency(&self) -> usize {
        1
    }

    /// Retry policy applied to each [`process_item`](BatchTask::process_item) call
    /// independently.
    ///
    /// Defaults to [`RetryMode::None`] — no per-item retries. When set, a failing item
    /// attempt is retried up to the mode's `max_retries` before the slot is filled with
    /// `Err`.
    fn item_retry(&self) -> RetryMode {
        RetryMode::None
    }

    /// Retry configuration for the *enclosing* task dispatch (the whole
    /// `load → process → finish` cycle).
    ///
    /// Defaults to [`TaskConfig::default()`] (exponential backoff with 3 retries).
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
    }

    /// Human-readable identifier for this batch task, reported to
    /// [`WorkflowObserver`](crate::observer::WorkflowObserver) hooks.
    ///
    /// Defaults to [`std::any::type_name`] of the implementing type.
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Produce the work items.
    ///
    /// Called once per batch task dispatch. If this returns `Err`, the batch is aborted
    /// and [`finish`](BatchTask::finish) is never called.
    async fn load(&self, res: &Resources<TResourceKey>) -> Result<Vec<Self::Item>, CanoError>;

    /// Process one item.
    ///
    /// Takes `&Item` (not `Item`) because [`item_retry`](BatchTask::item_retry) may
    /// re-invoke it without consuming the item — items live in the `Vec` the engine owns
    /// for the duration of the batch. Returning `Err` does **not** fail the batch — the
    /// error lands in the corresponding slot of [`finish`](BatchTask::finish)'s `outputs`
    /// argument.
    async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError>;

    /// Aggregate the per-item results and decide the next workflow state.
    ///
    /// Called with one slot per input item, **in input order** — the first element of
    /// `outputs` corresponds to the first element returned by [`load`](BatchTask::load),
    /// regardless of which item finished processing first.
    ///
    /// This is where the partial-failure policy lives: return `Err` to abort and
    /// propagate a hard failure, or return `Ok(TaskResult::Single(next))` even in the
    /// presence of per-item errors if the policy allows partial success.
    async fn finish(
        &self,
        res: &Resources<TResourceKey>,
        outputs: Vec<Result<Self::ItemOutput, CanoError>>,
    ) -> Result<TaskResult<TState>, CanoError>;
}

// ---------------------------------------------------------------------------
// run_batch — the free fn the macro-synthesized Task::run delegates to
// ---------------------------------------------------------------------------

/// Run the [`BatchTask`] fan-out loop for `b`.
///
/// The synthesised `Task::run` method emitted by `#[task::batch]` delegates here so
/// that the loop body (which needs `futures_util::{StreamExt, stream::iter}`) lives
/// in one place that can reference those types directly.
///
/// **Execution flow:**
/// 1. Call `b.load(res)` — on `Err` return immediately.
/// 2. For each `(index, item)` pair, spawn an async closure that calls
///    `run_with_retries(item_cfg, || b.process_item(item))` — respecting
///    `b.item_retry()` per item.
/// 3. Drive all closures through `buffer_unordered(b.concurrency())` — at most
///    `concurrency` items in flight at once.
/// 4. Sort the `(index, result)` pairs by `index` to restore input order.
/// 5. Call `b.finish(res, outputs)` and return the result.
///
/// Items whose `process_item` returns `Err` (after exhausting per-item retries) are
/// placed in the `Err` slot of `outputs` — they do not abort the batch.
pub async fn run_batch<B, S, K>(b: &B, res: &Resources<K>) -> Result<TaskResult<S>, CanoError>
where
    B: BatchTask<S, K> + ?Sized,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    use futures_util::StreamExt as _;
    use std::sync::Arc;

    let items: Arc<Vec<B::Item>> = Arc::new(b.load(res).await?);

    let retry_mode = b.item_retry();
    let conc = b.concurrency().max(1);
    let n = items.len();

    // Use index-based iteration so each closure captures only an `Arc` clone and
    // an index — no lifetime tied to the local `items` Vec (which would cause
    // "FnOnce not general enough" errors with `buffer_unordered`).
    let mut indexed: Vec<(usize, Result<B::ItemOutput, CanoError>)> =
        futures_util::stream::iter(0..n)
            .map(|i| {
                let items_ref = Arc::clone(&items);
                let mode = retry_mode.clone();
                async move {
                    let result = run_item_with_retry(b, &items_ref[i], &mode).await;
                    (i, result)
                }
            })
            .buffer_unordered(conc)
            .collect()
            .await;

    // Restore input order — `buffer_unordered` may settle items out of order.
    indexed.sort_unstable_by_key(|(i, _)| *i);
    let outputs: Vec<Result<B::ItemOutput, CanoError>> =
        indexed.into_iter().map(|(_, r)| r).collect();

    b.finish(res, outputs).await
}

/// Inline per-item retry loop that only requires `ItemOutput: Send` (not `Sync`).
///
/// `run_with_retries` from `task::retry` has a `TState: Send + Sync` bound (needed
/// because task results are shared across workflow state transitions), but per-item
/// results are never shared — they are collected into a `Vec` and passed to `finish`.
/// This helper avoids the unnecessary `Sync` bound.
async fn run_item_with_retry<B, S, K>(
    b: &B,
    item: &B::Item,
    retry_mode: &RetryMode,
) -> Result<B::ItemOutput, CanoError>
where
    B: BatchTask<S, K> + ?Sized,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    let max_attempts = retry_mode.max_attempts();
    let mut attempt = 0usize;
    loop {
        match b.process_item(item).await {
            Ok(output) => return Ok(output),
            Err(e) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(e);
                }
                // Sleep before retry (fixed or exponential back-off).
                // `delay_for_attempt` takes the 0-based attempt index.
                if let Some(delay) = retry_mode.delay_for_attempt(attempt)
                    && delay.as_millis() > 0
                {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

/// Type alias for a dynamic [`BatchTask`] trait object with associated types pinned for
/// object safety.
///
/// Both `Item` and `ItemOutput` are pinned to heap-erased boxes so that
/// `dyn BatchTask<TState, TResourceKey>` can exist without knowing the concrete
/// associated types. Prefer `dyn Task<TState, TResourceKey>` for ordinary workflow use.
pub type DynBatchTask<TState, TResourceKey = Cow<'static, str>> = dyn BatchTask<TState, TResourceKey, Item = DefaultBatchItem, ItemOutput = DefaultBatchItemOutput>
    + Send
    + Sync;

/// Type alias for an `Arc`-wrapped dynamic [`BatchTask`] trait object.
pub type BatchTaskObject<TState, TResourceKey = Cow<'static, str>> =
    std::sync::Arc<DynBatchTask<TState, TResourceKey>>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::task::Task;
    use cano_macros::batch_task;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    // Note: all `#[batch_task]` usages inside the `cano` crate itself use the
    // trait-impl form (`impl BatchTask<S> for T`), because the inherent form emits
    // `::cano::BatchTask<...>` paths that don't resolve inside this crate. The
    // inherent form is tested in `cano-macros/tests/batch_task_impl.rs` where
    // `::cano` resolves correctly.

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum Step {
        Process,
        Done,
    }

    // -----------------------------------------------------------------------
    // (a) Sequential processing: items arrive at finish in input order
    // -----------------------------------------------------------------------

    struct IndexedBatch {
        n: usize,
    }

    #[batch_task]
    impl BatchTask<Step> for IndexedBatch {
        type Item = usize;
        type ItemOutput = usize;

        // default concurrency() = 1 (sequential)

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Ok((0..self.n).collect())
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            Ok(*item)
        }

        async fn finish(
            &self,
            _res: &Resources,
            outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            let got: Vec<usize> = outputs.into_iter().map(|r| r.unwrap()).collect();
            let expected: Vec<usize> = (0..self.n).collect();
            assert_eq!(got, expected, "finish must receive items in input order");
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_sequential_items_in_input_order() {
        let task = IndexedBatch { n: 5 };
        let res = Resources::new();
        let result = Task::run(&task, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // -----------------------------------------------------------------------
    // (b) Concurrent processing: items complete out of order but finish gets them
    //     in input order
    // -----------------------------------------------------------------------

    struct ConcurrentBatch {
        n: usize,
    }

    #[batch_task]
    impl BatchTask<Step> for ConcurrentBatch {
        type Item = usize;
        type ItemOutput = usize;

        fn concurrency(&self) -> usize {
            4
        }

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Ok((0..self.n).collect())
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            // Item i sleeps (n - i) ms so earlier items sleep longer → they finish LATER
            // without concurrency the order would be 3, 2, 1, 0.
            let delay = self.n.saturating_sub(*item) as u64;
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            Ok(*item)
        }

        async fn finish(
            &self,
            _res: &Resources,
            outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            let got: Vec<usize> = outputs.into_iter().map(|r| r.unwrap()).collect();
            let expected: Vec<usize> = (0..self.n).collect();
            assert_eq!(
                got, expected,
                "concurrent batch must preserve input order in finish"
            );
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_concurrent_items_preserve_input_order() {
        let task = ConcurrentBatch { n: 4 };
        let res = Resources::new();
        let result = Task::run(&task, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // -----------------------------------------------------------------------
    // (c) One failing item: finish sees that slot as Err, others as Ok;
    //     Task::run still returns Ok (finish decides policy)
    // -----------------------------------------------------------------------

    struct PartialFailBatch;

    #[batch_task]
    impl BatchTask<Step> for PartialFailBatch {
        type Item = usize;
        type ItemOutput = usize;

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Ok(vec![0, 1, 2])
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            if *item == 1 {
                Err(CanoError::task_execution("item 1 failed"))
            } else {
                Ok(*item)
            }
        }

        async fn finish(
            &self,
            _res: &Resources,
            outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            assert_eq!(outputs.len(), 3);
            assert!(outputs[0].is_ok(), "item 0 should be Ok");
            assert!(outputs[1].is_err(), "item 1 should be Err");
            assert!(outputs[2].is_ok(), "item 2 should be Ok");
            // Partial success: proceed despite one error
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_failing_item_lands_in_outputs_not_batch_error() {
        let task = PartialFailBatch;
        let res = Resources::new();
        // Task::run should succeed (finish returned Ok) even though one item failed
        let result = Task::run(&task, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // -----------------------------------------------------------------------
    // (d) Flaky item with item_retry: slot ends Ok after retries
    // -----------------------------------------------------------------------

    struct FlakySingleBatch {
        call_count: AtomicU32,
    }

    #[batch_task]
    impl BatchTask<Step> for FlakySingleBatch {
        type Item = u32;
        type ItemOutput = u32;

        fn item_retry(&self) -> RetryMode {
            RetryMode::fixed(2, Duration::from_millis(1))
        }

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Ok(vec![42])
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            let n = self.call_count.fetch_add(1, Ordering::Relaxed);
            // Fail on attempts 0 and 1, succeed on attempt 2
            if n < 2 {
                Err(CanoError::task_execution("transient failure"))
            } else {
                Ok(*item)
            }
        }

        async fn finish(
            &self,
            _res: &Resources,
            outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            assert_eq!(outputs.len(), 1);
            assert_eq!(
                outputs[0].as_ref().unwrap(),
                &42,
                "flaky item should succeed after retries"
            );
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_item_retry_recovers_flaky_item() {
        let task = FlakySingleBatch {
            call_count: AtomicU32::new(0),
        };
        let res = Resources::new();
        let result = Task::run(&task, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
        // 2 failures + 1 success = 3 total calls
        assert_eq!(task.call_count.load(Ordering::Relaxed), 3);
    }

    // -----------------------------------------------------------------------
    // (e) load returns Err: Task::run returns that Err; finish is never called
    // -----------------------------------------------------------------------

    struct LoadFailsBatch {
        finish_called: Arc<AtomicBool>,
    }

    #[batch_task]
    impl BatchTask<Step> for LoadFailsBatch {
        type Item = u32;
        type ItemOutput = u32;

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Err(CanoError::task_execution("load failed"))
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            Ok(*item)
        }

        async fn finish(
            &self,
            _res: &Resources,
            _outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            self.finish_called.store(true, Ordering::Relaxed);
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_load_error_propagates_and_finish_not_called() {
        let finish_called = Arc::new(AtomicBool::new(false));
        let task = LoadFailsBatch {
            finish_called: Arc::clone(&finish_called),
        };
        let res = Resources::new();
        let err = Task::run(&task, &res).await.unwrap_err();
        assert!(
            matches!(err, CanoError::TaskExecution(_)),
            "load Err should propagate as TaskExecution, got: {err:?}"
        );
        assert!(
            !finish_called.load(Ordering::Relaxed),
            "finish must not be called when load fails"
        );
    }

    // -----------------------------------------------------------------------
    // (f) finish returns Err: Task::run propagates that Err
    // -----------------------------------------------------------------------

    struct FinishFailsBatch;

    #[batch_task]
    impl BatchTask<Step> for FinishFailsBatch {
        type Item = u32;
        type ItemOutput = u32;

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Ok(vec![1, 2])
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            Ok(*item)
        }

        async fn finish(
            &self,
            _res: &Resources,
            _outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            Err(CanoError::task_execution("finish failed"))
        }
    }

    #[tokio::test]
    async fn test_finish_error_propagates() {
        let task = FinishFailsBatch;
        let res = Resources::new();
        let err = Task::run(&task, &res).await.unwrap_err();
        assert!(
            matches!(err, CanoError::TaskExecution(_)),
            "finish Err should propagate, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // (g) concurrency = 0 is treated as 1 (no deadlock)
    // -----------------------------------------------------------------------

    struct ZeroConcurrencyBatch;

    #[batch_task]
    impl BatchTask<Step> for ZeroConcurrencyBatch {
        type Item = u32;
        type ItemOutput = u32;

        fn concurrency(&self) -> usize {
            0 // should be clamped to 1
        }

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Ok(vec![1, 2, 3])
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            Ok(*item)
        }

        async fn finish(
            &self,
            _res: &Resources,
            outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            assert_eq!(outputs.len(), 3);
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_zero_concurrency_does_not_deadlock() {
        let task = ZeroConcurrencyBatch;
        let res = Resources::new();
        let result = Task::run(&task, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // -----------------------------------------------------------------------
    // (h) Dynamic dispatch: Arc<dyn Task<S>>
    // -----------------------------------------------------------------------

    struct SimpleBatch;

    #[batch_task]
    impl BatchTask<Step> for SimpleBatch {
        type Item = u32;
        type ItemOutput = u32;

        async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
            Ok(vec![7])
        }

        async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
            Ok(*item)
        }

        async fn finish(
            &self,
            _res: &Resources,
            outputs: Vec<Result<Self::ItemOutput, CanoError>>,
        ) -> Result<TaskResult<Step>, CanoError> {
            let val = outputs[0].as_ref().unwrap();
            assert_eq!(*val, 7);
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_batch_task_as_dyn_task() {
        let task: Arc<dyn Task<Step>> = Arc::new(SimpleBatch);
        let res = Resources::new();
        let result = Task::run(task.as_ref(), &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // -----------------------------------------------------------------------
    // (i) Workflow integration
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_batch_task_in_workflow() {
        use crate::workflow::Workflow;

        let workflow = Workflow::bare()
            .register(Step::Process, IndexedBatch { n: 3 })
            .add_exit_state(Step::Done);

        let result = workflow.orchestrate(Step::Process).await.unwrap();
        assert_eq!(result, Step::Done);
    }

    // -----------------------------------------------------------------------
    // (j) run_batch via ?Sized bound
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_run_batch_dyn_dispatch() {
        let task = SimpleBatch;
        let res = Resources::new();
        // Can call run_batch via a reference to the trait object
        let result = run_batch::<SimpleBatch, Step, _>(&task, &res)
            .await
            .unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }
}
