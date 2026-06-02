+++
title = "Configuring Tasks: Retries, Timeouts & Circuit Breakers"
description = "Configure Cano tasks with TaskConfig: retry strategies (fixed, exponential backoff, minimal), per-attempt timeouts, and wiring a circuit breaker."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Configuring Tasks</h1>
<p class="subtitle">Retries, per-attempt timeouts, and circuit breakers via <code>TaskConfig</code>.</p>

<p>
Every <a href="../">Task</a> can carry a <code>TaskConfig</code> that controls how it retries, how long
each attempt may run, and whether a circuit breaker guards it.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#config-retries">Configuration &amp; Retries</a></li>
<li><a href="#config-circuit-breaker">Wiring a Circuit Breaker</a></li>
</ol>
</nav>
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
documented in the <a href="../../resilience/circuit-breakers/">Resilience guide</a>.
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
</div>
