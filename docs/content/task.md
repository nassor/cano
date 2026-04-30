+++
title = "Tasks"
description = "Learn how to use Tasks in Cano - simple, flexible processing units for async workflows in Rust."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Tasks</h1>
<p class="subtitle">Simple, flexible processing units for your workflows.</p>

<p>
A <code>Task</code> provides a simplified interface with a single <code>run</code> method.
Use tasks when you want simplicity and direct control over the execution logic.
Tasks are the fundamental building blocks of Cano workflows.
</p>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
A Task is the simplest way to define workflow logic in Cano. Implement a single <code>run()</code> method,
and you have a fully functional processing unit. For more structured operations, see <a href="../nodes/">Nodes</a>.
Tasks receive a <code>&amp;Resources</code> reference at dispatch time — see <a href="../resources/">Resources</a>
for how to register and retrieve typed dependencies.
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
<li><a href="#task-vs-node">Task vs Node</a></li>
<li><a href="#when-to-use">When to Use Tasks vs Nodes</a></li>
</ol>
</nav>

<!-- Section: Implementing a Task -->
<hr class="section-divider">
<h2 id="implementing"><a href="#implementing" class="anchor-link" aria-hidden="true">#</a>Implementing a Task</h2>
<p>To create a task, implement the <code>Task</code> trait for your struct. The trait requires a <code>run</code> method and an optional <code>config</code> method.</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Implementing Task trait</span>
<pre><code class="language-rust">use cano::prelude::*;
use rand::RngExt;
<!--blank-->
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Action { Generate, Count, Complete }
<!--blank-->
struct GeneratorTask;
<!--blank-->
#[task(state = Action)]
impl GeneratorTask {
    // Optional: Configure retries
    fn config(&self) -> TaskConfig {
        TaskConfig::default().with_fixed_retry(3, std::time::Duration::from_secs(1))
    }
<!--blank-->
    async fn run(&self, res: &Resources) -> Result<TaskResult<Action>, CanoError> {
        println!("🎲 GeneratorTask: Creating random numbers...");
<!--blank-->
        // 1. Look up the shared store from resources
        let store = res.get::<MemoryStore, str>("store")?;
<!--blank-->
        // 2. Perform logic
        let mut rng = rand::rng();
        let numbers: Vec<u32> = (0..10).map(|_| rng.random_range(1..=100)).collect();
<!--blank-->
        // 3. Store results
        store.put("numbers", numbers)?;
        println!("✅ Stored numbers");
<!--blank-->
        // 4. Return next state
        Ok(TaskResult::Single(Action::Count))
    }
}</code></pre>
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
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Compute, Done }
<!--blank-->
struct PureTask;
<!--blank-->
#[task(state = Step)]
impl PureTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        // No resources needed — pure computation
        let answer = 40 + 2;
        println!("Computed answer: {}", answer);
        Ok(TaskResult::Single(Step::Done))
    }
}</code></pre>
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

<div class="card-stack retry-cards">
<div class="card">
<h3>Fixed Retry</h3>
<p>Retry a fixed number of times with a constant delay between attempts.</p>
<div class="code-block">
<span class="code-block-label">Fixed retry config</span>
<pre><code class="language-rust">TaskConfig::default()
    .with_fixed_retry(3, Duration::from_secs(1))</code></pre>
</div>
</div>
<div class="card">
<h3>Exponential Backoff</h3>
<p>Retry with exponentially increasing delays, useful for rate-limited APIs.</p>
<div class="code-block">
<span class="code-block-label">Exponential backoff config</span>
<pre><code class="language-rust">TaskConfig::default()
    .with_exponential_retry(5)</code></pre>
</div>
</div>
<div class="card">
<h3>Minimal Config</h3>
<p>Fast execution with minimal retry overhead for reliable operations.</p>
<div class="code-block">
<span class="code-block-label">Minimal config</span>
<pre><code class="language-rust">TaskConfig::minimal()</code></pre>
</div>
</div>
</div>

<div class="card-stack retry-cards">
<div class="card">
<h3>Per-Attempt Timeout</h3>
<p>Bound each attempt with a fresh deadline. Composes with any retry mode.</p>
<div class="code-block">
<span class="code-block-label">Attempt timeout config</span>
<pre><code class="language-rust">TaskConfig::default()
    .with_exponential_retry(3)
    .with_attempt_timeout(Duration::from_secs(2))</code></pre>
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

