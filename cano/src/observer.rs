//! # Workflow Observer — lifecycle and failure event hooks
//!
//! [`WorkflowObserver`] lets you subscribe to workflow execution events without
//! touching the engine's [`tracing`](https://docs.rs/tracing) instrumentation.
//! Observers are **additive**: every event site continues to emit its existing
//! `tracing` spans and events regardless of whether observers are attached.
//!
//! ## Synchronous by design
//!
//! Every method is synchronous and defaults to a no-op. This keeps the hook
//! points free of `async` trait-object overhead on the hot path. If an observer
//! needs to do async work (write to a database, push to a remote sink), it
//! should hand the event off to a channel and process it on its own task:
//!
//! ```rust
//! use cano::prelude::*;
//! use std::sync::mpsc::{Sender, channel};
//!
//! struct ChannelObserver {
//!     tx: Sender<String>,
//! }
//!
//! impl WorkflowObserver for ChannelObserver {
//!     fn on_task_failure(&self, task_id: &str, err: &CanoError) {
//!         // Cheap, non-blocking: push and return. Async consumer drains it.
//!         let _ = self.tx.send(format!("{task_id} failed: {err}"));
//!     }
//! }
//!
//! let (tx, _rx) = channel();
//! let observer: std::sync::Arc<dyn WorkflowObserver> =
//!     std::sync::Arc::new(ChannelObserver { tx });
//! # let _ = observer;
//! ```
//!
//! ## Attaching an observer
//!
//! Use [`Workflow::with_observer`](crate::workflow::Workflow::with_observer).
//! It may be called multiple times to register several observers; each event is
//! delivered to every registered observer in registration order. Registration
//! is O(1) and the resulting list is shared by `Arc`, so dispatch never clones
//! observer state.
//!
//! ## Built-in: [`TracingObserver`]
//!
//! With the `tracing` feature enabled, [`TracingObserver`] is a ready-made observer
//! that turns each hook into a [`tracing`](https://docs.rs/tracing) event under the
//! `cano::observer` target. It emits **events, not spans**, and is a *complement* to —
//! not a replacement for — the `tracing` feature's built-in span instrumentation.

use crate::error::CanoError;

/// Subscribe to workflow lifecycle and failure events.
///
/// All methods have a default no-op implementation, so implementors only
/// override the events they care about. See the [module documentation](self)
/// for the synchronous-by-design rationale and a channel-based example.
///
/// Implementors must be `Send + Sync + 'static` because the engine holds them
/// behind `Arc<dyn WorkflowObserver>` and may invoke them from spawned tasks
/// (e.g. parallel split tasks).
pub trait WorkflowObserver: Send + Sync + 'static {
    /// Called each time the FSM enters a state, including the initial state and
    /// any terminal exit state. `state` is the `Debug` rendering of the state value.
    fn on_state_enter(&self, _state: &str) {}

    /// Called immediately before a task begins executing (before the retry loop).
    /// `task_id` is the task's [`name`](crate::task::Task::name); for parallel
    /// split tasks it is suffixed with `[index]`.
    fn on_task_start(&self, _task_id: &str) {}

    /// Called when a task completes successfully (after any retries).
    fn on_task_success(&self, _task_id: &str) {}

    /// Called when a task ultimately fails — after exhausting retries, on a
    /// per-attempt timeout that isn't retried, on a panic, or when the circuit
    /// breaker short-circuits the call.
    fn on_task_failure(&self, _task_id: &str, _err: &CanoError) {}

    /// Called before each retry attempt. `attempt` is the 1-based number of the
    /// attempt that just failed (i.e. the retry that follows is attempt `attempt + 1`).
    fn on_retry(&self, _task_id: &str, _attempt: u32) {}

    /// Called when a [`CircuitBreaker`](crate::circuit::CircuitBreaker) attached
    /// via [`TaskConfig::with_circuit_breaker`](crate::task::TaskConfig::with_circuit_breaker)
    /// rejects the call because it is open. Followed by an
    /// [`on_task_failure`](Self::on_task_failure) with a [`CanoError::CircuitOpen`].
    fn on_circuit_open(&self, _task_id: &str) {}

    /// Called after a [`CheckpointRow`](crate::recovery::CheckpointRow) is durably
    /// appended for `workflow_id` at `sequence`. Fires once per state entry on a
    /// workflow configured with [`Workflow::with_checkpoint_store`](crate::workflow::Workflow::with_checkpoint_store);
    /// never fires when no checkpoint store is attached.
    fn on_checkpoint(&self, _workflow_id: &str, _sequence: u64) {}

    /// Called once at the start of [`Workflow::resume_from`](crate::workflow::Workflow::resume_from),
    /// after the run has been rehydrated. `sequence` is the sequence of the last
    /// persisted row; execution continues from the state recorded by that row.
    fn on_resume(&self, _workflow_id: &str, _sequence: u64) {}
}

