//! # Concurrent Workflow Example
//!
//! This example demonstrates how to use the concurrent workflow feature to execute
//! multiple workflow instances in parallel with different timeout strategies.

use async_trait::async_trait;
use cano::{
    CanoError, Node,
    store::{KeyValueStore, MemoryStore},
    workflow::{
        ConcurrentStrategy, ConcurrentWorkflowBuilder, ConcurrentWorkflowInstance, Workflow,
        WorkflowBuilder,
    },
};
use std::time::Duration;

/// Simple states for our example workflows
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Start,
    Processing,
    Complete,
    Error,
}

/// A simple processing node that simulates some work
#[derive(Clone)]
struct ProcessingNode {
    next_state: WorkflowState,
    work_duration: Duration,
    node_id: String,
}

impl ProcessingNode {
    fn new(node_id: String, work_duration: Duration, next_state: WorkflowState) -> Self {
        Self {
            next_state,
            work_duration,
            node_id,
        }
    }
}

#[async_trait]
impl Node<WorkflowState> for ProcessingNode {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        // Try to get previous data or create default
        let data = store
            .get::<String>("workflow_data")
            .unwrap_or_else(|_| "initial_data".to_string());

        println!("Node {} preparing with data: {}", self.node_id, data);
        Ok(format!("prepared_by_{}", self.node_id))
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!(
            "Node {} executing for {:?}...",
            self.node_id, self.work_duration
        );

        // Simulate some work
        tokio::time::sleep(self.work_duration).await;

        format!("processed_{}_from_{}", self.node_id, prep_res)
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowState, CanoError> {
        // Store the result for potential next nodes
        store.put(&format!("result_{}", self.node_id), exec_res.clone())?;

        println!("Node {} completed with result: {}", self.node_id, exec_res);
        Ok(self.next_state.clone())
    }
}

