//! # Cano: Type-Safe Async Workflow Engine
//!
//! Cano is an async workflow orchestration engine for Rust built on Finite State Machines (FSM).
//! States are user-defined enums; the engine guarantees type-safe transitions between them.
//!
//! Well-suited for:
//! - Data pipelines: ETL jobs with parallel processing (Split/Join) and aggregation
//! - AI agents: Multi-step inference chains with shared context
//! - Background systems: Scheduled maintenance, periodic reporting, cron jobs
//!
//! ## Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Fetch, Process, Done }
//!
//! struct FetchTask;
//! struct ProcessTask;
//!
//! #[async_trait]
//! impl Task<Step> for FetchTask {
//!     async fn run(&self, store: &MemoryStore) -> Result<TaskResult<Step>, CanoError> {
//!         store.put("data", vec![1u32, 2, 3])?;
//!         Ok(TaskResult::Single(Step::Process))
//!     }
//! }
//!
//! #[async_trait]
//! impl Task<Step> for ProcessTask {
//!     async fn run(&self, store: &MemoryStore) -> Result<TaskResult<Step>, CanoError> {
//!         let data: Vec<u32> = store.get("data")?;
//!         store.put("sum", data.iter().sum::<u32>())?;
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let store = MemoryStore::new();
//! let workflow = Workflow::new(store.clone())
//!     .register(Step::Fetch, FetchTask)
//!     .register(Step::Process, ProcessTask)
//!     .add_exit_state(Step::Done);
//!
//! let final_state = workflow.orchestrate(Step::Fetch).await?;
//! assert_eq!(final_state, Step::Done);
//! # Ok(())
//! # }
//! ```
//!
//! ## Core Concepts
//!
//! ### Finite State Machines (FSM)
//!
//! Workflows in Cano are state machines. You define your states as an `enum`, register
//! handlers ([`Task`] or [`Node`]) for each state, and the engine manages transitions.
//!
//! ### Tasks and Nodes
//!
//! Two interfaces for processing logic:
//! - [`Task`] trait: single `run()` method — straightforward operations and prototyping
//! - [`Node`] trait: three-phase lifecycle (`prep` → `exec` → `post`) with built-in retry
//!
//! Every [`Node`] automatically implements [`Task`] via a blanket impl, so both can be
//! registered with the same [`Workflow::register`] method.
//!
//! ### Parallel Execution (Split/Join)
//!
//! Run tasks concurrently with [`Workflow::register_split`] and join results using
//! strategies: `All`, `Any`, `Quorum(n)`, `Percentage(f64)`, `PartialResults(min)`,
//! or `PartialTimeout`.
//!
//! ### Store
//!
//! [`MemoryStore`] provides a thread-safe `Arc<RwLock<HashMap>>` for sharing typed data
//! between states. Implement [`KeyValueStore`] to plug in a custom backend.
//!
//! ## Processing Lifecycle
//!
//! **Task**: Single `run()` method — full control over execution flow.
//!
//! **Node**: Three-phase lifecycle:
//! 1. `prep` — load data, validate inputs, allocate resources
//! 2. `exec` — core logic; retry wraps this phase
//! 3. `post` — write results, determine next state
//!
//! ## Module Overview
//!
//! - [`task`]: The [`Task`] trait — single `run()` method
//! - [`node`]: The [`Node`] trait — three-phase lifecycle with retry via [`TaskConfig`]
//! - [`workflow`]: [`Workflow`] — FSM orchestration with Split/Join support
//! - [`scheduler`] (requires `scheduler` feature): [`Scheduler`] — cron and interval scheduling
//! - [`store`]: [`MemoryStore`] and the [`KeyValueStore`] trait
//! - [`error`]: [`CanoError`] variants and the [`CanoResult`] alias
//!
//! ## Getting Started
//!
//! 1. Run an example: `cargo run --example workflow_simple`
//! 2. Read the module docs — each module has detailed documentation and examples
//! 3. Run benchmarks: `cargo bench --bench node_performance`

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
pub use task::{DefaultTaskParams, DynTask, RetryMode, Task, TaskConfig, TaskObject, TaskResult};
pub use workflow::{JoinConfig, JoinStrategy, SplitResult, SplitTaskResult, StateEntry, Workflow};

#[cfg(feature = "scheduler")]
pub use scheduler::{FlowInfo, Scheduler};

// Convenience re-exports for common patterns
pub mod prelude {
    //! Simplified imports for common usage patterns
    //!
    //! Use `use cano::prelude::*;` to import the most commonly used types and traits.

    pub use crate::{
        CanoError, CanoResult, DefaultNodeResult, DefaultParams, DefaultTaskParams, JoinConfig,
        JoinStrategy, KeyValueStore, MemoryStore, Node, RetryMode, SplitResult, SplitTaskResult,
        StateEntry, Task, TaskConfig, TaskObject, TaskResult, Workflow,
    };

    #[cfg(feature = "scheduler")]
    pub use crate::{FlowInfo, Scheduler};

    // Re-export async_trait for convenience
    pub use async_trait::async_trait;
}
