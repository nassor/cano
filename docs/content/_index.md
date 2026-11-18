+++
title = "Cano - Type-safe async workflow engine for Rust"
description = "Cano is an orchestration engine for building resilient systems in Rust. A workflow is a finite state machine (FSM): each state runs one task, and the task returns the next state."
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
Cano is an orchestration engine for building resilient systems in Rust.
A workflow is a <strong>finite state machine (FSM)</strong>: each state runs one task, and the task returns the next state as a type-checked enum value.
</p>

<p class="animate-in">
It fits work where state transitions matter:
</p>
<ul class="animate-in">
<li>Data pipelines: ETL jobs with parallel processing (split and join) and aggregation.</li>
<li>AI agents: multi-step inference chains with shared context and memory. Run <code>cargo run --example ai_workflow_yes_and</code> against a local OpenAI-compatible inference server.</li>
<li>Background systems: scheduled maintenance, periodic reporting and cron jobs.</li>
</ul>
</section>

<h2>Features</h2>
<div class="feature-grid">
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9881;</div>
<h3>Processing models</h3>
<p>Eight models in the <a href="task/#task-family"><code>Task</code> family</a>: <code>Task</code>, <code>RouterTask</code>, <code>PollTask</code>, <code>TimerTask</code>, <code>BatchTask</code>, <code>SteppedTask</code>, <code>StreamTask</code> and <code>SuspendTask</code>. Mix them freely in one workflow.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#9670;</div>
<h3>State machines</h3>
<p>Enum-driven state transitions, checked at compile time.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon accent" aria-hidden="true">&#8635;</div>
<h3>Retry strategies</h3>
<p>Opt-in fixed delays or exponential backoff with jitter. A task makes one attempt by default.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9202;</div>
<h3>Scheduling</h3>
<p>Intervals, cron schedules and manual triggers, with a bounded graceful stop. Several instances can share one <a href="scheduler/several-instances/">store</a> so each tick runs once.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#9881;</div>
<h3>Concurrency</h3>
<p>Split one state into parallel tasks and join them with a strategy such as <code>All</code>, <code>Quorum</code> or <code>PartialTimeout</code>.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#128190;</div>
<h3>Crash recovery</h3>
<p>A pluggable <code>CheckpointStore</code> records state entries. <code>resume_from</code> rehydrates a crashed run and refuses checkpoints whose <code>workflow_version</code> differs from the workflow's. Built-in stores keep the log in an embedded redb file, a PostgreSQL table or a NATS JetStream Key/Value bucket.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#8634;</div>
<h3>Sagas / Compensation</h3>
<p>Pair a forward step with a <code>compensate</code> action. A later failure rolls back the completed work in reverse, and the rollback replays after a crash.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon accent" aria-hidden="true">&#9673;</div>
<h3>Observability</h3>
<p><code>tracing</code> spans, <code>metrics</code> counters, <code>WorkflowObserver</code> hooks and resource health probes.</p>
</div>
</div>

