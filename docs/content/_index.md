+++
title = "Cano - Type-safe async workflow engine for Rust"
description = "Cano is a high-performance orchestration engine designed for building resilient, self-healing systems in Rust. Unlike simple task queues, Cano uses Finite State Machines (FSM) to define strict, type-safe transitions between processing steps."
template = "index.html"
+++

<section class="hero">
<h1 class="animate-in">Cano</h1>
<p class="subtitle animate-in">Type-safe async workflow engine with built-in scheduling, retry logic, and state machine semantics.</p>

<p class="prerelease-notice animate-in"><em>Cano is still far from a 1.0 release; the API is subject to change and may include breaking changes.</em></p>

<div class="badges animate-in">
<a href="https://crates.io/crates/cano" title="Crates.io">
<img src="https://img.shields.io/crates/v/cano.svg" alt="Crates.io">
</a>
<a href="https://docs.rs/cano" title="API Documentation">
<img src="https://docs.rs/cano/badge.svg" alt="Documentation">
</a>
<a href="https://crates.io/crates/cano" title="Download Statistics">
<img src="https://img.shields.io/crates/d/cano.svg" alt="Downloads">
</a>
<a href="https://github.com/nassor/cano/blob/main/LICENSE" title="MIT License">
<img src="https://img.shields.io/crates/l/cano.svg" alt="License">
</a>
</div>

<p class="animate-in">
Cano is a high-performance orchestration engine designed for building resilient, self-healing systems in Rust.
Unlike simple task queues, Cano uses <strong>Finite State Machines (FSM)</strong> to define strict, type-safe transitions between processing steps.
</p>

<p class="animate-in">
It excels at managing complex lifecycles where state transitions matter:
</p>
<ul class="animate-in">
<li><strong>Data Pipelines</strong>: ETL jobs with parallel processing (Split/Join) and aggregation.</li>
<li><strong>AI Agents</strong>: Multi-step inference chains with shared context and memory.</li>
<li><strong>Background Systems</strong>: Scheduled maintenance, periodic reporting, and distributed cron jobs.</li>
</ul>
</section>

<h2>Features</h2>
<div class="feature-grid">
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9881;</div>
<h3>Tasks & Nodes</h3>
<p>Single <code>Task</code> trait for simple logic, or <code>Node</code> trait for structured three-phase lifecycle.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#9670;</div>
<h3>State Machines</h3>
<p>Type-safe enum-driven state transitions with compile-time checking.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon accent" aria-hidden="true">&#8635;</div>
<h3>Retry Strategies</h3>
<p>Fixed delays, exponential backoff with jitter, and custom strategies.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9202;</div>
<h3>Scheduling</h3>
<p>Built-in scheduler with intervals, cron schedules, and manual triggers.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#9881;</div>
<h3>Concurrency</h3>
<p>Execute multiple workflow instances in parallel with timeout strategies.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#128190;</div>
<h3>Crash Recovery</h3>
<p>Pluggable <code>CheckpointStore</code> records every state entry; <code>resume_from</code> rehydrates a crashed run. Embedded, ACID <code>RedbCheckpointStore</code> behind the <code>recovery</code> feature.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#8634;</div>
<h3>Sagas / Compensation</h3>
<p>Pair a forward step with a <code>compensate</code> action; a later failure rolls back the work already done, in reverse — and replays the rollback across a crash.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon accent" aria-hidden="true">&#9673;</div>
<h3>Observability</h3>
<p>Built-in <code>tracing</code> spans, plus <code>WorkflowObserver</code> hooks and resource health probes.</p>
</div>
</div>

<h2>Resilient, Self-Healing — What the Tagline Maps To</h2>
<p>
Every word in <em>"high-performance orchestration engine for building resilient, self-healing systems"</em>
is a concrete primitive. All of them are <strong>opt-in and zero-cost when unused</strong> — the FSM
dispatch hot path stays allocation-light whether or not you wire any of this up.
</p>

<table>
<thead><tr><th>Tagline word</th><th>Primitive</th><th>Guide</th></tr></thead>
<tbody>
<tr><td rowspan="6"><strong>resilient</strong><br>recover from transient faults</td>
    <td><code>RetryMode</code> — fixed / exponential-backoff-with-jitter, via <code>TaskConfig</code></td>
    <td rowspan="5"><a href="resilience/">Resilience</a></td></tr>
