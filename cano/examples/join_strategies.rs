//! # Join Strategies — Comparing Any, Quorum, and Percentage
//!
//! Run with: `cargo run --example join_strategies`
//!
//! Demonstrates three [`JoinStrategy`] variants on the **same** set of four parallel
//! tasks with staggered completion times, so the strategies visibly differ.
//!
//! **Early-exit strategies** abort remaining tasks as soon as the threshold is met:
//!
//! - [`JoinStrategy::Any`] — succeeds after the first completion, aborts the rest.
//! - [`JoinStrategy::PartialResults(n)`] — succeeds after `n` completions (success or
//!   failure), aborts the rest.
//!
//! **Wait-for-all strategies** collect every result and then check the threshold:
//!
//! - [`JoinStrategy::Quorum(n)`] — waits for all tasks; succeeds if ≥ n succeeded.
//! - [`JoinStrategy::Percentage(p)`] — waits for all tasks; succeeds if ≥ p fraction
//!   of all tasks succeeded.
//!
//! Task timing (all succeed):
//!
//! ```text
//! Task A —  50 ms  finished first
//! Task B — 150 ms
//! Task C — 300 ms
//! Task D — 500 ms  finished last
//! ```
//!
//! Strategies compared in this demo:
//!
//! ```text
//! Any              → aborts at ~50 ms after A (1 success; B, C, D cancelled)
//! PartialResults(2)→ aborts at ~150 ms after A+B (2 completions; C, D cancelled)
//! Quorum(2)        → waits for all 4 (~500 ms), then confirms 4 >= 2
//! Percentage(0.5)  → waits for all 4 (~500 ms), then confirms 4/4 >= 50 %
//! ```
//!
//! Each run prints elapsed wall-clock time, how many completions were recorded, and the
//! merged next state so the difference between early-exit and wait-for-all is tangible.

use cano::prelude::*;
use std::time::{Duration, Instant};
use tokio::time::sleep;

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    /// Fan-out state: all four parallel tasks run here.
    Parallel,
    /// Merged into by the join; the workflow terminates here.
    Done,
}

// ---------------------------------------------------------------------------
// Parallel worker task
// ---------------------------------------------------------------------------

/// A single parallel worker. Sleeps for `delay_ms`, then records its completion
/// in the shared [`MemoryStore`] and returns `Step::Done`.
#[derive(Clone)]
struct Worker {
    id: &'static str,
    delay_ms: u64,
}

impl Worker {
    fn new(id: &'static str, delay_ms: u64) -> Self {
        Self { id, delay_ms }
    }
}

#[task(state = Step)]
impl Worker {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        sleep(Duration::from_millis(self.delay_ms)).await;
        store.put(self.id, format!("done after {}ms", self.delay_ms))?;
        println!("  [+] worker {} done ({}ms)", self.id, self.delay_ms);
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Helper — build the four workers with their staggered delays.
// ---------------------------------------------------------------------------

fn workers() -> Vec<Worker> {
    vec![
        Worker::new("A", 50),
        Worker::new("B", 150),
        Worker::new("C", 300),
        Worker::new("D", 500),
    ]
}

// ---------------------------------------------------------------------------
// Helper — run one strategy, print timing and result.
// ---------------------------------------------------------------------------

async fn run_strategy(label: &str, strategy: JoinStrategy) -> CanoResult<()> {
    println!("--- {} ---", label);
    let store = MemoryStore::new();
    let join_config = JoinConfig::new(strategy, Step::Done);

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register_split(Step::Parallel, workers(), join_config)
        .add_exit_state(Step::Done);

    let start = Instant::now();
    let result = workflow.orchestrate(Step::Parallel).await?;
    let elapsed = start.elapsed();

    // Count how many workers managed to log a result before being cancelled.
    let completed: usize = ["A", "B", "C", "D"]
        .iter()
        .filter(|&&id| store.get::<String>(id).is_ok())
        .count();

    println!(
        "  => strategy={label}, state={result:?}, elapsed={elapsed:?}, completions={completed}/4\n"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("=== Join Strategies Demo ===\n");

    // ── Early-exit strategies ───────────────────────────────────────────────

    // Any: proceed after the very first success (worker A at ~50 ms); abort B, C, D.
    run_strategy("Any (early-exit after 1st success)", JoinStrategy::Any).await?;

    // PartialResults(2): proceed after 2 completions (workers A+B at ~150 ms); abort C, D.
    run_strategy(
        "PartialResults(2) (early-exit after 2nd completion)",
        JoinStrategy::PartialResults(2),
    )
    .await?;

    // ── Wait-for-all strategies ─────────────────────────────────────────────

    // Quorum(2): waits for ALL tasks to finish (~500 ms), then verifies ≥ 2 succeeded.
    run_strategy(
        "Quorum(2) (waits for all, then checks threshold)",
        JoinStrategy::Quorum(2),
    )
    .await?;

    // Percentage(0.5): waits for ALL tasks (~500 ms), then verifies ≥ 50 % succeeded.
    run_strategy(
        "Percentage(0.5) (waits for all, then checks 50 %)",
        JoinStrategy::Percentage(0.5),
    )
    .await?;

    println!("=== Done ===");
    Ok(())
}
