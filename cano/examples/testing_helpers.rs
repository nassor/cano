//! # `cano::testing` — batteries-included test fixtures
//!
//! Run with: `cargo run --example testing_helpers --features testing`
//!
//! Everything here lives behind the `testing` feature and is imported wholesale with
//! `use cano::testing::*;`. This example exercises each helper the way a test would:
//!
//! - [`RecordingObserver`] — capture and assert the path a workflow took.
//! - [`InMemoryCheckpointStore`] — a process-local checkpoint store (no `recovery`
//!   feature, no on-disk file) whose `Checkpoint` events the observer records.
//! - [`TestResources`] — build the [`Resources`] a workflow needs in one chain.
//! - [`panic_on_attempt`] — a task that panics; the engine converts it to an error
//!   (panic safety) and fails fast.
//! - [`assert_compensation_ran`] — assert the order a saga rolled back in.

use std::sync::{Arc, Mutex};

use cano::prelude::*;
use cano::testing::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Start,
    Work,
    Finish,
    Done,
}

struct StartTask;
#[task(state = Step)]
impl StartTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Work))
    }
}

struct WorkTask;
#[task(state = Step)]
impl WorkTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("processed", 42_u32)?;
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---- A tiny saga whose compensations record the order they undo in. -------------------

/// Shared, ordered log of which compensations ran. Registered as a resource so every
/// step can reach it; `Clone` shares the inner `Arc`, so a handle kept outside the
/// workflow reads the same `Vec`.
#[derive(Default, Clone)]
struct CompensationLog(Arc<Mutex<Vec<String>>>);
#[resource]
impl Resource for CompensationLog {}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Unit;

struct Reserve;
#[saga::task(state = Step)]
impl Reserve {
    type Output = Unit;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Unit), CanoError> {
        Ok((TaskResult::Single(Step::Work), Unit))
    }
    async fn compensate(&self, res: &Resources, _out: Unit) -> Result<(), CanoError> {
        res.get::<CompensationLog, _>("log")?
            .0
            .lock()
            .unwrap()
            .push("reserve".into());
        Ok(())
    }
}

struct Charge;
#[saga::task(state = Step)]
impl Charge {
    type Output = Unit;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Unit), CanoError> {
        Ok((TaskResult::Single(Step::Finish), Unit))
    }
    async fn compensate(&self, res: &Resources, _out: Unit) -> Result<(), CanoError> {
        res.get::<CompensationLog, _>("log")?
            .0
            .lock()
            .unwrap()
            .push("charge".into());
        Ok(())
    }
}

struct Boom;
#[task(state = Step)]
impl Boom {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Err(CanoError::task_execution("boom"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. RecordingObserver + InMemoryCheckpointStore + TestResources --------------------
    let observer = Arc::new(RecordingObserver::new());
    let checkpoints = Arc::new(InMemoryCheckpointStore::new());
    let resources = TestResources::new().with_store("store").build();

    let workflow = Workflow::new(resources)
        .register(Step::Start, StartTask)
        .register(Step::Work, WorkTask)
        .add_exit_state(Step::Done)
        .with_observer(observer.clone())
        .with_checkpoint_store(checkpoints.clone())
        .with_workflow_id("demo-run");

    let final_state = workflow.orchestrate(Step::Start).await?;
    assert_eq!(final_state, Step::Done);

    // The observer captured the whole path and the checkpoint appends along the way.
    observer.assert_path(&["Start", "Work", "Done"]);
    observer.assert_completed_with("Done");
    let checkpoint_events = observer
        .events()
        .into_iter()
        .filter(|e| matches!(e, RecordedEvent::Checkpoint { .. }))
        .count();
    println!("observer recorded path {:?}", observer.states_entered());
    println!("observer saw {checkpoint_events} checkpoint append(s)");

    // 2. panic_on_attempt — panic safety: the panic becomes an error, fails fast. -------
    let panicky = Workflow::bare()
        .register(Step::Start, panic_on_attempt(1, Step::Done))
        .add_exit_state(Step::Done);
    match panicky.orchestrate(Step::Start).await {
        Ok(_) => unreachable!("the task panics on its first attempt"),
        Err(e) => println!("panic_on_attempt surfaced as error: {e}"),
    }

    // 3. assert_compensation_ran — a saga that rolls back in reverse. --------------------
    let log = CompensationLog::default();
    let handle = log.clone();
    let saga_resources = Resources::new().insert("log", log);
    let saga = Workflow::new(saga_resources)
        .register_with_compensation(Step::Start, Reserve)
        .register_with_compensation(Step::Work, Charge)
        .register(Step::Finish, Boom) // fails → drains the compensation stack in reverse
        .add_exit_state(Step::Done);
    let _ = saga.orchestrate(Step::Start).await; // expected to fail and roll back

    let ran = handle.0.lock().unwrap().clone();
    // Charge ran last, so it compensates first; then Reserve.
    assert_compensation_ran(&ran, &["charge", "reserve"]);
    println!("compensation ran in order: {ran:?}");

    println!("\nall testing helpers exercised ✔");
    Ok(())
}
