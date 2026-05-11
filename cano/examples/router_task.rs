//! # RouterTask — Conditional Branching in a Workflow
//!
//! Run with: `cargo run --example router_task`
//!
//! Demonstrates [`RouterTask`] as a side-effect-free branching unit. A router reads
//! a configuration value from [`Resources`] and decides which processing path to take
//! — without touching the store, without side effects, without consuming a checkpoint
//! sequence number.
//!
//! Workflow shape:
//!
//! ```text
//!   Classify ──(router)──► FastPath ──► Done
//!                     │
//!                     └──► SlowPath ──► Done
//! ```
//!
//! The example runs twice: once with a flag that routes to the fast path, and once with
//! a flag that routes to the slow path.

use cano::prelude::*;

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    /// Router state: inspects config and branches.
    Classify,
    /// Taken when `Config::use_fast_path` is `true`.
    FastPath,
    /// Taken when `Config::use_fast_path` is `false`.
    SlowPath,
    /// Terminal exit state.
    Done,
}

// ---------------------------------------------------------------------------
// Shared configuration resource — carries the branching flag
// ---------------------------------------------------------------------------

/// Workflow configuration injected via [`Resources`].
struct Config {
    use_fast_path: bool,
}

// `Resource` lifecycle hooks default to no-ops; we just need the trait impl to
// store this in `Resources`.
#[resource]
impl Resource for Config {}

// ---------------------------------------------------------------------------
// Router task
// ---------------------------------------------------------------------------

/// Reads `Config` from resources and returns the appropriate next step.
///
/// This is the only task in the workflow with branching logic. It is
/// side-effect-free: it reads, never writes.
struct Classifier;

#[router_task(state = Step)]
impl Classifier {
    async fn route(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let config = res.get::<Config, _>("config")?;
        if config.use_fast_path {
            println!("classifier : fast path selected");
            Ok(TaskResult::Single(Step::FastPath))
        } else {
            println!("classifier : slow path selected");
            Ok(TaskResult::Single(Step::SlowPath))
        }
    }
}

// ---------------------------------------------------------------------------
// Downstream tasks
// ---------------------------------------------------------------------------

struct FastProcessor;

#[task(state = Step)]
impl FastProcessor {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("fast path  : processing quickly...");
        Ok(TaskResult::Single(Step::Done))
    }
}

struct SlowProcessor;

#[task(state = Step)]
impl SlowProcessor {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("slow path  : processing thoroughly...");
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_workflow(use_fast_path: bool) -> (Workflow<Step>, impl std::fmt::Debug) {
    let config = Config { use_fast_path };
    let resources = Resources::new().insert("config", config);

    let workflow = Workflow::new(resources)
        // `register_router` stores the task as a Router state entry:
        // dispatched like a normal state but writes no CheckpointRow.
        .register_router(Step::Classify, Classifier)
        .register(Step::FastPath, FastProcessor)
        .register(Step::SlowPath, SlowProcessor)
        .add_exit_state(Step::Done);

    (workflow, ())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("=== fast-path run ===");
    let (workflow, _) = build_workflow(true);
    let result = workflow.orchestrate(Step::Classify).await?;
    assert_eq!(result, Step::Done);
    println!("completed at {result:?}\n");

    println!("=== slow-path run ===");
    let (workflow, _) = build_workflow(false);
    let result = workflow.orchestrate(Step::Classify).await?;
    assert_eq!(result, Step::Done);
    println!("completed at {result:?}");

    Ok(())
}
