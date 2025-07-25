//! # Cano: Simple & Fast Async Workflows in Rust
//!
//! Cano is an async workflow engine that makes complex data processing simple. Whether you need
//! to process one item or millions, Cano provides a clean API with minimal overhead for maximum performance.
//!
//! ## 🚀 Quick Start
//!
//! Create a node that processes data, create store for sharing data between nodes,
//! then run your workflow. The basic pattern involves creating a `Node`, setting up
//! a `MemoryStore`, and calling the `run` method with error handling.
//!
//! ## 🎯 Core Concepts
//!
//! ### Nodes - Your Processing Units
//!
//! A [`Node`] trait is where your logic lives. Implement it once, and Cano handles the execution.
//! The design focuses on simplicity and directness for maximum performance.
//!
//! ### store - Share Data Between Nodes
//!
//! Use [`MemoryStore`] to pass data around your workflow. Store different types of data
//! using key-value pairs, and retrieve them later with type safety. All values are
//! wrapped in `std::borrow::Cow` for memory efficiency.
//!
//! ### Custom Nodes - Your Business Logic
//!
//! Implement the [`Node`] trait to add your own processing logic. Every node follows
//! a simple three-phase lifecycle: Prep (load data, validate inputs), Exec (core processing),
//! and Post (store results, determine next action). Your custom nodes define the business
//! logic for each phase.
//!
//! ## 🏗️ Node Lifecycle
//!
//! Every node follows a simple three-phase lifecycle:
//!
//! 1. **Prep**: Load data, validate inputs, setup resources
//! 2. **Exec**: Core processing logic (with automatic retry support)
//! 3. **Post**: Store results, cleanup, determine next action
//!
//! This structure makes nodes predictable and easy to reason about.
//!
//! ## 📚 Module Overview
//!
//! - **[`workflow`]**: Core workflow orchestration
//!   - [`Workflow`] for state machine-based workflows
//!
//! - **[`scheduler`]**: Advanced workflow scheduling
//!   - [`Scheduler`] for managing multiple flows with cron support
//!   - Time-based and event-driven scheduling
//!
//! - **[`node`]**: The [`Node`] trait for custom processing logic
//!   - Built-in retry logic and error handling
//!   - Fluent configuration API via [`NodeConfig`]
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
pub mod scheduler;
pub mod store;
pub mod workflow;

// Core public API - simplified imports
pub use error::{CanoError, CanoResult};
pub use node::{DefaultNodeResult, DefaultParams, DynNode, Node, NodeConfig, RetryMode};
pub use scheduler::{FlowInfo, Scheduler};
pub use store::{KeyValueStore, MemoryStore};
pub use workflow::{Workflow, WorkflowBuilder};

// Convenience re-exports for common patterns
pub mod prelude {
    //! Simplified imports for common usage patterns
    //!
    //! Use `use cano::prelude::*;` to import the most commonly used types and traits.

    pub use crate::{
        CanoError, CanoResult, DefaultNodeResult, DefaultParams, FlowInfo, KeyValueStore,
        MemoryStore, Node, NodeConfig, RetryMode, Scheduler, Workflow, WorkflowBuilder,
    };

    // Re-export async_trait for convenience
    pub use async_trait::async_trait;
}
