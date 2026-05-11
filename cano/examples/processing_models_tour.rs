//! # Processing Models Tour — Chaining All Four Models in One Workflow
//!
//! Run with: `cargo run --example processing_models_tour --features recovery`
//!
//! Demonstrates that all four Cano processing models compose cleanly in a single
//! workflow, with a [`RedbCheckpointStore`] attached to observe checkpoint
//! behaviour across model boundaries.
//!
//! Workflow shape:
//!
//! ```text
//!   Route ──(router)──► Wait ──(poll loop)──► Crunch ──(batch fan-out)──► Grind ──(step loop)──► Done
//! ```
//!
//! Each state showcases a different processing model:
//!
//! - **`Route`** ([`RouterTask`]) — inspects a [`Config`] resource and returns the next
//!   state. Side-effect-free; writes no [`CheckpointRow`](cano::recovery::CheckpointRow).
//! - **`Wait`** ([`PollTask`]) — polls a shared [`AtomicU32`] counter until a background
//!   task has incremented it past a threshold, then proceeds.
//! - **`Crunch`** ([`BatchTask`]) — fan-out over a small `Vec<u32>`, squaring each item
//!   in parallel (concurrency = 4), then summing the results in `finish`.
//! - **`Grind`** ([`SteppedTask`]) — counts from 0 to 6 one step at a time, with each
//!   cursor persisted as a [`RowKind::StepCursor`](cano::recovery::RowKind) row.
//! - **`Done`** — exit state.
//!
//! On a successful run the engine clears the checkpoint log. Run
//! `cargo test -p cano --features recovery` for the assertion that no `Route` row
//! was ever written, and that sequence numbers are dense.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use cano::RedbCheckpointStore;
use cano::prelude::*;

// ---------------------------------------------------------------------------
// State enum — named `Stage` (not `Step`) to avoid shadowing `cano::Step`
// ---------------------------------------------------------------------------

/// Workflow states for this tour.
///
/// Named `Stage` — `Step` would also work since `cano::StepOutcome` no longer
/// collides with a plain identifier `Step`, but `Stage` is semantically fitting here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage {
    /// Router state: reads config, routes to `Wait`.
    Route,
    /// Poll state: waits until a background counter reaches a threshold.
    Wait,
    /// Batch state: fans out over a small Vec, squares each item.
    Crunch,
    /// Stepped state: counts 0..=6 one step at a time.
    Grind,
    /// Terminal exit state.
    Done,
}

// ---------------------------------------------------------------------------
// Config resource — carries a trivial flag read by the router
// ---------------------------------------------------------------------------

/// Shared configuration resource injected into the workflow.
struct Config {
    /// If `true`, the router will proceed to `Wait` (the normal path).
    proceed: bool,
}

#[resource]
impl Resource for Config {}

// ---------------------------------------------------------------------------
// Counter resource — shared between the poller and the background spawner
// ---------------------------------------------------------------------------

/// Tracks how many ticks a background job has completed.
struct Counter {
    done: Arc<AtomicU32>,
    threshold: u32,
}

impl Counter {
    fn new(threshold: u32) -> (Self, Arc<AtomicU32>) {
        let done = Arc::new(AtomicU32::new(0));
        let ctr = Counter {
            done: Arc::clone(&done),
            threshold,
        };
        (ctr, done)
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire) >= self.threshold
    }
}

#[resource]
impl Resource for Counter {}

// ---------------------------------------------------------------------------
// Route — RouterTask
// ---------------------------------------------------------------------------

/// Reads `Config` from resources and routes to `Wait`.
struct Router;