/// Creates a simple workflow with processing steps
fn create_workflow(workflow_id: &str) -> Workflow<WorkflowState> {
    let start_node = ProcessingNode::new(
        format!("{workflow_id}_start"),
        Duration::from_millis(50),
        WorkflowState::Processing,
    );

    let process_node = ProcessingNode::new(
        format!("{workflow_id}_process"),
        Duration::from_millis(100),
        WorkflowState::Complete,
    );

    WorkflowBuilder::new(WorkflowState::Start)
        .register_node(WorkflowState::Start, start_node)
        .register_node(WorkflowState::Processing, process_node)
        .add_exit_state(WorkflowState::Complete)
        .add_exit_state(WorkflowState::Error)
        .build()
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🚀 Concurrent Workflow Example");
    println!("===============================\n");

    // Example 1: Wait for all workflows to complete
    println!("📝 Example 1: Wait Forever Strategy");
    {
        let store = MemoryStore::new();

        // Create multiple workflow instances
        let instances = vec![
            ConcurrentWorkflowInstance::new("workflow_1".to_string(), create_workflow("w1")),
            ConcurrentWorkflowInstance::new("workflow_2".to_string(), create_workflow("w2")),
            ConcurrentWorkflowInstance::new("workflow_3".to_string(), create_workflow("w3")),
        ];

        let concurrent_workflow = ConcurrentWorkflowBuilder::new(ConcurrentStrategy::WaitForever)
            .add_instances(instances)
            .build();

        let start_time = std::time::Instant::now();
        let results = concurrent_workflow.orchestrate(store).await?;
        let elapsed = start_time.elapsed();

        println!("Results:");
        println!("  ✅ Completed: {}", results.completed);
        println!("  ❌ Failed: {}", results.failed);
        println!("  ⏰ Cancelled: {}", results.cancelled);
        println!("  🕒 Total Duration: {elapsed:?}");
        println!();
    }

    // Example 2: Wait for quota (first 2 to complete)
    println!("📝 Example 2: Wait for Quota Strategy (2 out of 4)");
    {
        let store = MemoryStore::new();

        let instances = vec![
            ConcurrentWorkflowInstance::new("fast_1".to_string(), create_workflow("fast1")),
            ConcurrentWorkflowInstance::new("fast_2".to_string(), create_workflow("fast2")),
            ConcurrentWorkflowInstance::new("slow_1".to_string(), {
                // Create a slower workflow
                let slow_node = ProcessingNode::new(
                    "slow_start".to_string(),
                    Duration::from_millis(500), // Much longer
                    WorkflowState::Complete,
                );

                WorkflowBuilder::new(WorkflowState::Start)
                    .register_node(WorkflowState::Start, slow_node)
                    .add_exit_state(WorkflowState::Complete)
                    .build()
            }),
            ConcurrentWorkflowInstance::new("slow_2".to_string(), {
                let slow_node = ProcessingNode::new(
                    "slow_start_2".to_string(),
                    Duration::from_millis(600),
                    WorkflowState::Complete,
                );

                WorkflowBuilder::new(WorkflowState::Start)
                    .register_node(WorkflowState::Start, slow_node)
                    .add_exit_state(WorkflowState::Complete)
                    .build()
            }),
        ];

        let concurrent_workflow =
            ConcurrentWorkflowBuilder::new(ConcurrentStrategy::WaitForQuota(2))
                .add_instances(instances)
                .build();

        let start_time = std::time::Instant::now();
        let results = concurrent_workflow.orchestrate(store).await?;
        let elapsed = start_time.elapsed();

        println!("Results:");
        println!("  ✅ Completed: {}", results.completed);
        println!("  ❌ Failed: {}", results.failed);
        println!("  ⏰ Cancelled: {}", results.cancelled);
        println!("  🕒 Total Duration: {elapsed:?}");
        println!("  📊 Should have cancelled 2 slow workflows");
        println!();
    }

    // Example 3: Wait for duration timeout
    println!("📝 Example 3: Duration Timeout Strategy (200ms limit)");
    {
        let store = MemoryStore::new();

        let instances = vec![
            ConcurrentWorkflowInstance::new("quick".to_string(), create_workflow("quick")),
            ConcurrentWorkflowInstance::new("slow".to_string(), {
                // This workflow will likely timeout
                let very_slow_node = ProcessingNode::new(
                    "very_slow".to_string(),
                    Duration::from_millis(1000), // 1 second - longer than timeout
                    WorkflowState::Complete,
                );

                WorkflowBuilder::new(WorkflowState::Start)
                    .register_node(WorkflowState::Start, very_slow_node)
                    .add_exit_state(WorkflowState::Complete)
                    .build()
            }),
        ];

        let timeout = Duration::from_millis(200);
        let concurrent_workflow =
            ConcurrentWorkflowBuilder::new(ConcurrentStrategy::WaitDuration(timeout))
                .add_instances(instances)
                .build();

        let start_time = std::time::Instant::now();
        let results = concurrent_workflow.orchestrate(store).await?;
        let elapsed = start_time.elapsed();

        println!("Results:");
        println!("  ✅ Completed: {}", results.completed);
        println!("  ❌ Failed: {}", results.failed);
        println!("  ⏰ Cancelled: {}", results.cancelled);
        println!("  🕒 Total Duration: {elapsed:?}");
        println!("  📊 Should have timed out around {timeout:?}");
        println!();
    }

    // Example 4: Quota OR Duration - whichever comes first
    println!("📝 Example 4: Quota OR Duration Strategy (3 workflows OR 300ms)");
    {
        let store = MemoryStore::new();

        let instances = vec![
            ConcurrentWorkflowInstance::new("w1".to_string(), create_workflow("w1")),
            ConcurrentWorkflowInstance::new("w2".to_string(), create_workflow("w2")),
            ConcurrentWorkflowInstance::new("w3".to_string(), create_workflow("w3")),
            ConcurrentWorkflowInstance::new("w4".to_string(), create_workflow("w4")),
            ConcurrentWorkflowInstance::new("w5".to_string(), create_workflow("w5")),
        ];

        let concurrent_workflow =
            ConcurrentWorkflowBuilder::new(ConcurrentStrategy::WaitQuotaOrDuration {
                quota: 3,
                duration: Duration::from_millis(300),
            })
            .add_instances(instances)
            .build();

        let start_time = std::time::Instant::now();
        let results = concurrent_workflow.orchestrate(store).await?;
        let elapsed = start_time.elapsed();

        println!("Results:");
        println!("  ✅ Completed: {}", results.completed);
        println!("  ❌ Failed: {}", results.failed);
        println!("  ⏰ Cancelled: {}", results.cancelled);
        println!("  🕒 Total Duration: {elapsed:?}");
        println!("  📊 Should complete 3 workflows or timeout at 300ms");
        println!();
    }

    println!("🎉 All concurrent workflow examples completed!");
    Ok(())
}
