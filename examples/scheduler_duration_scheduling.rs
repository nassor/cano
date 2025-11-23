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

// Example: Duration-based Scheduling

use async_trait::async_trait;
use cano::prelude::*;
use tokio::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TaskState {
    Start,
    Complete,
}

// Demo nodes for different schedules
#[derive(Clone)]
struct DailyTask;

#[async_trait]
impl Node<TaskState> for DailyTask {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        println!(
            "📅 Daily task executed at {}",
            chrono::Utc::now().format("%H:%M:%S")
        );
    }

    async fn post(
        &self,
        _store: &MemoryStore,
        _result: Self::ExecResult,
    ) -> Result<TaskState, CanoError> {
        Ok(TaskState::Complete)
    }
}

#[derive(Clone)]
struct HourlyTask;

#[async_trait]
impl Node<TaskState> for HourlyTask {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        println!(
            "⏰ Hourly task executed at {}",
            chrono::Utc::now().format("%H:%M:%S")
        );
    }

    async fn post(
        &self,
        _store: &MemoryStore,
        _result: Self::ExecResult,
    ) -> Result<TaskState, CanoError> {
        Ok(TaskState::Complete)
    }
}

#[derive(Clone)]
struct FrequentTask;

#[async_trait]
impl Node<TaskState> for FrequentTask {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        println!(
            "🔄 Frequent task executed at {}",
            chrono::Utc::now().format("%H:%M:%S")
        );
    }

    async fn post(
        &self,
        _store: &MemoryStore,
        _result: Self::ExecResult,
    ) -> Result<TaskState, CanoError> {
        Ok(TaskState::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("⏰ Duration-Based Scheduling Example");
    println!("====================================");

    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Create workflows with different durations
    let daily_flow = Workflow::new(store.clone())
        .register(TaskState::Start, DailyTask)
        .add_exit_state(TaskState::Complete);

    let hourly_flow = Workflow::new(store.clone())
        .register(TaskState::Start, HourlyTask)
        .add_exit_state(TaskState::Complete);

    let frequent_flow = Workflow::new(store.clone())
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

    println!("📅 Scheduled workflows:");
    println!("  • Daily task: Every 4 seconds (simulated)");
    println!("  • Hourly task: Every 2 seconds (simulated)");
    println!("  • Frequent task: Every 1 second");
    println!();

    // Start the scheduler in background
    let mut scheduler_handle = scheduler.clone();
    let scheduler_task = tokio::spawn(async move {
        println!("Scheduler background task started.");
        if let Err(e) = scheduler_handle.start().await {
            eprintln!("Scheduler failed: {}", e);
        }
        println!("Scheduler background task finished.");
    });

    println!("🚀 Scheduler started! Running for 10 seconds...");
    tokio::time::sleep(Duration::from_secs(10)).await;

    scheduler.stop().await?;
    println!("✅ Scheduler stopped gracefully");

    // Await the scheduler task to ensure it cleans up
    scheduler_task
        .await
        .map_err(|e| CanoError::task_execution(format!("Scheduler task failed: {}", e)))?;

    Ok(())
}
