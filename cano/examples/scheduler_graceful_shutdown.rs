#![cfg(feature = "scheduler")]
//! # Graceful Shutdown Example
//!
//! This example demonstrates how to implement graceful shutdown handling in the Scheduler:
//! 1. **Long Processing Workflow**: Simulates a long-running task that takes several seconds
//! 2. **Graceful Shutdown**: Shows how to stop the scheduler cleanly
//! 3. **Status Monitoring**: Demonstrates checking running workflows before shutdown
//!
//! The example showcases:
//! - Scheduler graceful shutdown with `scheduler.stop()`
//! - Checking for running workflows with `scheduler.has_running_flows()`
//! - Proper cleanup of resources and active tasks
//! - Manual timeout implementation for shutdown
//!
//! Key features:
//! - Tasks in progress are allowed to complete during shutdown
//! - Custom timeout implementation to prevent indefinite waiting
//! - Clean resource cleanup and status reporting
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_graceful_shutdown --features scheduler
//! ```

use cano::prelude::*;
use cano::scheduler::Status;
use tokio::time::{Duration, sleep, timeout};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MyState {
    Start,
    Processing,
    End,
}

// Long-running node that simulates work
#[derive(Clone)]
struct LongProcessingNode {
    duration_secs: u64,
}

impl LongProcessingNode {
    fn new(duration_secs: u64) -> Self {
        Self { duration_secs }
    }
}

#[task::node(state = MyState)]
impl LongProcessingNode {
    type PrepResult = ();
    type ExecResult = String;

    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        println!("🔧 Starting long task ({}s)", self.duration_secs);
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        // Simulate long-running work with progress updates
        for i in 1..=self.duration_secs {
            sleep(Duration::from_secs(1)).await;
            println!("  📊 Long task progress: {}/{}s", i, self.duration_secs);
        }
        println!("✅ Long task completed");
        format!("Long task finished after {}s", self.duration_secs)
    }

    async fn post(&self, res: &Resources, result: Self::ExecResult) -> Result<MyState, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("long_task_result", result)?;
        Ok(MyState::Processing)
    }
}

// Processing node that handles the result
#[derive(Clone)]
struct ProcessingNode;

#[task::node(state = MyState)]
impl ProcessingNode {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let result: String = store
            .get("long_task_result")
            .unwrap_or_else(|_| "No result available".to_string());
        Ok(result)
    }

    async fn exec(&self, data: Self::PrepResult) -> Self::ExecResult {
        println!("⚙️  Processing result: {}", data);
        sleep(Duration::from_secs(1)).await;
        format!("Processed: {}", data)
    }

    async fn post(&self, res: &Resources, result: Self::ExecResult) -> Result<MyState, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("processed_result", result)?;
        println!("✅ Processing completed");
        Ok(MyState::End)
    }
}

// Quick node for comparison
#[derive(Clone)]
struct QuickNode;

#[task::node(state = MyState)]
impl QuickNode {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        println!("⚡ Quick task starting");
        Ok(())
    }

    async fn exec(&self, _data: Self::PrepResult) -> Self::ExecResult {
        println!("⚡ Quick task completed");
    }

    async fn post(
        &self,
        _res: &Resources,
        _result: Self::ExecResult,
    ) -> Result<MyState, CanoError> {
        Ok(MyState::End)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Starting Scheduler Graceful Shutdown Example");
    println!("================================================\n");

    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Create a long-running workflow
    let long_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(MyState::Start, LongProcessingNode::new(5))
        .register(MyState::Processing, ProcessingNode)
        .add_exit_state(MyState::End);

    // Create a quick workflow
    let quick_flow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(MyState::Start, QuickNode)
        .add_exit_state(MyState::End);

    // Add flows to scheduler
    println!("📅 Scheduling workflows:");
    println!("  • Long task: Every 10 seconds (takes 5s to complete + 1s processing)");
    println!("  • Quick task: Every 2 seconds (instant completion)\n");

    scheduler.every_seconds("long_task", long_flow, MyState::Start, 10)?;
    scheduler.every_seconds("quick_task", quick_flow, MyState::Start, 2)?;

    // Start the scheduler — consumes the builder, returns a clone-able handle.
    println!("✅ Scheduler started");
    println!("💡 Press Ctrl+C to trigger graceful shutdown\n");
    let running = scheduler.start().await?;

    // Clone the handle for the Ctrl+C signal handler.
    let signal_handle = running.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");

        println!("\n\n🛑 Received Ctrl+C - Initiating graceful shutdown...");
        println!("   Stopping scheduler and waiting for running workflows to complete...");

        match signal_handle.stop().await {
            Ok(()) => println!("✅ All workflows completed gracefully"),
            Err(e) => eprintln!("⚠️  Shutdown error: {}", e),
        }
    });

    // DEMO: Simulate user interrupt after 15 seconds.
    let pid = std::process::id();
    tokio::spawn(async move {
        println!("⏳ Demo will automatically trigger Ctrl+C in 15 seconds...");
        sleep(Duration::from_secs(15)).await;
        println!("\n⏰ Demo time limit reached - sending SIGINT...");

        #[cfg(unix)]
        std::process::Command::new("kill")
            .arg("-s")
            .arg("INT")
            .arg(pid.to_string())
            .output()
            .expect("Failed to send SIGINT");

        #[cfg(not(unix))]
        println!(
            "⚠️  Automatic SIGINT not supported on this platform, please press Ctrl+C manually"
        );
    });

    // Block on the scheduler until shutdown completes.
    running.wait().await?;

    println!("\n🎉 Shutdown complete!");
    Ok(())
}