/// A [`WorkflowObserver`] that re-emits every event as a [`tracing`](https://docs.rs/tracing)
/// event (requires the `tracing` feature).
///
/// This is a convenience for getting workflow lifecycle/failure signals into your
/// `tracing` subscriber without writing an observer by hand. It emits **flat events,
/// not spans** — it does not (and cannot) reproduce the nested span tree the `tracing`
/// feature's built-in instrumentation creates (`workflow_orchestrate` → `single_task_execution`
/// → `task_attempt`, etc.). Use it *alongside* that instrumentation, or on its own when you
/// only want the high-level events.
///
/// All events are emitted under the `cano::observer` target, so they can be filtered
/// independently — e.g. `RUST_LOG=cano::observer=debug`. Event levels mirror the engine's
/// existing instrumentation for the analogous sites:
///
/// | hook | level | message | fields |
/// |---|---|---|---|
/// | [`on_state_enter`](WorkflowObserver::on_state_enter) | `DEBUG` | `"workflow entered state"` | `state` |
/// | [`on_task_start`](WorkflowObserver::on_task_start) | `DEBUG` | `"task started"` | `task_id` |
/// | [`on_task_success`](WorkflowObserver::on_task_success) | `INFO` | `"task succeeded"` | `task_id` |
/// | [`on_task_failure`](WorkflowObserver::on_task_failure) | `ERROR` | `"task failed"` | `task_id`, `error` |
/// | [`on_retry`](WorkflowObserver::on_retry) | `WARN` | `"task retry"` | `task_id`, `attempt` |
/// | [`on_circuit_open`](WorkflowObserver::on_circuit_open) | `WARN` | `"circuit breaker rejected task"` | `task_id` |
/// | [`on_checkpoint`](WorkflowObserver::on_checkpoint) | `DEBUG` | `"checkpoint appended"` | `workflow_id`, `sequence` |
/// | [`on_resume`](WorkflowObserver::on_resume) | `INFO` | `"workflow resumed from checkpoint"` | `workflow_id`, `sequence` |
///
/// # Example
///
/// ```rust
/// # use cano::prelude::*;
/// # use std::sync::Arc;
/// # #[derive(Clone, Debug, PartialEq, Eq, Hash)] enum Step { Start, Done }
/// # #[derive(Clone)] struct Work;
/// # #[task(state = Step)]
/// # impl Work {
/// #     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> { Ok(TaskResult::Single(Step::Done)) }
/// # }
/// let workflow = Workflow::bare()
///     .register(Step::Start, Work)
///     .add_exit_state(Step::Done)
///     .with_observer(Arc::new(TracingObserver::new()));
/// # let _ = workflow;
/// ```
#[cfg(feature = "tracing")]
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingObserver;

#[cfg(feature = "tracing")]
impl TracingObserver {
    /// Create a `TracingObserver`. Wrap it in an `Arc` and pass it to
    /// [`Workflow::with_observer`](crate::workflow::Workflow::with_observer).
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "tracing")]
impl WorkflowObserver for TracingObserver {
    fn on_state_enter(&self, state: &str) {
        tracing::debug!(state, "workflow entered state");
    }
    fn on_task_start(&self, task_id: &str) {
        tracing::debug!(task_id, "task started");
    }
    fn on_task_success(&self, task_id: &str) {
        tracing::info!(task_id, "task succeeded");
    }
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        tracing::error!(task_id, error = %err, "task failed");
    }
    fn on_retry(&self, task_id: &str, attempt: u32) {
        tracing::warn!(task_id, attempt, "task retry");
    }
    fn on_circuit_open(&self, task_id: &str) {
        tracing::warn!(task_id, "circuit breaker rejected task");
    }
    fn on_checkpoint(&self, workflow_id: &str, sequence: u64) {
        tracing::debug!(workflow_id, sequence, "checkpoint appended");
    }
    fn on_resume(&self, workflow_id: &str, sequence: u64) {
        tracing::info!(workflow_id, sequence, "workflow resumed from checkpoint");
    }
}

/// A [`WorkflowObserver`] that re-emits each hook as a metric (counter), under the
/// `cano_*` namespace. Attach with `Workflow::with_observer(Arc::new(MetricsObserver::new()))`
/// — the metrics it emits then require this observer to be attached, exactly as `tracing`
/// events from [`TracingObserver`] require attaching that observer.
///
/// Requires the `metrics` feature. Complements (does not replace) the always-on
/// `#[cfg(feature = "metrics")]` direct instrumentation elsewhere in the engine (workflow run
/// duration, circuit internals, scheduler flows, …) — see [`crate::metrics`].
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Default)]
pub struct MetricsObserver;

