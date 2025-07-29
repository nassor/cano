//! # Concurrent Workflow Example
//!
//! This example demonstrates the new simplified concurrent workflow API in Cano.
//! It shows how to execute multiple workflow instances in parallel with different
//! wait strategies for flexible execution control.
//!
//! ## New Simplified API
//!
//! The new API eliminates the need for `register_cloneable_node()` and makes it easier
//! to build concurrent workflows:
//!
//! ```rust,ignore
//! // Old API (deprecated)
//! concurrent_workflow.register_cloneable_node(state, node);
//!
//! // New API (recommended)
//! concurrent_workflow.register_node(state, node);
//! ```

use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProcessingState {
    Start,
    Complete,
}

#[derive(Clone)]
struct ProcessingNode {
    execution_count: Arc<AtomicU32>,
    node_id: String,
}

impl ProcessingNode {
    fn new(id: &str) -> Self {
        Self {
            execution_count: Arc::new(AtomicU32::new(0)),
            node_id: id.to_string(),
        }
    }

    fn get_execution_count(&self) -> u32 {
        self.execution_count.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Node<ProcessingState> for ProcessingNode {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        // Load some data from store or create default
        let input_data = store
            .get::<String>("input_data")
            .unwrap_or_else(|_| "default_input".to_string());

        Ok(format!(
            "Node {} prepared with: {}",
            self.node_id, input_data
        ))
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        // Increment execution counter
        let count = self.execution_count.fetch_add(1, Ordering::SeqCst) + 1;

        // Simulate some processing time
        tokio::time::sleep(Duration::from_millis(50)).await;

        format!("{} - Execution #{}", prep_result, count)
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<ProcessingState, CanoError> {
        // Store the result
        store.put("processed_result", exec_result)?;

        // Move to next state
        Ok(ProcessingState::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Concurrent Workflow Example");
    println!("================================\n");

    // Create a template workflow
    let mut template_workflow: Workflow<ProcessingState> = Workflow::new(ProcessingState::Start);
    template_workflow.add_exit_state(ProcessingState::Complete);

    // Create a concurrent workflow
    let mut concurrent_workflow = ConcurrentWorkflow::new(template_workflow);

    // Register a processing node directly - much simpler!
    let processing_node = ProcessingNode::new("DataProcessor");
    concurrent_workflow.register_node(ProcessingState::Start, processing_node.clone());

    println!("📊 Example 1: WaitForever Strategy");
    println!("----------------------------------");

    // Create stores for concurrent execution (each workflow gets its own store)
    let stores = vec![
        {
            let store = MemoryStore::new();
            store.put("input_data", "Dataset_A".to_string()).unwrap();
            store
        },
        {
            let store = MemoryStore::new();
            store.put("input_data", "Dataset_B".to_string()).unwrap();
            store
        },
        {
            let store = MemoryStore::new();
            store.put("input_data", "Dataset_C".to_string()).unwrap();
            store
        },
    ];

    // Execute with WaitForever strategy (wait for all to complete)
    let (results, status) = concurrent_workflow
        .execute_concurrent(stores, WaitStrategy::WaitForever)
        .await?;

    println!("Results: {} workflows executed", results.len());
    println!(
        "Status: completed={}, failed={}, cancelled={}",
        status.completed, status.failed, status.cancelled
    );
    println!("Duration: {:?}\n", status.duration);

    // Check individual results
    for (i, result) in results.iter().enumerate() {
        match result {
            WorkflowResult::Success(final_state) => {
                println!("  Workflow {}: Success -> {:?}", i + 1, final_state);
            }
            WorkflowResult::Failed(error) => {
                println!("  Workflow {}: Failed -> {}", i + 1, error);
            }
            WorkflowResult::Cancelled => {
                println!("  Workflow {}: Cancelled", i + 1);
            }
        }
    }

    println!("\n📊 Example 2: WaitForQuota Strategy");
    println!("-----------------------------------");

    // Create more stores for quota example
    let stores = vec![
        MemoryStore::new(),
        MemoryStore::new(),
        MemoryStore::new(),
        MemoryStore::new(),
        MemoryStore::new(),
    ];

    // Execute with quota strategy (complete 3 out of 5, cancel the rest)
    let (results, status) = concurrent_workflow
        .execute_concurrent(stores, WaitStrategy::WaitForQuota(3))
        .await?;

    println!("Results: {} workflows executed", results.len());
    println!(
        "Status: completed={}, failed={}, cancelled={}",
        status.completed, status.failed, status.cancelled
    );
    println!("Duration: {:?}\n", status.duration);

    println!("📊 Example 3: WaitDuration Strategy");
    println!("-----------------------------------");

    // Create stores for duration example
    let stores = vec![MemoryStore::new(), MemoryStore::new(), MemoryStore::new()];

    // Execute with duration strategy (100ms timeout)
    let (results, status) = concurrent_workflow
        .execute_concurrent(
            stores,
            WaitStrategy::WaitDuration(Duration::from_millis(100)),
        )
        .await?;

    println!("Results: {} workflows executed", results.len());
    println!(
        "Status: completed={}, failed={}, cancelled={}",
        status.completed, status.failed, status.cancelled
    );
    println!("Duration: {:?}\n", status.duration);

    println!("📊 Example 4: WaitQuotaOrDuration Strategy");
    println!("------------------------------------------");

    // Create stores for combined strategy example
    let stores = vec![
        MemoryStore::new(),
        MemoryStore::new(),
        MemoryStore::new(),
        MemoryStore::new(),
    ];

    // Execute with combined strategy (2 completions OR 200ms, whichever comes first)
    let (results, status) = concurrent_workflow
        .execute_concurrent(
            stores,
            WaitStrategy::WaitQuotaOrDuration {
                quota: 2,
                duration: Duration::from_millis(200),
            },
        )
        .await?;

    println!("Results: {} workflows executed", results.len());
    println!(
        "Status: completed={}, failed={}, cancelled={}",
        status.completed, status.failed, status.cancelled
    );
    println!("Duration: {:?}\n", status.duration);

    println!("📊 Example 5: Concurrent Workflow Builder Pattern");
    println!("-------------------------------------------------");

    // Demonstrate the builder pattern
    let _built_concurrent_workflow: ConcurrentWorkflow<ProcessingState> =
        ConcurrentWorkflowBuilder::new(Workflow::new(ProcessingState::Start)).build();

    let stores = vec![MemoryStore::new(), MemoryStore::new()];
    // Note: This example needs to be updated to register nodes properly
    // For now, let's reuse the existing concurrent_workflow
    let (results, status) = concurrent_workflow
        .execute_concurrent(stores, WaitStrategy::WaitForever)
        .await?;

    println!("Builder pattern results: {} workflows", results.len());
    println!(
        "Status: completed={}, failed={}, cancelled={}",
        status.completed, status.failed, status.cancelled
    );

    println!("\n✅ All concurrent workflow examples completed successfully!");
    println!(
        "Total node executions: {}",
        processing_node.get_execution_count()
    );

    Ok(())
}
