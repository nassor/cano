//! # Task API - Simplified Workflow Interface
//!
//! This module provides the [`Task`] trait, which offers a simplified interface for workflow processing.
//! A [`Task`] only requires implementing a single `run` method, giving you direct control over the execution flow.
//!
//! ## Key Differences
//!
//! - **[`Task`]**: Simple interface with a single `run` method. It's great for straightforward operations and quick prototyping.
//! - **[`crate::node::Node`]**: A more structured interface with a three-phase lifecycle (`prep`, `exec`, `post`). It's ideal for complex operations where separating concerns is beneficial.
//!
//! Both `Task` and `Node` support retry strategies.
//!
//! ## Relationship & Compatibility
//!
//! **Every [`crate::node::Node`] automatically implements [`Task`]** through a blanket implementation. This means:
//! - You can use any existing `Node` wherever a `Task` is expected.
//! - Workflows can register both `Task`s and `Node`s using the same `register()` method.
//! - This provides a seamless migration path from a simple `Task` to a more structured `Node` if complexity increases.
//!
//! ## When to Use Which
//!
//! - Use **[`Task`]** for simplicity and direct control.
//! - Use **[`crate::node::Node`]** for complex logic that benefits from a structured, multi-phase approach.
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
//!         let store = res.get::<MemoryStore, str>("store")?;
//!         let params = res.get::<FetchParams, str>("params")?;
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
//!
//! ## Interoperability
//!
//! Every [`crate::node::Node`] automatically implements [`Task`], so you can use existing nodes
//! wherever tasks are expected. This provides a smooth upgrade path and backward compatibility.

use crate::error::CanoError;
use crate::resource::Resources;
use cano_macros::task;
use rand::RngExt;
use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;
use std::time::Duration;

#[cfg(feature = "tracing")]
use tracing::{debug, error, info, info_span, instrument, warn};

/// Retry modes for task execution
///
/// Defines different retry strategies that can be used when task execution fails.
#[derive(Debug, Clone)]
pub enum RetryMode {
    /// No retries - fail immediately on first error
    None,

    /// Fixed number of retries with constant delay
    ///
    /// # Fields
    /// - `retries`: Number of retry attempts
    /// - `delay`: Fixed delay between attempts
    Fixed { retries: usize, delay: Duration },

    /// Exponential backoff with optional jitter
    ///
    /// Implements exponential backoff: delay = base_delay * multiplier^attempt + jitter
    ///
    /// # Fields
    /// - `max_retries`: Maximum number of retry attempts
    /// - `base_delay`: Initial delay duration
    /// - `multiplier`: Exponential multiplier (typically 2.0)
    /// - `max_delay`: Maximum delay cap to prevent excessive waits
    /// - `jitter`: Add randomness to prevent thundering herd (0.0 to 1.0)
    ExponentialBackoff {
        max_retries: usize,
        base_delay: Duration,
        multiplier: f64,
        max_delay: Duration,
        jitter: f64,
    },
}

impl RetryMode {
    /// Create a fixed retry mode with specified retries and delay
    pub fn fixed(retries: usize, delay: Duration) -> Self {
        Self::Fixed { retries, delay }
    }

    /// Create an exponential backoff retry mode with sensible defaults
    ///
    /// Uses base_delay=100ms, multiplier=2.0, max_delay=30s, jitter=0.1
    pub fn exponential(max_retries: usize) -> Self {
        Self::ExponentialBackoff {
            max_retries,
            base_delay: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(30),
            jitter: 0.1,
        }
    }

    /// Create a custom exponential backoff retry mode
    pub fn exponential_custom(
        max_retries: usize,
        base_delay: Duration,
        multiplier: f64,
        max_delay: Duration,
        jitter: f64,
    ) -> Self {
        Self::ExponentialBackoff {
            max_retries,
            base_delay,
            multiplier,
            max_delay,
            jitter: jitter.clamp(0.0, 1.0), // Ensure jitter is between 0 and 1
        }
    }

    /// Get the maximum number of attempts (initial + retries)
    pub fn max_attempts(&self) -> usize {
        match self {
            Self::None => 1,
            Self::Fixed { retries, .. } => retries + 1,
            Self::ExponentialBackoff { max_retries, .. } => max_retries + 1,
        }
    }

