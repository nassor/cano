//! # PollTask — Wait-Until Processing Model
//!
//! A [`PollTask`] repeatedly calls `poll()` until it returns [`Poll::Ready`] or until an
//! optional bound (maximum number of polls or a wall-clock timeout) is exceeded. This
//! "wait-until" pattern is the canonical way to poll an external system for completion
//! without blocking a thread.
//!
//! ## When to use `PollTask`
//!
//! - **Waiting for external jobs**: poll a job queue or long-running API until it finishes.
//! - **Adaptive backoff without retries**: the loop *is* the resilience mechanism, so the
//!   default `TaskConfig` is [`TaskConfig::minimal()`] (no outer retry wrapping).
//! - **Condition checks**: loop until a database row exists, a flag flips, etc.
//!
//! Every [`PollTask`] automatically implements [`Task`](crate::task::Task) via a
//! per-impl-site companion `impl Task<S> for T` emitted by the `#[poll_task]` macro.
//! This means you can register a `PollTask` with
//! [`Workflow::register`](crate::workflow::Workflow::register) exactly like any other task.
//!
//! ## Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//! use std::sync::atomic::{AtomicU32, Ordering};
//! use std::sync::Arc;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Wait, Done }
//!
//! struct Counter(Arc<AtomicU32>);
//!
//! #[poll_task(state = Step)]
//! impl Counter {
//!     async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
//!         let n = self.0.fetch_add(1, Ordering::Relaxed);
//!         if n >= 2 {
//!             Ok(Poll::Ready(TaskResult::Single(Step::Done)))
//!         } else {
//!             Ok(Poll::Pending { delay_ms: 0 })
//!         }
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let counter = Counter(Arc::new(AtomicU32::new(0)));
//! let workflow = Workflow::bare()
//!     .register(Step::Wait, counter)
//!     .add_exit_state(Step::Done);
//!
//! let result = workflow.orchestrate(Step::Wait).await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```
//!
//! ## Limiting polls
//!
//! Override [`PollTask::config`] to pass a [`PollConfig`] via [`TaskConfig`], or use
//! [`PollErrorPolicy`] fields on the config to limit how long the loop runs:
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Wait, Done }
//!
//! struct BoundedPoller;
//!
//! #[poll_task(state = Step)]
//! impl BoundedPoller {
//!     fn config(&self) -> TaskConfig { TaskConfig::minimal() }
//!
//!     async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
//!         Ok(Poll::Pending { delay_ms: 10 })
//!     }
//! }
//! ```
//!
//! ## The trait-impl form
//!
//! You can also write the full `impl PollTask<S> for T` header explicitly:
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Poll, Done }
//!
//! struct TraitPoller;
//!
//! #[poll_task]
//! impl PollTask<Step> for TraitPoller {
//!     async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
//!         Ok(Poll::Ready(TaskResult::Single(Step::Done)))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let workflow = Workflow::bare()
//!     .register(Step::Poll, TraitPoller)
//!     .add_exit_state(Step::Done);
//!
//! let result = workflow.orchestrate(Step::Poll).await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{TaskConfig, TaskResult};
use cano_macros::poll_task;
use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;

// ---------------------------------------------------------------------------
// Poll<TState> — the outcome of a single poll() call
// ---------------------------------------------------------------------------

/// The outcome returned by a single [`PollTask::poll`] call.
///
/// - [`Poll::Ready`] — the condition is satisfied; carry the `TaskResult` forward to the FSM.
/// - [`Poll::Pending`] — not yet ready; sleep for `delay_ms` milliseconds then poll again.
///
/// This mirrors `std::task::Poll` conceptually but carries workflow-specific data and an
/// explicit sleep duration to decouple the poller from async executor wakeup mechanics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll<TState> {
    /// The poll condition is satisfied. The inner [`TaskResult`] is forwarded to the FSM.
    Ready(TaskResult<TState>),
    /// Not yet ready. Sleep `delay_ms` milliseconds before the next poll.
    Pending {
        /// Milliseconds to sleep before the next poll call. Use `0` for no delay.
        delay_ms: u64,
    },
}

