//! # Task API - Workflow Processing Interface
//!
//! This module provides the [`Task`] trait, the single interface for workflow processing units.
//! A [`Task`] implements `run`, giving you direct control over the execution flow; it returns
//! the FSM's next state.
//!
//! Tasks support retry strategies via [`TaskConfig`]; by default a task runs once (no retries).
//!
//! ## Example
//!
//! When your task needs no external resources, leave out the resources parameter — the
//! `#[task]` macro adds it:
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
//!     async fn run(&self) -> Result<Step, CanoError> {
//!         // All work happens here with no external I/O
//!         Ok(Step::Done)
//!     }
//! }
//! ```
//!
//! Take `res: &Resources` when the task needs resources (store, config, HTTP client, etc.):
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
//!     async fn run(&self, res: &Resources) -> Result<Step, CanoError> {
//!         let store = res.get::<MemoryStore>()?;
//!         let params = res.get::<FetchParams>()?;
//!         let data: Vec<u32> = (0..params.limit as u32).collect();
//!         store.put("data", data);
//!         Ok(Step::Done)
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let store = MemoryStore::new();
//! let resources = Resources::new()
//!     .insert(store)
//!     .insert(FetchParams { limit: 10 });
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

pub mod batch;
pub mod poll;
mod retry;
pub mod router;
pub mod stepped;
pub mod stream;
pub mod suspend;
pub mod timer;

pub use batch::{BatchTask, run_batch};
pub use poll::{PollErrorPolicy, PollOutcome, PollTask, run_poll_loop};
pub(crate) use retry::{ObserverCtx, run_with_retries_observed};
pub use retry::{RetryMode, TaskConfig, run_with_retries};
pub use router::RouterTask;
pub use stepped::{StepOutcome, SteppedTask, run_stepped};
pub use stream::{CloseReason, StreamErrorPolicy, StreamTask, StreamWindow, WindowSignal};
pub use suspend::SuspendTask;
pub use timer::{TimerOutcome, TimerTask, run_timer};

// Attribute macros namespaced under `cano::task::` so that
// `#[task::router]`, `#[task::poll]`, `#[task::timer]`, `#[task::batch]`,
// `#[task::stepped]`, `#[task::stream]` and `#[task::suspend]` all resolve as
// path-qualified attribute macros. (Modules and macros occupy different namespaces, so
// these coexist with the submodules of the same names above.)
pub use cano_macros::batch_task as batch;
pub use cano_macros::poll_task as poll;
pub use cano_macros::router_task as router;
pub use cano_macros::stepped_task as stepped;
pub use cano_macros::stream_task as stream;
pub use cano_macros::suspend_task as suspend;
pub use cano_macros::timer_task as timer;

/// Task trait for workflow processing
///
/// This is the single interface for workflow processing units: implement `run`,
/// which returns the FSM's next state.
///
/// # Generic Types
///
/// - **`TState`**: The return type that determines workflow routing (typically an enum)
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
/// ```rust
/// use cano::prelude::*;
///
/// struct DataProcessor {
///     multiplier: i32,
/// }
///
/// #[task]
/// impl Task<String> for DataProcessor {
///     async fn run(&self, res: &Resources) -> Result<String, CanoError> {
///         let store = res.get::<MemoryStore>()?;
///         // Load data
///         let input: i32 = store.get("input").unwrap_or(1);
///
///         // Process
///         let result = input * self.multiplier;
///
///         // Store result
///         store.put("output", result);
///
///         // Determine next state
///         if result > 100 {
///             Ok("large_result".to_string())
///         } else {
///             Ok("small_result".to_string())
///         }
///     }
/// }
/// ```
#[task]
pub trait Task<TState>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
{
    /// Get the task configuration that controls execution behavior.
    ///
    /// The default is [`TaskConfig::new()`]: one attempt, no retries, no timeout.
    /// Override it to opt into retries or an attempt timeout, e.g.
    /// `TaskConfig::new().with_exponential_retry(3)`.
    fn config(&self) -> TaskConfig {
        TaskConfig::new()
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

    /// Execute the task and return the next state.
    ///
    /// With `#[task]`, a task that needs no resources may write `async fn run(&self)`;
    /// the macro adds the `res` parameter.
    ///
    /// # Errors
    ///
    /// Returns a [`CanoError`] propagated from the task logic.
    async fn run(&self, res: &Resources) -> Result<TState, CanoError>;
}

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
        async fn run(&self) -> Result<TestAction, CanoError> {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            Ok(TestAction::Complete)
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
        async fn run(&self) -> Result<TestAction, CanoError> {
            if self.should_fail {
                Err(CanoError::task_execution("Task intentionally failed"))
            } else {
                Ok(TestAction::Complete)
            }
        }
    }

    #[tokio::test]
    async fn test_simple_task_execution() {
        let task = SimpleTask::new();

        let result = task.run(&Resources::new()).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(task.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_failing_task() {
        // Test successful task
        let success_task = FailingTask::new(false);
        let result = success_task.run(&Resources::new()).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        // Test failing task
        let fail_task = FailingTask::new(true);
        let result = fail_task.run(&Resources::new()).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Task intentionally failed"));
    }

    #[tokio::test]
    async fn test_concurrent_task_execution() {
        use tokio::task;

        let task = Arc::new(SimpleTask::new());

        let mut handles = vec![];

        // Spawn multiple concurrent executions
        for _ in 0..10 {
            let task_clone = Arc::clone(&task);

            let handle = task::spawn(async move { task_clone.run(&Resources::new()).await });
            handles.push(handle);
        }

        // Wait for all executions to complete
        let mut success_count = 0;
        for handle in handles {
            let result = handle.await.unwrap();
            if let Ok(TestAction::Complete) = result {
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
            let result = task.run(&Resources::new()).await.unwrap();
            assert_eq!(result, TestAction::Complete);
            assert_eq!(task.execution_count(), i);
        }
    }
}
