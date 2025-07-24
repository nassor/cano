//! # Stream Scheduling Example
//!
//! This example demonstrates the Stream scheduler with multiple flows:
//! 1. **Hourly Report Flow**: Runs every minute using cron scheduling
//! 2. **Data Cleanup Flow**: Runs every 10 seconds using interval scheduling  
//! 3. **Manual Task Flow**: Only runs when manually triggered
//! 4. **One-time Setup Flow**: Runs once at a specific time
//!
//! The example showcases:
//! - Different scheduling modes (cron, interval, manual, once)
//! - Multiple concurrent flows
//! - Flow monitoring and status tracking
//! - Manual flow triggering
//!
//! Run with:
//! ```bash
//! cargo run --example stream_scheduling
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use chrono::Utc;
use std::collections::HashMap;
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
impl Node<WorkflowAction, DefaultParams, MemoryStore> for ReportNode {
    type PrepResult = String;
    type ExecResult = String;

    fn set_params(&mut self, _params: HashMap<String, String>) {}

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
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
        store: &impl Store,
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
impl Node<WorkflowAction, DefaultParams, MemoryStore> for CleanupNode {
    type PrepResult = Vec<String>;
    type ExecResult = usize;

    fn set_params(&mut self, _params: HashMap<String, String>) {}

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
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
        store: &impl Store,
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
impl Node<WorkflowAction, DefaultParams, MemoryStore> for ManualTaskNode {
    type PrepResult = String;
    type ExecResult = String;

    fn set_params(&mut self, _params: HashMap<String, String>) {}

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
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
        store: &impl Store,
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
impl Node<WorkflowAction, DefaultParams, MemoryStore> for SetupNode {
    type PrepResult = Vec<String>;
    type ExecResult = bool;

    fn set_params(&mut self, _params: HashMap<String, String>) {}

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
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
        store: &impl Store,
        exec_result: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        println!("🔧 Setup completed successfully: {}", exec_result);
        store.put("setup_complete", exec_result.to_string())?;
        Ok(WorkflowAction::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Starting Stream Scheduling Example");
    println!("=====================================");

    // Create flows
    let mut hourly_report_flow = Flow::new(WorkflowAction::Start);
    hourly_report_flow
        .register_node(WorkflowAction::Start, ReportNode::new("Hourly"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let mut cleanup_flow = Flow::new(WorkflowAction::Start);
    cleanup_flow
        .register_node(WorkflowAction::Start, CleanupNode::new("Temporary"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let mut manual_flow = Flow::new(WorkflowAction::Start);
    manual_flow
        .register_node(WorkflowAction::Start, ManualTaskNode::new("Data Migration"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    let mut setup_flow = Flow::new(WorkflowAction::Start);
    setup_flow
        .register_node(WorkflowAction::Start, SetupNode::new("System"))
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    // Create stream with multiple flows using builder pattern
    let one_time_run = Utc::now() + chrono::Duration::seconds(5);

    let mut stream = StreamBuilder::new()
        // Run hourly report every 5 seconds for demo to see concurrent executions
        .with_interval_flow("hourly_report", hourly_report_flow, 5)?
        // Run cleanup every 3 seconds for concurrent demo
        .with_interval_flow("data_cleanup", cleanup_flow, 3)?
        // Manual trigger only
        .with_manual_flow("manual_migration", manual_flow)?
        // One-time setup in 5 seconds
        .with_once_flow("system_setup", setup_flow, one_time_run)?
        .build();

    println!("📅 Configured flows:");
    println!("  • Hourly Report: Every 5 seconds (3s execution time - allows overlap)");
    println!("  • Data Cleanup: Every 3 seconds (2s execution time - allows overlap)");
    println!("  • Manual Migration: Manual trigger only");
    println!("  • System Setup: One-time in 5 seconds");
    println!();

    // Start the stream
    println!("▶️  Starting stream scheduler...");
    stream.start().await?;

    // Wait a bit and check flow status
    sleep(Duration::from_secs(2)).await;

    println!("📊 Current flow status:");
    let flows_info = stream.get_all_flows_info().await;
    for info in &flows_info {
        println!(
            "  • {}: {:?} (runs: {}, errors: {}, active: {})",
            info.id, info.status, info.run_count, info.error_count, info.active_instances
        );
        if let Some(next_run) = info.next_run {
            println!("    Next run: {}", next_run.format("%H:%M:%S"));
        }
    }
    println!();

    // Wait for the one-time setup to run
    println!("⏳ Waiting for one-time setup to execute...");
    sleep(Duration::from_secs(4)).await;

    // Manually trigger the migration task
    println!("🔧 Manually triggering data migration...");
    stream.trigger_flow("manual_migration").await?;

    // Let the stream run for a while to see scheduled executions
    println!("⏳ Running stream for 20 seconds to see concurrent executions...");
    sleep(Duration::from_secs(20)).await;

    // Show final status
    println!("\n📊 Final flow status:");
    let final_flows_info = stream.get_all_flows_info().await;
    for info in &final_flows_info {
        println!(
            "  • {}: {:?} (runs: {}, errors: {}, active: {})",
            info.id, info.status, info.run_count, info.error_count, info.active_instances
        );
    }

    // Stop the stream
    println!("\n⏹️  Stopping stream scheduler...");
    stream.stop().await?;

    println!("✅ Stream scheduling example completed!");
    Ok(())
}
