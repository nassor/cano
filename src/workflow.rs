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
//! - **PartialResults**: Accept partial results after minimum tasks complete (success or failure),
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
//! - **Early Cancellation**: Once the minimum number of tasks complete (success or failure),
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
use tracing::{Span, debug, error, info, info_span, warn};

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
    /// Accept partial results - continues after minimum tasks complete,
    /// cancels remaining tasks, and returns both successes and errors
    /// Parameter is the minimum number of tasks that must complete (success or failure)
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

    /// Enable storing partial results in the store (default: false)
    /// Results will be stored under the key "split_results"
    pub fn with_store_partial_results(mut self, store: bool) -> Self {
        self.store_partial_results = store;
        self
    }
}

/// Entry in the workflow state machine
pub enum StateEntry<TState>
where
    TState: Clone + Send + Sync + 'static,
{
    /// Single task execution
    Single {
        task: Arc<dyn Task<TState, MemoryStore, DefaultTaskParams> + Send + Sync>,
    },
    /// Split into parallel tasks with join configuration
    Split {
        tasks: Vec<Arc<dyn Task<TState, MemoryStore, DefaultTaskParams> + Send + Sync>>,
        join_config: Arc<JoinConfig<TState>>,
    },
}

impl<TState> Clone for StateEntry<TState>
where
    TState: Clone + Send + Sync + 'static,
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
pub struct Workflow<TState>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
{
    /// State machine with support for split/join
    states: HashMap<TState, StateEntry<TState>>,
    /// Shared store for all tasks
    store: Arc<MemoryStore>,
    /// Global workflow timeout
    workflow_timeout: Option<Duration>,
    /// Exit states that terminate workflow
    exit_states: std::collections::HashSet<TState>,
    /// Optional tracing span
    #[cfg(feature = "tracing")]
    tracing_span: Option<Span>,
}

