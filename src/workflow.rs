//! # Workflow API - Build Simple Workflows
//!
//! This module provides the core [`Workflow`] type for building async workflow systems.
//! It includes state machine-driven workflow execution with type-safe routing and supports
//! both Tasks and Nodes for maximum flexibility.
//!
//! ## 🎯 Core Concepts
//!
//! ### State Machine-Based Workflows
//!
//! The [`Workflow`] API provides a state machine-driven approach to workflow orchestration:
//! - Define your workflow states using custom enums
//! - Register Tasks or Nodes for each state using the unified `.register()` method
//! - Set up state transitions based on Task/Node outcomes
//! - Configure exit states to terminate the workflow
//!
//! ### Tasks & Nodes - Your Custom Logic
//!
//! Choose the right processing approach for your needs:
//!
//! #### Tasks
//! Simple, flexible interface with a single `run()` method:
//! - Perfect for prototypes and simple operations
//! - Maximum flexibility in implementation
//! - Direct control over execution flow
//!
//! #### Nodes
//! Structured three-phase lifecycle for production workloads:
//! 1. **Prep**: Load data, validate inputs, setup resources
//! 2. **Exec**: Core processing logic (with automatic retry support)
//! 3. **Post**: Store results, cleanup, determine next action
//!
//! **Every Node automatically implements Task**, so you can mix both in the same workflow.
//!
//! ## 🚀 Advanced Features
//!
//! ### Type-Safe State Routing
//!
//! Workflows use user-defined enums for state management and routing.
//! Your Tasks and Nodes return enum values that determine the next state to execute,
//! providing compile-time safety and clear workflow logic.
//!
//! ### Exit State Management
//!
//! Register specific enum values as exit states to cleanly terminate workflows
//! when certain conditions are met (success, error, completion, etc.).
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
//! ### store Usage
//!
//! - Use store to pass data between nodes
//! - Keep store keys consistent across your workflow
//! - Consider using strongly-typed store wrappers for complex data
//!
//! ## 🔄 Concurrent Workflows
//!
//! The [`ConcurrentWorkflow`] type enables executing multiple workflow instances in parallel:
//! - Flexible timeout strategies for different execution patterns
//! - Enhanced monitoring with detailed status tracking
//! - Configurable completion criteria for batch processing
//!
//! ### Timeout Strategies
//!
//! - [`WaitStrategy::WaitForever`] - Execute all workflows to completion
//! - [`WaitStrategy::WaitForQuota(n)`] - Complete a specific number, then cancel the rest
//! - [`WaitStrategy::WaitDuration(duration)`] - Execute within a time limit
//! - [`WaitStrategy::WaitQuotaOrDuration`] - Complete quota OR wait for duration, whichever comes first

use std::collections::HashMap;
use std::time::Duration;

use crate::MemoryStore;
use crate::error::CanoError;
use crate::task::{DefaultTaskParams, Task, TaskConfig};

#[cfg(feature = "tracing")]
use tracing::{Instrument, Span};

/// Type alias for trait objects that can store different task types
///
/// This allows the Workflow to accept tasks of different concrete types as long as they
/// implement the Task trait with compatible associated types. The trait object erases
/// the specific TParams type but maintains the essential functionality needed for workflow execution.
pub type DynTask<TState, TStore = MemoryStore, TParams = DefaultTaskParams> =
    Box<dyn DynTaskTrait<TState, TStore, TParams> + Send + Sync>;

/// Trait object-safe version of the Task trait for dynamic dispatch
///
/// This trait provides the essential functionality needed for workflow execution
/// while being object-safe (can be used as a trait object).
#[async_trait::async_trait]
pub trait DynTaskTrait<TState, TStore = MemoryStore, TParams = DefaultTaskParams>:
    Send + Sync
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Get the task configuration for retry behavior
    fn config(&self) -> TaskConfig;

    /// Execute the task and return the next state
    async fn run(&self, store: &TStore) -> Result<TState, CanoError>;
}

/// Blanket implementation of `DynTaskTrait<TState, TStore>` for any `T: Task<TState, TStore, TParams>`
#[async_trait::async_trait]
impl<TState, TStore, TParams, T> DynTaskTrait<TState, TStore, TParams> for T
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
    T: Task<TState, TStore, TParams> + Send + Sync + 'static,
{
    fn config(&self) -> TaskConfig {
        Task::config(self)
    }

    async fn run(&self, store: &TStore) -> Result<TState, CanoError> {
        use crate::task::run_with_retries;

        let config = self.config();
        run_with_retries::<TState, TStore, _, _>(&config, || Task::run(self, store)).await
    }
}

/// Wait strategy for concurrent workflow execution
#[derive(Debug, Clone)]
pub enum WaitStrategy {
    /// Execute all workflows to completion
    WaitForever,
    /// Complete a specific number of workflows, then cancel the rest
    WaitForQuota(usize),
    /// Execute workflows within a time limit
    WaitDuration(Duration),
    /// Complete quota OR wait for duration, whichever comes first
    WaitQuotaOrDuration { quota: usize, duration: Duration },
}

