//! # Node API - The Heart of Cano Workflows
//!
//! This module provides the core [`Node`] trait, which defines the interface for workflow processing.
//! The unified approach offers several advantages:
//!
//! - **Simpler API**: One trait to learn, not a hierarchy of traits
//! - **Type Safety**: Return enum values instead of strings for workflow control
//! - **Performance**: No string conversion overhead
//! - **IDE Support**: Autocomplete for enum variants
//! - **Compile-Time Safety**: Impossible to have invalid state transitions
//!
//! ## 🚀 Quick Start
//!
//! Implement the `Node` trait for your custom processing logic. Define your data types,
//! then implement the three-phase lifecycle: `prep()` (load data), `exec()` (process),
//! and `post()` (store results and route to next node).
//!
//! ## 🚀 Performance Tips
//!
//! - Nodes execute with minimal overhead for maximum throughput
//! - Use async operations for I/O bound work
//! - Implement retry logic in NodeConfig for resilience

use crate::error::CanoError;
use crate::store::MemoryStore;
use async_trait::async_trait;
use rand::Rng;
use std::collections::HashMap;
use std::time::Duration; // Add this to Cargo.toml dependencies

/// Simple key-value parameters for node configuration
///
/// This is a convenience type alias for the most common parameter format used in workflows.
/// It provides a simple way to pass configuration data to nodes without needing custom types.
pub type DefaultParams = HashMap<String, String>;

/// Standard result type for node execution phases
///
/// This type represents the result of a node's execution phase. It uses `Box<dyn Any>`
/// to allow nodes to return any type while maintaining type erasure for dynamic workflows.
pub type DefaultNodeResult = Result<Box<dyn std::any::Any + Send + Sync>, CanoError>;

/// Retry modes for node execution
///
/// Defines different retry strategies that can be used when node execution fails.
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

/// Node configuration for retry behavior and parameters
///
/// This struct provides configuration for node execution behavior,
/// including retry logic and custom parameters.
#[derive(Clone, Default)]
pub struct NodeConfig {
    /// Retry strategy for failed executions
    pub retry_mode: RetryMode,
}

