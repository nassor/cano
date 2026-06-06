//! # Testing helpers — batteries-included fixtures for workflow tests
//!
//! Everything in this module is gated behind the `testing` feature and is meant
//! to be pulled in wholesale from a test module:
//!
//! ```rust
//! use cano::testing::*;
//! ```
//!
//! It wraps existing public surface ([`WorkflowObserver`],
//! [`CheckpointStore`], [`Resources`],
//! [`MemoryStore`], [`Task`]) — it adds **no** new
//! traits and changes **no** existing public items, so a typical workflow test drops from a
//! pile of bespoke fixtures to a handful of lines.
//!
//! | helper | what it gives you |
//! |---|---|
//! | [`RecordingObserver`] | a [`WorkflowObserver`] that captures every lifecycle event into an inspectable [`Vec<RecordedEvent>`] |
//! | [`InMemoryCheckpointStore`] | a process-local [`CheckpointStore`] for resume/recovery tests, with no `recovery` feature or on-disk file needed |
//! | [`TestResources`] | a tiny builder for the [`Resources`] dictionary a workflow needs |
//! | [`panic_on_attempt`] | a [`Task`] that panics on its first *N* attempts — exercises the panic-safety path (panics fail fast, they are not retried) |
//! | [`assert_compensation_ran`] | a saga assertion that compares the recorded compensation order against an expected sequence |
//!
//! This module is **not** in the [`prelude`](crate::prelude) — import it explicitly with
//! `use cano::testing::*;` so production code never picks these up by accident.
//!
//! ## Example
//!
//! ```rust
//! use cano::prelude::*;
//! use cano::testing::*;
//! use std::sync::Arc;
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! enum S { Start, Done }
//!
//! struct OkTask;
//! #[task]
//! impl Task<S> for OkTask {
//!     async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
//!         Ok(TaskResult::Single(S::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() {
//! let observer = Arc::new(RecordingObserver::new());
//! let wf = Workflow::bare()
//!     .register(S::Start, OkTask)
//!     .add_exit_state(S::Done)
//!     .with_observer(observer.clone());
//! assert_eq!(wf.orchestrate(S::Start).await.unwrap(), S::Done);
//! observer.assert_path(&["Start", "Done"]);
//! # }
//! ```

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;

use crate::error::CanoError;
use crate::observer::WorkflowObserver;
use crate::recovery::{CheckpointRow, CheckpointStore};
use crate::resource::{Resource, Resources};
use crate::store::MemoryStore;
use crate::task::{Task, TaskConfig, TaskResult};

/// A single event captured by [`RecordingObserver`].
///
/// Mirrors the [`WorkflowObserver`] hooks. It is
/// `#[non_exhaustive]` because the observer trait may grow more hooks — match with a
/// wildcard arm.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedEvent {
    /// The FSM entered a state. Carries the `Debug` rendering of the state.
    StateEntered(String),
    /// A task began executing (before its retry loop).
    TaskStarted {
        /// The task's [`name`](crate::task::Task::name).
        state: String,
    },
    /// A task completed successfully.
    TaskSucceeded {
        /// The task's [`name`](crate::task::Task::name).
        state: String,
    },
    /// A task ultimately failed.
    TaskFailed {
        /// The task's [`name`](crate::task::Task::name).
        state: String,
        /// The failing error's `Display` rendering.
        error: String,
    },
    /// A retry was scheduled after a failed attempt.
    Retry {
        /// The task's [`name`](crate::task::Task::name).
        state: String,
        /// The 1-based number of the attempt that just failed.
        attempt: u32,
    },
    /// A circuit breaker rejected the call.
    CircuitOpen {
        /// The task's [`name`](crate::task::Task::name).
        state: String,
    },
    /// A checkpoint row was durably appended.
    Checkpoint {
        /// The workflow id the row was written for.
        workflow_id: String,
        /// The appended row's sequence.
        sequence: u64,
    },
    /// A run was resumed from a checkpoint log.
    Resume {
        /// The resumed workflow id.
        workflow_id: String,
        /// The sequence of the last persisted row.
        sequence: u64,
    },
    /// A run was cancelled via a [`CancellationToken`](crate::cancel::CancellationToken).
    Cancelled {
        /// The `Debug` rendering of the state cancellation was observed at.
        state: String,
    },
}

