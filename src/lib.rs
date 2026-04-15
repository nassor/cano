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
//! // A typed configuration struct shared with tasks via Resources.
//! struct FetchConfig { batch_size: usize }
//! impl Resource for FetchConfig {}
//!
//! struct FetchTask;
//! struct ProcessTask;
//!
//! #[async_trait]
//! impl Task<Step> for FetchTask {
//!     async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         let config = res.get::<FetchConfig, str>("config")?;
//!         let store = res.get::<MemoryStore, str>("store")?;
//!         // Produce `batch_size` items and store them for the next task.
//!         let data: Vec<u32> = (1..=(config.batch_size as u32)).collect();
//!         store.put("data", data)?;
//!         Ok(TaskResult::Single(Step::Process))
//!     }
//! }
//!
//! #[async_trait]
//! impl Task<Step> for ProcessTask {
//!     async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         let store = res.get::<MemoryStore, str>("store")?;
//!         let data: Vec<u32> = store.get("data")?;
//!         store.put("sum", data.iter().sum::<u32>())?;
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let store = MemoryStore::new();
//! let resources = Resources::<String>::new()
//!     .insert("config", FetchConfig { batch_size: 3 })
//!     .insert("store", store.clone());
//! let workflow = Workflow::new(resources)
//!     .register(Step::Fetch, FetchTask)
//!     .register(Step::Process, ProcessTask)
//!     .add_exit_state(Step::Done);
//!
//! let final_state = workflow.orchestrate(Step::Fetch).await?;
//! assert_eq!(final_state, Step::Done);
//!
//! // The sum of 1..=3 is 6.
//! assert_eq!(store.get::<u32>("sum")?, 6);
//! # Ok(())
//! # }
//! ```
//!
//! ## Resource-Free Workflows
//!
//! When tasks do not need shared resources, use [`Workflow::bare`] and implement
//! [`run_bare`](task::Task::run_bare) instead of `run`:
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Compute, Done }
//!
//! struct ComputeTask;
//!
//! #[async_trait]
//! impl Task<Step> for ComputeTask {
//!     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
//!         // Pure computation — no resource lookup needed.
//!         let result: u32 = (1..=100).sum();
//!         assert_eq!(result, 5050);
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let workflow = Workflow::bare()
//!     .register(Step::Compute, ComputeTask)
//!     .add_exit_state(Step::Done);
//!
//! let final_state = workflow.orchestrate(Step::Compute).await?;
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
//! **Node**: Three-phase lifecycle (retried as a unit on `prep` or `post` failure):
//! 1. `prep` — load data, validate inputs, allocate resources
//! 2. `exec` — core logic (infallible — signature returns result directly)
//! 3. `post` — write results, determine next state
//!
//! ## Module Overview
//!
//! - [`task`]: The [`Task`] trait — single `run()` method
//! - [`node`]: The [`Node`] trait — three-phase lifecycle with retry via [`TaskConfig`]
//! - [`workflow`]: [`Workflow`] — FSM orchestration with Split/Join support
//! - [`scheduler`] (requires `scheduler` feature): [`Scheduler`] — cron and interval scheduling
//! - [`resource`]: [`Resource`] trait and [`Resources`] dictionary — lifecycle-aware resource management
//! - [`store`]: [`MemoryStore`] and the [`KeyValueStore`] trait — [`MemoryStore`] implements [`Resource`]
//! - [`error`]: [`CanoError`] variants and the [`CanoResult`] alias
//!
//! ## Getting Started
//!
//! 1. Run an example: `cargo run --example workflow_simple`
//! 2. Read the module docs — each module has detailed documentation and examples
//! 3. Run benchmarks: `cargo bench --bench node_performance`

pub mod error;
pub mod node;
pub mod resource;
pub mod store;
pub mod task;
pub mod workflow;

#[cfg(feature = "scheduler")]
pub mod scheduler;

#[cfg(all(test, feature = "tracing"))]
mod tracing_tests;

// Core public API - simplified imports
pub use error::{CanoError, CanoResult};
pub use node::{DefaultNodeResult, DynNode, Node, NodeObject};
pub use resource::{Resource, Resources};
pub use store::{KeyValueStore, MemoryStore};
pub use task::{DynTask, RetryMode, Task, TaskConfig, TaskObject, TaskResult};
pub use workflow::{JoinConfig, JoinStrategy, SplitResult, SplitTaskResult, StateEntry, Workflow};

#[cfg(feature = "scheduler")]
pub use scheduler::{FlowInfo, Scheduler};

// Convenience re-exports for common patterns
pub mod prelude {
    //! Simplified imports for common usage patterns
    //!
    //! Use `use cano::prelude::*;` to import the most commonly used types and traits.

    pub use crate::{
        CanoError, CanoResult, DefaultNodeResult, JoinConfig, JoinStrategy, KeyValueStore,
        MemoryStore, Node, Resource, Resources, RetryMode, SplitResult, SplitTaskResult,
        StateEntry, Task, TaskConfig, TaskObject, TaskResult, Workflow,
    };

    #[cfg(feature = "scheduler")]
    pub use crate::{FlowInfo, Scheduler};

    // Re-export async_trait for convenience
    pub use async_trait::async_trait;
}
