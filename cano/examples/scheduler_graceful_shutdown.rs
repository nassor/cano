#![cfg(feature = "scheduler")]
//! # Graceful Shutdown Example
//!
//! Start a scheduler, wait for Ctrl-C (or SIGTERM on Unix), then call
//! [`RunningScheduler::stop`], which shuts down in bounded time:
//!
//! 1. **Drain** — stop dispatching and let in-flight runs finish (default 20 s).
//! 2. **Cancel** — fire the runs' cancellation tokens (default 5 s).
//! 3. **Abort** — drop whatever is still running and report it.
//! 4. **Teardown** — release every workflow's resources.
//!
//! The long task below is usually mid-run when the signal arrives; the drain
//! phase lets it print its completion line before "Shutdown complete!".
//!
//! On Unix the example sends itself SIGINT after 15 seconds so it terminates on
//! its own; elsewhere press Ctrl-C.
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_graceful_shutdown --features scheduler
//! ```

use cano::prelude::*;
use tokio::time::{Duration, sleep};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MyState {
    Start,
    Processing,
    End,
}

/// Long-running task that simulates work
#[derive(Clone)]
struct LongProcessingTask {
    duration_secs: u64,
}

impl LongProcessingTask {
    fn new(duration_secs: u64) -> Self {
        Self { duration_secs }
    }
}

#[task(state = MyState)]
impl LongProcessingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<MyState>, CanoError> {
        println!("Starting long task ({}s)", self.duration_secs);

        // Simulate long-running work with progress updates
        for i in 1..=self.duration_secs {
            sleep(Duration::from_secs(1)).await;
            println!("  Long task progress: {}/{}s", i, self.duration_secs);
        }
        println!("Long task completed");

        let result = format!("Long task finished after {}s", self.duration_secs);
        let store = res.get::<MemoryStore>()?;
        store.put("long_task_result", result);
        Ok(TaskResult::Single(MyState::Processing))
    }
}

/// Task that handles the result
#[derive(Clone)]
struct ProcessingTask;

#[task(state = MyState)]
impl ProcessingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<MyState>, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let result: String = store
            .get("long_task_result")
            .unwrap_or_else(|_| "No result available".to_string());

        println!("Processing result: {}", result);
        sleep(Duration::from_secs(1)).await;

        let processed = format!("Processed: {}", result);
        store.put("processed_result", processed);
        println!("Processing completed");
        Ok(TaskResult::Single(MyState::End))
    }
}

/// Quick task for comparison
#[derive(Clone)]
struct QuickTask;

#[task(state = MyState)]
impl QuickTask {
    async fn run_bare(&self) -> Result<TaskResult<MyState>, CanoError> {
        println!("Quick task starting");
        println!("Quick task completed");
        Ok(TaskResult::Single(MyState::End))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Scheduler Graceful Shutdown Example");
    println!("================================================\n");

    let store = MemoryStore::new();

    let long_flow = Workflow::new(Resources::new().insert(store.clone()))
        .register(MyState::Start, LongProcessingTask::new(5))
        .register(MyState::Processing, ProcessingTask)
        .add_exit_state(MyState::End);

    let quick_flow = Workflow::new(Resources::new().insert(store.clone()))
        .register(MyState::Start, QuickTask)
        .add_exit_state(MyState::End);

    println!("Scheduling workflows:");
    println!("  Long task: Every 10 seconds (takes 5s to complete + 1s processing)");
    println!("  Quick task: Every 2 seconds (instant completion)\n");

    let running = Scheduler::new()
        .every(
            "long_task",
            long_flow,
            MyState::Start,
            Duration::from_secs(10),
        )
        .every(
            "quick_task",
            quick_flow,
            MyState::Start,
            Duration::from_secs(2),
        )
        .with_drain_timeout(Duration::from_secs(20))
        .start()
        .await?;
    println!("Scheduler started");
    println!("Press Ctrl+C to trigger graceful shutdown\n");

    // DEMO: interrupt ourselves after 15 seconds so the example terminates on its own.
    let pid = std::process::id();
    tokio::spawn(async move {
        println!("Demo will automatically trigger Ctrl+C in 15 seconds...");
        sleep(Duration::from_secs(15)).await;
        println!("\nDemo time limit reached - sending SIGINT...");

        #[cfg(unix)]
        let _ = std::process::Command::new("kill")
            .arg("-s")
            .arg("INT")
            .arg(pid.to_string())
            .output();

        #[cfg(not(unix))]
        println!(
            "Automatic SIGINT not supported on this platform, please press Ctrl+C manually \
             (or terminate pid {pid})"
        );
    });

    cano::scheduler::shutdown_signal().await?;
    println!("\n\nShutdown signal received - draining in-flight workflows...");
    running.stop().await?;

    println!("\nShutdown complete!");
    Ok(())
}