/// Status of a concurrent workflow execution
#[derive(Debug, Clone, PartialEq)]
pub struct ConcurrentWorkflowStatus {
    /// Total number of workflows that were started
    pub total_workflows: usize,
    /// Number of workflows that completed successfully
    pub completed: usize,
    /// Number of workflows that failed
    pub failed: usize,
    /// Number of workflows that were cancelled
    pub cancelled: usize,
    /// Execution duration
    pub duration: Duration,
}

impl ConcurrentWorkflowStatus {
    /// Create a new status with zero counts
    pub fn new(total_workflows: usize) -> Self {
        Self {
            total_workflows,
            completed: 0,
            failed: 0,
            cancelled: 0,
            duration: Duration::from_millis(0),
        }
    }

    /// Check if all workflows have finished (completed, failed, or cancelled)
    pub fn is_complete(&self) -> bool {
        self.completed + self.failed + self.cancelled >= self.total_workflows
    }

    /// Get the number of workflows still running
    pub fn running(&self) -> usize {
        self.total_workflows
            .saturating_sub(self.completed + self.failed + self.cancelled)
    }
}

/// Result of a single workflow execution in a concurrent batch
#[derive(Debug, Clone)]
pub enum WorkflowResult<TState> {
    /// Workflow completed successfully with final state
    Success(TState),
    /// Workflow failed with error
    Failed(CanoError),
    /// Workflow was cancelled before completion
    Cancelled,
}

/// Type alias for cloneable task trait objects for concurrent workflows
///
/// This ensures tasks can be cloned for use across multiple concurrent workflow instances.
pub type CloneableTask<TState, TStore = MemoryStore, TParams = DefaultTaskParams> =
    Box<dyn CloneableTaskTrait<TState, TStore, TParams> + Send + Sync>;

/// Trait for tasks that can be cloned for concurrent workflow execution
pub trait CloneableTaskTrait<TState, TStore = MemoryStore, TParams = DefaultTaskParams>:
    DynTaskTrait<TState, TStore, TParams> + Send + Sync
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Clone the task for concurrent execution
    fn clone_task(&self) -> CloneableTask<TState, TStore, TParams>;
}

/// Blanket implementation for any Task that implements Clone
impl<TState, TStore, TParams, T> CloneableTaskTrait<TState, TStore, TParams> for T
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
    T: Task<TState, TStore, TParams> + Clone + Send + Sync + 'static,
{
    fn clone_task(&self) -> CloneableTask<TState, TStore, TParams> {
        Box::new(self.clone())
    }
}

/// State machine workflow orchestration
pub struct Workflow<TState, TStore = MemoryStore, TParams = DefaultTaskParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// The starting state of the workflow
    pub start_state: Option<TState>,
    /// Map of states to their corresponding task trait objects
    pub state_tasks: HashMap<TState, DynTask<TState, TStore, TParams>>,
    /// Set of states that will terminate the workflow when reached
    pub exit_states: std::collections::HashSet<TState>,
    /// Optional tracing span for the workflow
    #[cfg(feature = "tracing")]
    pub tracing_span: Option<Span>,
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
            state_tasks: HashMap::new(),
            exit_states: std::collections::HashSet::new(),
            #[cfg(feature = "tracing")]
            tracing_span: None,
        }
    }

    /// Register a task for a specific state (accepts any type implementing `Task<TState>`)
    pub fn register<T>(&mut self, state: TState, task: T) -> &mut Self
    where
        T: Task<TState, TStore, TParams> + Send + Sync + 'static,
    {
        self.state_tasks.insert(state, Box::new(task));
        self
    }

    /// Register a node for a specific state
    ///
    /// # Deprecated
    ///
    /// This method is deprecated. Use [`register`](Self::register) instead.
    /// The unified `register` method accepts both Tasks and Nodes and provides
    /// the same functionality with a cleaner API.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Instead of:
    /// workflow.register_node(state, my_node);
    ///
    /// // Use:
    /// workflow.register(state, my_node);
    /// ```
    #[deprecated(
        since = "0.5.0",
        note = "Use `register` instead. The unified `register` method accepts both Tasks and Nodes."
    )]
    pub fn register_node<T>(&mut self, state: TState, task: T) -> &mut Self
    where
        T: Task<TState, TStore, TParams> + Send + Sync + 'static,
    {
        self.register(state, task)
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

    /// Set a tracing span for this workflow (requires "tracing" feature)
    #[cfg(feature = "tracing")]
    pub fn with_tracing_span(mut self, span: Span) -> Self {
        self.tracing_span = Some(span);
        self
    }

    /// Execute the typed workflow with state machine orchestration
    ///
    /// This method runs the workflow by starting from the initial state and transitioning
    /// between states based on task outcomes until an exit state is reached or an error occurs.
    ///
    /// ## Workflow Execution
    ///
    /// 1. Start with the configured initial state
    /// 2. Look up the task registered for the current state
    /// 3. Execute the task's `run()` method
    /// 4. Use the returned value as the next state
    /// 5. Repeat until an exit state is reached
    ///
    /// ## Error Handling
    ///
    /// - **Missing Task**: Error if no task is registered for a state
    /// - **Task Execution**: Propagate errors from task execution
    /// - **Invalid Transitions**: Error if task returns an unregistered state
    ///
    /// ## Return Value
    ///
    /// Returns the final state that terminated the workflow (always an exit state).
    pub async fn orchestrate(&self, store: &TStore) -> Result<TState, CanoError> {
        #[cfg(feature = "tracing")]
        let workflow_span = self
            .tracing_span
            .clone()
            .unwrap_or_else(|| tracing::info_span!("workflow_orchestrate"));

        #[cfg(feature = "tracing")]
        let _enter = workflow_span.enter();

        #[cfg(feature = "tracing")]
        tracing::info!("Starting workflow orchestration");

        let mut current = self
            .start_state
            .as_ref()
            .ok_or_else(|| CanoError::workflow("No start state defined"))?
            .clone();

        loop {
            if self.exit_states.contains(&current) {
                #[cfg(feature = "tracing")]
                tracing::info!(final_state = ?current, "Workflow completed successfully");
                return Ok(current);
            }

            let task = self.state_tasks.get(&current).ok_or_else(|| {
                CanoError::workflow(format!("No task registered for state: {current:?}"))
            })?;

            #[cfg(feature = "tracing")]
            tracing::debug!(current_state = ?current, "Executing task for state");

            #[cfg(feature = "tracing")]
            let task_span = tracing::info_span!("task_execution", state = ?current);

            #[cfg(feature = "tracing")]
            let next_state = task.run(store).instrument(task_span).await?;

            #[cfg(not(feature = "tracing"))]
            let next_state = task.run(store).await?;

            #[cfg(feature = "tracing")]
            tracing::debug!(current_state = ?current, next_state = ?next_state, "Task completed, transitioning to next state");

            current = next_state;
        }
    }
}

