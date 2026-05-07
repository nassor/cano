//! # Task Interface Example
//!
//! This example demonstrates the new simplified Task interface compared to the traditional Node interface.
//! It shows how to implement both Node and Task, and how they can be used interchangeably in workflows.
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
    ProcessWithNode,
    ProcessWithTask,
    Complete,
    Error,
}

/// Traditional Node implementation (three-phase lifecycle)
#[derive(Clone)]
struct ProcessingNode {
    name: String,
}

impl ProcessingNode {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[node(state = TaskState)]
impl ProcessingNode {
    type PrepResult = String;
    type ExecResult = String;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal() // Fast execution
    }

    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        println!("🔧 Node '{}' - Prep phase: Loading data", self.name);
        Ok(format!("data_for_{}", self.name))
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!(
            "⚙️  Node '{}' - Exec phase: Processing {}",
            self.name, prep_res
        );
        format!("processed_{}", prep_res)
    }

    async fn post(
        &self,
        res: &Resources,
        exec_res: Self::ExecResult,
    ) -> Result<TaskState, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("📝 Node '{}' - Post phase: Storing {}", self.name, exec_res);
        store.put("node_result", exec_res)?;
        Ok(TaskState::ProcessWithTask)
    }
}

/// Simple Task implementation (single run method)
#[derive(Clone)]
struct ProcessingTask {
    name: String,
}

impl ProcessingTask {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[task(state = TaskState)]
impl ProcessingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TaskState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;

        println!(
            "🚀 Task '{}' - Single run method: doing everything",
            self.name
        );

        // All-in-one: prep, exec, post
        let node_result: String = store
            .get("node_result")
            .map_err(|e| CanoError::node_execution(format!("Failed to load node result: {e}")))?;

        println!("   📥 Loading previous result: {}", node_result);

        let processed = format!("task_enhanced_{node_result}");
        println!("   ⚙️  Processing: {}", processed);

        store.put("final_result", processed.clone())?;
        println!("   📤 Stored final result: {}", processed);

        Ok(TaskResult::Single(TaskState::Complete))
    }
}

/// Data initializer task
#[derive(Clone)]
struct InitializerTask;

#[task(state = TaskState)]
impl InitializerTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TaskState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;

        println!("🎯 Initializer Task - Setting up workflow data");
        store.put("workflow_id", "demo_123".to_string())?;
        store.put("start_time", std::time::SystemTime::now())?;
        Ok(TaskResult::Single(TaskState::ProcessWithNode))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🚀 Task Interface Demo");
    println!("======================");
    println!();

    println!("This demo shows the difference between Node and Task interfaces:");
    println!("• Node: Three-phase lifecycle (prep, exec, post) - more structured");
    println!("• Task: Single run method - simpler and more flexible");
    println!("• Both can be used in the same workflow!");
    println!();

    let store = MemoryStore::new();
    let resources = Resources::new().insert("store", store.clone());

    // Create workflow with mixed Node and Task implementations
    let workflow = Workflow::new(resources)
        .register(TaskState::Start, InitializerTask) // Task implementation
        .register(
            TaskState::ProcessWithNode,
            ProcessingNode::new("NodeProcessor"),
        ) // Node implementation
        .register(
            TaskState::ProcessWithTask,
            ProcessingTask::new("TaskProcessor"),
        ) // Task implementation
        .add_exit_states(vec![TaskState::Complete, TaskState::Error]);

    println!("📋 Workflow Overview:");
    println!(
        "   Start → InitializerTask (Task) → ProcessingNode (Node) → ProcessingTask (Task) → Complete"
    );
    println!();

    // Execute the workflow
    println!("🏃 Executing workflow...");
    println!();

    match workflow.orchestrate(TaskState::Start).await {
        Ok(final_state) => {
            println!();
            println!("✅ Workflow completed successfully!");
            println!("   Final state: {final_state:?}");

            // Show results
            if let Ok(workflow_id) = store.get::<String>("workflow_id") {
                println!("   Workflow ID: {workflow_id}");
            }
            if let Ok(final_result) = store.get::<String>("final_result") {
                println!("   Final result: {final_result}");
            }
        }
        Err(e) => {
            println!("❌ Workflow failed: {e}");
            return Err(e);
        }
    }

    println!();
    println!("🎉 Demo completed!");
    println!();
    println!("Key takeaways:");
    println!("• Tasks provide a simpler interface for straightforward operations");
    println!("• Nodes provide structured lifecycle management for complex operations");
    println!("• Both can be mixed in the same workflow seamlessly");
    println!("• Existing Node implementations automatically work as Tasks");

    Ok(())
}
