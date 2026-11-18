+++
title = "Task"
description = "Implement a Cano Task: one async method that returns the next state. Configure it, and choose among the eight processing models."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Task</h1>
<p class="subtitle">One async method that does the work and returns the next state.</p>

<p>
A <code>Task</code> is the basic building block of a Cano workflow: a single <code>run</code>
method that returns the next state. It is the default choice for every processing unit.
The other seven models
(<a href="../router-task/">RouterTask</a>,
<a href="../poll-task/">PollTask</a>, <a href="../timer-task/">TimerTask</a>,
<a href="../batch-task/">BatchTask</a>,
<a href="../stepped-task/">SteppedTask</a>, <a href="../stream-task/">StreamTask</a>,
<a href="../suspend-task/">SuspendTask</a>)
suit work that has a particular shape; see <a href="#task-family">the task family</a> and
<a href="#choosing">choosing a processing model</a>. At dispatch a task receives a <code>&amp;Resources</code>;
<a href="../resources/">Resources</a> explains how to register and retrieve typed dependencies.
</p>

<div class="callout callout-tip">
<div class="callout-label">New to Cano?</div>
<p>
Every example on this page wires a task into a <code>Workflow</code> and reads dependencies from a
<code>Resources</code>. Read <a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#implementing">Implementing a task</a></li>
<li><a href="#resource-free">Resource-free tasks</a></li>
<li><a href="#configuration-guide">Configuring tasks</a></li>
<li><a href="#patterns">Task patterns</a></li>
<li><a href="#task-family">The task family</a></li>
<li><a href="#choosing">Choosing a processing model</a></li>
</ol>
</nav>

<!-- Section: Implementing a Task -->
<hr class="section-divider">
<h2 id="implementing"><a href="#implementing" class="anchor-link" aria-hidden="true">#</a>Implementing a task</h2>
<p>Implement the <code>Task</code> trait for your struct. <code>run</code> is required and returns the next state. <code>config</code> and <code>name</code> are optional. With no <code>config</code>, a task makes a single attempt: retries are opt-in.</p>

<div class="diagram-frame">
<p class="diagram-label">One dispatch: config, retry envelope, run, next state</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 270" role="img" aria-labelledby="task-dispatch-title task-dispatch-desc">
<title id="task-dispatch-title">One task dispatch</title>
<desc id="task-dispatch-desc">The engine calls config() once when the task is registered and keeps the TaskConfig. At each dispatch, run_with_retries applies the retry mode, attempt timeout and circuit breaker around run(&amp;Resources). A failed attempt waits its backoff and runs again. A success returns Ok(next) and the workflow enters that state. A failure on the last attempt ends as an error: the raw error when no retries are configured, or RetryExhausted when they are.</desc>
<defs><marker id="task-dispatch-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M660,64 V42 Q660,34 652,34 H365 Q357,34 357,42 V57" marker-end="url(#task-dispatch-ah)"/>
<text class="t-mut" x="508" y="26">Err: wait backoff, then retry</text>
<path class="e" d="M140,87 H248" marker-end="url(#task-dispatch-ah)"/>
<text class="t-code t-mut" x="194" y="70">TaskConfig</text>
<path class="e" d="M462,87 H568" marker-end="url(#task-dispatch-ah)"/>
<text class="t-mut" x="515" y="70">each attempt</text>
<path class="e" d="M357,114 V148 Q357,156 349,156 H109 Q101,156 101,164 V192" marker-end="url(#task-dispatch-ah)"/>
<text class="t-mut t-err ta-e" x="345" y="144">last attempt failed</text>
<path class="e" d="M357,114 V148 Q357,156 365,156 H664 Q672,156 672,164 V192" marker-end="url(#task-dispatch-ah)"/>
<text class="t-code t-mut ta-s" x="367" y="144">Ok(next)</text>
<rect class="n" x="16" y="64" width="124" height="46" rx="10"/>
<text class="t-code" x="78" y="88">config()</text>
<text class="t-mut" x="78" y="128">once, at register</text>
<rect class="n-hot" x="252" y="60" width="210" height="54" rx="10"/>
<text class="t-code t-strong" x="357" y="78">run_with_retries</text>
<text class="t-mut" x="357" y="97">retry &#183; timeout &#183; breaker</text>
<rect class="n" x="572" y="64" width="176" height="46" rx="10"/>
<text class="t-code" x="660" y="88">run(&amp;Resources)</text>
<rect class="n-err" x="16" y="196" width="170" height="46" rx="10"/>
<text class="t-code" x="101" y="220">Err(CanoError)</text>
<rect class="n-ok" x="596" y="196" width="152" height="46" rx="10"/>
<text class="t-strong" x="672" y="220">Next state</text>
</svg>
</div>
</div>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Implementing the Task trait</span>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Action { Generate, Count, Complete }

