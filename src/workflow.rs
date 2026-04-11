//! # Workflow API - Build Workflows with Split/Join Support
//!
//! This module provides the core [`Workflow`] type for building async workflow systems
//! with support for parallel task execution through split/join patterns.
//!
//! ## 🎯 Core Concepts
//!
//! ### Split/Join Workflows
//!
//! The [`Workflow`] API now supports splitting into multiple parallel tasks:
//! - Define your workflow states using custom enums
//! - Register tasks for each state
//! - Use `register_split()` to execute multiple tasks in parallel
//! - Configure join strategies to control when workflow continues
//! - Set timeouts at both workflow and state level
//!
//! ### Join Strategies
//!
//! - **All**: Wait for all tasks to complete successfully
//! - **Any**: Continue after any single task completes
//! - **Quorum**: Wait for a specific number of tasks to complete successfully
//! - **Percentage**: Wait for a percentage of tasks to complete successfully
//! - **PartialResults**: Accept partial results after minimum tasks complete successfully,
//!   cancel remaining tasks, and track both successes and errors
//! - **PartialTimeout**: Accept whatever completes before timeout, cancel incomplete tasks,
//!   and proceed with completed tasks (requires timeout configuration)
//!
//! ## 📊 Partial Results Strategy
//!
//! The `PartialResults` strategy is designed for scenarios where you want to:
//! - Optimize for latency by cancelling slow tasks
//! - Handle both successes and failures gracefully
//! - Implement fault-tolerant systems with redundancy
//! - Get results as soon as a minimum threshold is met
//!
//! ### Key Features
//!
//! - **Early Cancellation**: Once the minimum number of tasks complete successfully,
//!   remaining tasks are cancelled
//! - **Result Tracking**: Tracks successful, failed, and cancelled tasks separately
//! - **Store Integration**: Optionally stores result summaries in the workflow store
//! - **Flexible Thresholds**: Configure minimum number of completions needed
//!
//! ### Example
//!
//! ```rust
//! use cano::prelude::*;
//!
//! # #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! # enum State { Start, Process, Complete }
//! # #[derive(Clone)]
//! # struct MyTask;
//! # #[async_trait]
//! # impl Task<State> for MyTask {
//! #     async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<State>, CanoError> {
//! #         Ok(TaskResult::Single(State::Complete))
//! #     }
//! # }
//! # async fn example() -> Result<(), CanoError> {
//! let store = MemoryStore::new();
//! let tasks = vec![MyTask, MyTask, MyTask, MyTask];
//!
//! // Wait for 2 tasks to complete, then cancel the rest
//! let join_config = JoinConfig::new(
//!     JoinStrategy::PartialResults(2),
//!     State::Process
//! ).with_store_partial_results(true);
//!
//! let workflow = Workflow::new(store)
//!     .register_split(State::Start, tasks, join_config)
//!     .add_exit_state(State::Complete);
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::error::CanoError;
use crate::store::{KeyValueStore, MemoryStore};
use crate::task::{DefaultTaskParams, Task, TaskResult};

#[cfg(feature = "tracing")]
use tracing::{Span, debug, info, info_span, warn};

use futures_util::stream::{FuturesUnordered, StreamExt};

/// Helper to abort task on drop to ensure proper cancellation
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> std::future::Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}

/// Strategy for joining parallel tasks
#[derive(Clone, Debug, PartialEq)]
pub enum JoinStrategy {
    /// All tasks must complete successfully
    All,
    /// Any single task completion triggers join
    Any,
    /// Specific number of tasks must complete
    Quorum(usize),
    /// Percentage of tasks must complete (0.0 to 1.0)
    Percentage(f64),
    /// Accept partial results - continues after minimum tasks complete successfully,
    /// cancels remaining tasks, and returns both successes and errors
    /// Parameter is the minimum number of tasks that must complete successfully
    PartialResults(usize),
    /// Accept whatever completes before timeout - cancels tasks that haven't completed
    /// when timeout expires, and proceeds with completed tasks (successes and failures)
    /// Requires timeout to be set in JoinConfig
    PartialTimeout,
}

impl JoinStrategy {
    /// Check if the join condition is met based on completed/total tasks
    pub fn is_satisfied(&self, completed: usize, total: usize) -> bool {
        match self {
            JoinStrategy::All => completed >= total,
            JoinStrategy::Any => completed >= 1,
            JoinStrategy::Quorum(n) => completed >= *n,
            JoinStrategy::Percentage(p) => {
                let required = (total as f64 * p).ceil() as usize;
                completed >= required
            }
            JoinStrategy::PartialResults(min) => completed >= *min,
            JoinStrategy::PartialTimeout => completed >= 1, // At least one task must complete
        }
    }
}

/// Result of a single split task execution
#[derive(Clone, Debug)]
pub struct SplitTaskResult<TState> {
    /// Index of the task in the split tasks vector
    pub task_index: usize,
    /// Result of the task execution
    pub result: Result<TaskResult<TState>, CanoError>,
}

/// Collection of split task results with both successes and errors
#[derive(Clone, Debug)]
pub struct SplitResult<TState> {
    /// Successfully completed tasks
    pub successes: Vec<SplitTaskResult<TState>>,
    /// Failed tasks
    pub errors: Vec<SplitTaskResult<TState>>,
    /// Tasks that were cancelled (not started or aborted)
    pub cancelled: Vec<usize>,
}

