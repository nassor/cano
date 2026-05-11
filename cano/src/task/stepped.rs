//! # SteppedTask — Resumable-Iterative Processing Model
//!
//! A [`SteppedTask`] processes work in discrete, resumable steps. Each call to
//! [`step`](SteppedTask::step) either advances the cursor (`StepOutcome::More(cursor)`) or signals
//! completion with a workflow state transition (`StepOutcome::Done(state)`). The cursor threading
//! enables crash-resume: when registered via
//! [`Workflow::register_stepped`](crate::workflow::Workflow::register_stepped) with a
//! checkpoint store attached, each `StepOutcome::More` persists the cursor as a
//! [`RowKind::StepCursor`](crate::recovery::RowKind::StepCursor) row so
//! [`Workflow::resume_from`](crate::workflow::Workflow::resume_from) can continue from the
//! last persisted cursor rather than restarting from `None`.
//!
//! ## When to use `SteppedTask`
//!
//! - **Large dataset processing**: scan a table page-by-page using a continuation token.
//! - **Chunked uploads or migrations**: advance an offset cursor through a large payload.
//! - **Progress-saving long jobs**: the cursor captures exactly where to resume after a
//!   restart.
//!
//! Every [`SteppedTask`] automatically implements [`Task`](crate::task::Task) via a
//! per-impl-site companion `impl Task<S> for T` emitted by the `#[task::stepped]` macro.
//! This means you can register a `SteppedTask` with
//! [`Workflow::register`](crate::workflow::Workflow::register) exactly like any other task.
//!
//! ## Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum MyState { Process, Done }
//!
//! struct PageScanner { total_pages: u32 }
//!
//! #[task::stepped(state = MyState)]
//! impl PageScanner {
//!     async fn step(
//!         &self,
//!         _res: &Resources,
//!         cursor: Option<u32>,
//!     ) -> Result<StepOutcome<u32, MyState>, CanoError> {
//!         let page = cursor.unwrap_or(0);
//!         if page + 1 >= self.total_pages {
//!             Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
//!         } else {
//!             Ok(StepOutcome::More(page + 1))
//!         }
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let scanner = PageScanner { total_pages: 3 };
//! let workflow = Workflow::bare()
//!     .register(MyState::Process, scanner)
//!     .add_exit_state(MyState::Done);
//!
//! let result = workflow.orchestrate(MyState::Process).await?;
//! assert_eq!(result, MyState::Done);
//! # Ok(())
//! # }
//! ```
//!
//! ## The trait-impl form
//!
//! You can also write the full `impl SteppedTask<S> for T` header explicitly:
//!
//! ```rust
//! use cano::prelude::*;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum MyState { Process, Done }
//!
//! #[derive(Serialize, Deserialize)]
//! struct MyCursor { offset: u64 }
//!
//! struct TraitStepper;
//!
//! #[task::stepped]
//! impl SteppedTask<MyState> for TraitStepper {
//!     type Cursor = MyCursor;
//!
//!     async fn step(
//!         &self,
//!         _res: &Resources,
//!         cursor: Option<MyCursor>,
//!     ) -> Result<StepOutcome<MyCursor, MyState>, CanoError> {
//!         let _ = cursor;
//!         Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let workflow = Workflow::bare()
//!     .register(MyState::Process, TraitStepper)
//!     .add_exit_state(MyState::Done);
//!
//! let result = workflow.orchestrate(MyState::Process).await?;
//! assert_eq!(result, MyState::Done);
//! # Ok(())
//! # }
//! ```

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{TaskConfig, TaskResult};
use cano_macros::stepped_task;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// StepOutcome<TCursor, TState> — the outcome of a single step() call
// ---------------------------------------------------------------------------

