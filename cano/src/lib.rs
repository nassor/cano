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
//!     async fn run(&self, res: &Resources) -> Result<Step, CanoError> {
//!         let config = res.get::<FetchConfig>()?;
//!         let board = res.get::<Blackboard>()?;
//!         // Produce `batch_size` items and put them on the blackboard for the next task.
//!         let data: Vec<u32> = (1..=(config.batch_size as u32)).collect();
//!         board.put("data", data);
//!         Ok(Step::Process)
//!     }
//! }
//!
//! #[task]
//! impl Task<Step> for ProcessTask {
//!     async fn run(&self, res: &Resources) -> Result<Step, CanoError> {
//!         let board = res.get::<Blackboard>()?;
//!         let data: Vec<u32> = board.get("data")?;
//!         board.put("sum", data.iter().sum::<u32>());
//!         Ok(Step::Done)
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let board = Blackboard::new();
//! let resources = Resources::new()
//!     .insert(FetchConfig { batch_size: 3 })
//!     .insert(board.clone());
//! let workflow = Workflow::new(resources)
//!     .register(Step::Fetch, FetchTask)
//!     .register(Step::Process, ProcessTask)
//!     .add_exit_state(Step::Done);
//!
//! let final_state = workflow.orchestrate(Step::Fetch).await?;
//! assert_eq!(final_state, Step::Done);
//!
//! // The sum of 1..=3 is 6.
//! assert_eq!(board.get::<u32>("sum")?, 6);
//! # Ok(())
//! # }
//! ```
//!
//! ## Resource-Free Workflows
//!
//! When tasks do not need shared resources, use [`Workflow::bare`] and write
//! `async fn run(&self)` — the `#[task]` macro adds the resources parameter:
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
//!     async fn run(&self) -> Result<Step, CanoError> {
//!         // Pure computation — no resource lookup needed.
//!         let result: u32 = (1..=100).sum();
//!         assert_eq!(result, 5050);
//!         Ok(Step::Done)
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
//! a [`Task`] handler for each state, and the engine manages transitions.
//!
//! ### Tasks
//!
//! [`Task`] is the single interface for processing logic — implement `run`. Several
//! specialized task models build on it:
//! - [`RouterTask`] trait: side-effect-free branching; reads resources, returns the next
//!   state without writing anything. Registered with [`Workflow::register`] — the
//!   engine dispatches it like a normal state but writes **no** `CheckpointRow`.
//! - [`PollTask`] trait: "wait-until" loop; `poll()` returns [`PollOutcome::Ready`] or
//!   [`PollOutcome::Pending { delay_ms }`](PollOutcome::Pending) on each call. Defaults to
//!   [`TaskConfig::new()`](task::TaskConfig::new) (no retries). Registered with
//!   [`Workflow::register`] — the engine applies `attempt_timeout` if set via `config()`.
//! - [`TimerTask`] trait: wait-then-transition; `wait()` returns a [`TimerOutcome`] (a
//!   [`Duration`](std::time::Duration) or monotonic [`Instant`](std::time::Instant)) and the
//!   engine sleeps **once** before `after_wait()` routes. Registered with
//!   [`Workflow::register`]. A resumed run re-runs `wait()`, so the delay restarts after a crash.
//! - [`BatchTask`] trait: fan-out over data items via `load`/`process_item`/`finish`.
//! - [`SteppedTask`] trait: resumable iterative work via `step()` with a serializable cursor.
//! - [`StreamTask`] trait: a genuine stream-processing model — consume an `impl Stream`
//!   continuously via `open`/`process_item`/`flush_window`/`on_close`, flush per
//!   [`StreamWindow`] window, run until the [`CancellationToken`] fires, and resume from a
//!   persisted cursor. Registered with [`Workflow::register`]; per-item errors are
//!   governed by [`StreamErrorPolicy`].
//! - [`SuspendTask`] trait: durable suspend / resume; `suspend()` parks the run at its state
//!   (`orchestrate` returns `Ok(state)`), and [`Workflow::resume_with`] later delivers a typed
//!   input to `resume()`, which returns the next state. Requires a checkpoint store; for
//!   human-in-the-loop steps such as an approval.
//!
//! Every model registers with the same [`Workflow::register`] call: the model macros
//! (`#[task::router]`, `#[task::poll]`, `#[task::timer]`, `#[task::batch]`,
//! `#[task::stepped]`, `#[task::stream]`, `#[task::suspend]`, `#[saga::task]`) implement
//! [`Registrable`](workflow::Registrable), which picks the engine path for the model.
//! They also emit a companion [`Task`] impl, so any model except [`SuspendTask`] can be a
//! [`Workflow::register_split`] branch (stepped and stream tasks then run in-memory loops
//! without cursor persistence).
//!
//! ### Parallel Execution (Split/Join)
//!
//! Run tasks concurrently with [`Workflow::register_split`] and join results using
//! strategies: `All`, `Any`, `Quorum(n)`, `Percentage(f64)`, `PartialResults(min)`,
//! or `PartialTimeout`.
//!
//! ### Blackboard
//!
//! [`Blackboard`] provides a thread-safe `Arc<RwLock<HashMap>>` for sharing typed data
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
//! be idempotent. The trait is backend-agnostic: implement it yourself, or use one of
//! the built-in [`store`]s: [`RedbStore`] (feature `store-redb`, on by default, an
//! embedded file), `PostgresStore` (feature `store-postgres`) or `NatsStore` (feature
//! `store-nats`, a JetStream Key/Value bucket). Workflows without a checkpoint store pay
//! nothing. See the `workflow_recovery` example.
//!
//! [`Workflow::with_workflow_version`] stamps a `u32` version onto every appended
//! [`CheckpointRow`] (default `0`); [`Workflow::resume_from`] then refuses to replay a
//! checkpoint whose stored version differs, returning
//! [`CanoError::WorkflowVersionMismatch`]. Bump it when the FSM shape changes between
//! deploys (added/removed states, cursor schema, compensation output layout) to avoid
//! silently resuming onto an incompatible workflow definition.
//!
//! ### Sagas & Compensation
//!
//! For steps that mutate external systems, implement [`CompensatableTask`] (its `run`
//! returns the next state *and* an `Output`; its `compensate` undoes the step given that
//! `Output`) and register it with [`Workflow::register`]. The engine
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
//! **RouterTask**: Single `route()` method — side-effect-free; reads resources, returns
//! the next state. Dispatched like a normal state but leaves no checkpoint row.
//!
//! **PollTask**: Single `poll()` method — returns [`PollOutcome::Ready(state)`](PollOutcome::Ready)
//! or [`PollOutcome::Pending { delay_ms }`](PollOutcome::Pending); the engine loops until `Ready`. Defaults
//! to `TaskConfig::new()` (no retries). Use `config().with_attempt_timeout(dur)` to cap
//! total wall-clock time; registered with [`Workflow::register`] like any other task.
//!
//! **TimerTask**: `wait()` returns a [`TimerOutcome`] and `after_wait()` returns the next state —
//! the engine schedules a single sleep between them (no polling loop). Defaults to
//! `TaskConfig::new()`; an `attempt_timeout` caps the wait. Registered with
//! [`Workflow::register`] like any other task.
//!
//! ### Timeouts
//!
//! Two layered budgets bound a run. [`TaskConfig::with_attempt_timeout`](task::TaskConfig::with_attempt_timeout)
//! caps each individual task attempt. [`Workflow::with_total_timeout`] sets a wall-clock
//! budget for the entire [`orchestrate`](Workflow::orchestrate) / [`resume_from`](Workflow::resume_from)
//! call; when it elapses the in-flight task is aborted, the saga compensation stack drains
//! against its own bounded budget (configurable via [`Workflow::with_compensation_timeout`]),
//! and the call returns [`CanoError::WorkflowTimeout`]. To stop a run on an external signal rather
//! than a deadline, use [cooperative cancellation](#cooperative-cancellation).
//!
//! ### Cooperative cancellation
//!
//! [`Workflow::orchestrate_with_token`] (and [`resume_from_with_token`](Workflow::resume_from_with_token))
//! take a [`CancellationToken`] (re-exported from `tokio_util`); calling
//! [`cancel`](CancellationToken::cancel) on it (or on any clone) aborts the in-flight cancellable
//! task at its next `.await`, drains the saga compensation stack, and returns
//! [`CanoError::Cancelled`]. Cancellation is *cooperative* (a task in tight synchronous work isn't
//! interrupted until it yields) and *saga-safe* (a [`CompensatableTask`] always runs to completion
//! so its rollback entry is recorded; the cancel is honoured at the next state boundary). The
//! compensation drain itself is uncancellable. [`Workflow::orchestrate`] runs without a token and
//! skips the cancellation `select!` entirely.
//!
//! ## Module Overview
//!
//! - [`mod@task`]: The [`Task`] trait — single `run()` method
//! - [`task::router`]: The [`RouterTask`] trait — side-effect-free branching via `route()`; registered with [`Workflow::register`]
//! - [`task::poll`]: The [`PollTask`] trait — wait-until loop via `poll()`; registered with [`Workflow::register`]
//! - [`task::timer`]: The [`TimerTask`] trait — wait-then-transition via `wait()`/`after_wait()`; registered with [`Workflow::register`]
//! - [`task::batch`]: The [`BatchTask`] trait — fan-out over data items via `load`/`process_item`/`finish`; registered with [`Workflow::register`]
//! - [`task::stepped`]: The [`SteppedTask`] trait — resumable iterative work via `step()` with a serializable cursor; registered with [`Workflow::register`] (persists the cursor when a checkpoint store is attached)
//! - [`task::stream`]: The [`StreamTask`] trait — a genuine stream-processing model: consume an `impl Stream` continuously, flush per [`StreamWindow`] window, run until the [`CancellationToken`] fires, and resume from a persisted cursor; registered with [`Workflow::register`]
//! - [`task::suspend`]: The [`SuspendTask`] trait — park a run until [`Workflow::resume_with`] delivers a typed input; registered with [`Workflow::register`]
//! - [`CancellationToken`] — cooperative cancellation for [`orchestrate_with_token`](Workflow::orchestrate_with_token)
//! - [`workflow`]: [`Workflow`] — FSM orchestration with Split/Join support
//! - `scheduler` (requires `scheduler` feature): `Scheduler` (builder) and `RunningScheduler` (live handle) — cron and interval scheduling
//! - [`mod@resource`]: [`Resource`] trait, [`Resources`] dictionary, and [`HealthStatus`] — lifecycle-aware resource management and health probes
//! - [`observer`]: [`WorkflowObserver`] — synchronous lifecycle/failure event hooks (and [`TracingObserver`], behind the `tracing` feature)
//! - `metrics` (requires `metrics` feature): a `MetricsObserver` plus low-cardinality
//!   counters / histograms / gauges for workflow, task, retry, split/join, circuit-breaker,
//!   scheduler, processing-loop, recovery and saga internals — call `cano::metrics::describe()`
//!   for the full list with help text and units
//! - [`checkpoint`]: [`CheckpointStore`] / [`CheckpointRow`], the append-only checkpoint log behind crash recovery, suspend and resume
//! - [`store`]: the storage backends that implement [`CheckpointStore`]: [`RedbStore`] (`store-redb`, on by default), `PostgresStore` (`store-postgres`) and `NatsStore` (`store-nats`)
//! - [`saga`]: [`CompensatableTask`] — pair a forward step with a compensating action; failures roll back via [`Workflow::register`]
//! - [`rate_limit`]: [`RateLimiter`] (token bucket) and [`WindowedRateLimiter`] (fixed window) throttles, both implementing [`Meter`] so they compose into a [`MultiRateLimiter`] that enforces several weighted tiers (e.g. a 5h + 7d + per-model limit) at once
//! - [`blackboard`]: [`Blackboard`] — a typed in-memory key-value map that implements [`Resource`]
//! - `testing` (requires `testing` feature): batteries-included test fixtures —
//!   a `RecordingObserver`, a public `InMemoryCheckpointStore`, a `panic_on_attempt` task factory and an `assert_compensation_ran`
//!   saga helper. Import explicitly with `use cano::testing::*;` — it is deliberately
//!   **not** in the [`prelude`] so production code never picks it up by accident.
//! - [`error`]: [`CanoError`] variants and the [`CanoResult`] alias
//!
//! ## Getting Started
//!
//! 1. Read the [documentation and setup guide](https://nassor.github.io/cano/)
//! 2. Run an example: `cargo run --example workflow_simple`
//! 3. Run benchmarks: `cargo bench --bench workflow_performance`