// ===============================
// SHUTDOWN PATTERNS
// ===============================

/// Pattern 1: Graceful with custom timeout
#[allow(dead_code)]
async fn graceful_shutdown_with_timeout(
    scheduler: &RunningScheduler<MyState>,
    timeout_duration: Duration,
) -> CanoResult<()> {
    println!("🛑 Stopping scheduler...");
    scheduler.stop().await?;

    let result = timeout(timeout_duration, async {
        while scheduler.has_running_flows().await {
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    match result {
        Ok(()) => {
            println!("✅ Graceful shutdown completed");
            Ok(())
        }
        Err(_) => {
            println!("⚠️  Timeout reached during shutdown");
            Err(CanoError::Workflow("Shutdown timeout".to_string()))
        }
    }
}

/// Pattern 2: Monitor and report progress
#[allow(dead_code)]
async fn monitored_shutdown(scheduler: &RunningScheduler<MyState>) -> CanoResult<()> {
    println!("🛑 Initiating shutdown...");
    scheduler.stop().await?;

    if scheduler.has_running_flows().await {
        println!("📊 Waiting for running flows to complete");

        let mut elapsed = 0;
        while scheduler.has_running_flows().await && elapsed < 60 {
            sleep(Duration::from_secs(5)).await;
            elapsed += 5;

            let flows = scheduler.list().await;
            let running = flows
                .iter()
                .filter(|f| matches!(f.status, Status::Running))
                .count();
            println!(
                "  Still running: {} workflows ({}s elapsed)",
                running, elapsed
            );
        }

        if scheduler.has_running_flows().await {
            println!("⚠️  Some flows still running after 60s");
        } else {
            println!("✅ All flows completed");
        }
    } else {
        println!("✅ No running flows, stopped immediately");
    }

    Ok(())
}

/// Pattern 3: Progressive timeout with escalation
#[allow(dead_code)]
async fn progressive_shutdown(scheduler: &RunningScheduler<MyState>) -> CanoResult<()> {
    scheduler.stop().await?;

    // First attempt: short timeout (10 seconds)
    println!("⏱️  Attempting quick shutdown (10s)...");
    let quick_result = timeout(Duration::from_secs(10), async {
        while scheduler.has_running_flows().await {
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    if quick_result.is_ok() {
        println!("✅ Quick shutdown successful");
        return Ok(());
    }

    // Second attempt: longer timeout (30 seconds)
    println!("⏳ Extending timeout (30s more)...");
    let extended_result = timeout(Duration::from_secs(30), async {
        while scheduler.has_running_flows().await {
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    if extended_result.is_ok() {
        println!("✅ Extended shutdown successful");
        return Ok(());
    }

    // If we get here, workflows are still running after 40 seconds
    println!("🚨 Workflows still running after 40s - would force shutdown in production");
    Err(CanoError::Workflow("Shutdown timeout exceeded".to_string()))
}

/// Pattern 4: Immediate check and conditional wait
#[allow(dead_code)]
async fn conditional_shutdown(scheduler: &RunningScheduler<MyState>) -> CanoResult<()> {
    scheduler.stop().await?;

    if !scheduler.has_running_flows().await {
        println!("✅ No active workflows - immediate shutdown");
        return Ok(());
    }

    println!("⏳ Active workflows detected - waiting for completion");

    timeout(Duration::from_secs(30), async {
        while scheduler.has_running_flows().await {
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| CanoError::Workflow("Shutdown timeout".to_string()))?;

    println!("✅ All workflows completed");
    Ok(())
}