/// The outcome returned by a single [`SteppedTask::step`] call.
///
/// - [`StepOutcome::More`] — more work remains; the cursor is threaded to the next call.
/// - [`StepOutcome::Done`] — processing is complete; carry the [`TaskResult`] forward to the FSM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome<TCursor, TState> {
    /// More steps remain. The cursor will be passed to the next `step` call.
    More(TCursor),
    /// All steps are complete. The inner [`TaskResult`] is forwarded to the FSM.
    Done(TaskResult<TState>),
}

// ---------------------------------------------------------------------------
// SteppedTask trait
// ---------------------------------------------------------------------------

/// A resumable-iterative processing model that advances work step by step via a cursor.
///
/// # Generic Types
///
/// - **`TState`**: The state enum used by the workflow (`Clone + Debug + Send + Sync`).
/// - **`TResourceKey`**: The key type used to look up resources (defaults to
///   [`Cow<'static, str>`]).
///
/// # Associated Types
///
/// - **`Cursor`**: Serialisable position marker threaded through each step call.
///   Must implement `Serialize + DeserializeOwned + Send + Sync + 'static` so it can be
///   persisted for crash-resume (Task 17).
///
/// # Default config
///
/// `SteppedTask` defaults to [`TaskConfig::default()`] (exponential backoff with 3 retries).
/// The step loop is the forward-progress mechanism; outer retry wrapping guards against
/// transient errors on individual step calls.
///
/// # Implementing SteppedTask
///
/// Prefer the inherent `#[task::stepped(state = S)]` form:
///
/// ```rust
/// use cano::prelude::*;
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// enum MyState { Process, Done }
///
/// struct MyProcessor { limit: u32 }
///
/// #[task::stepped(state = MyState)]
/// impl MyProcessor {
///     async fn step(
///         &self,
///         _res: &Resources,
///         cursor: Option<u32>,
///     ) -> Result<StepOutcome<u32, MyState>, CanoError> {
///         let n = cursor.unwrap_or(0);
///         if n + 1 >= self.limit {
///             Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
///         } else {
///             Ok(StepOutcome::More(n + 1))
///         }
///     }
/// }
/// ```
#[stepped_task]
pub trait SteppedTask<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// The cursor type that tracks progress between steps.
    ///
    /// Must be serialisable so it can be persisted for crash-resume (Task 17).
    type Cursor: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Get the task configuration that controls execution behaviour.
    ///
    /// Defaults to [`TaskConfig::default()`] (exponential backoff with 3 retries).
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
    }

    /// Human-readable identifier for this stepper, reported to [`WorkflowObserver`] hooks.
    ///
    /// Defaults to [`std::any::type_name`] of the implementing type. Override for a
    /// stable, friendly name.
    ///
    /// [`WorkflowObserver`]: crate::observer::WorkflowObserver
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Execute one step of the task.
    ///
    /// - `cursor`: `None` on the first call; `Some(cursor)` on subsequent calls with
    ///   the value returned by the previous `step` via [`StepOutcome::More`].
    ///
    /// Return [`StepOutcome::More`] with an updated cursor to continue, or [`StepOutcome::Done`] with
    /// a [`TaskResult`] when processing is complete.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError`] on a non-recoverable error.
    async fn step(
        &self,
        res: &Resources<TResourceKey>,
        cursor: Option<Self::Cursor>,
    ) -> Result<StepOutcome<Self::Cursor, TState>, CanoError>;
}

// ---------------------------------------------------------------------------
// run_stepped — the loop body called by the macro-synthesised Task::run
// ---------------------------------------------------------------------------