<div class="card-stack retry-cards">
<div class="card">
<h3>Circuit Breaker</h3>
<p>Share one breaker across tasks; an open breaker short-circuits the call before the retry loop runs.</p>
<div class="code-block">
<span class="code-block-label">Circuit breaker config</span>
<pre><code class="language-rust">let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
    failure_threshold: 5,
    reset_timeout: Duration::from_secs(30),
    half_open_max_calls: 1,
}));
<!--blank-->
TaskConfig::default()
    .with_exponential_retry(3)
    .with_circuit_breaker(Arc::clone(&breaker))</code></pre>
</div>
<h4>How the circuit breaker composes with retries</h4>
<p>
The breaker is consulted <em>before</em> each attempt. While <code>Closed</code>, calls flow through and
consecutive failures are counted toward <code>failure_threshold</code>; when the threshold trips the
breaker to <code>Open</code>, <code>run_with_retries</code> returns <code>CanoError::CircuitOpen</code>
immediately — without invoking the task body and without consuming a retry attempt — so the
underlying dependency is not hammered while it is unhealthy. After <code>reset_timeout</code> elapses,
the next call lazily transitions to <code>HalfOpen</code>, which admits up to
<code>half_open_max_calls</code> trial calls; that many consecutive successes close the breaker, and
any failure reopens it with a fresh cool-down (so to reach the close-threshold every admitted
trial must succeed — setting <code>half_open_max_calls</code> &gt; 1 means "admit up to N concurrent
probes, close only after N of them all succeed"). A timed-out attempt
(<code>CanoError::Timeout</code> from <code>attempt_timeout</code>) is recorded as a circuit failure
just like any other error, so the breaker also protects against slow upstreams. The same
<code>Arc&lt;CircuitBreaker&gt;</code> can be shared across multiple tasks (or across parallel split
tasks) so a trip from any caller protects every caller — see
<code>examples/circuit_breaker.rs</code> for an end-to-end demo.
</p>
<h4>Recovery requires a fresh call</h4>
<p>
A breaker tripped <em>mid-loop</em> ends the retry loop immediately, even when remaining retry
attempts could outlast the breaker's <code>reset_timeout</code>. Recovery requires a fresh
<code>run_with_retries</code> call after the cool-down — the loop will not silently re-probe the
breaker on its own. Retries against an open breaker would only add load to a dependency the
breaker is already protecting.
</p>
<h4>Default integration vs. <code>Resource</code> registration</h4>
<p>
<code>TaskConfig::with_circuit_breaker(...)</code> is the recommended path: the retry loop both
short-circuits on <code>CircuitOpen</code> and guarantees the breaker observes each call's outcome
before/after the task body. <code>CircuitBreaker</code> also implements
<code>Resource</code>, so it can be registered in <code>Resources</code> and looked up by key —
but that pattern bypasses both the retry-loop short-circuit and the
before-call/after-call ordering guarantee, since the caller drives <code>try_acquire</code> /
<code>record_success</code> / <code>record_failure</code> manually. Reach for the
<code>Resource</code> path only when one task body needs to share one breaker across several
internal sub-calls.
</p>
<h4>Stale outcomes are ignored across state transitions</h4>
<p>
Permits issued by <code>try_acquire</code> are tagged with the breaker's current
<em>epoch</em> — a counter bumped on every <code>Closed → Open</code>,
<code>Open → HalfOpen</code>, <code>HalfOpen → Open</code>, and <code>HalfOpen → Closed</code>
transition. When a permit is consumed (success, failure, or RAII drop) the breaker checks the
permit's epoch against the current one and discards stale outcomes silently. This means a slow
caller whose call straddles a state-machine session — for example, a request that started in
<code>Closed</code> and only returned after the breaker tripped, cooled down, and entered a new
<code>HalfOpen</code> probe — cannot accidentally close the breaker on the strength of a result
that was never meant to be a probe.
</p>
<h4>Policy validation</h4>
<p>
<code>CircuitBreaker::new</code> panics on misconfigured policy at construction —
<code>half_open_max_calls == 0</code> would deadlock the breaker permanently in
<code>HalfOpen</code> (no probe could ever be admitted), and values approaching
<code>u32::MAX</code> would either saturate the success counter before reaching the threshold or
take effectively forever to close. Treat both as programmer errors caught before any task runs.
</p>
</div>
</div>

<h3>Real-World Example: API Client with Retry</h3>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#127760;</span> API client with exponential backoff</span>
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
#[derive(Clone)]
struct ApiClientTask {
    endpoint: String,
}
<!--blank-->
#[task(state = State)]
impl ApiClientTask {
    fn config(&self) -> TaskConfig {
        // Exponential backoff for API rate limiting
        TaskConfig::default()
            .with_exponential_retry(5)
    }
<!--blank-->
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        println!("📡 Calling API: {}", self.endpoint);
<!--blank-->
        let store = res.get::<MemoryStore, str>("store")?;
<!--blank-->
        // Simulate API call that might fail
        let response = reqwest::get(&self.endpoint)
            .await
            .map_err(|e| CanoError::task_execution(e.to_string()))?;
<!--blank-->
        let data = response.text().await
            .map_err(|e| CanoError::task_execution(e.to_string()))?;
<!--blank-->
        store.put("api_response", data)?;
        println!("✅ API call successful");
<!--blank-->
        Ok(TaskResult::Single(State::Complete))
    }
}</code></pre>
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
<pre><code class="language-rust">#[derive(Clone)]
struct DataTransformer;
<!--blank-->
#[task(state = State)]
impl DataTransformer {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        let raw_data: Vec<i32> = store.get("raw_data")?;
<!--blank-->
        // Transform: filter and multiply
        let processed: Vec<i32> = raw_data
            .into_iter()
            .filter(|&x| x > 0)
            .map(|x| x * 2)
            .collect();
<!--blank-->
        store.put("processed_data", processed)?;
        Ok(TaskResult::Single(State::Complete))
    }
}</code></pre>
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
<pre><code class="language-rust">#[derive(Clone)]
struct ValidatorTask;
<!--blank-->
#[task(state = State)]
impl ValidatorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        let data: Vec<f64> = store.get("processed_data")?;
<!--blank-->
        let mut errors = Vec::new();