#[cfg(feature = "metrics")]
impl MetricsObserver {
    /// Construct a `MetricsObserver`.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "metrics")]
impl WorkflowObserver for MetricsObserver {
    fn on_state_enter(&self, state: &str) {
        crate::metrics::observed_state_enter(state);
    }
    fn on_task_success(&self, task_id: &str) {
        crate::metrics::observed_task_run(task_id, true);
    }
    fn on_task_failure(&self, task_id: &str, _err: &CanoError) {
        crate::metrics::observed_task_run(task_id, false);
    }
    fn on_retry(&self, task_id: &str, _attempt: u32) {
        crate::metrics::observed_task_retry(task_id);
    }
    fn on_circuit_open(&self, task_id: &str) {
        crate::metrics::observed_circuit_open(task_id);
    }
    fn on_checkpoint(&self, _workflow_id: &str, _sequence: u64) {
        crate::metrics::observed_checkpoint();
    }
    fn on_resume(&self, _workflow_id: &str, _sequence: u64) {
        crate::metrics::observed_resume();
    }
    // on_task_start: intentionally not emitted — on_task_success/on_task_failure already
    // count each dispatch by outcome; a separate "start" counter would just be their sum.
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use std::borrow::Cow;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Test observer that appends a stringified record of every event it receives.
    #[derive(Default)]
    struct RecordingObserver {
        events: Mutex<Vec<String>>,
    }

    impl RecordingObserver {
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
        fn record(&self, event: String) {
            self.events.lock().unwrap().push(event);
        }
    }

    impl WorkflowObserver for RecordingObserver {
        fn on_state_enter(&self, state: &str) {
            self.record(format!("state_enter:{state}"));
        }
        fn on_task_start(&self, task_id: &str) {
            self.record(format!("task_start:{task_id}"));
        }
        fn on_task_success(&self, task_id: &str) {
            self.record(format!("task_success:{task_id}"));
        }
        fn on_task_failure(&self, task_id: &str, _err: &CanoError) {
            self.record(format!("task_failure:{task_id}"));
        }
        fn on_retry(&self, task_id: &str, attempt: u32) {
            self.record(format!("retry:{task_id}:{attempt}"));
        }
        fn on_circuit_open(&self, task_id: &str) {
            self.record(format!("circuit_open:{task_id}"));
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum S {
        Start,
        Done,
    }

    struct OkTask;
    #[task]
    impl Task<S> for OkTask {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(S::Done))
        }
        fn name(&self) -> Cow<'static, str> {
            "OkTask".into()
        }
    }

