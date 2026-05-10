//! # Workflow API - Build Workflows with Split/Join Support
//!
//! This module provides the core [`Workflow`] type for building async workflow systems
//! with support for parallel task execution through split/join patterns.
//!
//! ## Core Concepts
//!
//! ### Split/Join Workflows
//!
//! The [`Workflow`] API supports splitting into multiple parallel tasks:
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
//! ## Partial Results Strategy
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
//! # #[task]
//! # impl Task<State> for MyTask {
//! #     async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> {
//! #         Ok(TaskResult::Single(State::Complete))
//! #     }
//! # }
//! # async fn example() -> Result<(), CanoError> {
//! let tasks = vec![MyTask, MyTask, MyTask, MyTask];
//!
//! // Wait for 2 tasks to complete, then cancel the rest
//! let join_config = JoinConfig::new(
//!     JoinStrategy::PartialResults(2),
//!     State::Process
//! );
//!
//! let workflow = Workflow::bare()
//!     .register_split(State::Start, tasks, join_config)
//!     .add_exit_state(State::Complete);
//! # Ok(())
//! # }
//! ```

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::FutureExt;

use crate::error::CanoError;
use crate::observer::WorkflowObserver;
use crate::resource::Resources;
use crate::task::{Task, TaskResult};

/// Invoke `f` for each observer in `observers`. Used at every workflow event site;
/// a no-op when the list is empty (the common case — observers are opt-in).
#[inline]
fn notify_observers(observers: &[Arc<dyn WorkflowObserver>], f: impl Fn(&dyn WorkflowObserver)) {
    for observer in observers {
        f(observer.as_ref());
    }
}

/// Best-effort extraction of a human-readable message from a panic payload.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn split_error_summary<TState>(errors: &[SplitTaskResult<TState>]) -> String {
    const MAX_ERRORS_TO_REPORT: usize = 3;

    let mut parts: Vec<String> = errors
        .iter()
        .take(MAX_ERRORS_TO_REPORT)
        .map(|err| match &err.result {
            Ok(_) => format!("task {}: unexpected success in error list", err.task_index),
            Err(e) => format!("task {}: {}", err.task_index, e),
        })
        .collect();

    if errors.len() > MAX_ERRORS_TO_REPORT {
        parts.push(format!(
            "... and {} more error(s)",
            errors.len() - MAX_ERRORS_TO_REPORT
        ));
    }

    parts.join("; ")
}

#[cfg(feature = "tracing")]
use tracing::{Span, debug, info, info_span, warn};

/// Strategy for joining parallel tasks
#[derive(Clone, Debug, PartialEq)]
pub enum JoinStrategy {
    /// All tasks must complete successfully
    All,
    /// Any single task completion triggers join
    Any,
    /// Specific number of tasks must complete
    Quorum(usize),
    /// A fraction of tasks must complete successfully.
    ///
    /// The value must be in the range `(0.0, 1.0]`. A value of `1.0` means all tasks
    /// must succeed (equivalent to [`All`](JoinStrategy::All)). Values outside this
    /// range return [`CanoError::Configuration`] when the split executes.
    Percentage(f64),
    /// Accept partial results - continues after minimum tasks complete successfully,
    /// cancels remaining tasks, and returns both successes and errors
    /// Parameter is the minimum number of tasks that must complete successfully
    PartialResults(usize),
    /// Accept whatever completes before the deadline, then cancel the rest.
    ///
    /// The join succeeds as long as at least one task (success **or** failure) finished
    /// before the timeout; the workflow continues with [`JoinConfig::join_state`].
    /// If zero tasks complete before the deadline, the workflow errors with
    /// [`CanoError::Workflow`].
    ///
    /// Unlike [`PartialResults`](JoinStrategy::PartialResults), which waits for a
    /// minimum number of *successful* completions, `PartialTimeout` is purely
    /// time-bounded and accepts any mixture of successes and failures.
    ///
    /// **Requires** a timeout to be set via [`JoinConfig::with_timeout`]; configuring
    /// this strategy without a timeout returns [`CanoError::Configuration`] at runtime.
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
                // Percentage must be in (0.0, 1.0] — validated at execute_split_join entry.
                // Saturate to usize::MAX rather than wrap; a task count large enough to
                // overflow f64→usize would OOM first anyway.
                let required_f = (total as f64 * p).ceil();
                let required = if required_f >= usize::MAX as f64 {
                    usize::MAX
                } else {
                    required_f as usize
                };
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

    /// Create a split result with capacity for `total_tasks` outcomes preallocated.
    /// Used internally by `collect_results` to avoid Vec resizes during collection.
    pub fn with_capacity(total_tasks: usize) -> Self {
        Self {
            successes: Vec::with_capacity(total_tasks),
            errors: Vec::with_capacity(total_tasks),
            cancelled: Vec::with_capacity(total_tasks),
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
    /// Optional bulkhead: maximum number of split tasks allowed to run
    /// concurrently. When `None` (default) all tasks run as soon as the
    /// runtime can schedule them. When `Some(n)`, a `tokio::sync::Semaphore`
    /// with `n` permits gates each task body, so excess tasks queue until
    /// a permit is free. `Some(0)` is rejected at execution time with
    /// [`CanoError::Configuration`].
    pub bulkhead: Option<usize>,
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
            bulkhead: None,
        }
    }

    /// Set timeout for the split execution
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set the state to transition to after the join condition is met
    pub fn with_join_state(mut self, state: TState) -> Self {
        self.join_state = state;
        self
    }

    /// Cap concurrent split task execution at `n`. `0` is rejected when the
    /// split runs.
    pub fn with_bulkhead(mut self, n: usize) -> Self {
        self.bulkhead = Some(n);
        self
    }
}

/// Entry in the workflow state machine
pub enum StateEntry<TState, TResourceKey = Cow<'static, str>>
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Single task execution
    Single {
        task: Arc<dyn Task<TState, TResourceKey> + Send + Sync>,
        /// TaskConfig captured at registration time. Avoids per-dispatch
        /// virtual call through dyn Task to retrieve it.
        config: Arc<crate::task::TaskConfig>,
    },
    /// Split into parallel tasks with join configuration
    Split {
        tasks: Vec<Arc<dyn Task<TState, TResourceKey> + Send + Sync>>,
        /// Per-task configs in the same order as `tasks`. Captured at
        /// registration time (see `Single::config` for rationale).
        configs: Arc<Vec<Arc<crate::task::TaskConfig>>>,
        join_config: Arc<JoinConfig<TState>>,
    },
}

