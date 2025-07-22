//! # Simple Two-Node Workflow Example
//!
//! This example demonstrates a basic workflow with two nodes using the Flow structure:
//! 1. **GeneratorNode**: Creates a random vector of u32 numbers, filters out odd numbers
//!
//! The workflow showcases the three-phase lifecycle (prep, exec, post) and
//! inter-node communication through shared storage using the Flow state machine.
//!
//! Run with:
//! ```bash
//! cargo run --example simple
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use rand::Rng;
use std::collections::HashMap;
use tokio;

/// Action enum for controlling workflow flow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowAction {
    Generate,
    Count,
    Complete,
    Error,
}

/// First node: Generates random numbers and filters out odd numbers
#[derive(Clone)]
struct GeneratorNode {
    params: HashMap<String, String>,
}

impl GeneratorNode {
    fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
    }
}

#[async_trait]
impl Node<WorkflowAction> for GeneratorNode {
    type Params = HashMap<String, String>;
    type Storage = MemoryStore;
    type PrepResult = Vec<u32>;
    type ExecResult = Vec<u32>;

    fn set_params(&mut self, params: Self::Params) {
        self.params = params;
    }

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal() // Fast execution with minimal retries
    }

    /// Preparation phase: Generate a random vector of u32 numbers (25 to 150 elements)
    async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
        let mut rng = rand::rng();

        // Generate random size between 25 and 150
        let size = rng.random_range(25..=150);

        // Generate random u32 numbers
        let numbers: Vec<u32> = (0..size).map(|_| rng.random_range(1..=1000)).collect();

        println!("Generated {} random numbers", numbers.len());
        println!("Sample: {:?}", &numbers[..std::cmp::min(10, numbers.len())]);

        Ok(numbers)
    }

    /// Execution phase: Filter out odd numbers (keep only even numbers)
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        let even_numbers: Vec<u32> = prep_res.into_iter().filter(|&n| n % 2 == 0).collect();

        println!("Filtered to {} even numbers", even_numbers.len());
        println!(
            "Sample even numbers: {:?}",
            &even_numbers[..std::cmp::min(10, even_numbers.len())]
        );

        even_numbers
    }

    /// Post-processing phase: Store the filtered vector in memory
    async fn post(
        &self,
        store: &Self::Storage,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        // Store the filtered vector in memory
        store.put("filtered_numbers", exec_res)?;

        println!("✓ Generator node completed - filtered numbers stored in memory");

        Ok(WorkflowAction::Count)
    }
}

/// Second node: Loads data, counts numbers, and cleans up
#[derive(Clone)]
struct CounterNode {
    params: HashMap<String, String>,
}

impl CounterNode {
    fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
    }
}

#[async_trait]
impl Node<WorkflowAction> for CounterNode {
    type Params = HashMap<String, String>;
    type Storage = MemoryStore;
    type PrepResult = Vec<u32>;
    type ExecResult = usize;

    fn set_params(&mut self, params: Self::Params) {
        self.params = params;
    }

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    /// Preparation phase: Load the filtered numbers from memory
    async fn prep(&self, store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
        let numbers: Vec<u32> = store.get("filtered_numbers").map_err(|e| {
            CanoError::preparation(&format!("Failed to load filtered numbers: {}", e))
        })?;

        println!("Loaded {} numbers from memory", numbers.len());

        Ok(numbers)
    }

    /// Execution phase: Count the numbers
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        let count = prep_res.len();

        println!("Counted {} even numbers", count);

        count
    }

    /// Post-processing phase: Store count and clean up the original vector
    async fn post(
        &self,
        store: &Self::Storage,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        // Store the count
        store.put("number_count", exec_res)?;

        // Delete the original vector from memory (cleanup)
        store.delete("filtered_numbers")?;

        println!(
            "✓ Counter node completed - count stored ({}) and original data cleaned up",
            exec_res
        );

        Ok(WorkflowAction::Complete)
    }
}