/// A [`WorkflowObserver`] that records every event it
/// receives into an inspectable list.
///
/// Wrap it in an `Arc`, attach it with
/// [`Workflow::with_observer`](crate::workflow::Workflow::with_observer), drive the
/// workflow, then assert against [`events`](Self::events) / [`states_entered`](Self::states_entered)
/// or use the [`assert_path`](Self::assert_path) / [`assert_completed_with`](Self::assert_completed_with)
/// convenience checks.
#[derive(Default, Debug)]
pub struct RecordingObserver {
    events: Mutex<Vec<RecordedEvent>>,
}

impl RecordingObserver {
    /// Create an empty `RecordingObserver`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot every recorded event, in arrival order.
    #[must_use]
    pub fn events(&self) -> Vec<RecordedEvent> {
        self.events.lock().clone()
    }

    /// The `Debug`-rendered state labels, in the order they were entered.
    #[must_use]
    pub fn states_entered(&self) -> Vec<String> {
        self.events
            .lock()
            .iter()
            .filter_map(|e| match e {
                RecordedEvent::StateEntered(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// Drop every recorded event. Useful to reset between phases of a test that
    /// reuses one observer.
    pub fn clear(&self) {
        self.events.lock().clear();
    }

    /// Assert that the last state the FSM entered is `final_state`.
    ///
    /// # Panics
    ///
    /// Panics if no states were entered, or if the last differs from `final_state`.
    pub fn assert_completed_with(&self, final_state: &str) {
        let last = self.states_entered().last().cloned();
        assert_eq!(last.as_deref(), Some(final_state));
    }

    /// Assert the full sequence of states entered equals `expected`.
    ///
    /// # Panics
    ///
    /// Panics if the recorded path differs from `expected`.
    pub fn assert_path(&self, expected: &[&str]) {
        let actual = self.states_entered();
        let actual_refs: Vec<&str> = actual.iter().map(String::as_str).collect();
        assert_eq!(actual_refs.as_slice(), expected);
    }

    /// Assert that every state in `expected` was entered at least once during the
    /// workflow run this observer recorded. Returns `Err` listing the missing
    /// states (in the order they appear in `expected`, deduplicated) when one or
    /// more were never entered; `Ok(())` when all were visited.
    ///
    /// Unlike [`assert_path`](Self::assert_path) this returns a `Result` instead of
    /// panicking, so the caller can inspect the missing labels — `?`-propagate, or
    /// `.expect(..)` / `.unwrap_err()` in a test.
    ///
    /// The comparison is string-based on the `Debug` rendering of each state,
    /// matching what [`WorkflowObserver::on_state_enter`](crate::observer::WorkflowObserver::on_state_enter)
    /// received from the engine. `TState` therefore only needs `Debug`.
    pub fn assert_all_states_entered<TState: std::fmt::Debug>(
        &self,
        expected: &[TState],
    ) -> Result<(), Vec<String>> {
        let entered: std::collections::HashSet<String> =
            self.states_entered().into_iter().collect();

        let mut missing: Vec<String> = Vec::new();
        let mut seen_in_input: std::collections::HashSet<String> = std::collections::HashSet::new();
        for state in expected {
            let label = format!("{state:?}");
            if seen_in_input.insert(label.clone()) && !entered.contains(&label) {
                missing.push(label);
            }
        }

        if missing.is_empty() {
            Ok(())
        } else {
            Err(missing)
        }
    }

    /// Assert that every state with a registered handler in `workflow` was
    /// entered at least once during the run this observer recorded. Catches dead
    /// states — handlers that exist in the registration map but are never routed
    /// to.
    ///
    /// Exit states added via [`add_exit_state`](crate::workflow::Workflow::add_exit_state)
    /// *without* a handler are not required (they are not part of
    /// [`Workflow::registered_states`](crate::workflow::Workflow::registered_states)).
    /// Returns `Err` with the missing state labels, like
    /// [`assert_all_states_entered`](Self::assert_all_states_entered).
    pub fn assert_registered_states_entered<TState, TResourceKey>(
        &self,
        workflow: &crate::workflow::Workflow<TState, TResourceKey>,
    ) -> Result<(), Vec<String>>
    where
        TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
        TResourceKey: std::hash::Hash + Eq + Send + Sync + 'static,
    {
        let registered: Vec<TState> = workflow.registered_states().cloned().collect();
        self.assert_all_states_entered(&registered)
    }
}

impl WorkflowObserver for RecordingObserver {
    fn on_state_enter(&self, state: &str) {
        self.events
            .lock()
            .push(RecordedEvent::StateEntered(state.into()));
    }
    fn on_task_start(&self, task_id: &str) {
        self.events.lock().push(RecordedEvent::TaskStarted {
            state: task_id.into(),
        });
    }
    fn on_task_success(&self, task_id: &str) {
        self.events.lock().push(RecordedEvent::TaskSucceeded {
            state: task_id.into(),
        });
    }
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        self.events.lock().push(RecordedEvent::TaskFailed {
            state: task_id.into(),
            error: err.to_string(),
        });
    }
    fn on_retry(&self, task_id: &str, attempt: u32) {
        self.events.lock().push(RecordedEvent::Retry {
            state: task_id.into(),
            attempt,
        });
    }
    fn on_circuit_open(&self, task_id: &str) {
        self.events.lock().push(RecordedEvent::CircuitOpen {
            state: task_id.into(),
        });
    }
    fn on_checkpoint(&self, workflow_id: &str, sequence: u64) {
        self.events.lock().push(RecordedEvent::Checkpoint {
            workflow_id: workflow_id.into(),
            sequence,
        });
    }
    fn on_resume(&self, workflow_id: &str, sequence: u64) {
        self.events.lock().push(RecordedEvent::Resume {
            workflow_id: workflow_id.into(),
            sequence,
        });
    }
    fn on_cancelled(&self, state: &str) {
        self.events.lock().push(RecordedEvent::Cancelled {
            state: state.into(),
        });
    }
}

/// A process-local [`CheckpointStore`] for resume /
/// recovery tests.
///
/// It needs neither the `recovery` feature nor an on-disk file — a single
/// `Arc<InMemoryCheckpointStore>` can be shared across an
/// [`orchestrate`](crate::workflow::Workflow::orchestrate) run and a later
/// [`resume_from`](crate::workflow::Workflow::resume_from) to exercise the crash-recovery
/// path entirely in memory. It honors the full trait contract: duplicate
/// `(workflow_id, sequence)` is rejected, [`load_run`](Self::load_run) returns rows sorted by
/// sequence, and [`clear`](Self::clear) is per-id.
#[derive(Default, Debug)]
pub struct InMemoryCheckpointStore {
    inner: Mutex<HashMap<String, Vec<CheckpointRow>>>,
}

impl InMemoryCheckpointStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[crate::checkpoint_store]
impl CheckpointStore for InMemoryCheckpointStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let mut runs = self.inner.lock();
        let rows = runs.entry(workflow_id.to_string()).or_default();
        if rows.iter().any(|r| r.sequence == row.sequence) {
            return Err(CanoError::checkpoint_store(format!(
                "checkpoint conflict: {workflow_id:?} already has sequence {}",
                row.sequence
            )));
        }
        rows.push(row);
        Ok(())
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let mut rows = self
            .inner
            .lock()
            .get(workflow_id)
            .cloned()
            .unwrap_or_default();
        rows.sort_by_key(|r| r.sequence);
        Ok(rows)
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.inner.lock().remove(workflow_id);
        Ok(())
    }
}

/// A small builder for the [`Resources`] dictionary a workflow
/// needs.
///
/// Saves the `Resources::new().insert(..).insert(..)` ceremony, and gives a one-call
/// [`with_store`](Self::with_store) for the common case of "I just need a
/// [`MemoryStore`] at some key".
///
/// ```rust
/// use cano::testing::TestResources;
///
/// let resources = TestResources::new().with_store("store").build();
/// assert!(resources.get::<cano::MemoryStore, _>("store").is_ok());
/// ```
#[derive(Default)]
pub struct TestResources {
    inner: Resources,
}

impl TestResources {
    /// Start an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a fresh [`MemoryStore`] at `key`.
    #[must_use]
    pub fn with_store(mut self, key: &'static str) -> Self {
        self.inner = std::mem::take(&mut self.inner).insert(key, MemoryStore::new());
        self
    }

