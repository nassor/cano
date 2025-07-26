//! # Workflow API - Build Simple Workflows
//!
//! This module provides the core [`Workflow`] type for building async workflow systems.
//! It includes state machine-driven workflow execution with type-safe routing.
//!
//! ## 🎯 Core Concepts
//!
//! ### State Machine-Based Workflows
//!
//! The [`Workflow`] API provides a state machine-driven approach to workflow orchestration:
//! - Define your workflow states using custom enums
//! - Register nodes for each state
//! - Set up state transitions based on node outcomes
//! - Configure exit states to terminate the workflow
//!
//! ### Node Trait - Your Custom Logic
//!
//! The node trait is where you implement your custom processing logic.
//! Every node follows a simple three-phase lifecycle:
//!
//! 1. **Prep**: Load data, validate inputs, setup resources
//! 2. **Exec**: Core processing logic (with automatic retry support)
//! 3. **Post**: Store results, cleanup, determine next action
//!
//! Your custom nodes implement these three phases with their specific business logic.
//!
//! ## 🚀 Advanced Features
//!
//! ### Type-Safe State Routing
//!
//! Flows use user-defined enums for state management and routing.
//! Your nodes return enum values that determine the next state to execute,
//! providing compile-time safety and clear workflow logic.
//!
//! ### Exit State Management
//!
//! Register specific enum values as exit states to cleanly terminate workflows
//! when certain conditions are met (success, error, completion, etc.).
//!
//! ### Concurrent Workflow Execution
//!
//! The [`ConcurrentWorkflow`] feature allows you to execute multiple workflow instances
//! in parallel with configurable timeout strategies:
//!
//! - **WaitForever**: Execute all workflows to completion
//! - **WaitForQuota**: Complete a specific number of workflows, then cancel the rest
//! - **WaitDuration**: Execute workflows within a time limit
//! - **WaitQuotaOrDuration**: Complete a quota OR wait for a duration, whichever comes first
//!
//! Each workflow instance can have different parameter values while sharing the same
//! workflow structure, enabling powerful parallel processing patterns.
//!
//! ## 💡 Best Practices
//!
//! ### State Design
//!
//! - Use descriptive enum variants for workflow states
//! - Group related states logically (e.g., validation, processing, cleanup)
//! - Define clear exit states for different termination scenarios
//!
//! ### Error Handling
//!
//! - Use [`CanoError`] for rich error context
//! - Define error states in your enum for graceful error handling
//! - Configure retries at the node level for transient failures
//!
//! ### Store Usage
//!
//! - Use store to pass data between nodes
//! - Keep store keys consistent across your workflow
//! - Consider using strongly-typed store wrappers for complex data
//!
//! ### Concurrent Execution
//!
//! - Use [`ConcurrentWorkflow`] for parallel processing of similar tasks
//! - Choose appropriate timeout strategies based on your use case
//! - Monitor workflow results to handle failures gracefully
//! - Consider resource limits when executing many workflows concurrently

use std::collections::HashMap;
use std::time::Duration;

use crate::MemoryStore;
use crate::error::CanoError;
use crate::node::{DefaultParams, Node};

/// Type alias for trait objects that can store different node types
///
/// This allows the Workflow to accept nodes of different concrete types as long as they
/// implement the Node trait with compatible associated types. The trait object erases
/// the specific TParams, PrepResult, and ExecResult types but maintains the essential
/// functionality needed for workflow execution.
pub type DynNode<TState, TStore = MemoryStore, TParams = DefaultParams> =
    Box<dyn DynNodeTrait<TState, TStore, TParams> + Send + Sync>;

/// Trait object-safe version of the Node trait for dynamic dispatch
///
/// This trait provides the essential functionality needed for workflow execution
/// while being object-safe (can be used as a trait object).
#[async_trait::async_trait]
pub trait DynNodeTrait<TState, TStore = MemoryStore, TParams = DefaultParams>: Send + Sync
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Execute the node and return the next state
    async fn run(&self, store: &TStore) -> Result<TState, CanoError>;
}

/// Blanket implementation of `DynNodeTrait<TState, TStore>` for any `N: Node<TState, P, TStore>`
#[async_trait::async_trait]
impl<TState, TStore, TParams, N> DynNodeTrait<TState, TStore, TParams> for N
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
    N: Node<TState, TStore, TParams> + Send + Sync + 'static,
{
    async fn run(&self, store: &TStore) -> Result<TState, CanoError> {
        // just forward to the inherent `Node::run`
        Node::run(self, store).await
    }
}

/// State machine workflow orchestration
pub struct Workflow<TState, TStore = MemoryStore, TParams = DefaultParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// The starting state of the workflow
    pub start_state: Option<TState>,
    /// Map of states to their corresponding node trait objects
    pub state_nodes: HashMap<TState, DynNode<TState, TStore, TParams>>,
    /// Set of states that will terminate the workflow when reached
    pub exit_states: std::collections::HashSet<TState>,
}

