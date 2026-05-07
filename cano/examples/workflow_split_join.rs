//! # Workflow Split/Join Example
//!
//! This example demonstrates the new split/join functionality in Cano workflows.
//! It shows how to:
//! - Execute multiple tasks in parallel
//! - Use different join strategies (All, Any, Quorum, Percentage)
//! - Handle timeouts at both workflow and state levels
//! - Share data between parallel tasks
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_split_join
//! ```

use cano::prelude::*;
use std::time::Duration;

// Define workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataProcessingState {
    Start,
    #[allow(dead_code)]
    LoadData,
    ParallelProcessing,
    Aggregate,
    Complete,
}

// Task that loads initial data
#[derive(Clone)]
struct DataLoader;

#[task(state = DataProcessingState)]
impl DataLoader {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataProcessingState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Loading initial data...");

        // Simulate loading data
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        store.put("input_data", data)?;

        println!("Data loaded: 10 numbers");
        Ok(TaskResult::Single(DataProcessingState::ParallelProcessing))
    }
}

// Parallel processing task that processes a chunk of data
#[derive(Clone)]
struct ProcessorTask {
    task_id: usize,
}

impl ProcessorTask {
    fn new(task_id: usize) -> Self {
        Self { task_id }
    }
}

#[task(state = DataProcessingState)]
impl ProcessorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataProcessingState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Processor {} starting...", self.task_id);

        // Get input data
        let data: Vec<i32> = store.get("input_data")?;

        // Simulate processing time
        tokio::time::sleep(Duration::from_millis(100 * self.task_id as u64)).await;

        // Process our chunk (simple example: multiply by task_id)
        let result: i32 = data.iter().map(|&x| x * self.task_id as i32).sum();

        // Store individual result
        store.put(&format!("result_{}", self.task_id), result)?;

        println!(
            "Processor {} completed with result: {}",
            self.task_id, result
        );
        Ok(TaskResult::Single(DataProcessingState::Aggregate))
    }
}

// Aggregator task that combines results
#[derive(Clone)]
struct Aggregator;

#[task(state = DataProcessingState)]
impl Aggregator {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataProcessingState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Aggregating results...");

        // Collect all results
        let mut total = 0;
        let mut count = 0;

        for i in 1..=3 {
            if let Ok(result) = store.get::<i32>(&format!("result_{}", i)) {
                total += result;
                count += 1;
            }
        }

        store.put("final_result", total)?;
        store.put("processor_count", count as usize)?;

        println!(
            "Aggregation complete: {} processors completed, total: {}",
            count, total
        );
        Ok(TaskResult::Single(DataProcessingState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("=== Cano Workflow Split/Join Demo ===\n");

    // Example 1: All Strategy - Wait for all tasks
    println!("--- Example 1: All Strategy ---");
    {
        let store = MemoryStore::new();

        let processors = vec![
            ProcessorTask::new(1),
            ProcessorTask::new(2),
            ProcessorTask::new(3),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, DataProcessingState::Aggregate);

        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(DataProcessingState::Start, DataLoader)
            .register_split(
                DataProcessingState::ParallelProcessing,
                processors,
                join_config,
            )
            .register(DataProcessingState::Aggregate, Aggregator)
            .add_exit_state(DataProcessingState::Complete);

        let result = workflow.orchestrate(DataProcessingState::Start).await?;

        let final_result: i32 = store.get("final_result")?;
        println!("Final result: {}", final_result);
        println!("Workflow completed: {:?}\n", result);
    }

    // Example 2: Quorum Strategy - Wait for 2 out of 3 tasks
    println!("--- Example 2: Quorum Strategy (2/3) ---");
    {
        let store = MemoryStore::new();

        let processors = vec![
            ProcessorTask::new(1),
            ProcessorTask::new(2),
            ProcessorTask::new(3),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Quorum(2), DataProcessingState::Aggregate);

        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(DataProcessingState::Start, DataLoader)
            .register_split(
                DataProcessingState::ParallelProcessing,
                processors,
                join_config,
            )
            .register(DataProcessingState::Aggregate, Aggregator)
            .add_exit_state(DataProcessingState::Complete);

        let result = workflow.orchestrate(DataProcessingState::Start).await?;

        let processor_count: usize = store.get("processor_count")?;
        println!(
            "Processors completed: {} (quorum satisfied)",
            processor_count
        );
        println!("Workflow completed: {:?}\n", result);
    }

    // Example 3: Any Strategy - Continue after first completion
    println!("--- Example 3: Any Strategy ---");
    {
        let store = MemoryStore::new();

        let processors = vec![
            ProcessorTask::new(1),
            ProcessorTask::new(2),
            ProcessorTask::new(3),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Any, DataProcessingState::Aggregate);

        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(DataProcessingState::Start, DataLoader)
            .register_split(
                DataProcessingState::ParallelProcessing,
                processors,
                join_config,
            )
            .register(DataProcessingState::Aggregate, Aggregator)
            .add_exit_state(DataProcessingState::Complete);

        let result = workflow.orchestrate(DataProcessingState::Start).await?;

        println!("Workflow completed with Any strategy: {:?}\n", result);
    }

    // Example 4: Percentage Strategy - Wait for 66% of tasks
    println!("--- Example 4: Percentage Strategy (66%) ---");
    {
        let store = MemoryStore::new();

        let processors = vec![
            ProcessorTask::new(1),
            ProcessorTask::new(2),
            ProcessorTask::new(3),
        ];

        let join_config = JoinConfig::new(
            JoinStrategy::Percentage(0.66),
            DataProcessingState::Aggregate,
        );

        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(DataProcessingState::Start, DataLoader)
            .register_split(
                DataProcessingState::ParallelProcessing,
                processors,
                join_config,
            )
            .register(DataProcessingState::Aggregate, Aggregator)
            .add_exit_state(DataProcessingState::Complete);

        let result = workflow.orchestrate(DataProcessingState::Start).await?;

        let processor_count: usize = store.get("processor_count")?;
        println!("Processors completed: {} (66% threshold)", processor_count);
        println!("Workflow completed: {:?}\n", result);
    }

    // Example 5: With Timeout
    println!("--- Example 5: Split with Timeout ---");
    {
        let store = MemoryStore::new();

        let processors = vec![
            ProcessorTask::new(1),
            ProcessorTask::new(2),
            ProcessorTask::new(3),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, DataProcessingState::Aggregate)
            .with_timeout(Duration::from_millis(250)); // Will complete 2 out of 3 tasks

        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(DataProcessingState::Start, DataLoader)
            .register_split(
                DataProcessingState::ParallelProcessing,
                processors,
                join_config,
            )
            .register(DataProcessingState::Aggregate, Aggregator)
            .add_exit_state(DataProcessingState::Complete);

        match workflow.orchestrate(DataProcessingState::Start).await {
            Ok(result) => println!("Workflow completed: {:?}", result),
            Err(e) => println!("Workflow failed (expected timeout): {}", e),
        }
    }

    println!("\n=== Demo Complete ===");
    Ok(())
}