    /// Insert an arbitrary [`Resource`] at `key`.
    #[must_use]
    pub fn with_resource<R: Resource + 'static>(mut self, key: &'static str, resource: R) -> Self {
        self.inner = std::mem::take(&mut self.inner).insert(key, resource);
        self
    }

    /// Finish building and hand back the [`Resources`].
    #[must_use]
    pub fn build(self) -> Resources {
        self.inner
    }
}

/// A [`Task`] that panics on its first `n` attempts, then transitions to a
/// fixed next state. Build it with [`panic_on_attempt`].
///
/// The engine converts a panicking task into a `CanoError`
/// ([`task_execution`](crate::error::CanoError::task_execution) whose message starts with
/// `"panic: "`) rather than unwinding the workflow loop — this is the helper for exercising
/// that **panic-safety** path.
///
/// Note that the engine wraps its catch-unwind around the *whole* retry loop, so a panic
/// **fails fast — panics are never retried** (only a returned `Err` is). A
/// [`with_config`](Self::with_config) retry policy therefore has no effect on a panicking
/// attempt; with `n >= 1` the run fails on the first attempt, and the `next_state` success
/// branch is only reached when `n == 0`.
pub struct PanicOnAttempt<TState> {
    panics: u32,
    next_state: TState,
    attempts: AtomicU32,
    config: TaskConfig,
}

