#![cfg(feature = "scheduler")]
//! # Scheduler with Mixed Workflows Example
//!
//! This example demonstrates the scheduler with various scheduling strategies:
//! 1. Manual trigger workflows
//! 2. Time-based recurring workflows (every N seconds)
//! 3. Multiple workflows running concurrently
//! 4. Status monitoring and workflow management
//!
//! For parallel task execution within a workflow, see examples like workflow_split_join.rs
//! which demonstrate split/join patterns.
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_mixed_workflows --features scheduler
//! ```

use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TaskState {
    Start,
    Complete,
}

#[derive(Clone)]
struct SimpleTask {
    task_id: String,
    execution_count: Arc<AtomicU32>,
}

impl SimpleTask {
    fn new(id: &str) -> Self {
        Self {
            task_id: id.to_string(),
            execution_count: Arc::new(AtomicU32::new(0)),
        }
    }

    fn get_execution_count(&self) -> u32 {
        self.execution_count.load(Ordering::SeqCst)
    }
}

#[cano::task]
impl Task<TaskState> for SimpleTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TaskState>, CanoError> {
        let prep_result = format!("Task {} prepared", self.task_id);
        let count = self.execution_count.fetch_add(1, Ordering::SeqCst) + 1;

        // Simulate some work
        tokio::time::sleep(Duration::from_millis(10)).await;

        let exec_result = format!("{} - Execution #{}", prep_result, count);

        let store = res.get::<MemoryStore, _>("store")?;
        store.put("last_execution", exec_result)?;
        Ok(TaskResult::Single(TaskState::Complete))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Scheduler with Mixed Workflows");
    println!("==================================\n");

    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Example 1: Manual workflows
    println!("📊 Setting up Manual Workflows");
    println!("-------------------------------");

    let task1 = SimpleTask::new("ManualTask1");
    let task1_clone = task1.clone();

    let workflow1 = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(TaskState::Start, task1)
        .add_exit_state(TaskState::Complete);

    scheduler.manual("manual_task_1", workflow1, TaskState::Start)?;
    println!("✅ Added manual workflow");

    // Example 2: Scheduled workflows
    println!("\n📊 Setting up Scheduled Workflows");
    println!("----------------------------------");

    let task2 = SimpleTask::new("ScheduledTask2");
    let task2_clone = task2.clone();

    let workflow2 = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(TaskState::Start, task2)
        .add_exit_state(TaskState::Complete);

    scheduler.every_seconds("scheduled_task_2", workflow2, TaskState::Start, 2)?;

    let task3 = SimpleTask::new("ScheduledTask3");
    let task3_clone = task3.clone();

    let workflow3 = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(TaskState::Start, task3)
        .add_exit_state(TaskState::Complete);

    scheduler.every_seconds("scheduled_task_3", workflow3, TaskState::Start, 3)?;
    println!("✅ Added 2 scheduled workflows");

    // Example 3: Pre-start summary (counts + ids known on the builder).
    println!("\n📊 Workflow Summary (pre-start)");
    println!("-------------------------------");
    println!("Registered: {} workflows", scheduler.len());
    println!();

    // Example 4: Start scheduler — consumes the builder, returns a live handle.
    println!("📊 Starting Scheduler");
    println!("---------------------");
    let running = scheduler.start().await?;

    // Example 5: Detailed status post-start.
    let workflows = running.list().await;
    for workflow in &workflows {
        println!("ID: {}", workflow.id);
        println!("  Status: {:?}", workflow.status);
        println!("  Run count: {}", workflow.run_count);
        println!("  Last run: {:?}", workflow.last_run);
        println!();
    }

    // Example 6: Trigger manual workflow
    println!("🔄 Triggering manual workflow...");
    running.trigger("manual_task_1").await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Example 7: Wait for scheduled workflows to run
    println!("\n⏳ Waiting for scheduled workflows to run...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Example 8: Check execution counts
    println!("\n📊 Execution Summary");
    println!("--------------------");
    println!(
        "ManualTask1 executed: {} times",
        task1_clone.get_execution_count()
    );
    println!(
        "ScheduledTask2 executed: {} times (every 2s)",
        task2_clone.get_execution_count()
    );
    println!(
        "ScheduledTask3 executed: {} times (every 3s)",
        task3_clone.get_execution_count()
    );

    // Example 9: Stop scheduler gracefully
    println!("\n🛑 Stopping scheduler...");
    running.stop().await?;
    println!("✅ Scheduler stopped successfully");

    println!("\n✅ Scheduler example completed successfully!");
    println!("\n💡 Tip: For parallel task execution within a workflow,");
    println!("   see examples/workflow_split_join.rs for split/join patterns");

    Ok(())
}