impl<TState> SplitResult<TState> {
    /// Create a new empty split result
    pub fn new() -> Self {
        Self {
            successes: Vec::new(),
            errors: Vec::new(),
            cancelled: Vec::new(),
        }
    }

    /// Total number of tasks that completed (success or error)
    pub fn completed_count(&self) -> usize {
        self.successes.len() + self.errors.len()
    }

    /// Total number of tasks including cancelled
    pub fn total_count(&self) -> usize {
        self.successes.len() + self.errors.len() + self.cancelled.len()
    }
}

impl<TState> Default for SplitResult<TState> {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for join behavior after split tasks
#[must_use]
#[derive(Clone)]
pub struct JoinConfig<TState> {
    /// Strategy to determine when to proceed
    pub strategy: JoinStrategy,
    /// Optional timeout for the split execution
    pub timeout: Option<Duration>,
    /// State to transition to after join condition is met
    pub join_state: TState,
    /// Whether to store partial results in the store (for PartialResults strategy)
    /// Results will be stored under the key "split_results"
    pub store_partial_results: bool,
}

impl<TState> JoinConfig<TState>
where
    TState: Clone,
{
    /// Create a new join configuration
    pub fn new(strategy: JoinStrategy, join_state: TState) -> Self {
        Self {
            strategy,
            timeout: None,
            join_state,
            store_partial_results: false,
        }
    }

    /// Set timeout for the split execution
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Enable or disable writing split-execution summaries to the workflow store.
    ///
    /// When `true`, the following keys are written after each split execution:
    ///
    /// | Key | Type | Description |
    /// |-----|------|-------------|
    /// | `split_results_summary` | `String` | Human-readable summary of the split results |
    /// | `split_successes_count` | `usize` | Number of tasks that completed successfully |
    /// | `split_errors_count` | `usize` | Number of tasks that returned an error |
    /// | `split_cancelled_count` | `usize` | Number of tasks that were cancelled |
    ///
    /// All keys are overwritten on each split execution. Downstream states can read these
    /// counts directly from the store after the join completes.
    ///
    /// Defaults to `false`.
    pub fn with_store_partial_results(mut self, store: bool) -> Self {
        self.store_partial_results = store;
        self
    }
}

/// Entry in the workflow state machine
pub enum StateEntry<TState, TStore = MemoryStore>
where
    TState: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    /// Single task execution
    Single {
        task: Arc<dyn Task<TState, TStore, DefaultTaskParams> + Send + Sync>,
    },
    /// Split into parallel tasks with join configuration
    Split {
        tasks: Vec<Arc<dyn Task<TState, TStore, DefaultTaskParams> + Send + Sync>>,
        join_config: Arc<JoinConfig<TState>>,
    },
}

impl<TState, TStore> Clone for StateEntry<TState, TStore>
where
    TState: Clone + Send + Sync + 'static,
    TStore: Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        match self {
            StateEntry::Single { task } => StateEntry::Single { task: task.clone() },
            StateEntry::Split { tasks, join_config } => StateEntry::Split {
                tasks: tasks.clone(),
                join_config: join_config.clone(),
            },
        }
    }
}

/// State machine workflow orchestration with split/join support
///
/// The store type defaults to [`MemoryStore`]. To use a custom store backend,
/// specify the store type explicitly:
///
/// ```rust,ignore
/// let workflow = Workflow::<MyState, MyCustomStore>::new(my_store);
/// ```
#[must_use]
pub struct Workflow<TState, TStore = MemoryStore>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TStore: KeyValueStore + 'static,
{
    /// State machine with support for split/join
    states: HashMap<TState, StateEntry<TState, TStore>>,
    /// Shared store for all tasks
    store: Arc<TStore>,
    /// Global workflow timeout
    workflow_timeout: Option<Duration>,
    /// Exit states that terminate workflow
    exit_states: std::collections::HashSet<TState>,
    /// Optional tracing span
    #[cfg(feature = "tracing")]
    tracing_span: Option<Span>,
}

impl<TState, TStore> Workflow<TState, TStore>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TStore: KeyValueStore + 'static,
{
    /// Create a new workflow with the given store
    pub fn new(store: TStore) -> Self {
        Self {
            states: HashMap::new(),
            store: Arc::new(store),
            workflow_timeout: None,
            exit_states: std::collections::HashSet::new(),
            #[cfg(feature = "tracing")]
            tracing_span: None,
        }
    }