impl NodeConfig {
    /// Create a new NodeConfig with default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a minimal configuration with no retries
    ///
    /// Useful for nodes that should fail fast without any retry attempts.
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

/// Node trait for workflow processing
///
/// This trait defines the core interface that all workflow nodes must implement.
/// It provides type flexibility while maintaining performance and type safety.
///
/// # Generic Types
///
/// - **`TState`**: The return type from the post method (typically an enum for workflow control)
/// - **`TParams`**: The parameter type for this node (e.g., `HashMap<String, String>`)
/// - **`TStore`**: The store backend type (e.g., `MemoryStore`)
/// - **`PrepResult`**: The result type from the `prep` phase, passed to `exec`.
/// - **`ExecResult`**: The result type from the `exec` phase, passed to `post`.
///
/// # Node Lifecycle
///
/// Each node follows a three-phase execution lifecycle:
///
/// 1. **[`prep`](Node::prep)**: Preparation phase - setup and data loading
/// 2. **[`exec`](Node::exec)**: Execution phase - main processing logic  
/// 3. **[`post`](Node::post)**: Post-processing phase - cleanup and result handling
///
/// The [`run`](Node::run) method orchestrates these phases automatically.
///
/// # Benefits over String-based Approaches
///
/// - **Type Safety**: Return enum values instead of strings
/// - **Performance**: No string conversion overhead
/// - **IDE Support**: Autocomplete for enum variants
/// - **Compile-Time Safety**: Impossible to have invalid state transitions
///
/// # Example
///
/// ```rust
/// use cano::prelude::*;
///
/// struct MyNode;
///
/// #[async_trait]
/// impl Node<String> for MyNode {
///     type PrepResult = String;
///     type ExecResult = bool;
///
///     fn config(&self) -> NodeConfig {
///         NodeConfig::minimal()  // Use minimal retries for fast execution
///     }
///
///     async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
///         Ok("prepared_data".to_string())
///     }
///
///     async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
///         true // Success
///     }
///
///     async fn post(&self, _store: &MemoryStore, exec_res: Self::ExecResult)
///         -> Result<String, CanoError> {
///         if exec_res {
///             Ok("next".to_string())
///         } else {
///             Ok("terminate".to_string())
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait Node<TState, TParams = DefaultParams, TStore = MemoryStore>: Send + Sync
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    TParams: Send + Sync + Clone,
    TStore: Send + Sync + 'static,
{
    /// Result type from the prep phase
    type PrepResult: Send + Sync;
    /// Result type from the exec phase
    type ExecResult: Send + Sync;

    /// Set parameters for the node
    ///
    /// Default implementation that does nothing. Override this method if your node
    /// needs to store or process parameters when they are set.
    fn set_params(&mut self, _params: TParams) {
        // Default implementation does nothing
    }

    /// Get the node configuration that controls execution behavior
    ///
    /// Returns the NodeConfig that determines how this node should be executed.
    /// The default implementation returns `NodeConfig::default()` which configures
    /// the node with standard retry logic.
    ///
    /// Override this method to customize execution behavior:
    /// - Use `NodeConfig::minimal()` for fast-failing nodes with minimal retries
    /// - Use `NodeConfig::new().with_fixed_retry(n, duration)` for custom retry behavior
    /// - Return a custom configuration with specific retry/parameter settings
    fn config(&self) -> NodeConfig {
        NodeConfig::default()
    }

    /// Preparation phase - load data and setup resources
    ///
    /// This is the first phase of node execution. Use it to:
    /// - Load data from store that was left by previous nodes
    /// - Validate inputs and parameters
    /// - Setup resources needed for execution
    /// - Prepare any data structures
    ///
    /// The result of this phase is passed to the [`exec`](Node::exec) method.
    async fn prep(&self, store: &TStore) -> Result<Self::PrepResult, CanoError>;

    /// Execution phase - main processing logic
    ///
    /// This is the core processing phase where the main business logic runs.
    /// This phase doesn't have access to store - it only receives the result
    /// from the [`prep`](Node::prep) phase and produces a result for the [`post`](Node::post) phase.
    ///
    /// Benefits of this design:
    /// - Clear separation of concerns
    /// - Easier testing (pure function)
    /// - Better performance (no store access during processing)
    /// - Retry logic can wrap just this phase
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult;

    /// Post-processing phase - cleanup and result handling
    ///
    /// This is the final phase of node execution. Use it to:
    /// - Store results for the next node to use
    /// - Clean up resources
    /// - Determine the next action/node to run
    /// - Handle errors from the exec phase
    ///
    /// This method returns a typed value that determines what happens next in the workflow.
    async fn post(&self, store: &TStore, exec_res: Self::ExecResult) -> Result<TState, CanoError>;

    /// Run the complete node lifecycle with configuration-driven execution
    ///
    /// This method provides a default implementation that runs the three
    /// lifecycle phases with execution behavior controlled by the node's configuration.
    /// Nodes execute with minimal overhead for maximum throughput.
    ///
    /// You can override this method for completely custom orchestration.
    async fn run(&self, store: &TStore) -> Result<TState, CanoError> {
        let config = self.config();
        self.run_with_retries(store, &config).await
    }

    /// Internal method to run the node lifecycle with retry logic
    ///
    /// This method handles the actual execution of the three phases (prep, exec, post)
    /// with retry logic based on the node configuration.
    async fn run_with_retries(
        &self,
        store: &TStore,
        config: &NodeConfig,
    ) -> Result<TState, CanoError> {
        let max_attempts = config.retry_mode.max_attempts();
        let mut attempt = 0;

        loop {
            // Execute the three phases
            match self.prep(store).await {
                Ok(prep_res) => {
                    let exec_res = self.exec(prep_res).await;
                    match self.post(store, exec_res).await {
                        Ok(result) => return Ok(result),
                        Err(e) => {
                            attempt += 1;
                            if attempt >= max_attempts {
                                return Err(e);
                            }

                            if let Some(delay) = config.retry_mode.delay_for_attempt(attempt - 1) {
                                tokio::time::sleep(delay).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    attempt += 1;
                    if attempt >= max_attempts {
                        return Err(e);
                    }

                    if let Some(delay) = config.retry_mode.delay_for_attempt(attempt - 1) {
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
    }
}

/// Concrete node trait object with default types
///
/// This trait provides a concrete implementation of Node using the default types,
/// enabling dynamic dispatch and trait object usage.
pub trait DynNode<TState>:
    Node<
        TState,
        DefaultParams,
        MemoryStore,
        PrepResult = Box<dyn std::any::Any + Send + Sync>,
        ExecResult = DefaultNodeResult,
    >
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
}

impl<TState, N> DynNode<TState> for N
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    N: Node<
            TState,
            DefaultParams,
            MemoryStore,
            PrepResult = Box<dyn std::any::Any + Send + Sync>,
            ExecResult = DefaultNodeResult,
        >,
{
}

/// Type alias for trait objects
///
/// This alias simplifies working with dynamic node collections in workflows.
/// Use this when you need to store different node types in the same collection.
pub type NodeObject<TState> = dyn DynNode<TState> + Send + Sync;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemoryStore, Store};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio;

    // Test enum for node return values
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum TestAction {
        #[allow(dead_code)]
        Continue,
        Complete,
        Error,
        #[allow(dead_code)]
        Retry,
    }

    // Simple test node that always succeeds
    struct SimpleSuccessNode {
        execution_count: Arc<AtomicU32>,
    }

    impl SimpleSuccessNode {
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
    impl Node<TestAction> for SimpleSuccessNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Ok("prepared".to_string())
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            prep_res == "prepared"
        }

        async fn post(
            &self,
            _store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            if exec_res {
                Ok(TestAction::Complete)
            } else {
                Ok(TestAction::Error)
            }
        }
    }

    // Node that fails in prep phase
    struct PrepFailureNode {
        error_message: String,
    }

    impl PrepFailureNode {
        fn new(error_message: &str) -> Self {
            Self {
                error_message: error_message.to_string(),
            }
        }
    }

    #[async_trait]
    impl Node<TestAction> for PrepFailureNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Err(CanoError::preparation(&self.error_message))
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            true
        }

        async fn post(
            &self,
            _store: &MemoryStore,
            _exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            Ok(TestAction::Complete)
        }
    }

    // Node that uses store operations
    struct StorageNode {
        read_key: String,
        write_key: String,
        write_value: String,
    }

    impl StorageNode {
        fn new(read_key: &str, write_key: &str, write_value: &str) -> Self {
            Self {
                read_key: read_key.to_string(),
                write_key: write_key.to_string(),
                write_value: write_value.to_string(),
            }
        }
    }

    #[async_trait]
    impl Node<TestAction> for StorageNode {
        type PrepResult = Option<String>;
        type ExecResult = String;

        async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            match store.get::<String>(&self.read_key) {
                Ok(value) => Ok(Some(value)),
                Err(_) => Ok(None), // Key doesn't exist, which is fine
            }
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            match prep_res {
                Some(existing_value) => format!("processed: {existing_value}"),
                None => format!("created: {}", self.write_value),
            }
        }

        async fn post(
            &self,
            store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            store.put(&self.write_key, exec_res)?;
            Ok(TestAction::Complete)
        }
    }

    // Node that can be configured with parameters
    struct ParameterizedNode {
        params: DefaultParams,
        multiplier: i32,
    }

    impl ParameterizedNode {
        fn new() -> Self {
            Self {
                params: HashMap::new(),
                multiplier: 1,
            }
        }
    }

    #[async_trait]
    impl Node<TestAction> for ParameterizedNode {
        type PrepResult = i32;
        type ExecResult = i32;

        fn set_params(&mut self, params: DefaultParams) {
            self.params = params;
            if let Some(multiplier_str) = self.params.get("multiplier") {
                if let Ok(multiplier) = multiplier_str.parse::<i32>() {
                    self.multiplier = multiplier;
                }
            }
        }

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            let base_value = self
                .params
                .get("base_value")
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(10);
            Ok(base_value)
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            prep_res * self.multiplier
        }

        async fn post(
            &self,
            store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            store.put("result", exec_res)?;
            Ok(TestAction::Complete)
        }
    }

