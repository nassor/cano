//! # Workflow Partial Results Example
//!
//! This example demonstrates the new PartialResults join strategy in Cano workflows.
//! It shows how to:
//! - Accept partial results from split tasks
//! - Cancel remaining tasks once minimum threshold is met
//! - Track both successful and failed tasks
//! - Access result summaries from the store
//!
//! ## Use Cases
//!
//! Partial results are useful when:
//! - You want to proceed as soon as enough tasks complete
//! - You need to handle both successes and failures gracefully
//! - You want to optimize for speed over completeness
//! - You're implementing fault-tolerant systems with redundancy

use cano::prelude::*;
use std::time::Duration;

// Define workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ApiAggregationState {
    Start,
    #[allow(dead_code)]
    FetchFromAPIs,
    ProcessResults,
    Complete,
}

// Simulates fetching data from an external API
#[derive(Clone)]
struct ApiTask {
    api_name: String,
    delay_ms: u64,
    should_fail: bool,
}

impl ApiTask {
    fn new(api_name: &str, delay_ms: u64, should_fail: bool) -> Self {
        Self {
            api_name: api_name.to_string(),
            delay_ms,
            should_fail,
        }
    }
}

#[async_trait]
impl Task<ApiAggregationState> for ApiTask {
    async fn run(
        &self,
        store: &MemoryStore,
    ) -> Result<TaskResult<ApiAggregationState>, CanoError> {
        println!(
            "🌐 Fetching from {} (delay: {}ms)...",
            self.api_name, self.delay_ms
        );

        // Simulate API call delay
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;

        if self.should_fail {
            println!("❌ {} - Request failed", self.api_name);
            return Err(CanoError::task_execution(format!(
                "{} API request failed",
                self.api_name
            )));
        }

        // Simulate successful response
        let data = format!("Data from {}", self.api_name);
        store.put(&format!("api_result_{}", self.api_name), data.clone())?;

        println!("✅ {} - Success: {}", self.api_name, data);
        Ok(TaskResult::Single(ApiAggregationState::ProcessResults))
    }
}

// Processes the partial results
#[derive(Clone)]
struct ResultProcessor;