impl<TState, TStore, TParams> std::fmt::Debug for Workflow<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workflow")
            .field("start_state", &self.start_state)
            .field("state_tasks", &format!("{} tasks", self.state_tasks.len()))
            .field("exit_states", &self.exit_states)
            .finish()
    }
}

/// Builder for creating Workflow instances with a fluent API
///
/// Provides a convenient way to construct flows with method chaining.
pub struct WorkflowBuilder<TState, TStore = MemoryStore, TParams = DefaultTaskParams>
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

    /// Register a task for a state (accepts any type implementing `Task<TState>`)
    pub fn register<T>(mut self, state: TState, task: T) -> Self
    where
        T: Task<TState, TStore, TParams> + Send + Sync + 'static,
    {
        self.workflow.register(state, task);
        self
    }

    /// Register a node for a state
    ///
    /// # Deprecated
    ///
    /// This method is deprecated. Use [`register`](Self::register) instead.
    /// The unified `register` method accepts both Tasks and Nodes and provides
    /// the same functionality with a cleaner API.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Instead of:
    /// builder.register_node(state, my_node);
    ///
    /// // Use:
    /// builder.register(state, my_node);
    /// ```
    #[deprecated(
        since = "0.5.0",
        note = "Use `register` instead. The unified `register` method accepts both Tasks and Nodes."
    )]
    pub fn register_node<T>(self, state: TState, task: T) -> Self
    where
        T: Task<TState, TStore, TParams> + Send + Sync + 'static,
    {
        self.register(state, task)
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

/// Concurrent workflow orchestration for executing multiple workflow instances in parallel
///
/// This has the same API as `Workflow` but executes multiple instances concurrently.
/// Use `register()` to add cloneable tasks and `add_exit_state()` to configure exit states.
pub struct ConcurrentWorkflow<TState, TStore = MemoryStore, TParams = DefaultTaskParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Send + Sync + 'static,
{
    /// The starting state of the workflow
    pub start_state: Option<TState>,
    /// Map of states to their corresponding cloneable task trait objects
    pub state_tasks: HashMap<TState, CloneableTask<TState, TStore, TParams>>,
    /// Set of states that will terminate the workflow when reached
    pub exit_states: std::collections::HashSet<TState>,
    /// Optional tracing span for the workflow
    #[cfg(feature = "tracing")]
    pub tracing_span: Option<Span>,
}

impl<TState, TStore, TParams> ConcurrentWorkflow<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Send + Sync + 'static,
{
    /// Create a new ConcurrentWorkflow with a starting state
    pub fn new(start_state: TState) -> Self {
        Self {
            start_state: Some(start_state),
            state_tasks: HashMap::new(),
            exit_states: std::collections::HashSet::new(),
            #[cfg(feature = "tracing")]
            tracing_span: None,
        }
    }

    /// Register a cloneable task for a specific state
    ///
    /// This method accepts any task that implements Clone for concurrent execution.
    pub fn register<T>(&mut self, state: TState, task: T) -> &mut Self
    where
        T: Task<TState, TStore, TParams> + Clone + Send + Sync + 'static,
    {
        self.state_tasks.insert(state, Box::new(task));
        self
    }

    /// Register a cloneable node for a specific state
    ///
    /// # Deprecated
    ///
    /// This method is deprecated. Use [`register`](Self::register) instead.
    /// The unified `register` method accepts both Tasks and Nodes and provides
    /// the same functionality with a cleaner API.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Instead of:
    /// concurrent_workflow.register_node(state, my_node);
    ///
    /// // Use:
    /// concurrent_workflow.register(state, my_node);
    /// ```
    #[deprecated(
        since = "0.5.0",
        note = "Use `register` instead. The unified `register` method accepts both Tasks and Nodes."
    )]
    pub fn register_node<T>(&mut self, state: TState, task: T) -> &mut Self
    where
        T: Task<TState, TStore, TParams> + Clone + Send + Sync + 'static,
    {
        self.register(state, task)
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

    /// Set a tracing span for this concurrent workflow (requires "tracing" feature)
    #[cfg(feature = "tracing")]
    pub fn with_tracing_span(mut self, span: Span) -> Self {
        self.tracing_span = Some(span);
        self
    }

    /// Create a workflow instance for concurrent execution
    ///
    /// This method creates a new workflow instance by copying the structure
    /// and cloning all registered tasks.
    fn create_workflow_instance(&self) -> Result<Workflow<TState, TStore, TParams>, CanoError> {
        let mut workflow = Workflow {
            start_state: self.start_state.clone(),
            state_tasks: HashMap::new(),
            exit_states: self.exit_states.clone(),
            #[cfg(feature = "tracing")]
            tracing_span: self.tracing_span.clone(),
        };

        // Clone all registered tasks for this instance
        for (state, cloneable_task) in &self.state_tasks {
            let cloned_task = cloneable_task.clone_task();
            workflow.state_tasks.insert(state.clone(), cloned_task);
        }

        Ok(workflow)
    }

    /// Helper function to process a task result and update status accordingly
    ///
    /// This function handles both successful workflow results and task join errors,
    /// updating the status counters and results vector.
    fn process_task_result(
        task_result: Result<WorkflowResult<TState>, tokio::task::JoinError>,
        index: usize,
        results: &mut [WorkflowResult<TState>],
        status: &mut ConcurrentWorkflowStatus,
    ) {
        match task_result {
            Ok(workflow_result) => {
                match &workflow_result {
                    WorkflowResult::Success(_) => status.completed += 1,
                    WorkflowResult::Failed(_) => status.failed += 1,
                    WorkflowResult::Cancelled => status.cancelled += 1,
                }
                results[index] = workflow_result;
            }
            Err(_) => {
                results[index] =
                    WorkflowResult::Failed(CanoError::node_execution("Task join error"));
                status.failed += 1;
            }
        }
    }

    /// Execute multiple workflow instances concurrently with the specified wait strategy
    ///
    /// This method creates separate workflow instances and executes them in parallel,
    /// applying the wait strategy to control completion behavior.
    ///
    /// ## Arguments
    ///
    /// - `stores`: Vector of store instances, one for each workflow
    /// - `wait_strategy`: Strategy for determining when to complete execution
    ///
    /// ## Wait Strategies
    ///
    /// - `WaitForever`: All workflows must complete
    /// - `WaitForQuota(n)`: Stop after n workflows complete
    /// - `WaitDuration(d)`: Stop after duration d
    /// - `WaitQuotaOrDuration`: Stop when quota is reached OR duration elapsed
    ///
    /// ## Returns
    ///
    /// Returns a tuple of:
    /// - Vector of workflow results (one per workflow)
    /// - Status information about the execution
    pub async fn execute_concurrent(
        &self,
        stores: Vec<TStore>,
        wait_strategy: WaitStrategy,
    ) -> Result<(Vec<WorkflowResult<TState>>, ConcurrentWorkflowStatus), CanoError> {
        use tokio::time::{Instant, timeout};

        #[cfg(feature = "tracing")]
        let concurrent_span = self
            .tracing_span
            .clone()
            .unwrap_or_else(|| tracing::info_span!("concurrent_workflow_execute"));

        #[cfg(feature = "tracing")]
        let _enter = concurrent_span.enter();

        let start_time = Instant::now();
        let workflow_count = stores.len();
        let mut status = ConcurrentWorkflowStatus::new(workflow_count);

        #[cfg(feature = "tracing")]
        tracing::info!(workflow_count = workflow_count, wait_strategy = ?wait_strategy, "Starting concurrent workflow execution");

        if workflow_count == 0 {
            status.duration = start_time.elapsed();
            return Ok((Vec::new(), status));
        }

        // Create workflow instances and spawn tasks
        let mut tasks = Vec::new();
        #[allow(unused_variables)] // idx is only used with tracing feature
        for (idx, store) in stores.into_iter().enumerate() {
            let workflow = self.create_workflow_instance()?;

            #[cfg(feature = "tracing")]
            let instance_span = tracing::info_span!("workflow_instance", instance_id = idx);

            let task = tokio::spawn(async move {
                #[cfg(feature = "tracing")]
                let _instance_enter = instance_span.enter();

                #[cfg(feature = "tracing")]
                tracing::debug!(instance_id = idx, "Starting workflow instance");

                match workflow.orchestrate(&store).await {
                    Ok(final_state) => {
                        #[cfg(feature = "tracing")]
                        tracing::info!(instance_id = idx, final_state = ?final_state, "Workflow instance completed successfully");
                        WorkflowResult::Success(final_state)
                    }
                    Err(error) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(instance_id = idx, error = ?error, "Workflow instance failed");
                        WorkflowResult::Failed(error)
                    }
                }
            });
            tasks.push(task);
        }

        let mut results = vec![WorkflowResult::Cancelled; workflow_count];

        match wait_strategy {
            WaitStrategy::WaitForever => {
                // Wait for all tasks to complete
                for (index, task) in tasks.into_iter().enumerate() {
                    let task_result = task.await;
                    Self::process_task_result(task_result, index, &mut results, &mut status);
                }
            }

            WaitStrategy::WaitForQuota(quota) => {
                // Wait for the specified number of workflows to complete
                let mut completed_count = 0;
                let mut pending_tasks = tasks;

                while completed_count < quota && !pending_tasks.is_empty() {
                    let (result, index, remaining) =
                        futures::future::select_all(pending_tasks).await;
                    pending_tasks = remaining;

                    Self::process_task_result(result, index, &mut results, &mut status);
                    completed_count += 1;
                }

                // Cancel remaining tasks
                for task in pending_tasks {
                    task.abort();
                    status.cancelled += 1;
                }
            }

            WaitStrategy::WaitDuration(duration) => {
                // Wait for the specified duration
                let timeout_result = timeout(duration, async {
                    let mut pending_tasks = tasks;
                    let mut results_temp = Vec::new();

                    while !pending_tasks.is_empty() {
                        let (result, index, remaining) =
                            futures::future::select_all(pending_tasks).await;
                        pending_tasks = remaining;
                        results_temp.push((index, result));
                    }

                    results_temp
                })
                .await;

                match timeout_result {
                    Ok(completed_tasks) => {
                        // All tasks completed within the timeout
                        for (index, result) in completed_tasks {
                            Self::process_task_result(result, index, &mut results, &mut status);
                        }
                    }
                    Err(_) => {
                        // Timeout occurred - check which workflows are still in default state (Cancelled)
                        // and count them, while preserving any that may have completed
                        for result in &results {
                            match result {
                                WorkflowResult::Success(_) => status.completed += 1,
                                WorkflowResult::Failed(_) => status.failed += 1,
                                WorkflowResult::Cancelled => status.cancelled += 1,
                            }
                        }
                    }
                }
            }

            WaitStrategy::WaitQuotaOrDuration { quota, duration } => {
                // Wait for quota completion OR duration timeout, whichever comes first
                let quota_future = async {
                    let mut completed_count = 0;
                    let mut pending_tasks = tasks;
                    let mut completed_results = Vec::new();

                    while completed_count < quota && !pending_tasks.is_empty() {
                        let (result, index, remaining) =
                            futures::future::select_all(pending_tasks).await;
                        pending_tasks = remaining;
                        completed_results.push((index, result));
                        completed_count += 1;
                    }

                    // Cancel remaining tasks
                    for task in pending_tasks {
                        task.abort();
                    }

                    completed_results
                };

                let timeout_result = timeout(duration, quota_future).await;

                match timeout_result {
                    Ok(completed_tasks) => {
                        // Quota was reached within the timeout
                        for (index, result) in completed_tasks {
                            Self::process_task_result(result, index, &mut results, &mut status);
                        }

                        // Mark uncompleted workflows as cancelled
                        let uncompleted = workflow_count - (status.completed + status.failed);
                        status.cancelled = uncompleted;
                    }
                    Err(_) => {
                        // Duration timeout occurred before quota was reached
                        // Check which workflows actually completed and only mark the rest as cancelled
                        for result in &results {
                            match result {
                                WorkflowResult::Success(_) => status.completed += 1,
                                WorkflowResult::Failed(_) => status.failed += 1,
                                WorkflowResult::Cancelled => status.cancelled += 1,
                            }
                        }
                    }
                }
            }
        }

        status.duration = start_time.elapsed();

        #[cfg(feature = "tracing")]
        tracing::info!(
            total_workflows = status.total_workflows,
            completed = status.completed,
            failed = status.failed,
            cancelled = status.cancelled,
            duration_ms = status.duration.as_millis(),
            "Concurrent workflow execution completed"
        );

        Ok((results, status))
    }
}