    struct FailTask;
    #[task]
    impl Task<S> for FailTask {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Err(CanoError::task_execution("boom"))
        }
        fn config(&self) -> TaskConfig {
            TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1))
        }
        fn name(&self) -> Cow<'static, str> {
            "FailTask".into()
        }
    }

    struct CircuitTask {
        breaker: Arc<CircuitBreaker>,
    }
    #[task]
    impl Task<S> for CircuitTask {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(S::Done))
        }
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal().with_circuit_breaker(Arc::clone(&self.breaker))
        }
        fn name(&self) -> Cow<'static, str> {
            "CircuitTask".into()
        }
    }

    fn position(events: &[String], needle: &str) -> usize {
        events
            .iter()
            .position(|e| e == needle)
            .unwrap_or_else(|| panic!("event {needle:?} not found in {events:?}"))
    }

    #[tokio::test]
    async fn observer_fires_on_success_path() {
        let observer = Arc::new(RecordingObserver::default());
        let workflow = Workflow::bare()
            .register(S::Start, OkTask)
            .add_exit_state(S::Done)
            .with_observer(observer.clone());

        assert_eq!(workflow.orchestrate(S::Start).await.unwrap(), S::Done);

        let events = observer.events();
        assert!(
            events.contains(&"state_enter:Start".to_string()),
            "{events:?}"
        );
        assert!(
            events.contains(&"task_start:OkTask".to_string()),
            "{events:?}"
        );
        assert!(
            events.contains(&"task_success:OkTask".to_string()),
            "{events:?}"
        );
        assert!(
            events.contains(&"state_enter:Done".to_string()),
            "{events:?}"
        );
        // Ordering: enter Start -> start task -> task success -> enter Done.
        assert!(position(&events, "state_enter:Start") < position(&events, "task_start:OkTask"));
        assert!(position(&events, "task_start:OkTask") < position(&events, "task_success:OkTask"));
        assert!(position(&events, "task_success:OkTask") < position(&events, "state_enter:Done"));
    }

    #[tokio::test]
    async fn observer_fires_on_retry_and_failure() {
        let observer = Arc::new(RecordingObserver::default());
        let workflow = Workflow::bare()
            .register(S::Start, FailTask)
            .add_exit_state(S::Done)
            .with_observer(observer.clone());

        assert!(workflow.orchestrate(S::Start).await.is_err());

        let events = observer.events();
        assert!(
            events.contains(&"task_start:FailTask".to_string()),
            "{events:?}"
        );
        // Fixed(2) -> 1 initial attempt + 2 retries; on_retry fires after attempt 1 and 2.
        assert!(
            events.contains(&"retry:FailTask:1".to_string()),
            "{events:?}"
        );
        assert!(
            events.contains(&"retry:FailTask:2".to_string()),
            "{events:?}"
        );
        assert!(
            events.contains(&"task_failure:FailTask".to_string()),
            "{events:?}"
        );
        assert!(
            !events.contains(&"task_success:FailTask".to_string()),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn observer_fires_on_circuit_open() {
        // Trip the breaker before the workflow runs: one failure at threshold 1.
        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 1,
            reset_timeout: Duration::from_secs(60),
            ..CircuitPolicy::default()
        }));
        let permit = breaker.try_acquire().expect("closed breaker admits");
        breaker.record_failure(permit);

        let observer = Arc::new(RecordingObserver::default());
        let workflow = Workflow::bare()
            .register(
                S::Start,
                CircuitTask {
                    breaker: Arc::clone(&breaker),
                },
            )
            .add_exit_state(S::Done)
            .with_observer(observer.clone());

        let err = workflow.orchestrate(S::Start).await.unwrap_err();
        assert!(matches!(err, CanoError::CircuitOpen(_)), "{err}");

        let events = observer.events();
        assert!(
            events.contains(&"circuit_open:CircuitTask".to_string()),
            "{events:?}"
        );
        assert!(
            events.contains(&"task_failure:CircuitTask".to_string()),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn no_observer_is_zero_overhead_and_still_runs() {
        // Sanity: a workflow without observers behaves exactly as before.
        let workflow = Workflow::bare()
            .register(S::Start, OkTask)
            .add_exit_state(S::Done);
        assert_eq!(workflow.orchestrate(S::Start).await.unwrap(), S::Done);
    }

    #[test]
    fn default_task_name_is_type_name() {
        // The default `Task::name` returns `std::any::type_name::<Self>()`.
        struct Anon;
        #[task]
        impl Task<S> for Anon {
            async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
                Ok(TaskResult::Single(S::Done))
            }
        }
        assert!(Anon.name().contains("Anon"), "{}", Anon.name());
    }
}

#[cfg(all(test, feature = "metrics"))]
mod metrics_observer_tests {
    use super::*;
    use crate::metrics::test_support::*;
    use crate::prelude::*;
    use std::sync::Arc;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        Start,
        Mid,
        Done,
    }

    struct GoTo(S);
    #[crate::task]
    impl Task<S> for GoTo {
        fn name(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed("GoTo")
        }
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(self.0.clone()))
        }
    }

    #[test]
    fn metrics_observer_emits_state_enters_and_task_runs() {
        let (res, rows) = run_with_recorder(|| async {
            Workflow::bare()
                .with_observer(Arc::new(MetricsObserver::new()))
                .register(S::Start, GoTo(S::Mid))
                .register(S::Mid, GoTo(S::Done))
                .add_exit_state(S::Done)
                .orchestrate(S::Start)
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(
            counter(&rows, "cano_state_enters_total", &[("state", "Start")]),
            1
        );
        assert_eq!(
            counter(&rows, "cano_state_enters_total", &[("state", "Mid")]),
            1
        );
        assert_eq!(
            counter(&rows, "cano_state_enters_total", &[("state", "Done")]),
            1
        );
        assert_eq!(
            counter(
                &rows,
                "cano_observed_task_runs_total",
                &[("task", "GoTo"), ("outcome", "completed")]
            ),
            2
        );
    }

    #[test]
    fn metrics_observer_emits_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = Arc::clone(&n);
        struct Flaky(Arc<AtomicUsize>);
        #[crate::task]
        impl Task<S> for Flaky {
            fn name(&self) -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed("Flaky")
            }
            fn config(&self) -> TaskConfig {
                TaskConfig::new().with_fixed_retry(3, std::time::Duration::from_millis(1))
            }
            async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
                if self.0.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(CanoError::task_execution("transient"))
                } else {
                    Ok(TaskResult::Single(S::Done))
                }
            }
        }
        let (res, rows) = run_with_recorder(move || async move {
            Workflow::bare()
                .with_observer(Arc::new(MetricsObserver::new()))
                .register(S::Start, Flaky(n2))
                .add_exit_state(S::Done)
                .orchestrate(S::Start)
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(
            counter(&rows, "cano_task_retries_total", &[("task", "Flaky")]),
            2
        );
        assert_eq!(
            counter(
                &rows,
                "cano_observed_task_runs_total",
                &[("task", "Flaky"), ("outcome", "completed")]
            ),
            1
        );
    }
}
