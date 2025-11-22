#![cfg(feature = "scheduler")]
//! # Scheduler with Mixed Workflows Example
//!
//! This example demonstrates the scheduler with various scheduling strategies:
//! 1. Manual trigger workflows
//! 2. Time-based recurring workflows (every N seconds)
//! 3. Multiple workflows running concurrently
//! 4. Status monitoring and workflow management
//!
//! For concurrent task execution within a workflow, see the workflow_concurrent.rs example
//! which demonstrates the split/join pattern for parallel processing.

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

#[async_trait::async_trait]
impl Node<TaskState> for SimpleTask {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        Ok(format!("Task {} prepared", self.task_id))
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        let count = self.execution_count.fetch_add(1, Ordering::SeqCst) + 1;

        // Simulate some work
        tokio::time::sleep(Duration::from_millis(10)).await;

        format!("{} - Execution #{}", prep_result, count)
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<TaskState, CanoError> {
        store.put("last_execution", exec_result)?;
        Ok(TaskState::Complete)
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

    let workflow1 = Workflow::new(store.clone())
        .register(TaskState::Start, task1)
        .add_exit_state(TaskState::Complete);

    scheduler.manual("manual_task_1", workflow1, TaskState::Start)?;
    println!("✅ Added manual workflow");

    // Example 2: Scheduled workflows
    println!("\n📊 Setting up Scheduled Workflows");
    println!("----------------------------------");

    let task2 = SimpleTask::new("ScheduledTask2");
    let task2_clone = task2.clone();

    let workflow2 = Workflow::new(store.clone())
        .register(TaskState::Start, task2)
        .add_exit_state(TaskState::Complete);

    scheduler.every_seconds("scheduled_task_2", workflow2, TaskState::Start, 2)?;

    let task3 = SimpleTask::new("ScheduledTask3");
    let task3_clone = task3.clone();

    let workflow3 = Workflow::new(store.clone())
        .register(TaskState::Start, task3)
        .add_exit_state(TaskState::Complete);

    scheduler.every_seconds("scheduled_task_3", workflow3, TaskState::Start, 3)?;
    println!("✅ Added 2 scheduled workflows");

    // Example 3: List all workflows
    println!("\n📊 Workflow Summary");
    println!("------------------");

    let workflows = scheduler.list().await;
    for workflow in &workflows {
        println!("ID: {}", workflow.id);
        println!("  Status: {:?}", workflow.status);
        println!("  Run count: {}", workflow.run_count);
        println!("  Last run: {:?}", workflow.last_run);
        println!();
    }

    // Example 4: Start scheduler
    println!("📊 Starting Scheduler");
    println!("---------------------");

    let scheduler_clone = scheduler.clone();
    tokio::spawn(async move {
        scheduler.start().await.expect("Scheduler failed");
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Example 5: Trigger manual workflow
    println!("🔄 Triggering manual workflow...");
    scheduler_clone.trigger("manual_task_1").await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Example 6: Wait for scheduled workflows to run
    println!("\n⏳ Waiting for scheduled workflows to run...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Example 7: Check execution counts
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

    // Example 8: Stop scheduler gracefully
    println!("\n🛑 Stopping scheduler...");
    scheduler_clone.stop().await?;
    println!("✅ Scheduler stopped successfully");

    println!("\n✅ Scheduler example completed successfully!");
    println!("\n💡 Tip: For concurrent task execution within a workflow,");
    println!("   see examples/workflow_concurrent.rs for split/join patterns");

    Ok(())
}
