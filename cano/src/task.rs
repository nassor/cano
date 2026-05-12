//! # Task API - Workflow Processing Interface
//!
//! This module provides the [`Task`] trait, the single interface for workflow processing units.
//! A [`Task`] requires implementing one of `run` / `run_bare`, giving you direct control over the execution flow.
//!
//! Tasks support retry strategies via [`TaskConfig`].
//!
//! ## Example
//!
//! When your task needs no external resources, implement [`Task::run_bare`] to avoid the
//! unused `_res` parameter:
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! enum Step { Process, Done }
//!
//! struct SimpleTask;
//!
//! #[task]
//! impl Task<Step> for SimpleTask {
//!     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
//!         // All work happens here with no external I/O
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//! ```
//!
//! Use [`Task::run`] when the task needs resources (store, config, HTTP client, etc.):
//!
//! - **`run_bare()`** — for pure computation with no external dependencies
//! - **`run()`** — for tasks that read from or write to [`Resources`] (store, params, clients)
//!
//! ## Using Params and Store as Resources
//!
//! A common pattern is to pass both a data store and typed configuration as resources:
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! enum Step { Fetch, Done }
//!
//! struct FetchParams { limit: usize }
//!
//! #[resource]
//! impl Resource for FetchParams {}
//!
//! struct FetchTask;
//!
//! #[task]
//! impl Task<Step> for FetchTask {
//!     async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         let store = res.get::<MemoryStore, _>("store")?;
//!         let params = res.get::<FetchParams, _>("params")?;
//!         let data: Vec<u32> = (0..params.limit as u32).collect();
//!         store.put("data", data)?;
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let store = MemoryStore::new();
//! let resources = Resources::new()
//!     .insert("store".to_owned(), store)
//!     .insert("params".to_owned(), FetchParams { limit: 10 });
//! let result = Workflow::new(resources)
//!     .register(Step::Fetch, FetchTask)
//!     .add_exit_state(Step::Done)
//!     .orchestrate(Step::Fetch)
//!     .await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```

use crate::error::CanoError;
use crate::resource::Resources;
use cano_macros::task;
use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;

pub mod batch;
pub mod poll;
mod retry;
pub mod router;
pub mod stepped;

pub use batch::{
    BatchTask, BatchTaskObject, DefaultBatchItem, DefaultBatchItemOutput, DynBatchTask, run_batch,
};
pub use poll::{
    DynPollTask, PollErrorPolicy, PollOutcome, PollTask, PollTaskObject, run_poll_loop,
};
pub use retry::{RetryMode, TaskConfig, run_with_retries};
pub use router::{DynRouterTask, RouterTask, RouterTaskObject};
pub use stepped::{
    DefaultStepCursor, DynSteppedTask, StepOutcome, SteppedTask, SteppedTaskObject, run_stepped,
};

// Attribute macros namespaced under `cano::task::` so that
// `#[task::router]`, `#[task::poll]`, `#[task::batch]`, and `#[task::stepped]`
// all resolve as path-qualified attribute macros.
// (Modules and macros occupy different namespaces, so these coexist with the
// `router`, `poll`, `batch`, and `stepped` submodules above.)
pub use cano_macros::batch_task as batch;
pub use cano_macros::poll_task as poll;
pub use cano_macros::router_task as router;
pub use cano_macros::stepped_task as stepped;

/// Result type for task execution that supports both single and split transitions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskResult<TState> {
    /// Transition to a single next state
    Single(TState),
    /// Split into multiple parallel states for concurrent execution
    Split(Vec<TState>),
}

