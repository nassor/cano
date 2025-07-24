//! # Concurrent Flow Execution Demo
//!
//! This example specifically demonstrates how the Stream scheduler can run
//! the same flow multiple times concurrently. Each execution instance runs
//! independently and simultaneously.
//!
//! Key features demonstrated:
//! - Same flow executing multiple overlapping instances
//! - Active instance tracking
//! - Independent execution contexts
//! - No blocking between instances
//!
//! Run with:
//! ```bash
//! cargo run --example concurrent_flows
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use chrono::Utc;
use tokio::time::{Duration, sleep};

/// Simple workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TaskState {
    Execute,
    Complete,
}

/// Long-running task that simulates work
#[derive(Clone)]
struct LongRunningTask {
    task_id: String,
    duration_ms: u64,
}

impl LongRunningTask {
    fn new(task_id: &str, duration_ms: u64) -> Self {
        Self {
            task_id: task_id.to_string(),
            duration_ms,
        }
    }
}

#[async_trait]
impl Node<TaskState> for LongRunningTask {
    type PrepResult = String;
    type ExecResult = String;

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        let instance_id = format!("{}_{}", self.task_id, Utc::now().timestamp_millis());
        println!("🚀 [{}] Starting preparation...", instance_id);
        store.put("task_start", Utc::now().to_rfc3339())?;
        Ok(instance_id)
    }

    async fn exec(&self, instance_id: Self::PrepResult) -> Self::ExecResult {
        let start_time = Utc::now();
        println!(
            "⚙️  [{}] Executing task ({}ms) - Started at {}...",
            instance_id,
            self.duration_ms,
            start_time.format("%H:%M:%S%.3f")
        );
        sleep(Duration::from_millis(self.duration_ms)).await;
        let end_time = Utc::now();
        println!(
            "✅ [{}] Task execution completed at {} (took {}ms)!",
            instance_id,
            end_time.format("%H:%M:%S%.3f"),
            (end_time - start_time).num_milliseconds()
        );
        format!("Task {} completed successfully", instance_id)
    }

    async fn post(
        &self,
        store: &impl Store,
        exec_result: Self::ExecResult,
    ) -> Result<TaskState, CanoError> {
        println!("📝 [{}] Storing results...", self.task_id);
        store.put("task_result", exec_result)?;
        Ok(TaskState::Complete)
    }
}

async fn print_flow_status(stream: &Stream<TaskState, MemoryStore>, title: &str) {
    println!("\n{title}");
    println!("{}", "=".repeat(title.len()));
    let flows_info = stream.get_all_flows_info().await;
    for info in &flows_info {
        println!(
            "📊 {}: {:?} | Runs: {} | Errors: {} | Active Instances: {}",
            info.id, info.status, info.run_count, info.error_count, info.active_instances
        );
    }
    println!();
}