// Lets macro output use `::cano::…` paths inside this crate too.
extern crate self as cano;

pub mod blackboard;
pub mod checkpoint;
pub mod circuit;
pub mod error;
pub mod observer;
pub mod rate_limit;
pub mod resource;
pub mod saga;
pub mod store;
pub mod task;
pub mod workflow;

#[cfg(feature = "metrics")]
pub mod metrics;

#[cfg(feature = "scheduler")]
pub mod scheduler;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

// Core public API - simplified imports
pub use checkpoint::{CheckpointRow, CheckpointStore, RowKind};
pub use circuit::{CircuitBreaker, CircuitPolicy, CircuitState, Permit as CircuitPermit};
pub use error::{CanoError, CanoResult};
pub use observer::WorkflowObserver;
pub use rate_limit::{
    Meter, MeterStatus, MultiPermit, MultiRateLimiter, Permit as RateLimiterPermit, RateLimiter,
    RateLimiterPolicy, Reservation, Tier, WindowPermit, WindowPolicy, WindowedRateLimiter,
};
pub use resource::{HealthStatus, Resource, Resources};
pub use saga::CompensatableTask;
pub use task::batch::BatchTask;
pub use task::poll::{PollErrorPolicy, PollOutcome, PollTask};
pub use task::router::RouterTask;
pub use task::stepped::{StepOutcome, SteppedTask};
pub use task::stream::{CloseReason, StreamErrorPolicy, StreamTask, StreamWindow, WindowSignal};
pub use task::suspend::SuspendTask;
pub use task::timer::{TimerOutcome, TimerTask};
pub use tokio_util::sync::CancellationToken;

