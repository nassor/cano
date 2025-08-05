#![cfg(feature = "tracing")]
//! # Tracing Demo Example
//!
//! This example demonstrates the tracing feature in Cano workflows and schedulers.
//! It shows how to:
//! 1. Enable tracing with spans for workflows
//! 2. Use custom tracing spans for better observability  
//! 3. Track workflow execution through the scheduler
//! 4. Monitor node execution phases (prep, exec, post)
//!
//! Run with:
//! ```bash
//! cargo run --example tracing_demo --features tracing
//! ```
//!
//! For better tracing output, you can also run with a tracing subscriber:
//! ```bash
//! RUST_LOG=info cargo run --example tracing_demo --features tracing
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use std::time::Duration;
use tracing::{Instrument, info, info_span, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(dead_code)] // Some variants used for demonstration purposes
enum WorkflowState {
    Start,
    DataValidation,
    Processing,
    PostProcessing,
    Complete,
    Error,
}

/// A node that demonstrates tracing in the prep, exec, and post phases
#[derive(Clone)]
struct TracedDataProcessor {
    processor_id: String,
    simulate_delay_ms: u64,
}

impl TracedDataProcessor {
    fn new(processor_id: &str, simulate_delay_ms: u64) -> Self {
        Self {
            processor_id: processor_id.to_string(),
            simulate_delay_ms,
        }
    }
}

#[async_trait]
impl Node<WorkflowState> for TracedDataProcessor {
    type PrepResult = Vec<String>;
    type ExecResult = Vec<String>;

    fn config(&self) -> TaskConfig {
        TaskConfig::default()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        info!(processor_id = %self.processor_id, "Starting data preparation");

        // Simulate some prep work
        tokio::time::sleep(Duration::from_millis(50)).await;

        let data = vec![
            "record_1".to_string(),
            "record_2".to_string(),
            "record_3".to_string(),
        ];

        store.put("prep_timestamp", chrono::Utc::now().to_rfc3339())?;

        info!(processor_id = %self.processor_id, record_count = data.len(), "Data preparation completed");
        Ok(data)
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        info!(processor_id = %self.processor_id, input_records = prep_result.len(), "Starting data processing");

        // Simulate processing work with custom span
        let processing_span = info_span!("data_processing", processor_id = %self.processor_id);
        let processed_data = async {
            tokio::time::sleep(Duration::from_millis(self.simulate_delay_ms)).await;

            prep_result
                .into_iter()
                .map(|item| format!("processed_{item}"))
                .collect::<Vec<_>>()
        }
        .instrument(processing_span)
        .await;

        info!(processor_id = %self.processor_id, output_records = processed_data.len(), "Data processing completed");
        processed_data
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<WorkflowState, CanoError> {
        info!(processor_id = %self.processor_id, "Starting post-processing");

        // Store results
        let result_summary = format!("Processed {} records", exec_result.len());
        store.put("processing_result", result_summary)?;
        store.put("completion_timestamp", chrono::Utc::now().to_rfc3339())?;

        // Simulate some post-processing delay
        tokio::time::sleep(Duration::from_millis(30)).await;

        info!(processor_id = %self.processor_id, processed_count = exec_result.len(), "Post-processing completed");
        Ok(WorkflowState::Complete)
    }
}

/// A simple validation node
#[derive(Clone)]
struct ValidationNode {
    should_pass: bool,
}

impl ValidationNode {
    fn new(should_pass: bool) -> Self {
        Self { should_pass }
    }
}

#[async_trait]
impl Node<WorkflowState> for ValidationNode {
    type PrepResult = String;
    type ExecResult = bool;

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        info!("Preparing validation");
        Ok("validation_data".to_string())
    }

    async fn exec(&self, _prep_result: Self::PrepResult) -> Self::ExecResult {
        info!(should_pass = self.should_pass, "Running validation");
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.should_pass
    }

