//! Shared task fixtures and test doubles for the `workflow` module's unit tests.
//!
//! `TestState` and the three trivial tasks below are used by the test modules in
//! `workflow.rs`, `workflow/execution.rs` and `workflow/compensation.rs`. They
//! live here so each of those `#[cfg(test)] mod tests` can `use` them rather than
//! redefining the same fixtures three times.
//!
//! `MemCheckpoints` lives here for the same reason — both the saga/recovery test
//! modules and the metrics test modules need an in-memory `CheckpointStore`
//! double, and prior to consolidation each defined its own identical copy.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use cano_macros::task;

use crate::error::CanoError;
use crate::observer::WorkflowObserver;
use crate::recovery::{CheckpointRow, CheckpointStore};
use crate::resource::Resources;
use crate::store::MemoryStore;
use crate::task::{Task, TaskResult};

/// Test workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TestState {
    Start,
    Process,
    Split,
    Join,
    Complete,
    #[allow(dead_code)]
    Error,
}

/// Simple task that returns a single state
#[derive(Clone)]
pub(crate) struct SimpleTask {
    next_state: TestState,
    counter: Arc<AtomicU32>,
}

impl SimpleTask {
    pub(crate) fn new(next_state: TestState) -> Self {
        Self {
            next_state,
            counter: Arc::new(AtomicU32::new(0)),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn count(&self) -> u32 {
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

/// Task that stores data using a `MemoryStore` from resources
#[derive(Clone)]
pub(crate) struct DataTask {
    key: String,
    value: String,
    next_state: TestState,
}

impl DataTask {
    pub(crate) fn new(key: &str, value: &str, next_state: TestState) -> Self {
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
        let store: Arc<MemoryStore> = res.get("store")?;
        store.put(&self.key, self.value.clone())?;
        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

/// Task that fails on demand
#[derive(Clone)]
pub(crate) struct FailTask {
    should_fail: bool,
}

impl FailTask {
    pub(crate) fn new(should_fail: bool) -> Self {
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

/// In-memory [`CheckpointStore`] test double. `live` is the real store state
/// (`clear` empties it); `audit` records every row ever appended, in order, and
/// is *never* cleared — so tests can inspect what was written even after a
/// successful run cleared the live log. Linear scans; fine for the tiny test
/// scenarios here, not for scale.
#[derive(Default)]
pub(crate) struct MemCheckpoints {
    live: std::sync::Mutex<HashMap<String, Vec<CheckpointRow>>>,
    audit: std::sync::Mutex<Vec<(String, CheckpointRow)>>,
}

#[cano_macros::checkpoint_store]
impl CheckpointStore for MemCheckpoints {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let mut live = self.live.lock().unwrap();
        let rows = live.entry(workflow_id.to_string()).or_default();
        if rows.iter().any(|r| r.sequence == row.sequence) {
            return Err(CanoError::checkpoint_store(format!(
                "checkpoint conflict: {workflow_id:?} already has sequence {}",
                row.sequence
            )));
        }
        self.audit
            .lock()
            .unwrap()
            .push((workflow_id.to_string(), row.clone()));
        rows.push(row);
        Ok(())
    }
    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        Ok(self.rows(workflow_id))
    }
    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.live.lock().unwrap().remove(workflow_id);
        Ok(())
    }
}

#[allow(dead_code)]
impl MemCheckpoints {
    /// Live rows for `workflow_id`, sorted by sequence (empty after a `clear`).
    pub(crate) fn rows(&self, workflow_id: &str) -> Vec<CheckpointRow> {
        let mut rows = self
            .live
            .lock()
            .unwrap()
            .get(workflow_id)
            .cloned()
            .unwrap_or_default();
        rows.sort_by_key(|r| r.sequence);
        rows
    }
    /// Every row ever appended for `workflow_id`, in append order — survives `clear`.
    pub(crate) fn audit_rows(&self, workflow_id: &str) -> Vec<CheckpointRow> {
        self.audit
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| id == workflow_id)
            .map(|(_, r)| r.clone())
            .collect()
    }
    pub(crate) fn audit_states(&self, workflow_id: &str) -> Vec<(u64, String)> {
        self.audit_rows(workflow_id)
            .into_iter()
            .map(|r| (r.sequence, r.state))
            .collect()
    }
}

/// Generic event recorder shared by every test module's mock observer.
///
/// Pre-consolidation, each test mod defined its own
/// `struct X(Mutex<Vec<EventTuple>>)` with the same `record` / `snapshot`
/// boilerplate. `Recorder<E>` parameterises over the event-tuple shape so a
/// test only has to spell out (1) the event type, and (2) a thin observer
/// impl that delegates to `record(...)`.
///
/// ```ignore
/// // Before:
/// struct CkptObserver(Mutex<Vec<(&'static str, String, u64)>>);
/// impl WorkflowObserver for CkptObserver { /* method body pushes to vec */ }
///
/// // After:
/// struct CkptObserver(Arc<Recorder<(&'static str, String, u64)>>);
/// impl WorkflowObserver for CkptObserver { /* method body calls self.0.record(...) */ }
/// ```
pub(crate) struct Recorder<E: Clone + Send + Sync + 'static> {
    events: std::sync::Mutex<Vec<E>>,
}

// Manual `Default` so the derive doesn't require `E: Default` — the bound
// would be wrong, since the Vec inside is created empty regardless of `E`.
impl<E: Clone + Send + Sync + 'static> Default for Recorder<E> {
    fn default() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[allow(dead_code)]
impl<E: Clone + Send + Sync + 'static> Recorder<E> {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }
    pub(crate) fn record(&self, event: E) {
        self.events.lock().unwrap().push(event);
    }
    pub(crate) fn snapshot(&self) -> Vec<E> {
        self.events.lock().unwrap().clone()
    }
    pub(crate) fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.events.lock().unwrap().is_empty()
    }
}

/// One captured [`WorkflowObserver`] event, one variant per hook. Recorded in
/// arrival order by [`EventLog`] so tests can assert both *what* fired and the
/// *order* it fired in. Replaces the per-test-module bespoke observers that each
/// recorded a different bespoke tuple/string shape.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TestEvent {
    StateEnter(String),
    TaskStart(String),
    TaskSuccess(String),
    TaskFailure {
        task: String,
        error: String,
    },
    Retry {
        task: String,
        attempt: u32,
    },
    CircuitOpen(String),
    Checkpoint {
        workflow_id: String,
        sequence: u64,
    },
    Resume {
        workflow_id: String,
        sequence: u64,
    },
    WorkflowTimeout {
        elapsed: Duration,
        limit: Duration,
    },
    CheckpointClearFailed {
        workflow_id: String,
        error: String,
    },
    UnknownResumeState {
        workflow_id: String,
        sequence: u64,
        label: String,
    },
}

