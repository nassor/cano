#![cfg(feature = "scheduler")]
//! # Scheduler Scheduling Example
//!
//! This example demonstrates the Scheduler scheduler with multiple flows:
//! 1. **Hourly Report Workflow**: Runs every minute using cron scheduling
//! 2. **Data Cleanup Workflow**: Runs every 10 seconds using interval scheduling
//! 3. **Manual Task Workflow**: Only runs when manually triggered
//! 4. **One-time Setup Workflow**: Runs once at a specific time
//!
//! The example showcases:
//! - Different scheduling modes (cron, interval, manual, once)
//! - Multiple concurrent flows
//! - Workflow monitoring and status tracking
//! - Manual workflow triggering
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_scheduling
//! ```

use cano::prelude::*;
use chrono::Utc;
use tokio::time::{Duration, sleep};

/// Workflow states for our example flows
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowAction {
    Start,
    Complete,
    Error,
}

/// Report generation task
#[derive(Clone)]
struct ReportTask {
    report_type: String,
}

impl ReportTask {
    fn new(report_type: &str) -> Self {
        Self {
            report_type: report_type.to_string(),
        }
    }
}

#[task(state = WorkflowAction)]
impl ReportTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<WorkflowAction>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Preparing {} report...", self.report_type);
        store.put("report_start_time", Utc::now().to_rfc3339())?;

        println!("Generating {} report...", self.report_type);
        // Simulate longer report generation to see concurrent executions
        sleep(Duration::from_millis(3000)).await;
        let result = format!("{} report generated successfully", self.report_type);

        println!("Report completed: {}", result);
        store.put("report_result", result)?;
        Ok(TaskResult::Single(WorkflowAction::Complete))
    }
}

/// Data cleanup task
#[derive(Clone)]
struct CleanupTask {
    cleanup_type: String,
}

impl CleanupTask {
    fn new(cleanup_type: &str) -> Self {
        Self {
            cleanup_type: cleanup_type.to_string(),
        }
    }
}

#[task(state = WorkflowAction)]
impl CleanupTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<WorkflowAction>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Scanning for {} cleanup...", self.cleanup_type);
        store.put("cleanup_start", Utc::now().to_rfc3339())?;

        let items = [
            "temp_file_1".to_string(),
            "temp_file_2".to_string(),
            "old_log".to_string(),
        ];
        println!("Cleaning up {} items...", items.len());
        // Simulate longer cleanup work to see concurrent executions
        sleep(Duration::from_millis(2000)).await;

        let count = items.len();
        println!("Cleanup completed: {} items removed", count);
        store.put("cleanup_count", count.to_string())?;
        Ok(TaskResult::Single(WorkflowAction::Complete))
    }
}

/// Manual task
#[derive(Clone)]
struct ManualTask {
    task_name: String,
}

impl ManualTask {
    fn new(task_name: &str) -> Self {
        Self {
            task_name: task_name.to_string(),
        }
    }
}

#[task(state = WorkflowAction)]
impl ManualTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<WorkflowAction>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Starting manual task: {}", self.task_name);
        store.put("manual_task_start", Utc::now().to_rfc3339())?;

        let prep = format!("Manual task: {}", self.task_name);
        println!("Executing: {}", prep);
        // Simulate task execution
        sleep(Duration::from_millis(200)).await;

        let result = format!("{} completed", prep);
        println!("Manual task finished: {}", result);
        store.put("manual_task_result", result)?;
        Ok(TaskResult::Single(WorkflowAction::Complete))
    }
}

/// Setup task
#[derive(Clone)]
struct SetupTask {
    setup_type: String,
}

impl SetupTask {
    fn new(setup_type: &str) -> Self {
        Self {
            setup_type: setup_type.to_string(),
        }
    }
}

#[task(state = WorkflowAction)]
impl SetupTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<WorkflowAction>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Preparing {} setup...", self.setup_type);
        store.put("setup_start", Utc::now().to_rfc3339())?;

        let steps = vec![
            "configure_database".to_string(),
            "setup_cache".to_string(),
            "initialize_logging".to_string(),
        ];
        println!("Running setup tasks: {:?}", steps);
        // Simulate setup work
        sleep(Duration::from_millis(300)).await;

        println!("Setup completed successfully");
        store.put("setup_complete", "true".to_string())?;
        Ok(TaskResult::Single(WorkflowAction::Complete))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("Starting Scheduler Scheduling Example");
    println!("=====================================");

    let store = MemoryStore::new();

    // Create flows
    let hourly_report_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(WorkflowAction::Start, ReportTask::new("Hourly"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let cleanup_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(WorkflowAction::Start, CleanupTask::new("Temporary"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let manual_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(WorkflowAction::Start, ManualTask::new("Data Migration"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let setup_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(WorkflowAction::Start, SetupTask::new("System"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    // Create scheduler with multiple flows
    let mut scheduler = Scheduler::new();

    // Run hourly report every 5 seconds for demo to see concurrent executions
    scheduler.every_seconds(
        "hourly_report",
        hourly_report_flow,
        WorkflowAction::Start,
        5,
    )?;
    // Run cleanup every 3 seconds for concurrent demo
    scheduler.every_seconds("data_cleanup", cleanup_flow, WorkflowAction::Start, 3)?;
    // Manual trigger only
    scheduler.manual("manual_migration", manual_flow, WorkflowAction::Start)?;
    // System setup
    scheduler.manual("system_setup", setup_flow, WorkflowAction::Start)?;

    println!("Configured flows:");
    println!("  Hourly Report: Every 5 seconds");
    println!("  Data Cleanup: Every 3 seconds");
    println!("  Manual Migration: Manual trigger only");
    println!("  System Setup: Manual trigger only");
    println!();

    // Start the scheduler — consumes the builder and returns a live handle.
    println!("Starting scheduler system...");
    let running = scheduler.start().await?;

    // Wait a bit and check workflow status
    sleep(Duration::from_secs(2)).await;

    println!("Current workflow status:");
    let flows_info = running.list().await;
    for info in &flows_info {
        println!(
            "  {}: {:?} (runs: {})",
            info.id, info.status, info.run_count
        );
    }
    println!();

    // Wait for the system setup and then trigger it manually
    println!("Waiting a bit then triggering system setup...");
    sleep(Duration::from_secs(4)).await;

    // Manually trigger the setup task
    println!("Manually triggering system setup...");
    running.trigger("system_setup").await?;

    // Manually trigger the migration task
    println!("Manually triggering data migration...");
    running.trigger("manual_migration").await?;

    // Let the scheduler run for a while to see scheduled executions
    println!("Running scheduler for 20 seconds to see concurrent executions...");
    sleep(Duration::from_secs(20)).await;

    // Show final status
    println!("\nFinal workflow status:");
    let final_flows_info = running.list().await;
    for info in &final_flows_info {
        println!(
            "  {}: {:?} (runs: {})",
            info.id, info.status, info.run_count
        );
    }

    // Stop the scheduler — sends Stop and awaits graceful shutdown.
    println!("\nStopping scheduler...");
    running.stop().await?;

    println!("Scheduler scheduling example completed!");
    Ok(())
}
