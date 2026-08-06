+++
title = "Task"
description = "Learn how to use Tasks in Cano - simple, flexible processing units for async workflows in Rust."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Task</h1>
<p class="subtitle">Simple, flexible processing units for your workflows.</p>

<p>
A <code>Task</code> is the fundamental building block of a Cano workflow: a single <code>run</code>
method that decides the next state. <strong>Start here</strong> — <code>Task</code> is the default
choice for every processing unit. The other six processing models
(<a href="../router-task/">RouterTask</a>,
<a href="../poll-task/">PollTask</a>, <a href="../timer-task/">TimerTask</a>,
<a href="../batch-task/">BatchTask</a>,
<a href="../stepped-task/">SteppedTask</a>, <a href="../stream-task/">StreamTask</a>) are specialisations you reach for only when a task
has a shape that one of them fits better — see <a href="#task-family">The Task Family</a> below for
the decision matrix. Tasks receive a <code>&amp;Resources</code> reference at dispatch time — see
<a href="../resources/">Resources</a> for how to register and retrieve typed dependencies.
</p>

<div class="callout callout-tip">
<div class="callout-label">New to Cano?</div>
<p>
Read <a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first — every
example on this page wires a task into a <code>Workflow</code> and pulls dependencies from a
<code>Resources</code> map. Then come back here.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#implementing">Implementing a Task</a></li>
<li><a href="#resource-free">Resource-Free Tasks</a></li>
<li><a href="#configuration-guide">Configuring Tasks</a></li>
<li><a href="#patterns">Real-World Task Patterns</a></li>
<li><a href="#task-family">The Task Family: Six More Processing Models</a></li>
<li><a href="#choosing">Choosing a Processing Model</a></li>
</ol>
</nav>

<!-- Section: Implementing a Task -->
<hr class="section-divider">
<h2 id="implementing"><a href="#implementing" class="anchor-link" aria-hidden="true">#</a>Implementing a Task</h2>
<p>To create a task, implement the <code>Task</code> trait for your struct. The trait requires a <code>run</code> method and an optional <code>config</code> method.</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Implementing Task trait</span>

```rust
use cano::prelude::*;
use rand::Rng;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Action { Generate, Count, Complete }

struct GeneratorTask;

#[task(state = Action)]
impl GeneratorTask {
    // Optional: Configure retries
    fn config(&self) -> TaskConfig {
        TaskConfig::default().with_fixed_retry(3, std::time::Duration::from_secs(1))
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<Action>, CanoError> {
        println!("🎲 GeneratorTask: Creating random numbers...");

        // 1. Look up the shared store from resources
        let store = res.get::<MemoryStore, _>("store")?;

        // 2. Perform logic
        let mut rng = rand::rng();
        let numbers: Vec<u32> = (0..10).map(|_| rng.random_range(1..=100)).collect();

        // 3. Store results
        store.put("numbers", numbers)?;
        println!("✅ Stored numbers");

        // 4. Return next state
        Ok(TaskResult::Single(Action::Count))
    }
}

```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example task_simple</code> — a two-task generate/count workflow
like the snippet above.</p>
</div>

<!-- Section: Resource-Free Tasks -->
<hr class="section-divider">
<h2 id="resource-free"><a href="#resource-free" class="anchor-link" aria-hidden="true">#</a>Resource-Free Tasks</h2>
<p>
When a task performs pure computation and needs no resources, override <code>run_bare()</code> instead of
<code>run()</code>. This skips the <code>Resources</code> parameter entirely, giving you a cleaner signature
for self-contained logic.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Task using run_bare</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Compute, Done }

struct PureTask;

#[task(state = Step)]
impl PureTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        // No resources needed — pure computation
        let answer = 40 + 2;
        println!("Computed answer: {}", answer);
        Ok(TaskResult::Single(Step::Done))
    }
}

