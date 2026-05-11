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
//!         let config = res.get::<FetchConfig, _>("config")?;
//!         let store = res.get::<MemoryStore, _>("store")?;
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
//!         let store = res.get::<MemoryStore, _>("store")?;
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
//! between states. For custom backends, register a concrete storage resource in [`Resources`].
//!
//! ### Observability
//!
//! Beyond the optional `tracing` integration, implement [`WorkflowObserver`] and attach it
//! with [`Workflow::with_observer`] to receive synchronous lifecycle and failure callbacks
//! (state entry, task start/success/failure, retry, circuit-open, checkpoint, resume). With
//! the `tracing` feature enabled, [`TracingObserver`] is a ready-made observer that re-emits
//! those callbacks as `tracing` events. Resources can also report their own [`HealthStatus`]
//! via [`Resource::health`], aggregated by [`Resources::check_all_health`].
//!
//! ### Crash Recovery
//!
//! Attach a [`CheckpointStore`] with [`Workflow::with_checkpoint_store`] (plus
//! [`Workflow::with_workflow_id`]) and the FSM records one [`CheckpointRow`] per state
//! entered — *before* that state's task runs. After a crash, [`Workflow::resume_from`]
//! reloads the run, re-enters the FSM at the last checkpointed state, and continues
//! forward; the resumed state's task re-runs, so tasks at and after the resume point must
//! be idempotent. The trait is backend-agnostic; with the `recovery` feature,
//! `RedbCheckpointStore` is a ready-made embedded, ACID implementation. Workflows
//! without a checkpoint store pay nothing. See the `workflow_recovery` example.
//!
//! ### Sagas & Compensation
//!
//! For steps that mutate external systems, implement [`CompensatableTask`] (its `run`
//! returns the next state *and* an `Output`; its `compensate` undoes the step given that
//! `Output`) and register it with [`Workflow::register_with_compensation`]. The engine
//! keeps a per-run compensation stack; if a later state fails, it drains the stack in
//! reverse and runs each `compensate`. A clean rollback returns the original error (and
//! clears the checkpoint log if one is attached); a failed `compensate` produces
//! [`CanoError::CompensationFailed`] with the original error plus every compensation
//! error. With a checkpoint store, outputs are persisted, so a resumed run can still
//! compensate work done before the crash. See the `saga_payment` example.
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
//! - [`task::node`](crate::task::node): The [`Node`] trait — three-phase lifecycle with retry via [`TaskConfig`]
//! - [`workflow`]: [`Workflow`] — FSM orchestration with Split/Join support
//! - `scheduler` (requires `scheduler` feature): `Scheduler` (builder) and `RunningScheduler` (live handle) — cron and interval scheduling
//! - [`mod@resource`]: [`Resource`] trait, [`Resources`] dictionary, and [`HealthStatus`] — lifecycle-aware resource management and health probes
//! - [`observer`]: [`WorkflowObserver`] — synchronous lifecycle/failure event hooks (and [`TracingObserver`], behind the `tracing` feature)
//! - [`recovery`]: [`CheckpointStore`] / [`CheckpointRow`] — append-only checkpoint log for crash recovery (and `RedbCheckpointStore`, behind the `recovery` feature)
//! - [`saga`]: [`CompensatableTask`] — pair a forward step with a compensating action; failures roll back via [`Workflow::register_with_compensation`]
//! - [`store`]: [`MemoryStore`] — a typed in-memory store that implements [`Resource`]
//! - [`error`]: [`CanoError`] variants and the [`CanoResult`] alias
//!
//! ## Getting Started
//!
//! 1. Run an example: `cargo run --example workflow_simple`
//! 2. Read the module docs — each module has detailed documentation and examples
//! 3. Run benchmarks: `cargo bench --bench node_performance`

pub mod circuit;
pub mod error;
pub mod observer;
pub mod recovery;
pub mod resource;
pub mod saga;
pub mod store;
pub mod task;
pub mod workflow;

#[cfg(feature = "scheduler")]
pub mod scheduler;

// Core public API - simplified imports
pub use circuit::{CircuitBreaker, CircuitPolicy, CircuitState, Permit as CircuitPermit};
pub use error::{CanoError, CanoResult};
pub use observer::WorkflowObserver;
pub use recovery::{CheckpointRow, CheckpointStore};
pub use resource::{HealthStatus, Resource, Resources};
pub use saga::{CompensatableTask, ErasedCompensatable};
pub use task::node::{DefaultNodeResult, DynNode, Node, NodeObject};
pub use task::router::{DynRouterTask, RouterTask, RouterTaskObject};

#[cfg(feature = "recovery")]
pub use recovery::RedbCheckpointStore;
pub use store::MemoryStore;
pub use task::{DynTask, RetryMode, Task, TaskConfig, TaskObject, TaskResult};
pub use workflow::{JoinConfig, JoinStrategy, SplitResult, SplitTaskResult, StateEntry, Workflow};

#[cfg(feature = "tracing")]
pub use observer::TracingObserver;

#[cfg(feature = "scheduler")]
pub use scheduler::{BackoffPolicy, FlowInfo, RunningScheduler, Schedule, Scheduler, Status};

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

/// Attribute macro applied to the `CheckpointStore` trait definition and
/// `impl CheckpointStore` blocks.
///
/// See [`macro@task`] for the rewrite shape; this macro is functionally identical and
/// differs only in name.
pub use cano_macros::checkpoint_store;

/// Attribute macro applied to the `CompensatableTask` trait definition and
/// `impl CompensatableTask` blocks.
///
/// See [`macro@task`] for the rewrite shape; this macro is functionally identical and
/// differs only in name.
pub use cano_macros::compensatable_task;

/// Attribute macro applied to the `RouterTask` trait definition and
/// `impl RouterTask` blocks.
///
/// Two surface forms:
/// - `#[router_task] impl RouterTask<S> for T { ... }` — user writes the trait header; the
///   macro async-rewrites it AND emits a companion `impl Task<S> for T`.
/// - `#[router_task(state = S [, key = K])] impl T { async fn route(...) { ... } }` — user
///   writes only the inherent block; the macro builds the trait header and companion Task impl.
///
/// Because a blanket `impl<R: RouterTask<..>> Task<..> for R` would conflict (E0119) with the
/// existing `impl<N: Node<..>> Task<..> for N`, the companion `Task` impl is synthesised
/// per-use-site rather than as a blanket.
pub use cano_macros::router_task;

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
        CanoError, CanoResult, CheckpointRow, CheckpointStore, CircuitBreaker, CircuitPermit,
        CircuitPolicy, CircuitState, CompensatableTask, DefaultNodeResult, HealthStatus,
        JoinConfig, JoinStrategy, MemoryStore, Node, Resource, Resources, RetryMode, RouterTask,
        SplitResult, SplitTaskResult, StateEntry, Task, TaskConfig, TaskObject, TaskResult,
        Workflow, WorkflowObserver,
    };

    #[cfg(feature = "scheduler")]
    pub use crate::{BackoffPolicy, FlowInfo, RunningScheduler, Schedule, Scheduler, Status};

    #[cfg(feature = "tracing")]
    pub use crate::TracingObserver;

    #[cfg(feature = "recovery")]
    pub use crate::RedbCheckpointStore;

    // Re-export the cano async-trait macros for convenience.
    pub use crate::{checkpoint_store, compensatable_task, node, resource, router_task, task};

    // Re-export derive macros alongside their trait counterparts. Rust's separate
    // macro and type namespaces let the derive `Resource` coexist with the
    // `Resource` trait imported above.
    pub use crate::FromResources;
    pub use crate::Resource as ResourceDerive;
}
