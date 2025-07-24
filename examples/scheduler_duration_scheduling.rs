// Example: Duration-based Scheduling

use async_trait::async_trait;
use cano::prelude::*;
use tokio::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TaskState {
    Start,
    #[allow(dead_code)]
    End,
}

// Simple demo node
#[derive(Clone)]
struct DemoNode(String);

#[async_trait]
impl Node<TaskState> for DemoNode {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        println!("Executing {}", self.0);
    }

    async fn post(
        &self,
        _store: &impl Store,
        _result: Self::ExecResult,
    ) -> Result<TaskState, CanoError> {
        Ok(TaskState::End)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    let mut scheduler: Scheduler<TaskState, MemoryStore> = Scheduler::new();

    // Create different flows
    let mut daily_flow = Workflow::new(TaskState::Start);
    daily_flow.register_node(TaskState::Start, DemoNode("Daily Report".to_string()));
    daily_flow.add_exit_state(TaskState::End);

    let mut hourly_flow = Workflow::new(TaskState::Start);
    hourly_flow.register_node(TaskState::Start, DemoNode("Hourly Check".to_string()));
    hourly_flow.add_exit_state(TaskState::End);

    let mut frequent_flow = Workflow::new(TaskState::Start);
    frequent_flow.register_node(TaskState::Start, DemoNode("Frequent Task".to_string()));
    frequent_flow.add_exit_state(TaskState::End);

    let mut manual_flow = Workflow::new(TaskState::Start);
    manual_flow.register_node(TaskState::Start, DemoNode("Manual Task".to_string()));
    manual_flow.add_exit_state(TaskState::End);

    // ===============================
    // DURATION-BASED SCHEDULING
    // ===============================

    // Convenience methods for common intervals
    scheduler.every_hours("daily_report", daily_flow, 24)?; // Every 24 hours
    scheduler.every_minutes("hourly_check", hourly_flow, 60)?; // Every hour (60 minutes)
    scheduler.every_seconds("frequent_task", frequent_flow, 30)?; // Every 30 seconds

    // Precise Duration control for sub-second timing
    scheduler.every("high_freq", manual_flow, Duration::from_millis(500))?; // Every 500ms

    // Cron for complex schedules
    // scheduler.cron("business_hours", workflow, "0 9-17 * * 1-5")?;  // Weekdays 9-5

    // Manual triggers
    // scheduler.manual("on_demand", workflow)?;

    println!("🚀 Starting scheduler with multiple interval types...");
    scheduler.start().await?;

    // Let it run for demonstration
    tokio::time::sleep(Duration::from_secs(5)).await;

    println!("📊 Current status:");
    for flow_info in scheduler.list().await {
        println!(
            "  {} - {:?} (runs: {})",
            flow_info.id, flow_info.status, flow_info.run_count
        );
    }

    // Graceful shutdown
    println!("🛑 Stopping scheduler...");
    scheduler.stop().await?;
    println!("✅ Done!");

    Ok(())
}
