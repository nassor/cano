//! # Task Interface Example
//!
//! This example demonstrates how to build a multi-step workflow using the `Task` trait.
//! Every step is a single `run` method — no three-phase boilerplate required.
//!
//! Run with:
//! ```bash
//! cargo run --example task_interface_demo
//! ```

use cano::prelude::*;

/// State enum for controlling task workflow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TaskState {
    Start,
    ProcessFirst,
    ProcessSecond,
    Complete,
    Error,
}

/// First processing task
#[derive(Clone)]
struct ProcessingTask1 {
    name: String,
}

impl ProcessingTask1 {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[task(state = TaskState)]
impl ProcessingTask1 {
    async fn run(&self, res: &Resources) -> Result<TaskState, CanoError> {
        let board = res.get::<Blackboard>()?;

        println!("Task '{}' - Loading data", self.name);
        let data = format!("data_for_{}", self.name);

        println!("Task '{}' - Processing {}", self.name, data);
        let processed = format!("processed_{}", data);

        println!("Task '{}' - Storing {}", self.name, processed);
        board.put("first_result", processed);

        Ok(TaskState::ProcessSecond)
    }
}

/// Second processing task
#[derive(Clone)]
struct ProcessingTask2 {
    name: String,
}

impl ProcessingTask2 {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[task(state = TaskState)]
impl ProcessingTask2 {
    async fn run(&self, res: &Resources) -> Result<TaskState, CanoError> {
        let board = res.get::<Blackboard>()?;

        println!("Task '{}' - Single run method: doing everything", self.name);

        let first_result: String = board
            .get("first_result")
            .map_err(|e| CanoError::task_execution(format!("Failed to load first result: {e}")))?;

        println!("   Loading previous result: {}", first_result);

        let processed = format!("task_enhanced_{first_result}");
        println!("   Processing: {}", processed);

        board.put("final_result", processed.clone());
        println!("   Stored final result: {}", processed);

        Ok(TaskState::Complete)
    }
}

/// Data initializer task
#[derive(Clone)]
struct InitializerTask;

#[task(state = TaskState)]
impl InitializerTask {
    async fn run(&self, res: &Resources) -> Result<TaskState, CanoError> {
        let board = res.get::<Blackboard>()?;

        println!("Initializer Task - Setting up workflow data");
        board.put("workflow_id", "demo_123".to_string());
        board.put("start_time", std::time::SystemTime::now());
        Ok(TaskState::ProcessFirst)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("Task Interface Demo");
    println!("======================");
    println!();

    println!("Multi-step workflow using the Task trait:");
    println!("  Every task uses a single run() method");
    println!("  Resources are shared across all tasks via the resource map");
    println!();

    let board = Blackboard::new();
    let resources = Resources::new().insert(board.clone());

    let workflow = Workflow::new(resources)
        .register(TaskState::Start, InitializerTask)
        .register(
            TaskState::ProcessFirst,
            ProcessingTask1::new("FirstProcessor"),
        )
        .register(
            TaskState::ProcessSecond,
            ProcessingTask2::new("SecondProcessor"),
        )
        .add_exit_states(vec![TaskState::Complete, TaskState::Error]);

    println!("Workflow: Start -> Initializer -> ProcessingTask1 -> ProcessingTask2 -> Complete");
    println!();

    println!("Executing workflow...");
    println!();

    match workflow.orchestrate(TaskState::Start).await {
        Ok(final_state) => {
            println!();
            println!("Workflow completed successfully!");
            println!("   Final state: {final_state:?}");

            if let Ok(workflow_id) = board.get::<String>("workflow_id") {
                println!("   Workflow ID: {workflow_id}");
            }
            if let Ok(final_result) = board.get::<String>("final_result") {
                println!("   Final result: {final_result}");
            }
        }
        Err(e) => {
            println!("Workflow failed: {e}");
            return Err(e);
        }
    }

    println!();
    println!("Demo completed!");

    Ok(())
}
