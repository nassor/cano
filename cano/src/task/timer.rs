//! # TimerTask — Wait-Then-Transition Processing Model
//!
//! A [`TimerTask`] sleeps for a [`Duration`](std::time::Duration) (or until a monotonic
//! [`Instant`](std::time::Instant) is reached) and then transitions to the next state. It
//! replaces the manual `PollTask + sleep + check` boilerplate users write when all they actually
//! need is "pause here, then continue": a [`PollTask`](crate::task::PollTask) re-evaluates a
//! condition on every wake-up, whereas a `TimerTask` schedules **a single** `tokio::time::sleep`
//! and wakes exactly once.
//!
//! ## When to use `TimerTask`
//!
//! - **Cool-down / debounce**: wait a fixed delay before the next step (rate-pacing, backoff
//!   between externally-triggered phases).
//! - **Deferred hand-off**: resume after a known delay via [`TimerOutcome::Until`].
//! - **Deliberate pauses** in a workflow that don't depend on polling an external condition.
//!
//! If you need to *re-check a condition* while waiting, use a
//! [`PollTask`](crate::task::PollTask) instead.
//!
//! Every [`TimerTask`] automatically implements [`Task`](crate::task::Task) via a per-impl-site
//! companion `impl Task<S> for T` emitted by the `#[task::timer]` macro, so you register one with
//! [`Workflow::register`](crate::workflow::Workflow::register) exactly like any other task.
//!
//! ## Recovery: a timer re-waits from scratch on resume
//!
//! A `TimerTask` is an ordinary checkpointed single-task state: the engine writes its
//! [`CheckpointRow`](crate::recovery::CheckpointRow) **before** running it, so a
//! [resumed](crate::workflow::Workflow::resume_from) run **re-runs `wait()` from the start**. A
//! `Duration(30 min)` timer that crashes at minute 29 sleeps the full 30 minutes again after
//! resume, and an [`Until`](TimerOutcome::Until) deadline built from
//! [`Instant::now()`](std::time::Instant::now) is recomputed relative to the *new* now — so it
//! shifts forward by the downtime rather than firing at the originally-intended moment. Note also
//! that [`Instant`] is a **monotonic, process-relative** clock reading, not a wall-clock/calendar
//! time, and is not comparable across process restarts. If you need a delay that survives a crash,
//! derive it inside `wait()` from a timestamp you persist in [`Resources`] (or a checkpoint),
//! not from `Instant::now()`.
//!
//! ## Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//! use std::time::Duration;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Wait, Done }
//!
//! struct CoolDown;
//!
//! #[task::timer(state = Step)]
//! impl CoolDown {
//!     async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
//!         Ok(TimerOutcome::Duration(Duration::from_millis(50)))
//!     }
//!
//!     async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let workflow = Workflow::bare()
//!     .register(Step::Wait, CoolDown)
//!     .add_exit_state(Step::Done);
//!
//! let result = workflow.orchestrate(Step::Wait, CancellationToken::disabled()).await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```
//!
//! ## Bounding the wait
//!
//! A `TimerTask` runs as a single dispatch attempt, so an `attempt_timeout` from
//! [`TimerTask::config`] caps the whole wait. If the timer hasn't fired by then, the workflow
//! receives [`CanoError::Timeout`]:
//!
//! ```rust
//! use cano::prelude::*;
//! use std::time::Duration;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Wait, Done }
//!
//! struct BoundedTimer;
//!
//! #[task::timer(state = Step)]
//! impl BoundedTimer {
//!     fn config(&self) -> TaskConfig {
//!         TaskConfig::minimal().with_attempt_timeout(Duration::from_secs(5))
//!     }
//!
//!     async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
//!         Ok(TimerOutcome::Duration(Duration::from_millis(10)))
//!     }
//!
//!     async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//! ```
//!
//! ## The trait-impl form
//!
//! You can also write the full `impl TimerTask<S> for T` header explicitly:
//!
//! ```rust
//! use cano::prelude::*;
//! use std::time::Duration;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Wait, Done }
//!
//! struct TraitTimer;
//!
//! #[task::timer]
//! impl TimerTask<Step> for TraitTimer {
//!     async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
//!         Ok(TimerOutcome::Duration(Duration::from_millis(10)))
//!     }
//!     async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//! ```

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{TaskConfig, TaskResult};
use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// TimerOutcome — how long to wait before transitioning
// ---------------------------------------------------------------------------

/// The outcome returned by [`TimerTask::wait`]: how long the engine should sleep before
/// calling [`after_wait`](TimerTask::after_wait).
///
/// - [`TimerOutcome::Duration`] — sleep for a relative duration from now.
/// - [`TimerOutcome::Until`] — sleep until a monotonic [`Instant`]. An instant in the past fires
///   immediately (no sleep).
///
/// Note that [`Instant`] is a **monotonic, process-relative** clock reading — not a
/// wall-clock/calendar time, and not meaningful across a process restart. See the
/// [module-level recovery note](self#recovery-a-timer-re-waits-from-scratch-on-resume).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimerOutcome {
    /// Sleep for this duration, then transition. `Duration::ZERO` transitions immediately.
    Duration(Duration),
    /// Sleep until this monotonic [`Instant`], then transition. A past instant fires immediately.
    Until(Instant),
}