```
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
Pair <code>run_bare()</code> with <code>Workflow::bare()</code> (or <code>Resources::empty()</code>)
when building workflows where no tasks need shared state — for example, pure pipelines or
computational benchmarks.
</p>
</div>

<!-- Section: Configuration & Retries -->
<hr class="section-divider">

<h2 id="configuration-guide"><a href="#configuration-guide" class="anchor-link" aria-hidden="true">#</a>Configuring Tasks</h2>
<p>Retries, per-attempt timeouts, and wiring a circuit breaker via <code>TaskConfig</code> are covered in depth on a dedicated page:</p>
<ul>
<li><a href="configuration/">Configuring Tasks: Retries, Timeouts &amp; Circuit Breakers</a></li>
</ul>
<hr class="section-divider">
<h2 id="patterns"><a href="#patterns" class="anchor-link" aria-hidden="true">#</a>Real-World Task Patterns</h2>
<p>Tasks excel at various workflow scenarios. Here are proven patterns from production use.</p>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">1</span>
<h3>Data Transformation Task</h3>
</div>
<p>Simple, direct data processing without complex setup.</p>
<div class="code-block">
<span class="code-block-label">Data transformation</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Transform, Complete }

#[derive(Clone)]
struct DataTransformer;

#[task(state = State)]
impl DataTransformer {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let raw_data: Vec<i32> = store.get("raw_data")?;

        // Transform: filter and multiply
        let processed: Vec<i32> = raw_data
            .into_iter()
            .filter(|&x| x > 0)
            .map(|x| x * 2)
            .collect();

        store.put("processed_data", processed)?;
        Ok(TaskResult::Single(State::Complete))
    }
}

```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">2</span>
<h3>Validation Task</h3>
</div>
<p>Quick validation logic with multiple outcomes.</p>
<div class="code-block">
<span class="code-block-label">Validation with branching</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Validate, Process, ValidationFailed }

#[derive(Clone)]
struct ValidatorTask;

#[task(state = State)]
impl ValidatorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let data: Vec<f64> = store.get("processed_data")?;

        let mut errors = Vec::new();

        if data.is_empty() {
            errors.push("Data is empty");
        }

        if data.iter().any(|&x| x.is_nan()) {
            errors.push("Contains NaN values");
        }

        store.put("validation_errors", errors.clone())?;

        if errors.is_empty() {
            Ok(TaskResult::Single(State::Process))
        } else {
            Ok(TaskResult::Single(State::ValidationFailed))
        }
    }
}

```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">3</span>
<h3>Conditional Routing Task</h3>
</div>
<p>Dynamic workflow routing based on runtime conditions.</p>
<div class="code-block">
<span class="code-block-label">Dynamic routing with match</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { ParallelProcess, FastTrack, BatchProcess, SimpleProcess, Skip }

#[derive(Clone)]
struct RoutingTask;

#[task(state = State)]
impl RoutingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let item_count: usize = store.get("item_count")?;
        let priority: String = store.get("priority")?;

        // Dynamic routing based on conditions
        let next_state = match (item_count, priority.as_str()) {
            (n, "high") if n > 100 => State::ParallelProcess,
            (n, "high") if n > 0 => State::FastTrack,
            (n, _) if n > 50 => State::BatchProcess,
            (n, _) if n > 0 => State::SimpleProcess,
            _ => State::Skip,
        };

        println!("Routing to: {:?}", next_state);
        Ok(TaskResult::Single(next_state))
    }
}

```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">4</span>
<h3>Aggregation Task</h3>
</div>
<p>Collect and combine results from previous steps.</p>
<div class="code-block">
<span class="code-block-label">Aggregating parallel results</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Aggregate, Complete }

#[derive(Clone)]
struct AggregatorTask;

#[task(state = State)]
impl AggregatorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Aggregating results...");

        let mut total = 0;
        let mut count = 0;

        // Collect results from parallel tasks
        for i in 1..=3 {
            if let Ok(result) = store.get::<i32>(&format!("result_{}", i)) {
                total += result;
                count += 1;
            }
        }

        store.put("total", total)?;
        store.put("count", count)?;

        println!("Aggregated {} results, total: {}", count, total);
        Ok(TaskResult::Single(State::Complete))
    }
}