struct GeneratorTask;

#[task(state = Action)]
impl GeneratorTask {
    // Read once at registration; 3 retries means up to 4 total attempts.
    fn config(&self) -> TaskConfig {
        TaskConfig::new().with_fixed_retry(3, Duration::from_secs(1))
    }

    async fn run(&self, res: &Resources) -> Result<Action, CanoError> {
        // Borrow the shared Blackboard by type; returns ResourceNotFound if absent.
        let board = res.get::<Blackboard>()?;

        let numbers: Vec<u32> = (1..=10).map(|i| i * 10).collect();

        // Store typed output in memory for downstream states.
        board.put("numbers", numbers);
        println!("Stored numbers");

        // Transition the workflow FSM to Action::Count.
        Ok(Action::Count)
    }
}
```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example task_simple</code>, a two-task generate and count workflow
like the snippet above.</p>
</div>

<!-- Section: Resource-Free Tasks -->
<hr class="section-divider">
<h2 id="resource-free"><a href="#resource-free" class="anchor-link" aria-hidden="true">#</a>Resource-free tasks</h2>
<p>
A task that needs no resources writes <code>run</code> without the <code>res</code> parameter.
The task macros add it, and every model macro accepts its methods this way.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Task without a resources parameter</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Compute, Done }

struct PureTask;

#[task(state = Step)]
impl PureTask {
    // Omit `res: &Resources` when unused; #[task] inserts the ignored parameter.
    async fn run(&self) -> Result<Step, CanoError> {
        let answer = 40 + 2;
        println!("Computed answer: {answer}");
        Ok(Step::Done)
    }
}
```
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
Pair a resource-free <code>run</code> with <code>Workflow::bare()</code> (or <code>Resources::new()</code>)
when no task needs shared state, such as pure pipelines or computational benchmarks.
</p>
</div>

<!-- Section: Configuration & Retries -->
<hr class="section-divider">

<h2 id="configuration-guide"><a href="#configuration-guide" class="anchor-link" aria-hidden="true">#</a>Configuring tasks</h2>
<p>Retries, per-attempt timeouts and circuit breakers are set through <code>TaskConfig</code>. See <a href="configuration/">Configuring Tasks: retries, timeouts and circuit breakers</a>.</p>
<hr class="section-divider">
<h2 id="patterns"><a href="#patterns" class="anchor-link" aria-hidden="true">#</a>Task patterns</h2>
<p>Four common shapes for a plain task.</p>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">1</span>
<h3>Data transformation</h3>
</div>
<p>Read data from the blackboard, transform it, write it back.</p>
<div class="code-block">
<span class="code-block-label">Data transformation</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Transform, Complete }

struct DataTransformer;

#[task(state = State)]
impl DataTransformer {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        let board = res.get::<Blackboard>()?;
        // Clone the typed input vector; `?` converts BlackboardError into CanoError.
        let raw_data: Vec<i32> = board.get("raw_data")?;

        // Keep positive entries and double them.
        let processed: Vec<i32> = raw_data
            .into_iter()
            .filter(|&x| x > 0)
            .map(|x| x * 2)
            .collect();

        // Store under a distinct key for the next state.
        board.put("processed_data", processed);
        Ok(State::Complete)
    }
}
```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">2</span>
<h3>Validation</h3>
</div>
<p>Check the data and return a different state for each outcome.</p>
<div class="code-block">
<span class="code-block-label">Validation with branching</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Validate, Process, ValidationFailed }

struct ValidatorTask;

#[task(state = State)]
impl ValidatorTask {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        let board = res.get::<Blackboard>()?;
        let data: Vec<f64> = board.get("processed_data")?;

        // Collect all validation errors as Vec<String> so downstream types match.
        let mut errors: Vec<String> = Vec::new();
        if data.is_empty() {
            errors.push("Data is empty".into());
        }
        if data.iter().copied().any(f64::is_nan) {
            errors.push("Contains NaN values".into());
        }

        let is_valid = errors.is_empty();
        // Store diagnostics for the ValidationFailed handler without cloning the Vec.
        board.put("validation_errors", errors);

        // Return a failure state rather than Err(...) so invalid data routes without retrying.
        if is_valid {
            Ok(State::Process)
        } else {
            Ok(State::ValidationFailed)
        }
    }
}
```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">3</span>
<h3>Conditional routing</h3>
</div>
<p>Pick the next state from values read at runtime.</p>
<div class="code-block">
<span class="code-block-label">Dynamic routing with match</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { ParallelProcess, FastTrack, BatchProcess, SimpleProcess, Skip }

