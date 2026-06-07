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

use crate::cancel::CancellationToken;
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
pub(crate) mod test_support;

pub use execution::StateEntry;
pub use join::{JoinConfig, JoinStrategy, SplitResult, SplitTaskResult};

/// Invoke `f` for each observer in `observers`. Used at every workflow event site;
/// a no-op when the list is empty (the common case — observers are opt-in).
///
/// Each dispatch is wrapped in `catch_unwind` so a panicking observer cannot
/// abort the runtime worker. Observer panics are converted to a `tracing::error!`
/// event (when the `tracing` feature is on) and, regardless of features, a one-line
/// stderr notice so a panic isn't silent in a default-features build — observers
/// are expected to be infallible, and surfacing the bug to the operator without
/// coupling it to the workflow's result requires *some* channel.
#[inline]
fn notify_observers(observers: &[Arc<dyn WorkflowObserver>], f: impl Fn(&dyn WorkflowObserver)) {
    for observer in observers {
        let observer_ref = observer.as_ref();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(observer_ref)));
        if let Err(payload) = result {
            let msg = panic_payload_message(&*payload);
            #[cfg(feature = "tracing")]
            tracing::error!(panic = %msg, "workflow observer panicked");
            // Even with `tracing` disabled the panic deserves a signal — a
            // default-features build would otherwise drop it on the floor and
            // mask test/regression observers that use `panic!` as an assertion.
            // We rate-limit by emitting at most one message per process via an
            // atomic flag so a panicky observer fired thousands of times can't
            // flood stderr.
            #[cfg(not(feature = "tracing"))]
            observer_panic_notice(&msg);
        }
    }
}

#[cfg(not(feature = "tracing"))]
fn observer_panic_notice(msg: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static EMITTED: AtomicBool = AtomicBool::new(false);
    // Compare-exchange so only the first panic prints; further panics are silent
    // (avoids drowning stderr in a loop) but the operator still sees one line.
    if EMITTED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        eprintln!(
            "cano: workflow observer panicked (further panics will be silent until process restart): {msg}"
        );
    }
}