    /// Calculate delay for a specific attempt number (0-based)
    pub fn delay_for_attempt(&self, attempt: usize) -> Option<Duration> {
        match self {
            Self::None => None,
            Self::Fixed { retries, delay } => {
                if attempt < *retries {
                    Some(*delay)
                } else {
                    None
                }
            }
            Self::ExponentialBackoff {
                max_retries,
                base_delay,
                multiplier,
                max_delay,
                jitter,
            } => {
                if attempt < *max_retries {
                    let base_ms = base_delay.as_millis() as f64;
                    let exponential_delay = base_ms * multiplier.powi(attempt as i32);
                    let capped_delay = exponential_delay.min(max_delay.as_millis() as f64);

                    // Add jitter: delay * (1 ± jitter * random_factor)
                    let jitter_factor = if *jitter > 0.0 {
                        let mut rng = rand::rng();
                        let random_factor: f64 = rng.random_range(-1.0..=1.0);
                        1.0 + (jitter * random_factor)
                    } else {
                        1.0
                    };

                    let final_delay_f = (capped_delay * jitter_factor).max(0.0);
                    // Saturate rather than wrap or panic when the computed delay
                    // exceeds u64::MAX milliseconds (e.g. enormous max_delay + jitter).
                    let final_delay = if final_delay_f >= u64::MAX as f64 {
                        u64::MAX
                    } else {
                        final_delay_f as u64
                    };
                    Some(Duration::from_millis(final_delay))
                } else {
                    None
                }
            }
        }
    }
}

impl Default for RetryMode {
    fn default() -> Self {
        Self::ExponentialBackoff {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(30),
            jitter: 0.1,
        }
    }
}

/// Task configuration for retry behavior and parameters
///
/// This struct provides configuration for task execution behavior,
/// including retry logic and custom parameters.
#[must_use]
#[derive(Clone, Default)]
pub struct TaskConfig {
    /// Retry strategy for failed executions
    pub retry_mode: RetryMode,
}

impl TaskConfig {
    /// Create a new TaskConfig with default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a minimal configuration with no retries
    ///
    /// Useful for tasks that should fail fast without any retry attempts.
    pub fn minimal() -> Self {
        Self {
            retry_mode: RetryMode::None,
        }
    }

    /// Set the retry mode for this configuration
    pub fn with_retry(mut self, retry_mode: RetryMode) -> Self {
        self.retry_mode = retry_mode;
        self
    }

    /// Convenience method for fixed retry configuration
    pub fn with_fixed_retry(self, retries: usize, delay: Duration) -> Self {
        self.with_retry(RetryMode::fixed(retries, delay))
    }

    /// Convenience method for exponential backoff retry configuration
    pub fn with_exponential_retry(self, max_retries: usize) -> Self {
        self.with_retry(RetryMode::exponential(max_retries))
    }
}