pub use blackboard::Blackboard;
#[cfg(feature = "store-nats")]
pub use store::NatsStore;
#[cfg(feature = "store-postgres")]
pub use store::PostgresStore;
#[cfg(feature = "store-redb")]
pub use store::RedbStore;
pub use task::{RetryMode, Task, TaskConfig};
pub use workflow::{JoinConfig, JoinStrategy, Workflow};

#[cfg(feature = "tracing")]
pub use observer::TracingObserver;

#[cfg(feature = "metrics")]
pub use observer::MetricsObserver;

#[cfg(feature = "scheduler")]
pub use scheduler::{
    BackoffPolicy, CasOutcome, FlowInfo, RunningScheduler, Scheduler, SchedulerStore, Status,
    VersionedRecord, shutdown_signal,
};

/// Attribute macro applied to `Task` trait definitions and `impl Task` blocks
/// to rewrite `async fn` methods into ones returning
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`.
///
/// The separate name makes `#[cano::task]` self-documenting at impl sites.
/// The implementation lives in the [`cano-macros`] sibling crate.
///
/// Processing-model macros are namespaced under `cano::task::` and `cano::saga::`:
/// - `#[cano::task::router]` — for `impl RouterTask` blocks
/// - `#[cano::task::poll]` — for `impl PollTask` blocks
/// - `#[cano::task::timer]` — for `impl TimerTask` blocks
/// - `#[cano::task::batch]` — for `impl BatchTask` blocks
/// - `#[cano::task::stepped]` — for `impl SteppedTask` blocks
/// - `#[cano::task::stream]` — for `impl StreamTask` blocks
/// - `#[cano::task::suspend]` — for `impl SuspendTask` blocks
/// - `#[cano::saga::task]` — for `impl CompensatableTask` blocks
///
/// [`cano-macros`]: https://docs.rs/cano-macros
pub use cano_macros::task;

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

