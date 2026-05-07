#![cfg(feature = "scheduler")]
//! # Scheduler Backoff & Trip Example
//!
//! Demonstrates per-flow exponential backoff and the trip / `reset_flow`
//! lifecycle. The example wires a flaky workflow that fails on its first few
//! runs, observe the scheduler stretching the interval after each failure,
//! and then recover by calling `reset_flow` once it has tripped.
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_backoff --features scheduler
//! ```
use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FlowState {
    Start,
    Done,
}

/// Fails the first `fail_count` runs, then succeeds.
#[derive(Clone)]
struct FlakyJob {
    runs: Arc<AtomicU32>,
    fail_count: u32,
}

#[node(state = FlowState)]
impl FlakyJob {
    type PrepResult = ();
    type ExecResult = bool;

    fn config(&self) -> TaskConfig {
        // Disable per-task retries so the scheduler-level backoff is the only
        // retry layer in play. Otherwise each scheduled dispatch would already
        // retry internally before reaching the scheduler.
        TaskConfig::minimal()
    }

    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _prep: Self::PrepResult) -> Self::ExecResult {
        let n = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        let succeeded = n > self.fail_count;
        println!(
            "  [{}] run #{n} {}",
            chrono::Utc::now().format("%H:%M:%S%.3f"),
            if succeeded { "OK" } else { "FAIL" }
        );
        succeeded
    }

    async fn post(&self, _res: &Resources, ok: Self::ExecResult) -> Result<FlowState, CanoError> {
        if ok {
            Ok(FlowState::Done)
        } else {
            Err(CanoError::node_execution("flaky job failure"))
        }
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("⏰ Scheduler Backoff Example");
    println!("============================");

    let mut scheduler: Scheduler<FlowState> = Scheduler::new();

    // Will fail 4 times then succeed. With streak_limit=3 the flow trips after
    // the third failure and we need to call `reset_flow` to give it another
    // chance.
    let flaky = FlakyJob {
        runs: Arc::new(AtomicU32::new(0)),
        fail_count: 4,
    };

    let workflow = Workflow::bare()
        .register(FlowState::Start, flaky.clone())
        .add_exit_state(FlowState::Done);

    scheduler.every(
        "flaky",
        workflow,
        FlowState::Start,
        Duration::from_millis(200),
    )?;

    scheduler.set_backoff(
        "flaky",
        BackoffPolicy {
            initial: Duration::from_millis(300),
            multiplier: 2.0,
            max_delay: Duration::from_secs(2),
            jitter: 0.0,
            streak_limit: Some(3),
        },
    )?;

    let mut run_handle = scheduler.clone();
    let bg = tokio::spawn(async move { run_handle.start().await });

    // Watch the flow trip after 3 failures.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let snap = scheduler.status("flaky").await.expect("flow exists");
    println!("\nAfter ~1.5s — status: {:?}", snap.status);
    println!(
        "  run_count = {}, streak = {}",
        snap.run_count, snap.failure_streak
    );

    if matches!(snap.status, Status::Tripped { .. }) {
        println!("\nFlow tripped — calling reset_flow to give it another chance...");
        scheduler.reset_flow("flaky").await?;
    }

    // After reset the flow runs again, hits 1 more failure (run #4), then
    // succeeds on run #5.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let snap = scheduler.status("flaky").await.expect("flow exists");
    println!("\nAfter reset + ~1.5s — status: {:?}", snap.status);
    println!(
        "  run_count = {}, streak = {}",
        snap.run_count, snap.failure_streak
    );

    scheduler.stop().await?;
    bg.await
        .map_err(|e| CanoError::task_execution(format!("scheduler join failed: {e}")))??;

    Ok(())
}