impl<TState, TStore, TParams> Workflow<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// Create a new Workflow with a starting state
    pub fn new(start_state: TState) -> Self {
        Self {
            start_state: Some(start_state),
            state_nodes: HashMap::new(),
            exit_states: std::collections::HashSet::new(),
        }
    }

    /// Register a node for a specific state (accepts any type implementing `Node<TState>`)
    pub fn register_node<N>(&mut self, state: TState, node: N) -> &mut Self
    where
        N: Node<TState, TStore, TParams> + Send + Sync + 'static,
    {
        self.state_nodes.insert(state, Box::new(node));
        self
    }

    /// Register multiple exit states
    pub fn add_exit_states(&mut self, states: Vec<TState>) -> &mut Self {
        self.exit_states.extend(states);
        self
    }

    /// Register a single exit state
    pub fn add_exit_state(&mut self, state: TState) -> &mut Self {
        self.exit_states.insert(state);
        self
    }

    /// Set the starting state
    pub fn start(&mut self, state: TState) -> &mut Self {
        self.start_state = Some(state);
        self
    }

    /// Execute the typed workflow with state machine orchestration
    ///
    /// This method runs the workflow by starting from the initial state and transitioning
    /// between states based on node outcomes until an exit state is reached or an error occurs.
    ///
    /// ## Workflow Execution
    ///
    /// 1. Start with the configured initial state
    /// 2. Look up the node registered for the current state
    /// 3. Execute the node's `run()` method
    /// 4. Use the returned value as the next state
    /// 5. Repeat until an exit state is reached
    ///
    /// ## Error Handling
    ///
    /// - **Missing Node**: Error if no node is registered for a state
    /// - **Node Execution**: Propagate errors from node execution
    /// - **Invalid Transitions**: Error if node returns an unregistered state
    ///
    /// ## Return Value
    ///
    /// Returns the final state that terminated the workflow (always an exit state).
    pub async fn orchestrate(&self, store: &TStore) -> Result<TState, CanoError> {
        let mut current = self
            .start_state
            .as_ref()
            .ok_or_else(|| CanoError::workflow("No start state defined"))?
            .clone();

        loop {
            if self.exit_states.contains(&current) {
                return Ok(current);
            }

            let node = self.state_nodes.get(&current).ok_or_else(|| {
                CanoError::workflow(format!("No node registered for state: {current:?}"))
            })?;

            current = node.run(store).await?;
        }
    }
}

/// Builder for creating Workflow instances with a fluent API
///
/// Provides a convenient way to construct flows with method chaining.
pub struct WorkflowBuilder<TState, TStore = MemoryStore, TParams = DefaultParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    workflow: Workflow<TState, TStore, TParams>,
}

impl<TState, TStore, TParams> WorkflowBuilder<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// Create a new WorkflowBuilder with a starting state
    pub fn new(start_state: TState) -> Self {
        Self {
            workflow: Workflow::new(start_state),
        }
    }

    /// Register a node for a state (accepts any type implementing `Node<TState>`)
    pub fn register_node<N>(mut self, state: TState, node: N) -> Self
    where
        N: Node<TState, TStore, TParams> + Send + Sync + 'static,
    {
        self.workflow.register_node(state, node);
        self
    }

    /// Add an exit state
    pub fn add_exit_state(mut self, state: TState) -> Self {
        self.workflow.add_exit_state(state);
        self
    }

    /// Add multiple exit states
    pub fn add_exit_states(mut self, states: Vec<TState>) -> Self {
        self.workflow.add_exit_states(states);
        self
    }

    /// Build the final Workflow instance
    pub fn build(self) -> Workflow<TState, TStore, TParams> {
        self.workflow
    }
}

/// Completion strategy for concurrent workflow execution
#[derive(Debug, Clone)]
pub enum ConcurrentStrategy {
    /// Wait indefinitely for all workflows to complete
    WaitForever,
    /// Wait for a specific number of workflows to complete (minimum 1)
    WaitForQuota(usize),
    /// Wait for a maximum duration, then cancel remaining workflows
    WaitDuration(Duration),
    /// Wait for either a quota OR a duration, whichever comes first
    WaitQuotaOrDuration { quota: usize, duration: Duration },
}

/// Represents a workflow instance to be executed concurrently
/// Each instance can have different parameter values
pub struct ConcurrentWorkflowInstance<TState, TStore = MemoryStore, TParams = DefaultParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// The workflow to execute
    pub workflow: Workflow<TState, TStore, TParams>,
    /// Parameters specific to this instance (can be different for each instance)
    pub params: Option<TParams>,
    /// Unique identifier for this instance
    pub id: String,
}

impl<TState, TStore, TParams> ConcurrentWorkflowInstance<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// Create a new concurrent workflow instance
    pub fn new(id: String, workflow: Workflow<TState, TStore, TParams>) -> Self {
        Self {
            workflow,
            params: None,
            id,
        }
    }

    /// Create a new concurrent workflow instance with parameters
    pub fn with_params(
        id: String,
        workflow: Workflow<TState, TStore, TParams>,
        params: TParams,
    ) -> Self {
        Self {
            workflow,
            params: Some(params),
            id,
        }
    }
}