    /// Set global workflow timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.workflow_timeout = Some(timeout);
        self
    }

    /// Register a single task for a state.
    ///
    /// Associates `task` with `state`. If a handler was already registered for `state`,
    /// it is replaced. This method is infallible.
    pub fn register<T>(mut self, state: TState, task: T) -> Self
    where
        T: Task<TState, TStore, DefaultTaskParams> + Send + Sync + 'static,
    {
        self.states.insert(
            state,
            StateEntry::Single {
                task: Arc::new(task),
            },
        );
        self
    }

    /// Register multiple tasks that will execute in parallel with a join configuration
    pub fn register_split<T>(
        mut self,
        state: TState,
        tasks: Vec<T>,
        join_config: JoinConfig<TState>,
    ) -> Self
    where
        T: Task<TState, TStore, DefaultTaskParams> + Send + Sync + 'static,
    {
        let arc_tasks: Vec<Arc<dyn Task<TState, TStore, DefaultTaskParams> + Send + Sync>> =
            tasks.into_iter().map(|t| Arc::new(t) as Arc<_>).collect();

        self.states.insert(
            state,
            StateEntry::Split {
                tasks: arc_tasks,
                join_config: Arc::new(join_config),
            },
        );
        self
    }

    /// Add exit state
    pub fn add_exit_state(mut self, state: TState) -> Self {
        self.exit_states.insert(state);
        self
    }

    /// Add multiple exit states
    pub fn add_exit_states(mut self, states: Vec<TState>) -> Self {
        self.exit_states.extend(states);
        self
    }

    /// Set a tracing span for this workflow (requires "tracing" feature)
    #[cfg(feature = "tracing")]
    pub fn with_tracing_span(mut self, span: Span) -> Self {
        self.tracing_span = Some(span);
        self
    }

    /// Validate the workflow configuration.
    ///
    /// Checks:
    /// - At least one exit state is defined
    /// - The workflow has at least one registered state handler
    ///
    /// Call this after building the workflow to catch configuration errors early.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError::Configuration`] if:
    /// - No state handlers have been registered
    /// - No exit states have been defined
    ///
    /// # Example
    ///
    /// ```rust
    /// use cano::prelude::*;
    ///
    /// # #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    /// # enum State { Start, Done }
    /// # #[derive(Clone)]
    /// # struct MyTask;
    /// # #[async_trait]
    /// # impl Task<State> for MyTask {
    /// #     async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<State>, CanoError> {
    /// #         Ok(TaskResult::Single(State::Done))
    /// #     }
    /// # }
    /// let workflow = Workflow::new(MemoryStore::new())
    ///     .register(State::Start, MyTask)
    ///     .add_exit_state(State::Done);
    ///
    /// assert!(workflow.validate().is_ok());
    /// ```
    pub fn validate(&self) -> Result<(), CanoError> {
        if self.states.is_empty() {
            return Err(CanoError::configuration(
                "Workflow has no registered state handlers",
            ));
        }
        if self.exit_states.is_empty() {
            return Err(CanoError::configuration(
                "Workflow has no exit states defined — orchestration may loop forever",
            ));
        }
        Ok(())
    }

    /// Validate that a specific state can be used as an initial state for orchestration.
    ///
    /// A valid initial state must either be registered as a handler or be an exit state.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError::Configuration`] if the given state is neither registered
    /// as a handler nor declared as an exit state.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cano::prelude::*;
    ///
    /// # #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    /// # enum State { Start, Done }
    /// # #[derive(Clone)]
    /// # struct MyTask;
    /// # #[async_trait]
    /// # impl Task<State> for MyTask {
    /// #     async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<State>, CanoError> {
    /// #         Ok(TaskResult::Single(State::Done))
    /// #     }
    /// # }
    /// let workflow = Workflow::new(MemoryStore::new())
    ///     .register(State::Start, MyTask)
    ///     .add_exit_state(State::Done);
    ///
    /// assert!(workflow.validate_initial_state(&State::Start).is_ok());
    /// assert!(workflow.validate_initial_state(&State::Done).is_ok());
    /// ```
    pub fn validate_initial_state(&self, state: &TState) -> Result<(), CanoError> {
        if !self.states.contains_key(state) && !self.exit_states.contains(state) {
            return Err(CanoError::configuration(format!(
                "Initial state {:?} is neither registered nor an exit state",
                state
            )));
        }
        Ok(())
    }

    /// Drive the workflow FSM from `initial_state` until an exit state is reached.
    ///
    /// The engine repeatedly executes the handler registered for the current state and
    /// transitions to the returned next state. Execution stops when the current state is
    /// found in the exit-states set.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] -- no handler is registered for the current state, a single
    ///   task returned a `TaskResult::Split` (use [`Workflow::register_split`] instead), the
    ///   global workflow timeout was exceeded, or a split strategy was misconfigured
    /// - Any [`CanoError`] variant propagated from a task or node during execution
    pub async fn orchestrate(&self, initial_state: TState) -> Result<TState, CanoError> {
        #[cfg(feature = "tracing")]
        let workflow_span = self
            .tracing_span
            .clone()
            .unwrap_or_else(|| info_span!("workflow_orchestrate"));

        #[cfg(feature = "tracing")]
        let _enter = workflow_span.enter();

        let workflow_future = self.execute_workflow(initial_state);

        if let Some(timeout_duration) = self.workflow_timeout {
            match tokio::time::timeout(timeout_duration, workflow_future).await {
                Ok(result) => result,
                Err(_) => Err(CanoError::workflow("Workflow timeout exceeded")),
            }
        } else {
            workflow_future.await
        }
    }

    async fn execute_workflow(&self, initial_state: TState) -> Result<TState, CanoError> {
        let mut current_state = initial_state;

        #[cfg(feature = "tracing")]
        info!(initial_state = ?current_state, "Starting workflow execution");

        loop {
            // Check if we've reached an exit state
            if self.exit_states.contains(&current_state) {
                #[cfg(feature = "tracing")]
                info!(final_state = ?current_state, "Workflow completed successfully");
                return Ok(current_state);
            }

            // Get the state entry
            let state_entry = self
                .states
                .get(&current_state)
                .ok_or_else(|| {
                    CanoError::workflow(format!(
                        "No task registered for state: {:?}",
                        current_state
                    ))
                })?
                .clone();

            #[cfg(feature = "tracing")]
            debug!(current_state = ?current_state, "Executing state");

            // Execute based on entry type
            current_state = match state_entry {
                StateEntry::Single { task } => self.execute_single_task(task).await?,
                StateEntry::Split { tasks, join_config } => {
                    self.execute_split_join(tasks, join_config).await?
                }
            };
        }
    }

    async fn execute_single_task(
        &self,
        task: Arc<dyn Task<TState, TStore, DefaultTaskParams> + Send + Sync>,
    ) -> Result<TState, CanoError> {
        #[cfg(feature = "tracing")]
        let task_span = info_span!("single_task_execution");

        #[cfg(feature = "tracing")]
        let result = {
            let _enter = task_span.enter();
            task.run(&*self.store).await
        };

        #[cfg(not(feature = "tracing"))]
        let result = task.run(&*self.store).await;

        match result? {
            TaskResult::Single(next_state) => {
                #[cfg(feature = "tracing")]
                debug!(next_state = ?next_state, "Single task completed");
                Ok(next_state)
            }
            TaskResult::Split(_) => Err(CanoError::workflow(
                "Single task returned split result - use register_split() for split tasks",
            )),
        }
    }

    async fn execute_split_join(
        &self,
        tasks: Vec<Arc<dyn Task<TState, TStore, DefaultTaskParams> + Send + Sync>>,
        join_config: Arc<JoinConfig<TState>>,
    ) -> Result<TState, CanoError> {
        let store = self.store.clone();
        let total_tasks = tasks.len();

        #[cfg(feature = "tracing")]
        info!(
            total_tasks = total_tasks,
            strategy = ?join_config.strategy,
            "Starting split execution"
        );

        // Validate PartialTimeout configuration before spawning tasks
        if matches!(join_config.strategy, JoinStrategy::PartialTimeout)
            && join_config.timeout.is_none()
        {
            return Err(CanoError::configuration(
                "PartialTimeout strategy requires a timeout to be configured",
            ));
        }

        // Spawn all parallel tasks
        let mut handles = Vec::new();
        #[cfg_attr(not(feature = "tracing"), allow(unused_variables))]
        for (idx, task) in tasks.into_iter().enumerate() {
            let task_clone = task.clone();
            let store_clone = store.clone();

            #[cfg(feature = "tracing")]
            let task_span = info_span!("split_task", task_id = idx);

            let handle = tokio::spawn(async move {
                #[cfg(feature = "tracing")]
                let _enter = task_span.enter();

                #[cfg(feature = "tracing")]
                debug!(task_id = idx, "Executing split task");

                let result = task_clone.run(&*store_clone).await;

                #[cfg(feature = "tracing")]
                match &result {
                    Ok(_) => debug!(task_id = idx, "Split task completed successfully"),
                    Err(e) => warn!(task_id = idx, error = %e, "Split task failed"),
                }

                result
            });
            handles.push(handle);
        }

        // Collect results using the unified strategy handler
        // This handles timeouts, cancellation, and strategy logic
        let split_result = self
            .collect_results(handles, &join_config, total_tasks)
            .await?;

        let successful = split_result.successes.len();
        let failed = split_result.errors.len();
        let cancelled = split_result.cancelled.len();

        #[cfg(feature = "tracing")]
        info!(
            successful = successful,
            failed = failed,
            cancelled = cancelled,
            total = total_tasks,
            "Split execution completed"
        );

        // Store partial results if requested
        if join_config.store_partial_results {
            // Store the split result for later access
            let summary = format!(
                "Split results: {} succeeded, {} failed, {} cancelled",
                successful, failed, cancelled
            );
            self.store.put("split_results_summary", summary)?;

            // Store individual results
            self.store.put("split_successes_count", successful)?;
            self.store.put("split_errors_count", failed)?;
            self.store.put("split_cancelled_count", cancelled)?;
        }

        // Check if join condition is met
        match &join_config.strategy {
            JoinStrategy::PartialResults(_) => {
                // For PartialResults, we always continue if minimum tasks completed successfully
                // We've already collected the required minimum
                if join_config.strategy.is_satisfied(successful, total_tasks) {
                    Ok(join_config.join_state.clone())
                } else {
                    Err(CanoError::workflow(format!(
                        "Partial results condition not met: {} completed successfully, {} required",
                        successful,
                        match &join_config.strategy {
                            JoinStrategy::PartialResults(min) => *min,
                            _ => 0,
                        }
                    )))
                }
            }
            JoinStrategy::PartialTimeout => {
                // For PartialTimeout, proceed with whatever completed before timeout
                if split_result.completed_count() >= 1 {
                    Ok(join_config.join_state.clone())
                } else {
                    Err(CanoError::workflow(
                        "PartialTimeout: No tasks completed before timeout",
                    ))
                }
            }
            _ => {
                // For other strategies, check successful tasks only
                if join_config.strategy.is_satisfied(successful, total_tasks) {
                    Ok(join_config.join_state.clone())
                } else {
                    Err(CanoError::workflow(format!(
                        "Join condition not met: {} of {} tasks completed successfully, strategy: {:?}",
                        successful, total_tasks, join_config.strategy
                    )))
                }
            }
        }
    }

    async fn collect_results(
        &self,
        handles: Vec<tokio::task::JoinHandle<Result<TaskResult<TState>, CanoError>>>,
        join_config: &JoinConfig<TState>,
        total_tasks: usize,
    ) -> Result<SplitResult<TState>, CanoError> {
        let mut split_result = SplitResult::new();
        let mut completed_indices = std::collections::HashSet::new();

        // Convert handles to futures with their indices, wrapped in AbortOnDrop for cancellation
        let mut futures = FuturesUnordered::new();
        for (idx, handle) in handles.into_iter().enumerate() {
            futures.push(async move {
                let handle = AbortOnDrop(handle);
                let result = handle.await;
                (idx, result)
            });
        }

        // Determine deadline if timeout is configured
        let deadline = join_config.timeout.map(|d| tokio::time::Instant::now() + d);

        loop {
            // Wait for next completion or timeout
            let next_result = if let Some(d) = deadline {
                match tokio::time::timeout_at(d, futures.next()).await {
                    Ok(res) => res,
                    Err(_) => {
                        // Timeout reached
                        if matches!(join_config.strategy, JoinStrategy::PartialTimeout) {
                            // For PartialTimeout, we stop and return what we have
                            break;
                        } else {
                            // For other strategies, timeout is an error
                            return Err(CanoError::workflow("Split task timeout exceeded"));
                        }
                    }
                }
            } else {
                futures.next().await
            };

            match next_result {
                Some((index, result)) => {
                    completed_indices.insert(index);

                    // Process the completed task
                    match result {
                        Ok(Ok(task_result)) => {
                            split_result.successes.push(SplitTaskResult {
                                task_index: index,
                                result: Ok(task_result),
                            });
                        }
                        Ok(Err(e)) => {
                            split_result.errors.push(SplitTaskResult {
                                task_index: index,
                                result: Err(e),
                            });
                        }
                        Err(e) => {
                            split_result.errors.push(SplitTaskResult {
                                task_index: index,
                                result: Err(CanoError::workflow(format!("Task panic: {:?}", e))),
                            });
                        }
                    }

                    // Check if we can return early based on strategy
                    match &join_config.strategy {
                        JoinStrategy::Any => {
                            if !split_result.successes.is_empty() {
                                break;
                            }
                        }
                        JoinStrategy::PartialResults(min) => {
                            if split_result.successes.len() >= *min {
                                break;
                            }
                        }
                        _ => {} // Continue for other strategies
                    }
                }
                None => break, // All tasks completed
            }
        }

        // Identify cancelled tasks (those that didn't complete)
        // When loop breaks, futures is dropped, and AbortOnDrop aborts remaining tasks
        for idx in 0..total_tasks {
            if !completed_indices.contains(&idx) {
                split_result.cancelled.push(idx);
            }
        }

        Ok(split_result)
    }
}