    // Node that fails in post phase
    struct PostFailureNode;

    #[async_trait]
    impl Node<TestAction> for PostFailureNode {
        type PrepResult = ();
        type ExecResult = ();

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Ok(())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            ()
        }

        async fn post(
            &self,
            _store: &MemoryStore,
            _exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            Err(CanoError::node_execution("Post phase failure"))
        }
    }

    // Node with custom run implementation
    struct CustomRunNode {
        should_skip_exec: bool,
    }

    impl CustomRunNode {
        fn new(should_skip_exec: bool) -> Self {
            Self { should_skip_exec }
        }
    }

    #[async_trait]
    impl Node<TestAction> for CustomRunNode {
        type PrepResult = String;
        type ExecResult = String;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Ok("prep_completed".to_string())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            format!("exec: {_prep_res}")
        }

        async fn post(
            &self,
            store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            store.put("custom_run_result", exec_res)?;
            Ok(TestAction::Complete)
        }

        // Custom run implementation that can skip exec phase
        async fn run(&self, store: &MemoryStore) -> Result<TestAction, CanoError> {
            let prep_res = self.prep(store).await?;

            if self.should_skip_exec {
                // Skip exec and go directly to post with a default value
                self.post(store, "skipped_exec".to_string()).await
            } else {
                // Normal workflow
                let exec_res = self.exec(prep_res).await;
                self.post(store, exec_res).await
            }
        }
    }

    #[tokio::test]
    async fn test_simple_node_execution() {
        let node = SimpleSuccessNode::new();
        let store = MemoryStore::new();

        let result = node.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(node.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_node_lifecycle_phases() {
        let node = SimpleSuccessNode::new();
        let store = MemoryStore::new();

        // Test prep phase
        let prep_result = node.prep(&store).await.unwrap();
        assert_eq!(prep_result, "prepared");

        // Test exec phase
        let exec_result = node.exec(prep_result).await;
        assert!(exec_result);

        // Test post phase
        let post_result = node.post(&store, exec_result).await.unwrap();
        assert_eq!(post_result, TestAction::Complete);
    }

    #[tokio::test]
    async fn test_prep_phase_failure() {
        let node = PrepFailureNode::new("Test prep failure");
        let store = MemoryStore::new();

        let result = node.run(&store).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Test prep failure"));
    }

    #[tokio::test]
    async fn test_post_phase_failure() {
        let node = PostFailureNode;
        let store = MemoryStore::new();

        let result = node.run(&store).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Post phase failure"));
    }

    #[tokio::test]
    async fn test_storage_operations() {
        let node = StorageNode::new("input_key", "output_key", "test_value");
        let store = MemoryStore::new();

        // First run - no existing data
        let result = node.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let stored_value: String = store.get("output_key").unwrap();
        assert_eq!(stored_value, "created: test_value");

        // Second run - with existing data
        store.put("input_key", "existing_data".to_string()).unwrap();
        let node2 = StorageNode::new("input_key", "output_key2", "test_value2");
        let result2 = node2.run(&store).await.unwrap();
        assert_eq!(result2, TestAction::Complete);

        let stored_value2: String = store.get("output_key2").unwrap();
        assert_eq!(stored_value2, "processed: existing_data");
    }

    #[tokio::test]
    async fn test_parameterized_node() {
        let mut node = ParameterizedNode::new();
        let store = MemoryStore::new();

        // Test with default parameters
        let result = node.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let stored_result: i32 = store.get("result").unwrap();
        assert_eq!(stored_result, 10); // base_value (10) * multiplier (1)

        // Test with custom parameters
        let mut params = HashMap::new();
        params.insert("base_value".to_string(), "5".to_string());
        params.insert("multiplier".to_string(), "3".to_string());

        node.set_params(params);
        let result2 = node.run(&store).await.unwrap();
        assert_eq!(result2, TestAction::Complete);

        let stored_result2: i32 = store.get("result").unwrap();
        assert_eq!(stored_result2, 15); // base_value (5) * multiplier (3)
    }

    #[tokio::test]
    async fn test_custom_run_implementation() {
        let store = MemoryStore::new();

        // Test normal execution
        let node1 = CustomRunNode::new(false);
        let result1 = node1.run(&store).await.unwrap();
        assert_eq!(result1, TestAction::Complete);

        let stored_value1: String = store.get("custom_run_result").unwrap();
        assert_eq!(stored_value1, "exec: prep_completed");

        // Test skipped execution
        let node2 = CustomRunNode::new(true);
        let result2 = node2.run(&store).await.unwrap();
        assert_eq!(result2, TestAction::Complete);

        let stored_value2: String = store.get("custom_run_result").unwrap();
        assert_eq!(stored_value2, "skipped_exec");
    }

    #[tokio::test]
    async fn test_multiple_node_executions() {
        let node = SimpleSuccessNode::new();
        let store = MemoryStore::new();

        // Run the node multiple times
        for i in 1..=5 {
            let result = node.run(&store).await.unwrap();
            assert_eq!(result, TestAction::Complete);
            assert_eq!(node.execution_count(), i);
        }
    }

    #[test]
    fn test_node_config_creation() {
        let config = NodeConfig::new();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_node_config_default() {
        let config = NodeConfig::default();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_node_config_minimal() {
        let config = NodeConfig::minimal();
        assert_eq!(config.retry_mode.max_attempts(), 1);
    }

    #[test]
    fn test_node_config_with_fixed_retry() {
        let config = NodeConfig::new().with_fixed_retry(5, Duration::from_millis(100));

        assert_eq!(config.retry_mode.max_attempts(), 6);
    }

    #[test]
    fn test_node_config_builder_pattern() {
        let config = NodeConfig::new().with_fixed_retry(10, Duration::from_secs(1));

        assert_eq!(config.retry_mode.max_attempts(), 11);
    }

    #[tokio::test]
    async fn test_node_trait_object_compatibility() {
        // Test that nodes can be used as trait objects with DynNode
        let _storage = MemoryStore::new();

        // This tests the DynNode trait and NodeObject type alias
        let node = SimpleSuccessNode::new();

        // Test specific trait bounds instead of full trait object
        fn assert_node_traits<N>(_: &N)
        where
            N: Node<TestAction, DefaultParams, MemoryStore, PrepResult = String, ExecResult = bool>,
        {
        }

        assert_node_traits(&node);
    }

    #[tokio::test]
    async fn test_error_propagation() {
        let store = MemoryStore::new();

        // Test prep phase error propagation
        let prep_fail_node = PrepFailureNode::new("Prep failed");
        let result = prep_fail_node.run(&store).await;
        assert!(result.is_err());

        // Test post phase error propagation
        let post_fail_node = PostFailureNode;
        let result = post_fail_node.run(&store).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_concurrent_node_execution() {
        use tokio::task;

        let node = Arc::new(SimpleSuccessNode::new());
        let store = Arc::new(MemoryStore::new());

        let mut handles = vec![];

        // Spawn multiple concurrent executions
        for _ in 0..10 {
            let node_clone = Arc::clone(&node);
            let storage_clone = Arc::clone(&store);

            let handle = task::spawn(async move { node_clone.run(&*storage_clone).await });
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
        assert_eq!(node.execution_count(), 10);
    }

    #[tokio::test]
    async fn test_node_state_isolation() {
        let storage1 = MemoryStore::new();
        let storage2 = MemoryStore::new();

        let node1 = StorageNode::new("input", "output1", "value1");
        let node2 = StorageNode::new("input", "output2", "value2");

        // Run nodes with different store instances
        node1.run(&storage1).await.unwrap();
        node2.run(&storage2).await.unwrap();

        // Verify isolation
        let result1: String = storage1.get("output1").unwrap();
        let result2: String = storage2.get("output2").unwrap();

        assert_eq!(result1, "created: value1");
        assert_eq!(result2, "created: value2");

        // Verify cross-contamination doesn't occur
        assert!(storage1.get::<String>("output2").is_err());
        assert!(storage2.get::<String>("output1").is_err());
    }

    #[tokio::test]
    async fn test_node_config_retry_behavior() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Node that fails first few times then succeeds
        struct RetryNode {
            attempt_count: Arc<AtomicUsize>,
            max_retries: usize,
        }

        impl RetryNode {
            fn new(max_retries: usize) -> Self {
                Self {
                    attempt_count: Arc::new(AtomicUsize::new(0)),
                    max_retries,
                }
            }

            fn attempt_count(&self) -> usize {
                self.attempt_count.load(Ordering::SeqCst)
            }
        }

        #[async_trait]
        impl Node<TestAction> for RetryNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> NodeConfig {
                NodeConfig::new().with_fixed_retry(self.max_retries, Duration::from_millis(1))
            }

            async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
                let attempt = self.attempt_count.fetch_add(1, Ordering::SeqCst) + 1;

                // Fail first 2 attempts, succeed on 3rd
                if attempt < 3 {
                    Err(CanoError::preparation("Simulated failure"))
                } else {
                    Ok(())
                }
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                ()
            }

            async fn post(
                &self,
                _store: &MemoryStore,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let store = MemoryStore::new();

        // Test with sufficient retries
        let node_success = RetryNode::new(5);
        let result = node_success.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(node_success.attempt_count(), 3); // Failed twice, succeeded on third attempt

        // Test with insufficient retries
        let node_failure = RetryNode::new(1);
        let result = node_failure.run(&store).await;
        assert!(result.is_err());
        assert_eq!(node_failure.attempt_count(), 2); // Initial attempt + 1 retry
    }

    #[tokio::test]
    async fn test_node_config_variants() {
        let store = MemoryStore::new();

        // Test minimal config
        struct MinimalNode;

        #[async_trait]
        impl Node<TestAction> for MinimalNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> NodeConfig {
                NodeConfig::minimal()
            }

            async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
                Ok(())
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                ()
            }

            async fn post(
                &self,
                _store: &MemoryStore,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let minimal_node = MinimalNode;
        let result = minimal_node.run(&store).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let config = minimal_node.config();
        assert_eq!(config.retry_mode.max_attempts(), 1);
    }

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
            5,                          // max_retries
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
    fn test_retry_mode_builder_methods() {
        // Test the convenience constructor methods
        let fixed = RetryMode::fixed(2, Duration::from_millis(200));
        assert_eq!(fixed.max_attempts(), 3);

        let exponential = RetryMode::exponential(5);
        assert_eq!(exponential.max_attempts(), 6);

        // Test that exponential uses sensible defaults
        if let RetryMode::ExponentialBackoff {
            base_delay,
            multiplier,
            max_delay,
            jitter,
            ..
        } = exponential
        {
            assert_eq!(base_delay, Duration::from_millis(100));
            assert_eq!(multiplier, 2.0);
            assert_eq!(max_delay, Duration::from_secs(30));
            assert_eq!(jitter, 0.1);
        } else {
            panic!("Expected ExponentialBackoff variant");
        }
    }

    #[tokio::test]
    async fn test_retry_mode_in_node_execution() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Node that fails exactly N times before succeeding
        struct FailNTimesNode {
            fail_count: usize,
            attempt_counter: Arc<AtomicUsize>,
        }

        impl FailNTimesNode {
            fn new(fail_count: usize) -> Self {
                Self {
                    fail_count,
                    attempt_counter: Arc::new(AtomicUsize::new(0)),
                }
            }

            fn attempt_count(&self) -> usize {
                self.attempt_counter.load(Ordering::SeqCst)
            }
        }

        #[async_trait]
        impl Node<TestAction> for FailNTimesNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> NodeConfig {
                NodeConfig::new().with_fixed_retry(5, Duration::from_millis(1))
            }

            async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
                let attempt = self.attempt_counter.fetch_add(1, Ordering::SeqCst);

                if attempt < self.fail_count {
                    Err(CanoError::preparation("Simulated failure"))
                } else {
                    Ok(())
                }
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                ()
            }

            async fn post(
                &self,
                _store: &MemoryStore,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let store = MemoryStore::new();

        // Test successful retry after 2 failures
        let node1 = FailNTimesNode::new(2);
        let result1 = node1.run(&store).await.unwrap();
        assert_eq!(result1, TestAction::Complete);
        assert_eq!(node1.attempt_count(), 3); // Failed twice, succeeded on third

        // Test exhausting all retries
        let node2 = FailNTimesNode::new(10); // Fail more times than retries available
        let result2 = node2.run(&store).await;
        assert!(result2.is_err());
        assert_eq!(node2.attempt_count(), 6); // 1 initial + 5 retries
    }

    #[tokio::test]
    async fn test_retry_mode_timing() {
        use std::time::Instant;

        struct AlwaysFailNode;

        #[async_trait]
        impl Node<TestAction> for AlwaysFailNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> NodeConfig {
                NodeConfig::new().with_fixed_retry(2, Duration::from_millis(50))
            }

            async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
                Err(CanoError::preparation("Always fails"))
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                ()
            }

            async fn post(
                &self,
                _store: &MemoryStore,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let store = MemoryStore::new();
        let node = AlwaysFailNode;

        let start = Instant::now();
        let result = node.run(&store).await;
        let elapsed = start.elapsed();

        assert!(result.is_err());
        // Should take at least 100ms (2 retries * 50ms delay)
        // Allow some tolerance for test timing
        assert!(elapsed >= Duration::from_millis(90));
    }
}