/// Result of a single workflow execution in a concurrent context
#[derive(Debug, Clone)]
pub struct WorkflowResult<TState>
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Unique identifier of the workflow instance
    pub id: String,
    /// The final state of the workflow execution
    pub result: Result<TState, CanoError>,
    /// Duration it took to execute this workflow
    pub duration: Duration,
}

/// Aggregated results from concurrent workflow execution
#[derive(Debug)]
pub struct ConcurrentWorkflowResults<TState>
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Number of workflows that completed successfully
    pub completed: usize,
    /// Number of workflows that were cancelled due to timeout
    pub cancelled: usize,
    /// Number of workflows that failed with errors
    pub failed: usize,
    /// Individual results for each workflow
    pub results: Vec<WorkflowResult<TState>>,
    /// Total execution duration
    pub total_duration: Duration,
}

impl<TState> ConcurrentWorkflowResults<TState>
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Get all successful workflow results
    pub fn successful_results(&self) -> Vec<&WorkflowResult<TState>> {
        self.results.iter().filter(|r| r.result.is_ok()).collect()
    }

    /// Get all failed workflow results
    pub fn failed_results(&self) -> Vec<&WorkflowResult<TState>> {
        self.results.iter().filter(|r| r.result.is_err()).collect()
    }

    /// Get the total number of workflows that were executed
    pub fn total(&self) -> usize {
        self.completed + self.cancelled + self.failed
    }
}

/// Concurrent workflow orchestrator
///
/// Executes multiple workflow instances in parallel with configurable timeout strategies.
/// Each workflow instance can have different parameter values while sharing the same
/// workflow structure.
pub struct ConcurrentWorkflow<TState, TStore = MemoryStore, TParams = DefaultParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// List of workflow instances to execute
    instances: Vec<ConcurrentWorkflowInstance<TState, TStore, TParams>>,
    /// Timeout strategy for execution
    timeout: ConcurrentStrategy,
}

impl<TState, TStore, TParams> ConcurrentWorkflow<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// Create a new concurrent workflow with timeout strategy
    pub fn new(timeout: ConcurrentStrategy) -> Self {
        Self {
            instances: Vec::new(),
            timeout,
        }
    }

    /// Add a workflow instance to be executed
    pub fn add_instance(&mut self, instance: ConcurrentWorkflowInstance<TState, TStore, TParams>) {
        self.instances.push(instance);
    }

    /// Add multiple workflow instances
    pub fn add_instances(
        &mut self,
        instances: Vec<ConcurrentWorkflowInstance<TState, TStore, TParams>>,
    ) {
        self.instances.extend(instances);
    }

    /// Execute all workflow instances concurrently according to the timeout strategy
    ///
    /// This method consumes the concurrent workflow and takes ownership of the store,
    /// spawning all workflow instances as concurrent tasks and waiting according
    /// to the configured timeout strategy.
    pub async fn orchestrate(
        self,
        store: TStore,
    ) -> Result<ConcurrentWorkflowResults<TState>, CanoError>
    where
        TStore: Clone + Send + Sync + 'static,
    {
        if self.instances.is_empty() {
            return Err(CanoError::workflow("No workflow instances to execute"));
        }

        let start_time = std::time::Instant::now();
        let mut handles = Vec::new();

        // Spawn all workflow instances as concurrent tasks
        for instance in self.instances {
            let instance_id = instance.id.clone();
            let store_clone = store.clone();

            let handle = tokio::spawn(async move {
                let task_start = std::time::Instant::now();
                let result = instance.workflow.orchestrate(&store_clone).await;
                let duration = task_start.elapsed();

                WorkflowResult {
                    id: instance_id,
                    result,
                    duration,
                }
            });

            handles.push(handle);
        }

        let num_instances = handles.len();

        // Handle different timeout strategies
        let results = match self.timeout {
            ConcurrentStrategy::WaitForever => {
                // Wait for all tasks to complete
                let mut results = Vec::new();
                for handle in handles {
                    match handle.await {
                        Ok(workflow_result) => results.push(workflow_result),
                        Err(_) => {
                            // Task was cancelled or panicked - skip
                            continue;
                        }
                    }
                }
                results
            }
            ConcurrentStrategy::WaitForQuota(quota) => {
                let quota = quota.min(handles.len()).max(1);
                let mut results = Vec::new();

                // Wait for the specified number of tasks to complete
                while results.len() < quota && !handles.is_empty() {
                    // Wait for any task to complete
                    let (result, _idx, remaining) = futures::future::select_all(handles).await;
                    handles = remaining;

                    match result {
                        Ok(workflow_result) => results.push(workflow_result),
                        Err(_) => continue, // Task was cancelled or panicked
                    }
                }

                // Cancel remaining tasks
                for handle in handles {
                    handle.abort();
                }

                results
            }
            ConcurrentStrategy::WaitDuration(duration) => {
                let mut results = Vec::new();

                match tokio::time::timeout(duration, async {
                    // Wait for all tasks to complete within timeout
                    for handle in handles {
                        if let Ok(workflow_result) = handle.await {
                            results.push(workflow_result);
                        }
                    }
                })
                .await
                {
                    Ok(_) => results, // All completed within timeout
                    Err(_) => {
                        // Timeout occurred, results contains what completed so far
                        results
                    }
                }
            }
            ConcurrentStrategy::WaitQuotaOrDuration { quota, duration } => {
                let quota = quota.min(handles.len()).max(1);
                let mut results = Vec::new();

                match tokio::time::timeout(duration, async {
                    // Wait for quota or all tasks to complete
                    while results.len() < quota && !handles.is_empty() {
                        let (result, _idx, remaining) = futures::future::select_all(handles).await;
                        handles = remaining;

                        match result {
                            Ok(workflow_result) => results.push(workflow_result),
                            Err(_) => continue,
                        }
                    }

                    // Cancel remaining tasks if quota reached
                    for handle in handles {
                        handle.abort();
                    }
                })
                .await
                {
                    Ok(_) => results,
                    Err(_) => results, // Timeout occurred, return what we have
                }
            }
        };

        let total_duration = start_time.elapsed();
        let completed = results.iter().filter(|r| r.result.is_ok()).count();
        let failed = results.iter().filter(|r| r.result.is_err()).count();
        let cancelled = num_instances - results.len();

        Ok(ConcurrentWorkflowResults {
            completed,
            cancelled,
            failed,
            results,
            total_duration,
        })
    }
}