impl<TState> Workflow<TState>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
{
    /// Create a new workflow with the given store
    pub fn new(store: MemoryStore) -> Self {
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

    /// Register a single task for a state
    pub fn register<T>(mut self, state: TState, task: T) -> Self
    where
        T: Task<TState, MemoryStore, DefaultTaskParams> + Send + Sync + 'static,
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
        T: Task<TState, MemoryStore, DefaultTaskParams> + Send + Sync + 'static,
    {
        let arc_tasks: Vec<Arc<dyn Task<TState, MemoryStore, DefaultTaskParams> + Send + Sync>> =
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

    /// Orchestrate workflow execution with split/join support
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
                StateEntry::Single { task } => {
                    self.execute_single_task(task, current_state.clone())
                        .await?
                }
                StateEntry::Split { tasks, join_config } => {
                    self.execute_split_join(tasks, join_config, current_state.clone())
                        .await?
                }
            };
        }
    }

    async fn execute_single_task(
        &self,
        task: Arc<dyn Task<TState, MemoryStore, DefaultTaskParams> + Send + Sync>,
        _state: TState,
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
        tasks: Vec<Arc<dyn Task<TState, MemoryStore, DefaultTaskParams> + Send + Sync>>,
        join_config: Arc<JoinConfig<TState>>,
        _state: TState,
    ) -> Result<TState, CanoError> {
        let store = self.store.clone();
        let total_tasks = tasks.len();

        #[cfg(feature = "tracing")]
        info!(
            total_tasks = total_tasks,
            strategy = ?join_config.strategy,
            "Starting split execution"
        );

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

        // Apply timeout if configured or required by strategy
        let split_result = if matches!(join_config.strategy, JoinStrategy::PartialTimeout) {
            // PartialTimeout requires a timeout
            let timeout_duration = join_config.timeout.ok_or_else(|| {
                CanoError::configuration(
                    "PartialTimeout strategy requires a timeout to be configured",
                )
            })?;

            self.collect_results_with_timeout(handles, timeout_duration, total_tasks)
                .await?
        } else if let Some(timeout_duration) = join_config.timeout {
            // Other strategies with timeout
            let results_future = self.collect_results(handles, &join_config.strategy, total_tasks);
            match tokio::time::timeout(timeout_duration, results_future).await {
                Ok(results) => results?,
                Err(_) => {
                    #[cfg(feature = "tracing")]
                    error!("Split task timeout exceeded");
                    return Err(CanoError::workflow("Split task timeout exceeded"));
                }
            }
        } else {
            // No timeout
            self.collect_results(handles, &join_config.strategy, total_tasks)
                .await?
        };

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
                // For PartialResults, we always continue if minimum tasks completed
                // We've already collected the required minimum
                if join_config
                    .strategy
                    .is_satisfied(split_result.completed_count(), total_tasks)
                {
                    Ok(join_config.join_state.clone())
                } else {
                    Err(CanoError::workflow(format!(
                        "Partial results condition not met: {} completed, {} required",
                        split_result.completed_count(),
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
        mut handles: Vec<tokio::task::JoinHandle<Result<TaskResult<TState>, CanoError>>>,
        strategy: &JoinStrategy,
        total_tasks: usize,
    ) -> Result<SplitResult<TState>, CanoError> {
        match strategy {
            JoinStrategy::Any => {
                // Use select_all to return as soon as any task completes successfully
                let (result, index, remaining) = futures::future::select_all(handles).await;

                let mut split_result = SplitResult::new();

                // Cancel remaining tasks
                for (idx, handle) in remaining.into_iter().enumerate() {
                    handle.abort();
                    // Calculate actual index (accounting for the completed task)
                    let actual_idx = if idx < index { idx } else { idx + 1 };
                    split_result.cancelled.push(actual_idx);
                }

                // Process the result
                match result {
                    Ok(task_result) => {
                        if task_result.is_ok() {
                            split_result.successes.push(SplitTaskResult {
                                task_index: index,
                                result: task_result,
                            });
                            Ok(split_result)
                        } else {
                            split_result.errors.push(SplitTaskResult {
                                task_index: index,
                                result: task_result,
                            });
                            Err(CanoError::workflow("First completed task failed"))
                        }
                    }
                    Err(e) => {
                        split_result.errors.push(SplitTaskResult {
                            task_index: index,
                            result: Err(CanoError::workflow(format!("Task panic: {:?}", e))),
                        });
                        Err(CanoError::workflow("Task join error"))
                    }
                }
            }
            JoinStrategy::PartialResults(min_tasks) => {
                // Poll tasks until minimum number complete, then cancel rest
                let mut split_result = SplitResult::new();
                let mut completed_indices = std::collections::HashSet::new();

                while split_result.completed_count() < *min_tasks && !handles.is_empty() {
                    let (result, index, remaining) = futures::future::select_all(handles).await;

                    completed_indices.insert(index);

                    // Process the completed task
                    match result {
                        Ok(task_result) => {
                            if task_result.is_ok() {
                                split_result.successes.push(SplitTaskResult {
                                    task_index: index,
                                    result: task_result,
                                });
                            } else {
                                split_result.errors.push(SplitTaskResult {
                                    task_index: index,
                                    result: task_result,
                                });
                            }
                        }
                        Err(e) => {
                            split_result.errors.push(SplitTaskResult {
                                task_index: index,
                                result: Err(CanoError::workflow(format!("Task panic: {:?}", e))),
                            });
                        }
                    }

                    handles = remaining;
                }

                // Cancel all remaining tasks
                for handle in handles {
                    handle.abort();
                }

                // Mark cancelled tasks
                for idx in 0..total_tasks {
                    if !completed_indices.contains(&idx) {
                        split_result.cancelled.push(idx);
                    }
                }

                Ok(split_result)
            }
            JoinStrategy::PartialTimeout => {
                // Wait for all tasks to complete or be cancelled by outer timeout
                // This strategy relies on the timeout wrapper in execute_split_join
                // When timeout occurs, tokio will drop the future and we won't reach here
                // This branch handles the case where all tasks complete before timeout
                let results = futures::future::join_all(handles).await;
                let mut split_result = SplitResult::new();

                for (idx, result) in results.into_iter().enumerate() {
                    match result {
                        Ok(task_result) => {
                            if task_result.is_ok() {
                                split_result.successes.push(SplitTaskResult {
                                    task_index: idx,
                                    result: task_result,
                                });
                            } else {
                                split_result.errors.push(SplitTaskResult {
                                    task_index: idx,
                                    result: task_result,
                                });
                            }
                        }
                        Err(e) => {
                            split_result.errors.push(SplitTaskResult {
                                task_index: idx,
                                result: Err(CanoError::workflow(format!("Task panic: {:?}", e))),
                            });
                        }
                    }
                }

                Ok(split_result)
            }
            _ => {
                // Wait for all tasks and collect results
                let results = futures::future::join_all(handles).await;
                let mut split_result = SplitResult::new();

                for (idx, result) in results.into_iter().enumerate() {
                    match result {
                        Ok(task_result) => {
                            if task_result.is_ok() {
                                split_result.successes.push(SplitTaskResult {
                                    task_index: idx,
                                    result: task_result,
                                });
                            } else {
                                split_result.errors.push(SplitTaskResult {
                                    task_index: idx,
                                    result: task_result,
                                });
                            }
                        }
                        Err(e) => {
                            split_result.errors.push(SplitTaskResult {
                                task_index: idx,
                                result: Err(CanoError::workflow(format!("Task panic: {:?}", e))),
                            });
                        }
                    }
                }

                Ok(split_result)
            }
        }
    }

    async fn collect_results_with_timeout(
        &self,
        handles: Vec<tokio::task::JoinHandle<Result<TaskResult<TState>, CanoError>>>,
        timeout: Duration,
        total_tasks: usize,
    ) -> Result<SplitResult<TState>, CanoError> {
        use futures::stream::StreamExt;
        use tokio::time::sleep;

        let mut split_result = SplitResult::new();
        let mut completed_indices = std::collections::HashSet::new();

        // Convert handles to futures with their indices
        let mut futures = futures::stream::FuturesUnordered::new();
        for (idx, handle) in handles.into_iter().enumerate() {
            futures.push(async move {
                let result = handle.await;
                (idx, result)
            });
        }

        // Set up timeout
        let deadline = tokio::time::Instant::now() + timeout;

        while !futures.is_empty() {
            let remaining_time = deadline.saturating_duration_since(tokio::time::Instant::now());

            if remaining_time.is_zero() {
                // Timeout reached - all remaining tasks are considered cancelled
                #[cfg(feature = "tracing")]
                debug!(
                    "PartialTimeout: timeout reached, {} tasks incomplete",
                    futures.len()
                );

                // Mark remaining tasks as cancelled
                for idx in 0..total_tasks {
                    if !completed_indices.contains(&idx) {
                        split_result.cancelled.push(idx);
                    }
                }
                break;
            }

            // Race between timeout and next task completion
            tokio::select! {
                _ = sleep(remaining_time) => {
                    // Timeout reached
                    #[cfg(feature = "tracing")]
                    debug!("PartialTimeout: timeout reached during select");

                    for idx in 0..total_tasks {
                        if !completed_indices.contains(&idx) {
                            split_result.cancelled.push(idx);
                        }
                    }
                    break;
                }
                Some((task_idx, task_result)) = futures.next() => {
                    completed_indices.insert(task_idx);

                    // Process the completed task
                    match task_result {
                        Ok(Ok(task_result)) => {
                            split_result.successes.push(SplitTaskResult {
                                task_index: task_idx,
                                result: Ok(task_result),
                            });
                        }
                        Ok(Err(e)) => {
                            split_result.errors.push(SplitTaskResult {
                                task_index: task_idx,
                                result: Err(e),
                            });
                        }
                        Err(e) => {
                            split_result.errors.push(SplitTaskResult {
                                task_index: task_idx,
                                result: Err(CanoError::workflow(format!("Task panic: {:?}", e))),
                            });
                        }
                    }
                }
            }
        }

        Ok(split_result)
    }
}

impl<TState> Clone for Workflow<TState>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
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

impl<TState> std::fmt::Debug for Workflow<TState>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
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

        // Should have 1 success, 1 error, and 2 cancelled
        assert_eq!(successes, 1);
        assert_eq!(errors, 1);
        assert_eq!(cancelled, 2);
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
}