impl<TState, TStore> Clone for Workflow<TState, TStore>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TStore: KeyValueStore + 'static,
{
    fn clone(&self) -> Self {
        Self {
            states: self.states.clone(),
            store: self.store.clone(),
            workflow_timeout: self.workflow_timeout,
            exit_states: self.exit_states.clone(),
            #[cfg(feature = "tracing")]
            tracing_span: self.tracing_span.clone(),
        }
    }
}

impl<TState, TStore> std::fmt::Debug for Workflow<TState, TStore>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TStore: KeyValueStore + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workflow")
            .field("states", &format!("{} states", self.states.len()))
            .field("exit_states", &self.exit_states)
            .field("workflow_timeout", &self.workflow_timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::KeyValueStore;
    use crate::task::Task;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio;

    // Test workflow states
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum TestState {
        Start,
        Process,
        Split,
        Join,
        Complete,
        #[allow(dead_code)]
        Error,
    }

    // Simple task that returns a single state
    #[derive(Clone)]
    struct SimpleTask {
        next_state: TestState,
        counter: Arc<AtomicU32>,
    }

    impl SimpleTask {
        fn new(next_state: TestState) -> Self {
            Self {
                next_state,
                counter: Arc::new(AtomicU32::new(0)),
            }
        }

        #[allow(dead_code)]
        fn count(&self) -> u32 {
            self.counter.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Task<TestState> for SimpleTask {
        async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(TaskResult::Single(self.next_state.clone()))
        }
    }

    // Task that stores data
    #[derive(Clone)]
    struct DataTask {
        key: String,
        value: String,
        next_state: TestState,
    }

    impl DataTask {
        fn new(key: &str, value: &str, next_state: TestState) -> Self {
            Self {
                key: key.to_string(),
                value: value.to_string(),
                next_state,
            }
        }
    }

    #[async_trait]
    impl Task<TestState> for DataTask {
        async fn run(&self, store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
            store.put(&self.key, self.value.clone())?;
            Ok(TaskResult::Single(self.next_state.clone()))
        }
    }

    // Task that fails
    #[derive(Clone)]
    struct FailTask {
        should_fail: bool,
    }

    impl FailTask {
        fn new(should_fail: bool) -> Self {
            Self { should_fail }
        }
    }

    #[async_trait]
    impl Task<TestState> for FailTask {
        async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
            if self.should_fail {
                Err(CanoError::task_execution("Task intentionally failed"))
            } else {
                Ok(TaskResult::Single(TestState::Complete))
            }
        }
    }

    #[tokio::test]
    async fn test_workflow_creation() {
        let store = MemoryStore::new();
        let workflow = Workflow::<TestState>::new(store);

        assert_eq!(workflow.states.len(), 0);
        assert_eq!(workflow.exit_states.len(), 0);
    }

    #[tokio::test]
    async fn test_simple_workflow() {
        let store = MemoryStore::new();
        let workflow = Workflow::new(store)
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_multi_step_workflow() {
        let store = MemoryStore::new();
        let workflow = Workflow::new(store)
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_workflow_with_data() {
        let store = MemoryStore::new();
        let workflow = Workflow::new(store.clone())
            .register(
                TestState::Start,
                DataTask::new("test_key", "test_value", TestState::Complete),
            )
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        let data: String = store.get("test_key").unwrap();
        assert_eq!(data, "test_value");
    }

    #[tokio::test]
    async fn test_split_all_strategy() {
        let store = MemoryStore::new();

        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_any_strategy() {
        let store = MemoryStore::new();

        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Any, TestState::Complete);

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_quorum_strategy() {
        let store = MemoryStore::new();

        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Quorum(3), TestState::Complete);

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_percentage_strategy() {
        let store = MemoryStore::new();

        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        // 75% of 4 tasks = 3 tasks
        let join_config = JoinConfig::new(JoinStrategy::Percentage(0.75), TestState::Complete);

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_with_failures_all_strategy() {
        let store = MemoryStore::new();

        let tasks = vec![
            FailTask::new(false),
            FailTask::new(true), // This will fail
            FailTask::new(false),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_split_with_failures_quorum_strategy() {
        let store = MemoryStore::new();

        let tasks = vec![
            FailTask::new(false),
            FailTask::new(false),
            FailTask::new(true), // This will fail
        ];

        // Quorum of 2, so should succeed despite one failure
        let join_config = JoinConfig::new(JoinStrategy::Quorum(2), TestState::Complete);

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_with_timeout() {
        let store = MemoryStore::new();

        // Task that sleeps longer than timeout
        #[derive(Clone)]
        struct SlowTask;

        #[async_trait]
        impl Task<TestState> for SlowTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SlowTask, SlowTask];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete)
            .with_timeout(Duration::from_millis(50));

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_workflow_timeout() {
        let store = MemoryStore::new();

        // Task that sleeps longer than workflow timeout
        #[derive(Clone)]
        struct SlowTask;

        #[async_trait]
        impl Task<TestState> for SlowTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let workflow = Workflow::new(store)
            .with_timeout(Duration::from_millis(50))
            .register(TestState::Start, SlowTask)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Workflow timeout"));
    }

    #[tokio::test]
    async fn test_split_with_data_sharing() {
        let store = MemoryStore::new();

        let tasks = vec![
            DataTask::new("task1", "value1", TestState::Join),
            DataTask::new("task2", "value2", TestState::Join),
            DataTask::new("task3", "value3", TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::new(store.clone())
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify all tasks wrote their data
        let data1: String = store.get("task1").unwrap();
        let data2: String = store.get("task2").unwrap();
        let data3: String = store.get("task3").unwrap();

        assert_eq!(data1, "value1");
        assert_eq!(data2, "value2");
        assert_eq!(data3, "value3");
    }

    #[tokio::test]
    async fn test_complex_workflow_with_split_join() {
        let store = MemoryStore::new();

        let split_tasks = vec![
            DataTask::new("parallel1", "data1", TestState::Join),
            DataTask::new("parallel2", "data2", TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Process);

        let workflow = Workflow::new(store.clone())
            .register(
                TestState::Start,
                DataTask::new("init", "initialized", TestState::Split),
            )
            .register_split(TestState::Split, split_tasks, join_config)
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify all data was written
        let init: String = store.get("init").unwrap();
        let parallel1: String = store.get("parallel1").unwrap();
        let parallel2: String = store.get("parallel2").unwrap();

        assert_eq!(init, "initialized");
        assert_eq!(parallel1, "data1");
        assert_eq!(parallel2, "data2");
    }

    #[tokio::test]
    async fn test_join_strategy_is_satisfied() {
        assert!(JoinStrategy::All.is_satisfied(3, 3));
        assert!(!JoinStrategy::All.is_satisfied(2, 3));

        assert!(JoinStrategy::Any.is_satisfied(1, 3));
        assert!(!JoinStrategy::Any.is_satisfied(0, 3));

        assert!(JoinStrategy::Quorum(2).is_satisfied(2, 3));
        assert!(JoinStrategy::Quorum(2).is_satisfied(3, 3));
        assert!(!JoinStrategy::Quorum(2).is_satisfied(1, 3));

        assert!(JoinStrategy::Percentage(0.5).is_satisfied(2, 4));
        assert!(JoinStrategy::Percentage(0.75).is_satisfied(3, 4));
        assert!(!JoinStrategy::Percentage(0.75).is_satisfied(2, 4));

        assert!(JoinStrategy::PartialResults(2).is_satisfied(2, 4));
        assert!(JoinStrategy::PartialResults(2).is_satisfied(3, 4));
        assert!(!JoinStrategy::PartialResults(2).is_satisfied(1, 4));

        assert!(JoinStrategy::PartialTimeout.is_satisfied(1, 4));
        assert!(JoinStrategy::PartialTimeout.is_satisfied(3, 4));
        assert!(!JoinStrategy::PartialTimeout.is_satisfied(0, 4));
    }

    #[tokio::test]
    async fn test_partial_results_strategy() {
        let store = MemoryStore::new();

        // Create tasks with varying delays - some will be cancelled
        #[derive(Clone)]
        struct DelayedTask {
            delay_ms: u64,
            #[allow(dead_code)]
            task_id: usize,
        }

        #[async_trait]
        impl Task<TestState> for DelayedTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![
            DelayedTask {
                delay_ms: 50,
                task_id: 1,
            },
            DelayedTask {
                delay_ms: 100,
                task_id: 2,
            },
            DelayedTask {
                delay_ms: 500,
                task_id: 3,
            }, // This should be cancelled
            DelayedTask {
                delay_ms: 600,
                task_id: 4,
            }, // This should be cancelled
        ];

        // Wait for 2 tasks to complete, then cancel the rest
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), TestState::Complete)
            .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Check stored results
        let successes: usize = store.get("split_successes_count").unwrap();
        let cancelled: usize = store.get("split_cancelled_count").unwrap();

        assert_eq!(successes, 2);
        assert_eq!(cancelled, 2);
    }

    #[tokio::test]
    async fn test_partial_results_with_failures() {
        let store = MemoryStore::new();

        // Mix of fast success, fast failure, and slow tasks
        #[derive(Clone)]
        struct MixedTask {
            delay_ms: u64,
            should_fail: bool,
        }

        #[async_trait]
        impl Task<TestState> for MixedTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                if self.should_fail {
                    Err(CanoError::task_execution("Task failed"))
                } else {
                    Ok(TaskResult::Single(TestState::Complete))
                }
            }
        }

        let tasks = vec![
            MixedTask {
                delay_ms: 50,
                should_fail: false,
            }, // Success
            MixedTask {
                delay_ms: 100,
                should_fail: true,
            }, // Failure
            MixedTask {
                delay_ms: 500,
                should_fail: false,
            }, // Should be cancelled
            MixedTask {
                delay_ms: 600,
                should_fail: false,
            }, // Should be cancelled
        ];

        // Wait for 2 tasks to complete (success or failure), then cancel rest
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), TestState::Complete)
            .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Check stored results
        let successes: usize = store.get("split_successes_count").unwrap();
        let errors: usize = store.get("split_errors_count").unwrap();
        let cancelled: usize = store.get("split_cancelled_count").unwrap();

        // Should have 2 successes (one fast, one slow), 1 error, and 1 cancelled
        assert_eq!(successes, 2);
        assert_eq!(errors, 1);
        assert_eq!(cancelled, 1);
    }

    #[tokio::test]
    async fn test_partial_results_minimum_not_met() {
        let store = MemoryStore::new();

        // All tasks will timeout before minimum is reached
        #[derive(Clone)]
        struct SlowTask;

        #[async_trait]
        impl Task<TestState> for SlowTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SlowTask, SlowTask, SlowTask];

        // Require 3 tasks but timeout after 100ms
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(3), TestState::Complete)
            .with_timeout(Duration::from_millis(100));

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_partial_timeout_strategy() {
        let store = MemoryStore::new();

        // Create tasks with varying delays
        #[derive(Clone)]
        struct DelayedTask {
            delay_ms: u64,
            #[allow(dead_code)]
            task_id: usize,
        }

        #[async_trait]
        impl Task<TestState> for DelayedTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![
            DelayedTask {
                delay_ms: 50,
                task_id: 1,
            },
            DelayedTask {
                delay_ms: 100,
                task_id: 2,
            },
            DelayedTask {
                delay_ms: 500,
                task_id: 3,
            }, // Won't complete in time
            DelayedTask {
                delay_ms: 600,
                task_id: 4,
            }, // Won't complete in time
        ];

        // Timeout after 200ms - should get 2 completions
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete)
            .with_timeout(Duration::from_millis(200))
            .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Check stored results
        let successes: usize = store.get("split_successes_count").unwrap();
        let cancelled: usize = store.get("split_cancelled_count").unwrap();

        assert_eq!(successes, 2);
        assert_eq!(cancelled, 2);
    }

    #[tokio::test]
    async fn test_partial_timeout_with_failures() {
        let store = MemoryStore::new();

        // Mix of fast success, fast failure, and slow tasks
        #[derive(Clone)]
        struct MixedTask {
            delay_ms: u64,
            should_fail: bool,
        }

        #[async_trait]
        impl Task<TestState> for MixedTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                if self.should_fail {
                    Err(CanoError::task_execution("Task failed"))
                } else {
                    Ok(TaskResult::Single(TestState::Complete))
                }
            }
        }

        let tasks = vec![
            MixedTask {
                delay_ms: 50,
                should_fail: false,
            }, // Success
            MixedTask {
                delay_ms: 100,
                should_fail: true,
            }, // Failure
            MixedTask {
                delay_ms: 150,
                should_fail: false,
            }, // Success
            MixedTask {
                delay_ms: 500,
                should_fail: false,
            }, // Won't complete
        ];

        // Timeout after 200ms
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete)
            .with_timeout(Duration::from_millis(200))
            .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Check stored results
        let successes: usize = store.get("split_successes_count").unwrap();
        let errors: usize = store.get("split_errors_count").unwrap();
        let cancelled: usize = store.get("split_cancelled_count").unwrap();

        assert_eq!(successes, 2);
        assert_eq!(errors, 1);
        assert_eq!(cancelled, 1);
    }

    #[tokio::test]
    async fn test_partial_timeout_all_complete() {
        let store = MemoryStore::new();

        // All tasks complete before timeout
        #[derive(Clone)]
        struct FastTask {
            delay_ms: u64,
        }

        #[async_trait]
        impl Task<TestState> for FastTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![
            FastTask { delay_ms: 20 },
            FastTask { delay_ms: 30 },
            FastTask { delay_ms: 40 },
        ];

        // Generous timeout
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete)
            .with_timeout(Duration::from_millis(500))
            .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // All should complete
        let successes: usize = store.get("split_successes_count").unwrap();
        let cancelled: usize = store.get("split_cancelled_count").unwrap();

        assert_eq!(successes, 3);
        assert_eq!(cancelled, 0);
    }

    #[tokio::test]
    async fn test_partial_timeout_no_timeout_configured() {
        let store = MemoryStore::new();

        #[derive(Clone)]
        struct SimpleTask;

        #[async_trait]
        impl Task<TestState> for SimpleTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SimpleTask, SimpleTask];

        // PartialTimeout without timeout should fail
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete);

        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("requires a timeout")
        );
    }

    #[tokio::test]
    async fn test_unregistered_state_error() {
        let store = MemoryStore::new();
        let workflow = Workflow::<TestState>::new(store).add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No task registered")
        );
    }

    // --- Validation tests ---

    #[test]
    fn test_validate_empty_workflow() {
        let workflow = Workflow::<TestState>::new(MemoryStore::new());
        let result = workflow.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no registered state handlers")
        );
    }

    #[test]
    fn test_validate_no_exit_states() {
        let workflow = Workflow::new(MemoryStore::new())
            .register(TestState::Start, SimpleTask::new(TestState::Complete));
        let result = workflow.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no exit states defined")
        );
    }

    #[test]
    fn test_validate_valid_workflow() {
        let workflow = Workflow::new(MemoryStore::new())
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);
        assert!(workflow.validate().is_ok());
    }

    #[test]
    fn test_validate_initial_state() {
        let workflow = Workflow::new(MemoryStore::new())
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        // Registered handler is a valid initial state
        assert!(workflow.validate_initial_state(&TestState::Start).is_ok());

        // Exit state is also a valid initial state
        assert!(
            workflow
                .validate_initial_state(&TestState::Complete)
                .is_ok()
        );

        // Unregistered, non-exit state is invalid
        let result = workflow.validate_initial_state(&TestState::Process);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("neither registered nor an exit state")
        );
    }

    // ── Edge case coverage tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_empty_split_task_list() {
        let store = MemoryStore::new();
        let tasks: Vec<SimpleTask> = vec![];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_percentage_zero() {
        let store = MemoryStore::new();
        let tasks = vec![FailTask::new(true), FailTask::new(true)];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(0.0), TestState::Complete);
        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_percentage_one() {
        let store = MemoryStore::new();
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(1.0), TestState::Complete);
        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );

        let store2 = MemoryStore::new();
        let tasks_fail = vec![FailTask::new(false), FailTask::new(true)];
        let join_config2 = JoinConfig::new(JoinStrategy::Percentage(1.0), TestState::Complete);
        let workflow2 = Workflow::new(store2)
            .register_split(TestState::Start, tasks_fail, join_config2)
            .add_exit_state(TestState::Complete);
        assert!(workflow2.orchestrate(TestState::Start).await.is_err());
    }

    #[tokio::test]
    async fn test_percentage_over_one() {
        let store = MemoryStore::new();
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(1.5), TestState::Complete);
        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        assert!(workflow.orchestrate(TestState::Start).await.is_err());
    }

    #[tokio::test]
    async fn test_quorum_zero() {
        let store = MemoryStore::new();
        let tasks = vec![FailTask::new(true), FailTask::new(true)];
        let join_config = JoinConfig::new(JoinStrategy::Quorum(0), TestState::Complete);
        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_single_task_register_split() {
        let store = MemoryStore::new();
        let tasks = vec![SimpleTask::new(TestState::Complete)];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::new(store)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_workflow_no_exit_states() {
        let store = MemoryStore::new();
        let workflow =
            Workflow::new(store).register(TestState::Start, SimpleTask::new(TestState::Complete));
        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No task registered")
        );
    }

    #[tokio::test]
    async fn test_split_task_from_single_register() {
        #[derive(Clone)]
        struct SplitReturningTask;

        #[async_trait]
        impl Task<TestState> for SplitReturningTask {
            async fn run(&self, _store: &MemoryStore) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Split(vec![TestState::Complete]))
            }
        }

        let store = MemoryStore::new();
        let workflow = Workflow::new(store)
            .register(TestState::Start, SplitReturningTask)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("register_split"));
    }
}