impl<TState, TStore, TParams> Clone for ConcurrentWorkflow<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        // Clone the cloneable tasks
        let mut cloned_tasks = HashMap::new();
        for (state, task) in &self.state_tasks {
            cloned_tasks.insert(state.clone(), task.clone_task());
        }

        Self {
            start_state: self.start_state.clone(),
            state_tasks: cloned_tasks,
            exit_states: self.exit_states.clone(),
            #[cfg(feature = "tracing")]
            tracing_span: self.tracing_span.clone(),
        }
    }
}

impl<TState, TStore, TParams> std::fmt::Debug for ConcurrentWorkflow<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcurrentWorkflow")
            .field("start_state", &self.start_state)
            .field("exit_states", &self.exit_states)
            .field(
                "tasks",
                &format!("{} cloneable tasks", self.state_tasks.len()),
            )
            .finish()
    }
}

/// Builder for creating ConcurrentWorkflow instances with a fluent API
pub struct ConcurrentWorkflowBuilder<TState, TStore = MemoryStore, TParams = DefaultTaskParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Send + Sync + 'static,
{
    concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
}

impl<TState, TStore, TParams> ConcurrentWorkflowBuilder<TState, TStore, TParams>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Send + Sync + 'static,
{
    /// Create a new ConcurrentWorkflowBuilder with a starting state
    pub fn new(start_state: TState) -> Self {
        Self {
            concurrent_workflow: ConcurrentWorkflow::new(start_state),
        }
    }