```
</div>
</section>

<!-- Section: The Task Family -->
<hr class="section-divider">
<h2 id="task-family"><a href="#task-family" class="anchor-link" aria-hidden="true">#</a>The Task Family: Six More Processing Models</h2>
<p>
Beyond the plain <code>Task</code>, Cano ships
<strong>six more <code>Task</code>-derived processing models</strong>. Each is a specialised shape —
they all ultimately dispatch as a <code>Task</code>, so you mix them freely in one workflow — and
each has its own page with the full reference.
</p>

<div class="card-stack">
<div class="card">
<h3><a href="../router-task/">RouterTask</a></h3>
<p>Side-effect-free branching: a <code>route</code> method that picks the next state and writes
nothing. Registered with <code>register_router</code>; leaves no checkpoint row.</p>
<p><strong>Reach for it when:</strong> branching on a flag / data shape with no side effects.</p>
</div>
<div class="card">
<h3><a href="../poll-task/">PollTask</a></h3>
<p>"Wait-until" loops: a <code>poll</code> method that returns <code>Ready</code> or
<code>Pending { delay_ms }</code>, looping with adaptive backoff — no blocked thread.</p>
<p><strong>Reach for it when:</strong> waiting on an external job, queue, or flag flip.</p>
</div>
<div class="card">
<h3><a href="../timer-task/">TimerTask</a></h3>
<p>Wait-then-transition: <code>wait</code> returns a <code>Duration</code> or monotonic
<code>Instant</code>, the engine sleeps <em>once</em>, then <code>after_wait</code> routes.</p>
<p><strong>Reach for it when:</strong> a fixed cool-down/debounce, or resuming at a set instant.</p>
</div>
<div class="card">
<h3><a href="../batch-task/">BatchTask</a></h3>
<p>Fan out over data: <code>load → process_item (×N, bounded concurrency, per-item retry) → finish</code>,
re-joined in one state.</p>
<p><strong>Reach for it when:</strong> mapping a sub-operation over a collection with a single re-join.</p>
</div>
<div class="card">
<h3><a href="../stepped-task/">SteppedTask</a></h3>
<p>Resumable iterative work: a <code>step(cursor)</code> method whose cursor is checkpointed each
step, so a crash resumes mid-loop. Registered with <code>register_stepped</code>.</p>
<p><strong>Reach for it when:</strong> long page-by-page scans, chunked migrations — crash-resume finer than per-state.</p>
</div>
<div class="card">
<h3><a href="../stream-task/">StreamTask</a></h3>
<p>Continuous stream consumption: <code>open → process_item → flush_window</code> per tumbling window,
running until cancelled/exhausted, resumable from a committed cursor. Registered with
<code>register_stream</code>.</p>
<p><strong>Reach for it when:</strong> unbounded sources — Kafka, SSE, file-tail — with per-window emission and resume-from-cursor.</p>
</div>
</div>

<!-- Section: Choosing a Processing Model -->
<hr class="section-divider">
<h2 id="choosing"><a href="#choosing" class="anchor-link" aria-hidden="true">#</a>Choosing a Processing Model</h2>
<p>
All seven models dispatch as a <code>Task</code>, so you can mix them in one workflow. Start from
<code>Task</code> and move to a specialised model only when your work has its shape:
</p>

<table class="styled-table">
<thead>
<tr>
<th>Model</th>
<th>Reach for it when…</th>
<th>Register with</th>
</tr>
</thead>
<tbody>
<tr>
<td><strong><code>Task</code></strong></td>
<td>The default — a single <code>run</code> that does the work and picks the next state. Everything else is a specialisation of this.</td>
<td><code>register</code></td>
</tr>
<tr>
<td><a href="../router-task/"><strong><code>RouterTask</code></strong></a></td>
<td>You're only <em>deciding</em> the next state — branching on a flag or data shape — with no side effects and no recovery footprint.</td>
<td><code>register_router</code></td>
</tr>
<tr>
<td><a href="../poll-task/"><strong><code>PollTask</code></strong></a></td>
<td>You need to <em>wait until</em> something is ready — an external job, a queue, a flag flip — without blocking a thread.</td>
<td><code>register</code></td>
</tr>
<tr>
<td><a href="../timer-task/"><strong><code>TimerTask</code></strong></a></td>
<td>You just need to <em>pause then continue</em> — a fixed cool-down/debounce, or sleeping until a monotonic instant — with a single scheduled wake-up, not a poll loop.</td>
<td><code>register</code></td>
</tr>
<tr>
<td><a href="../batch-task/"><strong><code>BatchTask</code></strong></a></td>
<td>You're doing the <em>same sub-operation over a collection</em>, with bounded concurrency and per-item retry, re-joined in one state.</td>
<td><code>register</code></td>
</tr>
<tr>
<td><a href="../stepped-task/"><strong><code>SteppedTask</code></strong></a></td>
<td>You have a <em>long iterative job</em> (page-by-page scan, chunked migration) you want to crash-resume mid-loop, finer than per-state.</td>
<td><code>register_stepped</code></td>
</tr>
<tr>
<td><a href="../stream-task/"><strong><code>StreamTask</code></strong></a></td>
<td>You're consuming an <em>unbounded / continuous source</em> (Kafka, SSE, file-tail) — per-window emission, bounded memory, runs until cancelled/exhausted, resumable from a cursor.</td>
<td><code>register_stream</code></td>
</tr>
</tbody>
</table>

</div>
