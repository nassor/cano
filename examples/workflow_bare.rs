//! # Bare Workflow Example
//!
//! Demonstrates three ergonomic features for resource-free tasks:
//!
//! - **`Task::run_bare()`** — override instead of `run()` when the task needs no resources.
//!   The default `run()` delegates to `run_bare()`, so bare tasks work in any workflow.
//! - **`Workflow::bare()`** — construct a workflow with no resources.
//! - **`Resources::empty()`** — named constructor alternative to `Workflow::bare()`.
//!
//! Use `run_bare()` for pure computation; use `run()` when the task accesses the store
//! or any other resource. Bare tasks can be mixed freely with resource tasks.
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_bare
//! ```

use async_trait::async_trait;
use cano::prelude::*;

/// Workflow states for a simple data validation pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage {
    Validate, // pure logic — no resources needed
    Sanitize, // pure logic — no resources needed
    Persist,  // writes to a store — needs resources
    Done,
}

// Bare tasks override `run_bare()` — no resources needed, no unused `_res` parameter.

/// Validates that a hardcoded input passes business rules (pure computation).
struct ValidateTask;

#[async_trait]
impl Task<Stage> for ValidateTask {
    async fn run_bare(&self) -> CanoResult<TaskResult<Stage>> {
        println!("ValidateTask: checking input constraints...");
        // Pure logic — no resources accessed.
        let value: i32 = 42;
        if value < 0 {
            return Err(CanoError::task_execution("value must be non-negative"));
        }
        println!("  value={value} -- OK");
        Ok(TaskResult::Single(Stage::Sanitize))
    }
}

/// Clamps the validated value to [0, 100] (pure computation).
struct SanitizeTask;

#[async_trait]
impl Task<Stage> for SanitizeTask {
    async fn run_bare(&self) -> CanoResult<TaskResult<Stage>> {
        println!("SanitizeTask: clamping value to [0, 100]...");
        let raw: i32 = 42;
        let clamped = raw.clamp(0, 100);
        println!("  raw={raw} -> clamped={clamped}");
        Ok(TaskResult::Single(Stage::Persist))
    }
}

// Resource tasks override `run()` to access the store or other resources.

/// Persists the sanitized value to the shared store.
struct PersistTask;

#[async_trait]
impl Task<Stage> for PersistTask {
    async fn run(&self, res: &Resources) -> CanoResult<TaskResult<Stage>> {
        let store = res.get::<MemoryStore, str>("store")?;
        println!("PersistTask: writing result to store...");
        let result: i32 = 42_i32.clamp(0, 100);
        store.put("sanitized_value", result)?;
        println!("  stored sanitized_value={result}");
        Ok(TaskResult::Single(Stage::Done))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("=== Bare workflow (no resources) ===\n");

    // Workflow::bare() is equivalent to Workflow::new(Resources::empty()) —
    // choose whichever reads more clearly at the call site.
    let workflow = Workflow::bare()
        .register(Stage::Validate, ValidateTask)
        .register(Stage::Sanitize, SanitizeTask)
        .add_exit_states(vec![Stage::Persist, Stage::Done]);

    match workflow.orchestrate(Stage::Validate).await {
        Ok(final_state) => println!("\nBare workflow reached: {final_state:?}\n"),
        Err(e) => {
            eprintln!("Workflow failed: {e}");
            return Err(e);
        }
    }

    println!("=== Mixed workflow (bare tasks + resource task) ===\n");

    // Bare tasks work in any workflow — the default run() delegates to run_bare().
    let store = MemoryStore::new();
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(Stage::Validate, ValidateTask) // bare task
        .register(Stage::Sanitize, SanitizeTask) // bare task
        .register(Stage::Persist, PersistTask) // resource task
        .add_exit_states(vec![Stage::Done]);

    match workflow.orchestrate(Stage::Validate).await {
        Ok(final_state) => {
            println!("\nMixed workflow reached: {final_state:?}");
            if let Ok(v) = store.get::<i32>("sanitized_value") {
                println!("Persisted value: {v}");
            }
        }
        Err(e) => {
            eprintln!("Workflow failed: {e}");
            return Err(e);
        }
    }

    Ok(())
}
