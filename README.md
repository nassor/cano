# Cano: Async Data & AI Workflows in Rust

[![Crates.io](https://img.shields.io/crates/v/cano.svg)](https://crates.io/crates/cano)
[![Documentation](https://docs.rs/cano/badge.svg)](https://docs.rs/cano)
[![Downloads](https://img.shields.io/crates/d/cano.svg)](https://crates.io/crates/cano)
[![License](https://img.shields.io/crates/l/cano.svg)](https://github.com/nassor/cano/blob/main/LICENSE)
[![CI](https://github.com/nassor/cano/workflows/CI/badge.svg?branch=main)](https://github.com/nassor/cano/actions)

**Async workflow engine with built-in scheduling, retry logic, and state machine semantics.**

Cano is an async workflow engine for Rust that manages complex processing through composable workflows. It can be used for data processing, AI inference workflows, and background jobs. Cano provides a simple, fast and type-safe API for defining workflows with retry strategies, scheduling capabilities, and shared state management.

The engine is built on three core concepts: **Tasks** and **Nodes** to encapsulate business logic, **Workflows** to manage state transitions, and **Schedulers** to run workflows on a schedule.

*The Node API is inspired by the [PocketFlow](https://github.com/The-Pocket/PocketFlow) project, adapted for Rust's async ecosystem.*

## Features

- **Task & Node APIs**: Single `Task` trait for simple processing logic, or `Node` trait for structured three-phase lifecycle
- **State Machines**: Type-safe enum-driven state transitions with compile-time checking
- **Retry Strategies**: None, fixed delays, and exponential backoff with jitter (for both Tasks and Nodes)
- **Flexible Storage**: Built-in `MemoryStore` or custom struct types for data sharing
- **Workflow Scheduling**: Built-in scheduler with intervals, cron schedules, and manual triggers
- **Concurrent Execution**: Execute multiple workflow instances in parallel with timeout strategies
- **Performance**: Minimal overhead with direct execution and zero-cost abstractions

## Getting Started

Add Cano to your `Cargo.toml`:

```toml
[dependencies]
cano = "0.4"
async-trait = "0.1"
tokio = { version = "1", features = ["full"] }
```

### Basic Example

```rust
use async_trait::async_trait;
use cano::prelude::*;

// Define your workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Start,
    Process,
    Complete,
}

// Simple Task implementation - single run method
struct SimpleTask;

#[async_trait]
impl Task<WorkflowState> for SimpleTask {
    fn config(&self) -> TaskConfig {
        // Configure retry behavior for resilience
        TaskConfig::new().with_exponential_retry(2)
    }

    async fn run(&self, store: &MemoryStore) -> Result<WorkflowState, CanoError> {
        let input: String = store.get("input").unwrap_or_default();
        println!("Processing: {input}");
        store.put("result", "task_processed".to_string())?;
        Ok(WorkflowState::Process)
    }
}

// Structured Node implementation - three-phase lifecycle
struct ProcessorNode;

#[async_trait]
impl Node<WorkflowState> for ProcessorNode {
    type PrepResult = String;
    type ExecResult = bool;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let input: String = store.get("result").unwrap_or_default();
        Ok(input)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("Node processing: {prep_res}");
        true // Success
    }

    async fn post(&self, store: &MemoryStore, exec_res: Self::ExecResult) 
        -> Result<WorkflowState, CanoError> {
        if exec_res {
            store.put("final_result", "node_processed".to_string())?;
            Ok(WorkflowState::Complete)
        } else {
            Ok(WorkflowState::Process) // Retry
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // Create workflow - can mix Tasks and Nodes
    let mut workflow = Workflow::new(WorkflowState::Start);
    workflow.register(WorkflowState::Start, SimpleTask)        // Task
        .register(WorkflowState::Process, ProcessorNode)       // Node
        .add_exit_state(WorkflowState::Complete);
    
    // Create store and run workflow
    let store = MemoryStore::new();
    store.put("input", "Hello Cano!".to_string())?;
    
    let result = workflow.orchestrate(&store).await?;
    println!("Workflow completed: {result:?}");
    
    Ok(())
}
```

## Core Concepts

### 1. Tasks & Nodes - Processing Units

Cano provides two approaches for implementing processing logic:

#### Tasks - Simple & Flexible

A `Task` provides a simplified interface with a single `run` method. Use tasks when you want simplicity and don't need structured phases or built-in retry logic:

```rust
struct DataProcessor;

#[async_trait]
impl Task<String> for DataProcessor {
    fn config(&self) -> TaskConfig {
        // Configure retry behavior (optional)
        TaskConfig::new().with_fixed_retry(3, Duration::from_secs(1))
    }

    async fn run(&self, store: &MemoryStore) -> Result<String, CanoError> {
        // Load data
        let input: String = store.get("input")?;
        
        // Process data
        let result = format!("processed: {input}");
        
        // Store result and determine next state
        store.put("output", result)?;
        Ok("complete".to_string())
    }
}
```

#### Nodes - Structured & Resilient  

A `Node` implements a structured three-phase lifecycle with built-in retry capabilities. Nodes are a superset of Tasks with additional structure and retry strategies:

1. **Prep**: Load data, validate inputs, setup resources
2. **Exec**: Core processing logic (with automatic retry support)  
3. **Post**: Store results, cleanup, determine next action

```rust
struct EmailProcessor;

#[async_trait]
impl Node<String> for EmailProcessor {
    type PrepResult = String;
    type ExecResult = bool;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let email: String = store.get("email").unwrap_or_default();
        Ok(email)
    }

    async fn exec(&self, email: Self::PrepResult) -> Self::ExecResult {
        println!("Sending email to: {email}");
        true // Success
    }

    async fn post(&self, store: &MemoryStore, success: Self::ExecResult) 
        -> Result<String, CanoError> {
        if success {
            store.put("result", "sent".to_string())?;
            Ok("complete".to_string())
        } else {
            Ok("retry".to_string())
        }
    }
}
```

#### Compatibility & When to Use Which

- **Every Node automatically implements Task** - you can use any Node wherever Tasks are accepted
- **Use Task for**: Simple processing, quick prototypes, one-off operations
- **Use Node for**: Production workloads, complex processing, when you need structured three-phase lifecycle

#### Retry Strategies

Both Tasks and Nodes support retry strategies. Configure retry behavior using `TaskConfig`:

```rust
// Task with retry configuration
impl Task<WorkflowState> for ReliableTask {
    fn config(&self) -> TaskConfig {
        // No retries (fail fast)
        TaskConfig::minimal()
        
        // Fixed retries: 3 attempts with 2 second delays
        // TaskConfig::new().with_fixed_retry(3, Duration::from_secs(2))
        
        // Exponential backoff: 5 retries with increasing delays
        // TaskConfig::new().with_exponential_retry(5)
    }

    async fn run(&self, store: &MemoryStore) -> Result<WorkflowState, CanoError> {
        // Your task logic here...
        Ok(WorkflowState::Complete)
    }
}

// Node with retry configuration
impl Node<WorkflowState> for ReliableNode {
    fn config(&self) -> TaskConfig {
        // No retries (fail fast)
        TaskConfig::minimal()
        
        // Fixed retries: 3 attempts with 2 second delays
        // TaskConfig::new().with_fixed_retry(3, Duration::from_secs(2))
        
        // Exponential backoff: 5 retries with increasing delays
        // TaskConfig::new().with_exponential_retry(5)
    }
    // ... rest of implementation
}
```
### 2. Store - Data Sharing

Cano supports flexible data sharing between workflow nodes through stores.

#### MemoryStore (Key-Value Store)

The built-in MemoryStore provides a flexible key-value interface:

```rust
let store = MemoryStore::new();

// Store different types of data
store.put("user_id", 123)?;
store.put("name", "Alice".to_string())?;
store.put("scores", vec![85, 92, 78])?;

// Retrieve data with type safety
let user_id: i32 = store.get("user_id")?;
let name: String = store.get("name")?;

// Append items to existing collections
store.append("scores", 95)?;  // scores is now [85, 92, 78, 95]

// Store operations
let count = store.len()?;
let is_empty = store.is_empty()?;
store.clear()?;
```

#### Custom Store Types

For better performance and type safety, use custom struct types:

```rust
#[derive(Debug, Clone, Default)]
struct RequestCtx {
    pub request_id: String,
    pub transaction_count: i32,
}

#[async_trait]
impl Node<ProcessingState, RequestCtx> for MetricsNode {
    async fn prep(&self, store: &RequestCtx) -> Result<String, CanoError> {
        // Direct field access - no hash map overhead
        Ok(store.request_id.clone())
    }

    async fn post(&self, store: &RequestCtx, result: ProcessingResult) 
        -> Result<ProcessingState, CanoError> {
        println!("Processing request: {}", store.request_id);
        Ok(ProcessingState::Complete)
    }
}
```

### 3. Workflows - State Management

Build workflows with state machine semantics. Workflows can register both Tasks and Nodes using the unified `register` method:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Validate,
    Process,
    Complete,
    Error,
}

let mut workflow = Workflow::new(WorkflowState::Validate);
workflow.register(WorkflowState::Validate, validator_task)  // Task
    .register(WorkflowState::Process, processor_node)       // Node  
    .add_exit_states(vec![WorkflowState::Complete, WorkflowState::Error]);

let result = workflow.orchestrate(&store).await?;
```

#### Complex Workflows

Build sophisticated state machine pipelines with conditional branching and error handling:

```mermaid
graph TD
    A[Start] --> B[LoadData]
    B --> C{Validate}
    C -->|Valid| D[Process]
    C -->|Invalid| E[Sanitize]  
    C -->|Critical Error| F[Error]
    E --> D
    D --> G{QualityCheck}
    G -->|High Quality| H[Enrich]
    G -->|Low Quality| I[BasicProcess]
    G -->|Failed & Retries Left| J[Retry]
    G -->|Failed & No Retries| K[Failed]
    H --> L[Complete]
    I --> L
    J --> D
    F --> M[Cleanup]
    M --> K
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OrderState {
    Start,
    LoadData,
    Validate,
    Sanitize,
    Process,
    QualityCheck,
    Enrich,
    BasicProcess,
    Retry,
    Cleanup,
    Complete,
    Failed,
    Error,
}

// Validation node with multiple outcomes
struct ValidationNode;

#[async_trait]
impl Node<OrderState> for ValidationNode {
    type PrepResult = String;
    type ExecResult = ValidationResult;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let data: String = store.get("raw_data")?;
        Ok(data)
    }

    async fn exec(&self, data: Self::PrepResult) -> Self::ExecResult {
        if data.contains("critical_error") {
            ValidationResult::CriticalError
        } else if data.len() < 10 {
            ValidationResult::Invalid
        } else {
            ValidationResult::Valid
        }
    }

    async fn post(&self, store: &MemoryStore, result: Self::ExecResult) 
        -> Result<OrderState, CanoError> {
        match result {
            ValidationResult::Valid => Ok(OrderState::Process),
            ValidationResult::Invalid => Ok(OrderState::Sanitize),
            ValidationResult::CriticalError => Ok(OrderState::Error),
        }
    }
}

// Quality check node with retry logic
struct QualityCheckNode;

#[async_trait]
impl Node<OrderState> for QualityCheckNode {
    type PrepResult = (String, i32);
    type ExecResult = QualityScore;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let data: String = store.get("processed_data")?;
        let attempt: i32 = store.get("retry_count").unwrap_or(0);
        Ok((data, attempt))
    }

    async fn exec(&self, (data, attempt): Self::PrepResult) -> Self::ExecResult {
        let score = calculate_quality_score(&data);
        QualityScore { score, attempt }
    }

    async fn post(&self, store: &MemoryStore, result: Self::ExecResult) 
        -> Result<OrderState, CanoError> {
        store.put("quality_score", result.score)?;
        
        match result.score {
            90..=100 => Ok(OrderState::Enrich),
            60..=89 => Ok(OrderState::BasicProcess),
            _ if result.attempt < 3 => {
                store.put("retry_count", result.attempt + 1)?;
                Ok(OrderState::Retry)
            }
            _ => Ok(OrderState::Failed),
        }
    }
}

// OTHER NODES ...

// Build the complete workflow
let mut workflow = Workflow::new(OrderState::Start);
workflow
    .register(OrderState::Start, DataLoaderNode)
    .register(OrderState::Validate, ValidationNode)
    .register(OrderState::Sanitize, SanitizeNode)  
    .register(OrderState::Process, ProcessNode)
    .register(OrderState::QualityCheck, QualityCheckNode)
    .register(OrderState::Enrich, EnrichNode)
    .register(OrderState::BasicProcess, CompleteNode)
    .register(OrderState::Retry, ProcessNode)
    .register(OrderState::Error, CleanupNode)
    .add_exit_states(vec![OrderState::Complete, OrderState::Failed]);

let result = workflow.orchestrate(&store).await?;
```

### Concurrent Workflows

Execute multiple workflow instances in parallel with different timeout strategies:

```rust
use cano::prelude::*;

// Create a concurrent workflow with the same API as regular workflows
let mut concurrent_workflow = ConcurrentWorkflow::new(ProcessingState::Start);
concurrent_workflow.register(ProcessingState::Start, processing_node);
concurrent_workflow.add_exit_state(ProcessingState::Complete);

// Execute with different wait strategies
let stores: Vec<MemoryStore> = (0..10).map(|_| MemoryStore::new()).collect();

// Wait for all workflows to complete
let (results, status) = concurrent_workflow
    .execute_concurrent(stores.clone(), WaitStrategy::WaitForever)
    .await?;

// Wait for first 5 to complete, then cancel the rest
let (results, status) = concurrent_workflow
    .execute_concurrent(stores.clone(), WaitStrategy::WaitForQuota(5))
    .await?;

// Execute within time limit
let (results, status) = concurrent_workflow
    .execute_concurrent(stores, WaitStrategy::WaitDuration(Duration::from_secs(30)))
    .await?;
```
## Scheduling Workflows

The Scheduler provides workflow scheduling capabilities for background jobs and automated workflows:

```rust
use cano::prelude::*;
use tokio::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MyState {
    Start,
    Complete,
}

#[derive(Clone)]
struct MyTask;

#[async_trait::async_trait]
impl Node<MyState> for MyTask {
    type PrepResult = ();
    type ExecResult = bool;

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("Executing task...");
        true
    }

    async fn post(&self, _store: &MemoryStore, exec_res: Self::ExecResult) 
        -> Result<MyState, CanoError> {
        if exec_res {
            Ok(MyState::Complete)
        } else {
            Ok(MyState::Start)
        }
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    let mut scheduler = Scheduler::new();
    
    // Create regular workflows with consistent API
    let mut workflow1 = Workflow::new(MyState::Start);
    workflow1.register(MyState::Start, MyTask);
    workflow1.add_exit_state(MyState::Complete);

    let mut workflow2 = Workflow::new(MyState::Start);
    workflow2.register(MyState::Start, MyTask);
    workflow2.add_exit_state(MyState::Complete);
    
    // Schedule regular workflows
    scheduler.every_seconds("task1", workflow1.clone(), 30)?;          // Every 30 seconds
    scheduler.every_minutes("task2", workflow2.clone(), 5)?;           // Every 5 minutes  
    scheduler.cron("task3", workflow1.clone(), "0 0 9 * * *")?;        // Daily at 9 AM
    scheduler.manual("task4", workflow1)?;                             // Manual trigger only
    
    // Create concurrent workflow with identical API to regular workflows
    let mut concurrent_workflow = ConcurrentWorkflow::new(MyState::Start);
    concurrent_workflow.register(MyState::Start, MyTask);
    concurrent_workflow.add_exit_state(MyState::Complete);
    
    // Schedule concurrent workflows (multiple instances in parallel)
    scheduler.manual_concurrent("concurrent1", concurrent_workflow.clone(), 
        3, WaitStrategy::WaitForever)?;                                // 3 instances, wait for all
    scheduler.every_seconds_concurrent("concurrent2", concurrent_workflow, 
        60, 5, WaitStrategy::WaitForQuota(3))?;                        // 5 instances every minute, wait for 3
    
    // Start the scheduler
    scheduler.start().await?;
    
    // Trigger workflows
    scheduler.trigger("task4").await?;
    
    // Monitor status
    if let Some(status) = scheduler.status("task1").await {
        println!("Task1 status: {:?}", status);
    }
    
    // Graceful shutdown
    scheduler.stop().await?;
    
    Ok(())
}
```

### Features

- **Flexible Scheduling**: Intervals, cron expressions, and manual triggers
- **Concurrent Workflows**: Execute multiple workflow instances in parallel with configurable wait strategies
- **Status Monitoring**: Check workflow status, run counts, and execution times
- **Graceful Shutdown**: Stop with timeout for running flows to complete
- **Concurrent Execution**: Multiple flows can run simultaneously

## Examples and Testing

### Run Examples

```bash
# Examples directory contains various workflow implementations
cargo run --example [example_name]
```

### Run Tests and Benchmarks

```bash
# Run all tests
cargo test

# Run benchmarks from the benches directory
cargo bench --bench [benchmark_name]
```

Benchmark results are saved in `target/criterion/`.

## Documentation

- **[API Documentation](https://docs.rs/cano)** - Complete API reference
- **[Examples Directory](./examples/)** - Hands-on code examples
- **[Benchmarks](./benches/)** - Performance testing and optimization

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
