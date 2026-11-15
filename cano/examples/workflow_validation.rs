//! # Workflow Validation
//!
//! Run with: `cargo run --example workflow_validation`
//!
//! Demonstrates [`Workflow::validate`] and [`Workflow::validate_initial_state`] — the two
//! static checks that catch misconfigured workflows before any async execution happens.
//!
//! Three scenarios are shown:
//!
//! 1. **Happy path** — a well-formed workflow validates and runs cleanly.
//! 2. **Misconfigured split** — a `register_split` whose `join_state` is neither registered
//!    nor an exit state; `validate()` catches the dangling transition target.
//! 3. **Bad initial state** — `validate_initial_state` rejects an unregistered start state.
//!
//! `validate()` is also called implicitly by `orchestrate` (cached via `OnceLock`), but
//! calling it explicitly lets you surface errors earlier — at build time or on startup —
//! rather than on the first run.

use cano::prelude::*;

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Prepare,
    Process,
    Aggregate,
    Done,
    // A state that we intentionally leave unregistered to provoke validation failures.
    Orphan,
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

struct PrepareTask;

#[task(state = Step)]
impl PrepareTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("  prepare: loading work items");
        Ok(TaskResult::Single(Step::Process))
    }
}

struct WorkerTask {
    id: usize,
}

#[task(state = Step)]
impl WorkerTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("  worker {}: done", self.id);
        Ok(TaskResult::Single(Step::Aggregate))
    }
}

struct AggregateTask;

#[task(state = Step)]
impl AggregateTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("  aggregate: collecting results");
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Workflow Validation Demo ===\n");

    // -----------------------------------------------------------------------
    // Scenario 1: Happy path — validate() passes, orchestrate succeeds.
    // -----------------------------------------------------------------------
    println!("-- Scenario 1: well-formed workflow --");
    {
        let workers = vec![WorkerTask { id: 1 }, WorkerTask { id: 2 }];
        let join_config = JoinConfig::new(JoinStrategy::All, Step::Aggregate);

        let workflow = Workflow::bare()
            .register(Step::Prepare, PrepareTask)
            .register_split(Step::Process, workers, join_config)
            .register(Step::Aggregate, AggregateTask)
            .add_exit_state(Step::Done);

        // Explicit pre-run validation — useful in service startup code.
        match workflow.validate() {
            Ok(()) => println!("  validate() -> Ok"),
            Err(e) => println!("  validate() -> Err: {e}"),
        }

        // validate_initial_state lets you check that a specific entry point is sane.
        match workflow.validate_initial_state(&Step::Prepare) {
            Ok(()) => println!("  validate_initial_state(Prepare) -> Ok"),
            Err(e) => println!("  validate_initial_state(Prepare) -> Err: {e}"),
        }

        let result = workflow.orchestrate(Step::Prepare).await?;
        println!("  orchestrate -> {result:?}\n");
    }

    // -----------------------------------------------------------------------
    // Scenario 2: Dangling split join_state — validate() rejects it.
    //
    // The split at `Process` targets `Step::Orphan` as its join_state, but
    // `Orphan` is neither registered as a task handler nor declared as an exit
    // state. Orchestration would always fail at runtime after the split
    // completed — validate() surfaces this error immediately.
    // -----------------------------------------------------------------------
    println!("-- Scenario 2: split join_state is neither registered nor an exit state --");
    {
        let workers = vec![WorkerTask { id: 1 }, WorkerTask { id: 2 }];
        // join_state points to an unregistered, non-exit state: Orphan.
        let bad_join = JoinConfig::new(JoinStrategy::All, Step::Orphan);

        let workflow = Workflow::bare()
            .register(Step::Prepare, PrepareTask)
            .register_split(Step::Process, workers, bad_join)
            // Orphan is intentionally not registered and not added as an exit state.
            .add_exit_state(Step::Done);

        match workflow.validate() {
            Ok(()) => println!("  validate() -> Ok  (unexpected!)"),
            Err(e) => println!("  validate() -> Err (expected): {e}"),
        }

        // Also no exit states at all — another validate() failure path.
        let no_exit = Workflow::bare().register(Step::Prepare, PrepareTask);
        match no_exit.validate() {
            Ok(()) => println!("  no-exit-state validate() -> Ok  (unexpected!)"),
            Err(e) => println!("  no-exit-state validate() -> Err (expected): {e}"),
        }

        println!();
    }

    // -----------------------------------------------------------------------
    // Scenario 3: validate_initial_state rejects an unregistered start state.
    //
    // The workflow is otherwise valid, but we pass an unregistered state as
    // the initial entry point. validate_initial_state() catches this before
    // any tasks run; orchestrate() would surface the same error on first call.
    // -----------------------------------------------------------------------
    println!("-- Scenario 3: unregistered initial state --");
    {
        let workflow = Workflow::bare()
            .register(Step::Prepare, PrepareTask)
            .register(Step::Aggregate, AggregateTask)
            .add_exit_state(Step::Done);

        // `Process` is not registered in this workflow.
        match workflow.validate_initial_state(&Step::Process) {
            Ok(()) => println!("  validate_initial_state(Process) -> Ok  (unexpected!)"),
            Err(e) => println!("  validate_initial_state(Process) -> Err (expected): {e}"),
        }

        // Exit states are valid initial states (they're skipped immediately).
        match workflow.validate_initial_state(&Step::Done) {
            Ok(()) => println!("  validate_initial_state(Done)    -> Ok  (exit states are valid)"),
            Err(e) => println!("  validate_initial_state(Done)    -> Err (unexpected!): {e}"),
        }
    }

    println!("\n=== Done ===");
    Ok(())
}