/// A [`WorkflowObserver`] that records *every* hook into a shared
/// [`Recorder<TestEvent>`]. Construct with [`EventLog::new`], hand the `EventLog`
/// to [`Workflow::with_observer`](crate::workflow::Workflow::with_observer), and
/// snapshot/inspect via the returned `Recorder` handle (typed accessors like
/// [`Recorder::<TestEvent>::checkpoints`] and the ordered [`Recorder::<TestEvent>::labels`]).
pub(crate) struct EventLog(Arc<Recorder<TestEvent>>);

#[allow(dead_code)]
impl EventLog {
    /// Wraps a fresh `Recorder` and returns both halves: the observer for
    /// `Workflow::with_observer`, and the recorder for snapshotting — mirroring
    /// the `*Observer::new()` convention the bespoke observers used.
    pub(crate) fn new() -> (Self, Arc<Recorder<TestEvent>>) {
        let rec = Recorder::new();
        (Self(rec.clone()), rec)
    }
}

impl WorkflowObserver for EventLog {
    fn on_state_enter(&self, state: &str) {
        self.0.record(TestEvent::StateEnter(state.to_string()));
    }
    fn on_task_start(&self, task_id: &str) {
        self.0.record(TestEvent::TaskStart(task_id.to_string()));
    }
    fn on_task_success(&self, task_id: &str) {
        self.0.record(TestEvent::TaskSuccess(task_id.to_string()));
    }
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        self.0.record(TestEvent::TaskFailure {
            task: task_id.to_string(),
            error: err.to_string(),
        });
    }
    fn on_retry(&self, task_id: &str, attempt: u32) {
        self.0.record(TestEvent::Retry {
            task: task_id.to_string(),
            attempt,
        });
    }
    fn on_circuit_open(&self, task_id: &str) {
        self.0.record(TestEvent::CircuitOpen(task_id.to_string()));
    }
    fn on_checkpoint(&self, workflow_id: &str, sequence: u64) {
        self.0.record(TestEvent::Checkpoint {
            workflow_id: workflow_id.to_string(),
            sequence,
        });
    }
    fn on_resume(&self, workflow_id: &str, sequence: u64) {
        self.0.record(TestEvent::Resume {
            workflow_id: workflow_id.to_string(),
            sequence,
        });
    }
    fn on_workflow_timeout(&self, elapsed: Duration, limit: Duration) {
        self.0.record(TestEvent::WorkflowTimeout { elapsed, limit });
    }
    fn on_checkpoint_clear_failed(&self, workflow_id: &str, error: &CanoError) {
        self.0.record(TestEvent::CheckpointClearFailed {
            workflow_id: workflow_id.to_string(),
            error: error.to_string(),
        });
    }
    fn on_unknown_resume_state(&self, workflow_id: &str, sequence: u64, unknown_state_label: &str) {
        self.0.record(TestEvent::UnknownResumeState {
            workflow_id: workflow_id.to_string(),
            sequence,
            label: unknown_state_label.to_string(),
        });
    }
}