<div class="diagram-frame">
<p class="diagram-label">One trigger, one FSM run, one task per state</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 24 780 444" role="img" aria-labelledby="home-arch-title home-arch-desc">
<title id="home-arch-title">How Cano fits together</title>
<desc id="home-arch-desc">A run starts from a direct orchestrate call or from the Scheduler's interval, cron and manual triggers. The Workflow FSM dispatches the task registered for the current state, receives the next state it returns, and stops at an exit state. The FSM appends state entries to the CheckpointStore, which resume_from reads to replay a crashed run. Each task receives the shared Resources. An observability rail underneath carries observer hooks, tracing spans and metrics counters.</desc>
<defs><marker id="home-arch-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim e-dash" d="M97,320 V392" marker-end="url(#home-arch-ah)"/>
<path class="e e-dim e-dash" d="M390,370 V392" marker-end="url(#home-arch-ah)"/>
<path class="e e-dim e-dash" d="M683,206 V392" marker-end="url(#home-arch-ah)"/>
<path class="e" d="M390,94 V144" marker-end="url(#home-arch-ah)"/>
<text class="t-mut ta-s" x="400" y="121">one run per trigger</text>
<path class="e" d="M97,96 V122 Q97,130 105,130 H312 Q320,130 320,138 V144" marker-end="url(#home-arch-ah)"/>
<rect class="n" x="16" y="40" width="162" height="54" rx="10"/>
<text class="t-strong" x="97" y="59">Direct run</text>
<text class="t-code" x="97" y="78">orchestrate()</text>
<path class="e e-hot" d="M266,175 H182" marker-end="url(#home-arch-ah)"/>
<text class="t-mut t-hot" x="222" y="162">reaches exit</text>
<path class="e" d="M510,161 H598" marker-end="url(#home-arch-ah)"/>
<text class="t-code" x="554" y="147">append()</text>
<path class="e e-dash" d="M598,189 H514" marker-end="url(#home-arch-ah)"/>
<text class="t-code" x="556" y="205">resume_from</text>
<path class="e" d="M366,202 V258" marker-end="url(#home-arch-ah)"/>
<text class="t-mut ta-e" x="358" y="232">dispatch</text>
<path class="e" d="M414,262 V206" marker-end="url(#home-arch-ah)"/>
<text class="t-code ta-s" x="422" y="232">next state</text>
<path class="e" d="M266,289 H182" marker-end="url(#home-arch-ah)"/>
<text class="t-code" x="224" y="276">&amp;Resources</text>
<rect class="n" x="270" y="40" width="240" height="54" rx="10"/>
<text class="t-strong" x="390" y="59">Scheduler</text>
<text class="t-mut" x="390" y="78">interval &middot; cron &middot; manual</text>
<rect class="n-ok" x="16" y="148" width="162" height="54" rx="10"/>
<text class="t-strong" x="97" y="167">Exit state</text>
<text class="t-mut" x="97" y="186">run complete</text>
<rect class="n-hot" x="270" y="148" width="240" height="54" rx="10"/>
<text class="t-strong" x="390" y="167">Workflow FSM</text>
<text class="t-mut" x="390" y="186">one task per state</text>
<rect class="n-cop" x="602" y="148" width="162" height="54" rx="10"/>
<text class="t-code" x="683" y="167">CheckpointStore</text>
<text class="t-mut" x="683" y="186">rows per state entry</text>
<rect class="n-cop" x="16" y="262" width="162" height="54" rx="10"/>
<text class="t-strong" x="97" y="281">Resources</text>
<text class="t-code" x="97" y="300">Blackboard</text>
<rect class="n" x="270" y="262" width="240" height="104" rx="10"/>
<text class="t-strong" x="390" y="284">Task family</text>
<text class="t-mut" x="390" y="310">Task &middot; RouterTask &middot; PollTask</text>
<text class="t-mut" x="390" y="330">TimerTask &middot; BatchTask &middot; SteppedTask</text>
<text class="t-mut" x="390" y="350">StreamTask &middot; SuspendTask</text>
<rect class="n" x="16" y="396" width="748" height="56" rx="10"/>
<text class="t-strong" x="390" y="415">Observability rail: opt-in, no cost when unused</text>
<text class="t-mut" x="390" y="435">WorkflowObserver hooks &middot; tracing spans &middot; metrics counters</text>
</svg>
</div>
</div>

<h2>Resilient and self-healing</h2>
<p>
Each of these is opt-in and costs nothing when unused: the FSM dispatch path stays allocation-light whether or not you wire any of it up.
</p>

<div class="feature-grid">
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#128737;&#65039;</div>
<h3>Resilient: recover from transient faults</h3>
<ul>
<li>Retries: fixed delay, or exponential backoff with jitter</li>
<li>Per-attempt timeouts and a <a href="resilience/#workflow-total-timeout">workflow total timeout</a> with a bounded compensation drain</li>
<li><a href="resilience/circuit-breakers/">Circuit breaker</a>: short-circuit a failing dependency</li>
<li><a href="resilience/rate-limiting/">Rate limiter</a>: token-bucket or fixed-window throttles, composable into <a href="resilience/rate-limiting/#rl-multi">multi-level</a> limits (for example 5 hours, 7 days and per model)</li>
<li>Split <a href="split-join/#bulkhead">bulkhead</a>: cap concurrent parallel tasks</li>
<li>Panic safety: a panicking task becomes an error and never unwinds the engine</li>
<li><a href="scheduler/backoff-and-trip/">Scheduler backoff and trip</a> for flaky scheduled flows</li>
</ul>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#128295;</div>
<h3>Self-healing: repair and report on its own state</h3>
<ul>
<li><a href="recovery/">Checkpoint and resume</a>: replay a crashed run from its last state</li>
<li><a href="saga/">Sagas and compensation</a>: roll back completed work in reverse on failure</li>
<li><a href="observers/">Observer hooks</a>: synchronous lifecycle, failure, retry and checkpoint events</li>
<li><a href="observers/#health">Resource health probes</a>: on-demand health for a workflow's dependencies</li>
</ul>
</div>
</div>
<p>Full coverage: the <a href="resilience/">Resilience</a>, <a href="recovery/">Recovery</a>, <a href="saga/">Saga</a> and <a href="observers/">Observers</a> guides.</p>

<h2>Getting started</h2>
<p>Cano requires Rust 1.98.0 or later (edition 2024). Add it to your <code>Cargo.toml</code>:</p>

<div class="getting-started-code">

