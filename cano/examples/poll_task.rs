//! # PollTask — Wait-Until Processing Model
//!
//! Run with: `cargo run --example poll_task`
//!
//! Demonstrates [`PollTask`] as a "wait-until" processing unit. A background job
//! increments a progress counter over time; the `AwaitJob` poller reads the counter
//! from [`Resources`] and loops until the job is complete — returning
//! [`PollOutcome::Pending`] on each tick and [`PollOutcome::Ready`] once progress is full.
//!
//! Workflow shape:
//!
//! ```text
//!   AwaitJob ──(poll loop)──► Process ──► Done
//! ```
//!
//! The example shows the most common `PollTask` shape:
//! - Attach the shared counter as a [`Resource`] so both the poller and the
//!   background task see the same [`AtomicU32`](std::sync::atomic::AtomicU32).
//! - Use adaptive `delay_ms`: poll more frequently as the job nears completion.
//! - Pair with a plain `#[task]` for the post-poll processing step.
//!
//! For a wall-clock cap, `config()` can return
//! `TaskConfig::minimal().with_attempt_timeout(dur)`. That timeout is enforced
//! by the workflow engine when the task is dispatched via `orchestrate()`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use cano::prelude::*;

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    /// Poll state: waits until the background job reports completion.
    AwaitJob,
    /// Processing state: runs after the job finishes.
    Process,
    /// Terminal exit state.
    Done,
}

// ---------------------------------------------------------------------------
// Shared job resource — tracks background job progress
// ---------------------------------------------------------------------------

/// Represents an external job whose progress is tracked by an atomic counter.
///
/// Both the background incrementer task and the [`AwaitJob`] poller hold an
/// `Arc<AtomicU32>` pointing at the same counter, so reads in the poller are
/// always up to date without any extra locking.
struct Job {
    ticks_done: Arc<AtomicU32>,
    ticks_total: u32,
}

impl Job {
    fn new(ticks_total: u32) -> (Self, Arc<AtomicU32>) {
        let counter = Arc::new(AtomicU32::new(0));
        let job = Job {
            ticks_done: Arc::clone(&counter),
            ticks_total,
        };
        (job, counter)
    }

    fn done(&self) -> u32 {
        self.ticks_done.load(Ordering::Acquire)
    }
}

// `Resource` lifecycle hooks default to no-ops; we just need the trait impl to
// store this in `Resources`.
#[resource]
impl Resource for Job {}

// ---------------------------------------------------------------------------
// Poll task — waits for the job to complete
// ---------------------------------------------------------------------------

/// Polls the `Job` resource until all ticks are done.
struct AwaitJob;

#[poll_task(state = Step)]
impl AwaitJob {
    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let job = res.get::<Job, _>("job")?;
        let done = job.done();
        let total = job.ticks_total;

        if done >= total {
            println!("await job  : complete ({done}/{total} ticks)");
            Ok(PollOutcome::Ready(TaskResult::Single(Step::Process)))
        } else {
            // Adaptive delay: poll more frequently when close to completion.
            let remaining = total - done;
            let delay_ms = if remaining <= 2 { 2 } else { 5 };
            println!("await job  : progress {done}/{total} ticks, next poll in {delay_ms}ms");
            Ok(PollOutcome::Pending { delay_ms })
        }

        // To bound the entire poll loop by wall-clock time, override `config()`:
        //
        //   fn config(&self) -> TaskConfig {
        //       TaskConfig::minimal().with_attempt_timeout(Duration::from_secs(5))
        //   }
        //
        // `CanoError::Timeout` is returned if the loop does not finish within
        // the timeout. The timeout is enforced by the workflow engine, not inside
        // this method.
    }
}

// ---------------------------------------------------------------------------
// Processing task — runs after the job completes
// ---------------------------------------------------------------------------

/// Simple post-job processor that transitions to the exit state.
struct Process;

#[task(state = Step)]
impl Process {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("process    : handling completed job results");
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> CanoResult<()> {
    const TICKS: u32 = 6;

    // Build the Job resource. The second return value is a cloned Arc pointing
    // at the same AtomicU32, shared with the background spawner below.
    let (job, counter) = Job::new(TICKS);

    // Spawn a background task that simulates the job making progress.
    // Each iteration sleeps briefly then increments the shared counter.
    tokio::spawn(async move {
        for _ in 0..TICKS {
            tokio::time::sleep(Duration::from_millis(10)).await;
            counter.fetch_add(1, Ordering::Release);
        }
    });

    println!("=== poll_task example ===");
    println!("background job: {TICKS} ticks at ~10 ms each\n");

    let resources = Resources::new().insert("job", job);

    let workflow = Workflow::new(resources)
        .register(Step::AwaitJob, AwaitJob)
        .register(Step::Process, Process)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::AwaitJob).await?;
    assert_eq!(result, Step::Done);
    println!("\ncompleted at {result:?}");

    Ok(())
}