/// Builder for creating ConcurrentWorkflow instances with a fluent API
pub struct ConcurrentWorkflowBuilder<TState, TStore = MemoryStore, TParams = DefaultParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
}

impl<TState, TStore, TParams> ConcurrentWorkflowBuilder<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// Create a new ConcurrentWorkflowBuilder
    pub fn new(timeout: ConcurrentStrategy) -> Self {
        Self {
            concurrent_workflow: ConcurrentWorkflow::new(timeout),
        }
    }

    /// Add a workflow instance
    pub fn add_instance(
        mut self,
        instance: ConcurrentWorkflowInstance<TState, TStore, TParams>,
    ) -> Self {
        self.concurrent_workflow.add_instance(instance);
        self
    }

    /// Add multiple workflow instances
    pub fn add_instances(
        mut self,
        instances: Vec<ConcurrentWorkflowInstance<TState, TStore, TParams>>,
    ) -> Self {
        self.concurrent_workflow.add_instances(instances);
        self
    }

    /// Build the final ConcurrentWorkflow instance
    pub fn build(self) -> ConcurrentWorkflow<TState, TStore, TParams> {
        self.concurrent_workflow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{KeyValueStore, MemoryStore};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio;

    // Test workflow states enum
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum TestState {
        Start,
        Process,
        Validate,
        Complete,
        Error,
        Retry,
        Cleanup,
    }

    // Simple test node that always succeeds
    #[derive(Clone)]
    struct SuccessNode {
        next_state: TestState,
        execution_counter: Arc<AtomicU32>,
    }

    impl SuccessNode {
        fn new(next_state: TestState) -> Self {
            Self {
                next_state,
                execution_counter: Arc::new(AtomicU32::new(0)),
            }
        }

        fn execution_count(&self) -> u32 {
            self.execution_counter.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Node<TestState> for SuccessNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            // Try to get previous data or create default
            let data = _store
                .get::<String>("test_data")
                .unwrap_or_else(|_| "default".to_string());
            Ok(format!("prepared: {data}"))
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            self.execution_counter.fetch_add(1, Ordering::SeqCst);
            // Simple processing - just check if data contains "prepared"
            prep_res.contains("prepared")
        }

        async fn post(
            &self,
            _store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestState, CanoError> {
            // Store the result for next node
            _store.put("last_result", exec_res)?;

            if exec_res {
                Ok(self.next_state.clone())
            } else {
                Ok(TestState::Error)
            }
        }
    }

    // Node that always fails
    #[derive(Clone)]
    struct FailureNode {
        error_message: String,
    }

    impl FailureNode {
        fn new(error_message: &str) -> Self {
            Self {
                error_message: error_message.to_string(),
            }
        }
    }

    #[async_trait]
    impl Node<TestState> for FailureNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Err(CanoError::preparation(&self.error_message))
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            false
        }

        async fn post(
            &self,
            _store: &MemoryStore,
            _exec_res: Self::ExecResult,
        ) -> Result<TestState, CanoError> {
            Ok(TestState::Error)
        }
    }

    // Node that stores data
    #[derive(Clone)]
    struct DataStoringNode {
        key: String,
        value: String,
        next_state: TestState,
    }

    impl DataStoringNode {
        fn new(key: &str, value: &str, next_state: TestState) -> Self {
            Self {
                key: key.to_string(),
                value: value.to_string(),
                next_state,
            }
        }
    }

    #[async_trait]
    impl Node<TestState> for DataStoringNode {
        type PrepResult = ();
        type ExecResult = String;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Ok(())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            self.value.clone()
        }

        async fn post(
            &self,
            store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestState, CanoError> {
            store.put(&self.key, exec_res)?;
            Ok(self.next_state.clone())
        }
    }

    // Node that reads data and decides next state based on content
    #[derive(Clone)]
    struct ConditionalNode {
        key_to_check: String,
        expected_value: String,
        success_state: TestState,
        failure_state: TestState,
    }

    impl ConditionalNode {
        fn new(
            key_to_check: &str,
            expected_value: &str,
            success_state: TestState,
            failure_state: TestState,
        ) -> Self {
            Self {
                key_to_check: key_to_check.to_string(),
                expected_value: expected_value.to_string(),
                success_state,
                failure_state,
            }
        }
    }

    #[async_trait]
    impl Node<TestState> for ConditionalNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            store.get::<String>(&self.key_to_check).map_err(|e| {
                CanoError::preparation(format!("Failed to get key '{}': {}", self.key_to_check, e))
            })
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            prep_res == self.expected_value
        }

        async fn post(
            &self,
            _store: &MemoryStore,
            exec_res: Self::ExecResult,
        ) -> Result<TestState, CanoError> {
            if exec_res {
                Ok(self.success_state.clone())
            } else {
                Ok(self.failure_state.clone())
            }
        }
    }

    #[tokio::test]
    async fn test_flow_creation() {
        let workflow: Workflow<TestState> = Workflow::new(TestState::Start);
        assert_eq!(workflow.start_state, Some(TestState::Start));
        assert!(workflow.state_nodes.is_empty());
        assert!(workflow.exit_states.is_empty());
    }

    #[tokio::test]
    async fn test_flow_register_node() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);
        let node = SuccessNode::new(TestState::Complete);

        workflow.register_node(TestState::Start, node);

        assert_eq!(workflow.state_nodes.len(), 1);
        assert!(workflow.state_nodes.contains_key(&TestState::Start));
    }

    #[tokio::test]
    async fn test_flow_add_exit_states() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

        workflow.add_exit_states(vec![TestState::Complete, TestState::Error]);

        assert_eq!(workflow.exit_states.len(), 2);
        assert!(workflow.exit_states.contains(&TestState::Complete));
        assert!(workflow.exit_states.contains(&TestState::Error));
    }

    #[tokio::test]
    async fn test_flow_add_single_exit_state() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

        workflow.add_exit_state(TestState::Complete);

        assert_eq!(workflow.exit_states.len(), 1);
        assert!(workflow.exit_states.contains(&TestState::Complete));
    }

    #[tokio::test]
    async fn test_flow_start_state_change() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

        workflow.start(TestState::Process);

        assert_eq!(workflow.start_state, Some(TestState::Process));
    }

    #[tokio::test]
    async fn test_simple_workflow_execution() {
        let mut workflow = Workflow::new(TestState::Start);
        let node = SuccessNode::new(TestState::Complete);

        workflow
            .register_node(TestState::Start, node)
            .add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_multi_step_workflow() {
        let mut workflow = Workflow::new(TestState::Start);

        // Create nodes for each step
        let start_node = SuccessNode::new(TestState::Process);
        let process_node = SuccessNode::new(TestState::Validate);
        let validate_node = SuccessNode::new(TestState::Complete);

        workflow
            .register_node(TestState::Start, start_node)
            .register_node(TestState::Process, process_node)
            .register_node(TestState::Validate, validate_node)
            .add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();

        assert_eq!(result, TestState::Complete);

        // Verify that the last result was stored by the last node
        let last_result: bool = store.get("last_result").unwrap();
        assert!(last_result);
    }

    #[tokio::test]
    async fn test_workflow_with_data_passing() {
        // Test with DataStoringNode first
        let mut flow1 = Workflow::new(TestState::Start);
        let data_node = DataStoringNode::new("workflow_data", "test_value", TestState::Process);
        flow1
            .register_node(TestState::Start, data_node)
            .add_exit_state(TestState::Process);

        let store = MemoryStore::new();
        let result1 = flow1.orchestrate(&store).await.unwrap();
        assert_eq!(result1, TestState::Process);

        // Verify data was stored
        let stored_data: String = store.get("workflow_data").unwrap();
        assert_eq!(stored_data, "test_value");

        // Test with ConditionalNode
        let mut flow2 = Workflow::new(TestState::Validate);
        let validation_node = ConditionalNode::new(
            "workflow_data",
            "test_value",
            TestState::Complete,
            TestState::Error,
        );
        flow2
            .register_node(TestState::Validate, validation_node)
            .add_exit_states(vec![TestState::Complete, TestState::Error]);

        let result2 = flow2.orchestrate(&store).await.unwrap();
        assert_eq!(result2, TestState::Complete);
    }

    #[tokio::test]
    async fn test_workflow_error_handling() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

        // Node that will fail
        let failing_node = FailureNode::new("Test failure");

        workflow
            .register_node(TestState::Start, failing_node)
            .add_exit_state(TestState::Error);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("Preparation error"));
        assert!(error.to_string().contains("Test failure"));
    }

    #[tokio::test]
    async fn test_unregistered_node_error() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

        // Don't register any nodes
        workflow.add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("No node registered for state"));
    }

    #[tokio::test]
    async fn test_no_start_state_error() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);
        workflow.start_state = None; // Manually clear start state

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("No start state defined"));
    }

    #[tokio::test]
    async fn test_immediate_exit_state() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Complete);

        workflow.add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_conditional_branching() {
        let mut workflow = Workflow::new(TestState::Start);

        // Store different values and test both paths
        let store = MemoryStore::new();
        store
            .put("branch_condition", "success".to_string())
            .unwrap();

        let conditional_node = ConditionalNode::new(
            "branch_condition",
            "success",
            TestState::Complete,
            TestState::Error,
        );

        workflow
            .register_node(TestState::Start, conditional_node)
            .add_exit_states(vec![TestState::Complete, TestState::Error]);

        let result = workflow.orchestrate(&store).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Test failure path
        store
            .put("branch_condition", "failure".to_string())
            .unwrap();
        let result2 = workflow.orchestrate(&store).await.unwrap();
        assert_eq!(result2, TestState::Error);
    }

    #[tokio::test]
    async fn test_complex_workflow_with_multiple_paths() {
        // Create separate flows for different paths since each workflow can only have one node type

        // Test the main data store path
        let mut flow1 = Workflow::new(TestState::Start);
        let start_node = DataStoringNode::new("process_data", "valid", TestState::Process);
        flow1
            .register_node(TestState::Start, start_node)
            .add_exit_state(TestState::Process);

        let store = MemoryStore::new();
        let result1 = flow1.orchestrate(&store).await.unwrap();
        assert_eq!(result1, TestState::Process);

        // Test the success node processing
        let mut flow2 = Workflow::new(TestState::Process);
        let process_node = SuccessNode::new(TestState::Validate);
        flow2
            .register_node(TestState::Process, process_node)
            .add_exit_state(TestState::Validate);

        let result2 = flow2.orchestrate(&store).await.unwrap();
        assert_eq!(result2, TestState::Validate);

        // Test the conditional validation
        let mut flow3 = Workflow::new(TestState::Validate);
        let validate_node = ConditionalNode::new(
            "process_data",
            "valid",
            TestState::Complete,
            TestState::Retry,
        );
        flow3
            .register_node(TestState::Validate, validate_node)
            .add_exit_states(vec![TestState::Complete, TestState::Retry]);

        let result3 = flow3.orchestrate(&store).await.unwrap();
        assert_eq!(result3, TestState::Complete);

        // Verify data was processed through the workflow
        let process_data: String = store.get("process_data").unwrap();
        assert_eq!(process_data, "valid");
    }

    #[tokio::test]
    async fn test_node_execution_counting() {
        let mut workflow = Workflow::new(TestState::Start);

        let start_node = SuccessNode::new(TestState::Process);
        let process_node = SuccessNode::new(TestState::Complete);

        // Keep references to check execution counts
        let start_node_ref = start_node.clone();
        let process_node_ref = process_node.clone();

        workflow
            .register_node(TestState::Start, start_node)
            .register_node(TestState::Process, process_node)
            .add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        workflow.orchestrate(&store).await.unwrap();

        // Verify each node was executed once
        assert_eq!(start_node_ref.execution_count(), 1);
        assert_eq!(process_node_ref.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_flow_builder_pattern() {
        let start_node = SuccessNode::new(TestState::Process);
        let process_node = SuccessNode::new(TestState::Complete);

        let workflow = WorkflowBuilder::<TestState>::new(TestState::Start)
            .register_node(TestState::Start, start_node)
            .register_node(TestState::Process, process_node)
            .add_exit_state(TestState::Complete)
            .build();

        assert_eq!(workflow.start_state, Some(TestState::Start));
        assert_eq!(workflow.state_nodes.len(), 2);
        assert!(workflow.exit_states.contains(&TestState::Complete));

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_flow_builder_with_multiple_exit_states() {
        let workflow: Workflow<TestState> = WorkflowBuilder::<TestState>::new(TestState::Start)
            .register_node(TestState::Start, SuccessNode::new(TestState::Complete))
            .add_exit_states(vec![
                TestState::Complete,
                TestState::Error,
                TestState::Cleanup,
            ])
            .build();

        assert_eq!(workflow.exit_states.len(), 3);
        assert!(workflow.exit_states.contains(&TestState::Complete));
        assert!(workflow.exit_states.contains(&TestState::Error));
        assert!(workflow.exit_states.contains(&TestState::Cleanup));
    }

    #[tokio::test]
    async fn test_workflow_data_persistence() {
        let mut workflow = Workflow::new(TestState::Start);

        // First node stores data
        let data_node1 = DataStoringNode::new("step1", "data1", TestState::Process);
        // Second node stores more data
        let data_node2 = DataStoringNode::new("step2", "data2", TestState::Complete);

        workflow
            .register_node(TestState::Start, data_node1)
            .register_node(TestState::Process, data_node2)
            .add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        workflow.orchestrate(&store).await.unwrap();

        // Verify both pieces of data are stored
        let data1: String = store.get("step1").unwrap();
        let data2: String = store.get("step2").unwrap();
        assert_eq!(data1, "data1");
        assert_eq!(data2, "data2");
    }

    #[tokio::test]
    async fn test_workflow_with_missing_data() {
        let mut workflow = Workflow::new(TestState::Start);

        // Node that tries to read non-existent data
        let validation_node = ConditionalNode::new(
            "missing_key",
            "expected_value",
            TestState::Complete,
            TestState::Error,
        );

        workflow
            .register_node(TestState::Start, validation_node)
            .add_exit_states(vec![TestState::Complete, TestState::Error]);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("Failed to get key"));
    }

    #[tokio::test]
    async fn test_mixed_node_types_workflow() {
        // Test with different node types in the same workflow
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

        // Register different node types
        let data_node = DataStoringNode::new("workflow_data", "test_value", TestState::Process);
        let success_node = SuccessNode::new(TestState::Validate);
        let conditional_node = ConditionalNode::new(
            "workflow_data",
            "test_value",
            TestState::Complete,
            TestState::Error,
        );

        workflow
            .register_node(TestState::Start, data_node)
            .register_node(TestState::Process, success_node)
            .register_node(TestState::Validate, conditional_node)
            .add_exit_states(vec![TestState::Complete, TestState::Error]);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify data was stored and processed correctly
        let stored_data: String = store.get("workflow_data").unwrap();
        assert_eq!(stored_data, "test_value");
    }

    // Concurrent Workflow Tests

    #[tokio::test]
    async fn test_concurrent_workflow_wait_forever() {
        let store = MemoryStore::new();

        // Create multiple workflow instances
        let mut instances = Vec::new();
        for i in 0..3 {
            let mut workflow = Workflow::new(TestState::Start);
            let node = SuccessNode::new(TestState::Complete);
            workflow
                .register_node(TestState::Start, node)
                .add_exit_state(TestState::Complete);

            let instance = ConcurrentWorkflowInstance::new(format!("workflow_{i}"), workflow);
            instances.push(instance);
        }

        let mut concurrent_workflow = ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
        concurrent_workflow.add_instances(instances);

        let results = concurrent_workflow.orchestrate(store).await.unwrap();

        assert_eq!(results.completed, 3);
        assert_eq!(results.failed, 0);
        assert_eq!(results.cancelled, 0);
        assert_eq!(results.total(), 3);
        assert_eq!(results.results.len(), 3);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_wait_for_quota() {
        let store = MemoryStore::new();

        // Create multiple workflow instances, some will be cancelled
        let mut instances = Vec::new();
        for i in 0..5 {
            let mut workflow = Workflow::new(TestState::Start);
            let node = SuccessNode::new(TestState::Complete);
            workflow
                .register_node(TestState::Start, node)
                .add_exit_state(TestState::Complete);

            let instance = ConcurrentWorkflowInstance::new(format!("workflow_{i}"), workflow);
            instances.push(instance);
        }

        let mut concurrent_workflow = ConcurrentWorkflow::new(ConcurrentStrategy::WaitForQuota(3));
        concurrent_workflow.add_instances(instances);

        let results = concurrent_workflow.orchestrate(store).await.unwrap();

        assert_eq!(results.completed, 3);
        assert_eq!(results.failed, 0);
        assert_eq!(results.cancelled, 2);
        assert_eq!(results.total(), 5);
        assert_eq!(results.results.len(), 3);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_wait_duration() {
        let store = MemoryStore::new();

        // Create workflow instances with different completion times
        let mut instances = Vec::new();
        for i in 0..3 {
            let mut workflow = Workflow::new(TestState::Start);
            let node = SuccessNode::new(TestState::Complete);
            workflow
                .register_node(TestState::Start, node)
                .add_exit_state(TestState::Complete);

            let instance = ConcurrentWorkflowInstance::new(format!("workflow_{i}"), workflow);
            instances.push(instance);
        }

        let timeout = Duration::from_millis(100); // Short timeout
        let mut concurrent_workflow =
            ConcurrentWorkflow::new(ConcurrentStrategy::WaitDuration(timeout));
        concurrent_workflow.add_instances(instances);

        let results = concurrent_workflow.orchestrate(store).await.unwrap();

        // All should complete quickly, so all should be successful
        assert_eq!(results.completed, 3);
        assert_eq!(results.failed, 0);
        assert_eq!(results.cancelled, 0);
        assert!(results.total_duration >= timeout || results.total() == 3);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_quota_or_duration() {
        let store = MemoryStore::new();

        let mut instances = Vec::new();
        for i in 0..4 {
            let mut workflow = Workflow::new(TestState::Start);
            let node = SuccessNode::new(TestState::Complete);
            workflow
                .register_node(TestState::Start, node)
                .add_exit_state(TestState::Complete);

            let instance = ConcurrentWorkflowInstance::new(format!("workflow_{i}"), workflow);
            instances.push(instance);
        }

        let timeout = Duration::from_millis(100);
        let mut concurrent_workflow =
            ConcurrentWorkflow::new(ConcurrentStrategy::WaitQuotaOrDuration {
                quota: 2,
                duration: timeout,
            });
        concurrent_workflow.add_instances(instances);

        let results = concurrent_workflow.orchestrate(store).await.unwrap();

        // Should complete at least 2 (quota) or be limited by timeout
        assert!(results.completed >= 2 || results.total_duration >= timeout);
        assert!(results.total() == 4);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_with_failures() {
        let store = MemoryStore::new();

        let mut instances = Vec::new();

        // Add successful workflows
        for i in 0..2 {
            let mut workflow = Workflow::new(TestState::Start);
            let node = SuccessNode::new(TestState::Complete);
            workflow
                .register_node(TestState::Start, node)
                .add_exit_state(TestState::Complete);

            let instance = ConcurrentWorkflowInstance::new(format!("success_{i}"), workflow);
            instances.push(instance);
        }

        // Add failing workflow
        let mut failing_workflow = Workflow::new(TestState::Start);
        let failing_node = FailureNode::new("Test failure");
        failing_workflow
            .register_node(TestState::Start, failing_node)
            .add_exit_state(TestState::Error);

        let failing_instance =
            ConcurrentWorkflowInstance::new("failing_workflow".to_string(), failing_workflow);
        instances.push(failing_instance);

        let mut concurrent_workflow = ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
        concurrent_workflow.add_instances(instances);

        let results = concurrent_workflow.orchestrate(store).await.unwrap();

        assert_eq!(results.completed, 2);
        assert_eq!(results.failed, 1);
        assert_eq!(results.cancelled, 0);
        assert_eq!(results.total(), 3);

        // Check that we have both successful and failed results
        let successful = results.successful_results();
        let failed = results.failed_results();
        assert_eq!(successful.len(), 2);
        assert_eq!(failed.len(), 1);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_builder() {
        let store = MemoryStore::new();

        let workflow1 = {
            let mut w = Workflow::new(TestState::Start);
            w.register_node(TestState::Start, SuccessNode::new(TestState::Complete))
                .add_exit_state(TestState::Complete);
            w
        };

        let workflow2 = {
            let mut w = Workflow::new(TestState::Start);
            w.register_node(TestState::Start, SuccessNode::new(TestState::Complete))
                .add_exit_state(TestState::Complete);
            w
        };

        let instance1 = ConcurrentWorkflowInstance::new("w1".to_string(), workflow1);
        let instance2 = ConcurrentWorkflowInstance::new("w2".to_string(), workflow2);

        let concurrent_workflow = ConcurrentWorkflowBuilder::new(ConcurrentStrategy::WaitForever)
            .add_instance(instance1)
            .add_instance(instance2)
            .build();

        let results = concurrent_workflow.orchestrate(store).await.unwrap();

        assert_eq!(results.completed, 2);
        assert_eq!(results.failed, 0);
        assert_eq!(results.cancelled, 0);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_with_parameters() {
        let store = MemoryStore::new();

        // Create workflow instances with different parameters
        let mut instances = Vec::new();

        let mut workflow = Workflow::new(TestState::Start);
        let node = DataStoringNode::new("param_data", "value1", TestState::Complete);
        workflow
            .register_node(TestState::Start, node)
            .add_exit_state(TestState::Complete);

        let instance = ConcurrentWorkflowInstance::with_params(
            "workflow_with_params".to_string(),
            workflow,
            vec![("key".to_string(), "value".to_string())]
                .into_iter()
                .collect(),
        );
        instances.push(instance);

        let mut concurrent_workflow = ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
        concurrent_workflow.add_instances(instances);

        let results = concurrent_workflow.orchestrate(store).await.unwrap();

        assert_eq!(results.completed, 1);
        assert_eq!(results.failed, 0);
        assert_eq!(results.cancelled, 0);

        // Verify the parameter workflow completed
        let successful = results.successful_results();
        assert_eq!(successful[0].id, "workflow_with_params");
    }

    #[tokio::test]
    async fn test_concurrent_workflow_empty_instances() {
        let store = MemoryStore::new();

        let concurrent_workflow =
            ConcurrentWorkflow::<TestState, MemoryStore>::new(ConcurrentStrategy::WaitForever);
        let result = concurrent_workflow.orchestrate(store).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("No workflow instances to execute")
        );
    }

    #[tokio::test]
    async fn test_concurrent_timeout_variants() {
        // Test that different timeout variants are created correctly
        let wait_forever = ConcurrentStrategy::WaitForever;
        let wait_quota = ConcurrentStrategy::WaitForQuota(5);
        let wait_duration = ConcurrentStrategy::WaitDuration(Duration::from_secs(30));
        let wait_quota_or_duration = ConcurrentStrategy::WaitQuotaOrDuration {
            quota: 3,
            duration: Duration::from_secs(10),
        };

        // Just ensure they compile and can be matched
        match wait_forever {
            ConcurrentStrategy::WaitForever => assert_eq!(true, true),
            _ => assert_eq!(false, true),
        }

        match wait_quota {
            ConcurrentStrategy::WaitForQuota(5) => assert_eq!(true, true),
            _ => assert_eq!(false, true),
        }

        match wait_duration {
            ConcurrentStrategy::WaitDuration(_) => assert_eq!(true, true),
            _ => assert_eq!(false, true),
        }

        match wait_quota_or_duration {
            ConcurrentStrategy::WaitQuotaOrDuration { quota: 3, .. } => assert_eq!(true, true),
            _ => assert_eq!(false, true),
        }
    }
}
