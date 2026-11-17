+++
title = "Configuring tasks"
description = "Configure Cano tasks with TaskConfig: opt-in retry strategies (fixed, exponential backoff), per-attempt timeouts, and wiring a circuit breaker."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Configuring tasks</h1>
<p class="subtitle">Retries, per-attempt timeouts and circuit breakers via <code>TaskConfig</code>.</p>

<p>
Every <a href="../">Task</a> can return a <code>TaskConfig</code> from its <code>config</code> method. It sets how the task retries, how long each attempt may run, and whether a circuit breaker guards it. The engine reads it once, when the task is registered.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#config-retries">Configuration and retries</a></li>
<li><a href="#config-circuit-breaker">Wiring a circuit breaker</a></li>
<li><a href="#config-reference">TaskConfig reference</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="config-retries"><a href="#config-retries" class="anchor-link" aria-hidden="true">#</a>Configuration and retries</h2>
<p>
A task runs once by default: a failed attempt fails the task. Opt in to retries to handle transient failures.
</p>

<h3>Retry strategy examples</h3>
<div class="diagram-frame">
<p class="diagram-label">Retry with backoff between attempts</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 398" role="img" aria-labelledby="task-config-retry-title task-config-retry-desc">
<title id="task-config-retry-title">Retry with exponential backoff</title>
<desc id="task-config-retry-desc">The workflow executes the task and the attempt fails. The workflow waits a backoff delay and retries. That attempt fails too, so the workflow waits about twice as long and retries again. The third attempt succeeds and the loop stops. With a fixed retry the delay stays the same between attempts.</desc>
<defs><marker id="task-config-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="190" y1="46" x2="190" y2="332"/>
<line class="lifeline" x1="530" y1="46" x2="530" y2="332"/>
<rect class="n" x="120" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="190" y="32">Workflow</text>
<rect class="n" x="460" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="530" y="32">Task</text>
<rect class="band" x="182" y="120" width="16" height="40" rx="5"/>
<text class="t-mut ta-e" x="172" y="145">wait (backoff)</text>
<rect class="band" x="182" y="202" width="16" height="80" rx="5"/>
<text class="t-mut ta-e" x="172" y="247">wait (about 2x longer)</text>
<path class="e" d="M190,82 H526" marker-end="url(#task-config-ah)"/>
<text class="t-mut" x="358" y="70">Execute</text>
<path class="e e-dash" d="M530,116 H194" marker-end="url(#task-config-ah)"/>
<text class="t-mut t-err" x="362" y="104">Fail</text>
<path class="e" d="M190,164 H526" marker-end="url(#task-config-ah)"/>
<text class="t-mut" x="358" y="152">Retry 1</text>
<path class="e e-dash" d="M530,198 H194" marker-end="url(#task-config-ah)"/>
<text class="t-mut t-err" x="362" y="186">Fail</text>
<path class="e" d="M190,286 H526" marker-end="url(#task-config-ah)"/>
<text class="t-mut" x="358" y="274">Retry 2</text>
<path class="e e-dash" d="M530,320 H194" marker-end="url(#task-config-ah)"/>
<text class="t-mut t-ok" x="362" y="308">Success</text>
<rect class="n-hot" x="40" y="340" width="640" height="34" rx="10"/>
<text class="t-strong" x="360" y="362">Exponential backoff shown; fixed retry repeats one delay.</text>
</svg>
</div>
</div>

<div class="card-stack retry-cards">
<div class="card">
<h3>Fixed retry</h3>
<p>Retry a fixed number of times with a constant delay. The count is retries, so this task runs at most four times.</p>
<div class="code-block">
<span class="code-block-label">Fixed retry config</span>

```rust
TaskConfig::new()
    .with_fixed_retry(3, Duration::from_secs(1))

```
</div>
</div>
<div class="card">
<h3>Exponential backoff</h3>
<p>Retry with exponentially growing delays, useful for rate-limited APIs. The default curve starts at 100 ms, doubles each retry, caps at 30 s and adds 10% jitter. <code>RetryMode::exponential_custom</code> sets each of these.</p>
<div class="code-block">
<span class="code-block-label">Exponential backoff config</span>

```rust
TaskConfig::new()
    .with_exponential_retry(5)

```
</div>
</div>
<div class="card">
<h3>No retries (default)</h3>
<p>One attempt, no retries, no timeout. Every task gets this when it does not override <code>config()</code>.</p>
<div class="code-block">
<span class="code-block-label">Default config</span>

```rust
TaskConfig::new()

```
</div>
</div>
</div>

<div class="card-stack retry-cards">
<div class="card">
<h3>Per-attempt timeout</h3>
<p>Bound each attempt with a fresh deadline. It composes with any retry mode.</p>
<div class="code-block">
<span class="code-block-label">Attempt timeout config</span>