/// Attribute macro applied to the `SchedulerStore` trait definition and
/// `impl SchedulerStore` blocks.
///
/// See [`macro@task`] for the rewrite shape; this macro is functionally identical and
/// differs only in name.
#[cfg(feature = "scheduler")]
pub use cano_macros::scheduler_store;

/// Derive macro that generates a `from_resources` associated function for a struct.
///
/// See [`FromResources`] for the full specification. Each
/// field must be `Arc<T>`; a plain field is looked up by type, a
/// `#[res("name")]` field by name.
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
        BatchTask, Blackboard, CancellationToken, CanoError, CanoResult, CheckpointRow,
        CheckpointStore, CircuitBreaker, CircuitPermit, CircuitPolicy, CircuitState, CloseReason,
        CompensatableTask, HealthStatus, JoinConfig, JoinStrategy, Meter, MeterStatus, MultiPermit,
        MultiRateLimiter, PollErrorPolicy, PollOutcome, PollTask, RateLimiter, RateLimiterPermit,
        RateLimiterPolicy, Reservation, Resource, Resources, RetryMode, RouterTask, RowKind,
        StepOutcome, SteppedTask, StreamErrorPolicy, StreamTask, StreamWindow, SuspendTask, Task,
        TaskConfig, Tier, TimerOutcome, TimerTask, WindowPermit, WindowPolicy, WindowSignal,
        WindowedRateLimiter, Workflow, WorkflowObserver,
    };

    #[cfg(feature = "store-redb")]
    pub use crate::RedbStore;

    #[cfg(feature = "scheduler")]
    pub use crate::{
        BackoffPolicy, CasOutcome, FlowInfo, RunningScheduler, Scheduler, SchedulerStore, Status,
        VersionedRecord, scheduler_store, shutdown_signal,
    };

    #[cfg(feature = "tracing")]
    pub use crate::TracingObserver;

    #[cfg(feature = "metrics")]
    pub use crate::MetricsObserver;

    // Attribute macros kept at the crate root: `#[task]`, `#[resource]`, `#[checkpoint_store]`.
    pub use crate::{checkpoint_store, resource, task};

    // Re-export the `task` and `saga` *modules* so that path-qualified macros resolve:
    // `#[task::router]`, `#[task::poll]`, `#[task::batch]`, `#[task::stepped]`,
    // `#[saga::task]` — all available via `use cano::prelude::*`.
    // (Modules and macros occupy different namespaces; `task` the module coexists with
    // `task` the macro imported above via `pub use crate::{..., task}`.)
    pub use crate::saga;
    // `task` module already in scope via the `pub use crate::{..., task}` line above.

    // Re-export derive macros alongside their trait counterparts. Rust's separate
    // macro and type namespaces let the derive `Resource` coexist with the
    // `Resource` trait imported above.
    pub use crate::FromResources;
    pub use crate::Resource as ResourceDerive;
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
            .map(|res| res.expect("dir entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                collect_files(&path, ext, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some(ext) {
                out.push(path);
            }
        }
    }

    fn parse_semver(version: &str) -> (u64, u64, u64) {
        let core = version.split(['-', '+']).next().unwrap_or(version);
        let mut parts = core.split('.');
        let major = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("invalid major version in {version:?}"));
        let minor = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("invalid minor version in {version:?}"));
        let patch = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("invalid patch version in {version:?}"));
        assert!(
            parts.next().is_none(),
            "unexpected extra version segment in {version:?}"
        );
        (major, minor, patch)
    }

    fn extract_quoted_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let mut rest = line;
        while let Some(idx) = rest.find(key) {
            let prev = idx.checked_sub(1).map(|i| rest.as_bytes()[i]);
            let before_ok =
                prev.is_none_or(|b| !b.is_ascii_alphanumeric() && b != b'_' && b != b'-');
            rest = &rest[idx + key.len()..];
            if !before_ok {
                continue;
            }
            let after_key = rest.trim_start();
            if let Some(after_eq) = after_key.strip_prefix('=') {
                let after_eq = after_eq.trim_start();
                if let Some(quoted) = after_eq.strip_prefix('"')
                    && let Some(end) = quoted.find('"')
                {
                    return Some(&quoted[..end]);
                }
            }
        }
        None
    }

    fn section_line<'a>(toml: &'a str, section: &str, key: &str) -> Option<&'a str> {
        let mut current = "";
        for raw in toml.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                current = line.trim_matches(|c| c == '[' || c == ']').trim();
                continue;
            }
            if current == section
                && let Some(after_key) = line.strip_prefix(key)
                && after_key.trim_start().starts_with('=')
            {
                return Some(line);
            }
        }
        None
    }

    #[test]
    fn version_sources_stay_in_sync() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(root) = manifest_dir.parent() else {
            return;
        };
        let root_cargo_path = root.join("Cargo.toml");
        let docs_config_path = root.join("docs/config.toml");
        if !root_cargo_path.exists() || !docs_config_path.exists() {
            // Standalone `.crate` package tests run outside the workspace checkout.
            return;
        }

        let pkg_version = env!("CARGO_PKG_VERSION");
        let (major, minor, patch) = parse_semver(pkg_version);
        let pkg_minor = format!("{major}.{minor}");

        // 1. docs/config.toml [extra].version matches CARGO_PKG_VERSION.
        let docs_config = fs::read_to_string(&docs_config_path).expect("read docs/config.toml");
        let docs_line = section_line(&docs_config, "extra", "version")
            .expect("docs/config.toml must define [extra].version");
        let docs_version = extract_quoted_field(docs_line, "version")
            .expect("docs/config.toml [extra].version must be a quoted string");
        assert_eq!(
            docs_version, pkg_version,
            "docs/config.toml [extra].version must match CARGO_PKG_VERSION"
        );

        // 2. Root Cargo.toml [workspace.dependencies].cano-macros matches major.minor
        //    (or an exact major.minor.m_patch pin with m_patch <= patch), and
        //    cano/Cargo.toml inherits it via `.workspace = true`.
        let root_cargo = fs::read_to_string(&root_cargo_path).expect("read root Cargo.toml");
        let macros_line = section_line(&root_cargo, "workspace.dependencies", "cano-macros")
            .expect("root Cargo.toml must define [workspace.dependencies].cano-macros");
        let macros_ver = extract_quoted_field(macros_line, "version")
            .expect("cano-macros workspace dependency must specify a version")
            .trim_start_matches('=');
        if macros_ver != pkg_minor {
            let (m_major, m_minor, m_patch) = parse_semver(macros_ver);
            assert!(
                m_major == major && m_minor == minor && m_patch <= patch,
                "cano-macros version {macros_ver:?} does not match cano {pkg_version:?}"
            );
        }

        let crate_cargo =
            fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("read cano/Cargo.toml");
        assert!(
            crate_cargo
                .lines()
                .map(str::trim)
                .any(|l| !l.starts_with('#') && l == "cano-macros.workspace = true"),
            "cano/Cargo.toml must declare `cano-macros.workspace = true`"
        );
        assert!(
            !crate_cargo
                .lines()
                .map(str::trim)
                .any(|l| !l.starts_with('#') && l.starts_with("cano-macros = {")),
            "cano/Cargo.toml must not pin cano-macros inline; use root Cargo.toml"
        );

        // 3. No hardcoded cano version snippets in cano/src/**/*.rs or README.md.
        let forbidden = [
            concat!("cano", " = { version ="),
            concat!("cano", " = \"0."),
            concat!("cano", " = \"1."),
        ];
        let mut scan_files = Vec::new();
        collect_files(&manifest_dir.join("src"), "rs", &mut scan_files);
        let readme = root.join("README.md");
        if readme.exists() {
            scan_files.push(readme);
        }
        for path in scan_files {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            for (idx, line) in text.lines().enumerate() {
                for pat in forbidden {
                    assert!(
                        !line.contains(pat),
                        "{}:{} contains hardcoded version snippet ({pat})",
                        path.display(),
                        idx + 1
                    );
                }
            }
        }

        // 4. Every `cano =` dependency snippet in docs/content/**/*.md uses a template token.
        let mut md_files = Vec::new();
        collect_files(&root.join("docs/content"), "md", &mut md_files);
        for path in md_files {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            for (idx, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                let stripped = trimmed.strip_prefix("# ").unwrap_or(trimmed).trim_start();
                if stripped.starts_with("cano =") {
                    if let Some(ver) = extract_quoted_field(stripped, "version") {
                        assert!(
                            ver == "@CANO_MINOR@" || ver == "@CANO_VERSION@",
                            "{}:{} must use @CANO_MINOR@ or @CANO_VERSION@, found {ver:?}",
                            path.display(),
                            idx + 1
                        );
                    } else if let Some(ver) = extract_quoted_field(stripped, "cano") {
                        assert!(
                            ver == "@CANO_MINOR@" || ver == "@CANO_VERSION@",
                            "{}:{} must use @CANO_MINOR@ or @CANO_VERSION@, found {ver:?}",
                            path.display(),
                            idx + 1
                        );
                    }
                }
            }
        }

        let docs_index =
            fs::read_to_string(root.join("docs/content/_index.md")).expect("read docs index");
        assert!(
            docs_index.contains("@CANO_MINOR@"),
            "docs/content/_index.md must use @CANO_MINOR@"
        );
    }
}