/// Run the [`SteppedTask`] step loop for `s`.
///
/// The synthesised `Task::run` method emitted by `#[task::stepped]` delegates here so
/// that the loop body lives in a single place. This in-memory loop is used when a
/// `SteppedTask` is registered via [`Workflow::register`](crate::workflow::Workflow::register).
///
/// For checkpoint-backed cursor persistence, register via
/// [`Workflow::register_stepped`](crate::workflow::Workflow::register_stepped) instead;
/// the engine will use [`ErasedSteppedTask`] to persist each cursor as a
/// [`RowKind::StepCursor`](crate::recovery::RowKind::StepCursor) row after each `StepOutcome::More`.
///
/// The loop calls `s.step(res, cursor)` repeatedly:
///
/// - `StepOutcome::More(new_cursor)` → update the cursor and call `step` again.
/// - `StepOutcome::Done(result)` → return `Ok(result)`.
/// - `Err(e)` → return `Err(e)` immediately (outer retry wrapping, if any, is applied
///   by the workflow dispatcher via `run_with_retries`).
pub async fn run_stepped<S, S2, K>(s: &S, res: &Resources<K>) -> Result<TaskResult<S2>, CanoError>
where
    S: SteppedTask<S2, K> + ?Sized,
    S2: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    let mut cursor: Option<S::Cursor> = None;

    loop {
        match s.step(res, cursor).await? {
            StepOutcome::More(new_cursor) => {
                cursor = Some(new_cursor);
            }
            StepOutcome::Done(result) => return Ok(result),
        }
    }
}

// ---------------------------------------------------------------------------
// Default cursor type alias
// ---------------------------------------------------------------------------

/// Default cursor type for [`DynSteppedTask`] trait objects.
///
/// Using `Vec<u8>` pins the cursor to an opaque byte buffer, enabling object-safe
/// dynamic dispatch at the cost of requiring manual serialisation in `step`.
pub type DefaultStepCursor = Vec<u8>;

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

/// Type alias for a dynamic [`SteppedTask`] trait object with a `Vec<u8>` cursor.
///
/// Pins `Cursor = Vec<u8>` for object safety — the cursor is an opaque byte buffer.
pub type DynSteppedTask<TState, TResourceKey = Cow<'static, str>> =
    dyn SteppedTask<TState, TResourceKey, Cursor = Vec<u8>> + Send + Sync;

/// Type alias for an `Arc`-wrapped dynamic [`SteppedTask`] trait object.
pub type SteppedTaskObject<TState, TResourceKey = Cow<'static, str>> =
    std::sync::Arc<DynSteppedTask<TState, TResourceKey>>;

// ---------------------------------------------------------------------------
// Type-erased stepped-task infrastructure (for StateEntry::Stepped)
// ---------------------------------------------------------------------------

/// The outcome of one erased step call: serialized cursor bytes (`More`) or the
/// final [`TaskResult`] (`Done`).
pub enum ErasedStep<TState> {
    /// More work remains; the opaque cursor bytes are passed to the next call.
    More(Vec<u8>),
    /// Processing is complete; carry the [`TaskResult`] forward to the FSM.
    Done(TaskResult<TState>),
}

/// Future type returned by [`ErasedSteppedTask::step`].
pub type StepFuture<'a, TState> =
    Pin<Box<dyn Future<Output = Result<ErasedStep<TState>, CanoError>> + Send + 'a>>;

/// Object-safe, type-erased view of a [`SteppedTask`].
///
/// All cursor serialization/deserialization is handled inside the internal
/// adapter that bridges a concrete `SteppedTask` to this trait; callers work
/// purely with `Vec<u8>` cursors.
pub trait ErasedSteppedTask<TState, TResourceKey>: Send + Sync
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Human-readable identifier, forwarded from the underlying [`SteppedTask::name`].
    fn name(&self) -> Cow<'static, str>;
    /// Task configuration, forwarded from the underlying [`SteppedTask::config`].
    fn config(&self) -> TaskConfig;
    /// Execute one step with a type-erased, optionally `None` cursor.
    ///
    /// `cursor_bytes` is `None` on the first call; `Some(bytes)` on subsequent calls,
    /// where `bytes` were returned by the previous `step` via [`ErasedStep::More`].
    fn step<'a>(
        &'a self,
        res: &'a Resources<TResourceKey>,
        cursor_bytes: Option<Vec<u8>>,
    ) -> StepFuture<'a, TState>;
}

