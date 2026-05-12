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
<li><strong>AI Agents</strong>: Multi-step inference chains with shared context and memory — see <code>cargo run --example ai_workflow_yes_and</code> (needs a local Ollama).</li>
<li><strong>Background Systems</strong>: Scheduled maintenance, periodic reporting, and distributed cron jobs.</li>
</ul>
</section>

<h2>Features</h2>
<div class="feature-grid">
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9881;</div>
<h3>Processing Models</h3>
<p>A whole <a href="task/#task-family"><code>Task</code> family</a>: plain <code>Task</code>, side-effect-free <code>RouterTask</code>, wait-until <code>PollTask</code>, fan-out <code>BatchTask</code>, resumable <code>SteppedTask</code> — mixed freely in one workflow.</p>
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

<h2>Resilient, Self-Healing</h2>
<p>
What the tagline means, concretely. Every one of these is <strong>opt-in and zero-cost when unused</strong> —
the FSM dispatch hot path stays allocation-light whether or not you wire any of it up.
</p>

<div class="feature-grid">
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#128737;&#65039;</div>
<h3>Resilient — recover from transient faults</h3>
<ul>
<li>Retries — fixed, or exponential backoff with jitter</li>
<li>Per-attempt timeouts</li>
<li><a href="resilience/#circuit-breaker">Circuit breaker</a> — short-circuit a failing dependency</li>
<li>Split <a href="split-join/#bulkhead">bulkhead</a> — cap concurrent parallel tasks</li>
<li>Panic safety — a panicking task becomes an error, never unwinds the engine</li>
<li><a href="scheduler/#backoff-and-trip">Scheduler backoff &amp; trip</a> for flaky scheduled flows</li>
</ul>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#128295;</div>
<h3>Self-healing — repair &amp; report on its own state</h3>
<ul>
<li><a href="recovery/">Checkpoint + resume</a> — replay a crashed run from its last state</li>
<li><a href="saga/">Sagas / compensation</a> — roll back completed work in reverse on failure</li>
<li><a href="observers/">Observer hooks</a> — synchronous lifecycle / failure / retry / checkpoint events</li>
<li><a href="observers/#health">Resource health probes</a> — on-demand health for a workflow's dependencies</li>
</ul>
</div>
</div>
<p>Full coverage: the <a href="resilience/">Resilience</a>, <a href="recovery/">Recovery</a>, <a href="saga/">Saga</a> and <a href="observers/">Observers</a> guides.</p>

<h2>Getting Started</h2>
<p>Cano requires <strong>Rust 1.95.0+</strong> (edition 2024). Add it to your <code>Cargo.toml</code>:</p>

<div class="getting-started-code">

```toml
[dependencies]
cano = { version = "0.12", features = ["all"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

```

</div>

<p>
Cano ships with <strong>no features enabled by default</strong>. <code>features = ["all"]</code> turns
on all three optional features at once:
</p>
<ul>
<li><code>scheduler</code> — the <a href="scheduler/"><code>Scheduler</code></a> (cron + interval + manual triggers)</li>
<li><code>tracing</code> — <a href="tracing/"><code>tracing</code>-crate spans</a> and the <code>TracingObserver</code></li>
<li><code>recovery</code> — <a href="recovery/"><code>RedbCheckpointStore</code></a>, the embedded ACID checkpoint store (the <code>CheckpointStore</code> trait itself is always available)</li>
</ul>
<p>
Pick only what you need — e.g. <code>features = ["recovery"]</code>, or omit <code>features</code>
entirely for the lean core. Cano runs on the Tokio runtime, so <code>tokio</code> is a required
direct dependency — you launch the runtime via <code>#[tokio::main]</code> or
<code>tokio::runtime::Builder</code>. The two <code>tokio</code> features above are the minimum to
do that; add <code>"time"</code>, <code>"sync"</code>, etc. only if your own code calls into them, or
use <code>"full"</code> if you prefer convenience over compile time.
</p>

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

<p>Run a working version with <code>cargo run --example workflow_simple</code> (or
<code>task_simple</code> for the bare-task variant).</p>

<h2>Where to go next</h2>
<p>New to Cano? Read the docs roughly in this order:</p>
<ol>
<li><a href="workflows/">Workflows</a> — defining states, the builder, validation, and how a run executes.</li>
<li><a href="resources/">Resources</a> — typed, lifecycle-managed dependency injection (every task receives a <code>&amp;Resources</code>).</li>
<li><a href="task/">Task</a> — the default processing unit, then the rest of the <a href="task/#task-family">Task family</a> (<a href="router-task/">RouterTask</a>, <a href="poll-task/">PollTask</a>, <a href="batch-task/">BatchTask</a>, <a href="stepped-task/">SteppedTask</a>) as you hit a shape that fits.</li>
<li><a href="split-join/">Split &amp; Join</a> and <a href="scheduler/">Scheduler</a> — parallelism within a workflow, and time-driven execution of workflows.</li>
<li>Resilience &amp; recovery: <a href="resilience/">Resilience</a>, <a href="recovery/">Recovery</a>, <a href="saga/">Saga</a>.</li>
<li>Observability: <a href="tracing/">Tracing</a>, <a href="observers/">Observers</a>.</li>
</ol>
<p>Every concept has a runnable example under <a href="https://github.com/nassor/cano/tree/main/cano/examples"><code>cano/examples/</code></a> — each page links the relevant ones.</p>

