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

use async_trait::async_trait;
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

/// Report generation node
#[derive(Clone)]
struct ReportNode {
    report_type: String,
}

impl ReportNode {
    fn new(report_type: &str) -> Self {
        Self {
            report_type: report_type.to_string(),
        }
    }
}

#[async_trait]
impl Node<WorkflowAction> for ReportNode {
    type PrepResult = String;
    type ExecResult = String;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("📊 Preparing {} report...", self.report_type);
        store.put("report_start_time", Utc::now().to_rfc3339())?;
        Ok(format!("Preparing {} report", self.report_type))
    }

    async fn exec(&self, _prep_result: Self::PrepResult) -> Self::ExecResult {
        println!("📊 Generating {} report...", self.report_type);
        // Simulate longer report generation to see concurrent executions
        sleep(Duration::from_millis(3000)).await;
        format!("{} report generated successfully", self.report_type)
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        println!("📊 Report completed: {}", exec_result);
        store.put("report_result", exec_result)?;
        Ok(WorkflowAction::Complete)
    }
}

/// Data cleanup node
#[derive(Clone)]
struct CleanupNode {
    cleanup_type: String,
}

impl CleanupNode {
    fn new(cleanup_type: &str) -> Self {
        Self {
            cleanup_type: cleanup_type.to_string(),
        }
    }
}

#[async_trait]
impl Node<WorkflowAction> for CleanupNode {
    type PrepResult = Vec<String>;
    type ExecResult = usize;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("🧹 Scanning for {} cleanup...", self.cleanup_type);
        store.put("cleanup_start", Utc::now().to_rfc3339())?;
        // Simulate finding items to clean
        Ok(vec![
            "temp_file_1".to_string(),
            "temp_file_2".to_string(),
            "old_log".to_string(),
        ])
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        println!("🧹 Cleaning up {} items...", prep_result.len());
        // Simulate longer cleanup work to see concurrent executions
        sleep(Duration::from_millis(2000)).await;
        prep_result.len()
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        println!("🧹 Cleanup completed: {} items removed", exec_result);
        store.put("cleanup_count", exec_result.to_string())?;
        Ok(WorkflowAction::Complete)
    }
}

/// Manual task node
#[derive(Clone)]
struct ManualTaskNode {
    task_name: String,
}

impl ManualTaskNode {
    fn new(task_name: &str) -> Self {
        Self {
            task_name: task_name.to_string(),
        }
    }
}

#[async_trait]
impl Node<WorkflowAction> for ManualTaskNode {
    type PrepResult = String;
    type ExecResult = String;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("⚡ Starting manual task: {}", self.task_name);
        store.put("manual_task_start", Utc::now().to_rfc3339())?;
        Ok(format!("Manual task: {}", self.task_name))
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        println!("⚡ Executing: {}", prep_result);
        // Simulate task execution
        sleep(Duration::from_millis(200)).await;
        format!("{} completed", prep_result)
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        println!("⚡ Manual task finished: {}", exec_result);
        store.put("manual_task_result", exec_result)?;
        Ok(WorkflowAction::Complete)
    }
}

/// Setup task node
#[derive(Clone)]
struct SetupNode {
    setup_type: String,
}

impl SetupNode {
    fn new(setup_type: &str) -> Self {
        Self {
            setup_type: setup_type.to_string(),
        }
    }
}

#[async_trait]
impl Node<WorkflowAction> for SetupNode {
    type PrepResult = Vec<String>;
    type ExecResult = bool;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("🔧 Preparing {} setup...", self.setup_type);
        store.put("setup_start", Utc::now().to_rfc3339())?;
        Ok(vec![
            "configure_database".to_string(),
            "setup_cache".to_string(),
            "initialize_logging".to_string(),
        ])
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        println!("🔧 Running setup tasks: {:?}", prep_result);
        // Simulate setup work
        sleep(Duration::from_millis(300)).await;
        true
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        println!("🔧 Setup completed successfully: {}", exec_result);
        store.put("setup_complete", exec_result.to_string())?;
        Ok(WorkflowAction::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Starting Scheduler Scheduling Example");
    println!("=====================================");

    let store = MemoryStore::new();

    // Create flows
    let hourly_report_flow = Workflow::new(store.clone())
        .register(WorkflowAction::Start, ReportNode::new("Hourly"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let cleanup_flow = Workflow::new(store.clone())
        .register(WorkflowAction::Start, CleanupNode::new("Temporary"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let manual_flow = Workflow::new(store.clone())
        .register(WorkflowAction::Start, ManualTaskNode::new("Data Migration"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let setup_flow = Workflow::new(store.clone())
        .register(WorkflowAction::Start, SetupNode::new("System"))
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

    println!("📅 Configured flows:");
    println!("  • Hourly Report: Every 5 seconds");
    println!("  • Data Cleanup: Every 3 seconds");
    println!("  • Manual Migration: Manual trigger only");
    println!("  • System Setup: Manual trigger only");
    println!();

    // Start the scheduler
    println!("▶️  Starting scheduler system...");
    scheduler.start().await?;

    // Wait a bit and check workflow status
    sleep(Duration::from_secs(2)).await;

    println!("📊 Current workflow status:");
    let flows_info = scheduler.list().await;
    for info in &flows_info {
        println!(
            "  • {}: {:?} (runs: {})",
            info.id, info.status, info.run_count
        );
    }
    println!();

    // Wait for the system setup and then trigger it manually
    println!("⏳ Waiting a bit then triggering system setup...");
    sleep(Duration::from_secs(4)).await;

    // Manually trigger the setup task
    println!("🔧 Manually triggering system setup...");
    scheduler.trigger("system_setup").await?;

    // Manually trigger the migration task
    println!("🔧 Manually triggering data migration...");
    scheduler.trigger("manual_migration").await?;

    // Let the scheduler run for a while to see scheduled executions
    println!("⏳ Running scheduler for 20 seconds to see concurrent executions...");
    sleep(Duration::from_secs(20)).await;

    // Show final status
    println!("\n📊 Final workflow status:");
    let final_flows_info = scheduler.list().await;
    for info in &final_flows_info {
        println!(
            "  • {}: {:?} (runs: {})",
            info.id, info.status, info.run_count
        );
    }

    // Stop the scheduler
    println!("\n⏹️  Stopping scheduler...");
    scheduler.stop().await?;

    println!("✅ Scheduler scheduling example completed!");
    Ok(())
}
