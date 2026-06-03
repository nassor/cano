//! Integration coverage for the `cano::testing` module.
//!
//! Drives real workflows through the public testing helpers — the `RecordingObserver`,
//! the `InMemoryCheckpointStore` (including a full crash-and-resume cycle), the
//! `panic_on_attempt` task factory, and the `assert_compensation_ran` saga assertion.
//!
//! Requires the `testing` feature.
#![cfg(feature = "testing")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cano::prelude::*;
use cano::testing::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum S {
    Start,
    Work,
    Finish,
    Done,
}

struct Go(S);
#[task(state = S)]
impl Go {
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        Ok(TaskResult::Single(self.0.clone()))
    }
}

#[tokio::test]
async fn recording_observer_captures_path_and_checkpoints() {
    let observer = Arc::new(RecordingObserver::new());
    let store = Arc::new(InMemoryCheckpointStore::new());
    let wf = Workflow::bare()
        .register(S::Start, Go(S::Work))
        .register(S::Work, Go(S::Done))
        .add_exit_state(S::Done)
        .with_observer(observer.clone())
        .with_checkpoint_store(store.clone())
        .with_workflow_id("run-1");

    assert_eq!(wf.orchestrate(S::Start).await.unwrap(), S::Done);
    observer.assert_path(&["Start", "Work", "Done"]);
    observer.assert_completed_with("Done");

    // One checkpoint append per state entry (Start, Work, Done).
    let checkpoints = observer
        .events()
        .into_iter()
        .filter(|e| matches!(e, RecordedEvent::Checkpoint { .. }))
        .count();
    assert_eq!(checkpoints, 3);
}

/// A `Work` task that fails on its first run, then succeeds — used to simulate a crash
/// before `Done`, so a later `resume_from` re-runs it and finishes.
struct FlakyWork(Arc<AtomicUsize>);
#[task(state = S)]
impl FlakyWork {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal() // fail fast so the run stops and the log persists
    }
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(CanoError::task_execution("crash before Done"))
        } else {
            Ok(TaskResult::Single(S::Done))
        }
    }
}

#[tokio::test]
async fn in_memory_store_supports_resume_after_failure() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let wf_id = "resume-run";

    let build = |runs: Arc<AtomicUsize>| {
        Workflow::bare()
            .register(S::Start, Go(S::Work))
            .register(S::Work, FlakyWork(runs))
            .add_exit_state(S::Done)
            .with_checkpoint_store(store.clone())
            .with_workflow_id(wf_id)
    };

    // Generation 1: crashes at Work. The log keeps Start + Work entries.
    build(runs.clone())
        .orchestrate(S::Start)
        .await
        .expect_err("generation 1 must fail at Work");
    assert_eq!(store.load_run(wf_id).await.unwrap().len(), 2);

    // Generation 2: resume re-enters at Work, re-runs it (now succeeds), reaches Done.
    let observer = Arc::new(RecordingObserver::new());
    let resumed = build(runs.clone()).with_observer(observer.clone());
    assert_eq!(resumed.resume_from(wf_id).await.unwrap(), S::Done);
    assert!(
        observer
            .events()
            .iter()
            .any(|e| matches!(e, RecordedEvent::Resume { .. })),
        "resume must fire an on_resume event"
    );
    // Work ran twice total (gen 1 failure + gen 2 success); Start did not re-run.
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn panic_on_attempt_fails_fast_with_panic_error() {
    let observer = Arc::new(RecordingObserver::new());
    let wf = Workflow::bare()
        .register(S::Start, panic_on_attempt(1, S::Done))
        .add_exit_state(S::Done)
        .with_observer(observer.clone());
    let err = wf.orchestrate(S::Start).await.unwrap_err();
    assert!(err.to_string().contains("panic"), "{err}");
    // Panics are not retried — no retry event recorded.
    assert!(
        !observer
            .events()
            .iter()
            .any(|e| matches!(e, RecordedEvent::Retry { .. }))
    );
}

// ---- Saga: assert_compensation_ran -----------------------------------------------------

#[derive(Default, Clone)]
struct Log(Arc<Mutex<Vec<String>>>);
#[resource]
impl Resource for Log {}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Unit;

struct Step1;
#[saga::task(state = S)]
impl Step1 {
    type Output = Unit;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<S>, Unit), CanoError> {
        Ok((TaskResult::Single(S::Work), Unit))
    }
    async fn compensate(&self, res: &Resources, _o: Unit) -> Result<(), CanoError> {
        res.get::<Log, _>("log")?
            .0
            .lock()
            .unwrap()
            .push("step1".into());
        Ok(())
    }
}

struct Step2;
#[saga::task(state = S)]
impl Step2 {
    type Output = Unit;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<S>, Unit), CanoError> {
        Ok((TaskResult::Single(S::Finish), Unit))
    }
    async fn compensate(&self, res: &Resources, _o: Unit) -> Result<(), CanoError> {
        res.get::<Log, _>("log")?
            .0
            .lock()
            .unwrap()
            .push("step2".into());
        Ok(())
    }
}

struct Fail;
#[task(state = S)]
impl Fail {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        Err(CanoError::task_execution("boom"))
    }
}

#[tokio::test]
async fn assert_compensation_ran_matches_reverse_order() {
    let log = Log::default();
    let handle = log.clone();
    let resources = Resources::new().insert("log", log);
    let saga = Workflow::new(resources)
        .register_with_compensation(S::Start, Step1)
        .register_with_compensation(S::Work, Step2)
        .register(S::Finish, Fail)
        .add_exit_state(S::Done);
    let _ = saga.orchestrate(S::Start).await; // fails, rolls back

    let ran = handle.0.lock().unwrap().clone();
    // Step2 completed last, so it compensates first; then Step1.
    assert_compensation_ran(&ran, &["step2", "step1"]);
}

#[tokio::test]
#[should_panic(expected = "compensation order mismatch")]
async fn assert_compensation_ran_mismatch_panics() {
    let ran = vec!["step1".to_string()];
    assert_compensation_ran(&ran, &["step2", "step1"]);
}
