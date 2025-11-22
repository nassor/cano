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
//! - **All**: Wait for all tasks to complete
//! - **Any**: Continue after any single task completes
//! - **Quorum**: Wait for a specific number of tasks
//! - **Percentage**: Wait for a percentage of tasks to complete

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::error::CanoError;
use crate::store::MemoryStore;
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
        }
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
        }
    }

    /// Set timeout for the split execution
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
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

        // Apply timeout if configured
        let results_future = self.collect_results(handles, &join_config.strategy, total_tasks);

        let results = if let Some(timeout_duration) = join_config.timeout {
            match tokio::time::timeout(timeout_duration, results_future).await {
                Ok(results) => results?,
                Err(_) => {
                    #[cfg(feature = "tracing")]
                    error!("Split task timeout exceeded");
                    return Err(CanoError::workflow("Split task timeout exceeded"));
                }
            }
        } else {
            results_future.await?
        };

        // Check if join condition is met
        let successful = results.iter().filter(|r| r.is_ok()).count();

        #[cfg(feature = "tracing")]
        info!(
            successful = successful,
            total = total_tasks,
            "Split execution completed"
        );

        if join_config.strategy.is_satisfied(successful, total_tasks) {
            Ok(join_config.join_state.clone())
        } else {
            Err(CanoError::workflow(format!(
                "Join condition not met: {} of {} tasks completed, strategy: {:?}",
                successful, total_tasks, join_config.strategy
            )))
        }
    }

    async fn collect_results(
        &self,
        handles: Vec<tokio::task::JoinHandle<Result<TaskResult<TState>, CanoError>>>,
        strategy: &JoinStrategy,
        _total_tasks: usize,
    ) -> Result<Vec<Result<TaskResult<TState>, CanoError>>, CanoError> {
        match strategy {
            JoinStrategy::Any => {
                // Use select_all to return as soon as any task completes successfully
                let (result, _index, remaining) = futures::future::select_all(handles).await;

                // Cancel remaining tasks
                for handle in remaining {
                    handle.abort();
                }

                // Process the result
                match result {
                    Ok(task_result) => {
                        if task_result.is_ok() {
                            Ok(vec![task_result])
                        } else {
                            Err(CanoError::workflow("First completed task failed"))
                        }
                    }
                    Err(_) => Err(CanoError::workflow("Task join error")),
                }
            }
            _ => {
                // Wait for all tasks and collect results
                let results = futures::future::join_all(handles).await;
                Ok(results
                    .into_iter()
                    .map(|r| {
                        r.unwrap_or_else(|e| {
                            Err(CanoError::workflow(format!("Task panic: {:?}", e)))
                        })
                    })
                    .collect())
            }
        }
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
