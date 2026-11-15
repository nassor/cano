//! # Simple Two-Task Workflow Example
//!
//! Demonstrates a basic two-step workflow using boilerplate-eliminating macros:
//!
//! 1. [`#[derive(Resource)]`](cano::Resource) — turns a struct into a `Resource`
//!    with no-op lifecycle, ready to insert into a `Resources` map.
//! 2. [`#[derive(FromResources)]`](cano::FromResources) — generates a
//!    `from_resources(&Resources<_>)` constructor that pulls each declared
//!    field out of the map. Field-level `#[res("key")]` declares the lookup key.
//! 3. [`#[task(state = ...)]`](cano::task) on an inherent `impl` block — the
//!    macro builds the `impl Task<State> for X` header from the attribute,
//!    and infers the return type from the `run` method signature.
//!
//! The result: the user writes the **business logic** and a few small structs;
//! the macros generate every line of trait-impl boilerplate.
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_simple
//! ```

use cano::prelude::*;
use rand::RngExt;
use std::sync::Arc;

/// Action enum for controlling workflow workflow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowAction {
    Generate,
    Count,
    Complete,
    Error,
}

// =============================================================================
// Workflow-level configuration as a `Resource` (zero lifecycle hooks)
// =============================================================================

/// Tunable bounds for the random-number generator. `#[derive(Resource)]` makes
/// this struct insertable into a `Resources` map; it inherits the no-op
/// `setup` / `teardown` defaults from the `Resource` trait.
#[derive(Resource)]
struct GeneratorConfig {
    min_size: usize,
    max_size: usize,
}

// =============================================================================
// Per-step dependency structs derived from the resource map
// =============================================================================

/// Dependencies the generator task needs.
#[derive(FromResources)]
struct GeneratorDeps {
    #[res("config")]
    config: Arc<GeneratorConfig>,
}

/// Dependencies the store step needs to write the filtered vector.
#[derive(FromResources)]
struct StoreDeps {
    #[res("store")]
    store: Arc<MemoryStore>,
}

// =============================================================================
// Generator task — generates random numbers and filters them
// =============================================================================

/// Generates random numbers and filters out odd values.
#[derive(Clone)]
struct GeneratorTask;

#[task(state = WorkflowAction)]
impl GeneratorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<WorkflowAction>, CanoError> {
        // Load config
        let GeneratorDeps { config } = GeneratorDeps::from_resources(res)?;
        let mut rng = rand::rng();
        let size = rng.random_range(config.min_size..=config.max_size);
        let numbers: Vec<u32> = (0..size).map(|_| rng.random_range(1..=1000)).collect();

        println!("Generated {} random numbers", numbers.len());
        println!("Sample: {:?}", &numbers[..std::cmp::min(10, numbers.len())]);

        // Filter even values
        let evens: Vec<u32> = numbers.into_iter().filter(|&n| n % 2 == 0).collect();
        println!("Filtered to {} even numbers", evens.len());

        // Persist
        let StoreDeps { store } = StoreDeps::from_resources(res)?;
        store.put("filtered_numbers", evens)?;
        println!("Generator task completed");
        Ok(TaskResult::Single(WorkflowAction::Count))
    }
}

// =============================================================================
// Counter task — counts and cleans up
// =============================================================================

/// Loads the filtered vector, counts it, and cleans up.
#[derive(Clone)]
struct CounterTask;

#[task(state = WorkflowAction)]
impl CounterTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<WorkflowAction>, CanoError> {
        let StoreDeps { store } = StoreDeps::from_resources(res)?;
        let numbers: Vec<u32> = store.get("filtered_numbers").map_err(|e| {
            CanoError::task_execution(format!("Failed to load filtered numbers: {e}"))
        })?;
        println!("Loaded {} numbers from memory", numbers.len());

        let count = numbers.len();
        println!("Counted {} even numbers", count);

        store.put("number_count", count)?;
        store.remove("filtered_numbers")?;
        println!(
            "Counter task completed — count {} stored, intermediate data cleaned",
            count
        );
        Ok(TaskResult::Single(WorkflowAction::Complete))
    }
}

// =============================================================================
// Main
// =============================================================================

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("Simple Two-Task Workflow");
    println!("===============================================");

    let store = MemoryStore::new();

    // Build the resource map: a config struct + the shared store.
    let resources = Resources::new()
        .insert(
            "config",
            GeneratorConfig {
                min_size: 25,
                max_size: 150,
            },
        )
        .insert("store", store.clone());

    let workflow = Workflow::new(resources)
        .register(WorkflowAction::Generate, GeneratorTask)
        .register(WorkflowAction::Count, CounterTask)
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    match workflow.orchestrate(WorkflowAction::Generate).await {
        Ok(WorkflowAction::Complete) => {
            println!("\nWorkflow completed successfully!");
            match store.get::<usize>("number_count") {
                Ok(final_count) => {
                    println!("\nFINAL RESULTS");
                    println!("================");
                    println!("Total even numbers found: {final_count}");
                    if store.get::<Vec<u32>>("filtered_numbers").is_err() {
                        println!("Original data successfully cleaned up");
                    }
                }
                Err(e) => {
                    return Err(CanoError::task_execution(format!(
                        "Failed to get final count: {e}"
                    )));
                }
            }
        }
        Ok(other) => {
            eprintln!("Workflow ended in unexpected state: {other:?}");
            return Err(CanoError::workflow(format!(
                "unexpected final state: {other:?}"
            )));
        }
        Err(e) => {
            eprintln!("Workflow execution failed: {e}");
            return Err(e);
        }
    }

    Ok(())
}
