//! # Simple Two-Node Workflow Example
//!
//! Demonstrates a basic two-step workflow using all three boilerplate-eliminating
//! macros:
//!
//! 1. [`#[derive(Resource)]`](cano::Resource) — turns a struct into a `Resource`
//!    with no-op lifecycle, ready to insert into a `Resources` map.
//! 2. [`#[derive(FromResources)]`](cano::FromResources) — generates a
//!    `from_resources(&Resources<_>)` constructor that pulls each declared
//!    field out of the map. Field-level `#[res("key")]` declares the lookup key.
//! 3. [`#[node(state = ...)]`](cano::node) on an inherent `impl` block — the
//!    macro builds the `impl Node<State> for X` header from the attribute,
//!    enforces that `prep` / `exec` / `post` are present, and infers
//!    `type PrepResult` / `type ExecResult` from the return types of `prep`
//!    and `exec`. A default `fn config(&self) -> TaskConfig` is supplied when
//!    absent.
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

/// Dependencies the generator's `prep` phase needs.
#[derive(FromResources)]
struct GeneratorDeps {
    #[res("config")]
    config: Arc<GeneratorConfig>,
}

/// Dependencies the post-step needs to write the filtered vector.
#[derive(FromResources)]
struct StoreDeps {
    #[res("store")]
    store: Arc<MemoryStore>,
}

// =============================================================================
// Generator node — uses inference (no `type PrepResult` / `type ExecResult`)
// =============================================================================

/// Generates random numbers and filters out odd values.
#[derive(Clone)]
struct GeneratorNode;

#[node(state = WorkflowAction)]
impl GeneratorNode {
    /// Prep: pull configuration from the resource map and produce a random vector.
    async fn prep(&self, res: &Resources) -> Result<Vec<u32>, CanoError> {
        let GeneratorDeps { config } = GeneratorDeps::from_resources(res)?;
        let mut rng = rand::rng();
        let size = rng.random_range(config.min_size..=config.max_size);
        let numbers: Vec<u32> = (0..size).map(|_| rng.random_range(1..=1000)).collect();

        println!("Generated {} random numbers", numbers.len());
        println!("Sample: {:?}", &numbers[..std::cmp::min(10, numbers.len())]);

        Ok(numbers)
    }

    /// Exec: keep only the even values. (Infallible by design — `exec` returns `T`, not `Result<T, _>`.)
    async fn exec(&self, prep_res: Vec<u32>) -> Vec<u32> {
        let evens: Vec<u32> = prep_res.into_iter().filter(|&n| n % 2 == 0).collect();
        println!("Filtered to {} even numbers", evens.len());
        evens
    }

    /// Post: persist the filtered vector to the shared store.
    async fn post(&self, res: &Resources, exec_res: Vec<u32>) -> Result<WorkflowAction, CanoError> {
        let StoreDeps { store } = StoreDeps::from_resources(res)?;
        store.put("filtered_numbers", exec_res)?;
        println!("✓ Generator node completed");
        Ok(WorkflowAction::Count)
    }
}

// =============================================================================
// Counter node — also uses inference
// =============================================================================

/// Loads the filtered vector, counts it, and cleans up.
#[derive(Clone)]
struct CounterNode;

#[node(state = WorkflowAction)]
impl CounterNode {
    /// Prep: pull the filtered numbers out of the store.
    async fn prep(&self, res: &Resources) -> Result<Vec<u32>, CanoError> {
        let StoreDeps { store } = StoreDeps::from_resources(res)?;
        let numbers: Vec<u32> = store
            .get("filtered_numbers")
            .map_err(|e| CanoError::preparation(format!("Failed to load filtered numbers: {e}")))?;
        println!("Loaded {} numbers from memory", numbers.len());
        Ok(numbers)
    }

    /// Exec: count them.
    async fn exec(&self, prep_res: Vec<u32>) -> usize {
        let count = prep_res.len();
        println!("Counted {} even numbers", count);
        count
    }

    /// Post: store the count and clean up the original vector.
    async fn post(&self, res: &Resources, exec_res: usize) -> Result<WorkflowAction, CanoError> {
        let StoreDeps { store } = StoreDeps::from_resources(res)?;
        store.put("number_count", exec_res)?;
        store.remove("filtered_numbers")?;
        println!(
            "✓ Counter node completed — count {} stored, intermediate data cleaned",
            exec_res
        );
        Ok(WorkflowAction::Complete)
    }
}

// =============================================================================
// Main
// =============================================================================

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🚀 Simple Two-Node Workflow");
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
        .register(WorkflowAction::Generate, GeneratorNode)
        .register(WorkflowAction::Count, CounterNode)
        .add_exit_states(vec![WorkflowAction::Complete, WorkflowAction::Error]);

    match workflow.orchestrate(WorkflowAction::Generate).await {
        Ok(WorkflowAction::Complete) => {
            println!("\n✅ Workflow completed successfully!");
            match store.get::<usize>("number_count") {
                Ok(final_count) => {
                    println!("\n📈 FINAL RESULTS");
                    println!("================");
                    println!("Total even numbers found: {final_count}");
                    if store.get::<Vec<u32>>("filtered_numbers").is_err() {
                        println!("✓ Original data successfully cleaned up");
                    }
                }
                Err(e) => {
                    return Err(CanoError::node_execution(format!(
                        "Failed to get final count: {e}"
                    )));
                }
            }
        }
        Ok(other) => {
            eprintln!("⚠️  Workflow ended in unexpected state: {other:?}");
            return Err(CanoError::workflow(format!(
                "unexpected final state: {other:?}"
            )));
        }
        Err(e) => {
            eprintln!("❌ Workflow execution failed: {e}");
            return Err(e);
        }
    }

    Ok(())
}
