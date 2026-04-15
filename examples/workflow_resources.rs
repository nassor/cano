//! # Workflow Resources Example
//!
//! This example demonstrates the `Resources` abstraction in Cano workflows.
//! It shows how to:
//!
//! 1. Define custom resource types with setup/teardown lifecycle hooks
//! 2. Use a typed enum as the resource key for compile-time safety
//! 3. Pass workflow parameters as a plain resource (no-op lifecycle)
//! 4. Access typed resources inside tasks via `res.get::<T>(&key)?`
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_resources
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use std::sync::{Arc, Mutex};

// ============================================================================
// Resource key type
// ============================================================================

/// Typed key enum used to identify resources stored in `Resources<Key>`.
///
/// Using an enum instead of string literals gives compile-time safety —
/// a typo in a key is a compile error, not a runtime panic.
#[derive(Debug, Hash, Eq, PartialEq)]
enum Key {
    Store,
    Counter,
    Params,
}

// ============================================================================
// Custom resource: CounterResource
// ============================================================================

/// A resource that tracks how many times its `setup` lifecycle hook has been
/// called. Uses interior mutability so it can be shared across clones.
struct CounterResource {
    setup_count: Arc<Mutex<u32>>,
}

impl CounterResource {
    fn new() -> Self {
        Self {
            setup_count: Arc::new(Mutex::new(0)),
        }
    }

    fn get_setup_count(&self) -> u32 {
        *self.setup_count.lock().unwrap()
    }
}

#[async_trait]
impl Resource for CounterResource {
    async fn setup(&self) -> Result<(), CanoError> {
        *self.setup_count.lock().unwrap() += 1;
        println!("CounterResource: setup called");
        Ok(())
    }

    async fn teardown(&self) -> Result<(), CanoError> {
        println!("CounterResource: teardown called");
        Ok(())
    }
}

// ============================================================================
// Workflow parameters resource
// ============================================================================

/// Plain workflow parameters passed as a resource.
///
/// No lifecycle logic needed — the default `impl Resource` no-ops are fine.
struct WorkflowParams {
    multiplier: u32,
}

impl Resource for WorkflowParams {}

// ============================================================================
// Workflow states
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Init,
    Process,
    Done,
}

// ============================================================================
// Tasks
// ============================================================================

/// Reads `WorkflowParams` and writes an initial value to the store.
struct InitTask;

#[async_trait]
impl Task<Step, Key> for InitTask {
    async fn run(&self, res: &Resources<Key>) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, Key>(&Key::Store)?;
        let params = res.get::<WorkflowParams, Key>(&Key::Params)?;

        let value = 10u32 * params.multiplier;
        store.put("value", value)?;
        println!("InitTask: stored value = {value}");

        Ok(TaskResult::Single(Step::Process))
    }
}

/// Reads the value written by `InitTask`, adds the setup counter, and stores
/// the final result.
struct ProcessTask;

#[async_trait]
impl Task<Step, Key> for ProcessTask {
    async fn run(&self, res: &Resources<Key>) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, Key>(&Key::Store)?;
        let counter = res.get::<CounterResource, Key>(&Key::Counter)?;

        let value: u32 = store.get("value")?;
        let result = value + counter.get_setup_count();
        store.put("result", result)?;
        println!("ProcessTask: result = {result}");

        Ok(TaskResult::Single(Step::Done))
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("Workflow Resources Example");
    println!("==========================");
    println!();

    let store = MemoryStore::new();
    let counter = CounterResource::new();

    // Keep a second handle to the same Arc so we can inspect it after the
    // workflow takes ownership of the primary `CounterResource`.
    let counter_clone = CounterResource {
        setup_count: Arc::clone(&counter.setup_count),
    };

    let resources = Resources::<Key>::new()
        .insert(Key::Store, store.clone())
        .insert(Key::Counter, counter)
        .insert(Key::Params, WorkflowParams { multiplier: 3 });

    let workflow = Workflow::new(resources)
        .register(Step::Init, InitTask)
        .register(Step::Process, ProcessTask)
        .add_exit_state(Step::Done);

    println!("Running workflow...");
    let final_state = workflow.orchestrate(Step::Init).await?;
    assert_eq!(final_state, Step::Done);

    let result: u32 = store.get("result")?;
    println!("Result: {result}");

    // The orchestrator called setup() once before running tasks.
    assert_eq!(counter_clone.get_setup_count(), 1);
    println!(
        "CounterResource setup was called {} time(s)",
        counter_clone.get_setup_count()
    );

    println!();
    println!("Key concepts demonstrated:");
    println!("  - Enum keys for compile-time resource safety");
    println!("  - Custom Resource with setup/teardown lifecycle");
    println!("  - Plain struct resource (WorkflowParams) with no-op lifecycle");
    println!("  - Arc-based sharing for post-workflow inspection");

    Ok(())
}