/// Bridges a concrete [`SteppedTask`] to the object-safe [`ErasedSteppedTask`].
///
/// Handles `serde_json` cursor (de)serialization so the engine only sees `Vec<u8>`.
pub(crate) struct SteppedAdapter<T>(pub Arc<T>);

impl<TState, TResourceKey, T> ErasedSteppedTask<TState, TResourceKey> for SteppedAdapter<T>
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
    T: SteppedTask<TState, TResourceKey> + 'static,
{
    fn name(&self) -> Cow<'static, str> {
        self.0.name()
    }
    fn config(&self) -> TaskConfig {
        self.0.config()
    }
    fn step<'a>(
        &'a self,
        res: &'a Resources<TResourceKey>,
        cursor_bytes: Option<Vec<u8>>,
    ) -> StepFuture<'a, TState> {
        Box::pin(async move {
            let cursor: Option<T::Cursor> = match cursor_bytes {
                None => None,
                Some(ref b) => Some(serde_json::from_slice(b).map_err(|e| {
                    CanoError::task_execution(format!(
                        "deserialize cursor for `{}`: {e}",
                        self.0.name()
                    ))
                })?),
            };
            match self.0.step(res, cursor).await? {
                StepOutcome::More(c) => {
                    let blob = serde_json::to_vec(&c).map_err(|e| {
                        CanoError::task_execution(format!(
                            "serialize cursor for `{}`: {e}",
                            self.0.name()
                        ))
                    })?;
                    Ok(ErasedStep::More(blob))
                }
                StepOutcome::Done(result) => Ok(ErasedStep::Done(result)),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::task::Task;
    use cano_macros::stepped_task;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    // Note: all `#[stepped_task]` usages inside the `cano` crate itself use the
    // trait-impl form (`impl SteppedTask<S> for T`), because the inherent form emits
    // `::cano::SteppedTask<...>` paths that don't resolve inside this crate. The
    // inherent form is tested in `cano-macros/tests/stepped_task_impl.rs` where
    // `::cano` resolves correctly.

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum MyState {
        Work,
        Done,
        Next,
    }

    // ---------------------------------------------------------------------------
    // (a) SteppedTask that completes on the first step
    // ---------------------------------------------------------------------------

    struct ImmediateStepper;

    #[stepped_task]
    impl SteppedTask<MyState> for ImmediateStepper {
        type Cursor = u32;

        async fn step(
            &self,
            _res: &Resources,
            _cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, MyState>, CanoError> {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        }
    }

    #[tokio::test]
    async fn test_stepped_task_immediate_via_step() {
        let stepper = ImmediateStepper;
        let res = Resources::new();
        let result = SteppedTask::step(&stepper, &res, None).await.unwrap();
        assert_eq!(result, StepOutcome::Done(TaskResult::Single(MyState::Done)));
    }

    #[tokio::test]
    async fn test_stepped_task_immediate_via_task_run() {
        let stepper = ImmediateStepper;
        let res = Resources::new();
        let result = Task::run(&stepper, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(MyState::Done));
    }

    // ---------------------------------------------------------------------------
    // (b) SteppedTask that takes multiple steps before Done
    // ---------------------------------------------------------------------------

    struct CountingStepper {
        target: u32,
    }

    impl CountingStepper {
        fn new(target: u32) -> Self {
            Self { target }
        }
    }

    #[stepped_task]
    impl SteppedTask<MyState> for CountingStepper {
        type Cursor = u32;

        async fn step(
            &self,
            _res: &Resources,
            cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, MyState>, CanoError> {
            let n = cursor.unwrap_or(0) + 1;
            if n >= self.target {
                Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
            } else {
                Ok(StepOutcome::More(n))
            }
        }
    }

    #[tokio::test]
    async fn test_stepped_task_multiple_steps() {
        let stepper = CountingStepper::new(5);
        let res = Resources::new();
        let result = Task::run(&stepper, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(MyState::Done));
    }

    #[tokio::test]
    async fn test_stepped_task_cursor_threading() {
        // Verify that cursor is correctly threaded: starts at None, ends at target-1
        let stepper = CountingStepper::new(3);
        let res = Resources::new();

        // Step 1: cursor None → More(1)
        let r1 = SteppedTask::step(&stepper, &res, None).await.unwrap();
        assert_eq!(r1, StepOutcome::More(1));

        // Step 2: cursor Some(1) → More(2)
        let r2 = SteppedTask::step(&stepper, &res, Some(1)).await.unwrap();
        assert_eq!(r2, StepOutcome::More(2));

        // Step 3: cursor Some(2) → Done
        let r3 = SteppedTask::step(&stepper, &res, Some(2)).await.unwrap();
        assert_eq!(r3, StepOutcome::Done(TaskResult::Single(MyState::Done)));
    }

    // ---------------------------------------------------------------------------
    // (c) Error propagates immediately
    // ---------------------------------------------------------------------------

    struct ErrorStepper;

    #[stepped_task]
    impl SteppedTask<MyState> for ErrorStepper {
        type Cursor = u32;

        async fn step(
            &self,
            _res: &Resources,
            _cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, MyState>, CanoError> {
            Err(CanoError::task_execution("step failed"))
        }
    }

    #[tokio::test]
    async fn test_stepped_task_error_propagates() {
        let stepper = ErrorStepper;
        let res = Resources::new();
        let err = Task::run(&stepper, &res).await.unwrap_err();
        assert!(matches!(err, CanoError::TaskExecution(_)));
    }

    // ---------------------------------------------------------------------------
    // (d) SteppedTask returning Split
    // ---------------------------------------------------------------------------

    struct SplitStepper;

    #[stepped_task]
    impl SteppedTask<MyState> for SplitStepper {
        type Cursor = u32;

        async fn step(
            &self,
            _res: &Resources,
            _cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, MyState>, CanoError> {
            Ok(StepOutcome::Done(TaskResult::Split(vec![
                MyState::Work,
                MyState::Next,
            ])))
        }
    }

    #[tokio::test]
    async fn test_stepped_task_split() {
        let stepper = SplitStepper;
        let res = Resources::new();
        let result = Task::run(&stepper, &res).await.unwrap();
        assert_eq!(
            result,
            TaskResult::Split(vec![MyState::Work, MyState::Next])
        );
    }

    // ---------------------------------------------------------------------------
    // (e) Config + name overrides
    // ---------------------------------------------------------------------------

    struct CustomStepper;

    #[stepped_task]
    impl SteppedTask<MyState> for CustomStepper {
        type Cursor = u32;

        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }

        fn name(&self) -> Cow<'static, str> {
            Cow::Borrowed("my-custom-stepper")
        }

        async fn step(
            &self,
            _res: &Resources,
            _cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, MyState>, CanoError> {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        }
    }

    #[test]
    fn test_stepped_task_config_override() {
        let stepper = CustomStepper;
        assert_eq!(
            SteppedTask::<MyState>::config(&stepper)
                .retry_mode
                .max_attempts(),
            1
        );
    }

    #[test]
    fn test_stepped_task_name_override() {
        let stepper = CustomStepper;
        assert_eq!(SteppedTask::<MyState>::name(&stepper), "my-custom-stepper");
    }

    #[test]
    fn test_companion_task_forwards_config_and_name() {
        let stepper = CustomStepper;
        assert_eq!(Task::config(&stepper).retry_mode.max_attempts(), 1);
        assert_eq!(Task::name(&stepper), "my-custom-stepper");
    }

    // ---------------------------------------------------------------------------
    // (f) Default config is TaskConfig::default() (exponential backoff)
    // ---------------------------------------------------------------------------

    struct DefaultConfigStepper;

    #[stepped_task]
    impl SteppedTask<MyState> for DefaultConfigStepper {
        type Cursor = u32;

        async fn step(
            &self,
            _res: &Resources,
            _cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, MyState>, CanoError> {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        }
    }

    #[test]
    fn test_stepped_task_default_config_has_retries() {
        let stepper = DefaultConfigStepper;
        assert!(
            SteppedTask::<MyState>::config(&stepper)
                .retry_mode
                .max_attempts()
                > 1,
            "SteppedTask default config must have retries"
        );
    }

    #[test]
    fn test_stepped_task_default_name_contains_type_name() {
        let stepper = DefaultConfigStepper;
        let name = SteppedTask::<MyState>::name(&stepper);
        assert!(
            name.contains("DefaultConfigStepper"),
            "default name should contain the type name, got: {name}",
        );
    }

    // ---------------------------------------------------------------------------
    // (g) Dynamic dispatch: Arc<dyn Task<S>>
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_stepped_task_as_dyn_task() {
        let stepper: Arc<dyn Task<MyState>> = Arc::new(ImmediateStepper);
        let res = Resources::new();
        let result = Task::run(stepper.as_ref(), &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(MyState::Done));
    }

    // ---------------------------------------------------------------------------
    // (h) run_stepped with dyn SteppedTask (?Sized)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_run_stepped_dyn_dispatch() {
        let stepper: &dyn SteppedTask<MyState, Cursor = u32> = &ImmediateStepper;
        let res = Resources::new();
        let result = run_stepped(stepper, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(MyState::Done));
    }

    // ---------------------------------------------------------------------------
    // (i) Step enum equality and clone
    // ---------------------------------------------------------------------------

    #[test]
    fn test_step_enum_more_clone_eq() {
        let s1: StepOutcome<u32, MyState> = StepOutcome::More(42);
        let s2 = s1.clone();
        assert_eq!(s1, s2);
    }

    #[test]
    fn test_step_enum_done_clone_eq() {
        let s1: StepOutcome<u32, MyState> = StepOutcome::Done(TaskResult::Single(MyState::Done));
        let s2 = s1.clone();
        assert_eq!(s1, s2);
    }

    // ---------------------------------------------------------------------------
    // (j) Workflow integration
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_stepped_task_in_workflow() {
        use crate::workflow::Workflow;
        use cano_macros::task;

        struct NextTask;

        #[task]
        impl Task<MyState> for NextTask {
            async fn run_bare(&self) -> Result<TaskResult<MyState>, CanoError> {
                Ok(TaskResult::Single(MyState::Done))
            }
        }

        let stepper = CountingStepper::new(3);

        let workflow = Workflow::bare()
            .register(MyState::Work, stepper)
            .register(MyState::Next, NextTask)
            .add_exit_state(MyState::Done);

        let result = workflow.orchestrate(MyState::Work).await.unwrap();
        assert_eq!(result, MyState::Done);
    }

    // ---------------------------------------------------------------------------
    // (k) AtomicU32 step count verification
    // ---------------------------------------------------------------------------

    struct TrackedStepper {
        calls: Arc<AtomicU32>,
        target: u32,
    }

    impl TrackedStepper {
        fn new(target: u32) -> (Self, Arc<AtomicU32>) {
            let calls = Arc::new(AtomicU32::new(0));
            (
                Self {
                    calls: Arc::clone(&calls),
                    target,
                },
                calls,
            )
        }
    }

    #[stepped_task]
    impl SteppedTask<MyState> for TrackedStepper {
        type Cursor = u32;

        async fn step(
            &self,
            _res: &Resources,
            cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, MyState>, CanoError> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            let pos = cursor.unwrap_or(0) + 1;
            if n + 1 >= self.target {
                Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
            } else {
                Ok(StepOutcome::More(pos))
            }
        }
    }

    #[tokio::test]
    async fn test_stepped_task_step_count() {
        let (stepper, calls) = TrackedStepper::new(4);
        let res = Resources::new();
        let result = Task::run(&stepper, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(MyState::Done));
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }
}