<!--blank-->
        if data.is_empty() {
            errors.push("Data is empty");
        }
<!--blank-->
        if data.iter().any(|&x| x.is_nan()) {
            errors.push("Contains NaN values");
        }
<!--blank-->
        store.put("validation_errors", errors.clone())?;
<!--blank-->
        if errors.is_empty() {
            Ok(TaskResult::Single(State::Process))
        } else {
            Ok(TaskResult::Single(State::ValidationFailed))
        }
    }
}</code></pre>
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
<pre><code class="language-rust">#[derive(Clone)]
struct RoutingTask;
<!--blank-->
#[task(state = State)]
impl RoutingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        let item_count: usize = store.get("item_count")?;
        let priority: String = store.get("priority")?;
<!--blank-->
        // Dynamic routing based on conditions
        let next_state = match (item_count, priority.as_str()) {
            (n, "high") if n > 100 => State::ParallelProcess,
            (n, "high") if n > 0 => State::FastTrack,
            (n, _) if n > 50 => State::BatchProcess,
            (n, _) if n > 0 => State::SimpleProcess,
            _ => State::Skip,
        };
<!--blank-->
        println!("Routing to: {:?}", next_state);
        Ok(TaskResult::Single(next_state))
    }
}</code></pre>
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
<pre><code class="language-rust">#[derive(Clone)]
struct AggregatorTask;
<!--blank-->
#[task(state = State)]
impl AggregatorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        println!("Aggregating results...");
<!--blank-->
        let mut total = 0;
        let mut count = 0;
<!--blank-->
        // Collect results from parallel tasks
        for i in 1..=3 {
            if let Ok(result) = store.get::<i32>(&format!("result_{}", i)) {
                total += result;
                count += 1;
            }
        }
<!--blank-->
        store.put("total", total)?;
        store.put("count", count)?;
<!--blank-->
        println!("Aggregated {} results, total: {}", count, total);
        Ok(TaskResult::Single(State::Complete))
    }
}</code></pre>
</div>
</section>

<!-- Section: Task vs Node -->
<hr class="section-divider">
<h2 id="task-vs-node"><a href="#task-vs-node" class="anchor-link" aria-hidden="true">#</a>Task vs Node</h2>
<p>
Cano supports both <code>Task</code> and <code>Node</code> interfaces. Every Node automatically implements Task, so they can be mixed in the same workflow.
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
<pre><code class="language-rust">// Mixing Tasks and Nodes in one workflow
let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
    .register(State::Init, SimpleTask)            // Task
    .register(State::Process, ComplexNode::new()) // Node
    .register(State::Finish, FinishTask);         // Another Task</code></pre>
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

