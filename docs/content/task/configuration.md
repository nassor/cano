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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 398" role="img">
<title>Retry with backoff: the workflow calls the task, the attempt fails, the workflow waits a backoff delay, retries, fails again, waits twice as long, and the third attempt succeeds.</title>
<defs><marker id="cfg-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="190" y1="46" x2="190" y2="332"/>
<line class="lifeline" x1="530" y1="46" x2="530" y2="332"/>
<rect class="n" x="120" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="190" y="32">Workflow</text>
<rect class="n" x="460" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="530" y="32">Task</text>
<rect class="band" x="182" y="120" width="16" height="40" rx="5"/>
<text class="t-mut ta-e" x="172" y="145">wait (backoff)</text>
<rect class="band" x="182" y="202" width="16" height="80" rx="5"/>
<text class="t-mut ta-e" x="172" y="247">wait (longer, 2×)</text>
<path class="e" d="M190,82 H526" marker-end="url(#cfg-ah)"/>
<text class="t-mut" x="358" y="70">Execute</text>
<path class="e e-dash" d="M530,116 H194" marker-end="url(#cfg-ah)"/>
<text class="t-mut t-err" x="362" y="104">Fail</text>
<path class="e" d="M190,164 H526" marker-end="url(#cfg-ah)"/>
<text class="t-mut" x="358" y="152">Retry 1</text>
<path class="e e-dash" d="M530,198 H194" marker-end="url(#cfg-ah)"/>
<text class="t-mut t-err" x="362" y="186">Fail</text>
<path class="e" d="M190,286 H526" marker-end="url(#cfg-ah)"/>
<text class="t-mut" x="358" y="274">Retry 2</text>
<path class="e e-dash" d="M530,320 H194" marker-end="url(#cfg-ah)"/>
<text class="t-mut t-ok" x="362" y="308">Success ✓</text>
<rect class="n-hot" x="40" y="340" width="640" height="34" rx="10"/>
<text class="t-strong" x="360" y="362">Backoff grows between attempts; the loop stops at the first success.</text>
</svg>
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
