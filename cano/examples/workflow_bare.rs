//! # Bare Workflow Example
//!
//! Demonstrates two ergonomic features for resource-free tasks:
//!
//! - **`async fn run(&self)`** — leave out the resources parameter when the task needs
//!   none; the `#[task]` macro adds it, so such tasks work in any workflow.
//! - **`Workflow::bare()`** — construct a workflow with no resources.
//!
//! Resource-free tasks can be mixed freely with tasks that take `res: &Resources`.
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_bare
//! ```

use cano::prelude::*;

/// Workflow states for a simple data validation pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage {
    Validate, // pure logic — no resources needed
    Sanitize, // pure logic — no resources needed
    Persist,  // writes to the blackboard — needs resources
    Done,
}

// Resource-free tasks write `run(&self)` — no unused `_res` parameter.

/// Validates that a hardcoded input passes business rules (pure computation).
struct ValidateTask;

#[task(state = Stage)]
impl ValidateTask {
    async fn run(&self) -> CanoResult<Stage> {
        println!("ValidateTask: checking input constraints...");
        // Pure logic — no resources accessed.
        let value: i32 = 42;
        if value < 0 {
            return Err(CanoError::task_execution("value must be non-negative"));
        }
        println!("  value={value} -- OK");
        Ok(Stage::Sanitize)
    }
}

/// Clamps the validated value to [0, 100] (pure computation).
struct SanitizeTask;

#[task(state = Stage)]
impl SanitizeTask {
    async fn run(&self) -> CanoResult<Stage> {
        println!("SanitizeTask: clamping value to [0, 100]...");
        let raw: i32 = 42;
        let clamped = raw.clamp(0, 100);
        println!("  raw={raw} -> clamped={clamped}");
        Ok(Stage::Persist)
    }
}

// Resource tasks override `run()` to access the blackboard or other resources.

/// Persists the sanitized value to the shared blackboard.
struct PersistTask;

#[task(state = Stage)]
impl PersistTask {
    async fn run(&self, res: &Resources) -> CanoResult<Stage> {
        let board = res.get::<Blackboard>()?;
        println!("PersistTask: writing result to blackboard...");
        let result: i32 = 42_i32.clamp(0, 100);
        board.put("sanitized_value", result);
        println!("  stored sanitized_value={result}");
        Ok(Stage::Done)
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

    // Resource-free tasks work in any workflow, next to tasks that use resources.
    let board = Blackboard::new();
    let workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(Stage::Validate, ValidateTask) // bare task
        .register(Stage::Sanitize, SanitizeTask) // bare task
        .register(Stage::Persist, PersistTask) // resource task
        .add_exit_states(vec![Stage::Done]);

    match workflow.orchestrate(Stage::Validate).await {
        Ok(final_state) => {
            println!("\nMixed workflow reached: {final_state:?}");
            if let Ok(v) = board.get::<i32>("sanitized_value") {
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
