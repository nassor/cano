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
<li><strong>AI Agents</strong>: Multi-step inference chains with shared context and memory — see <code>cargo run --example ai_workflow_yes_and</code> (needs a local OpenAI-compatible inference server).</li>
<li><strong>Background Systems</strong>: Scheduled maintenance, periodic reporting, and distributed cron jobs.</li>
</ul>
</section>

<h2>Features</h2>
<div class="feature-grid">
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9881;</div>
<h3>Processing Models</h3>
<p>A whole <a href="task/#task-family"><code>Task</code> family</a>: plain <code>Task</code>, side-effect-free <code>RouterTask</code>, wait-until <code>PollTask</code>, wait-then-go <code>TimerTask</code>, fan-out <code>BatchTask</code>, resumable <code>SteppedTask</code>, continuous <code>StreamTask</code> — mixed freely in one workflow.</p>
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
<p>Pluggable <code>CheckpointStore</code> records every state entry with an optional <code>workflow_version</code> stamp; <code>resume_from</code> rehydrates a crashed run and refuses checkpoints whose version disagrees with the workflow. Embedded, ACID <code>RedbCheckpointStore</code> behind the <code>recovery</code> feature.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#8634;</div>
<h3>Sagas / Compensation</h3>
<p>Pair a forward step with a <code>compensate</code> action; a later failure rolls back the work already done, in reverse — and replays the rollback across a crash.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon accent" aria-hidden="true">&#9673;</div>
<h3>Observability</h3>
<p>Built-in <code>tracing</code> spans and <code>metrics</code> counters, plus <code>WorkflowObserver</code> hooks and resource health probes.</p>
</div>
</div>

<div class="diagram-frame">
<p class="diagram-label">One trigger &rarr; one FSM run &rarr; one task per state</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 466" role="img">
<title>How Cano fits together: a run starts either from a direct orchestrate() call or from the Scheduler's interval, cron and manual triggers; the Workflow FSM dispatches the Task registered for the current state and routes the returned TaskResult onward, appends every state entry to the CheckpointStore that resume_from replays after a crash, hands each task the shared Resources, and stops at an exit state &mdash; while observer hooks, tracing spans and metrics counters listen on a rail underneath.</title>
<defs><marker id="arch-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim e-dash" d="M97,320 V392" marker-end="url(#arch-ah)"/>
<path class="e e-dim e-dash" d="M390,370 V392" marker-end="url(#arch-ah)"/>
<path class="e e-dim e-dash" d="M683,206 V392" marker-end="url(#arch-ah)"/>
<path class="e" d="M390,94 V144" marker-end="url(#arch-ah)"/>
<text class="t-mut ta-s" x="400" y="121">one run per trigger</text>
<path class="e" d="M97,96 V122 Q97,130 105,130 H312 Q320,130 320,138 V144" marker-end="url(#arch-ah)"/>
<rect class="n" x="16" y="40" width="162" height="54" rx="10"/>
<text class="t-strong" x="97" y="59">Direct run</text>
<text class="t-code" x="97" y="78">orchestrate()</text>
<path class="e e-hot" d="M266,175 H182" marker-end="url(#arch-ah)"/>
<text class="t-mut t-hot" x="222" y="162">reaches exit</text>
<path class="e" d="M510,161 H598" marker-end="url(#arch-ah)"/>
<text class="t-code" x="554" y="147">append()</text>
<path class="e e-dash" d="M598,189 H514" marker-end="url(#arch-ah)"/>
<text class="t-code" x="556" y="205">resume_from</text>
<path class="e" d="M366,202 V258" marker-end="url(#arch-ah)"/>
<text class="t-mut ta-e" x="358" y="232">dispatch</text>
<path class="e" d="M414,262 V206" marker-end="url(#arch-ah)"/>
<text class="t-code ta-s" x="422" y="232">TaskResult</text>
<path class="e" d="M266,289 H182" marker-end="url(#arch-ah)"/>
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
<text class="t-mut" x="683" y="186">one row per state</text>
<rect class="n-cop" x="16" y="262" width="162" height="54" rx="10"/>
<text class="t-strong" x="97" y="281">Resources</text>
<text class="t-code" x="97" y="300">MemoryStore</text>
<rect class="n" x="270" y="262" width="240" height="104" rx="10"/>
<text class="t-strong" x="390" y="284">Task family</text>
<text class="t-mut" x="390" y="310">Task &middot; RouterTask &middot; PollTask</text>
<text class="t-mut" x="390" y="330">TimerTask &middot; BatchTask</text>
<text class="t-mut" x="390" y="350">SteppedTask &middot; StreamTask</text>
<rect class="n" x="16" y="396" width="748" height="56" rx="10"/>
<text class="t-strong" x="390" y="415">Observability rail &mdash; opt-in, zero-cost when unused</text>
<text class="t-mut" x="390" y="435">WorkflowObserver hooks &middot; tracing spans &middot; metrics counters</text>
</svg>
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
<li>Per-attempt timeouts and <a href="resilience/#workflow-total-timeout">workflow total timeout</a> with bounded compensation drain</li>
<li><a href="resilience/circuit-breakers/">Circuit breaker</a> — short-circuit a failing dependency</li>
<li><a href="resilience/rate-limiting/">Rate limiter</a> — token-bucket or fixed-window throttles, composable into <a href="resilience/rate-limiting/#rl-multi">multi-level</a> (5h + 7d + per-model) limits</li>
<li>Split <a href="split-join/#bulkhead">bulkhead</a> — cap concurrent parallel tasks</li>
<li>Panic safety — a panicking task becomes an error, never unwinds the engine</li>
<li><a href="scheduler/backoff-and-trip/">Scheduler backoff &amp; trip</a> for flaky scheduled flows</li>
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
<p>Cano requires <strong>Rust 1.98.0+</strong> (edition 2024). Add it to your <code>Cargo.toml</code>:</p>

