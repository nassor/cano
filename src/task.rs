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
//! ```rust
//! use cano::prelude::*;
//!
//! // Simple Task implementation
//! struct SimpleTask;
//!
//! #[async_trait]
//! impl Task<String> for SimpleTask {
//!     async fn run(&self, store: &MemoryStore) -> Result<String, CanoError> {
//!         // Do all your work here - load, process, store
//!         let input: String = store.get("input")?;
//!         let result = format!("processed: {input}");
//!         store.put("result", result)?;
//!         Ok("next_state".to_string())
//!     }
//! }
//! ```
//!
//! ## Interoperability
//!
//! Every [`crate::node::Node`] automatically implements [`Task`], so you can use existing nodes
//! wherever tasks are expected. This provides a smooth upgrade path and backward compatibility.

use crate::error::CanoError;
use crate::store::MemoryStore;
use async_trait::async_trait;
use rand::Rng;
use std::collections::HashMap;
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

                    let final_delay = (capped_delay * jitter_factor).max(0.0) as u64;
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
pub async fn run_with_retries<TState, TStore, F, Fut>(
    config: &TaskConfig,
    run_fn: F,
) -> Result<TState, CanoError>
where
    TState: Send + Sync,
    TStore: Send + Sync,
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
                    return Err(e);
                }

                if let Some(delay) = config.retry_mode.delay_for_attempt(attempt - 1) {
                    #[cfg(feature = "tracing")]
                    debug!(delay_ms = delay.as_millis(), "Waiting before retry");

                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

/// Simple key-value parameters for task configuration
///
/// This is a convenience type alias for the most common parameter format used in workflows.
pub type DefaultTaskParams = HashMap<String, String>;

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
/// - **`TStore`**: The store backend type (e.g., `MemoryStore`)
/// - **`TParams`**: The parameter type for this task (e.g., `HashMap<String, String>`)
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
/// ```rust
/// use cano::prelude::*;
///
/// struct DataProcessor {
///     multiplier: i32,
/// }
///
/// #[async_trait]
/// impl Task<String> for DataProcessor {
///     async fn run(&self, store: &MemoryStore) -> Result<String, CanoError> {
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
///             Ok("large_result".to_string())
///         } else {
///             Ok("small_result".to_string())
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait Task<TState, TStore = MemoryStore, TParams = DefaultTaskParams>: Send + Sync
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    TParams: Send + Sync + Clone,
    TStore: Send + Sync + 'static,
{
    /// Set parameters for the task
    ///
    /// Default implementation that does nothing. Override this method if your task
    /// needs to store or process parameters when they are set.
    fn set_params(&mut self, _params: TParams) {
        // Default implementation does nothing
    }

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

    /// Execute the task with the given store
    ///
    /// This method contains all the task logic in a single place. Unlike [`crate::node::Node`],
    /// there's no separation into prep/exec/post phases - you have full control
    /// over the execution flow.
    ///
    /// # Parameters
    ///
    /// - `store`: The store for reading and writing data shared between tasks
    ///
    /// # Returns
    ///
    /// A result containing the next state to transition to, or an error if the task failed.
    async fn run(&self, store: &TStore) -> Result<TState, CanoError>;
}

/// Blanket implementation: Every Node is automatically a Task
///
/// This implementation makes all existing [`crate::node::Node`] implementations automatically
/// work as [`Task`] implementations. This means [`crate::node::Node`] is a superset of [`Task`]:
///
/// - **[`Task`]**: Simple `run()` method
/// - **[`crate::node::Node`]**: Three-phase lifecycle (`prep`, `exec`, `post`) + retry strategies
///
/// The [`crate::node::Node::run`] method (which orchestrates the three phases) is used directly
/// as the [`Task::run`] implementation, providing seamless interoperability.
///
/// This enables:
/// - Using any Node wherever Tasks are expected
/// - Mixing Tasks and Nodes in the same workflow
/// - Gradual migration from simple Tasks to full-featured Nodes
#[async_trait]
impl<TState, TStore, TParams, N> Task<TState, TStore, TParams> for N
where
    N: crate::node::Node<TState, TStore, TParams>,
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    TParams: Send + Sync + Clone,
    TStore: Send + Sync + 'static,
{
    fn set_params(&mut self, params: TParams) {
        crate::node::Node::set_params(self, params);
    }

    fn config(&self) -> TaskConfig {
        let node_config = crate::node::Node::config(self);
        TaskConfig {
            retry_mode: node_config.retry_mode,
        }
    }

    #[cfg_attr(
        feature = "tracing",
        instrument(skip(self, store), fields(task_type = "node_adapter"))
    )]
    async fn run(&self, store: &TStore) -> Result<TState, CanoError> {
        #[cfg(feature = "tracing")]
        debug!("Executing task through Node adapter");

        let result = crate::node::Node::run(self, store).await;

        #[cfg(feature = "tracing")]
        match &result {
            Ok(state) => info!(next_state = ?state, "Task execution completed successfully"),
            Err(error) => error!(error = %error, "Task execution failed"),
        }

        result
    }
}

