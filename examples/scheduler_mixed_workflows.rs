//! # Enhanced Scheduler with Concurrent Workflows Example
//!
//! This example demonstrates the enhanced scheduler that supports both regular
//! and concurrent workflows with various scheduling strategies.

use cano::prelude::*;
use cano::scheduler::Status;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TaskState {
    Start,
    Complete,
    #[allow(dead_code)]
    Error,
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

    #[allow(dead_code)]
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
    println!("🚀 Enhanced Scheduler with Concurrent Workflows");
    println!("===============================================\n");

    let mut scheduler = Scheduler::new();

    // Example 1: Regular workflows
    println!("📊 Setting up Regular Workflows");
    println!("-------------------------------");

    // Create regular workflows
    let mut regular_workflow1 = Workflow::new(TaskState::Start);
    regular_workflow1.add_exit_state(TaskState::Complete);
    regular_workflow1.register_node(TaskState::Start, SimpleTask::new("RegularTask1"));

    let mut regular_workflow2 = Workflow::new(TaskState::Start);
    regular_workflow2.add_exit_state(TaskState::Complete);
    regular_workflow2.register_node(TaskState::Start, SimpleTask::new("RegularTask2"));

    // Schedule regular workflows
    scheduler.manual("regular_task_1", regular_workflow1)?;
    scheduler.every_seconds("regular_task_2", regular_workflow2, 2)?;

    println!("✅ Added 2 regular workflows");

    // Example 2: Concurrent workflows
    println!("\n📊 Setting up Concurrent Workflows");
    println!("----------------------------------");

    // Create concurrent workflow directly
    let mut concurrent_workflow = ConcurrentWorkflow::new(TaskState::Start);
    concurrent_workflow.add_exit_state(TaskState::Complete);

    concurrent_workflow.register_node(TaskState::Start, SimpleTask::new("ConcurrentTask"));

    // Schedule concurrent workflows with different strategies
    scheduler.manual_concurrent(
        "concurrent_batch_1",
        concurrent_workflow.clone(),
        3, // 3 instances
        WaitStrategy::WaitForever,
    )?;

    scheduler.every_seconds_concurrent(
        "concurrent_batch_2",
        concurrent_workflow.clone(),
        5,                             // every 5 seconds
        5,                             // 5 instances
        WaitStrategy::WaitForQuota(3), // wait for 3 to complete
    )?;

    scheduler.manual_concurrent(
        "concurrent_batch_3",
        concurrent_workflow,
        4,                                                      // 4 instances
        WaitStrategy::WaitDuration(Duration::from_millis(200)), // 200ms timeout
    )?;

    println!("✅ Added 3 concurrent workflows");

    // Example 3: List all workflows
    println!("\n📊 Workflow Summary");
    println!("------------------");

    let workflows = scheduler.list().await;
    for workflow in &workflows {
        println!("ID: {}", workflow.id);
        println!("  Status: {:?}", workflow.status);
        println!("  Run count: {}", workflow.run_count);
        if let Some(instances) = workflow.concurrent_instances {
            println!("  Concurrent instances: {instances}");
        }
        println!("  Last run: {:?}", workflow.last_run);
        println!();
    }

    // Example 4: Start scheduler and trigger manual workflows
    println!("📊 Starting Scheduler and Triggering Manual Workflows");
    println!("-----------------------------------------------------");

    scheduler.start().await?;

    // Trigger manual workflows
    println!("🔄 Triggering regular manual workflow...");
    scheduler.trigger("regular_task_1").await?;

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("🔄 Triggering concurrent manual workflow (WaitForever)...");
    scheduler.trigger("concurrent_batch_1").await?;

    tokio::time::sleep(Duration::from_millis(200)).await;

    println!("🔄 Triggering concurrent manual workflow (WaitDuration)...");
    scheduler.trigger("concurrent_batch_3").await?;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Example 5: Check status of workflows
    println!("\n📊 Workflow Status After Execution");
    println!("----------------------------------");

    for workflow_id in ["regular_task_1", "concurrent_batch_1", "concurrent_batch_3"] {
        if let Some(info) = scheduler.status(workflow_id).await {
            println!("Workflow: {}", info.id);
            println!("  Status: {:?}", info.status);
            println!("  Run count: {}", info.run_count);
            if let Status::ConcurrentCompleted(status) = &info.status {
                println!("  Total workflows: {}", status.total_workflows);
                println!("  Completed: {}", status.completed);
                println!("  Failed: {}", status.failed);
                println!("  Cancelled: {}", status.cancelled);
                println!("  Duration: {:?}", status.duration);
            }
            println!();
        }
    }

    // Example 6: Running workflows tracking
    println!("📊 Scheduler Monitoring");
    println!("----------------------");

    println!(
        "Has running workflows: {}",
        scheduler.has_running_flows().await
    );
    println!(
        "Running workflow count: {}",
        scheduler.running_count().await
    );

    // Wait a bit for any scheduled workflows
    println!("\n⏳ Waiting for scheduled workflows to run...");
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Example 7: Stop scheduler gracefully
    println!("\n📊 Stopping Scheduler");
    println!("--------------------");

    scheduler.stop().await?;
    println!("✅ Scheduler stopped gracefully");

    // Final status
    let final_workflows = scheduler.list().await;
    println!("\n📊 Final Workflow Summary");
    println!("-------------------------");

    for workflow in &final_workflows {
        println!(
            "ID: {} - Runs: {} - Status: {:?}",
            workflow.id, workflow.run_count, workflow.status
        );
    }

    println!("\n✅ Enhanced Scheduler example completed successfully!");

    Ok(())
}
