#![cfg(feature = "scheduler")]
//! # Duration-based Scheduling Example
//!
//! This example demonstrates how to use the Scheduler with custom Duration intervals:
//! 1. **Short Task**: Runs every 2 seconds with a quick execution
//! 2. **Medium Task**: Runs every 5 seconds with moderate processing
//! 3. **Long Task**: Runs every 10 seconds with extended execution time
//!
//! The example showcases:
//! - Duration-based scheduling using `scheduler.every()`
//! - Multiple workflows with different intervals
//! - Concurrent execution of scheduled tasks
//! - Precise timing control with std::time::Duration
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_duration_scheduling
//! ```

use cano::prelude::*;
use tokio::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TaskState {
    Start,
    Complete,
}

#[derive(Clone)]
struct DailyTask;

#[task(state = TaskState)]
impl DailyTask {
    async fn run_bare(&self) -> Result<TaskResult<TaskState>, CanoError> {
        println!(
            "Daily task executed at {}",
            chrono::Utc::now().format("%H:%M:%S")
        );
        Ok(TaskResult::Single(TaskState::Complete))
    }
}

#[derive(Clone)]
struct HourlyTask;

#[task(state = TaskState)]
impl HourlyTask {
    async fn run_bare(&self) -> Result<TaskResult<TaskState>, CanoError> {
        println!(
            "Hourly task executed at {}",
            chrono::Utc::now().format("%H:%M:%S")
        );
        Ok(TaskResult::Single(TaskState::Complete))
    }
}

#[derive(Clone)]
struct FrequentTask;

#[task(state = TaskState)]
impl FrequentTask {
    async fn run_bare(&self) -> Result<TaskResult<TaskState>, CanoError> {
        println!(
            "Frequent task executed at {}",
            chrono::Utc::now().format("%H:%M:%S")
        );
        Ok(TaskResult::Single(TaskState::Complete))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("Duration-Based Scheduling Example");
    println!("====================================");

    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Create workflows with different durations
    let daily_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(TaskState::Start, DailyTask)
        .add_exit_state(TaskState::Complete);

    let hourly_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(TaskState::Start, HourlyTask)
        .add_exit_state(TaskState::Complete);

    let frequent_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(TaskState::Start, FrequentTask)
        .add_exit_state(TaskState::Complete);

    // Schedule workflows with different durations
    scheduler.every(
        "daily_task",
        daily_flow,
        TaskState::Start,
        Duration::from_secs(4),
    )?; // Simulated daily (every 4s)
    scheduler.every(
        "hourly_task",
        hourly_flow,
        TaskState::Start,
        Duration::from_secs(2),
    )?; // Simulated hourly (every 2s)
    scheduler.every(
        "frequent_task",
        frequent_flow,
        TaskState::Start,
        Duration::from_secs(1),
    )?; // Frequent (every 1s)

    println!("Scheduled workflows:");
    println!("  Daily task: Every 4 seconds (simulated)");
    println!("  Hourly task: Every 2 seconds (simulated)");
    println!("  Frequent task: Every 1 second");
    println!();

    // Start consumes the builder and returns a live handle.
    let running = scheduler.start().await?;

    println!("Scheduler started! Running for 10 seconds...");
    tokio::time::sleep(Duration::from_secs(10)).await;

    running.stop().await?;
    println!("Scheduler stopped gracefully");

    Ok(())
}