    async fn post(
        &self,
        _store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<WorkflowState, CanoError> {
        if exec_result {
            info!("Validation passed");
            Ok(WorkflowState::Processing)
        } else {
            warn!("Validation failed");
            Ok(WorkflowState::Error)
        }
    }
}

/// A simple task that demonstrates Task-level tracing (simpler than Node)
#[derive(Clone)]
struct SimpleMathTask {
    task_id: String,
    operation: String,
}

impl SimpleMathTask {
    fn new(task_id: &str, operation: &str) -> Self {
        Self {
            task_id: task_id.to_string(),
            operation: operation.to_string(),
        }
    }
}

#[async_trait]
impl Task<WorkflowState> for SimpleMathTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::default().with_fixed_retry(2, Duration::from_millis(100))
    }

    async fn run(&self, store: &MemoryStore) -> Result<WorkflowState, CanoError> {
        info!(task_id = %self.task_id, operation = %self.operation, "Starting math task");

        // Simulate some computation work
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Get operands from store or use defaults
        let a: i32 = store.get("operand_a").unwrap_or(10);
        let b: i32 = store.get("operand_b").unwrap_or(5);

        let result = match self.operation.as_str() {
            "add" => a + b,
            "multiply" => a * b,
            "subtract" => a - b,
            _ => {
                warn!(operation = %self.operation, "Unknown operation, defaulting to addition");
                a + b
            }
        };

        store.put("math_result", result)?;
        store.put("task_completed_by", self.task_id.clone())?;

        info!(
            task_id = %self.task_id,
            operation = %self.operation,
            operand_a = a,
            operand_b = b,
            result,
            "Math task completed"
        );

        Ok(WorkflowState::Complete)
    }
}