<div class="getting-started-code">

```toml
[dependencies]
{{ cano_dep(features=["all"]) }}
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

</div>

<p>
Cano ships with <strong>no features enabled by default</strong>. <code>features = ["all"]</code> turns
on all five optional features at once:
</p>
<ul>
<li><code>scheduler</code> — the <a href="scheduler/"><code>Scheduler</code></a> (cron + interval + manual triggers)</li>
<li><code>tracing</code> — <a href="tracing/"><code>tracing</code>-crate spans</a> and the <code>TracingObserver</code></li>
<li><code>recovery</code> — <a href="recovery/"><code>RedbCheckpointStore</code></a>, the embedded ACID checkpoint store (the <code>CheckpointStore</code> trait itself is always available)</li>
<li><code>metrics</code> — <a href="metrics/"><code>metrics</code>-crate counters / histograms / gauges</a> and the <code>MetricsObserver</code></li>
<li><code>testing</code> — <a href="testing/">batteries-included test fixtures</a> (recording observer, in-memory checkpoint store, resources builder); usually a <code>[dev-dependencies]</code> feature</li>
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

    workflow.orchestrate(WorkflowState::Start, CancellationToken::disabled()).await?;
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
<li><a href="task/">Task</a> — the default processing unit, then the rest of the <a href="task/#task-family">Task family</a> (<a href="router-task/">RouterTask</a>, <a href="poll-task/">PollTask</a>, <a href="timer-task/">TimerTask</a>, <a href="batch-task/">BatchTask</a>, <a href="stepped-task/">SteppedTask</a>, <a href="stream-task/">StreamTask</a>) as you hit a shape that fits.</li>
<li><a href="split-join/">Split &amp; Join</a> and <a href="scheduler/">Scheduler</a> — parallelism within a workflow, and time-driven execution of workflows.</li>
<li>Resilience &amp; recovery: <a href="resilience/">Resilience</a>, <a href="recovery/">Recovery</a>, <a href="saga/">Saga</a>.</li>
<li>Observability: <a href="tracing/">Tracing</a>, <a href="metrics/">Metrics</a>, <a href="observers/">Observers</a>.</li>
</ol>
<p>Every concept has a runnable example under <a href="https://github.com/nassor/cano/tree/main/cano/examples"><code>cano/examples/</code></a> — each page links the relevant ones.</p>