impl<TState> PanicOnAttempt<TState> {
    /// Override the retry configuration (default: [`TaskConfig::minimal`], no retries).
    ///
    /// Mostly useful to assert that panics are *not* retried — the engine fails a panicking
    /// task fast regardless of the policy set here.
    #[must_use]
    pub fn with_config(mut self, config: TaskConfig) -> Self {
        self.config = config;
        self
    }

    /// Total number of attempts seen so far (panicking attempts included).
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst)
    }
}

/// Build a [`PanicOnAttempt`] task that panics on its first `panics` attempts, then
/// transitions to `next_state`.
///
/// With `panics >= 1` the first attempt panics and the run fails with a `"panic: ..."`
/// error — panics are never retried (see [`PanicOnAttempt`]). Pass `panics == 0` for a
/// task that simply transitions on its first attempt.
///
/// ```rust
/// use cano::prelude::*;
/// use cano::testing::panic_on_attempt;
///
/// # #[derive(Clone, Debug, PartialEq, Eq, Hash)] enum S { Start, Done }
/// // Panics on the first attempt; the run fails fast with a "panic: ..." error.
/// let task = panic_on_attempt(1, S::Done);
/// # let _ = task;
/// ```
pub fn panic_on_attempt<TState>(panics: u32, next_state: TState) -> PanicOnAttempt<TState> {
    PanicOnAttempt {
        panics,
        next_state,
        attempts: AtomicU32::new(0),
        config: TaskConfig::minimal(),
    }
}

#[crate::task]
impl<TState> Task<TState> for PanicOnAttempt<TState>
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    fn config(&self) -> TaskConfig {
        self.config.clone()
    }

    fn name(&self) -> Cow<'static, str> {
        "PanicOnAttempt".into()
    }

    async fn run_bare(&self) -> Result<TaskResult<TState>, CanoError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.panics {
            panic!("panic_on_attempt: forced panic on attempt {attempt}");
        }
        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