/// Task trait for workflow processing
///
/// This is the single interface for workflow processing units. You implement one
/// of `run` / `run_bare`, giving you direct control over the execution flow.
///
/// # Generic Types
///
/// - **`TState`**: The return type that determines workflow routing (typically an enum)
/// - **`TResourceKey`**: The key type used to look up resources (defaults to
///   [`Cow<'static, str>`](std::borrow::Cow) — accepts `&'static str` literals
///   without allocating, plus owned `String` keys for runtime-built names)
///
/// # Benefits
///
/// - **Simplicity**: One method to implement
/// - **Flexibility**: Full control over execution flow
/// - **Type Safety**: Compile-time-checked state transitions
/// - **Performance**: Zero-cost abstraction with direct execution
///
/// # Example
///
/// ```rust,ignore
/// use cano::prelude::*;
///
/// struct DataProcessor {
///     multiplier: i32,
/// }
///
/// #[task]
/// impl Task<String> for DataProcessor {
///     async fn run(&self, res: &Resources) -> Result<TaskResult<String>, CanoError> {
///         let store = res.get::<MemoryStore, _>("store")?;
///         // Load data
///         let input: i32 = store.get("input").unwrap_or(1);
///
///         // Process
///         let result = input * self.multiplier;
///
///         // Store result
///         store.put("output", result)?;
///
///         // Determine next state
///         if result > 100 {
///             Ok(TaskResult::Single("large_result".to_string()))
///         } else {
///             Ok(TaskResult::Single("small_result".to_string()))
///         }
///     }
/// }
/// ```
#[task]
pub trait Task<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Get the task configuration that controls execution behavior
    ///
    /// Returns the TaskConfig that determines how this task should be executed.
    /// The default implementation returns `TaskConfig::default()` which configures
    /// the task with standard retry logic.
    ///
    /// Override this method to customize execution behavior:
    /// - Use `TaskConfig::minimal()` for fast-failing tasks with minimal retries
    /// - Use `TaskConfig::new().with_fixed_retry(n, duration)` for custom retry behavior
    /// - Return a custom configuration with specific retry/parameter settings
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
    }

    /// Human-readable identifier for this task, reported to
    /// [`WorkflowObserver`] hooks.
    ///
    /// The default returns [`std::any::type_name`] of the implementing type
    /// (e.g. `"my_crate::tasks::FetchTask"`). Override it to give a task a
    /// stable, friendlier name — useful when the type name is long or when
    /// several workflow states share one task type.
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Execute the task with access to shared resources.
    ///
    /// Override this when the task needs resources. The default implementation
    /// delegates to [`run_bare`](Self::run_bare) — **you must override one of the two**.
    /// Implementing neither is a programmer error: the default `run_bare` returns a
    /// [`CanoError::Configuration`] at runtime rather than panicking, so the
    /// workflow's retry/error path can surface it cleanly.
    ///
    /// # Errors
    ///
    /// Returns a [`CanoError`] propagated from the task logic.
    async fn run(&self, res: &Resources<TResourceKey>) -> Result<TaskResult<TState>, CanoError> {
        let _ = res;
        self.run_bare().await
    }

    /// Execute the task without resources.
    ///
    /// Override this instead of [`run`](Self::run) when the task needs no resources.
    /// This avoids an unused `_res: &Resources` parameter.
    ///
    /// The default implementation returns a [`CanoError::Configuration`]; if you see
    /// that error at runtime it means the [`Task`] impl forgot to override either
    /// `run` or `run_bare`.
    ///
    /// # Errors
    ///
    /// Returns a [`CanoError`] propagated from the task logic.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cano::prelude::*;
    ///
    /// #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    /// enum Step { Count, Done }
    ///
    /// struct CountTask;
    ///
    /// #[task]
    /// impl Task<Step> for CountTask {
    ///     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
    ///         Ok(TaskResult::Single(Step::Done))
    ///     }
    /// }
    /// ```
    async fn run_bare(&self) -> Result<TaskResult<TState>, CanoError> {
        Err(CanoError::configuration(format!(
            "Task<{}>: neither `run` nor `run_bare` was implemented; override one of them",
            std::any::type_name::<Self>(),
        )))
    }
}

/// Type alias for a dynamic task trait object.
///
/// Use this when you need to store different task types in the same collection.
/// `TResourceKey` defaults to [`Cow<'static, str>`](std::borrow::Cow) to match
/// [`Resources`]; pass an enum key type for typed
/// resource lookups.
///
/// ```
/// use cano::prelude::*;
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)] enum Step { Start, Done }
/// #[derive(Hash, Eq, PartialEq)] enum Key { Store }
///
/// struct First;
/// #[task]
/// impl Task<Step, Key> for First {
///     async fn run(&self, _res: &Resources<Key>) -> Result<TaskResult<Step>, CanoError> {
///         Ok(TaskResult::Single(Step::Done))
///     }
/// }
///
/// // Heterogeneous collection of enum-keyed tasks.
/// let tasks: Vec<TaskObject<Step, Key>> = vec![std::sync::Arc::new(First)];
/// assert_eq!(tasks.len(), 1);
/// ```
pub type DynTask<TState, TResourceKey = Cow<'static, str>> =
    dyn Task<TState, TResourceKey> + Send + Sync;