struct RoutingTask;

#[task(state = State)]
impl RoutingTask {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        let board = res.get::<Blackboard>()?;
        let item_count: usize = board.get("item_count")?;
        let priority: String = board.get("priority")?;

        // Map runtime inputs to a typed State (use #[task::router] to skip checkpointing).
        let next_state = match (item_count, priority.as_str()) {
            (101.., "high") => State::ParallelProcess,
            (1..=100, "high") => State::FastTrack,
            (51.., _) => State::BatchProcess,
            (1..=50, _) => State::SimpleProcess,
            _ => State::Skip,
        };

        println!("Routing to: {next_state:?}");
        Ok(next_state)
    }
}
```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">4</span>
<h3>Aggregation</h3>
</div>
<p>Combine results written by earlier steps.</p>
<div class="code-block">
<span class="code-block-label">Aggregating parallel results</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Aggregate, Complete }

struct AggregatorTask;

#[task(state = State)]
impl AggregatorTask {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        let board = res.get::<Blackboard>()?;

        // Annotate concrete types so downstream board.get::<usize>("count") matches.
        let mut total = 0i32;
        let mut count = 0usize;

        // Skip missing branch keys when following a partial join (PartialResults or PartialTimeout).
        for i in 1..=3 {
            if let Ok(result) = board.get::<i32>(&format!("result_{i}")) {
                total += result;
                count += 1;
            }
        }

        board.put("total", total);
        board.put("count", count);

        println!("Aggregated {count} results, total: {total}");
        Ok(State::Complete)
    }
}
```
</div>
</section>

<!-- Section: The Task Family -->
<hr class="section-divider">
<h2 id="task-family"><a href="#task-family" class="anchor-link" aria-hidden="true">#</a>The task family</h2>
<p>
Seven more processing models build on <code>Task</code>. Each fits one shape of work and all of them mix freely in one workflow. Each has its own page with the full reference.
</p>

<div class="diagram-frame">
<p class="diagram-label">Seven of the processing models, grouped by what they add to a plain Task</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 272" role="img" aria-labelledby="task-family-title task-family-desc">
<title id="task-family-title">The task family</title>
<desc id="task-family-desc">A plain Task runs once and returns the next state. RouterTask also returns a state in one pass but records no checkpoint row. PollTask and TimerTask add a wait that the engine sleeps through before continuing. BatchTask, SteppedTask and StreamTask process many units of work inside one state.</desc>
<defs><marker id="task-family-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M156,138 H174 Q182,138 182,130 V62 Q182,54 190,54 H202" marker-end="url(#task-family-ah)"/>
<path class="e" d="M156,138 H202" marker-end="url(#task-family-ah)"/>
<path class="e" d="M156,138 H174 Q182,138 182,146 V214 Q182,222 190,222 H202" marker-end="url(#task-family-ah)"/>
<rect class="n-hot" x="14" y="106" width="142" height="64" rx="10"/>
<text class="t-strong" x="85" y="129">Task</text>
<text class="t-mut" x="85" y="148">run, then next state</text>
<text class="t-mut ta-s" x="206" y="18">immediate: returns a state in one pass</text>
<rect class="n" x="206" y="28" width="176" height="52" rx="10"/>
<text class="t-strong" x="294" y="45">RouterTask</text>
<text class="t-mut" x="294" y="64">decide, no checkpoint</text>
<text class="t-mut ta-s" x="206" y="102">wait-based: the engine sleeps, then continues</text>
<rect class="n" x="206" y="112" width="176" height="52" rx="10"/>
<text class="t-strong" x="294" y="129">PollTask</text>
<text class="t-mut" x="294" y="148">loop until Ready</text>
<rect class="n" x="398" y="112" width="176" height="52" rx="10"/>
<text class="t-strong" x="486" y="129">TimerTask</text>
<text class="t-mut" x="486" y="148">wait once, then route</text>
<text class="t-mut ta-s" x="206" y="186">iterating: many units inside one state</text>
<rect class="n" x="206" y="196" width="176" height="52" rx="10"/>
<text class="t-strong" x="294" y="213">BatchTask</text>
<text class="t-mut" x="294" y="232">map over a collection</text>
<rect class="n" x="398" y="196" width="176" height="52" rx="10"/>
<text class="t-strong" x="486" y="213">SteppedTask</text>
<text class="t-mut" x="486" y="232">cursor per step</text>
<rect class="n" x="590" y="196" width="176" height="52" rx="10"/>
<text class="t-strong" x="678" y="213">StreamTask</text>
<text class="t-mut" x="678" y="232">flush per window</text>
</svg>
</div>
</div>