/// Concrete task trait object with default types
///
/// This trait provides a concrete implementation of Task using the default types,
/// enabling dynamic dispatch and trait object usage.
pub trait DynTask<TState>: Task<TState, MemoryStore, DefaultTaskParams>
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
}

impl<TState, T> DynTask<TState> for T
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    T: Task<TState, MemoryStore, DefaultTaskParams>,
{
}

/// Type alias for trait objects
///
/// This alias simplifies working with dynamic task collections in workflows.
/// Use this when you need to store different task types in the same collection.
pub type TaskObject<TState> = dyn DynTask<TState> + Send + Sync;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{KeyValueStore, MemoryStore};
    use async_trait::async_trait;
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

    #[async_trait]
    impl Task<TestAction> for SimpleTask {
        async fn run(&self, store: &MemoryStore) -> Result<TestAction, CanoError> {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            store.put("simple_task_executed", true)?;
            Ok(TestAction::Complete)
        }
    }

    // Task that uses parameters
    struct ParameterizedTask {
        params: DefaultTaskParams,
        multiplier: i32,
    }

    impl ParameterizedTask {
        fn new() -> Self {
            Self {
                params: HashMap::new(),
                multiplier: 1,
            }
        }
    }

    #[async_trait]
    impl Task<TestAction> for ParameterizedTask {
        fn set_params(&mut self, params: DefaultTaskParams) {
            self.params = params;
            if let Some(multiplier_str) = self.params.get("multiplier") {
                if let Ok(multiplier) = multiplier_str.parse::<i32>() {
                    self.multiplier = multiplier;
                }
            }
        }

        async fn run(&self, store: &MemoryStore) -> Result<TestAction, CanoError> {
            let base_value = self
                .params
                .get("base_value")
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(10);

            let result = base_value * self.multiplier;
            store.put("result", result)?;
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

    #[async_trait]
    impl Task<TestAction> for FailingTask {
        async fn run(&self, store: &MemoryStore) -> Result<TestAction, CanoError> {
            if self.should_fail {
                Err(CanoError::task_execution("Task intentionally failed"))
            } else {
                store.put("failing_task_executed", true)?;
                Ok(TestAction::Complete)
            }
        }
    }

    // Data processing task that reads, processes, and writes
    struct DataProcessingTask {
        input_key: String,
        output_key: String,
    }

    impl DataProcessingTask {
        fn new(input_key: &str, output_key: &str) -> Self {
            Self {
                input_key: input_key.to_string(),
                output_key: output_key.to_string(),
            }
        }
    }

    #[async_trait]
    impl Task<TestAction> for DataProcessingTask {
        async fn run(&self, store: &MemoryStore) -> Result<TestAction, CanoError> {
            // Read input data
            let input_data: String = store
                .get(&self.input_key)
                .map_err(|e| CanoError::task_execution(format!("Failed to read input: {e}")))?;

            // Process data
            let processed_data = format!("processed: {input_data}");

            // Write output data
            store.put(&self.output_key, processed_data)?;

            Ok(TestAction::Complete)
        }
    }

    #[tokio::test]
    async fn test_simple_task_execution() {
        let task = SimpleTask::new();
        let store = MemoryStore::new();

        let result = task.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(task.execution_count(), 1);

        let executed: bool = store.get("simple_task_executed").unwrap();
        assert!(executed);
    }

    #[tokio::test]
    async fn test_parameterized_task() {
        let mut task = ParameterizedTask::new();
        let store = MemoryStore::new();

        // Test with default parameters
        let result = task.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let stored_result: i32 = store.get("result").unwrap();
        assert_eq!(stored_result, 10); // base_value (10) * multiplier (1)

        // Test with custom parameters
        let mut params = HashMap::new();
        params.insert("base_value".to_string(), "5".to_string());
        params.insert("multiplier".to_string(), "3".to_string());

        task.set_params(params);
        let result2 = task.run(&store).await.unwrap();
        assert_eq!(result2, TestAction::Complete);

        let stored_result2: i32 = store.get("result").unwrap();
        assert_eq!(stored_result2, 15); // base_value (5) * multiplier (3)
    }

    #[tokio::test]
    async fn test_failing_task() {
        let store = MemoryStore::new();

        // Test successful task
        let success_task = FailingTask::new(false);
        let result = success_task.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let executed: bool = store.get("failing_task_executed").unwrap();
        assert!(executed);

        // Test failing task
        let fail_task = FailingTask::new(true);
        let result = fail_task.run(&store).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Task intentionally failed"));
    }

    #[tokio::test]
    async fn test_data_processing_task() {
        let store = MemoryStore::new();
        let task = DataProcessingTask::new("input_data", "output_data");

        // Setup input data
        store.put("input_data", "test_value".to_string()).unwrap();

        // Run task
        let result = task.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        // Verify output
        let output: String = store.get("output_data").unwrap();
        assert_eq!(output, "processed: test_value");
    }

    #[tokio::test]
    async fn test_data_processing_task_missing_input() {
        let store = MemoryStore::new();
        let task = DataProcessingTask::new("missing_input", "output_data");

        // Run task without setting input
        let result = task.run(&store).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Failed to read input"));
    }

    #[tokio::test]
    async fn test_concurrent_task_execution() {
        use tokio::task;

        let task = Arc::new(SimpleTask::new());
        let store = Arc::new(MemoryStore::new());

        let mut handles = vec![];

        // Spawn multiple concurrent executions
        for _ in 0..10 {
            let task_clone = Arc::clone(&task);
            let store_clone = Arc::clone(&store);

            let handle = task::spawn(async move { task_clone.run(&*store_clone).await });
            handles.push(handle);
        }

        // Wait for all executions to complete
        let mut success_count = 0;
        for handle in handles {
            let result = handle.await.unwrap();
            if result.is_ok() && result.unwrap() == TestAction::Complete {
                success_count += 1;
            }
        }

        assert_eq!(success_count, 10);
        assert_eq!(task.execution_count(), 10);
    }

    #[tokio::test]
    async fn test_task_trait_object_compatibility() {
        // Test that tasks can be used as trait objects with DynTask
        let _store = MemoryStore::new();

        // This tests the DynTask trait and TaskObject type alias
        let task = SimpleTask::new();

        // Test specific trait bounds
        fn assert_task_traits<T>(_: &T)
        where
            T: Task<TestAction, MemoryStore, DefaultTaskParams>,
        {
        }

        assert_task_traits(&task);
    }

    #[tokio::test]
    async fn test_multiple_task_executions() {
        let task = SimpleTask::new();
        let store = MemoryStore::new();

        // Run the task multiple times
        for i in 1..=5 {
            let result = task.run(&store).await.unwrap();
            assert_eq!(result, TestAction::Complete);
            assert_eq!(task.execution_count(), i);
        }
    }

    #[tokio::test]
    async fn test_task_state_isolation() {
        let store1 = MemoryStore::new();
        let store2 = MemoryStore::new();

        let task1 = DataProcessingTask::new("input", "output1");
        let task2 = DataProcessingTask::new("input", "output2");

        // Setup different input data for each store
        store1.put("input", "data1".to_string()).unwrap();
        store2.put("input", "data2".to_string()).unwrap();

        // Run tasks with different store instances
        task1.run(&store1).await.unwrap();
        task2.run(&store2).await.unwrap();

        // Verify isolation
        let result1: String = store1.get("output1").unwrap();
        let result2: String = store2.get("output2").unwrap();

        assert_eq!(result1, "processed: data1");
        assert_eq!(result2, "processed: data2");

        // Verify cross-contamination doesn't occur
        assert!(store1.get::<String>("output2").is_err());
        assert!(store2.get::<String>("output1").is_err());
    }

    // Test that demonstrates Node -> Task compatibility
    // This uses the existing Node from the node module
    use crate::node::Node;

    struct TestNode;

    #[async_trait]
    impl Node<TestAction> for TestNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Ok("node_prepared".to_string())
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            prep_res == "node_prepared"
        }

        async fn post(
            &self,
            store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            store.put("node_executed", exec_res)?;
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
        let store = MemoryStore::new();

        // Use the node as a task - this should work due to the blanket implementation
        let result: Result<TestAction, CanoError> = Task::run(&node, &store).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TestAction::Complete);

        let executed: bool = store.get("node_executed").unwrap();
        assert!(executed);
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::minimal();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, (), _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<String, CanoError>("success".to_string())
            }
        })
        .await
        .unwrap();

        assert_eq!(result, "success");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_with_retries_failure() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, (), _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    Err(CanoError::task_execution("failure"))
                } else {
                    Ok::<String, CanoError>("success".to_string())
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(result, "success");
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_run_with_retries_exhausted() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, (), _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<String, CanoError>(CanoError::task_execution("always fails"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }
}