/// Workflow orchestrator using Flow with different node types
async fn run_simple_workflow_with_flow() -> Result<(), CanoError> {
    println!("🚀 Starting Simple Two-Node Workflow with Flow");
    println!("===============================================");

    let store = MemoryStore::new();

    // Create a Flow that can handle different node types
    let mut flow = Flow::new(WorkflowAction::Generate);

    // Register different node types - this now works!
    flow.register_node(WorkflowAction::Generate, GeneratorNode::new())
        .register_node(WorkflowAction::Count, CounterNode::new())
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    // Execute the workflow using the Flow orchestrator
    match flow.orchestrate(&store).await {
        Ok(final_state) => {
            match final_state {
                WorkflowAction::Complete => {
                    println!("✅ Workflow completed successfully!");

                    // Display final results
                    match store.get::<usize>("number_count") {
                        Ok(final_count) => {
                            println!("\n📈 FINAL RESULTS");
                            println!("================");
                            println!("Total even numbers found: {}", final_count);

                            // Verify cleanup
                            match store.get::<Vec<u32>>("filtered_numbers") {
                                Ok(_) => {
                                    println!("⚠️  Warning: Original data still exists in memory")
                                }
                                Err(_) => println!("✓ Original data successfully cleaned up"),
                            }
                        }
                        Err(e) => {
                            return Err(CanoError::node_execution(format!(
                                "Failed to get final count: {}",
                                e
                            )));
                        }
                    }
                }
                WorkflowAction::Error => {
                    eprintln!("❌ Workflow terminated with error state");
                    return Err(CanoError::flow("Workflow terminated with error state"));
                }
                other => {
                    eprintln!("⚠️  Workflow ended in unexpected state: {:?}", other);
                    return Err(CanoError::flow(format!(
                        "Workflow ended in unexpected state: {:?}",
                        other
                    )));
                }
            }
        }
        Err(e) => {
            eprintln!("❌ Workflow failed: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

/// Demonstrate the workflow with Flow accepting different node types
#[tokio::main]
async fn main() {
    println!("🚀 Simple Two-Node Workflow Example");
    println!("===================================");

    // Run the Flow-based workflow with different node types
    println!("\n� Running Flow-Based Workflow (Different Node Types):");
    match run_simple_workflow_with_flow().await {
        Ok(()) => {
            println!("✅ Manual workflow completed successfully!");
        }
        Err(e) => {
            eprintln!("❌ Manual workflow failed: {e}");
            std::process::exit(1);
        }
    }

    // Then, run the Flow-based workflow with different node types
    println!("\n� Running Flow-Based Workflow (Different Node Types):");
    match run_simple_workflow_with_flow().await {
        Ok(()) => {
            println!("✅ Flow-based workflow completed successfully!");
        }
        Err(e) => {
            eprintln!("❌ Flow-based workflow failed: {e}");
            std::process::exit(1);
        }
    }

    println!("\n🎉 Workflow executed successfully!");
    println!("✨ The Flow supports different node types in the same workflow!");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generator_node() {
        let generator = GeneratorNode::new();
        let store = MemoryStore::new();

        let result = generator.run(&store).await.unwrap();
        assert_eq!(result, WorkflowAction::Count);

        // Verify data was stored
        let stored_numbers: Vec<u32> = store.get("filtered_numbers").unwrap();

        // All numbers should be even
        for &num in &stored_numbers {
            assert_eq!(num % 2, 0, "Number {} should be even", num);
        }

        println!(
            "Generator test passed - {} even numbers stored",
            stored_numbers.len()
        );
    }

    #[tokio::test]
    async fn test_counter_node() {
        let store = MemoryStore::new();

        // Setup: Put some test data using the same type as the generator node
        let test_numbers: Vec<u32> = vec![2, 4, 6, 8, 10];
        store.put("filtered_numbers", test_numbers.clone()).unwrap();

        let counter = CounterNode::new();
        let result = counter.run(&store).await.unwrap();

        assert_eq!(result, WorkflowAction::Complete);

        // Verify count was stored
        let count: usize = store.get("number_count").unwrap();
        assert_eq!(count, test_numbers.len());

        // Verify original data was cleaned up
        assert!(store.get::<Vec<u32>>("filtered_numbers").is_err());

        println!("Counter test passed - count: {}", count);
    }

    #[tokio::test]
    async fn test_full_workflow_with_flow_different_node_types() {
        let result = run_simple_workflow_with_flow().await;
        assert!(result.is_ok());

        println!("Full Flow workflow test with different node types passed");
    }

    #[tokio::test]
    async fn test_generator_number_range() {
        let generator = GeneratorNode::new();
        let store = MemoryStore::new();

        // Run multiple times to test size variance
        for _ in 0..5 {
            let prep_result = generator.prep(&store).await.unwrap();

            // Check that generated vector is within expected range
            assert!(
                prep_result.len() >= 25,
                "Generated vector too small: {}",
                prep_result.len()
            );
            assert!(
                prep_result.len() <= 150,
                "Generated vector too large: {}",
                prep_result.len()
            );

            // Check that all numbers are in reasonable range
            for &num in &prep_result {
                assert!(
                    num >= 1 && num <= 1000,
                    "Number {} out of expected range",
                    num
                );
            }
        }
    }

    #[tokio::test]
    async fn test_odd_number_filtering() {
        let generator = GeneratorNode::new();

        // Test with known input
        let test_input = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let result = generator.exec(test_input).await;

        let expected = vec![2, 4, 6, 8, 10];
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn test_counter_node_error_handling() {
        let counter = CounterNode::new();
        let store = MemoryStore::new();

        // Try to run counter without data in store
        let result = counter.run(&store).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to load filtered numbers")
        );
    }

    #[tokio::test]
    async fn test_workflow_error_state() {
        let store = MemoryStore::new();

        // Create counter node without any data in store (should fail)
        let counter = CounterNode::new();
        let result = counter.run(&store).await;
        assert!(result.is_err());

        println!("Error handling test passed");
    }
}