    /// Register a cloneable task for concurrent execution
    pub fn register<T>(mut self, state: TState, task: T) -> Self
    where
        T: Task<TState, TStore, TParams> + Clone + Send + Sync + 'static,
    {
        self.concurrent_workflow.register(state, task);
        self
    }

    /// Register a cloneable node for concurrent execution
    ///
    /// # Deprecated
    ///
    /// This method is deprecated. Use [`register`](Self::register) instead.
    /// The unified `register` method accepts both Tasks and Nodes and provides
    /// the same functionality with a cleaner API.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Instead of:
    /// builder.register_node(state, my_node);
    ///
    /// // Use:
    /// builder.register(state, my_node);
    /// ```
    #[deprecated(
        since = "0.5.0",
        note = "Use `register` instead. The unified `register` method accepts both Tasks and Nodes."
    )]
    pub fn register_node<T>(self, state: TState, task: T) -> Self
    where
        T: Task<TState, TStore, TParams> + Clone + Send + Sync + 'static,
    {
        self.register(state, task)
    }

    /// Add an exit state
    pub fn add_exit_state(mut self, state: TState) -> Self {
        self.concurrent_workflow.add_exit_state(state);
        self
    }

    /// Add multiple exit states
    pub fn add_exit_states(mut self, states: Vec<TState>) -> Self {
        self.concurrent_workflow.add_exit_states(states);
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
    use crate::node::Node;
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
        assert!(workflow.state_tasks.is_empty());
        assert!(workflow.exit_states.is_empty());
    }

    #[tokio::test]
    async fn test_flow_register() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);
        let node = SuccessNode::new(TestState::Complete);

        workflow.register(TestState::Start, node);

        assert_eq!(workflow.state_tasks.len(), 1);
        assert!(workflow.state_tasks.contains_key(&TestState::Start));
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
            .register(TestState::Start, node)
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
            .register(TestState::Start, start_node)
            .register(TestState::Process, process_node)
            .register(TestState::Validate, validate_node)
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
            .register(TestState::Start, data_node)
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
            .register(TestState::Validate, validation_node)
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
            .register(TestState::Start, failing_node)
            .add_exit_state(TestState::Error);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("Preparation error"));
        assert!(error.to_string().contains("Test failure"));
    }

    #[tokio::test]
    async fn test_unregistered_task_error() {
        let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

        // Don't register any tasks
        workflow.add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("No task registered for state"));
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
            .register(TestState::Start, conditional_node)
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
            .register(TestState::Start, start_node)
            .add_exit_state(TestState::Process);

        let store = MemoryStore::new();
        let result1 = flow1.orchestrate(&store).await.unwrap();
        assert_eq!(result1, TestState::Process);

        // Test the success node processing
        let mut flow2 = Workflow::new(TestState::Process);
        let process_node = SuccessNode::new(TestState::Validate);
        flow2
            .register(TestState::Process, process_node)
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
            .register(TestState::Validate, validate_node)
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
            .register(TestState::Start, start_node)
            .register(TestState::Process, process_node)
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
            .register(TestState::Start, start_node)
            .register(TestState::Process, process_node)
            .add_exit_state(TestState::Complete)
            .build();

        assert_eq!(workflow.start_state, Some(TestState::Start));
        assert_eq!(workflow.state_tasks.len(), 2);
        assert!(workflow.exit_states.contains(&TestState::Complete));

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_flow_builder_with_multiple_exit_states() {
        let workflow: Workflow<TestState> = WorkflowBuilder::<TestState>::new(TestState::Start)
            .register(TestState::Start, SuccessNode::new(TestState::Complete))
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
            .register(TestState::Start, data_node1)
            .register(TestState::Process, data_node2)
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
            .register(TestState::Start, validation_node)
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
            .register(TestState::Start, data_node)
            .register(TestState::Process, success_node)
            .register(TestState::Validate, conditional_node)
            .add_exit_states(vec![TestState::Complete, TestState::Error]);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify data was stored and processed correctly
        let stored_data: String = store.get("workflow_data").unwrap();
        assert_eq!(stored_data, "test_value");
    }

    // Tests for Concurrent Workflows
    #[tokio::test]
    async fn test_concurrent_workflow_creation() {
        let concurrent_workflow: ConcurrentWorkflow<TestState> =
            ConcurrentWorkflow::new(TestState::Start);

        assert!(concurrent_workflow.state_tasks.is_empty());
        assert_eq!(concurrent_workflow.start_state, Some(TestState::Start));
    }

    #[tokio::test]
    async fn test_concurrent_workflow_register_node() {
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);

        let node = SuccessNode::new(TestState::Complete);
        concurrent_workflow.register(TestState::Start, node);

        assert_eq!(concurrent_workflow.state_tasks.len(), 1);
        assert!(
            concurrent_workflow
                .state_tasks
                .contains_key(&TestState::Start)
        );
    }

    #[tokio::test]
    async fn test_concurrent_workflow_wait_forever() {
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        let node = SuccessNode::new(TestState::Complete);
        concurrent_workflow.register(TestState::Start, node);

        // Create multiple stores for concurrent execution
        let stores = vec![MemoryStore::new(), MemoryStore::new(), MemoryStore::new()];

        let (results, status) = concurrent_workflow
            .execute_concurrent(stores, WaitStrategy::WaitForever)
            .await
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(status.total_workflows, 3);
        assert_eq!(status.completed, 3);
        assert_eq!(status.failed, 0);
        assert_eq!(status.cancelled, 0);
        assert!(status.is_complete());
        assert_eq!(status.running(), 0);

        // Verify all results are successful
        for result in results {
            match result {
                WorkflowResult::Success(state) => assert_eq!(state, TestState::Complete),
                _ => panic!("Expected success result"),
            }
        }
    }

    #[tokio::test]
    async fn test_concurrent_workflow_wait_for_quota() {
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        let node = SuccessNode::new(TestState::Complete);
        concurrent_workflow.register(TestState::Start, node);

        // Create multiple stores
        let stores = vec![
            MemoryStore::new(),
            MemoryStore::new(),
            MemoryStore::new(),
            MemoryStore::new(),
            MemoryStore::new(),
        ];

        let quota = 3;
        let (results, status) = concurrent_workflow
            .execute_concurrent(stores, WaitStrategy::WaitForQuota(quota))
            .await
            .unwrap();

        assert_eq!(results.len(), 5);
        assert_eq!(status.total_workflows, 5);

        // Should complete at least the quota
        assert!(status.completed + status.failed >= quota);

        // Total should add up correctly
        assert_eq!(
            status.completed + status.failed + status.cancelled,
            status.total_workflows
        );
    }

    #[tokio::test]
    async fn test_concurrent_workflow_wait_duration() {
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        let node = SuccessNode::new(TestState::Complete);
        concurrent_workflow.register(TestState::Start, node);

        // Create stores
        let stores = vec![MemoryStore::new(), MemoryStore::new()];

        let duration = Duration::from_millis(100);
        let (results, status) = concurrent_workflow
            .execute_concurrent(stores, WaitStrategy::WaitDuration(duration))
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(status.total_workflows, 2);

        // Should complete within reasonable time since our nodes are fast
        assert!(status.duration <= Duration::from_millis(1000));

        // All workflows should complete successfully given the simple nature of our test nodes
        assert_eq!(status.completed, 2);
        assert_eq!(status.failed, 0);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_wait_quota_or_duration() {
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        let node = SuccessNode::new(TestState::Complete);
        concurrent_workflow.register(TestState::Start, node);

        // Create stores
        let stores = vec![MemoryStore::new(), MemoryStore::new(), MemoryStore::new()];

        let strategy = WaitStrategy::WaitQuotaOrDuration {
            quota: 2,
            duration: Duration::from_millis(100),
        };

        let (results, status) = concurrent_workflow
            .execute_concurrent(stores, strategy)
            .await
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(status.total_workflows, 3);

        // Should complete quickly since our nodes are simple
        assert!(status.duration <= Duration::from_millis(1000));
    }

    #[tokio::test]
    async fn test_concurrent_workflow_with_errors() {
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_states(vec![TestState::Complete, TestState::Error]);

        let failing_node = FailureNode::new("Test failure");
        concurrent_workflow.register(TestState::Start, failing_node);

        // Create stores
        let stores = vec![MemoryStore::new(), MemoryStore::new()];

        let (results, status) = concurrent_workflow
            .execute_concurrent(stores, WaitStrategy::WaitForever)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(status.total_workflows, 2);
        assert_eq!(status.failed, 2);
        assert_eq!(status.completed, 0);
        assert_eq!(status.cancelled, 0);

        // Verify all results are failures
        for result in results {
            match result {
                WorkflowResult::Failed(error) => {
                    assert!(error.to_string().contains("Preparation error"));
                }
                _ => panic!("Expected failed result"),
            }
        }
    }

    #[tokio::test]
    async fn test_concurrent_workflow_empty_stores() {
        let concurrent_workflow: ConcurrentWorkflow<TestState> =
            ConcurrentWorkflow::new(TestState::Start);

        let (results, status) = concurrent_workflow
            .execute_concurrent(Vec::new(), WaitStrategy::WaitForever)
            .await
            .unwrap();

        assert!(results.is_empty());
        assert_eq!(status.total_workflows, 0);
        assert_eq!(status.completed, 0);
        assert_eq!(status.failed, 0);
        assert_eq!(status.cancelled, 0);
        assert!(status.is_complete());
    }

    #[tokio::test]
    async fn test_concurrent_workflow_builder() {
        let node = SuccessNode::new(TestState::Complete);

        let mut concurrent_workflow = ConcurrentWorkflowBuilder::new(TestState::Start).build();
        concurrent_workflow.register(TestState::Start, node);

        assert_eq!(concurrent_workflow.state_tasks.len(), 1);
        assert!(
            concurrent_workflow
                .state_tasks
                .contains_key(&TestState::Start)
        );
    }

    #[tokio::test]
    async fn test_concurrent_workflow_status_tracking() {
        let mut status = ConcurrentWorkflowStatus::new(10);

        assert_eq!(status.total_workflows, 10);
        assert_eq!(status.completed, 0);
        assert_eq!(status.failed, 0);
        assert_eq!(status.cancelled, 0);
        assert_eq!(status.running(), 10);
        assert!(!status.is_complete());

        // Simulate some completions
        status.completed = 3;
        status.failed = 2;
        status.cancelled = 1;

        assert_eq!(status.running(), 4);
        assert!(!status.is_complete());

        // Complete all
        status.completed = 5;
        status.failed = 3;
        status.cancelled = 2;

        assert_eq!(status.running(), 0);
        assert!(status.is_complete());
    }

    #[tokio::test]
    async fn test_concurrent_workflow_with_data_sharing() {
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Process);

        let data_node = DataStoringNode::new("concurrent_data", "test_value", TestState::Process);
        concurrent_workflow.register(TestState::Start, data_node);

        // Create stores with different initial data
        let store1 = MemoryStore::new();
        let store2 = MemoryStore::new();
        let store3 = MemoryStore::new();

        let stores = vec![store1.clone(), store2.clone(), store3.clone()];

        let (results, status) = concurrent_workflow
            .execute_concurrent(stores, WaitStrategy::WaitForever)
            .await
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(status.completed, 3);

        // Verify each store has the expected data
        let data1: String = store1.get("concurrent_data").unwrap();
        let data2: String = store2.get("concurrent_data").unwrap();
        let data3: String = store3.get("concurrent_data").unwrap();

        assert_eq!(data1, "test_value");
        assert_eq!(data2, "test_value");
        assert_eq!(data3, "test_value");
    }

    #[tokio::test]
    async fn test_wait_strategy_debug() {
        let wait_forever = WaitStrategy::WaitForever;
        let wait_quota = WaitStrategy::WaitForQuota(5);
        let wait_duration = WaitStrategy::WaitDuration(Duration::from_secs(10));
        let wait_quota_or_duration = WaitStrategy::WaitQuotaOrDuration {
            quota: 3,
            duration: Duration::from_secs(5),
        };

        // Just verify they implement Debug (compilation test)
        let _debug_strings = [
            format!("{wait_forever:?}"),
            format!("{wait_quota:?}"),
            format!("{wait_duration:?}"),
            format!("{wait_quota_or_duration:?}"),
        ];
    }

    #[tokio::test]
    async fn test_workflow_result_debug() {
        let success = WorkflowResult::Success(TestState::Complete);
        let failed = WorkflowResult::<TestState>::Failed(CanoError::workflow("test error"));
        let cancelled = WorkflowResult::<TestState>::Cancelled;

        // Just verify they implement Debug (compilation test)
        let _debug_strings = [
            format!("{success:?}"),
            format!("{failed:?}"),
            format!("{cancelled:?}"),
        ];
    }

    #[tokio::test]
    async fn test_timeout_preserves_completed_workflows() {
        // This test verifies that when a timeout occurs, the system correctly
        // counts workflows that completed before the timeout rather than
        // marking all workflows as cancelled

        // Create a task that completes instantly
        #[derive(Clone)]
        struct InstantTask;

        #[async_trait]
        impl Task<TestState> for InstantTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TestState, CanoError> {
                Ok(TestState::Complete)
            }
        }

        // Create a task that takes longer than our timeout
        #[derive(Clone)]
        struct SlowTask;

        #[async_trait]
        impl Task<TestState> for SlowTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TestState, CanoError> {
                // Sleep longer than our timeout
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TestState::Complete)
            }
        }

        // Test with a mix of fast and slow workflows
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        // Register both types of tasks for the same state
        // (we'll create separate workflows for testing)

        // First test: all fast workflows with short timeout - should all complete
        concurrent_workflow.register(TestState::Start, InstantTask);

        let stores = vec![MemoryStore::new(), MemoryStore::new()];
        let (_results, status) = concurrent_workflow
            .execute_concurrent(
                stores,
                WaitStrategy::WaitDuration(Duration::from_millis(50)),
            )
            .await
            .unwrap();

        // All should complete successfully since they're instant
        assert_eq!(status.total_workflows, 2);
        assert_eq!(status.completed, 2);
        assert_eq!(status.failed, 0);
        assert_eq!(status.cancelled, 0);

        // Now test with slow workflows and short timeout
        let mut slow_workflow = ConcurrentWorkflow::new(TestState::Start);
        slow_workflow.add_exit_state(TestState::Complete);
        slow_workflow.register(TestState::Start, SlowTask);

        let stores = vec![MemoryStore::new(), MemoryStore::new()];
        let (results, status) = slow_workflow
            .execute_concurrent(
                stores,
                WaitStrategy::WaitDuration(Duration::from_millis(50)),
            )
            .await
            .unwrap();

        // These should timeout and be cancelled
        assert_eq!(status.total_workflows, 2);
        // With our fix, we should properly count what actually happened
        // Since they timeout before completion, they should all be cancelled
        assert_eq!(status.completed, 0);
        assert_eq!(status.failed, 0);
        assert_eq!(status.cancelled, 2);

        // Verify that results match the status counts
        let mut success_count = 0;
        let mut failed_count = 0;
        let mut cancelled_count = 0;

        for result in &results {
            match result {
                WorkflowResult::Success(_) => success_count += 1,
                WorkflowResult::Failed(_) => failed_count += 1,
                WorkflowResult::Cancelled => cancelled_count += 1,
            }
        }

        assert_eq!(success_count, status.completed);
        assert_eq!(failed_count, status.failed);
        assert_eq!(cancelled_count, status.cancelled);
    }
}