#[router_task(state = Stage)]
impl Router {
    async fn route(&self, res: &Resources) -> Result<TaskResult<Stage>, CanoError> {
        let config = res.get::<Config, _>("config")?;
        if config.proceed {
            println!("route      : config.proceed=true → Wait");
            Ok(TaskResult::Single(Stage::Wait))
        } else {
            // In this example the flag is always true, but branching is possible.
            Err(CanoError::configuration(
                "config.proceed must be true for this tour",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Wait — PollTask
// ---------------------------------------------------------------------------

/// Polls the `Counter` resource until the background job completes.
struct Waiter;

#[poll_task(state = Stage)]
impl Waiter {
    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Stage>, CanoError> {
        let counter = res.get::<Counter, _>("counter")?;
        if counter.is_done() {
            println!("wait       : counter reached threshold → Crunch");
            Ok(PollOutcome::Ready(TaskResult::Single(Stage::Crunch)))
        } else {
            Ok(PollOutcome::Pending { delay_ms: 5 })
        }
    }
}

// ---------------------------------------------------------------------------
// Crunch — BatchTask
// ---------------------------------------------------------------------------

/// Squares each item in the batch and sums them in `finish`.
struct Cruncher;

#[batch_task(state = Stage)]
impl Cruncher {
    fn concurrency(&self) -> usize {
        4
    }

    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        let items: Vec<u32> = (1..=8).collect();
        println!("crunch     : loaded {} items", items.len());
        Ok(items)
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        Ok(item * item)
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Stage>, CanoError> {
        let sum: u32 = outputs.into_iter().filter_map(|r| r.ok()).sum();
        println!("crunch     : sum of squares 1²+…+8² = {sum} → Grind");
        Ok(TaskResult::Single(Stage::Grind))
    }
}

// ---------------------------------------------------------------------------
// Grind — SteppedTask
// ---------------------------------------------------------------------------

/// Counts from 0 to 6, advancing one step at a time.
///
/// Each `Step::More` persists the cursor as a `RowKind::StepCursor` row when a
/// checkpoint store is attached.
struct Grinder;

#[stepped_task(state = Stage)]
impl Grinder {
    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, Stage>, CanoError> {
        let n = cursor.unwrap_or(0);
        println!("grind      : step {}", n + 1);
        if n >= 6 {
            println!("grind      : complete → Done");
            Ok(StepOutcome::Done(TaskResult::Single(Stage::Done)))
        } else {
            Ok(StepOutcome::More(n + 1))
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let run_id = "processing-models-tour";

    // Create a temporary checkpoint store.
    let dir = tempfile::tempdir()?;
    let store = Arc::new(RedbCheckpointStore::new(dir.path().join("tour.redb"))?);

    // --- counter resource + background incrementer ---
    const THRESHOLD: u32 = 4;
    let (counter, done) = Counter::new(THRESHOLD);

    // Background task increments the counter every 8ms until the threshold is reached.
    tokio::spawn(async move {
        for _ in 0..THRESHOLD {
            tokio::time::sleep(Duration::from_millis(8)).await;
            done.fetch_add(1, Ordering::Release);
        }
    });

    // --- build resources ---
    let resources = Resources::new()
        .insert("config", Config { proceed: true })
        .insert("counter", counter);

    // --- build workflow ---
    println!("=== processing models tour ===\n");

    let workflow = Workflow::new(resources)
        .register_router(Stage::Route, Router)
        .register(Stage::Wait, Waiter)
        .register(Stage::Crunch, Cruncher)
        .register_stepped(Stage::Grind, Grinder)
        .add_exit_state(Stage::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id(run_id);

    let result = workflow.orchestrate(Stage::Route).await?;
    assert_eq!(result, Stage::Done);

    println!("\ncompleted at {result:?}");

    // The checkpoint log is cleared on a successful run.
    let rows = store.load_run(run_id).await?;
    println!(
        "\ncheckpoint log after successful run: {} row(s) (cleared on success)",
        rows.len()
    );
    assert!(
        rows.is_empty(),
        "checkpoint log should be empty after a successful run"
    );

    println!(
        "\nNote: the `Route` router state never wrote a checkpoint row; `Wait`, `Crunch`,\n\
         `Grind` (entry + cursor rows), and `Done` each did during the run.\n\
         Run `cargo test -p cano --features recovery` for the cross-model interop assertion."
    );

    // The temp directory (and the redb file) is cleaned up when `dir` drops here.
    Ok(())
}
