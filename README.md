# Cano: Simple & Fast Async Workflows in Rust 🚀

[![Crates.io](https://img.shields.io/crates/v/cano.svg)](https://crates.io/crates/cano)
[![Documentation](https://docs.rs/cano/badge.svg)](https://docs.rs/cano)
[![Downloads](https://img.shields.io/crates/d/cano.svg)](https://crates.io/crates/cano)
[![License](https://img.shields.io/crates/l/cano.svg)](https://github.com/nassor/cano/blob/main/LICENSE)
[![CI](https://github.com/nassor/cano/workflows/CI/badge.svg)](https://github.com/nassor/cano/actions)

**Build powerful data processing pipelines with minimal code.**

Cano is an async workflow engine that makes complex data processing simple. Whether you need to process one item or millions, Cano provides a clean API with minimal overhead for maximum performance.

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
    let mut flow = Flow::new(WorkflowState::Start);
    flow.register_node(WorkflowState::Start, ProcessorNode)
        .add_exit_state(WorkflowState::Complete);
    
    // Create store for sharing data between steps
    let store = MemoryStore::new();
    store.put("input", "Hello Cano!".to_string())?;
    
    // Run your workflow
    let result = flow.orchestrate(&store).await?;
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

### 3. Flows - Chain Nodes Together

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

let mut flow = Flow::new(WorkflowState::Validate);
flow.register_node(WorkflowState::Validate, validator)
    .register_node(WorkflowState::Process, processor)
    .register_node(WorkflowState::SendEmail, email_sender)
    .add_exit_states(vec![WorkflowState::Complete, WorkflowState::Error]);

let result = flow.orchestrate(&store).await?;
```

## 🧩 Advanced Features

### Built-in Retry Logic

Every node includes configurable retry logic with multiple strategies:

#### Retry Modes

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

#### Why Use Different Retry Modes?

- **None**: Database transactions, critical validations where failure should be immediate
- **Fixed**: Network calls, file operations where consistent timing is preferred  
- **Exponential**: API calls, external services where you want to back off gracefully
- **Custom Exponential**: High-load scenarios where you need precise control over timing and jitter

### Complex Workflows

Chain multiple nodes together to build sophisticated state machine pipelines:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Validate, Process, Complete, Error }

let mut flow = Flow::new(State::Start);
flow.register_node(State::Start, data_loader)
    .register_node(State::Validate, validator)
    .register_node(State::Process, processor)
    .add_exit_states(vec![State::Complete, State::Error]);

let result = flow.orchestrate(&store).await?;
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

# Run all examples
cargo run --examples examples/simple.rs
cargo run --examples examples/book_prepositions.rs
cargo run --examples examples/negotiation.rs
```

Benchmark results are saved in `target/criterion/` with detailed HTML reports.

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
