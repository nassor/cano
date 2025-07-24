// Example: Graceful Shutdown with Timeout

use async_trait::async_trait;
use cano::prelude::*;
use tokio::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MyState {
    Start,
    #[allow(dead_code)]
    Processing,
    End,
}

// Long-running node that simulates work
#[derive(Clone)]
struct LongProcessingNode;

#[async_trait]
impl Node<MyState> for LongProcessingNode {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        // Simulate long-running work
        tokio::time::sleep(Duration::from_secs(5)).await;
        ()
    }

    async fn post(
        &self,
        _store: &impl Store,
        _result: Self::ExecResult,
    ) -> Result<MyState, CanoError> {
        Ok(MyState::End)
    }
}

// Quick node for comparison
#[derive(Clone)]
struct QuickNode;

#[async_trait]
impl Node<MyState> for QuickNode {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        ()
    }

    async fn post(
        &self,
        _store: &impl Store,
        _result: Self::ExecResult,
    ) -> Result<MyState, CanoError> {
        Ok(MyState::End)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    let mut scheduler: Scheduler<MyState, MemoryStore> = Scheduler::new();

    // Create flows with proper nodes
    let mut long_flow_builder = Workflow::new(MyState::Start);
    long_flow_builder.register_node(MyState::Start, LongProcessingNode);
    long_flow_builder.add_exit_state(MyState::End);

    let mut quick_flow_builder = Workflow::new(MyState::Start);
    quick_flow_builder.register_node(MyState::Start, QuickNode);
    quick_flow_builder.add_exit_state(MyState::End);

    // Add flows to scheduler
    scheduler.every_seconds("long_task", long_flow_builder, 10)?; // Every 10 seconds
    scheduler.every_seconds("quick_task", quick_flow_builder, 2)?; // Every 2 seconds

    // Start the scheduler
    scheduler.start().await?;

    // Let it run for a bit
    tokio::time::sleep(Duration::from_secs(3)).await;

    println!("Running flows: {}", scheduler.running_count().await);

    // ===============================
    // GRACEFUL SHUTDOWN OPTIONS
    // ===============================

    // Option 1: Default graceful stop (30 second timeout)
    println!("Stopping gracefully with default timeout...");
    match scheduler.stop().await {
        Ok(()) => println!("All flows completed gracefully"),
        Err(e) => println!("Timeout waiting for flows: {e}"),
    }

    // Option 2: Custom timeout
    // match scheduler.stop_with_timeout(Duration::from_secs(10)).await {
    //     Ok(()) => println!("All flows completed within 10 seconds"),
    //     Err(e) => println!("Timeout after 10 seconds: {}", e),
    // }

    // Option 3: Immediate stop (no waiting)
    // scheduler.stop_immediately().await?;
    // println!("Stopped immediately");

    // Option 4: Check status before stopping
    // if scheduler.has_running_flows().await {
    //     println!("Flows still running, waiting...");
    //     scheduler.stop_with_timeout(Duration::from_secs(60)).await?;
    // } else {
    //     scheduler.stop_immediately().await?;
    // }

    Ok(())
}

// ===============================
// SHUTDOWN PATTERNS
// ===============================

// Pattern 1: Graceful with fallback
#[allow(dead_code)]
async fn graceful_shutdown_pattern(
    scheduler: &mut Scheduler<MyState, MemoryStore>,
) -> CanoResult<()> {
    // Try graceful shutdown first
    match scheduler.stop_with_timeout(Duration::from_secs(30)).await {
        Ok(()) => {
            println!("✅ Graceful shutdown completed");
            Ok(())
        }
        Err(_) => {
            println!("⚠️  Timeout reached, forcing shutdown");
            scheduler.stop_immediately().await
        }
    }
}

// Pattern 2: Monitor and report
#[allow(dead_code)]
async fn monitored_shutdown(scheduler: &mut Scheduler<MyState, MemoryStore>) -> CanoResult<()> {
    println!("🛑 Initiating shutdown...");

    if scheduler.has_running_flows().await {
        let count = scheduler.running_count().await;
        println!("📊 Waiting for {count} running flows to complete");

        scheduler.stop_with_timeout(Duration::from_secs(60)).await?;
        println!("✅ All flows completed");
    } else {
        scheduler.stop_immediately().await?;
        println!("✅ No running flows, stopped immediately");
    }

    Ok(())
}

// Pattern 3: Progressive timeout
#[allow(dead_code)]
async fn progressive_shutdown(scheduler: &mut Scheduler<MyState, MemoryStore>) -> CanoResult<()> {
    // First attempt: short timeout
    if scheduler
        .stop_with_timeout(Duration::from_secs(10))
        .await
        .is_ok()
    {
        println!("✅ Quick shutdown successful");
        return Ok(());
    }

    // Second attempt: longer timeout
    println!("⏳ Extending timeout...");
    if scheduler
        .stop_with_timeout(Duration::from_secs(30))
        .await
        .is_ok()
    {
        println!("✅ Extended shutdown successful");
        return Ok(());
    }

    // Final: force stop
    println!("🚨 Forcing immediate shutdown");
    scheduler.stop_immediately().await
}
