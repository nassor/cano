<div align="center">
  <img src="https://raw.githubusercontent.com/nassor/cano/main/docs/static/logo.png" alt="Cano Logo" width="200">
  <h1>Cano: Type-Safe Async Workflow Engine</h1>

[![Crates.io](https://img.shields.io/crates/v/cano.svg)](https://crates.io/crates/cano)
[![Documentation](https://docs.rs/cano/badge.svg)](https://docs.rs/cano)
[![Website](https://img.shields.io/badge/website-nassor.github.io%2Fcano-blue)](https://nassor.github.io/cano/)
[![Downloads](https://img.shields.io/crates/d/cano.svg)](https://crates.io/crates/cano)
[![License](https://img.shields.io/crates/l/cano.svg)](https://github.com/nassor/cano/blob/main/LICENSE)
[![CI](https://github.com/nassor/cano/workflows/CI/badge.svg)](https://github.com/nassor/cano/actions)
[![Rust Version](https://img.shields.io/badge/rust-1.98%2B-blue.svg)](https://www.rust-lang.org)

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
- **Multiple Processing Models**: `Task` for general-purpose work, plus `RouterTask`, `PollTask`, `TimerTask`, `BatchTask`, `SteppedTask`, and `StreamTask` for specialized shapes — mixed freely in one workflow.
- **Resource Dependency Injection**: Typed, lifecycle-managed `Resources` dictionary with `setup`/`teardown`/`health` hooks, looked up by key and type, plus `#[derive(FromResources)]` for ergonomic wiring.
- **Parallel Execution (Split/Join)**: Run tasks concurrently and join results with strategies like `All`, `Any`, `Quorum`, or `PartialResults`, with an optional bulkhead to cap concurrency.
- **Robust Retry Logic**: Configurable strategies including exponential backoff with jitter and per-attempt timeouts.
- **Circuit Breaker**: Shared `CircuitBreaker` short-circuits calls to failing dependencies before the retry loop, with configurable failure threshold, cool-down, and half-open probing.
- **Rate Limiting**: Token-bucket (`RateLimiter`) and fixed-window (`WindowedRateLimiter`) throttles that compose into a `MultiRateLimiter` enforcing several weighted tiers at once.
- **Built-in Scheduling**: Cron-based, interval, and manual triggers for background jobs.
- **Crash Recovery**: Pluggable `CheckpointStore` records every FSM state entry; `Workflow::resume_from` rehydrates a crashed run and continues. Ships with an embedded, ACID `RedbCheckpointStore` behind the `recovery` feature.
- **Sagas / Compensation**: Pair a forward step with a `compensate` action via `CompensatableTask` + `register_with_compensation`; if a later step fails, the engine rolls back the work already done in reverse order (and replays the rollback across a crash when checkpointing is on).
- **Observability**: Optional `tracing` (spans + events, plus `TracingObserver`) and `metrics` (a `MetricsObserver` plus low-cardinality counters / histograms / gauges via the [`metrics`](https://docs.rs/metrics) facade) features for deep insight into workflow, task, retry, split/join, circuit-breaker, scheduler, processing-loop, recovery and saga internals; plus synchronous `WorkflowObserver` hooks for lifecycle/failure events and `Resource::health()` probes (`Resources::check_all_health`).
- **Performance-Focused**: Minimizes heap allocations by leveraging stack-based objects wherever possible, giving you control over where allocations occur.

For how the *resilient, self-healing* tagline maps to concrete primitives — retries, timeouts, circuit breakers, rate limiters, bulkheads, panic safety, checkpoint+resume, sagas, observers, health probes — see the [Resilience](https://nassor.github.io/cano/resilience/), [Recovery](https://nassor.github.io/cano/recovery/) and [Saga](https://nassor.github.io/cano/saga/) guides.

## Simple Example: Parallel Processing

Here is a real-world example: fan a price lookup out across four exchanges, each returning a
batch of quotes, tolerate one of them being down, pool every quote that landed into a single
reference price, then stream a live tick feed against it. It combines **Split/Join** with a
**quorum** join strategy, **per-task retries**, **resource injection**, and a windowed
**`StreamTask`**.

<div align="center">
  <img src="https://raw.githubusercontent.com/nassor/cano/main/docs/static/split-join-quorum.svg" alt="Four FetchPriceTask instances run in parallel from Start, each returning a batch of quotes: alpha sends 2 averaging 101.28, beta 4 averaging 101.43, gamma 6 averaging 101.00, and delta retries twice and still fails. JoinStrategy::Quorum(3) proceeds on the three that reported. Aggregate pools all 12 quotes into a single quote-weighted reference of 101.19, so the deeper books pull it further than the thin one. A StreamTask then consumes the trade-tick feed, flushing one window of three ticks at a time and looping until the feed is exhausted, at which point on_close moves the workflow to Complete. Fewer than three reporting exchanges would instead return Err(CanoError::Workflow)." width="760">
</div>

```rust
use cano::prelude::*;
use futures_util::{Stream, stream}; // StreamTask sources are plain `futures` streams
use std::pin::Pin;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FlowState {
    Start,
    Aggregate,
    Monitor,
    Complete,
}

// One row per exchange: its name, the batch of quotes it returns, and whether it's
// simulated as unreachable (e.g. down for maintenance). Exchanges return different
// numbers of quotes, and every quote counts toward the reference price — so a deeper
// book pulls it further than a thin one.
const EXCHANGES: &[(&str, &[f64], bool)] = &[
    ("alpha", &[101.20, 101.36], false),
    ("beta", &[101.45, 101.30, 101.55, 101.42], false),
    ("gamma", &[100.95, 101.10, 100.85, 101.05, 100.90, 101.15], false),
    ("delta", &[], true), // unreachable — every attempt fails
];

// Fetches a batch of quotes from one exchange. Real networks are flaky, so each task
// carries its own retry budget: exponential backoff, up to 2 retries.
#[derive(Clone)]
struct FetchPriceTask {
    exchange: &'static str,
    quotes: &'static [f64],
    unreachable: bool,
}

#[task(state = FlowState)]
impl FetchPriceTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::new().with_exponential_retry(2)
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<FlowState>, CanoError> {
        // Look up the shared store from the workflow's resources.
        let store = res.get::<MemoryStore, _>("store")?;

        // Simulate the network round-trip.
        tokio::time::sleep(Duration::from_millis(20)).await;

        if self.unreachable {
            // Retries are applied by the engine; after they're exhausted this task
            // is simply one of the split's failures.
            return Err(CanoError::task_execution(format!(
                "{}: connection refused",
                self.exchange
            )));
        }

        store.put(&format!("quotes_{}", self.exchange), self.quotes.to_vec())?;
        Ok(TaskResult::Single(FlowState::Aggregate))
    }
}

// Runs once the join is satisfied: pools every quote that landed. Flattening the batches
// means the reference is quote-weighted, not exchange-weighted.
struct AggregatePricesTask {
    exchanges: Vec<&'static str>,
}

#[task(state = FlowState)]
impl AggregatePricesTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<FlowState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;

        let batches: Vec<Vec<f64>> = self
            .exchanges
            .iter()
            .filter_map(|exchange| store.get::<Vec<f64>>(&format!("quotes_{exchange}")).ok())
            .collect();
        let reporting = batches.len();
        let total = self.exchanges.len();

        let quotes: Vec<f64> = batches.into_iter().flatten().collect();
        let reference = quotes.iter().sum::<f64>() / quotes.len() as f64;
        store.put("average_price", reference)?;
        println!(
            "Reference {reference:.2} from {} quotes across {reporting}/{total} exchanges",
            quotes.len()
        );

        Ok(TaskResult::Single(FlowState::Monitor))
    }
}

// A bounded feed of trade ticks arriving after the reference price is known.
const TICKS: &[f64] = &[101.30, 101.05, 103.90, 101.10, 100.80, 98.20, 101.35];

// Flag a tick once it strays this far from the aggregated reference price.
const ALERT_PCT: f64 = 1.0;

#[derive(Clone, Copy)]
struct Tick {
    seq: u64,
    price: f64,
}

// Consumes the tick feed continuously, emitting once per window instead of once at the
// end. The feed is bounded here so the example terminates; a real source (Kafka, a
// WebSocket) would run until the CancellationToken fires.
struct MonitorTicksTask;

#[task::stream(state = FlowState)]
impl MonitorTicksTask {
    fn window(&self) -> StreamWindow {
        StreamWindow::Count(3)
    }

    // `cursor` is the last committed position, so a resumed run skips what it already saw.
    async fn open(
        &self,
        _res: &Resources,
        cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = Tick> + Send>>, CanoError> {
        let resume_at = cursor.map_or(0, |seq| seq + 1);
        let ticks: Vec<Tick> = TICKS
            .iter()
            .enumerate()
            .map(|(i, &price)| Tick { seq: i as u64, price })
            .filter(|tick| tick.seq >= resume_at)
            .collect();
        let feed: Pin<Box<dyn Stream<Item = Tick> + Send>> = Box::pin(stream::iter(ticks));
        Ok(feed)
    }

    // Per-item work: how far this tick strays from the reference the split produced.
    // The returned cursor is the position to commit once this item's window flushes.
    async fn process_item(&self, res: &Resources, item: Tick) -> Result<(f64, u64), CanoError> {
        let reference: f64 = res.get::<MemoryStore, _>("store")?.get("average_price")?;
        Ok(((item.price - reference) / reference * 100.0, item.seq))
    }

    // Per-window emission: downstream sees progress before the feed ends.
    async fn flush_window(
        &self,
        res: &Resources,
        deviations: Vec<f64>,
    ) -> Result<WindowSignal<FlowState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let outliers = deviations.iter().filter(|d| d.abs() >= ALERT_PCT).count();
        let worst = deviations.iter().fold(0.0f64, |acc, d| acc.max(d.abs()));
        println!(
            "  window of {}: worst {worst:.2}% — {outliers} outlier(s)",
            deviations.len()
        );

        let seen: usize = store.get("outliers").unwrap_or(0);
        store.put("outliers", seen + outliers)?;
        Ok(WindowSignal::Continue)
    }

    // The feed ran dry: the partial window is flushed, then this picks the next state.
    async fn on_close(
        &self,
        res: &Resources,
        reason: CloseReason,
    ) -> Result<TaskResult<FlowState>, CanoError> {
        let total: usize = res.get::<MemoryStore, _>("store")?.get("outliers").unwrap_or(0);
        println!("Tick feed closed ({reason:?}): {total} outlier(s) beyond {ALERT_PCT:.0}%");
        Ok(TaskResult::Single(FlowState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // 1. Register shared resources (the store is one resource among many).
    let resources = Resources::new().insert("store", MemoryStore::new());

    // 2. Build one fetch task per exchange, each carrying its own retry budget.
    let exchanges: Vec<&'static str> = EXCHANGES.iter().map(|&(name, _, _)| name).collect();
    let fetchers: Vec<FetchPriceTask> = EXCHANGES
        .iter()
        .map(|&(exchange, quotes, unreachable)| FetchPriceTask {
            exchange,
            quotes,
            unreachable,
        })
        .collect();

    // 3. Configure the join strategy.
    // Tolerate one exchange being down: proceed once 3 of the 4 report quotes. Fewer
    // than 3 successes returns `Err(CanoError::Workflow(..))` from `orchestrate` rather
    // than advancing on incomplete data.
    let join_config = JoinConfig::new(JoinStrategy::Quorum(3), FlowState::Aggregate)
        .with_timeout(Duration::from_secs(5));

    // 4. Build the workflow: Start -> Split fetches -> Aggregate -> Monitor -> Complete.
    //    `register_stream` is the durable, cancellable path; plain `register` would run a
    //    non-persistent in-memory companion loop instead.
    let workflow = Workflow::new(resources)
        .register_split(FlowState::Start, fetchers, join_config)
        .register(FlowState::Aggregate, AggregatePricesTask { exchanges })
        .register_stream(FlowState::Monitor, MonitorTicksTask)
        .add_exit_state(FlowState::Complete);

    // 5. Run.
    let result = workflow
        .orchestrate(FlowState::Start, CancellationToken::disabled())
        .await?;
    println!("Workflow finished: {result:?}");

    Ok(())
}
```

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
