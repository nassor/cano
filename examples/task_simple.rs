//! # Simple Task-Based Workflow Example
//!
//! This example demonstrates the same functionality as workflow_simple.rs but using
//! the **Task trait** instead of the **Node trait** to show the difference:
//!
//! - **Task trait**: Simple interface with single `run()` method - maximum flexibility
//! - **Node trait**: Structured three-phase lifecycle - built-in retry and structure
//!
//! The workflow includes:
//! 1. **GeneratorTask**: Creates and filters random numbers (single run method)
//! 2. **CounterTask**: Counts the filtered numbers (single run method)
//!
//! Both Tasks and Nodes can be registered using the same `.register()` method
//! and mixed in the same workflow since every Node automatically implements Task.
//!
//! Run with:
//! ```bash
//! cargo run --example task_simple
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use rand::Rng;

/// Action enum for controlling workflow flow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Action {
    Generate,
    Count,
    Complete,
}

/// Simple task that generates and filters random numbers
struct GeneratorTask;

#[async_trait]
impl Task<Action> for GeneratorTask {
    async fn run(&self, store: &MemoryStore) -> CanoResult<Action> {
        println!("🎲 GeneratorTask: Creating random numbers...");

        // Generate random numbers (combining what would be prep + exec in a Node)
        let mut rng = rand::rng();
        let numbers: Vec<u32> = (0..10).map(|_| rng.random_range(1..=100)).collect();
        println!("Generated numbers: {:?}", numbers);

        // Filter out odd numbers (still in the same run method)
        let even_numbers: Vec<u32> = numbers.into_iter().filter(|&n| n % 2 == 0).collect();
        println!("Filtered to even numbers: {:?}", even_numbers);

        // Store the result and determine next action (combining what would be post in a Node)
        store.put("even_numbers", even_numbers)?;
        println!("✅ GeneratorTask: Stored even numbers, moving to Count\n");

        Ok(Action::Count)
    }
}

/// Simple task that counts the numbers
struct CounterTask;

#[async_trait]
impl Task<Action> for CounterTask {
    async fn run(&self, store: &MemoryStore) -> CanoResult<Action> {
        println!("🔢 CounterTask: Counting numbers...");

        // Get the numbers and count them (all in one method)
        let numbers: Vec<u32> = store.get("even_numbers")?;
        let count = numbers.len();
        let sum: u32 = numbers.iter().sum();

        println!("Found {} even numbers with sum: {}", count, sum);

        // Store results and complete
        store.put("count", count)?;
        store.put("sum", sum)?;
        println!("✅ CounterTask: Processing complete!\n");

        Ok(Action::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Starting Task-based workflow example\n");

    // Create workflow with initial state
    let mut workflow = Workflow::new(Action::Generate);

    // Register tasks using the unified .register() method
    // (Same method works for both Tasks and Nodes!)
    workflow.register(Action::Generate, GeneratorTask);
    workflow.register(Action::Count, CounterTask);

    // Set exit state
    workflow.add_exit_states(vec![Action::Complete]);

    // Run the workflow
    let store = MemoryStore::new();
    match workflow.orchestrate(&store).await {
        Ok(_final_state) => {
            println!("🎉 Workflow completed!");
            println!("📊 Final Results:");

            if let Ok(count) = store.get::<usize>("count") {
                println!("   • Count of even numbers: {count}");
            }

            if let Ok(sum) = store.get::<u32>("sum") {
                println!("   • Sum of even numbers: {sum}");
            }

            if let Ok(numbers) = store.get::<Vec<u32>>("even_numbers") {
                println!("   • Even numbers: {numbers:?}");
            }
        }
        Err(e) => {
            eprintln!("❌ Workflow failed: {e}");
            return Err(e);
        }
    }

    println!("\n💡 Compare this Task-based approach with examples/workflow_simple.rs");
    println!("   to see the difference between Task and Node implementations!");

    Ok(())
}
