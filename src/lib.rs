//! # Cano: Simple & Fast Async Workflows in Rust
//!
//! Cano is an async workflow engine that makes complex data processing simple. Whether you need
//! to process one item or millions, Cano provides a clean API with minimal overhead for maximum performance.
//!
//! ## 🚀 Quick Start
//!
//! Choose between [`Task`] (simple) or [`Node`] (structured) for your processing logic,
//! create a [`MemoryStore`] for sharing data, then run your workflow. Every [`Node`] automatically
//! works as a [`Task`] for maximum flexibility.
//!
//! ## 🎯 Core Concepts
//!
//! ### Tasks & Nodes - Your Processing Units
//!
//! **Two approaches for implementing processing logic:**
//! - [`Task`] trait: Simple interface with single `run()` method - perfect for prototypes and simple operations
//! - [`Node`] trait: Structured three-phase lifecycle with built-in retry strategies - ideal for production workloads
//!
//! **Every [`Node`] automatically implements [`Task`]**, providing seamless interoperability and upgrade paths.
//!
//! ### Store - Share Data Between Processing Units
//!
//! Use [`MemoryStore`] to pass data around your workflow. Store different types of data
//! using key-value pairs, and retrieve them later with type safety. All values are
//! wrapped in `std::borrow::Cow` for memory efficiency.
//!
//! ### Custom Logic - Your Business Implementation
//!
//! **Choose the right approach for your needs:**
//! - Implement the [`Task`] trait for simple, single-method processing
//! - Implement the [`Node`] trait for structured processing with three phases:
//!   Prep (load data, validate inputs), Exec (core processing), and Post (store results, determine next action)
//!
//! ## 🏗️ Processing Lifecycle
//!
//! **Task**: Single `run()` method with full control over execution flow
//!
//! **Node**: Three-phase lifecycle for structured processing:
//!
//! 1. **Prep**: Load data, validate inputs, setup resources
//! 2. **Exec**: Core processing logic (with automatic retry support)
//! 3. **Post**: Store results, cleanup, determine next action
//!
//! This structure makes nodes predictable and easy to reason about, while tasks provide maximum flexibility.
//!
//! ## 📚 Module Overview
//!
//! - **[`task`]**: The [`Task`] trait for simple, flexible processing logic
//!   - Single `run()` method for maximum simplicity
//!   - Perfect for prototypes and straightforward operations
//!
//! - **[`node`]**: The [`Node`] trait for structured processing logic  
//!   - Built-in retry logic and error handling
//!   - Three-phase lifecycle (`prep`, `exec`, `post`)
//!   - Fluent configuration API via [`TaskConfig`]
//!
//! - **[`workflow`]**: Core workflow orchestration
//!   - [`Workflow`] for state machine-based workflows
//!
//! - **[`scheduler`]** (optional `scheduler` feature): Advanced workflow scheduling
//!   - [`Scheduler`] for managing multiple flows with cron support
//!   - Time-based and event-driven scheduling
//!
//! - **[`store`]**: Thread-safe key-value storage helpers for pipeline data sharing
//!   - [`MemoryStore`] for in-memory data sharing
//!   - [`KeyValueStore`] trait for custom storage backends
//!
//! - **[`error`]**: Comprehensive error handling system
//!   - [`CanoError`] for categorized error types
//!   - [`CanoResult`] type alias for convenient error handling
//!   - Rich error context and conversion traits
//!
//! ## 📈 Getting Started
//!
//! 1. **Start with the examples**: Run `cargo run --example basic_node_usage`
//! 2. **Read the module docs**: Each module has detailed documentation and examples
//! 3. **Check the benchmarks**: Run `cargo bench --bench node_performance` to see performance
//! 4. **Join the community**: Contribute features, fixes, or feedback
//!
//! ## Performance Characteristics
//!
//! - **Low Latency**: Minimal overhead with direct execution
//! - **High Throughput**: Direct execution for maximum performance
//! - **Memory Efficient**: Scales with data size, not concurrency settings
//! - **Async I/O**: Efficient async operations with tokio runtime

pub mod error;
pub mod node;
pub mod store;
pub mod task;
pub mod workflow;

#[cfg(feature = "scheduler")]
pub mod scheduler;

#[cfg(all(test, feature = "tracing"))]
mod tracing_tests;

// Core public API - simplified imports
pub use error::{CanoError, CanoResult};
pub use node::{DefaultNodeResult, DefaultParams, DynNode, Node};
pub use store::{KeyValueStore, MemoryStore};
pub use task::{DefaultTaskParams, DynTask, RetryMode, Task, TaskConfig, TaskObject};
pub use workflow::{
    ConcurrentWorkflow, ConcurrentWorkflowBuilder, ConcurrentWorkflowStatus, WaitStrategy,
    Workflow, WorkflowBuilder, WorkflowResult,
};

#[cfg(feature = "scheduler")]
pub use scheduler::{FlowInfo, Scheduler};

// Convenience re-exports for common patterns
pub mod prelude {
    //! Simplified imports for common usage patterns
    //!
    //! Use `use cano::prelude::*;` to import the most commonly used types and traits.

    pub use crate::{
        CanoError, CanoResult, ConcurrentWorkflow, ConcurrentWorkflowBuilder,
        ConcurrentWorkflowStatus, DefaultNodeResult, DefaultParams, DefaultTaskParams,
        KeyValueStore, MemoryStore, Node, RetryMode, Task, TaskConfig, TaskObject, WaitStrategy,
        Workflow, WorkflowBuilder, WorkflowResult,
    };

    #[cfg(feature = "scheduler")]
    pub use crate::{FlowInfo, Scheduler};

    // Re-export async_trait for convenience
    pub use async_trait::async_trait;
}