impl<TState, TResourceKey> Clone for StateEntry<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        match self {
            StateEntry::Single { task, config } => StateEntry::Single {
                task: task.clone(),
                config: Arc::clone(config),
            },
            StateEntry::Split {
                tasks,
                configs,
                join_config,
            } => StateEntry::Split {
                tasks: tasks.clone(),
                configs: Arc::clone(configs),
                join_config: join_config.clone(),
            },
        }
    }
}

/// State machine workflow orchestration with split/join support
///
/// The resource key type defaults to [`Cow<'static, str>`](std::borrow::Cow), which
/// accepts both `&'static str` literals (allocation-free) and owned `String` keys.
/// To use a different key type — e.g. an enum for compile-time typo protection —
/// specify it explicitly:
///
/// ```rust,ignore
/// let workflow = Workflow::<MyState, MyKeyType>::new(resources);
/// ```
#[must_use]
pub struct Workflow<TState, TResourceKey = Cow<'static, str>>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// State machine with support for split/join.
    /// Arc-wrapped so the FSM loop clones the entry as a cheap refcount bump
    /// rather than cloning the inner `Vec<Arc<dyn Task>>` on every iteration.
    states: HashMap<TState, Arc<StateEntry<TState, TResourceKey>>>,
    /// Shared resources for all tasks
    pub(crate) resources: Arc<Resources<TResourceKey>>,
    /// Global workflow timeout
    workflow_timeout: Option<Duration>,
    /// Exit states that terminate workflow. Stored as a Vec because typical
    /// workflows have ≤16 exit states; linear scan beats HashSet on cache locality.
    exit_states: Vec<TState>,
    /// Cached result of `validate()`. Populated on the first `orchestrate` call;
    /// subsequent calls read this and skip the O(states + splits) validation walk.
    validated: OnceLock<Result<(), CanoError>>,
    /// Observers notified of lifecycle and failure events. Empty by default;
    /// each event is delivered to every observer in registration order.
    observers: Vec<Arc<dyn WorkflowObserver>>,
    /// Optional tracing span
    #[cfg(feature = "tracing")]
    tracing_span: Option<Span>,
}

