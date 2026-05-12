//! # Split / Join with Bulkhead Concurrency Limit
//!
//! Run with: `cargo run --example split_bulkhead`
//!
//! Demonstrates [`JoinConfig::with_bulkhead`] — a concurrency cap on the number of split
//! tasks that are allowed to execute simultaneously.
//!
//! Eight parallel tasks are registered with `register_split`, but
//! `JoinConfig::with_bulkhead(2)` limits execution to at most **2** tasks at a time. The
//! tasks record their start and finish timestamps into a shared resource so we can print
//! the overlap pattern and verify that at most 2 tasks ever ran concurrently.
//!
//! Expected output (approximate, four waves of two):
//!
//! ```text
//! wave 1: tasks 0 and 1 start together
//! wave 2: tasks 2 and 3 start after wave 1 finishes
//! wave 3: tasks 4 and 5 start after wave 2 finishes
//! wave 4: tasks 6 and 7 start after wave 3 finishes
//! ```

use cano::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Shared timestamp log — recorded by each task
// ---------------------------------------------------------------------------

/// Records (task_id, start_offset_ms, finish_offset_ms) triples for all tasks.
struct TimestampLog {
    epoch: Instant,
    entries: Arc<Mutex<Vec<(usize, u64, u64)>>>,
}

impl TimestampLog {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn record(&self, task_id: usize, start: Instant, finish: Instant) {
        let start_ms = start.duration_since(self.epoch).as_millis() as u64;
        let finish_ms = finish.duration_since(self.epoch).as_millis() as u64;
        self.entries
            .lock()
            .unwrap()
            .push((task_id, start_ms, finish_ms));
    }

    fn snapshot(&self) -> Vec<(usize, u64, u64)> {
        let mut v = self.entries.lock().unwrap().clone();
        v.sort_by_key(|&(id, start, _)| (start, id));
        v
    }
}

#[resource]
impl Resource for TimestampLog {}

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    ParallelWork,
    Summarize,
    Done,
}

// ---------------------------------------------------------------------------
// Worker task — simulates ~50ms of work and records timestamps
// ---------------------------------------------------------------------------

struct Worker {
    id: usize,
}

#[task(state = Step)]
impl Worker {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let log = res.get::<TimestampLog, _>("log")?;

        let start = Instant::now();
        println!(
            "  task {:>2}: start  (+{}ms)",
            self.id,
            start.duration_since(log.epoch).as_millis()
        );

        // Simulate 50ms of work for every task.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let finish = Instant::now();
        println!(
            "  task {:>2}: finish (+{}ms)",
            self.id,
            finish.duration_since(log.epoch).as_millis()
        );

        log.record(self.id, start, finish);
        Ok(TaskResult::Single(Step::Summarize))
    }
}

// ---------------------------------------------------------------------------
// Summary task — prints the overlap report
// ---------------------------------------------------------------------------

struct Summarize;

#[task(state = Step)]
impl Summarize {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let log = res.get::<TimestampLog, _>("log")?;
        let entries = log.snapshot();

        println!("\n  Timeline (task_id | start_ms | finish_ms):");
        for (id, start, finish) in &entries {
            println!("    {:>2}  {:>6}ms -> {:>6}ms", id, start, finish);
        }

        // Verify the bulkhead: count the maximum number of overlapping intervals.
        let mut max_concurrent = 0usize;
        for &(_, s1, f1) in &entries {
            let concurrent = entries
                .iter()
                .filter(|&&(_, s2, f2)| s2 < f1 && s1 < f2)
                .count();
            max_concurrent = max_concurrent.max(concurrent);
        }
        println!("\n  max concurrent tasks observed: {max_concurrent}");
        assert!(
            max_concurrent <= 2,
            "bulkhead=2 violated: {max_concurrent} tasks ran concurrently"
        );
        println!("  bulkhead=2 respected (max_concurrent={max_concurrent} <= 2)");

        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("=== Split / Join with Bulkhead Demo ===\n");
    println!("8 tasks, bulkhead=2: at most 2 run at a time (4 waves expected)\n");

    let log = TimestampLog::new();
    let resources = Resources::new().insert("log", log);

    // Eight workers, each taking ~50ms. With bulkhead=2 only 2 run at once,
    // so total time will be roughly 4 × 50ms = 200ms instead of 50ms flat.
    let workers: Vec<Worker> = (0..8).map(|id| Worker { id }).collect();

    let join_config = JoinConfig::new(JoinStrategy::All, Step::Summarize).with_bulkhead(2);

    let workflow = Workflow::new(resources)
        .register_split(Step::ParallelWork, workers, join_config)
        .register(Step::Summarize, Summarize)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::ParallelWork).await?;
    println!("\ncompleted at {result:?}");

    println!("\n=== Done ===");
    Ok(())
}