/// Default implementation for retry logic that can be used by any task
///
/// This function provides a standard retry mechanism that can be used by any task
/// that implements a simple run function.
#[cfg_attr(feature = "tracing", instrument(
    skip(config, run_fn),
    fields(max_attempts = config.retry_mode.max_attempts())
))]
pub async fn run_with_retries<TState, F, Fut>(
    config: &TaskConfig,
    run_fn: F,
) -> Result<TState, CanoError>
where
    TState: Send + Sync,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<TState, CanoError>>,
{
    let max_attempts = config.retry_mode.max_attempts();
    let mut attempt = 0;

    #[cfg(feature = "tracing")]
    info!(max_attempts, "Starting task execution with retry logic");

    loop {
        #[cfg(feature = "tracing")]
        let attempt_span = info_span!("task_attempt", attempt = attempt + 1, max_attempts);

        #[cfg(feature = "tracing")]
        let _span_guard = attempt_span.enter();

        #[cfg(feature = "tracing")]
        debug!(attempt = attempt + 1, "Executing task attempt");

        match run_fn().await {
            Ok(result) => {
                #[cfg(feature = "tracing")]
                info!(attempt = attempt + 1, "Task execution successful");
                return Ok(result);
            }
            Err(e) => {
                attempt += 1;

                #[cfg(feature = "tracing")]
                if attempt >= max_attempts {
                    error!(
                        error = %e,
                        final_attempt = attempt,
                        max_attempts,
                        "Task execution failed after all retry attempts"
                    );
                } else {
                    warn!(
                        error = %e,
                        attempt,
                        max_attempts,
                        "Task execution failed, will retry"
                    );
                }

                if attempt >= max_attempts {
                    if max_attempts <= 1 {
                        return Err(e);
                    }
                    return Err(CanoError::retry_exhausted(format!(
                        "Task failed after {} attempt(s): {}",
                        attempt, e
                    )));
                } else if let Some(delay) = config.retry_mode.delay_for_attempt(attempt - 1) {
                    #[cfg(feature = "tracing")]
                    debug!(delay_ms = delay.as_millis(), "Waiting before retry");

                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

/// Result type for task execution that supports both single and split transitions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskResult<TState> {
    /// Transition to a single next state
    Single(TState),
    /// Split into multiple parallel states for concurrent execution
    Split(Vec<TState>),
}

/// Task trait for simplified workflow processing
///
/// This trait provides a simplified interface for workflow processing compared to [`crate::node::Node`].
/// Instead of implementing three separate phases (`prep`, `exec`, `post`), you only need
/// to implement a single `run` method.
///
/// # Relationship with Node
///
/// **Every [`crate::node::Node`] automatically implements [`Task`]** through a blanket implementation.
/// This means [`crate::node::Node`] is a superset of [`Task`] with additional structure and retry capabilities:
/// - [`Task`]: Simple `run()` method - great for prototypes and simple operations
/// - [`crate::node::Node`]: Three-phase lifecycle + retry strategies - ideal for production workloads
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
/// - **Simplicity**: Single method to implement instead of three
/// - **Flexibility**: Full control over execution flow
/// - **Compatibility**: Works seamlessly with existing [`crate::node::Node`] implementations
/// - **Type Safety**: Same type safety guarantees as [`crate::node::Node`]
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
///         let store = res.get::<MemoryStore, str>("store")?;
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

/// Blanket implementation: Every Node is automatically a Task
///
/// This implementation makes all existing [`crate::node::Node`] implementations automatically
/// work as [`Task`] implementations. This means [`crate::node::Node`] is a superset of [`Task`]:
///
/// - **[`Task`]**: Simple `run()` method
/// - **[`crate::node::Node`]**: Three-phase lifecycle (`prep`, `exec`, `post`) + retry strategies
///
/// This enables:
/// - Using any Node wherever Tasks are expected
/// - Mixing Tasks and Nodes in the same workflow
/// - Gradual migration from simple Tasks to full-featured Nodes
///
/// # Retry contract
///
/// This blanket `Task::run` executes exactly **one** `prep` → `exec` → `post` pass with no
/// retries. Retries are driven by the workflow dispatcher's outer `run_with_retries` call,
/// which uses this single-pass method as the unit of work.
///
/// **Do not call [`crate::node::Node::run`] inside a `Task::run` override for a Node** —
/// `Node::run` applies its own retry loop, so doing so would retry twice: once inside
/// `Node::run` and again in the workflow dispatcher.
#[task]
impl<TState, TResourceKey, N> Task<TState, TResourceKey> for N
where
    N: crate::node::Node<TState, TResourceKey>,
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn config(&self) -> TaskConfig {
        crate::node::Node::config(self)
    }

    #[cfg_attr(
        feature = "tracing",
        instrument(skip(self, res), fields(task_type = "node_adapter"))
    )]
    async fn run(&self, res: &Resources<TResourceKey>) -> Result<TaskResult<TState>, CanoError> {
        #[cfg(feature = "tracing")]
        debug!("Executing task through Node adapter");

        // Run a single attempt of prep → exec → post without the Node's own retry loop.
        // Retries are driven by the outer `run_with_retries` in both `execute_single_task` and
        // `execute_split_join`, which use this method as the unit of work. Calling `Node::run`
        // here would double-retry nodes (inner Node::run_with_retries + outer run_with_retries).
        let prep_result = crate::node::Node::prep(self, res).await?;
        let exec_result = crate::node::Node::exec(self, prep_result).await;
        let next_state = crate::node::Node::post(self, res, exec_result).await?;

        #[cfg(feature = "tracing")]
        info!(next_state = ?next_state, "Task execution completed successfully");

        Ok(TaskResult::Single(next_state))
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
    use cano_macros::{node, task};
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

    // Test that demonstrates Node -> Task compatibility
    use crate::node::Node;

    struct TestNode;

    #[node]
    impl Node<TestAction> for TestNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
            Ok("node_prepared".to_string())
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            prep_res == "node_prepared"
        }

        async fn post(
            &self,
            _res: &Resources,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            if exec_res {
                Ok(TestAction::Complete)
            } else {
                Ok(TestAction::Error)
            }
        }
    }

    #[tokio::test]
    async fn test_node_as_task_compatibility() {
        let node = TestNode;
        let res = Resources::new();

        // Use the node as a task - this should work due to the blanket implementation
        let result = Task::run(&node, &res).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TaskResult::Single(TestAction::Complete));
    }

    // Tests for RetryMode and TaskConfig
    #[test]
    fn test_retry_mode_none() {
        let retry_mode = RetryMode::None;

        assert_eq!(retry_mode.max_attempts(), 1);
        assert_eq!(retry_mode.delay_for_attempt(0), None);
        assert_eq!(retry_mode.delay_for_attempt(1), None);
    }

    #[test]
    fn test_retry_mode_fixed() {
        let retry_mode = RetryMode::fixed(3, Duration::from_millis(100));

        assert_eq!(retry_mode.max_attempts(), 4); // 1 initial + 3 retries

        // Test delay calculations
        assert_eq!(
            retry_mode.delay_for_attempt(0),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(1),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(2),
            Some(Duration::from_millis(100))
        );
        assert_eq!(retry_mode.delay_for_attempt(3), None); // No more retries
        assert_eq!(retry_mode.delay_for_attempt(4), None);
    }

    #[test]
    fn test_retry_mode_exponential_basic() {
        let retry_mode = RetryMode::exponential(3);

        assert_eq!(retry_mode.max_attempts(), 4); // 1 initial + 3 retries

        // Test that delays increase (exact values may vary due to jitter)
        let delay0 = retry_mode.delay_for_attempt(0).unwrap();
        let delay1 = retry_mode.delay_for_attempt(1).unwrap();
        let delay2 = retry_mode.delay_for_attempt(2).unwrap();

        // With exponential backoff, each delay should generally be larger
        // (allowing for some jitter variance)
        assert!(delay1.as_millis() >= delay0.as_millis() / 2); // Account for negative jitter
        assert!(delay2.as_millis() >= delay1.as_millis() / 2);

        // No delay for attempts beyond max_retries
        assert_eq!(retry_mode.delay_for_attempt(3), None);
        assert_eq!(retry_mode.delay_for_attempt(4), None);
    }

    #[test]
    fn test_retry_mode_exponential_custom() {
        let retry_mode = RetryMode::exponential_custom(
            2,                         // max_retries
            Duration::from_millis(50), // base_delay
            3.0,                       // multiplier
            Duration::from_secs(5),    // max_delay
            0.0,                       // no jitter
        );

        assert_eq!(retry_mode.max_attempts(), 3);

        // With no jitter, delays should be predictable
        // attempt 0: 50ms * 3^0 = 50ms
        // attempt 1: 50ms * 3^1 = 150ms
        // attempt 2: None (beyond max_retries)
        assert_eq!(
            retry_mode.delay_for_attempt(0),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(1),
            Some(Duration::from_millis(150))
        );
        assert_eq!(retry_mode.delay_for_attempt(2), None);
    }

    #[test]
    fn test_retry_mode_exponential_max_delay_cap() {
        let retry_mode = RetryMode::exponential_custom(
            5,
            Duration::from_millis(100), // base_delay
            10.0,                       // high multiplier
            Duration::from_millis(500), // low max_delay cap
            0.0,                        // no jitter
        );

        // All delays should be capped at max_delay
        let delay0 = retry_mode.delay_for_attempt(0).unwrap();
        let delay1 = retry_mode.delay_for_attempt(1).unwrap();
        let delay2 = retry_mode.delay_for_attempt(2).unwrap();

        assert_eq!(delay0, Duration::from_millis(100)); // 100 * 10^0 = 100
        assert_eq!(delay1, Duration::from_millis(500)); // 100 * 10^1 = 1000, capped to 500
        assert_eq!(delay2, Duration::from_millis(500)); // Capped to 500
    }

    #[test]
    fn test_retry_mode_exponential_jitter_bounds() {
        let retry_mode = RetryMode::exponential_custom(
            3,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            0.5, // 50% jitter
        );

        // Run multiple times to test jitter variability
        let mut delays = Vec::new();
        for _ in 0..20 {
            if let Some(delay) = retry_mode.delay_for_attempt(0) {
                delays.push(delay.as_millis());
            }
        }

        // With 50% jitter, delays should vary between 50ms and 150ms (100ms ± 50%)
        // Due to randomness, we'll check that we get some variation
        let min_delay = delays.iter().min().unwrap();
        let max_delay = delays.iter().max().unwrap();

        // Should have some variation due to jitter
        assert!(*min_delay >= 50); // 100ms - 50% = 50ms minimum
        assert!(*max_delay <= 150); // 100ms + 50% = 150ms maximum
    }

    #[test]
    fn test_retry_mode_jitter_clamping() {
        // Test that jitter values outside [0, 1] are clamped
        let retry_mode1 = RetryMode::exponential_custom(
            1,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            -0.5, // Should be clamped to 0.0
        );

        let retry_mode2 = RetryMode::exponential_custom(
            1,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            1.5, // Should be clamped to 1.0
        );

        // Both should work without panicking
        assert!(retry_mode1.delay_for_attempt(0).is_some());
        assert!(retry_mode2.delay_for_attempt(0).is_some());
    }

    #[test]
    fn test_retry_mode_default() {
        let retry_mode = RetryMode::default();

        // Default should be exponential backoff with 3 retries
        assert_eq!(retry_mode.max_attempts(), 4);

        // Should have delays for first 3 attempts
        assert!(retry_mode.delay_for_attempt(0).is_some());
        assert!(retry_mode.delay_for_attempt(1).is_some());
        assert!(retry_mode.delay_for_attempt(2).is_some());
        assert!(retry_mode.delay_for_attempt(3).is_none());
    }

    #[test]
    fn test_task_config_creation() {
        let config = TaskConfig::new();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_task_config_default() {
        let config = TaskConfig::default();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_task_config_minimal() {
        let config = TaskConfig::minimal();
        assert_eq!(config.retry_mode.max_attempts(), 1);
    }

    #[test]
    fn test_task_config_with_fixed_retry() {
        let config = TaskConfig::new().with_fixed_retry(5, Duration::from_millis(100));

        assert_eq!(config.retry_mode.max_attempts(), 6);
    }

    #[test]
    fn test_task_config_builder_pattern() {
        let config = TaskConfig::new().with_fixed_retry(10, Duration::from_secs(1));

        assert_eq!(config.retry_mode.max_attempts(), 11);
    }

    #[tokio::test]
    async fn test_run_with_retries_success() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::minimal();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("success".to_string()))
            }
        })
        .await
        .unwrap();

        assert_eq!(result, TaskResult::Single("success".to_string()));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_with_retries_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    Err(CanoError::task_execution("failure"))
                } else {
                    Ok::<TaskResult<String>, CanoError>(TaskResult::Single("success".to_string()))
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(result, TaskResult::Single("success".to_string()));
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_run_with_retries_exhausted() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution("always fails"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_run_with_retries_mode_none() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::minimal();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution("immediate fail"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::TaskExecution(_)),
            "expected original TaskExecution variant when retries disabled, got: {err}"
        );
        assert!(err.to_string().contains("immediate fail"));
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

    #[tokio::test]
    async fn test_retry_exhausted_error_type() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution(
                    "persistent failure",
                ))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::RetryExhausted(_)),
            "expected RetryExhausted after retry exhaustion, got: {err}"
        );
    }
}