impl<TState, TResourceKey> Workflow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Create a new workflow with the given resources
    pub fn new(resources: Resources<TResourceKey>) -> Self {
        Self {
            states: HashMap::new(),
            resources: Arc::new(resources),
            workflow_timeout: None,
            exit_states: Vec::new(),
            validated: OnceLock::new(),
            observers: Vec::new(),
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
        T: Task<TState, TResourceKey> + Send + Sync + 'static,
    {
        let config = Arc::new(task.config());
        self.states.insert(
            state,
            Arc::new(StateEntry::Single {
                task: Arc::new(task),
                config,
            }),
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
        T: Task<TState, TResourceKey> + Send + Sync + 'static,
    {
        let configs: Vec<Arc<crate::task::TaskConfig>> =
            tasks.iter().map(|t| Arc::new(t.config())).collect();
        let arc_tasks: Vec<Arc<dyn Task<TState, TResourceKey> + Send + Sync>> =
            tasks.into_iter().map(|t| Arc::new(t) as Arc<_>).collect();

        self.states.insert(
            state,
            Arc::new(StateEntry::Split {
                tasks: arc_tasks,
                configs: Arc::new(configs),
                join_config: Arc::new(join_config),
            }),
        );
        self
    }

    /// Add exit state
    pub fn add_exit_state(mut self, state: TState) -> Self {
        if !self.exit_states.contains(&state) {
            self.exit_states.push(state);
        }
        self
    }

    /// Add multiple exit states
    pub fn add_exit_states(mut self, states: Vec<TState>) -> Self {
        for state in states {
            if !self.exit_states.contains(&state) {
                self.exit_states.push(state);
            }
        }
        self
    }

    /// Register a [`WorkflowObserver`] to receive lifecycle and failure events.
    ///
    /// May be called more than once; every observer receives every event in
    /// registration order. Observers are additive — they do not replace or
    /// suppress the engine's `tracing` instrumentation. Registration is O(1).
    ///
    /// # Example
    ///
    /// ```rust
    /// use cano::prelude::*;
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicUsize, Ordering};
    ///
    /// #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    /// enum Step { Start, Done }
    ///
    /// struct NoopTask;
    /// #[task]
    /// impl Task<Step> for NoopTask {
    ///     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
    ///         Ok(TaskResult::Single(Step::Done))
    ///     }
    /// }
    ///
    /// #[derive(Default)]
    /// struct Counter(AtomicUsize);
    /// impl WorkflowObserver for Counter {
    ///     fn on_task_success(&self, _task_id: &str) {
    ///         self.0.fetch_add(1, Ordering::Relaxed);
    ///     }
    /// }
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), CanoError> {
    /// let counter = Arc::new(Counter::default());
    /// let workflow = Workflow::bare()
    ///     .register(Step::Start, NoopTask)
    ///     .add_exit_state(Step::Done)
    ///     .with_observer(counter.clone());
    /// workflow.orchestrate(Step::Start).await?;
    /// assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_observer(mut self, observer: Arc<dyn WorkflowObserver>) -> Self {
        self.observers.push(observer);
        self
    }

    /// Set a tracing span for this workflow (requires "tracing" feature)
    #[cfg(feature = "tracing")]
    pub fn with_tracing_span(mut self, span: Span) -> Self {
        self.tracing_span = Some(span);
        self
    }

    /// A shareable snapshot of the registered observers, or `None` when none are
    /// attached. The `None` fast path keeps observer-free workflows allocation-free
    /// per dispatch; with observers attached this allocates one small slice per
    /// state transition / split task.
    fn observer_slice(&self) -> Option<Arc<[Arc<dyn WorkflowObserver>]>> {
        if self.observers.is_empty() {
            None
        } else {
            Some(Arc::from(self.observers.as_slice()))
        }
    }

    /// Return `base` unchanged when no observers are attached; otherwise a cloned
    /// config carrying the observer slice and task name so [`run_with_retries`]
    /// can emit `on_retry` / `on_circuit_open`.
    ///
    /// [`run_with_retries`]: crate::task::run_with_retries
    fn config_with_observers(
        base: &Arc<crate::task::TaskConfig>,
        observers: &Option<Arc<[Arc<dyn WorkflowObserver>]>>,
        task_name: &str,
    ) -> Arc<crate::task::TaskConfig> {
        match observers {
            None => Arc::clone(base),
            Some(slice) => {
                let mut cfg = (**base).clone();
                cfg.observers = Some(Arc::clone(slice));
                cfg.task_name = Some(Cow::Owned(task_name.to_owned()));
                Arc::new(cfg)
            }
        }
    }

    fn validate_join_config(
        join_config: &JoinConfig<TState>,
        _total_tasks: usize,
    ) -> Result<(), CanoError> {
        if matches!(join_config.strategy, JoinStrategy::PartialTimeout)
            && join_config.timeout.is_none()
        {
            return Err(CanoError::configuration(
                "PartialTimeout strategy requires a timeout to be configured",
            ));
        }
        if let JoinStrategy::Percentage(p) = join_config.strategy
            && (!p.is_finite() || p <= 0.0 || p > 1.0)
        {
            return Err(CanoError::configuration(format!(
                "Percentage strategy requires a finite value in (0.0, 1.0], got {p}"
            )));
        }
        if let Some(0) = join_config.bulkhead {
            return Err(CanoError::configuration(
                "bulkhead requires a positive permit count, got 0",
            ));
        }

        Ok(())
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
    /// # #[task]
    /// # impl Task<State> for MyTask {
    /// #     async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> {
    /// #         Ok(TaskResult::Single(State::Done))
    /// #     }
    /// # }
    /// let workflow = Workflow::bare()
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
        // Each join_state in a Split entry must either be registered or an exit state;
        // otherwise orchestration will always error at runtime after the split completes.
        for entry in self.states.values() {
            if let StateEntry::Split {
                tasks, join_config, ..
            } = entry.as_ref()
            {
                Self::validate_join_config(join_config, tasks.len())?;
                let js = &join_config.join_state;
                if !self.states.contains_key(js) && !self.exit_states.contains(js) {
                    return Err(CanoError::configuration(format!(
                        "Split join_state {:?} is neither registered nor an exit state",
                        js
                    )));
                }
            }
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
    /// # #[task]
    /// # impl Task<State> for MyTask {
    /// #     async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> {
    /// #         Ok(TaskResult::Single(State::Done))
    /// #     }
    /// # }
    /// let workflow = Workflow::bare()
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
    /// Runs lifecycle setup before execution and teardown after, regardless of outcome.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] -- no handler is registered for the current state, a single
    ///   task returned a `TaskResult::Split` (use [`Workflow::register_split`] instead), the
    ///   global workflow timeout was exceeded, or a split strategy was misconfigured
    /// - Any [`CanoError`] variant propagated from a task or node during execution
    pub async fn orchestrate(&self, initial_state: TState) -> Result<TState, CanoError> {
        #[cfg(feature = "tracing")]
        let workflow_span = self.tracing_span.clone().unwrap_or_else(|| {
            if tracing::enabled!(tracing::Level::INFO) {
                info_span!("workflow_orchestrate")
            } else {
                tracing::Span::none()
            }
        });

        #[cfg(feature = "tracing")]
        let _enter = workflow_span.enter();

        // Validate once and cache. Subsequent calls return the cached result without
        // re-walking the states + splits tree — workflow shape is immutable post-build.
        let cached_validation = self.validated.get_or_init(|| self.validate());
        if let Err(e) = cached_validation {
            return Err(e.clone());
        }

        // initial_state varies per call; this still runs every time, but it's O(1).
        self.validate_initial_state(&initial_state)?;

        self.resources.setup_all().await?;
        let result = self.run_workflow(initial_state).await;
        self.resources
            .teardown_range(0..self.resources.lifecycle_len())
            .await;
        result
    }

    async fn run_workflow(&self, initial_state: TState) -> Result<TState, CanoError> {
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

    pub(crate) async fn execute_workflow(
        &self,
        initial_state: TState,
    ) -> Result<TState, CanoError> {
        let mut current_state = initial_state;

        #[cfg(feature = "tracing")]
        info!(initial_state = ?current_state, "Starting workflow execution");

        loop {
            // Notify observers of every state entry, including the initial state
            // and any terminal exit state. Skip the `Debug` formatting allocation
            // entirely when no observers are attached.
            if !self.observers.is_empty() {
                let state_label = format!("{current_state:?}");
                notify_observers(&self.observers, |o| o.on_state_enter(&state_label));
            }

            // Check if we've reached an exit state
            if self.exit_states.contains(&current_state) {
                #[cfg(feature = "tracing")]
                info!(final_state = ?current_state, "Workflow completed successfully");
                return Ok(current_state);
            }

            // Get the state entry — borrow from map, clone only the Arc handles
            let state_entry = self.states.get(&current_state).ok_or_else(|| {
                CanoError::workflow(format!("No task registered for state: {:?}", current_state))
            })?;

            #[cfg(feature = "tracing")]
            debug!(current_state = ?current_state, "Executing state");

            // Execute based on entry type — deref the Arc then clone only the inner Arc handles
            current_state = match state_entry.as_ref() {
                StateEntry::Single { task, config } => {
                    self.execute_single_task(task.clone(), Arc::clone(config))
                        .await?
                }
                StateEntry::Split {
                    tasks,
                    configs,
                    join_config,
                } => {
                    self.execute_split_join(tasks.clone(), Arc::clone(configs), join_config.clone())
                        .await?
                }
            };
        }
    }

    async fn execute_single_task(
        &self,
        task: Arc<dyn Task<TState, TResourceKey> + Send + Sync>,
        config: Arc<crate::task::TaskConfig>,
    ) -> Result<TState, CanoError> {
        use crate::task::run_with_retries;

        let observers = self.observer_slice();
        let task_name = task.name();
        if let Some(ref slice) = observers {
            notify_observers(slice, |o| o.on_task_start(task_name.as_ref()));
        }
        // Stamp observers + task name onto a per-dispatch config so
        // `run_with_retries` can emit `on_retry` / `on_circuit_open`. Zero-cost
        // (returns the same Arc) when no observers are attached.
        let config = Self::config_with_observers(&config, &observers, &task_name);

        #[cfg(feature = "tracing")]
        let task_span = if tracing::enabled!(tracing::Level::INFO) {
            info_span!("single_task_execution")
        } else {
            tracing::Span::none()
        };

        // Wrap the retry-driving future in `catch_unwind` so a panic inside
        // the task body becomes a `CanoError::TaskExecution` instead of
        // unwinding through the workflow loop and aborting the runtime
        // worker. Split tasks apply the same catch-unwind pattern inside each
        // spawned task so their task index and panic payload are preserved.
        let run_future = async {
            run_with_retries(&config, || {
                let task_clone = task.clone();
                let resources_clone = Arc::clone(&self.resources);
                async move { task_clone.run(&*resources_clone).await }
            })
            .await
        };

        #[cfg(feature = "tracing")]
        let unwind_result = {
            let _enter = task_span.enter();
            AssertUnwindSafe(run_future).catch_unwind().await
        };

        #[cfg(not(feature = "tracing"))]
        let unwind_result = AssertUnwindSafe(run_future).catch_unwind().await;

        let result = match unwind_result {
            Ok(inner) => inner,
            Err(payload) => {
                // Forward the inner trait object so `downcast_ref` inspects
                // the actual panic payload type rather than the surrounding
                // `Box<dyn Any>`.
                let payload_str = panic_payload_message(&*payload);
                #[cfg(feature = "tracing")]
                tracing::error!(panic = %payload_str, "Single task panicked");
                Err(CanoError::task_execution(format!("panic: {payload_str}")))
            }
        };

        let outcome: Result<TState, CanoError> = match result {
            Ok(TaskResult::Single(next_state)) => {
                #[cfg(feature = "tracing")]
                debug!(next_state = ?next_state, "Single task completed");
                Ok(next_state)
            }
            Ok(TaskResult::Split(_)) => Err(CanoError::workflow(
                "Single task returned split result - use register_split() for split tasks",
            )),
            Err(e) => Err(e),
        };

        if let Some(ref slice) = observers {
            match &outcome {
                Ok(_) => notify_observers(slice, |o| o.on_task_success(task_name.as_ref())),
                Err(e) => notify_observers(slice, |o| o.on_task_failure(task_name.as_ref(), e)),
            }
        }
        outcome
    }

    async fn execute_split_join(
        &self,
        tasks: Vec<Arc<dyn Task<TState, TResourceKey> + Send + Sync>>,
        configs: Arc<Vec<Arc<crate::task::TaskConfig>>>,
        join_config: Arc<JoinConfig<TState>>,
    ) -> Result<TState, CanoError> {
        let resources = Arc::clone(&self.resources);
        let total_tasks = tasks.len();

        #[cfg(feature = "tracing")]
        info!(
            total_tasks = total_tasks,
            strategy = ?join_config.strategy,
            "Starting split execution"
        );

        // Validate strategy configuration before spawning tasks. `validate()`
        // catches these at build/start time; keep the execution-time check as
        // defense-in-depth for internal callers that bypass public orchestration.
        Self::validate_join_config(join_config.as_ref(), total_tasks)?;

        // Optional bulkhead: gate task bodies on a shared semaphore so at
        // most `n` split tasks execute concurrently.
        let bulkhead = join_config
            .bulkhead
            .map(|n| Arc::new(tokio::sync::Semaphore::new(n)));

        let mut join_set: tokio::task::JoinSet<(usize, Result<TaskResult<TState>, CanoError>)> =
            tokio::task::JoinSet::new();

        // Observer wiring. `task_ids[idx]` holds each split task's reported id
        // (`"{name}[{idx}]"`); kept on the parent so `on_task_success` /
        // `on_task_failure` can be fired from here once results are collected.
        // Both stay empty / `None` and cost nothing when no observers are attached.
        let observers = self.observer_slice();
        let mut task_ids: Vec<Cow<'static, str>> = if observers.is_some() {
            Vec::with_capacity(total_tasks)
        } else {
            Vec::new()
        };

        #[cfg_attr(not(feature = "tracing"), allow(unused_variables))]
        for (idx, task) in tasks.into_iter().enumerate() {
            use crate::task::run_with_retries;

            let task_id: Cow<'static, str> = if observers.is_some() {
                Cow::Owned(format!("{}[{}]", task.name(), idx))
            } else {
                Cow::Borrowed("<split-task>")
            };
            if let Some(ref slice) = observers {
                notify_observers(slice, |o| o.on_task_start(&task_id));
            }
            // Use cached config from registration time, with observers + task id
            // stamped on when present (a no-op clone-free path otherwise).
            let config = Self::config_with_observers(&configs[idx], &observers, &task_id);
            if observers.is_some() {
                task_ids.push(task_id);
            }
            let resources_clone = Arc::clone(&resources);
            let bulkhead_clone = bulkhead.clone();

            #[cfg(feature = "tracing")]
            let task_span = if tracing::enabled!(tracing::Level::INFO) {
                info_span!("split_task", task_id = idx)
            } else {
                tracing::Span::none()
            };

            join_set.spawn(async move {
                let run_future = async {
                    #[cfg(feature = "tracing")]
                    let _enter = task_span.enter();

                    #[cfg(feature = "tracing")]
                    debug!(task_id = idx, "Executing split task");

                    // Hold the permit (if any) until this future returns. The
                    // semaphore is never closed here, so `acquire_owned` only
                    // fails if it has been closed elsewhere — treat that as a
                    // task-execution error rather than panicking.
                    let _permit = match bulkhead_clone {
                        Some(sem) => match sem.acquire_owned().await {
                            Ok(p) => Some(p),
                            Err(e) => {
                                return (
                                    idx,
                                    Err(CanoError::task_execution(format!(
                                        "bulkhead semaphore closed: {e}"
                                    ))),
                                );
                            }
                        },
                        None => None,
                    };

                    let result = run_with_retries(&config, || {
                        let t = task.clone();
                        let r = Arc::clone(&resources_clone);
                        async move { t.run(&*r).await }
                    })
                    .await;

                    #[cfg(feature = "tracing")]
                    match &result {
                        Ok(_) => debug!(task_id = idx, "Split task completed successfully"),
                        Err(e) => warn!(task_id = idx, error = %e, "Split task failed"),
                    }

                    (idx, result)
                };

                match AssertUnwindSafe(run_future).catch_unwind().await {
                    Ok(outcome) => outcome,
                    Err(payload) => {
                        let payload_str = panic_payload_message(&*payload);
                        #[cfg(feature = "tracing")]
                        tracing::error!(task_id = idx, panic = %payload_str, "Split task panicked");
                        (
                            idx,
                            Err(CanoError::task_execution(format!(
                                "panic in split task {idx}: {payload_str}"
                            ))),
                        )
                    }
                }
            });
        }

        // Collect results using the unified strategy handler
        let split_result = self
            .collect_results(join_set, &join_config, total_tasks)
            .await?;

        // Fire per-task observer events for everything that ran to completion.
        // Cancelled tasks are intentionally silent (no dedicated hook), and an
        // aggregated error entry carries `task_index == usize::MAX`, which
        // `task_ids.get` filters out.
        if let Some(ref slice) = observers {
            for success in &split_result.successes {
                if let Some(id) = task_ids.get(success.task_index) {
                    notify_observers(slice, |o| o.on_task_success(id));
                }
            }
            for failure in &split_result.errors {
                if let (Some(id), Err(err)) = (task_ids.get(failure.task_index), &failure.result) {
                    notify_observers(slice, |o| o.on_task_failure(id, err));
                }
            }
        }

        let successful = split_result.successes.len();
        let _failed = split_result.errors.len();
        let _cancelled = split_result.cancelled.len();

        #[cfg(feature = "tracing")]
        info!(
            successful = successful,
            failed = _failed,
            cancelled = _cancelled,
            total = total_tasks,
            "Split execution completed"
        );

        // Check if join condition is met
        match &join_config.strategy {
            JoinStrategy::PartialResults(_) => {
                // For PartialResults, we always continue if minimum tasks completed successfully
                if join_config.strategy.is_satisfied(successful, total_tasks) {
                    Ok(join_config.join_state.clone())
                } else {
                    let mut message = format!(
                        "Partial results condition not met: {} completed successfully, {} required",
                        successful,
                        match &join_config.strategy {
                            JoinStrategy::PartialResults(min) => *min,
                            _ => 0,
                        }
                    );
                    if !split_result.errors.is_empty() {
                        message.push_str("; errors: ");
                        message.push_str(&split_error_summary(&split_result.errors));
                    }
                    Err(CanoError::workflow(message))
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
                    let mut message = format!(
                        "Join condition not met: {} of {} tasks completed successfully, strategy: {:?}",
                        successful, total_tasks, join_config.strategy
                    );
                    if !split_result.errors.is_empty() {
                        message.push_str("; errors: ");
                        message.push_str(&split_error_summary(&split_result.errors));
                    }
                    Err(CanoError::workflow(message))
                }
            }
        }
    }

    async fn collect_results(
        &self,
        mut join_set: tokio::task::JoinSet<(usize, Result<TaskResult<TState>, CanoError>)>,
        join_config: &JoinConfig<TState>,
        total_tasks: usize,
    ) -> Result<SplitResult<TState>, CanoError> {
        let mut split_result = SplitResult::with_capacity(total_tasks);
        // Bitset over task indices; cheaper than HashSet<usize> on cache and alloc.
        let mut completed_indices: Vec<bool> = vec![false; total_tasks];

        // Determine deadline if timeout is configured
        let deadline = join_config.timeout.map(|d| tokio::time::Instant::now() + d);

        loop {
            // Wait for next completion or timeout
            let next_result = if let Some(d) = deadline {
                match tokio::time::timeout_at(d, join_set.join_next()).await {
                    Ok(res) => res,
                    Err(_) => {
                        // Timeout reached
                        if matches!(join_config.strategy, JoinStrategy::PartialTimeout) {
                            // For PartialTimeout, abort remaining and return what we have
                            join_set.abort_all();
                            break;
                        } else {
                            // For other strategies, timeout is an error
                            join_set.abort_all();
                            return Err(CanoError::workflow("Split task timeout exceeded"));
                        }
                    }
                }
            } else {
                join_set.join_next().await
            };

            match next_result {
                Some(Ok((index, Ok(task_result)))) => {
                    completed_indices[index] = true;
                    split_result.successes.push(SplitTaskResult {
                        task_index: index,
                        result: Ok(task_result),
                    });
                }
                Some(Ok((index, Err(e)))) => {
                    completed_indices[index] = true;
                    split_result.errors.push(SplitTaskResult {
                        task_index: index,
                        result: Err(e),
                    });
                }
                Some(Err(join_err)) => {
                    // Task panicked or was aborted; index correlation is lost.
                    // Record as anonymous error so caller still sees the failure count.
                    split_result.errors.push(SplitTaskResult {
                        task_index: usize::MAX,
                        result: Err(CanoError::workflow(format!("Task panic: {:?}", join_err))),
                    });
                }
                None => break, // JoinSet drained
            }

            // Check if we can return early based on strategy
            match &join_config.strategy {
                JoinStrategy::Any if !split_result.successes.is_empty() => {
                    join_set.abort_all();
                    break;
                }
                JoinStrategy::PartialResults(min) if split_result.successes.len() >= *min => {
                    join_set.abort_all();
                    break;
                }
                _ => {} // Continue for other strategies
            }
        }

        // Identify cancelled tasks (those that didn't complete).
        // JoinSet::abort_all() and drop handle cleanup automatically.
        for (idx, completed) in completed_indices.iter().enumerate() {
            if !completed {
                split_result.cancelled.push(idx);
            }
        }

        Ok(split_result)
    }
}

impl<TState, TResourceKey> Clone for Workflow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            states: self.states.clone(),
            resources: Arc::clone(&self.resources),
            workflow_timeout: self.workflow_timeout,
            exit_states: self.exit_states.clone(),
            // Fresh OnceLock: cloned workflows re-validate on first orchestrate.
            validated: OnceLock::new(),
            observers: self.observers.clone(),
            #[cfg(feature = "tracing")]
            tracing_span: self.tracing_span.clone(),
        }
    }
}

