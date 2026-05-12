+++
title = "Task"
description = "Learn how to use Tasks in Cano - simple, flexible processing units for async workflows in Rust."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Task</h1>
<p class="subtitle">Simple, flexible processing units for your workflows.</p>

<p>
A <code>Task</code> is the fundamental building block of a Cano workflow: a single <code>run</code>
method that decides the next state. <strong>Start here</strong> — <code>Task</code> is the default
choice for every processing unit. The other five processing models
(<a href="../nodes/">Node</a>, <a href="../router-task/">RouterTask</a>,
<a href="../poll-task/">PollTask</a>, <a href="../batch-task/">BatchTask</a>,
<a href="../stepped-task/">SteppedTask</a>) are specialisations you reach for only when a task
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
<li><a href="#config-retries">Configuration &amp; Retries</a></li>
<li><a href="#patterns">Real-World Task Patterns</a></li>
<li><a href="#task-family">The Task Family: Four More Processing Models</a></li>
<li><a href="#choosing">Choosing a Processing Model</a></li>
<li><a href="#task-vs-node">Task vs Node, in Detail</a></li>
<li><a href="#when-to-use">When to Use Tasks vs Nodes</a></li>
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
<p>Runnable examples: <code>cargo run --example task_simple</code> (a two-task generate/count workflow,
like the snippet above) and <code>cargo run --example task_interface_demo</code> (the same struct used
through both the <code>Task</code> and <code>Node</code> interfaces).</p>
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
<h2 id="config-retries"><a href="#config-retries" class="anchor-link" aria-hidden="true">#</a>Configuration &amp; Retries</h2>
<p>
Tasks can be configured with retry strategies to handle transient failures.
The <code>TaskConfig</code> struct allows you to specify the retry behavior.
</p>

<h3>Retry Strategy Examples</h3>
<div class="diagram-frame">
<p class="diagram-label">Retry with backoff between attempts</p>
<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant T as Task
W->>T: Execute
T-->>W: Fail
Note over W: Wait (backoff)
W->>T: Retry 1
T-->>W: Fail
Note over W: Wait (longer)
W->>T: Retry 2
T-->>W: Success ✓
</div>
</div>

<div class="card-stack retry-cards">
<div class="card">
<h3>Fixed Retry</h3>
<p>Retry a fixed number of times with a constant delay between attempts.</p>
<div class="code-block">
<span class="code-block-label">Fixed retry config</span>

```rust
TaskConfig::default()
    .with_fixed_retry(3, Duration::from_secs(1))

```
</div>
</div>
<div class="card">
<h3>Exponential Backoff</h3>
<p>Retry with exponentially increasing delays, useful for rate-limited APIs.</p>
<div class="code-block">
<span class="code-block-label">Exponential backoff config</span>

```rust
TaskConfig::default()
    .with_exponential_retry(5)

```
</div>
</div>
<div class="card">
<h3>Minimal Config</h3>
<p>Fast execution with minimal retry overhead for reliable operations.</p>
<div class="code-block">
<span class="code-block-label">Minimal config</span>

```rust
TaskConfig::minimal()

```
</div>
</div>
</div>

<div class="card-stack retry-cards">
<div class="card">
<h3>Per-Attempt Timeout</h3>
<p>Bound each attempt with a fresh deadline. Composes with any retry mode.</p>
<div class="code-block">
<span class="code-block-label">Attempt timeout config</span>

```rust
TaskConfig::default()
    .with_exponential_retry(3)
    .with_attempt_timeout(Duration::from_secs(2))

```
</div>
<h4>How attempt timeouts compose with retries</h4>
<p>
When <code>attempt_timeout</code> is set, each attempt inside <code>run_with_retries</code> is wrapped in
<code>tokio::time::timeout</code>. An expired attempt produces a <code>CanoError::Timeout</code>, which is
fed through the same retry path as any other failure — so the configured <code>RetryMode</code> decides
whether to retry. The deadline resets on every attempt, and retry exhaustion still surfaces as
<code>CanoError::RetryExhausted</code> wrapping the underlying timeout context.
</p>
</div>
</div>

<h3 id="config-circuit-breaker"><a href="#config-circuit-breaker" class="anchor-link" aria-hidden="true">#</a>Wiring a Circuit Breaker</h3>
<p>
A <code>CircuitBreaker</code> can be attached to a task's config via
<code>TaskConfig::with_circuit_breaker(Arc::clone(&amp;breaker))</code>. The retry loop consults it
<em>before</em> each attempt; an open breaker short-circuits the whole loop with
<code>CanoError::CircuitOpen</code> (returned raw, not wrapped in <code>RetryExhausted</code>), so a
dependency that is already down is not hammered. Share one <code>Arc&lt;CircuitBreaker&gt;</code>
across every task that hits the same dependency so they trip together.
</p>

<div class="code-block">
<span class="code-block-label">Attaching a breaker to a task config</span>

```rust
fn build_config(breaker: Arc<CircuitBreaker>) -> TaskConfig {
    TaskConfig::default()
        .with_exponential_retry(3)
        .with_circuit_breaker(breaker)
}
```
</div>

<p>
The breaker itself — its <code>Closed → Open { until } → HalfOpen</code> state machine,
<code>CircuitPolicy</code>, the lazy <code>Open → HalfOpen</code> transition, and the manual
<code>try_acquire</code> / <code>record_success</code> / <code>record_failure</code> RAII API — is
documented in the <a href="../resilience/#circuit-breaker">Resilience guide</a>.
</p>