<tr><td>Per-attempt timeout (<code>TaskConfig::with_attempt_timeout</code> → <code>CanoError::Timeout</code>, retried)</td></tr>
<tr><td><code>CircuitBreaker</code> — short-circuits a failing dependency before the retry loop (closed → open → half-open)</td></tr>
<tr><td>Split <strong>bulkhead</strong> — cap concurrent parallel tasks (<code>JoinConfig::with_bulkhead</code>)</td></tr>
<tr><td>Panic safety — a panicking task body becomes <code>CanoError::TaskExecution</code>, never unwinds through the engine</td></tr>
<tr><td>Scheduler backoff &amp; trip (<code>BackoffPolicy</code>, <code>Status::Backoff</code> / <code>Status::Tripped</code>, <code>reset_flow</code>)</td>
    <td><a href="scheduler/">Scheduler</a></td></tr>
<tr><td rowspan="4"><strong>self-healing</strong><br>repair / roll back / report on its own state</td>
    <td>Checkpoint + resume: <code>CheckpointStore</code> records each state entry; <code>resume_from</code> rehydrates a crashed run (built-in <code>RedbCheckpointStore</code>)</td>
    <td><a href="recovery/">Recovery</a></td></tr>
<tr><td>Sagas / compensation: <code>CompensatableTask</code> + <code>register_with_compensation</code> — a failure drains the compensation stack in reverse</td>
    <td><a href="saga/">Saga</a></td></tr>
<tr><td><code>WorkflowObserver</code> — synchronous lifecycle / failure / checkpoint / resume hooks (and the built-in <code>TracingObserver</code>)</td>
    <td rowspan="2"><a href="observers/">Observers</a></td></tr>
<tr><td>Resource health probes: <code>Resource::health()</code>, <code>Resources::check_all_health()</code> / <code>aggregate_health()</code></td></tr>
<tr><td><strong>high-performance</strong></td>
    <td>Allocation-light FSM dispatch; every primitive above is a <code>dyn</code>-erased / <code>Option</code> check that's skipped when not configured — see <code>cargo bench --bench workflow_performance</code></td>
    <td>—</td></tr>
</tbody>
</table>

<h2>Getting Started</h2>
<p>Add Cano to your <code>Cargo.toml</code>:</p>

<div class="getting-started-code">

```toml
[dependencies]
cano = { version = "0.11", features = ["all"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

```

</div>

<p>Cano runs on the Tokio runtime, so <code>tokio</code> is a required direct dependency — you launch the runtime via <code>#[tokio::main]</code> or <code>tokio::runtime::Builder</code>. The two features above are the minimum to do that; add <code>"time"</code>, <code>"sync"</code>, etc. only if your own code calls into them. Use <code>"full"</code> if you prefer convenience over compile time.</p>

<h3>Basic Example</h3>
<div class="getting-started-code">

```rust
use cano::prelude::*;

// Define workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Start,
    Process,
    Complete,
}

// #[derive(Resource)] generates a no-op Resource impl for stateless config structs
#[derive(Resource)]
struct AppConfig { batch_size: usize }

// #[task] handles the async-trait rewrite — no external async-trait crate needed
#[derive(Clone)]
struct SimpleTask;

#[task(state = WorkflowState)]
impl SimpleTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<WorkflowState>, CanoError> {
        let config = res.get::<AppConfig, _>("config")?;
        println!("Processing task (batch_size={})...", config.batch_size);
        Ok(TaskResult::Single(WorkflowState::Process))
    }
}

#[derive(Clone)]
struct DoneTask;

#[task(state = WorkflowState)]
impl DoneTask {
    async fn run_bare(&self) -> Result<TaskResult<WorkflowState>, CanoError> {
        println!("Done!");
        Ok(TaskResult::Single(WorkflowState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let resources = Resources::new()
        .insert("config", AppConfig { batch_size: 64 });

    let workflow = Workflow::new(resources)
        .register(WorkflowState::Start, SimpleTask)
        .register(WorkflowState::Process, DoneTask)
        .add_exit_state(WorkflowState::Complete);

    workflow.orchestrate(WorkflowState::Start).await?;
    Ok(())
}

```

</div>

