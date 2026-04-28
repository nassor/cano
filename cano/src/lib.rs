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
//! #[resource]
//! impl Resource for FetchConfig {}
//!
//! struct FetchTask;
//! struct ProcessTask;
//!
//! #[task]
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
//! #[task]
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
//! let resources = Resources::new()
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
//! #[task]
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
//! - [`mod@task`]: The [`Task`] trait — single `run()` method
//! - [`mod@node`]: The [`Node`] trait — three-phase lifecycle with retry via [`TaskConfig`]
//! - [`workflow`]: [`Workflow`] — FSM orchestration with Split/Join support
//! - [`scheduler`] (requires `scheduler` feature): [`Scheduler`] — cron and interval scheduling
//! - [`mod@resource`]: [`Resource`] trait and [`Resources`] dictionary — lifecycle-aware resource management
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

/// Attribute macro applied to `Task` trait definitions and `impl Task` blocks
/// to rewrite `async fn` methods into ones returning
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`.
///
/// Functionally identical to [`macro@node`] and [`macro@resource`]; the separate name makes
/// `#[cano::task]` self-documenting at impl sites. The implementation lives in
/// the [`cano-macros`] sibling crate.
///
/// [`cano-macros`]: https://docs.rs/cano-macros
pub use cano_macros::task;

/// Attribute macro applied to the `Node` trait definition and `impl Node` blocks.
///
/// See [`macro@task`] for the rewrite shape; this macro is functionally identical and
/// differs only in name.
pub use cano_macros::node;

/// Attribute macro applied to the `Resource` trait definition and `impl Resource` blocks.
///
/// See [`macro@task`] for the rewrite shape; this macro is functionally identical and
/// differs only in name.
pub use cano_macros::resource;

/// Derive macro that generates a `from_resources` associated function for a struct.
///
/// See [`FromResources`] for the full specification. Each
/// field must be `Arc<T>`; annotate it with `#[res("key")]` or
/// `#[res(EnumKey::Variant)]`.
pub use cano_macros::FromResources;

/// Derive macro that generates an empty `Resource` impl for a struct.
///
/// Equivalent to writing `#[resource] impl Resource for MyStruct {}` by hand,
/// but derived directly from the struct definition. The trait's no-op `setup`
/// and `teardown` defaults take effect automatically.
///
/// In the prelude, this is exported as [`Resource`] alongside the trait of the
/// same name — Rust's separate macro and type namespaces let them coexist.
pub use cano_macros::Resource;

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

    // Re-export the cano async-trait macros for convenience.
    pub use crate::{node, resource, task};

    // Re-export derive macros alongside their trait counterparts. Rust's separate
    // macro and type namespaces let the derive `Resource` coexist with the
    // `Resource` trait imported above.
    pub use crate::FromResources;
    pub use crate::Resource as ResourceDerive;
}
