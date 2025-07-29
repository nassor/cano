//! # Task API - Simplified Workflow Interface
//!
//! This module provides the [`Task`] trait, which offers a simplified interface for workflow processing.
//! While [`Node`] requires implementing three phases (`prep`, `exec`, `post`), [`Task`] only requires
//! implementing a single `run` method.
//!
//! ## Key Differences
//!
//! - **[`Task`]**: Simple interface with just a `run` method
//! - **[`Node`]**: Full control with `prep`, `exec`, and `post` phases
//!
//! ## When to Use Which
//!
//! - Use **[`Task`]** when you want simplicity and don't need fine-grained phase control
//! - Use **[`Node`]** when you need the full three-phase lifecycle with retry logic and separation of concerns
//!
//! ## Example
//!
//! ```rust
//! use cano::prelude::*;
//!
//! struct SimpleTask;
//!
//! #[async_trait]
//! impl Task<String> for SimpleTask {
//!     async fn run(&self, store: &MemoryStore) -> Result<String, CanoError> {
//!         // Do all your work here
//!         store.put("result", "task completed")?;
//!         Ok("next_state".to_string())
//!     }
//! }
//! ```
//!
//! ## Interoperability
//!
//! Every [`Node`] automatically implements [`Task`], so you can use existing nodes
//! wherever tasks are expected. This provides a smooth upgrade path and backward compatibility.

use crate::error::CanoError;
use crate::store::MemoryStore;
use async_trait::async_trait;
use std::collections::HashMap;

/// Simple key-value parameters for task configuration
///
/// This is a convenience type alias for the most common parameter format used in workflows.
pub type DefaultTaskParams = HashMap<String, String>;

/// Task trait for simplified workflow processing
///
/// This trait provides a simplified interface for workflow processing compared to [`Node`].
/// Instead of implementing three separate phases (`prep`, `exec`, `post`), you only need
/// to implement a single `run` method.
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
/// - **Compatibility**: Works seamlessly with existing [`Node`] implementations
/// - **Type Safety**: Same type safety guarantees as [`Node`]
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

    /// Execute the task with the given store
    ///
    /// This method contains all the task logic in a single place. Unlike [`Node`],
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
/// This implementation makes all existing [`Node`] implementations automatically
/// work as [`Task`] implementations. The [`Node::run`] method is used directly
/// as the [`Task::run`] implementation.
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

    async fn run(&self, store: &TStore) -> Result<TState, CanoError> {
        crate::node::Node::run(self, store).await
    }
}

/// Concrete task trait object with default types
///
/// This trait provides a concrete implementation of Task using the default types,
/// enabling dynamic dispatch and trait object usage.
pub trait DynTask<TState>:
    Task<TState, MemoryStore, DefaultTaskParams>
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
                Err(CanoError::node_execution("Task intentionally failed"))
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
            let input_data: String = store.get(&self.input_key)
                .map_err(|e| CanoError::node_execution(format!("Failed to read input: {}", e)))?;

            // Process data
            let processed_data = format!("processed: {}", input_data);

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

            let handle = task::spawn(async move { 
                task_clone.run(&*store_clone).await 
            });
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
}