<h3>Real-World Example: API Client with Retry</h3>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#127760;</span> API client with exponential backoff</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Call, Complete }

#[derive(Clone)]
struct ApiClientTask {
    endpoint: String,
}

#[task(state = State)]
impl ApiClientTask {
    fn config(&self) -> TaskConfig {
        // Exponential backoff for API rate limiting
        TaskConfig::default()
            .with_exponential_retry(5)
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        println!("📡 Calling API: {}", self.endpoint);

        let store = res.get::<MemoryStore, _>("store")?;

        // Replace this with your HTTP client of choice (reqwest, hyper, etc.)
        let data = String::new();

        store.put("api_response", data)?;
        println!("✅ API call successful");

        Ok(TaskResult::Single(State::Complete))
    }
}
```
</div>

<!-- Section: Real-World Patterns -->
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
<h2 id="task-family"><a href="#task-family" class="anchor-link" aria-hidden="true">#</a>The Task Family: Four More Processing Models</h2>
<p>
Beyond the plain <code>Task</code> and the structured <a href="../nodes/">Node</a>, Cano ships
<strong>four more <code>Task</code>-derived processing models</strong>. Each is a specialised shape —
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
</div>

<!-- Section: Choosing a Processing Model -->
<hr class="section-divider">
<h2 id="choosing"><a href="#choosing" class="anchor-link" aria-hidden="true">#</a>Choosing a Processing Model</h2>
<p>
All six models dispatch as a <code>Task</code>, so you can mix them in one workflow. Start from
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
<td><a href="../nodes/"><strong><code>Node</code></strong></a></td>
<td>You want a structured <code>prep → exec → post</code> split, with full-pipeline retry, to keep IO and compute apart.</td>
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
<td><a href="../batch-task/"><strong><code>BatchTask</code></strong></a></td>
<td>You're doing the <em>same sub-operation over a collection</em>, with bounded concurrency and per-item retry, re-joined in one state.</td>
<td><code>register</code></td>
</tr>
<tr>
<td><a href="../stepped-task/"><strong><code>SteppedTask</code></strong></a></td>
<td>You have a <em>long iterative job</em> (page-by-page scan, chunked migration) you want to crash-resume mid-loop, finer than per-state.</td>
<td><code>register_stepped</code></td>
</tr>
</tbody>
</table>

<!-- Section: Task vs Node, in detail -->
<hr class="section-divider">
<h2 id="task-vs-node"><a href="#task-vs-node" class="anchor-link" aria-hidden="true">#</a>Task vs Node, in Detail</h2>
<p>
<code>Task</code> and <code>Node</code> are the two you'll use most. Every <code>Node</code> automatically
implements <code>Task</code>, so they mix freely in the same workflow.
</p>

<div class="comparison-grid">
<div class="comparison-col">
<h3>Task</h3>
<p><strong>Best for:</strong> Simple logic, quick prototyping, functional style.</p>
<ul>
<li>Single <code>run()</code> method</li>
<li>Direct control over flow</li>
<li>Supports <code>run_bare()</code> for resource-free tasks</li>
</ul>
</div>
<div class="comparison-col">
<h3>Node</h3>
<p><strong>Best for:</strong> Complex operations, robust error handling, structured data flow.</p>
<ul>
<li>3 Phases: <code>prep</code>, <code>exec</code>, <code>post</code></li>
<li>Full-pipeline retry: <code>prep</code> &rarr; <code>exec</code> &rarr; <code>post</code> restarts on failure</li>
<li>Separation of concerns (IO vs Compute)</li>
</ul>
</div>
</div>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128260;</span> Mixing Tasks and Nodes</span>

```rust
// Mixing Tasks and Nodes in one workflow
let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
    .register(State::Init, SimpleTask)            // Task
    .register(State::Process, ComplexNode::new()) // Node
    .register(State::Finish, FinishTask);         // Another Task

```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example mixed_workflow</code> — Tasks and Nodes in one workflow.
For a tour that chains <em>all six</em> processing models in a single checkpointed workflow, see
<code>cargo run --example processing_models_tour --features recovery</code>.</p>
</div>

<!-- Section: When to Use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use Tasks vs Nodes?</h2>
<p>Choose the right abstraction for your use case:</p>

<table class="styled-table">
<thead>
<tr>
<th>Scenario</th>
<th>Use Task</th>
<th>Use Node</th>
</tr>
</thead>
<tbody>
<tr>
<td>Data transformation</td>
<td>Simple transform</td>
<td>Complex with validation</td>
</tr>
<tr>
<td>API calls</td>
<td>Simple requests</td>
<td>With auth &amp; retry logic</td>
</tr>
<tr>
<td>Validation</td>
<td>Quick checks</td>
<td>Usually overkill</td>
</tr>
<tr>
<td>File operations</td>
<td>For simple cases</td>
<td>Load, process, save pattern</td>
</tr>
<tr>
<td>Prototyping</td>
<td>Fastest iteration</td>
<td>More structure</td>
</tr>
<tr>
<td>Production systems</td>
<td>When simple is sufficient</td>
<td>For robust operations</td>
</tr>
</tbody>
</table>
</div>

