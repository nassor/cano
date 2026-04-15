//! # Partial Results Workflow Example
//!
//! This example demonstrates how to handle partial results in a split/join workflow.
//! It shows how to:
//! - Execute multiple tasks in parallel with different failure rates and latencies
//! - Use the `PartialResults` join strategy to proceed as soon as a threshold is met
//! - Handle mixed success/failure results
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_partial_results
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use std::time::Duration;
use tokio::time::sleep;

// Define workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ApiState {
    Start,
    #[allow(dead_code)]
    FetchData,
    Process,
    Complete,
    Failed,
}

// A task that simulates an API call with variable latency and failure rate
#[derive(Clone)]
struct ApiTask {
    id: usize,
    latency_ms: u64,
    should_fail: bool,
}

impl ApiTask {
    fn new(id: usize, latency_ms: u64, should_fail: bool) -> Self {
        Self {
            id,
            latency_ms,
            should_fail,
        }
    }
}

#[async_trait]
impl Task<ApiState> for ApiTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ApiState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        println!(
            "Task {}: Starting (latency: {}ms)",
            self.id, self.latency_ms
        );

        // Simulate network latency
        sleep(Duration::from_millis(self.latency_ms)).await;

        if self.should_fail {
            println!("Task {}: Failed", self.id);
            Err(CanoError::task_execution(format!(
                "Task {} failed",
                self.id
            )))
        } else {
            println!("Task {}: Completed successfully", self.id);
            // Store result
            store.put(
                &format!("result_{}", self.id),
                format!("data_from_{}", self.id),
            )?;
            Ok(TaskResult::Single(ApiState::Process))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("--- Partial Results Workflow Example ---\n");

    // We want to fetch data from multiple sources redundantly.
    // We have 4 sources, but we only need 2 successful responses to proceed.
    // This is a common pattern to reduce tail latency.

    let tasks = vec![
        ApiTask::new(1, 100, false), // Fast success
        ApiTask::new(2, 500, false), // Slow success
        ApiTask::new(3, 150, true),  // Fast failure
        ApiTask::new(4, 800, false), // Very slow success (should be cancelled)
    ];

    // Configure the join strategy:
    // - PartialResults(2): Proceed as soon as 2 tasks complete successfully
    let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), ApiState::Complete);

    let store = MemoryStore::new();

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register_split(ApiState::Start, tasks, join_config)
        .add_exit_state(ApiState::Complete)
        .add_exit_state(ApiState::Failed);

    println!("Starting workflow...");
    let start = std::time::Instant::now();

    let result = workflow.orchestrate(ApiState::Start).await?;

    let duration = start.elapsed();
    println!(
        "\nWorkflow finished in {:?} with state: {:?}",
        duration, result
    );

    // Check individual results stored by successful tasks
    let result_1 = store.get::<String>("result_1").ok();
    let result_2 = store.get::<String>("result_2").ok();

    println!("\nResults:");
    println!("- Task 1: {:?}", result_1);
    println!("- Task 2: {:?}", result_2);

    // Verify results
    // We expect Task 1 (success) and Task 2 (success) to complete.
    // Task 3 (failure) will complete but not count towards the threshold.
    // Task 4 should be cancelled because we only asked for 2 successful completions.
    // Task 1 succeeds at 100ms.
    // Task 3 fails at 150ms.
    // Task 2 succeeds at 500ms.
    // So at 500ms we have 2 successes.
    // The workflow should proceed.

    let successes = [result_1, result_2].iter().filter(|r| r.is_some()).count();
    if successes >= 2 {
        println!("\nSUCCESS: Workflow behaved as expected (waited for successes).");
    } else {
        println!("\nWARNING: Unexpected results.");
    }

    Ok(())
}