// ---------------------------------------------------------------------------
// PollErrorPolicy — what to do when poll() returns Err
// ---------------------------------------------------------------------------

/// Controls how the poll loop responds when [`PollTask::poll`] returns an [`Err`].
///
/// Attach to [`TaskConfig`] by returning a custom [`TaskConfig`] from
/// [`PollTask::config`], or by using [`PollConfig`] to wrap the behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PollErrorPolicy {
    /// Propagate the error immediately — the loop stops and the error is returned.
    #[default]
    FailFast,
    /// Log and retry up to `max_errors` consecutive errors before failing.
    RetryOnError {
        /// Maximum number of consecutive errors before the loop fails.
        max_errors: u32,
    },
}

// ---------------------------------------------------------------------------
// PollTask trait
// ---------------------------------------------------------------------------

/// A "wait-until" processing model that repeatedly calls [`poll`](PollTask::poll) until
/// it returns [`Poll::Ready`].
///
/// # Generic Types
///
/// - **`TState`**: The state enum used by the workflow (`Clone + Debug + Send + Sync`).
/// - **`TResourceKey`**: The key type used to look up resources (defaults to
///   [`Cow<'static, str>`]).
///
/// # Default config
///
/// Unlike [`RouterTask`](crate::task::router::RouterTask), `PollTask` defaults to
/// [`TaskConfig::minimal()`] (no retries). The poll loop itself is the resilience
/// mechanism; outer retry wrapping rarely makes sense here.
///
/// # Implementing PollTask
///
/// Prefer the inherent `#[poll_task(state = S)]` form:
///
/// ```rust
/// use cano::prelude::*;
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// enum Step { Wait, Done }
///
/// struct MyPoller;
///
/// #[poll_task(state = Step)]
/// impl MyPoller {
///     async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
///         Ok(Poll::Ready(TaskResult::Single(Step::Done)))
///     }
/// }
/// ```
#[poll_task]
pub trait PollTask<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Get the task configuration that controls execution behaviour.
    ///
    /// Defaults to [`TaskConfig::minimal()`] (no retries, no timeout). Override to
    /// attach a circuit-breaker or per-attempt timeout if needed.
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    /// Human-readable identifier for this poller, reported to [`WorkflowObserver`] hooks.
    ///
    /// Defaults to [`std::any::type_name`] of the implementing type. Override for a
    /// stable, friendly name.
    ///
    /// [`WorkflowObserver`]: crate::observer::WorkflowObserver
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Call once per poll iteration.
    ///
    /// Return [`Poll::Ready`] with the next [`TaskResult`] when the condition is met,
    /// or [`Poll::Pending`] with a sleep duration when it is not yet ready.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError`] on a non-recoverable error. By default the loop stops
    /// immediately (`PollErrorPolicy::FailFast`).
    async fn poll(&self, res: &Resources<TResourceKey>) -> Result<Poll<TState>, CanoError>;
}

// ---------------------------------------------------------------------------
// run_poll_loop — the loop body called by the macro-synthesised Task::run
// ---------------------------------------------------------------------------