// ---------------------------------------------------------------------------
// TimerTask trait
// ---------------------------------------------------------------------------

/// A "wait-then-transition" processing model that sleeps once for the
/// [`TimerOutcome`] returned by [`wait`](TimerTask::wait), then routes via
/// [`after_wait`](TimerTask::after_wait).
///
/// # Generic Types
///
/// - **`TState`**: The state enum used by the workflow (`Clone + Debug + Send + Sync`).
/// - **`TResourceKey`**: The key type used to look up resources (defaults to
///   [`Cow<'static, str>`]).
///
/// # Default config
///
/// Like [`PollTask`](crate::task::PollTask), `TimerTask` defaults to
/// [`TaskConfig::minimal()`] (no retries). A single scheduled sleep does not benefit from outer
/// retry wrapping; to bound the wait, attach an `attempt_timeout` instead.
///
/// # Implementing TimerTask
///
/// Prefer the inherent `#[task::timer(state = S)]` form:
///
/// ```rust
/// use cano::prelude::*;
/// use std::time::Duration;
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// enum Step { Wait, Done }
///
/// struct MyTimer;
///
/// #[task::timer(state = Step)]
/// impl MyTimer {
///     async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
///         Ok(TimerOutcome::Duration(Duration::from_millis(10)))
///     }
///     async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
///         Ok(TaskResult::Single(Step::Done))
///     }
/// }
/// ```
#[crate::task::timer]
pub trait TimerTask<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Get the task configuration that controls execution behaviour.
    ///
    /// Defaults to [`TaskConfig::minimal()`] (no retries, no timeout). Override to attach a
    /// per-attempt timeout that bounds the wait.
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    /// Human-readable identifier for this timer, reported to [`WorkflowObserver`] hooks.
    ///
    /// Defaults to [`std::any::type_name`] of the implementing type. Override for a stable,
    /// friendly name.
    ///
    /// [`WorkflowObserver`]: crate::observer::WorkflowObserver
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Decide how long to wait. Called once, before the engine sleeps.
    ///
    /// Return [`TimerOutcome::Duration`] for a relative delay or [`TimerOutcome::Until`] for a
    /// monotonic [`Instant`](std::time::Instant) deadline.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError`] to abort before sleeping (no [`after_wait`](Self::after_wait) call).
    async fn wait(&self, res: &Resources<TResourceKey>) -> Result<TimerOutcome, CanoError>;

    /// Decide the next state. Called once, after the sleep has elapsed.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError`] propagated from the task logic.
    async fn after_wait(
        &self,
        res: &Resources<TResourceKey>,
    ) -> Result<TaskResult<TState>, CanoError>;
}

// ---------------------------------------------------------------------------
// run_timer — the body called by the macro-synthesised Task::run
// ---------------------------------------------------------------------------

/// Run the [`TimerTask`] wait-then-transition sequence for `t`.
///
/// The synthesised `Task::run` method emitted by `#[task::timer]` delegates here so that the
/// sleep body (which needs `tokio::time`) lives in a single place. It calls `t.wait(res)`, sleeps
/// for the returned [`TimerOutcome`] (a `Duration::ZERO` or a past `Until` instant resolves
/// immediately), then returns `t.after_wait(res)`.
///
/// To cap the whole wait, return `TaskConfig::minimal().with_attempt_timeout(dur)` from
/// [`TimerTask::config`] and dispatch the task via
/// [`Workflow::orchestrate`](crate::workflow::Workflow::orchestrate).
pub async fn run_timer<T, S, K>(t: &T, res: &Resources<K>) -> Result<TaskResult<S>, CanoError>
where
    T: TimerTask<S, K> + ?Sized,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    match t.wait(res).await? {
        TimerOutcome::Duration(d) => tokio::time::sleep(d).await,
        // `sleep_until` resolves immediately for a deadline already in the past.
        TimerOutcome::Until(i) => tokio::time::sleep_until(tokio::time::Instant::from_std(i)).await,
    }
    t.after_wait(res).await
}

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

/// Type alias for a dynamic [`TimerTask`] trait object.
pub type DynTimerTask<TState, TResourceKey = Cow<'static, str>> =
    dyn TimerTask<TState, TResourceKey> + Send + Sync;

