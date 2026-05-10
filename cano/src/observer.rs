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