async fn setup_tracing() {
    // Initialize tracing subscriber for better output
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    setup_tracing().await;

    info!("🔍 Starting Cano Tracing Demo");
    info!("=============================");

    // Example 1: Basic workflow with tracing spans
    {
        info!("📋 Example 1: Basic Workflow with Custom Tracing Span");

        let workflow_span = info_span!(
            "user_data_workflow",
            workflow_type = "basic",
            user_id = "12345"
        );

        let mut workflow = Workflow::new(WorkflowState::Start).with_tracing_span(workflow_span);

        workflow
            .register(WorkflowState::Start, ValidationNode::new(true))
            .register(
                WorkflowState::Processing,
                TracedDataProcessor::new("basic_processor", 200),
            )
            .add_exit_states(vec![WorkflowState::Complete, WorkflowState::Error]);

        let store = MemoryStore::new();

        info!("Starting workflow execution...");
        let result = workflow.orchestrate(&store).await?;
        info!(final_state = ?result, "Workflow completed");

        println!("✅ Basic workflow completed with state: {result:?}\n");
    }

    // Example 2: Concurrent workflow with tracing
    {
        info!("📋 Example 2: Concurrent Workflow with Tracing");

        let concurrent_span = info_span!("batch_processing_workflow", batch_size = 3);

        let mut concurrent_workflow =
            ConcurrentWorkflow::new(WorkflowState::Start).with_tracing_span(concurrent_span);

        concurrent_workflow
            .register(WorkflowState::Start, ValidationNode::new(true))
            .register(
                WorkflowState::Processing,
                TracedDataProcessor::new("concurrent_processor", 300),
            )
            .add_exit_state(WorkflowState::Complete);

        let stores = vec![MemoryStore::new(), MemoryStore::new(), MemoryStore::new()];

        info!("Starting concurrent workflow execution...");
        let (_results, status) = concurrent_workflow
            .execute_concurrent(stores, WaitStrategy::WaitForever)
            .await?;

        info!(
            completed = status.completed,
            failed = status.failed,
            duration_ms = status.duration.as_millis(),
            "Concurrent execution completed"
        );

        println!("✅ Concurrent workflow completed:");
        println!("   - Total workflows: {}", status.total_workflows);
        println!("   - Completed: {}", status.completed);
        println!("   - Failed: {}", status.failed);
        println!("   - Duration: {:?}\n", status.duration);
    }

    // Example 3: Simple Task with Tracing and Retry Logic
    {
        info!("📋 Example 3: Simple Task with Tracing and Retry");

        let task_span = info_span!("math_computation_workflow", workflow_type = "task_based");

        let mut task_workflow = Workflow::new(WorkflowState::Start).with_tracing_span(task_span);

        task_workflow
            .register(
                WorkflowState::Start,
                SimpleMathTask::new("math_task_1", "multiply"),
            )
            .add_exit_state(WorkflowState::Complete);

        let store = MemoryStore::new();

        // Set some operands for the math task
        store.put("operand_a", 7)?;
        store.put("operand_b", 6)?;

        info!("Starting task-based workflow execution...");
        let result = task_workflow.orchestrate(&store).await?;

        let math_result: i32 = store.get("math_result").unwrap_or(0);
        let completed_by: String = store.get("task_completed_by").unwrap_or_default();

        info!(
            final_state = ?result,
            math_result,
            completed_by = %completed_by,
            "Task-based workflow completed"
        );

        println!("✅ Task-based workflow completed:");
        println!("   - Final state: {result:?}");
        println!("   - Math result: {math_result}");
        println!("   - Completed by: {completed_by}\n");
    }

    // Example 4: Scheduler with tracing
    #[cfg(feature = "scheduler")]
    {
        info!("📋 Example 4: Scheduler with Workflow Tracing");

        let scheduler_span = info_span!("scheduled_workflows");
        let _scheduler_enter = scheduler_span.enter();

        let mut scheduler = Scheduler::new();

        // Create a workflow with a custom span for scheduled execution
        let scheduled_span = info_span!(
            "scheduled_data_processing",
            schedule_type = "every_2_seconds"
        );
        let mut scheduled_workflow =
            Workflow::new(WorkflowState::Start).with_tracing_span(scheduled_span);

        scheduled_workflow
            .register(WorkflowState::Start, ValidationNode::new(true))
            .register(
                WorkflowState::Processing,
                TracedDataProcessor::new("scheduled_processor", 150),
            )
            .add_exit_state(WorkflowState::Complete);

        scheduler.every_seconds("traced_workflow", scheduled_workflow, 2)?;

        info!("Starting scheduler...");
        scheduler.start().await?;

        // Let it run for a few iterations
        info!("Letting scheduler run for 6 seconds...");
        tokio::time::sleep(Duration::from_secs(6)).await;

        // Check status
        if let Some(flow_info) = scheduler.status("traced_workflow").await {
            info!(
                workflow_id = %flow_info.id,
                run_count = flow_info.run_count,
                status = ?flow_info.status,
                "Workflow status"
            );

            println!(
                "✅ Scheduled workflow executed {} times",
                flow_info.run_count
            );
        }

        info!("Stopping scheduler...");
        scheduler.stop().await?;
    }

    // Example 5: Error handling with tracing
    {
        info!("📋 Example 5: Error Handling with Tracing");

        let error_span = info_span!("error_handling_demo");

        let mut error_workflow = Workflow::new(WorkflowState::Start).with_tracing_span(error_span);

        error_workflow
            .register(WorkflowState::Start, ValidationNode::new(false)) // This will fail
            .register(
                WorkflowState::Processing,
                TracedDataProcessor::new("error_processor", 100),
            )
            .add_exit_states(vec![WorkflowState::Complete, WorkflowState::Error]);

        let store = MemoryStore::new();

        info!("Starting workflow that will encounter validation failure...");
        let result = error_workflow.orchestrate(&store).await?;

        println!("✅ Error workflow completed with state: {result:?}");

        match result {
            WorkflowState::Error => {
                println!(
                    "   As expected, the workflow ended in error state due to validation failure"
                );
            }
            _ => {
                println!("   Unexpected result");
            }
        }
    }

    info!("🎉 Tracing demo completed!");
    println!("\n🔍 Tracing Demo Summary:");
    println!("• Workflows can be instrumented with custom tracing spans");
    println!("• Node execution phases (prep, exec, post) are automatically traced");
    println!("• Concurrent workflows trace each instance separately");
    println!("• Schedulers trace workflow executions with context");
    println!("• Error paths are traced with appropriate log levels");
    println!("\nTo see more detailed tracing output, run with RUST_LOG=debug");

    Ok(())
}