/// Run the [`PollTask`] poll loop for `p`.
///
/// The synthesised `Task::run` method emitted by `#[poll_task]` delegates here so
/// that the loop body (which needs `tokio::time::sleep`) lives in a single place that
/// can reference `tokio` directly.
///
/// The loop calls `p.poll(res)` repeatedly:
///
/// - [`Poll::Ready(result)`] → return `Ok(result)`.
/// - [`Poll::Pending { delay_ms }`] → sleep `delay_ms` ms, then poll again.
/// - `Err(e)` → return `Err(e)` immediately.
///
/// There is no built-in iteration limit. Use [`TaskConfig::attempt_timeout`] (via
/// `PollTask::config`) to cap total wall-clock time.
pub async fn run_poll_loop<P, S, K>(p: &P, res: &Resources<K>) -> Result<TaskResult<S>, CanoError>
where
    P: PollTask<S, K> + ?Sized,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    loop {
        match p.poll(res).await? {
            Poll::Ready(result) => return Ok(result),
            Poll::Pending { delay_ms } => {
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

/// Type alias for a dynamic [`PollTask`] trait object.
pub type DynPollTask<TState, TResourceKey = Cow<'static, str>> =
    dyn PollTask<TState, TResourceKey> + Send + Sync;

/// Type alias for an `Arc`-wrapped dynamic [`PollTask`] trait object.
pub type PollTaskObject<TState, TResourceKey = Cow<'static, str>> =
    std::sync::Arc<DynPollTask<TState, TResourceKey>>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::task::Task;
    use cano_macros::{poll_task, task};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    // Note: all `#[poll_task]` usages inside the `cano` crate itself use the
    // trait-impl form (`impl PollTask<S> for T`), because the inherent form emits
    // `::cano::PollTask<...>` paths that don't resolve inside this crate. The
    // inherent form is tested in `cano-macros/tests/poll_task_impl.rs` where
    // `::cano` resolves correctly.

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum Step {
        Wait,
        Done,
        Next,
    }

    // ---------------------------------------------------------------------------
    // (a) PollTask that becomes Ready on the first call
    // ---------------------------------------------------------------------------

    struct ImmediatePoller;

    #[poll_task]
    impl PollTask<Step> for ImmediatePoller {
        async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
            Ok(Poll::Ready(TaskResult::Single(Step::Done)))
        }
    }

    #[tokio::test]
    async fn test_poll_task_immediate_via_poll() {
        let poller = ImmediatePoller;
        let res = Resources::new();
        let result = PollTask::poll(&poller, &res).await.unwrap();
        assert_eq!(result, Poll::Ready(TaskResult::Single(Step::Done)));
    }

    #[tokio::test]
    async fn test_poll_task_immediate_via_task_run() {
        let poller = ImmediatePoller;
        let res = Resources::new();
        let result = Task::run(&poller, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // ---------------------------------------------------------------------------
    // (b) PollTask that takes multiple polls before becoming Ready
    // ---------------------------------------------------------------------------

    struct CountingPoller {
        target: u32,
        count: AtomicU32,
    }

    impl CountingPoller {
        fn new(target: u32) -> Self {
            Self {
                target,
                count: AtomicU32::new(0),
            }
        }
    }

    #[poll_task]
    impl PollTask<Step> for CountingPoller {
        async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
            let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= self.target {
                Ok(Poll::Ready(TaskResult::Single(Step::Done)))
            } else {
                Ok(Poll::Pending { delay_ms: 0 })
            }
        }
    }

    #[tokio::test]
    async fn test_poll_task_multiple_polls() {
        let poller = CountingPoller::new(3);
        let res = Resources::new();
        let result = Task::run(&poller, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
        assert_eq!(poller.count.load(Ordering::Relaxed), 3);
    }

    // ---------------------------------------------------------------------------
    // (c) Poll error propagates immediately
    // ---------------------------------------------------------------------------

    struct ErrorPoller;

    #[poll_task]
    impl PollTask<Step> for ErrorPoller {
        async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
            Err(CanoError::task_execution("poll failed"))
        }
    }

    #[tokio::test]
    async fn test_poll_task_error_propagates() {
        let poller = ErrorPoller;
        let res = Resources::new();
        let err = Task::run(&poller, &res).await.unwrap_err();
        assert!(matches!(err, CanoError::TaskExecution(_)));
    }

    // ---------------------------------------------------------------------------
    // (d) PollTask returning Split
    // ---------------------------------------------------------------------------

    struct SplitPoller;

    #[poll_task]
    impl PollTask<Step> for SplitPoller {
        async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
            Ok(Poll::Ready(TaskResult::Split(vec![Step::Wait, Step::Next])))
        }
    }

    #[tokio::test]
    async fn test_poll_task_split() {
        let poller = SplitPoller;
        let res = Resources::new();
        let result = Task::run(&poller, &res).await.unwrap();
        assert_eq!(result, TaskResult::Split(vec![Step::Wait, Step::Next]));
    }

    // ---------------------------------------------------------------------------
    // (e) Config + name overrides
    // ---------------------------------------------------------------------------

    struct CustomPoller;

    #[poll_task]
    impl PollTask<Step> for CustomPoller {
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        fn name(&self) -> Cow<'static, str> {
            Cow::Borrowed("my-custom-poller")
        }
        async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
            Ok(Poll::Ready(TaskResult::Single(Step::Done)))
        }
    }

    #[test]
    fn test_poll_task_config_override() {
        let poller = CustomPoller;
        assert_eq!(
            PollTask::<Step>::config(&poller).retry_mode.max_attempts(),
            1
        );
    }

    #[test]
    fn test_poll_task_name_override() {
        let poller = CustomPoller;
        assert_eq!(PollTask::<Step>::name(&poller), "my-custom-poller");
    }

    #[test]
    fn test_companion_task_forwards_config_and_name() {
        let poller = CustomPoller;
        assert_eq!(Task::config(&poller).retry_mode.max_attempts(), 1);
        assert_eq!(Task::name(&poller), "my-custom-poller");
    }

    // ---------------------------------------------------------------------------
    // (f) Default config is minimal (no retries)
    // ---------------------------------------------------------------------------

    struct DefaultConfigPoller;

    #[poll_task]
    impl PollTask<Step> for DefaultConfigPoller {
        async fn poll(&self, _res: &Resources) -> Result<Poll<Step>, CanoError> {
            Ok(Poll::Ready(TaskResult::Single(Step::Done)))
        }
    }

    #[test]
    fn test_poll_task_default_config_is_minimal() {
        let poller = DefaultConfigPoller;
        assert_eq!(
            PollTask::<Step>::config(&poller).retry_mode.max_attempts(),
            1,
            "PollTask default config must be minimal (no retries)"
        );
    }

    #[test]
    fn test_poll_task_default_name_contains_type_name() {
        let poller = DefaultConfigPoller;
        let name = PollTask::<Step>::name(&poller);
        assert!(
            name.contains("DefaultConfigPoller"),
            "default name should contain the type name, got: {name}",
        );
    }

    // ---------------------------------------------------------------------------
    // (g) Dynamic dispatch: Arc<dyn Task<S>>
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_poll_task_as_dyn_task() {
        let poller: Arc<dyn Task<Step>> = Arc::new(ImmediatePoller);
        let res = Resources::new();
        let result = Task::run(poller.as_ref(), &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    // ---------------------------------------------------------------------------
    // (h) Workflow integration
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_poll_task_in_workflow() {
        use crate::workflow::Workflow;

        struct NextTask;

        #[task]
        impl Task<Step> for NextTask {
            async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
                Ok(TaskResult::Single(Step::Done))
            }
        }

        let poller = CountingPoller::new(2);

        let workflow = Workflow::bare()
            .register(Step::Wait, poller)
            .register(Step::Next, NextTask)
            .add_exit_state(Step::Done);

        // CountingPoller::new(2) -> first poll returns Pending, second returns Ready(Next)
        // But wait: poll 1 => count becomes 1, 1 < 2 => Pending; poll 2 => count becomes 2, 2 >= 2 => Ready(Done)
        // But we registered Step::Done as exit state so Done is the final state
        // Actually CountingPoller returns Single(Step::Done) when ready, so we skip Next entirely
        let result = workflow.orchestrate(Step::Wait).await.unwrap();
        assert_eq!(result, Step::Done);
    }

    // ---------------------------------------------------------------------------
    // (i) run_poll_loop with dyn PollTask (?Sized)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_run_poll_loop_dyn_dispatch() {
        let poller: &dyn PollTask<Step> = &ImmediatePoller;
        let res = Resources::new();
        let result = run_poll_loop(poller, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }
}
