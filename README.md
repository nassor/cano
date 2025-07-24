# Cano: Simple & Fast Async Workflows in Rust 🚀

[![Crates.io](https://img.shields.io/crates/v/cano.svg)](https://crates.io/crates/cano)
[![Documentation](https://docs.rs/cano/badge.svg)](https://docs.rs/cano)
[![Downloads](https://img.shields.io/crates/d/cano.svg)](https://crates.io/crates/cano)
[![License](https://img.shields.io/crates/l/cano.svg)](https://github.com/nassor/cano/blob/main/LICENSE)
[![CI](https://github.com/nassor/cano/workflows/CI/badge.svg)](https://github.com/nassor/cano/actions)

**Async workflow engine with built-in scheduling, retry logic, and state machine semantics.**

Cano is a lightweight, async workflow engine for Rust that turns complex processing into simple, composable workflows.

The engine is built on three core concepts: **Nodes** to encapsulate your business logic, **Workflows** to manage state transitions, and **Schedulers** to run your workflows on a schedule. This library-driven approach allows you to define complex processes with ease.

The Node API is inspired by the [PocketFlow](https://github.com/The-Pocket/PocketFlow) project, but has been re-imagined for Rust's async ecosystem with a strong focus on simplicity and performance.

## ⚡ Quick Start

Add Cano to your `Cargo.toml`:

```toml
[dependencies]
cano = "0.1"
async-trait = "0.1"
tokio = { version = "1.0", features = ["full"] }
```

### The Simplest Example

```rust
use cano::prelude::*;

// Define your workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Start,
    Process,
    Complete,
}

struct ProcessorNode;

#[async_trait]
impl Node<WorkflowState> for ProcessorNode {
    type PrepResult = String;
    type ExecResult = bool;

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        let input: String = store.get("input").unwrap_or_default();
        Ok(input)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("Processing: {prep_res}");
        true // Success
    }

    async fn post(&self, store: &impl Store, exec_res: Self::ExecResult) 
        -> Result<WorkflowState, CanoError> {
        if exec_res {
            store.put("result", "processed".to_string())?;
            Ok(WorkflowState::Complete)
        } else {
            Ok(WorkflowState::Start) // Retry
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // Create a 2-step workflow
    let mut workflow = Workflow::new(WorkflowState::Start);
    workflow.register_node(WorkflowState::Start, ProcessorNode)
        .add_exit_state(WorkflowState::Complete);
    
    // Create store for sharing data between steps
    let store = MemoryStore::new();
    store.put("input", "Hello Cano!".to_string())?;
    
    // Run your workflow
    let result = workflow.orchestrate(&store).await?;
    println!("Workflow completed: {result:?}");
    
    Ok(())
}
```

That's it! You just ran your first Cano workflow.

## 🎯 Why Choose Cano?

- **🏗️ Simple API**: A single `Node` trait handles everything - no complex type hierarchies
- **🔗 Simple Configuration**: Fluent builder pattern makes setup intuitive  
- **🔄 Smart Retries**: Multiple retry strategies (none, fixed, exponential backoff) with jitter support
- **💾 Shared Store**: Thread-safe key-value store for data passing between nodes
- **🌊 Scheduler Scheduling**: Built-in scheduler for running flows on intervals, cron schedules, or manual triggers
- **🌊 Complex Workflows**: Chain nodes together into sophisticated state machine pipelines
- **⚡ Type Safety**: Enum-driven state transitions with compile-time safety
- **🚀 High Performance**: Minimal overhead with direct execution for maximum throughput

## 📖 How It Works

Cano is built around three simple concepts:

### 1. Nodes - Your Processing Units

A `Node` trait is where your logic lives. Configure it once, and Cano handles the execution:

```rust
// Custom node with specific logic
struct EmailProcessor;

#[async_trait]
impl Node<String> for EmailProcessor {
    type PrepResult = String;
    type ExecResult = bool;

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        // Load email data from store
        let email: String = store.get("email").unwrap_or_default();
        Ok(email)
    }

    async fn exec(&self, email: Self::PrepResult) -> Self::ExecResult {
        // Process the email (your business logic here)
        println!("Sending email to: {email}");
        true // Success
    }

    async fn post(&self, store: &impl Store, success: Self::ExecResult) 
        -> Result<String, CanoError> {
        // Store the result and return next action
        if success {
            store.put("result", "sent".to_string())?;
            Ok("complete".to_string())
        } else {
            Ok("retry".to_string())
        }
    }
}
```

#### Built-in Retry Logic

Every node includes configurable retry logic with multiple strategies:

**No Retries** - Fail fast for critical operations:

```rust
impl Node<WorkflowState> for CriticalNode {
    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()  // No retries, fail immediately
    }
    // ... rest of implementation
}
```

**Fixed Retries** - Consistent delays between attempts:

```rust
impl Node<WorkflowState> for ReliableNode {
    fn config(&self) -> NodeConfig {
        NodeConfig::new().with_fixed_retry(3, Duration::from_secs(2))
        // 3 retries with 2 second delays
    }
    // ... rest of implementation
}
```

**Exponential Backoff** - Smart retry with increasing delays:

```rust
impl Node<WorkflowState> for ResilientNode {
    fn config(&self) -> NodeConfig {
        NodeConfig::new().with_exponential_retry(5)
        // 5 retries with exponential backoff (100ms, 200ms, 400ms, 800ms, 1.6s)
    }
    // ... rest of implementation
}
```

**Custom Exponential Backoff** - Full control over retry behavior:

```rust
impl Node<WorkflowState> for CustomNode {
    fn config(&self) -> NodeConfig {
        NodeConfig::new().with_retry(
            RetryMode::exponential_custom(
                3,                              // max retries
                Duration::from_millis(50),      // base delay
                3.0,                            // multiplier  
                Duration::from_secs(10),        // max delay cap
                0.2,                            // 20% jitter
            )
        )
    }
    // ... rest of implementation
}
```

**Why Use Different Retry Modes?**

- **None**: Database transactions, critical validations where failure should be immediate
- **Fixed**: Network calls, file operations where consistent timing is preferred  
- **Exponential**: API calls, external services where you want to back off gracefully
- **Custom Exponential**: High-load scenarios where you need precise control over timing and jitter

### 2. Store - Share Data Between Nodes

Use the built-in store to pass data around your workflow:

```rust
let store = MemoryStore::new();

// Store some data
store.put("user_id", 123)?;
store.put("name", "Alice".to_string())?;

// Retrieve it later
let user_id: Result<i32, _> = store.get("user_id");
let name: Result<String, _> = store.get("name");
```

### 3. Workflows - Chain Nodes Together

Build complex workflows with state machine semantics:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Validate,
    Process,
    SendEmail,
    Complete,
    Error,
}

let mut workflow = Workflow::new(WorkflowState::Validate);
workflow.register_node(WorkflowState::Validate, validator)
    .register_node(WorkflowState::Process, processor)
    .register_node(WorkflowState::SendEmail, email_sender)
    .add_exit_states(vec![WorkflowState::Complete, WorkflowState::Error]);

let result = workflow.orchestrate(&store).await?;
```

#### Complex Workflows

Build sophisticated state machine pipelines with conditional branching, error handling, and retry logic:

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

// Validation node with multiple possible outcomes
struct ValidationNode;

#[async_trait]
impl Node<OrderState> for ValidationNode {
    type PrepResult = String;
    type ExecResult = ValidationResult;

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
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

    async fn post(&self, store: &impl Store, result: Self::ExecResult) 
        -> Result<OrderState, CanoError> {
        match result {
            ValidationResult::Valid => {
                store.put("validation_status", "passed")?;
                Ok(OrderState::Process)
            }
            ValidationResult::Invalid => {
                store.put("validation_status", "needs_sanitization")?;
                Ok(OrderState::Sanitize)
            }
            ValidationResult::CriticalError => {
                store.put("error_reason", "critical_validation_failure")?;
                Ok(OrderState::Error)
            }
        }
    }
}

// Quality check node with conditional processing paths
struct QualityCheckNode;

#[async_trait]
impl Node<OrderState> for QualityCheckNode {
    type PrepResult = (String, i32);
    type ExecResult = QualityScore;

    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        let data: String = store.get("processed_data")?;
        let attempt: i32 = store.get("retry_count").unwrap_or(0);
        Ok((data, attempt))
    }

    async fn exec(&self, (data, attempt): Self::PrepResult) -> Self::ExecResult {
        let score = calculate_quality_score(&data);
        QualityScore { score, attempt }
    }

    async fn post(&self, store: &impl Store, result: Self::ExecResult) 
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

// # ALL OTHER NODES...

// Build the complex workflow
let mut workflow = Workflow::new(OrderState::Start);
workflow
    .register_node(OrderState::Start, DataLoaderNode)
    .register_node(OrderState::LoadData, ValidationNode)
    .register_node(OrderState::Validate, SanitizeNode)
    .register_node(OrderState::Sanitize, ProcessNode)  
    .register_node(OrderState::Process, QualityCheckNode)
    .register_node(OrderState::QualityCheck, EnrichNode)
    .register_node(OrderState::Enrich, CompleteNode)     // -> Complete
    .register_node(OrderState::BasicProcess, CompleteNode) // -> Complete
    .register_node(OrderState::Retry, ProcessNode)        // -> Process
    .register_node(OrderState::Error, CleanupNode)        // -> Failed
    .add_exit_states(vec![
        OrderState::Complete, 
        OrderState::Failed
    ]);

let result = workflow.orchestrate(&store).await?;
```

## Scheduler - Simplified Scheduling

The Scheduler module provides an easy-to-use scheduler for running workflows on various schedules. Perfect for building background job systems, periodic data processing, and automated workflows.

### Quick Start with Scheduler

```rust
use cano::prelude::*;
use tokio::time::Duration;

#[tokio::main]
async fn main() -> CanoResult<()> {
    let mut scheduler: Scheduler<MyState, MemoryStore> = Scheduler::new();
    
    let workflow = Workflow::new(MyState::Start);
    
    // Multiple ways to schedule flows:
    scheduler.every_seconds("task1", workflow.clone(), 30)?;                    // Every 30 seconds
    scheduler.every_minutes("task2", workflow.clone(), 5)?;                     // Every 5 minutes  
    scheduler.every_hours("task3", workflow.clone(), 2)?;                       // Every 2 hours
    scheduler.every("task4", workflow.clone(), Duration::from_millis(500))?;    // Every 500ms
    scheduler.cron("task5", workflow.clone(), "0 */10 * * * *")?;              // Every 10 minutes (cron)
    scheduler.manual("task6", workflow)?;                                       // Manual trigger only
    
    // Start the scheduler
    scheduler.start().await?;
    
    // Trigger a manual workflow
    scheduler.trigger("task6").await?;
    
    // Check workflow status
    if let Some(status) = scheduler.status("task1").await {
        println!("Task1 status: {:?}", status);
    }
    
    // Graceful shutdown with timeout
    scheduler.stop().await?;
    
    Ok(())
}
```

### Scheduler Features

- **🕐 Flexible Scheduling**: Support for intervals, cron expressions, and manual triggers
- **⏰ Convenience Methods**: Easy `every_seconds()`, `every_minutes()`, `every_hours()` helpers
- **📊 Status Monitoring**: Check workflow status, run counts, and last execution times
- **🔧 Manual Control**: Trigger flows manually and monitor execution
- **🛑 Graceful Shutdown**: Stop with timeout to wait for running flows to complete
- **🔄 Concurrent Execution**: Multiple flows can run simultaneously

### Advanced Scheduler Usage

```rust
// Create a more complex scheduled workflow
let mut scheduler = Scheduler::new();

// Data processing every hour
scheduler.every_hours("data_sync", data_processing_flow, 1)?;

// Health checks every 30 seconds  
scheduler.every_seconds("health_check", monitoring_flow, 30)?;

// Daily reports at 9 AM using cron
scheduler.cron("daily_report", reporting_flow, "0 0 9 * * *")?;

// Manual cleanup task
scheduler.manual("cleanup", cleanup_flow)?;

// Start the scheduler
scheduler.start().await?;

// Monitor running flows
tokio::spawn(async move {
    loop {
        let running_count = scheduler.running_count().await;
        println!("Currently running flows: {}", running_count);
        
        // List all workflow statuses
        let flows = scheduler.list().await;
        for flow_info in flows {
            println!("Workflow {}: {:?} (runs: {})", 
                flow_info.id, flow_info.status, flow_info.run_count);
        }
        
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
});

// Graceful shutdown with 30 second timeout
scheduler.stop_with_timeout(Duration::from_secs(30)).await?;
```

## 🧪 Testing & Benchmarks

Verify everything works and see performance metrics:

```bash
# Run all tests
cargo test

# Run performance benchmarks  
cargo bench --bench store_performance
cargo bench --bench flow_performance
cargo bench --bench node_performance

# Run workflow examples
cargo run --example workflow_simple
cargo run --example workflow_book_prepositions
cargo run --example workflow_negotiation

# Run scheduler scheduling examples
cargo run --example scheduler_scheduling
cargo run --example scheduler_duration_scheduling
cargo run --example scheduler_concurrent_workflows
cargo run --example scheduler_graceful_shutdown

Benchmark results are saved in `target/criterion/` with detailed HTML reports.
```

## 📚 Documentation

- **[API Documentation](https://docs.rs/cano)** - Complete API reference
- **[Examples Directory](./examples/)** - Hands-on code examples
- **[Benchmarks](./benches/)** - Performance testing and optimization

## 🤝 Contributing

We welcome contributions! Areas where you can help:

- **📝 Documentation** - Improve guides and examples
- **🔧 Features** - Add new store backends or workflow capabilities  
- **⚡ Performance** - Optimize hot paths and memory usage
- **🧪 Testing** - Add test cases and edge case coverage
- **🐛 Bug Fixes** - Report and fix issues

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

**Ready to build fast, reliable workflows?** Start with the examples and let Cano handle the complexity while you focus on your business logic.