```rust
TaskConfig::new()
    .with_exponential_retry(3)
    .with_attempt_timeout(Duration::from_secs(2))

```
</div>
<h4>How attempt timeouts compose with retries</h4>
<p>
Each attempt runs under <code>tokio::time::timeout</code>. An expired attempt produces a <code>CanoError::Timeout</code>, which goes through the same retry path as any other failure, so the <code>RetryMode</code> decides whether to retry. The deadline resets on every attempt. With no retries the task fails with the raw <code>Timeout</code>; with retries, exhaustion surfaces as <code>CanoError::RetryExhausted</code> wrapping the last error.
</p>
<div class="callout callout-warning">
<div class="callout-label">Cancellation</div>
<p>
When the timeout fires, the attempt is dropped at its next await point. External state it already started, such as an HTTP POST that reached the wire or a transaction without a <code>Drop</code> guard, stays as it is, and a retry may collide with it. Release such resources in a <code>Drop</code> guard, or leave <code>attempt_timeout</code> unset and bound the run with <a href="../../resilience/#workflow-total-timeout">the workflow total timeout</a>.
</p>
</div>
</div>
</div>

<h3 id="config-circuit-breaker"><a href="#config-circuit-breaker" class="anchor-link" aria-hidden="true">#</a>Wiring a circuit breaker</h3>
<p>
Attach a <code>CircuitBreaker</code> with <code>TaskConfig::with_circuit_breaker(breaker.clone())</code>. The retry loop asks the breaker before each attempt and records the outcome after it. An open breaker ends the whole loop with <code>CanoError::CircuitOpen</code> (or <code>CircuitHalfOpenBusy</code> while every trial slot is taken). Both come back raw, not wrapped in <code>RetryExhausted</code>, and no retry is spent. A breaker is cheap to clone and clones share state, so give every task that hits one dependency a clone of one breaker and they trip together.
</p>

<div class="code-block">
<span class="code-block-label">Attaching a breaker to a task config</span>

```rust
fn build_config(breaker: CircuitBreaker) -> TaskConfig {
    TaskConfig::new()
        .with_exponential_retry(3)
        .with_circuit_breaker(breaker)
}
```
</div>

<p>
The breaker's Closed, Open and HalfOpen states, <code>CircuitPolicy</code>, and the manual <code>try_acquire</code>, <code>record_success</code> and <code>record_failure</code> API are documented in the <a href="../../resilience/circuit-breakers/">Resilience guide</a>.
</p>

<h3>Example: API client with retry</h3>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#127760;</span> API client with exponential backoff</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Call, Complete }

struct ApiClientTask {
    endpoint: String,
}

#[task(state = State)]
impl ApiClientTask {
    fn config(&self) -> TaskConfig {
        // Exponential backoff for API rate limiting
        TaskConfig::new()
            .with_exponential_retry(5)
    }

    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        println!("Calling API: {}", self.endpoint);

        let board = res.get::<Blackboard>()?;

        // Replace this with your HTTP client of choice (reqwest, hyper, etc.)
        let data = String::new();

        board.put("api_response", data);
        println!("API call successful");

        Ok(State::Complete)
    }
}
```
</div>

<hr class="section-divider">
<h2 id="config-reference"><a href="#config-reference" class="anchor-link" aria-hidden="true">#</a>TaskConfig reference</h2>
<p>
<code>TaskConfig</code> has three public fields. Each builder takes <code>self</code> and returns it, so calls chain. Each entry below shows the builder and, where one applies, its default.
</p>

<dl class="key-list">
<dt><code>retry_mode: RetryMode</code> <small>default <code>RetryMode::None</code></small></dt>
<dd>Retry strategy. <code>None</code> makes one attempt, <code>Fixed { retries, delay }</code> waits a constant delay, <code>ExponentialBackoff { .. }</code> grows the delay up to <code>max_delay</code>. <code>max_attempts()</code> is retries plus one.</dd>

<dt><code>attempt_timeout: Option&lt;Duration&gt;</code> <small>default <code>None</code></small></dt>
<dd>Deadline for each attempt. An expired attempt fails with <code>CanoError::Timeout</code>.</dd>

<dt><code>circuit_breaker: Option&lt;CircuitBreaker&gt;</code> <small>default <code>None</code></small></dt>
<dd>Breaker consulted before every attempt. Share one across tasks by cloning it.</dd>

<dt><code>TaskConfig::new()</code></dt>
<dd>One attempt, no retries, no timeout, no breaker. <code>Default::default()</code> returns the same value.</dd>

<dt><code>with_retry(RetryMode)</code></dt>
<dd>Sets the retry mode directly, for example <code>RetryMode::exponential_custom(..)</code>.</dd>

<dt><code>with_fixed_retry(retries: usize, delay: Duration)</code></dt>
<dd>Shorthand for <code>RetryMode::fixed</code>.</dd>

<dt><code>with_exponential_retry(max_retries: usize)</code></dt>
<dd>Shorthand for <code>RetryMode::exponential</code>: 100 ms base, multiplier 2.0, 30 s cap, 0.1 jitter.</dd>

<dt><code>with_attempt_timeout(Duration)</code></dt>
<dd>Sets <code>attempt_timeout</code>.</dd>

<dt><code>with_circuit_breaker(CircuitBreaker)</code></dt>
<dd>Takes the breaker by value. Pass <code>breaker.clone()</code> to share state.</dd>
</dl>

</div>