#[async_trait]
impl Task<ApiAggregationState> for ResultProcessor {
    async fn run(
        &self,
        store: &MemoryStore,
    ) -> Result<TaskResult<ApiAggregationState>, CanoError> {
        println!("\n📊 Processing results...");

        // Get result summary
        let summary: String = store
            .get("split_results_summary")
            .unwrap_or_else(|_| "No summary available".to_string());
        println!("   {}", summary);

        // Get counts
        let successes: usize = store.get("split_successes_count").unwrap_or(0);
        let errors: usize = store.get("split_errors_count").unwrap_or(0);
        let cancelled: usize = store.get("split_cancelled_count").unwrap_or(0);

        println!("   ✅ Successful: {}", successes);
        println!("   ❌ Failed: {}", errors);
        println!("   ⏸️  Cancelled: {}", cancelled);

        println!("\n✨ Processing complete with partial results!");
        Ok(TaskResult::Single(ApiAggregationState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("=== Cano Workflow Partial Results Demo ===\n");

    // Example 1: Fast response wins
    println!("--- Example 1: Get 2 fastest responses, cancel the rest ---");
    {
        let store = MemoryStore::new();

        // Create API tasks with different delays
        // We only need 2 responses, so the slower ones will be cancelled
        let api_tasks = vec![
            ApiTask::new("FastAPI", 100, false),
            ApiTask::new("MediumAPI", 300, false),
            ApiTask::new("SlowAPI", 1000, false),    // Will be cancelled
            ApiTask::new("VerySlowAPI", 2000, false), // Will be cancelled
        ];

        // Wait for 2 tasks to complete, then cancel the rest
        let join_config =
            JoinConfig::new(JoinStrategy::PartialResults(2), ApiAggregationState::ProcessResults)
                .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(ApiAggregationState::Start, api_tasks, join_config)
            .register(ApiAggregationState::ProcessResults, ResultProcessor)
            .add_exit_state(ApiAggregationState::Complete);

        let start = std::time::Instant::now();
        let result = workflow.orchestrate(ApiAggregationState::Start).await?;
        let elapsed = start.elapsed();

        println!("Final state: {:?}", result);
        println!("Completed in: {:?}\n", elapsed);
    }

    // Example 2: Fault tolerance - accept partial results even with failures
    println!("--- Example 2: Fault tolerance with partial results ---");
    {
        let store = MemoryStore::new();

        // Create API tasks where some fail
        let api_tasks = vec![
            ApiTask::new("ReliableAPI", 100, false),
            ApiTask::new("UnstableAPI", 150, true), // This will fail
            ApiTask::new("BackupAPI", 200, false),
            ApiTask::new("FallbackAPI", 250, false),
            ApiTask::new("SlowBackupAPI", 1000, false), // Will be cancelled
        ];

        // Wait for 3 tasks to complete (success or failure)
        // This ensures we can handle failures gracefully
        let join_config =
            JoinConfig::new(JoinStrategy::PartialResults(3), ApiAggregationState::ProcessResults)
                .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(ApiAggregationState::Start, api_tasks, join_config)
            .register(ApiAggregationState::ProcessResults, ResultProcessor)
            .add_exit_state(ApiAggregationState::Complete);

        let start = std::time::Instant::now();
        let result = workflow.orchestrate(ApiAggregationState::Start).await?;
        let elapsed = start.elapsed();

        println!("Final state: {:?}", result);
        println!("Completed in: {:?}\n", elapsed);
    }

    // Example 3: Timeout with partial results
    println!("--- Example 3: Partial results with timeout ---");
    {
        let store = MemoryStore::new();

        let api_tasks = vec![
            ApiTask::new("QuickAPI", 100, false),
            ApiTask::new("MediumAPI", 300, false),
            ApiTask::new("SlowAPI", 800, false),
        ];

        // Wait for 2 tasks, but timeout if it takes too long
        let join_config =
            JoinConfig::new(JoinStrategy::PartialResults(2), ApiAggregationState::ProcessResults)
                .with_timeout(Duration::from_millis(500))
                .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(ApiAggregationState::Start, api_tasks, join_config)
            .register(ApiAggregationState::ProcessResults, ResultProcessor)
            .add_exit_state(ApiAggregationState::Complete);

        let start = std::time::Instant::now();
        let result = workflow.orchestrate(ApiAggregationState::Start).await?;
        let elapsed = start.elapsed();

        println!("Final state: {:?}", result);
        println!("Completed in: {:?}\n", elapsed);
    }

    // Example 4: All tasks complete before minimum
    println!("--- Example 4: All tasks complete quickly ---");
    {
        let store = MemoryStore::new();

        // All APIs are fast, so they'll all complete before cancellation
        let api_tasks = vec![
            ApiTask::new("FastAPI1", 50, false),
            ApiTask::new("FastAPI2", 60, false),
            ApiTask::new("FastAPI3", 70, false),
        ];

        // Wait for 2 tasks minimum, but all will complete
        let join_config =
            JoinConfig::new(JoinStrategy::PartialResults(2), ApiAggregationState::ProcessResults)
                .with_store_partial_results(true);

        let workflow = Workflow::new(store.clone())
            .register_split(ApiAggregationState::Start, api_tasks, join_config)
            .register(ApiAggregationState::ProcessResults, ResultProcessor)
            .add_exit_state(ApiAggregationState::Complete);

        let start = std::time::Instant::now();
        let result = workflow.orchestrate(ApiAggregationState::Start).await?;
        let elapsed = start.elapsed();

        println!("Final state: {:?}", result);
        println!("Completed in: {:?}\n", elapsed);
    }

    println!("\n=== Demo Complete ===");
    println!("\n📚 Key Takeaways:");
    println!("   • PartialResults strategy cancels remaining tasks after minimum completes");
    println!("   • Both successes and failures count toward the minimum");
    println!("   • Cancelled tasks are tracked separately");
    println!("   • Results can be stored in the workflow store for later access");
    println!("   • Great for optimizing latency in redundant systems");

    Ok(())
}