```toml
[dependencies]
cano = { version = "0.16", features = ["all"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

</div>

<p>
Cano enables one feature by default, <code>store-redb</code>. <code>features = ["all"]</code> turns on all seven optional features:
</p>
<ul>
<li><code>scheduler</code>: the <a href="scheduler/"><code>Scheduler</code></a> (cron, interval and manual triggers), and with a store feature <a href="scheduler/several-instances/">several instances sharing one set of flows</a></li>
<li><code>tracing</code>: <a href="tracing/"><code>tracing</code>-crate spans</a> and the <code>TracingObserver</code></li>
<li><code>store-redb</code> (default): <a href="recovery/checkpoint-stores/#redb"><code>RedbStore</code></a>, a checkpoint store in an embedded redb file. <code>default-features = false</code> drops redb from the build</li>
<li><code>store-postgres</code>: <a href="recovery/checkpoint-stores/#postgres"><code>PostgresStore</code></a>, a checkpoint and scheduler store in PostgreSQL tables</li>
<li><code>store-nats</code>: <a href="recovery/checkpoint-stores/#nats"><code>NatsStore</code></a>, a checkpoint and scheduler store in NATS JetStream Key/Value buckets</li>
<li><code>metrics</code>: <a href="metrics/"><code>metrics</code>-crate counters, histograms and gauges</a> and the <code>MetricsObserver</code></li>
<li><code>testing</code>: <a href="testing/">test fixtures</a> (recording observer, in-memory checkpoint and scheduler stores, store contract suites, panic and saga helpers), usually enabled under <code>[dev-dependencies]</code></li>
</ul>
<p>
Enable only what you need, for example <code>features = ["scheduler"]</code>, or omit <code>features</code>
for the core. Cano runs on Tokio, so <code>tokio</code> is a direct dependency and you start the runtime
with <code>#[tokio::main]</code> or <code>tokio::runtime::Builder</code>. The two <code>tokio</code> features above are the minimum for that.
Add <code>"time"</code>, <code>"sync"</code> and others only when your own code uses them, or use <code>"full"</code>.
</p>

<h3>Basic example</h3>
<div class="getting-started-code">

```rust
use cano::prelude::*;

// Workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Start,
    Process,
    Complete,
}

// #[derive(Resource)] implements Resource for a stateless config struct
#[derive(Resource)]
struct AppConfig { batch_size: usize }

// #[task] handles the async-trait rewrite, so no async-trait crate is needed
struct SimpleTask;

#[task(state = WorkflowState)]
impl SimpleTask {
    async fn run(&self, res: &Resources) -> Result<WorkflowState, CanoError> {
        let config = res.get::<AppConfig>()?;
        println!("Processing task (batch_size={})...", config.batch_size);
        Ok(WorkflowState::Process)
    }
}

struct DoneTask;

// A task that needs no resources omits the `res` parameter
#[task(state = WorkflowState)]
impl DoneTask {
    async fn run(&self) -> Result<WorkflowState, CanoError> {
        println!("Done!");
        Ok(WorkflowState::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let resources = Resources::new()
        .insert(AppConfig { batch_size: 64 });

    let workflow = Workflow::new(resources)
        .register(WorkflowState::Start, SimpleTask)
        .register(WorkflowState::Process, DoneTask)
        .add_exit_state(WorkflowState::Complete);

    workflow.orchestrate(WorkflowState::Start).await?;
    Ok(())
}

```

</div>

<p>Run a working version with <code>cargo run --example workflow_simple</code>. The <code>task_simple</code> example runs a two-task generate and count workflow, and <code>workflow_bare</code> shows tasks without resources.</p>

<h2>Where to go next</h2>
<p>New to Cano? Read the docs in this order:</p>
<ol>
<li><a href="workflows/">Workflows</a>: states, the builder, validation, and how a run executes.</li>
<li><a href="resources/">Resources</a>: typed, lifecycle-managed dependency injection. Every task receives a <code>&amp;Resources</code>.</li>
<li><a href="task/">Task</a>: the default processing unit. The rest of the <a href="task/#task-family">Task family</a> (<a href="router-task/">RouterTask</a>, <a href="poll-task/">PollTask</a>, <a href="timer-task/">TimerTask</a>, <a href="batch-task/">BatchTask</a>, <a href="stepped-task/">SteppedTask</a>, <a href="stream-task/">StreamTask</a>, <a href="suspend-task/">SuspendTask</a>) covers shapes a plain task does not fit.</li>
<li><a href="split-join/">Split and join</a> and <a href="scheduler/">Scheduler</a>: parallelism within a workflow, and time-driven execution of workflows.</li>
<li>Resilience and recovery: <a href="resilience/">Resilience</a>, <a href="recovery/">Recovery</a>, <a href="saga/">Saga</a>.</li>
<li>Observability: <a href="tracing/">Tracing</a>, <a href="metrics/">Metrics</a>, <a href="observers/">Observers</a>.</li>
</ol>
<p>Every concept has a runnable example under <a href="https://github.com/nassor/cano/tree/main/cano/examples"><code>cano/examples/</code></a>, and each page links the relevant ones.</p>