/// Type alias for an `Arc`-wrapped dynamic task trait object.
///
/// This alias simplifies working with dynamic task collections in workflows.
pub type TaskObject<TState, TResourceKey = Cow<'static, str>> =
    std::sync::Arc<DynTask<TState, TResourceKey>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use cano_macros::task;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio;

    // Test enum for task return values
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    #[allow(dead_code)]
    enum TestAction {
        Continue,
        Complete,
        Error,
        Retry,
    }

    // Simple task that always succeeds
    struct SimpleTask {
        execution_count: Arc<AtomicU32>,
    }

    impl SimpleTask {
        fn new() -> Self {
            Self {
                execution_count: Arc::new(AtomicU32::new(0)),
            }
        }

        fn execution_count(&self) -> u32 {
            self.execution_count.load(Ordering::SeqCst)
        }
    }

    #[task]
    impl Task<TestAction> for SimpleTask {
        async fn run_bare(&self) -> Result<TaskResult<TestAction>, CanoError> {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            Ok(TaskResult::Single(TestAction::Complete))
        }
    }

    // Task that can fail
    struct FailingTask {
        should_fail: bool,
    }

    impl FailingTask {
        fn new(should_fail: bool) -> Self {
            Self { should_fail }
        }
    }

    #[task]
    impl Task<TestAction> for FailingTask {
        async fn run_bare(&self) -> Result<TaskResult<TestAction>, CanoError> {
            if self.should_fail {
                Err(CanoError::task_execution("Task intentionally failed"))
            } else {
                Ok(TaskResult::Single(TestAction::Complete))
            }
        }
    }

    // Task that returns Split result
    struct SplitTask;

    #[task]
    impl Task<TestAction> for SplitTask {
        async fn run_bare(&self) -> Result<TaskResult<TestAction>, CanoError> {
            Ok(TaskResult::Split(vec![
                TestAction::Continue,
                TestAction::Complete,
            ]))
        }
    }

    #[tokio::test]
    async fn test_simple_task_execution() {
        let task = SimpleTask::new();

        let result = task.run_bare().await.unwrap();
        assert_eq!(result, TaskResult::Single(TestAction::Complete));
        assert_eq!(task.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_failing_task() {
        // Test successful task
        let success_task = FailingTask::new(false);
        let result = success_task.run_bare().await.unwrap();
        assert_eq!(result, TaskResult::Single(TestAction::Complete));

        // Test failing task
        let fail_task = FailingTask::new(true);
        let result = fail_task.run_bare().await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Task intentionally failed"));
    }

    #[tokio::test]
    async fn test_split_task() {
        let task = SplitTask;

        let result = task.run_bare().await.unwrap();
        assert_eq!(
            result,
            TaskResult::Split(vec![TestAction::Continue, TestAction::Complete])
        );
    }

    #[tokio::test]
    async fn test_unimplemented_run_returns_configuration_error() {
        // A Task that overrides neither `run` nor `run_bare` should surface a
        // Configuration error rather than panic, so workflow retry/error paths
        // can handle it cleanly.
        struct ForgotToImplement;

        #[task]
        impl Task<TestAction> for ForgotToImplement {}

        let task = ForgotToImplement;
        let res = Resources::new();
        let err = task.run(&res).await.unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(
            err.message().contains("ForgotToImplement"),
            "error should name the offending type, got: {}",
            err.message()
        );
    }

    #[tokio::test]
    async fn test_concurrent_task_execution() {
        use tokio::task;

        let task = Arc::new(SimpleTask::new());

        let mut handles = vec![];

        // Spawn multiple concurrent executions
        for _ in 0..10 {
            let task_clone = Arc::clone(&task);

            let handle = task::spawn(async move { task_clone.run_bare().await });
            handles.push(handle);
        }

        // Wait for all executions to complete
        let mut success_count = 0;
        for handle in handles {
            let result = handle.await.unwrap();
            if let Ok(TaskResult::Single(TestAction::Complete)) = result {
                success_count += 1;
            }
        }

        assert_eq!(success_count, 10);
        assert_eq!(task.execution_count(), 10);
    }

    #[tokio::test]
    async fn test_multiple_task_executions() {
        let task = SimpleTask::new();

        // Run the task multiple times
        for i in 1..=5 {
            let result = task.run_bare().await.unwrap();
            assert_eq!(result, TaskResult::Single(TestAction::Complete));
            assert_eq!(task.execution_count(), i);
        }
    }

    // ------------------------------------------------------------------
    // run_bare delegation tests
    // ------------------------------------------------------------------

    struct BareTask;

    #[task]
    impl Task<TestAction> for BareTask {
        async fn run_bare(&self) -> Result<TaskResult<TestAction>, CanoError> {
            Ok(TaskResult::Single(TestAction::Complete))
        }
    }

    struct ExplicitRunTask {
        bare_called: Arc<AtomicU32>,
    }

    #[task]
    impl Task<TestAction> for ExplicitRunTask {
        async fn run(&self, _res: &Resources) -> Result<TaskResult<TestAction>, CanoError> {
            Ok(TaskResult::Single(TestAction::Continue))
        }

        async fn run_bare(&self) -> Result<TaskResult<TestAction>, CanoError> {
            self.bare_called.fetch_add(1, Ordering::SeqCst);
            Ok(TaskResult::Single(TestAction::Error))
        }
    }

    #[tokio::test]
    async fn test_run_bare_called_when_run_not_overridden() {
        let task = BareTask;
        let res = Resources::new();
        let result = task.run(&res).await.unwrap();
        assert_eq!(result, TaskResult::Single(TestAction::Complete));
    }

    #[tokio::test]
    async fn test_run_overrides_bypass_bare() {
        let bare_called = Arc::new(AtomicU32::new(0));
        let task = ExplicitRunTask {
            bare_called: Arc::clone(&bare_called),
        };
        let res = Resources::new();
        let result = task.run(&res).await.unwrap();
        // run() override returns Continue, not Error from run_bare
        assert_eq!(result, TaskResult::Single(TestAction::Continue));
        // run_bare must never have been called
        assert_eq!(bare_called.load(Ordering::SeqCst), 0);
    }
}