/// Typed projections over a recorded [`TestEvent`] stream. Each returns the
/// fields the relevant tests assert on, in arrival order; tests that need a
/// different shape can still `snapshot()` and match variants directly.
#[allow(dead_code)]
impl Recorder<TestEvent> {
    /// Short `"kind:arg"` rendering of each event, in order — for cross-hook
    /// ordering assertions (e.g. state-enter before task-start before success).
    pub(crate) fn labels(&self) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .map(|e| match e {
                TestEvent::StateEnter(s) => format!("state_enter:{s}"),
                TestEvent::TaskStart(t) => format!("task_start:{t}"),
                TestEvent::TaskSuccess(t) => format!("task_success:{t}"),
                TestEvent::TaskFailure { task, .. } => format!("task_failure:{task}"),
                TestEvent::Retry { task, attempt } => format!("retry:{task}:{attempt}"),
                TestEvent::CircuitOpen(t) => format!("circuit_open:{t}"),
                TestEvent::Checkpoint {
                    workflow_id,
                    sequence,
                } => format!("checkpoint:{workflow_id}:{sequence}"),
                TestEvent::Resume {
                    workflow_id,
                    sequence,
                } => format!("resume:{workflow_id}:{sequence}"),
                TestEvent::WorkflowTimeout { limit, .. } => {
                    format!("workflow_timeout:{}ms", limit.as_millis())
                }
                TestEvent::CheckpointClearFailed { workflow_id, .. } => {
                    format!("checkpoint_clear_failed:{workflow_id}")
                }
                TestEvent::UnknownResumeState { label, .. } => {
                    format!("unknown_resume_state:{label}")
                }
            })
            .collect()
    }
    /// State labels passed to `on_state_enter`, in order.
    pub(crate) fn state_enters(&self) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .filter_map(|e| match e {
                TestEvent::StateEnter(s) => Some(s),
                _ => None,
            })
            .collect()
    }
    /// `(task, attempt)` for each `on_retry`, in order.
    pub(crate) fn retries(&self) -> Vec<(String, u32)> {
        self.snapshot()
            .into_iter()
            .filter_map(|e| match e {
                TestEvent::Retry { task, attempt } => Some((task, attempt)),
                _ => None,
            })
            .collect()
    }
    /// `(workflow_id, sequence)` for each `on_checkpoint`, in order.
    pub(crate) fn checkpoints(&self) -> Vec<(String, u64)> {
        self.snapshot()
            .into_iter()
            .filter_map(|e| match e {
                TestEvent::Checkpoint {
                    workflow_id,
                    sequence,
                } => Some((workflow_id, sequence)),
                _ => None,
            })
            .collect()
    }
    /// `(workflow_id, sequence)` for each `on_resume`, in order.
    pub(crate) fn resumes(&self) -> Vec<(String, u64)> {
        self.snapshot()
            .into_iter()
            .filter_map(|e| match e {
                TestEvent::Resume {
                    workflow_id,
                    sequence,
                } => Some((workflow_id, sequence)),
                _ => None,
            })
            .collect()
    }
    /// `(elapsed, limit)` for each `on_workflow_timeout`, in order.
    pub(crate) fn timeouts(&self) -> Vec<(Duration, Duration)> {
        self.snapshot()
            .into_iter()
            .filter_map(|e| match e {
                TestEvent::WorkflowTimeout { elapsed, limit } => Some((elapsed, limit)),
                _ => None,
            })
            .collect()
    }
    /// `(workflow_id, error)` for each `on_checkpoint_clear_failed`, in order.
    pub(crate) fn clear_failures(&self) -> Vec<(String, String)> {
        self.snapshot()
            .into_iter()
            .filter_map(|e| match e {
                TestEvent::CheckpointClearFailed { workflow_id, error } => {
                    Some((workflow_id, error))
                }
                _ => None,
            })
            .collect()
    }
    /// `(workflow_id, sequence, label)` for each `on_unknown_resume_state`, in order.
    pub(crate) fn unknown_states(&self) -> Vec<(String, u64, String)> {
        self.snapshot()
            .into_iter()
            .filter_map(|e| match e {
                TestEvent::UnknownResumeState {
                    workflow_id,
                    sequence,
                    label,
                } => Some((workflow_id, sequence, label)),
                _ => None,
            })
            .collect()
    }
}