/// Best-effort extraction of a human-readable message from a panic payload.
pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Drive `fut` under `catch_unwind`, converting a panic payload into a
/// `CanoError::TaskExecution("panic: <msg>")`. Use when the caller is
/// running user code (a task body, a compensate closure) and must not
/// let an unhandled panic propagate to the runtime worker.
///
/// `panic_label` is used for the `tracing::error!` event (`"{label}
/// panicked"`) when the `tracing` feature is enabled. Five call sites
/// across `workflow/execution.rs` and `workflow/compensation.rs` share
/// this shape; centralising it ensures the message format and panic-log
/// event are uniform.
///
/// Callers that need a different return shape (split-task tuples, the
/// drain's per-entry error list, the saga inline-compensate flattened
/// envelope) inline their own `catch_unwind` — see the plan's "out of
/// scope" notes for S2.
pub(crate) async fn catch_panic_to_error<T, F>(
    fut: F,
    panic_label: &'static str,
) -> Result<T, CanoError>
where
    F: std::future::Future<Output = Result<T, CanoError>>,
{
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(inner) => inner,
        Err(payload) => {
            let msg = panic_payload_message(&*payload);
            #[cfg(feature = "tracing")]
            tracing::error!(panic = %msg, "{} panicked", panic_label);
            #[cfg(not(feature = "tracing"))]
            let _ = panic_label;
            Err(CanoError::task_execution(format!("panic: {msg}")))
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
    /// Total wall-clock budget for the entire `orchestrate` / `resume_from` call.
    /// When set, the FSM aborts the in-flight task at its next await point as soon
    /// as the budget elapses and drains the compensation stack against
    /// [`compensation_timeout`](Self::compensation_timeout) (or a sensible default).
    /// `None` by default — workflows without this budget keep their zero-cost behavior.
    pub(crate) total_timeout: Option<Duration>,
    /// Optional bound on the saga compensation drain after a total-timeout trip.
    /// When unset, the engine derives a default from the remaining wall-clock budget.
    pub(crate) compensation_timeout: Option<Duration>,
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
    compensators: HashMap<Arc<str>, Arc<dyn ErasedCompensatable<TState, TResourceKey>>>,
    /// Optional append-only checkpoint store. When set (alongside [`workflow_id`](Self::workflow_id)),
    /// every state entry writes a [`CheckpointRow`](crate::recovery::CheckpointRow) before the state's task runs, so a
    /// crashed run can be resumed via [`resume_from`](Self::resume_from). `None` by default —
    /// workflows without a store keep their zero-overhead behavior.
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    /// Identifier under which checkpoint rows are recorded for the forward
    /// (`orchestrate`) direction. Required whenever [`checkpoint_store`](Self::checkpoint_store)
    /// is set on that path (the first checkpoint write errors if it's missing);
    /// [`resume_from`](Self::resume_from) takes the id explicitly.
    workflow_id: Option<Arc<str>>,
    /// Version stamped onto every [`CheckpointRow`](crate::recovery::CheckpointRow) the
    /// engine appends, and checked against the persisted version when
    /// [`resume_from`](Self::resume_from) replays a run. Defaults to `0`; set via
    /// [`with_workflow_version`](Self::with_workflow_version).
    workflow_version: u32,
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
            total_timeout: None,
            compensation_timeout: None,
            exit_states: Vec::new(),
            validated: OnceLock::new(),
            observers: Vec::new(),
            compensators: HashMap::new(),
            checkpoint_store: None,
            workflow_id: None,
            workflow_version: 0,
            #[cfg(feature = "tracing")]
            tracing_span: None,
        }
    }

    /// Set a wall-clock budget for the entire `orchestrate` (or `resume_from`) call.
    ///
    /// When the budget elapses, the in-flight task is aborted at its next await
    /// point, the compensation stack is drained against a bounded budget (see
    /// [`with_compensation_timeout`](Self::with_compensation_timeout)), and the call
    /// returns [`CanoError::WorkflowTimeout`](crate::CanoError::WorkflowTimeout)
    /// — or [`CanoError::CompensationFailed`](crate::CanoError::CompensationFailed)
    /// if rollback itself fails. Contrast with [`with_timeout`](Self::with_timeout),
    /// which is a single `tokio::time::timeout` around the whole future and offers
    /// no graceful compensation, and with
    /// [`TaskConfig::with_attempt_timeout`](crate::task::TaskConfig::with_attempt_timeout),
    /// which bounds each individual task attempt.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cano::prelude::*;
    /// use std::time::Duration;
    ///
    /// # #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    /// # enum Step { Start, Done }
    /// # #[derive(Clone)]
    /// # struct Slow;
    /// # #[task]
    /// # impl Task<Step> for Slow {
    /// #     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
    /// #         tokio::time::sleep(Duration::from_millis(50)).await;
    /// #         Ok(TaskResult::Single(Step::Done))
    /// #     }
    /// # }
    /// #
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), CanoError> {
    /// let workflow = Workflow::bare()
    ///     .with_total_timeout(Duration::from_millis(10))
    ///     .register(Step::Start, Slow)
    ///     .add_exit_state(Step::Done);
    ///
    /// let err = workflow
    ///     .orchestrate(Step::Start, CancellationToken::disabled())
    ///     .await
    ///     .expect_err("budget elapses before Done");
    /// // The engine wraps task errors with state context; `.inner()` peels one layer.
    /// assert!(matches!(err.inner(), CanoError::WorkflowTimeout { .. }));
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_total_timeout(mut self, timeout: Duration) -> Self {
        self.total_timeout = Some(timeout);
        self
    }

    /// Override the wall-clock budget for the saga compensation drain after a
    /// failure under [`with_total_timeout`](Self::with_total_timeout). Defaults
    /// to `min(total_timeout / 2, 30s)` — half of the user's configured total,
    /// capped at 30 seconds. Set explicitly to give compensations a known ceiling.
    ///
    /// Applies whenever `with_total_timeout` is set, regardless of failure
    /// cause: a per-attempt timeout, a circuit-open burst, a checkpoint-store
    /// error, or the engine-fired workflow timeout all drain under the same
    /// bounded budget.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use cano::prelude::*;
    /// # use std::time::Duration;
    /// # #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    /// # enum Step { Start, Done }
    /// # #[derive(Clone)]
    /// # struct Noop;
    /// # #[task]
    /// # impl Task<Step> for Noop {
    /// #     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
    /// #         Ok(TaskResult::Single(Step::Done))
    /// #     }
    /// # }
    /// let workflow = Workflow::bare()
    ///     .with_total_timeout(Duration::from_secs(30))
    ///     .with_compensation_timeout(Duration::from_secs(5))
    ///     .register(Step::Start, Noop)
    ///     .add_exit_state(Step::Done);
    /// # let _ = workflow;
    /// ```
    pub fn with_compensation_timeout(mut self, timeout: Duration) -> Self {
        self.compensation_timeout = Some(timeout);
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

    /// Register multiple tasks that execute in parallel with a join configuration.
    ///
    /// `tasks` accepts any [`IntoIterator`] (a `Vec`, an array, a `(0..n).map(…)` chain,
    /// etc.); the tasks are collected eagerly at registration time.
    pub fn register_split<T>(
        mut self,
        state: TState,
        tasks: impl IntoIterator<Item = T>,
        join_config: JoinConfig<TState>,
    ) -> Self
    where
        T: Task<TState, TResourceKey> + Send + Sync + 'static,
    {
        self.forget_compensator_for(&state);
        let mut configs: Vec<Arc<crate::task::TaskConfig>> = Vec::new();
        let mut arc_tasks: Vec<Arc<dyn Task<TState, TResourceKey> + Send + Sync>> = Vec::new();
        for t in tasks {
            configs.push(Arc::new(t.config()));
            arc_tasks.push(Arc::new(t) as Arc<_>);
        }

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
        let stale_name: Option<Arc<str>> =
            if let Some(StateEntry::CompensatableSingle { task, .. }) =
                self.states.get(state).map(|e| e.as_ref())
            {
                Some(Arc::from(task.name()))
            } else {
                None
            };
        if let Some(name) = stale_name {
            self.compensators.remove(&*name);
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
        let name: Arc<str> = Arc::from(task.name());
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

    /// Add multiple exit states.
    ///
    /// `states` accepts any [`IntoIterator`] (a `Vec`, an array, a `.map(…)` chain, etc.);
    /// duplicates already present are skipped.
    pub fn add_exit_states(mut self, states: impl IntoIterator<Item = TState>) -> Self {
        for state in states {
            if !self.exit_states.contains(&state) {
                self.exit_states.push(state);
            }
        }
        self
    }

    /// Iterate over every state that has a registered handler — every key in the
    /// internal state map populated by [`register`](Self::register),
    /// [`register_router`](Self::register_router),
    /// [`register_split`](Self::register_split),
    /// [`register_with_compensation`](Self::register_with_compensation), and
    /// [`register_stepped`](Self::register_stepped). **Exit-only states** added via
    /// [`add_exit_state`](Self::add_exit_state) without a handler are *not* yielded.
    ///
    /// Iteration order is unspecified (backing storage is a `HashMap`).
    pub fn registered_states(&self) -> impl Iterator<Item = &TState> {
        self.states.keys()
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
    /// workflow.orchestrate(Step::Start, CancellationToken::disabled()).await?;
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
    pub fn with_checkpoint_store(mut self, checkpoint_store: Arc<dyn CheckpointStore>) -> Self {
        self.checkpoint_store = Some(checkpoint_store);
        self
    }

    /// Set the identifier under which this run's [`CheckpointRow`](crate::recovery::CheckpointRow)s are recorded.
    ///
    /// Required whenever a [`checkpoint_store`](Self::with_checkpoint_store) is attached.
    /// [`resume_from`](Self::resume_from) takes the id explicitly, so this builder is only
    /// needed for the forward (`orchestrate`) direction.
    ///
    /// Accepts `impl Into<Arc<str>>` — `String`, `&str`, and `Arc<str>` all
    /// satisfy this. Generic wrappers should bound their parameter on
    /// `T: Into<Arc<str>>` to pass through cleanly.
    pub fn with_workflow_id(mut self, workflow_id: impl Into<Arc<str>>) -> Self {
        self.workflow_id = Some(workflow_id.into());
        self
    }

    /// Stamp this version onto every [`CheckpointRow`](crate::recovery::CheckpointRow)
    /// the engine appends, and reject [`resume_from`](Self::resume_from) when the
    /// persisted version differs. Defaults to `0`.
    ///
    /// Bump the version whenever the FSM shape changes between deploys (added or
    /// removed states, cursor schema, compensation output layout) so a resumed run
    /// can't silently land on an incompatible workflow definition — the mismatch
    /// surfaces as [`CanoError::WorkflowVersionMismatch`](crate::CanoError::WorkflowVersionMismatch).
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
    ///     .add_exit_state(State::Done)
    ///     .with_workflow_version(2);
    ///
    /// assert!(workflow.validate().is_ok());
    /// ```
    pub fn with_workflow_version(mut self, version: u32) -> Self {
        self.workflow_version = version;
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
    /// `token` controls cooperative cancellation. Drive the run with a [`CancellationToken`]
    /// obtained from [`CancellationToken::new`](crate::cancel::CancellationToken::new) and keep the
    /// paired [`CancellationHandle`](crate::cancel::CancellationHandle); when the handle's
    /// [`cancel`](crate::cancel::CancellationHandle::cancel) fires, the in-flight cancellable task
    /// is dropped at its next await point, the saga compensation stack is drained, and the call
    /// returns [`CanoError::Cancelled`] (wrapped in [`CanoError::WithStateContext`]; a dirty
    /// rollback yields [`CanoError::CompensationFailed`] whose `errors[0]` is the wrapped cancel).
    /// To opt a run out of cancellation, pass [`CancellationToken::disabled`] — it never fires and
    /// is zero-cost (the FSM skips the cancellation `select!` entirely).
    ///
    /// Cancellation is cooperative and saga-safe: a task is only interrupted at an `.await`, and a
    /// [`CompensatableTask`](crate::saga::CompensatableTask) is never interrupted mid-run (it
    /// completes so its rollback entry is recorded, and the cancel is honoured at the next state
    /// boundary). The compensation drain itself is uncancellable. See the
    /// [`cancel`](crate::cancel) module for the full semantics and precedence rules against
    /// [`with_total_timeout`](Self::with_total_timeout).
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] -- no handler is registered for the current state, a single
    ///   task returned a `TaskResult::Split` (use [`Workflow::register_split`] instead), the
    ///   global workflow timeout was exceeded, or a split strategy was misconfigured
    /// - [`CanoError::Cancelled`] -- the run was cancelled via `token` (see above)
    /// - Any [`CanoError`] variant propagated from a task during execution
    pub async fn orchestrate(
        &self,
        initial_state: TState,
        token: CancellationToken,
    ) -> Result<TState, CanoError> {
        #[cfg(feature = "tracing")]
        let workflow_span = self.tracing_span.clone().unwrap_or_else(|| {
            if tracing::enabled!(tracing::Level::INFO) {
                // `workflow_id` is recorded so that, when the `metrics-tracing-context`
                // bridge is wired up, every Cano metric emitted while this span is entered
                // inherits a `workflow_id` label. `Option<&str>` records nothing for `None`.
                info_span!(
                    "workflow_orchestrate",
                    workflow_id = self.workflow_id.as_deref()
                )
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
        let result = self.run_workflow(initial_state, token).await;
        self.resources
            .teardown_range(0..self.resources.lifecycle_len())
            .await;
        result
    }

    async fn run_workflow(
        &self,
        initial_state: TState,
        token: CancellationToken,
    ) -> Result<TState, CanoError> {
        #[cfg(feature = "metrics")]
        let _active = crate::metrics::WorkflowActiveGuard::new();
        let started = std::time::Instant::now();
        let total_budget = self.resolve_total_budget(started);
        let result = self
            .execute_workflow(initial_state, total_budget, token)
            .await;
        Self::record_run_outcome(&result, started);
        result
    }

    /// Resolve the wall-clock budget for the entire FSM call: the
    /// [`with_total_timeout`](Self::with_total_timeout) duration, or `None`
    /// (the zero-cost path) when unset.
    pub(crate) fn resolve_total_budget(
        &self,
        started: std::time::Instant,
    ) -> Option<(std::time::Instant, Duration)> {
        self.total_timeout.map(|d| (started, d))
    }

    /// Emit the workflow-run outcome metric (`completed` / `failed`) once per
    /// run. Called by both `run_workflow` (forward) and `execute_resume_inner`
    /// (resume) so the emission lives in one place. No-op without the `metrics`
    /// feature.
    #[cfg_attr(not(feature = "metrics"), allow(unused_variables))]
    pub(crate) fn record_run_outcome<T>(
        result: &Result<T, CanoError>,
        started: std::time::Instant,
    ) {
        #[cfg(feature = "metrics")]
        crate::metrics::workflow_run(
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
            started.elapsed(),
        );
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
            total_timeout: self.total_timeout,
            compensation_timeout: self.compensation_timeout,
            exit_states: self.exit_states.clone(),
            // Fresh OnceLock: cloned workflows re-validate on first orchestrate.
            validated: OnceLock::new(),
            observers: self.observers.clone(),
            compensators: self.compensators.clone(),
            checkpoint_store: self.checkpoint_store.clone(),
            workflow_id: self.workflow_id.clone(),
            workflow_version: self.workflow_version,
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
    ///     .orchestrate(Step::Start, CancellationToken::disabled())
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
            .field("total_timeout", &self.total_timeout)
            .field("compensation_timeout", &self.compensation_timeout)
            .field("workflow_id", &self.workflow_id)
            .field("workflow_version", &self.workflow_version)
            .field("checkpoint_store", &self.checkpoint_store.is_some())
            .field(
                "compensators",
                &format!("{} compensators", self.compensators.len()),
            )
            .finish()
    }
}

#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use crate::metrics::test_support::*;
    use crate::prelude::*;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        Start,
        Mid,
        Done,
    }
    struct GoTo(S);
    #[crate::task]
    impl Task<S> for GoTo {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(self.0.clone()))
        }
    }
    struct Boom;
    #[crate::task]
    impl Task<S> for Boom {
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Err(CanoError::task_execution("boom"))
        }
    }
    fn ok_workflow() -> Workflow<S> {
        Workflow::bare()
            .register(S::Start, GoTo(S::Mid))
            .register(S::Mid, GoTo(S::Done))
            .add_exit_state(S::Done)
    }

    #[test]
    fn successful_run_records_outcome_duration_and_clears_active_gauge() {
        let (res, rows) = run_with_recorder(|| async {
            ok_workflow()
                .orchestrate(S::Start, CancellationToken::disabled())
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(
            counter(
                &rows,
                "cano_workflow_runs_total",
                &[("outcome", "completed")]
            ),
            1
        );
        assert_eq!(
            histogram_count(
                &rows,
                "cano_workflow_duration_seconds",
                &[("outcome", "completed")]
            ),
            1
        );
        assert_eq!(gauge(&rows, "cano_workflow_active", &[]), 0.0);
    }

    #[test]
    fn failed_run_records_failed_outcome() {
        let (res, rows) = run_with_recorder(|| async {
            Workflow::bare()
                .register(S::Start, Boom)
                .add_exit_state(S::Done)
                .orchestrate(S::Start, CancellationToken::disabled())
                .await
        });
        assert!(res.is_err());
        assert_eq!(
            counter(&rows, "cano_workflow_runs_total", &[("outcome", "failed")]),
            1
        );
        assert_eq!(gauge(&rows, "cano_workflow_active", &[]), 0.0);
    }

    #[test]
    fn per_state_task_durations_are_recorded_single_and_split() {
        let (res, rows) = run_with_recorder(|| async {
            ok_workflow()
                .orchestrate(S::Start, CancellationToken::disabled())
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(
            histogram_count(
                &rows,
                "cano_task_duration_seconds",
                &[("state", "Start"), ("kind", "single")]
            ),
            1
        );
        assert_eq!(
            histogram_count(
                &rows,
                "cano_task_duration_seconds",
                &[("state", "Mid"), ("kind", "single")]
            ),
            1
        );
    }

    #[derive(Clone)]
    struct Branch {
        fail: bool,
    }
    #[crate::task]
    impl Task<S> for Branch {
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            if self.fail {
                Err(CanoError::task_execution("nope"))
            } else {
                Ok(TaskResult::Single(S::Done))
            }
        }
    }

    #[test]
    fn split_records_branch_results_and_a_split_kind_duration() {
        let (res, rows) = run_with_recorder(|| async {
            Workflow::bare()
                .register_split(
                    S::Start,
                    vec![
                        Branch { fail: false },
                        Branch { fail: true },
                        Branch { fail: false },
                    ],
                    JoinConfig::new(JoinStrategy::PartialResults(2), S::Done),
                )
                .add_exit_state(S::Done)
                .orchestrate(S::Start, CancellationToken::disabled())
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(
            counter(
                &rows,
                "cano_split_branch_results_total",
                &[("result", "success")]
            ),
            2
        );
        assert_eq!(
            counter(
                &rows,
                "cano_split_branch_results_total",
                &[("result", "failure")]
            ),
            1
        );
        assert_eq!(
            counter_opt(
                &rows,
                "cano_split_branch_results_total",
                &[("result", "cancelled")]
            ),
            None
        );
        assert_eq!(
            histogram_count(
                &rows,
                "cano_task_duration_seconds",
                &[("state", "Start"), ("kind", "split")]
            ),
            1
        );
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

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_multi_step_workflow() {
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
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

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);

        let data: String = store.get("test_key").unwrap();
        assert_eq!(data, "test_value");
    }

    #[tokio::test]
    async fn test_unregistered_state_error() {
        // Workflow has no registered handlers — orchestrate should fail validation
        // upfront rather than reaching the FSM loop.
        let workflow = Workflow::<TestState>::bare().add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
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

    #[tokio::test]
    async fn test_register_split_accepts_array_input() {
        // An array literal is `IntoIterator<Item = T>` but not a `Vec`; it must
        // register and run through split/join just like a `Vec` does.
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                [
                    SimpleTask::new(TestState::Complete),
                    SimpleTask::new(TestState::Complete),
                ],
                JoinConfig::new(JoinStrategy::All, TestState::Complete),
            )
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_register_split_accepts_lazy_iterator_input() {
        // A `Range::map` adapter is a lazy iterator with no `Vec` backing — it must
        // be consumed and materialized by `register_split` without a `.collect()`.
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                (0..3).map(|_| SimpleTask::new(TestState::Complete)),
                JoinConfig::new(JoinStrategy::All, TestState::Complete),
            )
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_add_exit_states_accepts_array_input() {
        // An array literal is `IntoIterator` but not a `Vec`.
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_states([TestState::Complete, TestState::Error]);
        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_add_exit_states_accepts_lazy_iterator_and_dedups() {
        // A lazy iterator (no Vec) carrying a duplicate — dedup must still hold.
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_states([TestState::Complete, TestState::Complete].into_iter());
        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
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
            .orchestrate(TestState::Start, CancellationToken::disabled())
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
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[test]
    fn registered_states_returns_all_registered_keys() {
        use crate::task::{RouterTask, TaskConfig};

        struct GoRoute;
        #[task::router]
        impl RouterTask<TestState> for GoRoute {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Process))
            }
        }

        let wf = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register_router(TestState::Process, GoRoute)
            .register_split(
                TestState::Split,
                vec![SimpleTask::new(TestState::Join)],
                JoinConfig::new(JoinStrategy::All, TestState::Join),
            )
            .add_exit_state(TestState::Complete);

        let mut registered: Vec<TestState> = wf.registered_states().cloned().collect();
        registered.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(
            registered,
            vec![TestState::Process, TestState::Split, TestState::Start]
        );
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

    // ------------------------------------------------------------------
    // with_total_timeout / with_compensation_timeout builder tests
    // ------------------------------------------------------------------

    #[test]
    fn test_with_total_timeout_stores_value_and_clones() {
        use std::time::Duration;
        let wf = Workflow::<TestState>::bare()
            .with_total_timeout(Duration::from_secs(5))
            .with_compensation_timeout(Duration::from_millis(750))
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);
        assert_eq!(wf.total_timeout, Some(Duration::from_secs(5)));
        assert_eq!(wf.compensation_timeout, Some(Duration::from_millis(750)));
        let cloned = wf.clone();
        assert_eq!(cloned.total_timeout, Some(Duration::from_secs(5)));
        assert_eq!(
            cloned.compensation_timeout,
            Some(Duration::from_millis(750))
        );
    }

    #[test]
    fn test_with_total_timeout_defaults_to_none() {
        let wf = Workflow::<TestState>::bare();
        assert_eq!(wf.total_timeout, None);
        assert_eq!(wf.compensation_timeout, None);
    }

    #[test]
    fn test_workflow_debug_includes_total_and_compensation_timeouts() {
        use std::time::Duration;
        let wf = Workflow::<TestState>::bare()
            .with_total_timeout(Duration::from_secs(5))
            .with_compensation_timeout(Duration::from_millis(750));
        let debug_str = format!("{wf:?}");
        assert!(debug_str.contains("total_timeout"), "got: {debug_str}");
        assert!(
            debug_str.contains("compensation_timeout"),
            "got: {debug_str}"
        );
    }

    #[tokio::test]
    async fn orchestrate_survives_observer_panic_via_notify_observers_catch_unwind() {
        // A panicking observer must NOT abort the orchestrate caller — the
        // scheduler had its own catch_unwind, but direct `orchestrate` users
        // relied on `notify_observers` being panic-safe. This test exercises
        // the orchestrate path directly with an observer that panics on
        // every state-enter call; the workflow should still complete.
        use crate::observer::WorkflowObserver;

        struct PanickyObserver;
        impl WorkflowObserver for PanickyObserver {
            fn on_state_enter(&self, _state: &str) {
                panic!("observer panic — must not abort the workflow");
            }
        }

        let result = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_observer(Arc::new(PanickyObserver))
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .expect("orchestrate must complete despite observer panic");
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn repeated_observer_panics_do_not_block_workflow_progress() {
        // Regression for F15: even with `tracing` disabled, the no-tracing
        // fallback (one-shot stderr) must NEVER block, deadlock, or otherwise
        // disrupt the FSM loop when the same observer panics many times.
        // The atomic rate-limit means only the first panic actually prints;
        // the remaining N-1 must still flow through the catch_unwind silently.
        use crate::observer::WorkflowObserver;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountThenPanic(Arc<AtomicUsize>);
        impl WorkflowObserver for CountThenPanic {
            fn on_state_enter(&self, _state: &str) {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("repeated panic — must not block the FSM loop");
            }
        }

        let count = Arc::new(AtomicUsize::new(0));
        let result = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_observer(Arc::new(CountThenPanic(Arc::clone(&count))))
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .expect("orchestrate must complete despite repeated observer panics");
        assert_eq!(result, TestState::Complete);
        // Three state entries (Start, Process, Complete) — the panic fires per
        // entry; if any panic propagated, the FSM would not have completed.
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    // ------------------------------------------------------------------
    // Edge cases: exit-state handling, runtime transition errors, builder semantics
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn orchestrate_with_exit_state_as_initial_returns_immediately() {
        // The exit-state check precedes the handler lookup, so starting *at* an exit state
        // returns it without running any task.
        let start = SimpleTask::new(TestState::Complete);
        let wf = Workflow::bare()
            .register(TestState::Start, start.clone())
            .add_exit_state(TestState::Complete);
        let result = wf
            .orchestrate(TestState::Complete, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
        assert_eq!(
            start.count(),
            0,
            "no handler runs when the initial state is an exit state"
        );
    }

    #[tokio::test]
    async fn exit_state_takes_precedence_over_registered_handler() {
        // `Process` is BOTH a registered handler AND an exit state. Reaching it must exit
        // (the exit check at line ~357 runs before the handler lookup) — its task never runs.
        let process = SimpleTask::new(TestState::Start); // would loop back to Start if it ran
        let wf = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, process.clone())
            .add_exit_state(TestState::Process);
        let result = wf
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Process);
        assert_eq!(
            process.count(),
            0,
            "an exit state short-circuits before its handler runs"
        );
    }

    #[tokio::test]
    async fn transition_to_unregistered_state_errors_at_runtime() {
        // The workflow validates fine (has handlers + an exit state), but Start routes to
        // Process, which has no handler and isn't an exit state -> runtime "No task registered".
        let wf = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .add_exit_state(TestState::Complete);
        let err = wf
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No task registered"), "got: {err}");
    }

    #[tokio::test]
    async fn orchestrate_from_unknown_initial_state_errors() {
        // Distinct from the empty-workflow case: the workflow is valid, but the *initial* state
        // is neither a registered handler nor an exit state -> validate_initial_state error.
        let wf = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);
        let err = wf
            .orchestrate(TestState::Process, CancellationToken::disabled())
            .await
            .unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(
            err.to_string()
                .contains("neither registered nor an exit state"),
            "got: {err}"
        );
    }

    struct ReturnsSplit;
    #[task_macro]
    impl Task<TestState> for ReturnsSplit {
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            Ok(TaskResult::Split(vec![TestState::Complete]))
        }
    }

    #[tokio::test]
    async fn single_task_returning_split_errors() {
        // A non-split state whose task returns TaskResult::Split is a misuse -> clear error.
        let wf = Workflow::bare()
            .register(TestState::Start, ReturnsSplit)
            .add_exit_state(TestState::Complete);
        let err = wf
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("use register_split"), "got: {err}");
    }

    #[tokio::test]
    async fn register_replaces_a_prior_handler_for_the_same_state() {
        let first = SimpleTask::new(TestState::Process); // would route to an unregistered state
        let wf = Workflow::bare()
            .register(TestState::Start, first.clone())
            .register(TestState::Start, SimpleTask::new(TestState::Complete)) // replaces `first`
            .add_exit_state(TestState::Complete);
        let result = wf
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete); // the second handler ran
        assert_eq!(first.count(), 0, "the replaced handler must not run");
    }

    struct LoopUntil {
        limit: u32,
        count: Arc<std::sync::atomic::AtomicU32>,
    }
    #[task_macro]
    impl Task<TestState> for LoopUntil {
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            use std::sync::atomic::Ordering;
            let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= self.limit {
                Ok(TaskResult::Single(TestState::Complete))
            } else {
                Ok(TaskResult::Single(TestState::Start)) // self-loop
            }
        }
    }

    #[tokio::test]
    async fn self_looping_state_runs_until_it_routes_to_exit() {
        // A state whose task returns its own state re-runs each iteration until it routes to an
        // exit state — the FSM loop handles cycles, terminating only on an exit state.
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let wf = Workflow::bare()
            .register(
                TestState::Start,
                LoopUntil {
                    limit: 5,
                    count: Arc::clone(&count),
                },
            )
            .add_exit_state(TestState::Complete);
        let result = wf
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn empty_split_joins_to_its_join_state() {
        // A split with zero branches: the All-join is vacuously satisfied, so the engine
        // transitions straight to the join_state. A timeout guard turns a hang regression into a
        // failure instead of stalling the suite.
        let wf = Workflow::bare()
            .register_split(
                TestState::Start,
                Vec::<SimpleTask>::new(),
                JoinConfig::new(JoinStrategy::All, TestState::Complete),
            )
            .add_exit_state(TestState::Complete);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wf.orchestrate(TestState::Start, CancellationToken::disabled()),
        )
        .await
        .expect("orchestrate of an empty split must not hang");
        assert_eq!(result.unwrap(), TestState::Complete);
    }

    #[test]
    fn add_exit_state_and_add_exit_states_deduplicate() {
        let wf = Workflow::<TestState>::bare()
            .add_exit_state(TestState::Complete)
            .add_exit_state(TestState::Complete) // duplicate ignored
            .add_exit_states(vec![
                TestState::Complete, // duplicate ignored
                TestState::Error,
                TestState::Error, // duplicate ignored
            ]);
        assert_eq!(wf.exit_states.len(), 2);
        assert!(wf.exit_states.contains(&TestState::Complete));
        assert!(wf.exit_states.contains(&TestState::Error));
    }

    #[test]
    fn workflow_id_and_version_are_stored_defaulted_and_cloned() {
        let wf = Workflow::<TestState>::bare();
        assert_eq!(wf.workflow_version, 0); // default
        assert!(wf.workflow_id.is_none());

        let wf = wf.with_workflow_id("run-42").with_workflow_version(7);
        assert_eq!(wf.workflow_version, 7);
        assert_eq!(wf.workflow_id.as_deref(), Some("run-42"));

        let cloned = wf.clone();
        assert_eq!(cloned.workflow_version, 7);
        assert_eq!(cloned.workflow_id.as_deref(), Some("run-42"));
    }
}

/// Edge-case unit tests for `resolve_total_budget` — the budget is simply the
/// `with_total_timeout` duration (or `None`).
#[cfg(test)]
mod resolve_total_budget_tests {
    use super::test_support::TestState;
    use super::*;
    use std::time::Duration;

    #[test]
    fn unset_returns_none() {
        let w = Workflow::<TestState>::bare();
        assert!(w.resolve_total_budget(std::time::Instant::now()).is_none());
    }

    #[test]
    fn total_timeout_set_returns_total() {
        let w = Workflow::<TestState>::bare().with_total_timeout(Duration::from_secs(7));
        let now = std::time::Instant::now();
        let (start, limit) = w.resolve_total_budget(now).unwrap();
        assert_eq!(start, now);
        assert_eq!(limit, Duration::from_secs(7));
    }
}

/// Edge-case unit tests for `catch_panic_to_error`. The integration-level
/// sentinels (every single/compensatable/stepped panic test) exercise the
/// helper end-to-end through the task dispatch; this module pins down the
/// helper's contract in isolation so a regression in panic-message
/// extraction or error shape lands here first.
#[cfg(test)]
mod catch_panic_to_error_tests {
    use super::*;

    #[tokio::test]
    async fn passes_through_ok_result_unchanged() {
        let result =
            catch_panic_to_error(async { Ok::<u32, CanoError>(42) }, "non-panicking").await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn passes_through_err_result_unchanged() {
        // A non-panicking future that returns `Err` must surface that error
        // verbatim — NOT convert it to "panic: ...".
        let result = catch_panic_to_error::<u32, _>(
            async { Err(CanoError::task_execution("explicit failure")) },
            "non-panicking",
        )
        .await;
        match result {
            Err(CanoError::TaskExecution(m)) => assert_eq!(m, "explicit failure"),
            other => panic!("expected explicit TaskExecution err, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn str_literal_panic_payload_yields_panic_prefixed_task_execution_err() {
        let result = catch_panic_to_error::<u32, _>(
            async {
                panic!("static literal");
            },
            "labelled",
        )
        .await;
        match result {
            Err(CanoError::TaskExecution(m)) => assert_eq!(m, "panic: static literal"),
            other => panic!("expected TaskExecution(\"panic: ...\"), got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn formatted_string_panic_payload_preserves_message() {
        let result = catch_panic_to_error::<u32, _>(
            async {
                let detail = 99;
                panic!("formatted message {detail}");
            },
            "labelled",
        )
        .await;
        match result {
            Err(CanoError::TaskExecution(m)) => assert_eq!(m, "panic: formatted message 99"),
            other => panic!("expected formatted TaskExecution, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_string_panic_payload_yields_fallback_message() {
        // `panic_any` with a non-string payload exercises the
        // `panic_payload_message` fallback ("<non-string panic payload>").
        let result = catch_panic_to_error::<u32, _>(
            async {
                std::panic::panic_any(42i32);
            },
            "labelled",
        )
        .await;
        match result {
            Err(CanoError::TaskExecution(m)) => {
                assert_eq!(m, "panic: <non-string panic payload>")
            }
            other => panic!("expected fallback TaskExecution, got: {other:?}"),
        }
    }
}