impl<TState> Workflow<TState, Cow<'static, str>>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
{
    /// Construct a workflow with no resources.
    ///
    /// Use when the workflow needs no external dependencies.
    /// Equivalent to `Workflow::new(Resources::new())`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cano::prelude::*;
    ///
    /// #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    /// enum Step { Start, Done }
    ///
    /// struct NoopTask;
    ///
    /// #[task]
    /// impl Task<Step> for NoopTask {
    ///     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
    ///         Ok(TaskResult::Single(Step::Done))
    ///     }
    /// }
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), CanoError> {
    /// let result = Workflow::bare()
    ///     .register(Step::Start, NoopTask)
    ///     .add_exit_state(Step::Done)
    ///     .orchestrate(Step::Start)
    ///     .await?;
    /// assert_eq!(result, Step::Done);
    /// # Ok(())
    /// # }
    /// ```
    pub fn bare() -> Self {
        Self::new(Resources::new())
    }
}

impl<TState, TResourceKey> std::fmt::Debug for Workflow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
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
    use crate::resource::Resources;
    use crate::task::Task;
    use cano_macros::{node, task};
    use std::sync::Arc;
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

    #[task]
    impl Task<TestState> for SimpleTask {
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(TaskResult::Single(self.next_state.clone()))
        }
    }

    // Task that stores data using a MemoryStore from resources
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

    #[task]
    impl Task<TestState> for DataTask {
        async fn run(&self, res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
            let store: Arc<crate::store::MemoryStore> = res.get("store")?;
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

    #[task]
    impl Task<TestState> for FailTask {
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            if self.should_fail {
                Err(CanoError::task_execution("Task intentionally failed"))
            } else {
                Ok(TaskResult::Single(TestState::Complete))
            }
        }
    }

    #[tokio::test]
    async fn test_workflow_creation() {
        let workflow = Workflow::<TestState>::bare();

        assert_eq!(workflow.states.len(), 0);
        assert_eq!(workflow.exit_states.len(), 0);
    }

    #[tokio::test]
    async fn test_simple_workflow() {
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_multi_step_workflow() {
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_workflow_with_data() {
        let store = crate::store::MemoryStore::new();
        let resources = Resources::new().insert("store", store.clone());
        let workflow = Workflow::new(resources)
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
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_any_strategy() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Any, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_quorum_strategy() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Quorum(3), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_percentage_strategy() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        // 75% of 4 tasks = 3 tasks
        let join_config = JoinConfig::new(JoinStrategy::Percentage(0.75), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_with_failures_all_strategy() {
        let tasks = vec![
            FailTask::new(false),
            FailTask::new(true), // This will fail
            FailTask::new(false),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_split_with_failures_quorum_strategy() {
        let tasks = vec![
            FailTask::new(false),
            FailTask::new(false),
            FailTask::new(true), // This will fail
        ];

        // Quorum of 2, so should succeed despite one failure
        let join_config = JoinConfig::new(JoinStrategy::Quorum(2), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_with_timeout() {
        // Task that sleeps longer than timeout
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SlowTask, SlowTask];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete)
            .with_timeout(Duration::from_millis(50));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_workflow_timeout() {
        // Task that sleeps longer than workflow timeout
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let workflow = Workflow::bare()
            .with_timeout(Duration::from_millis(50))
            .register(TestState::Start, SlowTask)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Workflow timeout"));
    }

    #[tokio::test]
    async fn test_split_with_data_sharing() {
        let store = crate::store::MemoryStore::new();
        let resources = Resources::new().insert("store", store.clone());

        let tasks = vec![
            DataTask::new("task1", "value1", TestState::Join),
            DataTask::new("task2", "value2", TestState::Join),
            DataTask::new("task3", "value3", TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::new(resources)
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
        let store = crate::store::MemoryStore::new();
        let resources = Resources::new().insert("store", store.clone());

        let split_tasks = vec![
            DataTask::new("parallel1", "data1", TestState::Join),
            DataTask::new("parallel2", "data2", TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Process);

        let workflow = Workflow::new(resources)
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
        // Create tasks with varying delays - some will be cancelled
        #[derive(Clone)]
        struct DelayedTask {
            delay_ms: u64,
            #[allow(dead_code)]
            task_id: usize,
        }

        #[task]
        impl Task<TestState> for DelayedTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
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
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_results_with_failures() {
        // Mix of fast success, fast failure, and slow tasks
        #[derive(Clone)]
        struct MixedTask {
            delay_ms: u64,
            should_fail: bool,
        }

        #[task]
        impl Task<TestState> for MixedTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
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
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_results_minimum_not_met() {
        // All tasks will timeout before minimum is reached
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SlowTask, SlowTask, SlowTask];

        // Require 3 tasks but timeout after 100ms
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(3), TestState::Complete)
            .with_timeout(Duration::from_millis(100));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_partial_timeout_strategy() {
        // Create tasks with varying delays
        #[derive(Clone)]
        struct DelayedTask {
            delay_ms: u64,
            #[allow(dead_code)]
            task_id: usize,
        }

        #[task]
        impl Task<TestState> for DelayedTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
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
            .with_timeout(Duration::from_millis(200));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_timeout_with_failures() {
        // Mix of fast success, fast failure, and slow tasks
        #[derive(Clone)]
        struct MixedTask {
            delay_ms: u64,
            should_fail: bool,
        }

        #[task]
        impl Task<TestState> for MixedTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
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
            .with_timeout(Duration::from_millis(200));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_timeout_all_complete() {
        // All tasks complete before timeout
        #[derive(Clone)]
        struct FastTask {
            delay_ms: u64,
        }

        #[task]
        impl Task<TestState> for FastTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
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
            .with_timeout(Duration::from_millis(500));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_timeout_no_timeout_configured() {
        #[derive(Clone)]
        struct SimpleTaskLocal;

        #[task]
        impl Task<TestState> for SimpleTaskLocal {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SimpleTaskLocal, SimpleTaskLocal];

        // PartialTimeout without timeout should fail
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete);

        let workflow = Workflow::bare()
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
        // Workflow has no registered handlers — orchestrate should fail validation
        // upfront rather than reaching the FSM loop.
        let workflow = Workflow::<TestState>::bare().add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        let err = result.unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(err.to_string().contains("no registered state handlers"));
    }

    // --- Validation tests ---

    #[test]
    fn test_validate_empty_workflow() {
        let workflow = Workflow::<TestState>::bare();
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
        let workflow =
            Workflow::bare().register(TestState::Start, SimpleTask::new(TestState::Complete));
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
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);
        assert!(workflow.validate().is_ok());
    }

    #[test]
    fn test_validate_split_join_state_unregistered() {
        // join_state points to a state that is neither registered nor an exit state
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                vec![SimpleTask::new(TestState::Join)],
                JoinConfig::new(JoinStrategy::All, TestState::Process), // Process not registered
            )
            .add_exit_state(TestState::Complete);
        let result = workflow.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("join_state"));
    }

    #[test]
    fn test_validate_split_join_state_as_exit_state() {
        // join_state that is an exit state (valid)
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                vec![SimpleTask::new(TestState::Complete)],
                JoinConfig::new(JoinStrategy::All, TestState::Complete),
            )
            .add_exit_state(TestState::Complete);
        assert!(workflow.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_partial_timeout_without_timeout() {
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                vec![SimpleTask::new(TestState::Complete)],
                JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete),
            )
            .add_exit_state(TestState::Complete);

        let err = workflow
            .validate()
            .expect_err("PartialTimeout without timeout must fail validation");
        assert!(matches!(err, CanoError::Configuration(_)), "got {err:?}");
        assert!(err.to_string().contains("requires a timeout"));
    }

    #[test]
    fn test_validate_rejects_invalid_percentage() {
        for value in [0.0, 1.5, f64::NAN] {
            let workflow = Workflow::bare()
                .register_split(
                    TestState::Start,
                    vec![SimpleTask::new(TestState::Complete)],
                    JoinConfig::new(JoinStrategy::Percentage(value), TestState::Complete),
                )
                .add_exit_state(TestState::Complete);

            let err = workflow
                .validate()
                .expect_err("invalid Percentage strategy must fail validation");
            assert!(matches!(err, CanoError::Configuration(_)), "got {err:?}");
            assert!(err.to_string().contains("Percentage strategy"));
        }
    }

    #[test]
    fn test_validate_rejects_zero_bulkhead() {
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                vec![SimpleTask::new(TestState::Complete)],
                JoinConfig::new(JoinStrategy::All, TestState::Complete).with_bulkhead(0),
            )
            .add_exit_state(TestState::Complete);

        let err = workflow
            .validate()
            .expect_err("bulkhead=0 must fail validation");
        assert!(matches!(err, CanoError::Configuration(_)), "got {err:?}");
        assert!(err.to_string().contains("bulkhead"));
    }

    #[test]
    fn test_validate_initial_state() {
        let workflow = Workflow::bare()
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

    // Edge case coverage tests

    #[tokio::test]
    async fn test_empty_split_task_list() {
        let tasks: Vec<SimpleTask> = vec![];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_percentage_zero() {
        // Invalid percentage (0.0) should return a configuration error
        let tasks = vec![FailTask::new(true), FailTask::new(true)];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(0.0), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert!(
            matches!(err, CanoError::Configuration(_)),
            "expected Configuration error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_percentage_one() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(1.0), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );

        let tasks_fail = vec![FailTask::new(false), FailTask::new(true)];
        let join_config2 = JoinConfig::new(JoinStrategy::Percentage(1.0), TestState::Complete);
        let workflow2 = Workflow::bare()
            .register_split(TestState::Start, tasks_fail, join_config2)
            .add_exit_state(TestState::Complete);
        assert!(workflow2.orchestrate(TestState::Start).await.is_err());
    }

    #[tokio::test]
    async fn test_percentage_over_one() {
        // Invalid percentage (>1.0) should return a configuration error
        let tasks = vec![
            FailTask::new(false), // One task succeeds
            FailTask::new(true),  // One task fails
        ];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(1.5), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert!(
            matches!(err, CanoError::Configuration(_)),
            "expected Configuration error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_quorum_zero() {
        let tasks = vec![FailTask::new(true), FailTask::new(true)];
        let join_config = JoinConfig::new(JoinStrategy::Quorum(0), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_single_task_register_split() {
        let tasks = vec![SimpleTask::new(TestState::Complete)];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_workflow_no_exit_states() {
        // No exit states means validate() rejects the workflow before any task runs.
        let workflow =
            Workflow::bare().register(TestState::Start, SimpleTask::new(TestState::Complete));
        let result = workflow.orchestrate(TestState::Start).await;
        let err = result.unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(err.to_string().contains("no exit states"));
    }

    #[tokio::test]
    async fn test_split_task_from_single_register() {
        #[derive(Clone)]
        struct SplitReturningTask;

        #[task]
        impl Task<TestState> for SplitReturningTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Split(vec![TestState::Complete]))
            }
        }

        let workflow = Workflow::bare()
            .register(TestState::Start, SplitReturningTask)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("register_split"));
    }

    /// Regression test: a Node used in a workflow should only be retried once per the
    /// configured retry count, not double-retried by both Node::run_with_retries and
    /// the outer execute_single_task run_with_retries.
    #[tokio::test]
    async fn test_node_in_workflow_no_double_retry() {
        use crate::node::Node;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        struct CountingNode {
            call_count: Arc<std::sync::atomic::AtomicUsize>,
        }

        #[node]
        impl Node<TestState> for CountingNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1))
            }

            async fn prep(&self, _res: &Resources) -> Result<(), CanoError> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                Err(CanoError::preparation("always fails"))
            }

            async fn exec(&self, _: ()) -> () {}

            async fn post(&self, _res: &Resources, _: ()) -> Result<TestState, CanoError> {
                Ok(TestState::Complete)
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let node = CountingNode {
            call_count: Arc::clone(&call_count),
        };

        let workflow = Workflow::bare()
            .register(TestState::Start, node)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());

        // With max_retries=2, there should be exactly 3 attempts (1 initial + 2 retries).
        // Before the fix, double-retry would cause 3*3 = 9 attempts.
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            3,
            "Node should be called exactly 3 times (1 + 2 retries), not double-retried"
        );
    }

    /// Regression test: tasks registered via register_split() must honour their TaskConfig
    /// retry settings. Before the fix, split tasks called task.run() directly and retries
    /// were silently ignored.
    #[tokio::test]
    async fn test_split_task_retry_config_honoured() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone)]
        struct RetryCountingTask {
            call_count: Arc<AtomicUsize>,
            succeed_after: usize,
        }

        #[task]
        impl Task<TestState> for RetryCountingTask {
            fn config(&self) -> crate::task::TaskConfig {
                // Allow up to 4 retries so the task can eventually succeed
                crate::task::TaskConfig::new()
                    .with_fixed_retry(4, std::time::Duration::from_millis(1))
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
                if count >= self.succeed_after {
                    Ok(TaskResult::Single(TestState::Complete))
                } else {
                    Err(CanoError::task_execution("not ready yet"))
                }
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let tasks = vec![RetryCountingTask {
            call_count: Arc::clone(&call_count),
            succeed_after: 3, // fails twice, succeeds on third attempt
        }];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_ok(), "workflow should succeed after retries");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            3,
            "task should have been called exactly 3 times (2 failures + 1 success)"
        );
    }

    // ------------------------------------------------------------------
    // Workflow::bare() + Task::run_bare() integration
    // ------------------------------------------------------------------

    struct BareWorkflowTask;

    #[task]
    impl Task<TestState> for BareWorkflowTask {
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            Ok(TaskResult::Single(TestState::Complete))
        }
    }

    #[tokio::test]
    async fn test_workflow_bare_runs_task_with_run_bare() {
        let result = Workflow::bare()
            .register(TestState::Start, BareWorkflowTask)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    // ------------------------------------------------------------------
    // Resilience primitives: panic safety + bulkhead
    // ------------------------------------------------------------------

    struct PanickingTask;

    #[task]
    impl Task<TestState> for PanickingTask {
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::minimal()
        }

        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            panic!("boom");
        }
    }

    #[tokio::test]
    async fn test_single_task_panic_caught() {
        let workflow = Workflow::bare()
            .register(TestState::Start, PanickingTask)
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start)
            .await
            .expect_err("panic must surface as Err");
        match err {
            CanoError::TaskExecution(msg) => {
                assert!(msg.contains("panic"), "expected 'panic' in: {msg}");
                assert!(msg.contains("boom"), "expected 'boom' in: {msg}");
            }
            other => panic!("expected TaskExecution, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_split_task_panic_reports_index_and_payload() {
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                vec![PanickingTask],
                JoinConfig::new(JoinStrategy::All, TestState::Complete),
            )
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start)
            .await
            .expect_err("split panic must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("task 0"),
            "expected split error to include task index, got: {msg}"
        );
        assert!(
            msg.contains("boom"),
            "expected split error to include panic payload, got: {msg}"
        );
    }

    #[derive(Clone)]
    struct ConcurrencyProbe {
        live: Arc<std::sync::atomic::AtomicUsize>,
        max: Arc<std::sync::atomic::AtomicUsize>,
        sleep: Duration,
    }

    #[task]
    impl Task<TestState> for ConcurrencyProbe {
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::minimal()
        }

        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            // Update peak via a CAS loop.
            let mut peak = self.max.load(Ordering::SeqCst);
            while now > peak {
                match self
                    .max
                    .compare_exchange(peak, now, Ordering::SeqCst, Ordering::SeqCst)
                {
                    Ok(_) => break,
                    Err(actual) => peak = actual,
                }
            }
            tokio::time::sleep(self.sleep).await;
            self.live.fetch_sub(1, Ordering::SeqCst);
            Ok(TaskResult::Single(TestState::Complete))
        }
    }

    #[tokio::test]
    async fn test_split_bulkhead_caps_concurrency() {
        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tasks: Vec<ConcurrencyProbe> = (0..10)
            .map(|_| ConcurrencyProbe {
                live: Arc::clone(&live),
                max: Arc::clone(&max),
                sleep: Duration::from_millis(50),
            })
            .collect();

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete).with_bulkhead(2);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
        let observed = max.load(Ordering::SeqCst);
        assert!(
            observed <= 2,
            "bulkhead breached: observed concurrency = {observed}"
        );
        assert!(observed >= 1, "no tasks ran?");
    }

    #[tokio::test]
    async fn test_split_bulkhead_zero_rejected() {
        let tasks = vec![SimpleTask::new(TestState::Complete)];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete).with_bulkhead(0);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start)
            .await
            .expect_err("bulkhead=0 must error");
        assert!(matches!(err, CanoError::Configuration(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn test_attempt_timeout_via_workflow_retries() {
        // Sanity check: per-attempt timeout integrates with the workflow's
        // single-task path through TaskConfig.
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::new()
                    .with_fixed_retry(1, Duration::from_millis(1))
                    .with_attempt_timeout(Duration::from_millis(20))
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let err = Workflow::bare()
            .register(TestState::Start, SlowTask)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
            .await
            .expect_err("expected attempt timeout to exhaust retries");
        assert!(matches!(err, CanoError::RetryExhausted(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn test_split_tasks_share_circuit_breaker() {
        // N parallel split tasks share one Arc<CircuitBreaker>. They all fail
        // in the same workflow run; the breaker — protected by a std Mutex —
        // must record every failure correctly across the JoinSet workers and
        // end up Open.
        use crate::circuit::{CircuitBreaker, CircuitPolicy, CircuitState};

        struct FailingTask {
            breaker: Arc<CircuitBreaker>,
        }

        #[task]
        impl Task<TestState> for FailingTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal().with_circuit_breaker(Arc::clone(&self.breaker))
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Err(CanoError::task_execution("always fails"))
            }
        }

        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 4,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        }));

        let tasks: Vec<FailingTask> = (0..4)
            .map(|_| FailingTask {
                breaker: Arc::clone(&breaker),
            })
            .collect();

        // `All` waits for every task to run to completion (and record its failure
        // against the shared breaker) before the workflow returns. We discard the
        // workflow result — `All` propagates an Err when any task fails — because
        // we only care about the breaker side-effect.
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let _ = workflow.orchestrate(TestState::Start).await;
        assert!(
            matches!(breaker.state(), CircuitState::Open { .. }),
            "shared breaker must trip after 4 concurrent failures, got {:?}",
            breaker.state()
        );
    }
}
