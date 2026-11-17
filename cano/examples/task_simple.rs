//! # Simple Task-Based Workflow Example
//!
//! Demonstrates the `Task` trait with a single `run()` method.
//!
//! The workflow includes:
//! 1. **GeneratorTask**: Creates and filters random numbers
//! 2. **CounterTask**: Counts the filtered numbers
//!
//! Run with:
//! ```bash
//! cargo run --example task_simple
//! ```

use cano::prelude::*;
use rand::RngExt;

/// Action enum for controlling workflow flow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Action {
    Generate,
    Count,
    Complete,
}

/// Simple task that generates and filters random numbers
struct GeneratorTask;

#[task(state = Action)]
impl GeneratorTask {
    async fn run(&self, res: &Resources) -> CanoResult<Action> {
        let board = res.get::<Blackboard>()?;

        println!("GeneratorTask: Creating random numbers...");

        let mut rng = rand::rng();
        let numbers: Vec<u32> = (0..10).map(|_| rng.random_range(1..=100)).collect();
        println!("Generated numbers: {:?}", numbers);

        let even_numbers: Vec<u32> = numbers.into_iter().filter(|&n| n % 2 == 0).collect();
        println!("Filtered to even numbers: {:?}", even_numbers);

        board.put("even_numbers", even_numbers);
        println!("GeneratorTask: Stored even numbers, moving to Count\n");

        Ok(Action::Count)
    }
}

/// Simple task that counts the numbers
struct CounterTask;

#[task(state = Action)]
impl CounterTask {
    async fn run(&self, res: &Resources) -> CanoResult<Action> {
        let board = res.get::<Blackboard>()?;

        println!("CounterTask: Counting numbers...");

        let numbers: Vec<u32> = board.get("even_numbers")?;
        let count = numbers.len();
        let sum: u32 = numbers.iter().sum();

        println!("Found {} even numbers with sum: {}", count, sum);

        board.put("count", count);
        board.put("sum", sum);
        println!("CounterTask: Processing complete!\n");

        Ok(Action::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("Starting Task-based workflow example\n");

    let board = Blackboard::new();
    let resources = Resources::new().insert(board.clone());

    let workflow = Workflow::new(resources)
        .register(Action::Generate, GeneratorTask)
        .register(Action::Count, CounterTask)
        .add_exit_states(vec![Action::Complete]);

    match workflow.orchestrate(Action::Generate).await {
        Ok(_final_state) => {
            println!("Workflow completed!");
            println!("Final Results:");

            if let Ok(count) = board.get::<usize>("count") {
                println!("   Count of even numbers: {count}");
            }

            if let Ok(sum) = board.get::<u32>("sum") {
                println!("   Sum of even numbers: {sum}");
            }

            if let Ok(numbers) = board.get::<Vec<u32>>("even_numbers") {
                println!("   Even numbers: {numbers:?}");
            }
        }
        Err(e) => {
            eprintln!("Workflow failed: {e}");
            return Err(e);
        }
    }

    Ok(())
}
