<div align="center">
  <img src="https://raw.githubusercontent.com/nassor/cano/main/docs/static/logo.png" alt="Cano Logo" width="200">
  <h1>Cano: Type-Safe Async Workflow Engine</h1>

[![Crates.io](https://img.shields.io/crates/v/cano.svg)](https://crates.io/crates/cano)
[![Documentation](https://docs.rs/cano/badge.svg)](https://docs.rs/cano)
[![Website](https://img.shields.io/badge/website-nassor.github.io%2Fcano-blue)](https://nassor.github.io/cano/)
[![Downloads](https://img.shields.io/crates/d/cano.svg)](https://crates.io/crates/cano)
[![License](https://img.shields.io/crates/l/cano.svg)](https://github.com/nassor/cano/blob/main/LICENSE)
[![CI](https://github.com/nassor/cano/workflows/CI/badge.svg)](https://github.com/nassor/cano/actions)
[![Rust Version](https://img.shields.io/badge/rust-1.89%2B-blue.svg)](https://www.rust-lang.org)

<em>**Orchestrate complex async processes with finite state machines, parallel execution, and built-in scheduling.**</em>

<em>Cano is still far from a 1.0 release. The API is subject to changes and may include breaking changes.</em>

</div>

# Overview

Cano is a high-performance orchestration engine designed for building resilient, self-healing systems in Rust. Unlike simple task queues, Cano uses **Finite State Machines (FSM)** to define strict, type-safe transitions between processing steps.

It excels at managing complex lifecycles where state transitions matter:
*   **Data Pipelines**: ETL jobs with parallel processing (Split/Join) and aggregation.
*   **AI Agents**: Multi-step inference chains with shared context and memory.
*   **Background Systems**: Scheduled maintenance, periodic reporting, and distributed cron jobs.

The engine is built on three core concepts: **Tasks** for logic, **Workflows** for state transitions, and **Schedulers** for timing.

## Features

- **Type-Safe State Machines**: Enum-driven transitions with compile-time guarantees.
- **Multiple Processing Models**: `Task` for general-purpose work, plus `RouterTask`, `PollTask`, `BatchTask`, and `SteppedTask` for specialized shapes — mixed freely in one workflow.
- **Parallel Execution (Split/Join)**: Run tasks concurrently and join results with strategies like `All`, `Any`, `Quorum`, or `PartialResults`.
- **Robust Retry Logic**: Configurable strategies including exponential backoff with jitter and per-attempt timeouts.
- **Circuit Breaker**: Shared `CircuitBreaker` short-circuits calls to failing dependencies before the retry loop, with configurable failure threshold, cool-down, and half-open probing.
- **Built-in Scheduling**: Cron-based, interval, and manual triggers for background jobs.
- **Crash Recovery**: Pluggable `CheckpointStore` records every FSM state entry; `Workflow::resume_from` rehydrates a crashed run and continues. Ships with an embedded, ACID `RedbCheckpointStore` behind the `recovery` feature.
- **Sagas / Compensation**: Pair a forward step with a `compensate` action via `CompensatableTask` + `register_with_compensation`; if a later step fails, the engine rolls back the work already done in reverse order (and replays the rollback across a crash when checkpointing is on).
- **Observability**: Optional `tracing` (spans + events, plus `TracingObserver`) and `metrics` (a `MetricsObserver` plus low-cardinality counters / histograms / gauges via the [`metrics`](https://docs.rs/metrics) facade) features for deep insight into workflow, task, retry, split/join, circuit-breaker, scheduler, processing-loop, recovery and saga internals; plus synchronous `WorkflowObserver` hooks for lifecycle/failure events and `Resource::health()` probes (`Resources::check_all_health`).
- **Performance-Focused**: Minimizes heap allocations by leveraging stack-based objects wherever possible, giving you control over where allocations occur.

Everything above is opt-in and zero-cost when unused. For how the *resilient, self-healing* tagline maps to concrete primitives — retries, timeouts, circuit breakers, bulkheads, panic safety, checkpoint+resume, sagas, observers, health probes — see the [Resilience](https://nassor.github.io/cano/resilience/), [Recovery](https://nassor.github.io/cano/recovery/) and [Saga](https://nassor.github.io/cano/saga/) guides.

## Simple Example: Parallel Processing

Here is a real-world example showing how to split execution into parallel tasks and join them back together.

```mermaid
graph TD
    Start([Start]) --> Split{Split}
    Split -->|Source 1| T1[FetchSourceTask 1]
    Split -->|Source 2| T2[FetchSourceTask 2]
    Split -->|Source 3| T3[FetchSourceTask 3]
    T1 --> Join{Join All}
    T2 --> Join
    T3 --> Join
    Join --> Complete([Complete])
```

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FlowState {
    Start,
    Complete,
}

// A task that simulates fetching data from a source.
#[derive(Clone)]
struct FetchSourceTask {
    source_id: u32,
}

#[task]
impl Task<FlowState> for FetchSourceTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<FlowState>, CanoError> {
        // Look up the shared store from the workflow's resources.
        let store = res.get::<MemoryStore, str>("store")?;

        // Simulate async work.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Store the per-source result for downstream aggregation.
        let key = format!("source_{}", self.source_id);
        store.put(&key, format!("data_from_{}", self.source_id))?;

        Ok(TaskResult::Single(FlowState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // 1. Register shared resources (the store is one resource among many).
    let resources = Resources::new().insert("store", MemoryStore::new());

    // 2. Define parallel tasks.
    let sources = vec![
        FetchSourceTask { source_id: 1 },
        FetchSourceTask { source_id: 2 },
        FetchSourceTask { source_id: 3 },
    ];

    // 3. Configure the join strategy.
    // Wait for ALL tasks to complete successfully before moving to Complete.
    let join_config = JoinConfig::new(JoinStrategy::All, FlowState::Complete)
        .with_timeout(Duration::from_secs(5));

    // 4. Build the workflow: Start -> Split into parallel tasks -> Complete.
    let workflow = Workflow::new(resources)
        .register_split(FlowState::Start, sources, join_config)
        .add_exit_state(FlowState::Complete);

    // 5. Run.
    let result = workflow.orchestrate(FlowState::Start).await?;
    println!("Workflow finished: {:?}", result);

    Ok(())
}
```

## Crash Recovery & Resume

Attach a `CheckpointStore` and the workflow records one `CheckpointRow` per state entered — *before* that state's task runs. After a crash, `resume_from(workflow_id)` reloads the run and re-enters the FSM at the last checkpointed state. The resumed state's task **re-runs**, so tasks at and after the resume point must be idempotent.

```rust
use cano::prelude::*;
use cano::RedbCheckpointStore;            // behind the `recovery` feature
use std::sync::Arc;

# #[derive(Debug, Clone, PartialEq, Eq, Hash)] enum Step { Start, Work, Done }
# #[derive(Clone)] struct StartTask; #[derive(Clone)] struct WorkTask;
# #[task(state = Step)] impl StartTask { async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> { Ok(TaskResult::Single(Step::Work)) } }
# #[task(state = Step)] impl WorkTask { async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> { Ok(TaskResult::Single(Step::Done)) } }
# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let store = Arc::new(RedbCheckpointStore::new("workflow.redb")?);
let workflow = Workflow::bare()
    .register(Step::Start, StartTask)
    .register(Step::Work, WorkTask)
    .add_exit_state(Step::Done)
    .with_checkpoint_store(store)
    .with_workflow_id("run-42");

// First time: run forward (checkpoints are written along the way).
let _ = workflow.orchestrate(Step::Start).await;

// After a crash: pick up where it left off.
let final_state = workflow.resume_from("run-42").await?;
# let _ = final_state; Ok(())
# }
```

The `CheckpointStore` trait is backend-agnostic (no feature flag) — implement it over Postgres, an HTTP service, anything; `RedbCheckpointStore` is just the batteries-included default. `cargo run --example workflow_recovery --features recovery` walks through a full crash-and-resume cycle. See the [Recovery guide](https://nassor.github.io/cano/recovery/).

## Sagas / Compensation

For steps that mutate external systems, write a `#[saga::task(state = …)]` — like a plain `#[task]`, but its `run` returns the next state *and* an `Output`, and it has a `compensate` that undoes the step given that `Output` — and register it with `register_with_compensation`. The engine keeps a per-run compensation stack; if a later state fails, it drains the stack in reverse and runs each `compensate`. A clean rollback returns the original error (and clears the checkpoint log if one is attached); a failed `compensate` produces `CanoError::CompensationFailed` with the original error plus every compensation error.

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

# #[derive(Debug, Clone, PartialEq, Eq, Hash)] enum Step { Reserve, Ship, Done }
#[derive(Serialize, Deserialize)]
struct Reservation { sku: String, qty: u32 }

struct ReserveInventory;

#[saga::task(state = Step)]
impl ReserveInventory {
    type Output = Reservation;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        // ... reserve the stock ...
        Ok((TaskResult::Single(Step::Ship), Reservation { sku: "WIDGET-7".into(), qty: 3 }))
    }
    async fn compensate(&self, _res: &Resources, output: Reservation) -> Result<(), CanoError> {
        // ... release output.qty of output.sku — must be idempotent ...
        let _ = output; Ok(())
    }
}
```

With a checkpoint store attached, outputs are persisted and `resume_from` rehydrates the stack — so a failure after a crash still rolls back work done before it. Compensation is for single-task states only. `cargo run --example saga_payment` walks through a reserve-charge-ship rollback. See the [Saga guide](https://nassor.github.io/cano/saga/).

## Documentation

For complete documentation, examples, and guides, please visit our website:

👉 **[https://nassor.github.io/cano/](https://nassor.github.io/cano/)**

You can also find:
- **[API Documentation](https://docs.rs/cano)** on docs.rs
- **[Examples Directory](./examples/)** in the repository

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## AI Disclosure

The primary developer of this repository uses AI coding assistants while
working on Cano. At the time of writing, the assistants in regular use are:

- **Claude Code** (Anthropic API), and
- **Qwen** and **DeepSeek** models running locally.

All AI-assisted output is reviewed, edited, tested, and submitted by a human
developer who is fully responsible for the resulting code. AI tools are
treated as accelerators, not authors. See
[AI_USAGE_POLICY.md](AI_USAGE_POLICY.md) for the full policy that contributors
are expected to follow when using AI assistants on this project.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