/// Assert that a saga's recorded compensation order matches `expected`.
///
/// Collect the names (or labels) of the compensations that ran — typically by having each
/// `compensate` push its identifier into a shared `Vec` in [`Resources`] —
/// and pass that slice as `actual`. Compensations drain in reverse of completion order, so
/// `expected` should list them in the order you expect them to *undo*.
///
/// # Panics
///
/// Panics with a diff-style message when the orders differ.
///
/// ```rust
/// use cano::testing::assert_compensation_ran;
///
/// let ran = vec!["charge".to_string(), "reserve".to_string()];
/// assert_compensation_ran(&ran, &["charge", "reserve"]);
/// ```
pub fn assert_compensation_ran<S: AsRef<str>>(actual: &[S], expected: &[&str]) {
    let actual_refs: Vec<&str> = actual.iter().map(AsRef::as_ref).collect();
    assert_eq!(
        actual_refs.as_slice(),
        expected,
        "compensation order mismatch:\n  expected: {expected:?}\n  actual:   {actual_refs:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum S {
        Start,
        Done,
        A,
        B,
        C,
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

    /// Transitions unconditionally to a supplied next state.
    #[derive(Clone)]
    struct Go(S);
    #[task]
    impl Task<S> for Go {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn recording_observer_captures_success_path() {
        let observer = Arc::new(RecordingObserver::default());
        let wf = Workflow::bare()
            .register(S::Start, OkTask)
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        assert_eq!(wf.orchestrate(S::Start).await.unwrap(), S::Done);
        observer.assert_path(&["Start", "Done"]);
        observer.assert_completed_with("Done");
        assert!(observer.events().contains(&RecordedEvent::TaskSucceeded {
            state: "OkTask".into()
        }));
    }

    #[tokio::test]
    async fn recording_observer_clear_resets_events() {
        let observer = Arc::new(RecordingObserver::new());
        let wf = Workflow::bare()
            .register(S::Start, OkTask)
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        wf.orchestrate(S::Start).await.unwrap();
        assert!(!observer.events().is_empty());
        observer.clear();
        assert!(observer.events().is_empty());
    }

    #[tokio::test]
    async fn in_memory_checkpoint_store_roundtrip() {
        let store = InMemoryCheckpointStore::new();
        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        store
            .append("run", CheckpointRow::new(1, "B", "t1"))
            .await
            .unwrap();
        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[tokio::test]
    async fn in_memory_checkpoint_store_sorts_on_load() {
        let store = InMemoryCheckpointStore::new();
        store
            .append("run", CheckpointRow::new(2, "C", "t2"))
            .await
            .unwrap();
        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[tokio::test]
    async fn in_memory_checkpoint_store_rejects_duplicate_sequence() {
        let store = InMemoryCheckpointStore::new();
        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        let err = store
            .append("run", CheckpointRow::new(0, "A2", "t0"))
            .await
            .expect_err("duplicate sequence must be rejected");
        assert_eq!(err.category(), "checkpoint_store");
    }

    #[tokio::test]
    async fn in_memory_checkpoint_store_clear_is_per_id() {
        let store = InMemoryCheckpointStore::new();
        store
            .append("a", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        store
            .append("b", CheckpointRow::new(0, "B", "t0"))
            .await
            .unwrap();
        store.clear("a").await.unwrap();
        assert!(store.load_run("a").await.unwrap().is_empty());
        assert_eq!(store.load_run("b").await.unwrap().len(), 1);
    }

    #[test]
    fn test_resources_with_store_inserts_memory_store() {
        let resources = TestResources::new().with_store("store").build();
        assert!(resources.get::<MemoryStore, _>("store").is_ok());
    }

    #[test]
    fn test_resources_with_resource_inserts_custom() {
        #[derive(Clone)]
        struct Cfg {
            limit: usize,
        }
        #[resource]
        impl Resource for Cfg {}

        let resources = TestResources::new()
            .with_store("store")
            .with_resource("cfg", Cfg { limit: 7 })
            .build();
        assert_eq!(resources.get::<Cfg, _>("cfg").unwrap().limit, 7);
        assert!(resources.get::<MemoryStore, _>("store").is_ok());
    }

    #[tokio::test]
    async fn panic_on_attempt_first_attempt_surfaces_as_error() {
        // No retries: the first attempt panics. The engine converts a task panic into a
        // CanoError whose message starts with "panic:" — assert that conversion here by
        // driving the task's future through the same `catch_panic_to_error` wrapper.
        let task = panic_on_attempt(1, S::Done);
        let err = crate::workflow::catch_panic_to_error(
            <PanicOnAttempt<S> as Task<S>>::run_bare(&task),
            "Single task",
        )
        .await
        .expect_err("expected the forced panic to surface as an error");
        assert!(err.to_string().contains("panic"), "{err}");
    }

    #[tokio::test]
    async fn panic_on_attempt_fails_fast_even_with_retries_configured() {
        // The engine wraps its catch-unwind around the *whole* retry loop, so a panicking
        // task fails fast — panics are never retried (only returned `Err`s are). Even with a
        // generous retry policy, the very first panic surfaces as a task failure with no
        // retry recorded.
        let observer = Arc::new(RecordingObserver::new());
        let task = panic_on_attempt(2, S::Done)
            .with_config(TaskConfig::new().with_fixed_retry(5, Duration::from_millis(1)));
        let wf = Workflow::bare()
            .register(S::Start, task)
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        let err = wf.orchestrate(S::Start).await.unwrap_err();
        assert!(err.to_string().contains("panic"), "{err}");
        let retries = observer
            .events()
            .into_iter()
            .filter(|e| matches!(e, RecordedEvent::Retry { .. }))
            .count();
        assert_eq!(retries, 0, "panics are not retried");
        assert!(
            observer
                .events()
                .iter()
                .any(|e| matches!(e, RecordedEvent::TaskFailed { .. }))
        );
    }

    #[tokio::test]
    async fn panic_on_attempt_zero_panics_transitions_immediately() {
        // `panics = 0` never panics: the success branch is reachable and the task
        // transitions to its next state on the first attempt.
        let wf = Workflow::bare()
            .register(S::Start, panic_on_attempt(0, S::Done))
            .add_exit_state(S::Done);
        assert_eq!(wf.orchestrate(S::Start).await.unwrap(), S::Done);
    }

    #[test]
    fn assert_compensation_ran_matches() {
        let ran = vec!["charge".to_string(), "reserve".to_string()];
        assert_compensation_ran(&ran, &["charge", "reserve"]);
    }

    #[test]
    #[should_panic(expected = "compensation order mismatch")]
    fn assert_compensation_ran_mismatch_panics() {
        let ran = vec!["reserve".to_string()];
        assert_compensation_ran(&ran, &["charge", "reserve"]);
    }

    #[tokio::test]
    async fn assert_all_states_entered_passes_when_all_visited() {
        let observer = Arc::new(RecordingObserver::new());
        let wf = Workflow::bare()
            .register(S::A, Go(S::B))
            .register(S::B, Go(S::Done))
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        wf.orchestrate(S::A).await.unwrap();
        observer
            .assert_all_states_entered(&[S::A, S::B, S::Done])
            .expect("all states visited");
    }

    #[tokio::test]
    async fn assert_all_states_entered_returns_missing_in_input_order() {
        let observer = Arc::new(RecordingObserver::new());
        let wf = Workflow::bare()
            .register(S::A, Go(S::Done))
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        wf.orchestrate(S::A).await.unwrap();
        let missing = observer
            .assert_all_states_entered(&[S::A, S::B, S::C, S::Done])
            .unwrap_err();
        assert_eq!(missing, vec!["B".to_string(), "C".to_string()]);
    }

    #[tokio::test]
    async fn assert_all_states_entered_handles_duplicates_in_input() {
        let observer = Arc::new(RecordingObserver::new());
        let wf = Workflow::bare()
            .register(S::A, Go(S::Done))
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        wf.orchestrate(S::A).await.unwrap();
        let missing = observer
            .assert_all_states_entered(&[S::A, S::A, S::B])
            .unwrap_err();
        assert_eq!(missing, vec!["B".to_string()]);
    }

    #[tokio::test]
    async fn assert_registered_states_entered_passes_for_full_path() {
        let observer = Arc::new(RecordingObserver::new());
        let wf = Workflow::bare()
            .register(S::A, Go(S::B))
            .register(S::B, Go(S::Done))
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        wf.orchestrate(S::A).await.unwrap();
        observer
            .assert_registered_states_entered(&wf)
            .expect("all registered states visited");
    }

    #[tokio::test]
    async fn assert_registered_states_entered_reports_dead_state() {
        let observer = Arc::new(RecordingObserver::new());
        let wf = Workflow::bare()
            .register(S::A, Go(S::Done))
            .register(S::C, Go(S::Done)) // never routed to
            .add_exit_state(S::Done)
            .with_observer(observer.clone());
        wf.orchestrate(S::A).await.unwrap();
        let missing = observer.assert_registered_states_entered(&wf).unwrap_err();
        assert!(missing.contains(&"C".to_string()), "missing={missing:?}");
    }
}