/// Type alias for an `Arc`-wrapped dynamic [`TimerTask`] trait object.
pub type TimerTaskObject<TState, TResourceKey = Cow<'static, str>> =
    std::sync::Arc<DynTimerTask<TState, TResourceKey>>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::task;
    use crate::task::Task;
    use std::sync::Arc;

    // Note: like `poll.rs`, all `#[task::timer]` usages inside the `cano` crate use the
    // trait-impl form (`impl TimerTask<S> for T`), because the inherent form emits
    // `::cano::TimerTask<...>` paths that don't resolve inside this crate. The inherent
    // form is tested in `cano-macros/tests/timer_task_impl.rs` where `::cano` resolves.

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum Step {
        Wait,
        Done,
        Next,
    }

    // (a) Duration timer drives the wait-then-transition sequence.
    struct DurationTimer;

    #[task::timer]
    impl TimerTask<Step> for DurationTimer {
        async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
            Ok(TimerOutcome::Duration(Duration::ZERO))
        }
        async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_timer_task_duration_via_run_timer() {
        let timer = DurationTimer;
        let res = Resources::new();
        let result = run_timer(&timer, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    #[tokio::test]
    async fn test_timer_task_duration_via_task_run() {
        let timer = DurationTimer;
        let res = Resources::new();
        let result = Task::run(&timer, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // (b) Until a past instant — fires immediately (no sleep).
    struct PastInstantTimer;

    #[task::timer]
    impl TimerTask<Step> for PastInstantTimer {
        async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
            let past = Instant::now() - Duration::from_secs(60);
            Ok(TimerOutcome::Until(past))
        }
        async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_timer_task_until_past_instant_fires_immediately() {
        let timer = PastInstantTimer;
        let res = Resources::new();
        let start = Instant::now();
        let result = run_timer(&timer, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "past Until should not sleep"
        );
    }

    // (c) after_wait may return Split.
    struct SplitTimer;

    #[task::timer]
    impl TimerTask<Step> for SplitTimer {
        async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
            Ok(TimerOutcome::Duration(Duration::ZERO))
        }
        async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Split(vec![Step::Wait, Step::Next]))
        }
    }

    #[tokio::test]
    async fn test_timer_task_split() {
        let timer = SplitTimer;
        let res = Resources::new();
        let result = Task::run(&timer, &res).await.unwrap();
        assert_eq!(result, TaskResult::Split(vec![Step::Wait, Step::Next]));
    }

    // (d) Config + name defaults / overrides.
    struct CustomTimer;

    #[task::timer]
    impl TimerTask<Step> for CustomTimer {
        fn name(&self) -> Cow<'static, str> {
            Cow::Borrowed("my-custom-timer")
        }
        async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
            Ok(TimerOutcome::Duration(Duration::ZERO))
        }
        async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[test]
    fn test_timer_task_default_config_is_minimal() {
        let timer = DurationTimer;
        assert_eq!(
            TimerTask::<Step>::config(&timer).retry_mode.max_attempts(),
            1,
            "TimerTask default config must be minimal (no retries)"
        );
    }

    #[test]
    fn test_timer_task_default_name_contains_type_name() {
        let timer = DurationTimer;
        let name = TimerTask::<Step>::name(&timer);
        assert!(
            name.contains("DurationTimer"),
            "default name should contain the type name, got: {name}",
        );
    }

    #[test]
    fn test_timer_task_name_override_forwarded_to_task() {
        let timer = CustomTimer;
        assert_eq!(TimerTask::<Step>::name(&timer), "my-custom-timer");
        assert_eq!(Task::name(&timer), "my-custom-timer");
    }

    // (e) Dynamic dispatch: Arc<dyn Task<S>>.
    #[tokio::test]
    async fn test_timer_task_as_dyn_task() {
        let timer: Arc<dyn Task<Step>> = Arc::new(DurationTimer);
        let res = Resources::new();
        let result = Task::run(timer.as_ref(), &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // (f) Errors from wait / after_wait propagate.
    struct WaitErrorTimer;

    #[task::timer]
    impl TimerTask<Step> for WaitErrorTimer {
        async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
            Err(CanoError::task_execution("wait failed"))
        }
        async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn test_wait_error_propagates() {
        let timer = WaitErrorTimer;
        let res = Resources::new();
        let err = Task::run(&timer, &res).await.unwrap_err();
        assert!(matches!(err, CanoError::TaskExecution(_)));
    }

    struct AfterWaitErrorTimer;

    #[task::timer]
    impl TimerTask<Step> for AfterWaitErrorTimer {
        async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
            Ok(TimerOutcome::Duration(Duration::ZERO))
        }
        async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Err(CanoError::task_execution("after_wait failed"))
        }
    }

    #[tokio::test]
    async fn test_after_wait_error_propagates() {
        let timer = AfterWaitErrorTimer;
        let res = Resources::new();
        let err = Task::run(&timer, &res).await.unwrap_err();
        assert!(matches!(err, CanoError::TaskExecution(_)));
    }

    // (g) run_timer with a dyn TimerTask (?Sized).
    #[tokio::test]
    async fn test_run_timer_dyn_dispatch() {
        let timer: &dyn TimerTask<Step> = &DurationTimer;
        let res = Resources::new();
        let result = run_timer(timer, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }
}
