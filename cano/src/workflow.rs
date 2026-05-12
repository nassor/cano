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
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::error::CanoError;
use crate::observer::WorkflowObserver;
use crate::recovery::CheckpointStore;
use crate::resource::Resources;
use crate::saga::{CompensatableTask, ErasedCompensatable};
use crate::task::stepped::{ErasedSteppedTask, SteppedAdapter, SteppedTask};
use crate::task::{RouterTask, Task};

#[cfg(feature = "tracing")]
use tracing::{Span, info_span};

mod compensation;
mod execution;
mod join;
#[cfg(test)]
mod test_support;

pub use execution::StateEntry;
pub use join::{JoinConfig, JoinStrategy, SplitResult, SplitTaskResult};

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
    /// Compensators for every state registered via [`register_with_compensation`](Self::register_with_compensation),
    /// keyed by the task's [`name`](crate::saga::CompensatableTask::name). Used to look a
    /// compensator up by id when draining the compensation stack — including a stack
    /// rehydrated from a checkpoint log, where only the `task_id` string is available.
    compensators: HashMap<String, Arc<dyn ErasedCompensatable<TState, TResourceKey>>>,
    /// Optional append-only checkpoint store. When set (alongside [`workflow_id`](Self::workflow_id)),
    /// every state entry writes a [`CheckpointRow`](crate::recovery::CheckpointRow) before the state's task runs, so a
    /// crashed run can be resumed via [`resume_from`](Self::resume_from). `None` by default —
    /// workflows without a store keep their zero-overhead behavior.
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    /// Identifier under which checkpoint rows are recorded for the forward
    /// (`orchestrate`) direction. Required whenever [`checkpoint_store`](Self::checkpoint_store)
    /// is set on that path (the first checkpoint write errors if it's missing);
    /// [`resume_from`](Self::resume_from) takes the id explicitly.
    workflow_id: Option<String>,
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
            compensators: HashMap::new(),
            checkpoint_store: None,
            workflow_id: None,
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
        self.forget_compensator_for(&state);
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

    /// Register a side-effect-free routing task for a state.
    ///
    /// A router only reads resources and returns the next state — it never writes.
    /// Use this for conditional branching without side effects. Router states are
    /// dispatched like [`StateEntry::Single`] but **skipped by the checkpoint writer**:
    /// no [`CheckpointRow`](crate::recovery::CheckpointRow) is appended and no sequence
    /// number is consumed when a router state is entered. The `on_state_enter` observer
    /// is still fired.
    ///
    /// **Recovery note:** if the *initial* state is a router and the workflow crashes
    /// before any non-router state has been checkpointed, `resume_from` will find zero
    /// rows and return an error. Restarting from the initial state is safe because
    /// routers have no side effects.
    ///
    /// `T` must implement both [`RouterTask`] (signals read-only intent) and [`Task`]
    /// (provides the `run` method the engine calls). Use the `#[task::router]` macro to
    /// derive the companion [`Task`] impl from your [`RouterTask`] implementation.
    ///
    /// Associates `task` with `state`. If a handler was already registered for `state`,
    /// it is replaced. This method is infallible.
    pub fn register_router<T>(mut self, state: TState, task: T) -> Self
    where
        T: RouterTask<TState, TResourceKey> + Task<TState, TResourceKey> + 'static,
    {
        self.forget_compensator_for(&state);
        let config = Arc::new(Task::config(&task));
        self.states.insert(
            state,
            Arc::new(StateEntry::Router {
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
        self.forget_compensator_for(&state);
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

    /// If `state` currently holds a [`StateEntry::CompensatableSingle`], drop that task's
    /// (now-stale) entry from the [`compensators`](Self::compensators) registry, so a later
    /// `register*` for the same state doesn't leave a dangling compensator reachable by id.
    fn forget_compensator_for(&mut self, state: &TState) {
        let stale_name = if let Some(StateEntry::CompensatableSingle { task, .. }) =
            self.states.get(state).map(|e| e.as_ref())
        {
            Some(task.name().into_owned())
        } else {
            None
        };
        if let Some(name) = stale_name {
            self.compensators.remove(&name);
        }
    }

    /// Register a [`CompensatableTask`] for a state.
    ///
    /// Like [`register`](Self::register), but the engine captures the task's
    /// [`Output`](CompensatableTask::Output) when it succeeds and pushes
    /// `(task name, serialized output)` onto the run's compensation stack. If a *later*
    /// state's task then fails, the stack is drained in reverse and each
    /// [`compensate`](CompensatableTask::compensate) runs. With a
    /// [`checkpoint store`](Self::with_checkpoint_store) attached, the serialized output
    /// is also persisted, so a [resumed](Self::resume_from) run can still compensate it.
    ///
    /// Replaces any handler previously registered for `state`. Infallible.
    ///
    /// **Compensation is supported for single-task states only** — there is no
    /// `register_split_with_compensation` in this version.
    pub fn register_with_compensation<T>(mut self, state: TState, task: T) -> Self
    where
        T: CompensatableTask<TState, TResourceKey> + 'static,
    {
        self.forget_compensator_for(&state);
        let config = Arc::new(task.config());
        let name = task.name().into_owned();
        let erased: Arc<dyn ErasedCompensatable<TState, TResourceKey>> =
            Arc::new(crate::saga::CompensatableAdapter(Arc::new(task)));
        self.compensators.insert(name, Arc::clone(&erased));
        self.states.insert(
            state,
            Arc::new(StateEntry::CompensatableSingle {
                task: erased,
                config,
            }),
        );
        self
    }

    /// Register a [`SteppedTask`] for a state with cursor-level checkpoint persistence.
    ///
    /// Unlike [`register`](Self::register), which runs the step loop in-memory via the
    /// macro-synthesised `Task::run`, this method hands the step loop to the engine so
    /// it can persist each cursor as a
    /// [`RowKind::StepCursor`](crate::recovery::RowKind::StepCursor) row after every
    /// `StepOutcome::More`. A subsequent [`resume_from`](Self::resume_from) rehydrates the
    /// latest cursor for this state and passes it to the first `step` call, so
    /// processing resumes from exactly where it left off rather than from `None`.
    ///
    /// When no [`checkpoint store`](Self::with_checkpoint_store) is attached the
    /// behavior is identical to `register` — the step loop runs in-memory with no
    /// cursor persistence.
    ///
    /// Replaces any handler previously registered for `state`. Infallible.
    pub fn register_stepped<T>(mut self, state: TState, task: T) -> Self
    where
        T: SteppedTask<TState, TResourceKey> + 'static,
    {
        self.forget_compensator_for(&state);
        let config = Arc::new(SteppedTask::config(&task));
        let erased: Arc<dyn ErasedSteppedTask<TState, TResourceKey>> =
            Arc::new(SteppedAdapter(Arc::new(task)));
        self.states.insert(
            state,
            Arc::new(StateEntry::Stepped {
                task: erased,
                config,
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

    /// Attach an append-only [`CheckpointStore`] so the workflow records its FSM
    /// progress and can be resumed after a crash via [`resume_from`](Self::resume_from).
    ///
    /// When set, every state entry writes one [`CheckpointRow`](crate::recovery::CheckpointRow) *before* the state's
    /// task runs: the sequence, the state's `Debug` label, and `task_id` — the
    /// [`Task::name`] for a single-task state, or `""` for a split state or a pure
    /// exit state. Split states write exactly one row — the entry row — not one per
    /// parallel task.
    ///
    /// You must also call [`with_workflow_id`](Self::with_workflow_id) to name the run
    /// (for the forward direction); [`orchestrate`](Self::orchestrate) errors on the first
    /// checkpoint write if a store is attached without an id. [`resume_from`](Self::resume_from)
    /// takes the id explicitly and doesn't need the builder.
    ///
    /// Workflows without a checkpoint store are unaffected — there is zero per-transition
    /// cost when no store is attached.
    ///
    /// State labels are the `Debug` rendering of `TState`; resume relies on each registered
    /// (or exit) state having a distinct `Debug` form, which holds for any `#[derive(Debug)]`
    /// enum.
    pub fn with_checkpoint_store(mut self, store: Arc<dyn CheckpointStore>) -> Self {
        self.checkpoint_store = Some(store);
        self
    }

    /// Set the identifier under which this run's [`CheckpointRow`](crate::recovery::CheckpointRow)s are recorded.
    ///
    /// Required whenever a [`checkpoint_store`](Self::with_checkpoint_store) is attached.
    /// [`resume_from`](Self::resume_from) takes the id explicitly, so this builder is only
    /// needed for the forward (`orchestrate`) direction.
    pub fn with_workflow_id(mut self, workflow_id: impl Into<String>) -> Self {
        self.workflow_id = Some(workflow_id.into());
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
            compensators: self.compensators.clone(),
            checkpoint_store: self.checkpoint_store.clone(),
            workflow_id: self.workflow_id.clone(),
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
            .field("workflow_id", &self.workflow_id)
            .field("checkpoint_store", &self.checkpoint_store.is_some())
            .field(
                "compensators",
                &format!("{} compensators", self.compensators.len()),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::resource::Resources;
    use crate::task;
    use crate::task::{Task, TaskResult};
    use cano_macros::task as task_macro;
    use tokio;

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

    // ------------------------------------------------------------------
    // Workflow::bare() + Task::run_bare() integration
    // ------------------------------------------------------------------

    struct BareWorkflowTask;

    #[task_macro]
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
    // register_router tests
    // ------------------------------------------------------------------

    // Note: inside the `cano` crate the inherent `#[task::router(state = S)]` form emits
    // `::cano::RouterTask<...>` paths that don't resolve. Use the trait-impl form instead.

    #[tokio::test]
    async fn test_register_router_orchestration_round_trip() {
        use crate::task::{RouterTask, TaskConfig};

        // A router that unconditionally routes Start → Process → Complete.
        struct RouteToProcess;

        #[task::router]
        impl RouterTask<TestState> for RouteToProcess {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Process))
            }
        }

        let result = Workflow::bare()
            .register_router(TestState::Start, RouteToProcess)
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
            .await
            .unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[test]
    fn test_validate_passes_with_router_state() {
        use crate::task::{RouterTask, TaskConfig};

        struct RouteToComplete;

        #[task::router]
        impl RouterTask<TestState> for RouteToComplete {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let workflow = Workflow::bare()
            .register_router(TestState::Start, RouteToComplete)
            .add_exit_state(TestState::Complete);

        assert!(workflow.validate().is_ok());
    }
}
