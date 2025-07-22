//! # Node API - The Heart of Cano Workflows
//!
//! This module provides the core [`Node`] trait, which defines the interface for workflow processing.
//! The unified approach offers several advantages:
//!
//! - **Simpler API**: One trait to learn, not a hierarchy of traits
//! - **Type Safety**: Return enum values instead of strings for flow control
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
use crate::store::{MemoryStore, StoreTrait};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

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

/// Backward compatibility type aliases
pub type Params = DefaultParams;
pub type NodeResult = DefaultNodeResult;

/// Node trait for workflow processing
///
/// This trait defines the core interface that all workflow nodes must implement.
/// It provides type flexibility while maintaining performance and type safety.
///
/// # Generic Types
///
/// - **`T`**: The return type from the post method (typically an enum for flow control)
/// - **`Params`**: The parameter type for this node (e.g., `HashMap<String, String>`)
/// - **`Storage`**: The storage backend type (e.g., `MemoryStore`)
/// - **`PrepResult`**: The result type from the `prep` phase, passed to `exec`.
/// - **`ExecResult`**: The result type from the `exec` phase, passed to `post`.
///
/// # Node Lifecycle
///
/// Each node follows a three-phase execution lifecycle:
///
/// 1. **[`prep`]**: Preparation phase - setup and data loading
/// 2. **[`exec`]**: Execution phase - main processing logic  
/// 3. **[`post`]**: Post-processing phase - cleanup and result handling
///
/// The [`run`] method orchestrates these phases automatically.
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
///     type Params = DefaultParams;
///     type Storage = MemoryStore;
///     type PrepResult = String;
///     type ExecResult = bool;
///
///     fn set_params(&mut self, _params: Self::Params) {}
///
///     fn config(&self) -> NodeConfig {
///         NodeConfig::minimal()  // Use minimal retries for fast execution
///     }
///
///     async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
///         Ok("prepared_data".to_string())
///     }
///
///     async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
///         true // Success
///     }
///
///     async fn post(&self, _store: &Self::Storage, exec_res: Self::ExecResult)
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
pub trait Node<T>: Send + Sync
where
    T: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Parameter type for this node
    type Params: Send + Sync + Clone;
    /// Storage type for this node
    type Storage: StoreTrait;
    /// Result type from the prep phase
    type PrepResult: Send + Sync;
    /// Result type from the exec phase
    type ExecResult: Send + Sync;

    /// Set parameters for the node
    fn set_params(&mut self, params: Self::Params);

    /// Get the node configuration that controls execution behavior
    ///
    /// Returns the NodeConfig that determines how this node should be executed.
    /// The default implementation returns `NodeConfig::default()` which configures
    /// the node with standard retry logic.
    ///
    /// Override this method to customize execution behavior:
    /// - Use `NodeConfig::minimal()` for fast-failing nodes with minimal retries
    /// - Use `NodeConfig::new().with_retries(n, duration)` for custom retry behavior
    /// - Return a custom configuration with specific retry/parameter settings
    fn config(&self) -> NodeConfig {
        NodeConfig::default()
    }

    /// Preparation phase - load data and setup resources
    ///
    /// This is the first phase of node execution. Use it to:
    /// - Load data from storage that was left by previous nodes
    /// - Validate inputs and parameters
    /// - Setup resources needed for execution
    /// - Prepare any data structures
    ///
    /// The result of this phase is passed to the [`exec`] method.
    async fn prep(&self, store: &Self::Storage) -> Result<Self::PrepResult, CanoError>;

    /// Execution phase - main processing logic
    ///
    /// This is the core processing phase where the main business logic runs.
    /// This phase doesn't have access to storage - it only receives the result
    /// from the [`prep`] phase and produces a result for the [`post`] phase.
    ///
    /// Benefits of this design:
    /// - Clear separation of concerns
    /// - Easier testing (pure function)
    /// - Better performance (no storage access during processing)
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
    async fn post(&self, store: &Self::Storage, exec_res: Self::ExecResult)
    -> Result<T, CanoError>;

    /// Run the complete node lifecycle with configuration-driven execution
    ///
    /// This method provides a default implementation that runs the three
    /// lifecycle phases with execution behavior controlled by the node's configuration.
    /// Nodes execute with minimal overhead for maximum throughput.
    ///
    /// You can override this method for completely custom orchestration.
    async fn run(&self, store: &Self::Storage) -> Result<T, CanoError> {
        let config = self.config();
        self.run_with_retries(store, &config).await
    }

    /// Internal method to run the node lifecycle with retry logic
    ///
    /// This method handles the actual execution of the three phases (prep, exec, post)
    /// with retry logic based on the node configuration.
    async fn run_with_retries(
        &self,
        store: &Self::Storage,
        config: &NodeConfig,
    ) -> Result<T, CanoError> {
        let mut attempts = 0;

        loop {
            attempts += 1;

            // Execute the three phases
            match self.prep(store).await {
                Ok(prep_res) => {
                    let exec_res = self.exec(prep_res).await;
                    match self.post(store, exec_res).await {
                        Ok(result) => return Ok(result),
                        Err(_) if attempts <= config.max_retries => {
                            tokio::time::sleep(config.wait).await;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(_) if attempts <= config.max_retries => {
                    tokio::time::sleep(config.wait).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Concrete node trait object with default types
///
/// This trait provides a concrete implementation of Node using the default types,
/// enabling dynamic dispatch and trait object usage.
pub trait DynNode<T>:
    Node<
        T,
        Params = DefaultParams,
        Storage = MemoryStore,
        PrepResult = Box<dyn std::any::Any + Send + Sync>,
        ExecResult = DefaultNodeResult,
    >
where
    T: Clone + std::fmt::Debug + Send + Sync + 'static,
{
}

impl<T, N> DynNode<T> for N
where
    T: Clone + std::fmt::Debug + Send + Sync + 'static,
    N: Node<
            T,
            Params = DefaultParams,
            Storage = MemoryStore,
            PrepResult = Box<dyn std::any::Any + Send + Sync>,
            ExecResult = DefaultNodeResult,
        >,
{
}

/// Type alias for trait objects
///
/// This alias simplifies working with dynamic node collections in workflows.
/// Use this when you need to store different node types in the same collection.
pub type NodeObject<T> = dyn DynNode<T> + Send + Sync;

/// Node configuration for retry behavior and parameters
///
/// This struct provides configuration for node execution behavior,
/// including retry logic and custom parameters.
///
/// ## Configuration Fields
/// - **`max_retries`**: Maximum number of retry attempts (default: 3)
/// - **`wait`**: Duration to wait between retry attempts (default: 50μs)
/// - **`params`**: Key-value parameters for node configuration
#[derive(Clone)]
pub struct NodeConfig {
    /// Maximum number of retry attempts before failing
    pub max_retries: usize,
    /// Duration to wait between retry attempts  
    pub wait: Duration,
    /// Key-value parameters for node configuration
    pub params: Params,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            wait: Duration::from_micros(50),
            params: HashMap::new(),
        }
    }
}

impl NodeConfig {
    /// Create a new NodeConfig with default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a minimal configuration with reduced retries
    ///
    /// Useful for nodes that should fail fast with minimal retry attempts.
    pub fn minimal() -> Self {
        Self {
            max_retries: 1,
            wait: Duration::from_millis(0),
            params: HashMap::new(),
        }
    }

    /// Set the maximum number of retry attempts
    pub fn with_retries(mut self, retries: usize, wait: Duration) -> Self {
        self.max_retries = retries;
        self.wait = wait;
        self
    }

    /// Set parameters for the node
    pub fn with_params(mut self, params: Params) -> Self {
        self.params = params;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemoryStore, StoreTrait};
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
        type Params = DefaultParams;
        type Storage = MemoryStore;
        type PrepResult = String;
        type ExecResult = bool;

        fn set_params(&mut self, _params: Self::Params) {}

        async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
            Ok("prepared".to_string())
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            prep_res == "prepared"
        }

        async fn post(
            &self,
            _store: &Self::Storage,
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
        type Params = DefaultParams;
        type Storage = MemoryStore;
        type PrepResult = String;
        type ExecResult = bool;

        fn set_params(&mut self, _params: Self::Params) {}

        async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
            Err(CanoError::preparation(&self.error_message))
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            true
        }

        async fn post(
            &self,
            _store: &Self::Storage,
            _exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            Ok(TestAction::Complete)
        }
    }

    // Node that uses storage operations
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
        type Params = DefaultParams;
        type Storage = MemoryStore;
        type PrepResult = Option<String>;
        type ExecResult = String;

        fn set_params(&mut self, _params: Self::Params) {}

        async fn prep(&self, store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
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
            store: &Self::Storage,
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
        type Params = DefaultParams;
        type Storage = MemoryStore;
        type PrepResult = i32;
        type ExecResult = i32;

        fn set_params(&mut self, params: Self::Params) {
            self.params = params;
            if let Some(multiplier_str) = self.params.get("multiplier") {
                if let Ok(multiplier) = multiplier_str.parse::<i32>() {
                    self.multiplier = multiplier;
                }
            }
        }

        async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
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
            store: &Self::Storage,
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
        type Params = DefaultParams;
        type Storage = MemoryStore;
        type PrepResult = ();
        type ExecResult = ();

        fn set_params(&mut self, _params: Self::Params) {}

        async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
            Ok(())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            ()
        }

        async fn post(
            &self,
            _store: &Self::Storage,
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
        type Params = DefaultParams;
        type Storage = MemoryStore;
        type PrepResult = String;
        type ExecResult = String;

        fn set_params(&mut self, _params: Self::Params) {}

        async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
            Ok("prep_completed".to_string())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            format!("exec: {_prep_res}")
        }

        async fn post(
            &self,
            store: &Self::Storage,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            store.put("custom_run_result", exec_res)?;
            Ok(TestAction::Complete)
        }

        // Custom run implementation that can skip exec phase
        async fn run(&self, store: &Self::Storage) -> Result<TestAction, CanoError> {
            let prep_res = self.prep(store).await?;

            if self.should_skip_exec {
                // Skip exec and go directly to post with a default value
                self.post(store, "skipped_exec".to_string()).await
            } else {
                // Normal flow
                let exec_res = self.exec(prep_res).await;
                self.post(store, exec_res).await
            }
        }
    }

    #[tokio::test]
    async fn test_simple_node_execution() {
        let node = SimpleSuccessNode::new();
        let storage = MemoryStore::new();

        let result = node.run(&storage).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(node.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_node_lifecycle_phases() {
        let node = SimpleSuccessNode::new();
        let storage = MemoryStore::new();

        // Test prep phase
        let prep_result = node.prep(&storage).await.unwrap();
        assert_eq!(prep_result, "prepared");

        // Test exec phase
        let exec_result = node.exec(prep_result).await;
        assert!(exec_result);

        // Test post phase
        let post_result = node.post(&storage, exec_result).await.unwrap();
        assert_eq!(post_result, TestAction::Complete);
    }

    #[tokio::test]
    async fn test_prep_phase_failure() {
        let node = PrepFailureNode::new("Test prep failure");
        let storage = MemoryStore::new();

        let result = node.run(&storage).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Test prep failure"));
    }

    #[tokio::test]
    async fn test_post_phase_failure() {
        let node = PostFailureNode;
        let storage = MemoryStore::new();

        let result = node.run(&storage).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Post phase failure"));
    }

    #[tokio::test]
    async fn test_storage_operations() {
        let node = StorageNode::new("input_key", "output_key", "test_value");
        let storage = MemoryStore::new();

        // First run - no existing data
        let result = node.run(&storage).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let stored_value: String = storage.get("output_key").unwrap();
        assert_eq!(stored_value, "created: test_value");

        // Second run - with existing data
        storage
            .put("input_key", "existing_data".to_string())
            .unwrap();
        let node2 = StorageNode::new("input_key", "output_key2", "test_value2");
        let result2 = node2.run(&storage).await.unwrap();
        assert_eq!(result2, TestAction::Complete);

        let stored_value2: String = storage.get("output_key2").unwrap();
        assert_eq!(stored_value2, "processed: existing_data");
    }

    #[tokio::test]
    async fn test_parameterized_node() {
        let mut node = ParameterizedNode::new();
        let storage = MemoryStore::new();

        // Test with default parameters
        let result = node.run(&storage).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let stored_result: i32 = storage.get("result").unwrap();
        assert_eq!(stored_result, 10); // base_value (10) * multiplier (1)

        // Test with custom parameters
        let mut params = HashMap::new();
        params.insert("base_value".to_string(), "5".to_string());
        params.insert("multiplier".to_string(), "3".to_string());

        node.set_params(params);
        let result2 = node.run(&storage).await.unwrap();
        assert_eq!(result2, TestAction::Complete);

        let stored_result2: i32 = storage.get("result").unwrap();
        assert_eq!(stored_result2, 15); // base_value (5) * multiplier (3)
    }

    #[tokio::test]
    async fn test_custom_run_implementation() {
        let storage = MemoryStore::new();

        // Test normal execution
        let node1 = CustomRunNode::new(false);
        let result1 = node1.run(&storage).await.unwrap();
        assert_eq!(result1, TestAction::Complete);

        let stored_value1: String = storage.get("custom_run_result").unwrap();
        assert_eq!(stored_value1, "exec: prep_completed");

        // Test skipped execution
        let node2 = CustomRunNode::new(true);
        let result2 = node2.run(&storage).await.unwrap();
        assert_eq!(result2, TestAction::Complete);

        let stored_value2: String = storage.get("custom_run_result").unwrap();
        assert_eq!(stored_value2, "skipped_exec");
    }

    #[tokio::test]
    async fn test_multiple_node_executions() {
        let node = SimpleSuccessNode::new();
        let storage = MemoryStore::new();

        // Run the node multiple times
        for i in 1..=5 {
            let result = node.run(&storage).await.unwrap();
            assert_eq!(result, TestAction::Complete);
            assert_eq!(node.execution_count(), i);
        }
    }

    #[test]
    fn test_node_config_creation() {
        let config = NodeConfig::new();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.wait, Duration::from_micros(50));
        assert!(config.params.is_empty());
    }

    #[test]
    fn test_node_config_default() {
        let config = NodeConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.wait, Duration::from_micros(50));
        assert!(config.params.is_empty());
    }

    #[test]
    fn test_node_config_minimal() {
        let config = NodeConfig::minimal();
        assert_eq!(config.max_retries, 1);
        assert_eq!(config.wait, Duration::from_millis(0));
        assert!(config.params.is_empty());
    }

    #[test]
    fn test_node_config_with_retries() {
        let config = NodeConfig::new().with_retries(5, Duration::from_millis(100));

        assert_eq!(config.max_retries, 5);
        assert_eq!(config.wait, Duration::from_millis(100));
    }

    #[test]
    fn test_node_config_with_params() {
        let mut params = HashMap::new();
        params.insert("key1".to_string(), "value1".to_string());
        params.insert("key2".to_string(), "value2".to_string());

        let config = NodeConfig::new().with_params(params.clone());
        assert_eq!(config.params, params);
    }

    #[test]
    fn test_node_config_builder_pattern() {
        let mut params = HashMap::new();
        params.insert("timeout".to_string(), "30".to_string());
        params.insert("retries".to_string(), "5".to_string());

        let config = NodeConfig::new()
            .with_retries(10, Duration::from_secs(1))
            .with_params(params.clone());

        assert_eq!(config.max_retries, 10);
        assert_eq!(config.wait, Duration::from_secs(1));
        assert_eq!(config.params, params);
    }

    #[test]
    fn test_type_aliases() {
        // Test that type aliases compile and work correctly
        let _params: DefaultParams = HashMap::new();
        let _legacy_params: Params = HashMap::new();

        // These should be the same type
        let default_params: DefaultParams = HashMap::new();
        let legacy_params: Params = default_params;
        assert!(legacy_params.is_empty());
    }

    #[tokio::test]
    async fn test_node_trait_object_compatibility() {
        // Test that nodes can be used as trait objects with DynNode
        let _storage = MemoryStore::new();

        // This tests the DynNode trait and NodeObject type alias
        let node = SimpleSuccessNode::new();

        // Test that the node implements the required traits
        let _: &dyn Node<
            TestAction,
            Params = DefaultParams,
            Storage = MemoryStore,
            PrepResult = String,
            ExecResult = bool,
        > = &node;
    }

    #[tokio::test]
    async fn test_error_propagation() {
        let storage = MemoryStore::new();

        // Test prep phase error propagation
        let prep_fail_node = PrepFailureNode::new("Prep failed");
        let result = prep_fail_node.run(&storage).await;
        assert!(result.is_err());

        // Test post phase error propagation
        let post_fail_node = PostFailureNode;
        let result = post_fail_node.run(&storage).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_concurrent_node_execution() {
        use tokio::task;

        let node = Arc::new(SimpleSuccessNode::new());
        let storage = Arc::new(MemoryStore::new());

        let mut handles = vec![];

        // Spawn multiple concurrent executions
        for _ in 0..10 {
            let node_clone = Arc::clone(&node);
            let storage_clone = Arc::clone(&storage);

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

        // Run nodes with different storage instances
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
            type Params = DefaultParams;
            type Storage = MemoryStore;
            type PrepResult = ();
            type ExecResult = ();

            fn set_params(&mut self, _params: Self::Params) {}

            fn config(&self) -> NodeConfig {
                NodeConfig::new().with_retries(self.max_retries, Duration::from_millis(1))
            }

            async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
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
                _store: &Self::Storage,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let storage = MemoryStore::new();

        // Test with sufficient retries
        let node_success = RetryNode::new(5);
        let result = node_success.run(&storage).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(node_success.attempt_count(), 3); // Failed twice, succeeded on third attempt

        // Test with insufficient retries
        let node_failure = RetryNode::new(1);
        let result = node_failure.run(&storage).await;
        assert!(result.is_err());
        assert_eq!(node_failure.attempt_count(), 2); // Initial attempt + 1 retry
    }

    #[tokio::test]
    async fn test_node_config_variants() {
        let storage = MemoryStore::new();

        // Test minimal config
        struct MinimalNode;

        #[async_trait]
        impl Node<TestAction> for MinimalNode {
            type Params = DefaultParams;
            type Storage = MemoryStore;
            type PrepResult = ();
            type ExecResult = ();

            fn set_params(&mut self, _params: Self::Params) {}

            fn config(&self) -> NodeConfig {
                NodeConfig::minimal()
            }

            async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
                Ok(())
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                ()
            }

            async fn post(
                &self,
                _store: &Self::Storage,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let minimal_node = MinimalNode;
        let result = minimal_node.run(&storage).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let config = minimal_node.config();
        assert_eq!(config.max_retries, 1);
        assert_eq!(config.wait, Duration::from_millis(0));
    }
}