<div class="card-stack">
<div class="card">
<h3><a href="../router-task/">RouterTask</a></h3>
<p>Side-effect-free branching: a <code>route</code> method picks the next state and writes
nothing. It leaves no checkpoint row.</p>
</div>
<div class="card">
<h3><a href="../poll-task/">PollTask</a></h3>
<p>Wait-until loops: <code>poll</code> returns <code>Ready</code> or
<code>Pending { delay_ms }</code>, and the engine sleeps for that delay between polls without blocking a thread.</p>
</div>
<div class="card">
<h3><a href="../timer-task/">TimerTask</a></h3>
<p>Wait, then transition: <code>wait</code> returns a <code>Duration</code> or a monotonic
<code>Instant</code>, the engine sleeps once, then <code>after_wait</code> routes.</p>
</div>
<div class="card">
<h3><a href="../batch-task/">BatchTask</a></h3>
<p>Fan out over data: <code>load</code>, then <code>process_item</code> for each item (bounded concurrency, per-item retry), then <code>finish</code>,
all inside one state.</p>
</div>
<div class="card">
<h3><a href="../stepped-task/">SteppedTask</a></h3>
<p>Resumable iterative work: <code>step(cursor)</code> runs in a loop and the cursor is checkpointed after each
step, so a crash resumes mid-loop.</p>
</div>
<div class="card">
<h3><a href="../stream-task/">StreamTask</a></h3>
<p>Continuous stream consumption: <code>open</code>, then <code>process_item</code> and <code>flush_window</code> per tumbling window,
until the stream is cancelled or exhausted. It resumes from a committed cursor.</p>
</div>
<div class="card">
<h3><a href="../suspend-task/">SuspendTask</a></h3>
<p>Durable pause: <code>suspend</code> notifies and the run parks. <code>Workflow::resume_with</code> later delivers a typed input to
<code>resume</code>, which returns the next state.</p>
</div>
</div>

<!-- Section: Choosing a Processing Model -->
<hr class="section-divider">
<h2 id="choosing"><a href="#choosing" class="anchor-link" aria-hidden="true">#</a>Choosing a processing model</h2>
<p>
Every model is registered with <code>Workflow::register</code>. Start from
<code>Task</code> and move to a specialised model only when your work has its shape:
</p>

<table class="styled-table">
<thead>
<tr>
<th>Model</th>
<th>Use it when</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>Task</code></td>
<td>A single <code>run</code> does the work and picks the next state. This is the default.</td>
</tr>
<tr>
<td><a href="../router-task/"><code>RouterTask</code></a></td>
<td>You only decide the next state, for example by branching on a flag or data shape, with no side effects and no recovery footprint.</td>
</tr>
<tr>
<td><a href="../poll-task/"><code>PollTask</code></a></td>
<td>You wait until something is ready, such as an external job, a queue or a flag flip, without blocking a thread.</td>
</tr>
<tr>
<td><a href="../timer-task/"><code>TimerTask</code></a></td>
<td>You pause once and continue: a fixed cool-down or debounce, or a sleep until a monotonic instant.</td>
</tr>
<tr>
<td><a href="../batch-task/"><code>BatchTask</code></a></td>
<td>You apply the same sub-operation to a collection, with bounded concurrency and per-item retry, and re-join in one state.</td>
</tr>
<tr>
<td><a href="../stepped-task/"><code>SteppedTask</code></a></td>
<td>A long iterative job, such as a page-by-page scan or a chunked migration, must resume mid-loop after a crash.</td>
</tr>
<tr>
<td><a href="../stream-task/"><code>StreamTask</code></a></td>
<td>You consume an unbounded source (Kafka, SSE, file tail) with per-window emission and bounded memory, and resume from a cursor.</td>
</tr>
<tr>
<td><a href="../suspend-task/"><code>SuspendTask</code></a></td>
<td>The run must wait for an outside party, such as a human approval or a webhook, possibly across restarts, then continue with that party's input.</td>
</tr>
</tbody>
</table>

</div>