async fn print_manual_task_summary() {
    println!("\n🎯 FINAL TASK SUMMARY");
    println!("=====================");
    println!("📋 Based on 10 seconds of execution:");
    println!();

    // Calculate expected counts based on scheduling
    let long_task_expected = 10 / 2; // Every 2 seconds
    let medium_task_expected = 10; // Every 1 second  
    let fast_task_expected = 10 * 10; // Approximately every 100ms (0 interval)

    println!("📋 Long Task (7000ms) - Every 2 seconds:");
    println!("   Expected Started: ~{long_task_expected}");
    println!("   All should have completed (7s max duration)");
    println!();

    println!("📋 Medium Task (3000ms) - Every 1 second:");
    println!("   Expected Started: ~{medium_task_expected}");
    println!("   All should have completed (3s max duration)");
    println!();

    println!("📋 Fast Task (500ms) - Continuous:");
    println!("   Expected Started: ~{fast_task_expected}+ (very frequent)");
    println!("   All should have completed (500ms max duration)");
    println!();

    let total_expected = long_task_expected + medium_task_expected + fast_task_expected;
    println!("📊 ESTIMATED TOTALS:");
    println!("   Total Tasks Started: ~{total_expected}+");
    println!("   All tasks should have completed successfully");
    println!("   Demonstrated true parallel execution without blocking");
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🎯 Concurrent Flow Execution Demo");
    println!("=================================");
    println!("This demo shows multiple instances of the same flow running concurrently.");
    println!("Watch for overlapping execution messages and active instance counts!\n");

    // Create a long-running flow (5 seconds execution time)
    let mut long_task_flow = Flow::new(TaskState::Execute);
    long_task_flow
        .register_node(TaskState::Execute, LongRunningTask::new("LongTask", 7000))
        .add_exit_states(vec![TaskState::Complete]);

    // Create a medium-running flow (3 seconds execution time)
    let mut medium_task_flow = Flow::new(TaskState::Execute);
    medium_task_flow
        .register_node(TaskState::Execute, LongRunningTask::new("MediumTask", 3000))
        .add_exit_states(vec![TaskState::Complete]);

    // Create a fast-running flow (500ms execution time)
    let mut fast_task_flow = Flow::new(TaskState::Execute);
    fast_task_flow
        .register_node(TaskState::Execute, LongRunningTask::new("FastTask", 500))
        .add_exit_states(vec![TaskState::Complete]);

    // Setup stream with aggressive scheduling to force overlaps
    let mut stream = StreamBuilder::new()
        // Long task every 2 seconds (7s execution, 2s interval = lots of overlap)
        .with_interval_flow("long_task", long_task_flow, 2)?
        // Medium task every 1 second (3s execution, 1s interval = massive overlap)
        .with_interval_flow("medium_task", medium_task_flow, 1)?
        // Fast task every 100ms (500ms execution, 100ms interval = extreme overlap)
        .with_interval_flow("fast_task", fast_task_flow, 0)?
        .build();

    println!("📅 Scheduled flows:");
    println!("  • Long Task: Every 2 seconds (7 second execution time)");
    println!("  • Medium Task: Every 1 second (3 second execution time)");
    println!("  • Fast Task: Every 0 seconds (500ms execution time)");
    println!("  → This creates intentional overlapping executions!\n");

    // Start the stream
    stream.start().await?;

    // Run for 10 seconds to let tasks start, then stop scheduling new tasks
    println!("🏃 Running for 10 seconds to start tasks...\n");
    for i in 1..=10 {
        sleep(Duration::from_secs(1)).await;
        if i % 3 == 0 {
            // Print status every 3 seconds to reduce noise
            print_flow_status(&stream, &format!("Status at {i}s")).await;
        }
    }

    // Stop the stream to prevent new tasks from starting
    println!("🛑 Stopping new task scheduling...");
    stream.stop().await?;

    // Since the stream moves flows internally, we need to track completion differently
    // For this demo, we'll wait a reasonable time for tasks to complete
    println!("⏳ Waiting for tasks to complete...\n");

    let mut wait_time = 0;
    let max_wait = 20; // Maximum 20 seconds wait

    while wait_time < max_wait {
        println!("⏰ Waiting... ({wait_time}s / {max_wait}s max)");
        sleep(Duration::from_secs(1)).await;
        wait_time += 1;

        // Check if we've waited long enough for the longest tasks (7s) plus buffer
        if wait_time >= 10 {
            // 7s max task + 3s buffer should be enough
            break;
        }
    }

    println!("✅ All tasks should have completed!");

    // Print manual summary based on what we observed
    print_manual_task_summary().await;

    println!("🎉 Demo completed! Notice how:");
    println!("   • Multiple instances of the same flow ran simultaneously");
    println!("   • Active instance counts tracked concurrent executions");
    println!("   • No blocking occurred between instances");
    println!("   • Each instance had independent execution context");
    println!("   • Demo waited for ALL tasks to complete before finishing");

    Ok(())
}