/// Shared, ordered log of `(task name, output value)` for every `compensate`
/// call. Used by both saga tests in `workflow/compensation.rs::tests` and the
/// metrics counterparts; sharing the type avoids duplicate aliases.
pub(crate) type CompLog = std::sync::Arc<std::sync::Mutex<Vec<(String, u32)>>>;

/// A compensatable saga test task. The forward `run` returns `next_state` and
/// `value` (unless `fail_forward`); `compensate` records `(name, output)` onto
/// `log` (and errors if `fail_compensate`). No retries, so a forward failure
/// surfaces immediately. Used by both `compensation::tests` and
/// `compensation::metrics_tests` (R5 already lifted the sibling test double
/// `MemCheckpoints`; this finishes the consolidation for saga tests).
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct CompTask {
    pub(crate) name: &'static str,
    pub(crate) value: u32,
    pub(crate) next_state: TestState,
    pub(crate) log: CompLog,
    pub(crate) fail_forward: bool,
    pub(crate) fail_compensate: bool,
}

#[allow(dead_code)]
impl CompTask {
    /// Default-constructed task that never fails. Most tests use this; only
    /// the ones that exercise the rollback path opt into the `fail_*` flags
    /// via the struct literal.
    pub(crate) fn ok(name: &'static str, value: u32, next_state: TestState, log: &CompLog) -> Self {
        Self {
            name,
            value,
            next_state,
            log: log.clone(),
            fail_forward: false,
            fail_compensate: false,
        }
    }
}

#[crate::saga::task]
impl crate::saga::CompensatableTask<TestState> for CompTask {
    type Output = u32;
    fn config(&self) -> crate::task::TaskConfig {
        crate::task::TaskConfig::minimal()
    }
    fn name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(self.name)
    }
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<TestState>, u32), CanoError> {
        if self.fail_forward {
            return Err(CanoError::task_execution(format!(
                "{} forward failed",
                self.name
            )));
        }
        Ok((TaskResult::Single(self.next_state.clone()), self.value))
    }
    async fn compensate(&self, _res: &Resources, output: u32) -> Result<(), CanoError> {
        self.log
            .lock()
            .unwrap()
            .push((self.name.to_string(), output));
        if self.fail_compensate {
            return Err(CanoError::generic(format!(
                "{} compensate failed",
                self.name
            )));
        }
        Ok(())
    }
}
