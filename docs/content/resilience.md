+++
title = "Resilience"
description = "The resilience guide for Cano workflows: the full circuit breaker treatment plus retries, per-attempt timeouts, rate limiting, bulkheads, panic safety, and scheduler backoff — how each primitive works and how they compose."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Resilience</h1>
<p class="subtitle">Recover from transient faults — retries, timeouts, circuit breakers, rate limiting, bulkheads, panic safety, scheduler backoff.</p>

<p>
"Resilient" in Cano's tagline isn't a vibe — it's a set of concrete, composable primitives that
sit on the FSM dispatch path. Every one of them is <strong>opt-in</strong>: a workflow that wires
none of them up pays nothing, and the hot path stays allocation-light. This is the guide: the
<a href="#circuit-breaker">circuit breaker</a> and <a href="#rate-limiter">rate limiter</a> get their
full treatment here; retries, per-attempt timeouts, and bulkheads get an overview with a pointer to
the API reference page that owns the builder methods.
</p>
<p>
The <em>self-healing</em> half of the tagline — checkpoint + resume, sagas, observers, health
probes — lives in <a href="../recovery/">Recovery</a>, <a href="../saga/">Saga</a>, and
<a href="../observers/">Observers</a>.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#retries">Retries</a></li>
<li><a href="#timeouts">Per-Attempt Timeouts</a></li>
<li><a href="#workflow-total-timeout">Workflow Total Timeout</a></li>
<li><a href="#circuit-breaker">Circuit Breakers</a></li>
<li class="toc-sub"><a href="#cb-state-machine">The state machine</a></li>
<li class="toc-sub"><a href="#cb-policy"><code>CircuitPolicy</code></a></li>
<li class="toc-sub"><a href="#cb-permits">Permits and the RAII API</a></li>
<li class="toc-sub"><a href="#cb-via-config">Wiring: <code>with_circuit_breaker</code></a></li>
<li class="toc-sub"><a href="#cb-manual">Driving it by hand</a></li>
<li><a href="#rate-limiter">Rate Limiter</a></li>
<li class="toc-sub"><a href="#rl-policy"><code>RateLimiterPolicy</code></a></li>
<li class="toc-sub"><a href="#rl-acquire">Acquiring: <code>try_acquire</code> / <code>acquire</code></a></li>
<li><a href="#bulkhead">Bulkheads (split concurrency)</a></li>
<li><a href="#panic-safety">Panic Safety</a></li>
<li><a href="#scheduler-backoff">Scheduler Backoff &amp; Trip</a></li>
<li><a href="#composing">Composing It All</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="retries"><a href="#retries" class="anchor-link" aria-hidden="true">#</a>Retries</h2>
<p>
A task's <code>config()</code> returns a <a href="../task/#config-retries"><code>TaskConfig</code></a>
whose <code>retry_mode</code> drives the workflow dispatcher's retry loop. The default is
exponential backoff with jitter (3 retries, 100ms base, 2.0× multiplier, 30s cap, 0.1 jitter);
<code>RetryMode</code> also has <code>None</code> and <code>Fixed(count, delay)</code>. On exhaustion
the loop returns <code>CanoError::RetryExhausted</code> wrapping the last error.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Fetch, Done }

#[derive(Clone)]
struct FetchTask;

#[task(state = Step)]
impl FetchTask {
    fn config(&self) -> TaskConfig {
        // 5 attempts, 200ms apart; or `.with_exponential_retry(n)` / `TaskConfig::minimal()` for none.
        TaskConfig::new().with_fixed_retry(4, Duration::from_millis(200))
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        // ... call the flaky thing ...
        Ok(TaskResult::Single(Step::Done))
    }
}
```

<p>See <a href="../task/#config-retries">Tasks → Configuration &amp; Retries</a> for the full
<code>RetryMode</code> / <code>TaskConfig</code> reference.</p>
<hr class="section-divider">

<h2 id="timeouts"><a href="#timeouts" class="anchor-link" aria-hidden="true">#</a>Per-Attempt Timeouts</h2>
<p>
<code>TaskConfig::with_attempt_timeout(Duration)</code> wraps <em>each retry attempt</em> in
<code>tokio::time::timeout</code>. A blown deadline produces <code>CanoError::Timeout</code>, which
the retry loop treats as a recoverable failure — the attempt is retried like any other, and only if
every attempt times out does the final timeout get wrapped in <code>CanoError::RetryExhausted</code>.
Combine it with retries to bound total wall-clock per state: 3 attempts × a 2s attempt timeout ≈ 6s
worst case (plus backoff).
</p>

```rust
#[task(state = Step)]
impl CallTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::new()
            .with_fixed_retry(2, Duration::from_millis(100))
            .with_attempt_timeout(Duration::from_secs(2)) // each attempt gets 2s
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}
```

<p>Distinct from <code>Workflow::with_total_timeout</code> (the wall-clock budget for the entire
orchestration — see below) and from the legacy <code>Workflow::with_timeout</code> (a blunt outer
<code>tokio::time::timeout</code> with no graceful compensation). The full <code>TaskConfig</code> /
<code>RetryMode</code> API — including how attempt timeouts compose with each retry mode — lives in
<a href="../task/#config-retries">Tasks → Configuration &amp; Retries</a>.</p>
<hr class="section-divider">

<h2 id="workflow-total-timeout"><a href="#workflow-total-timeout" class="anchor-link" aria-hidden="true">#</a>Workflow Total Timeout</h2>
<p>
<code>Workflow::with_total_timeout(Duration)</code> sets a wall-clock budget for the entire
<code>orchestrate</code> (or <a href="../recovery/"><code>resume_from</code></a>) call. When the
budget elapses, the in-flight task is aborted at its next await point, the
<a href="../saga/">saga compensation stack</a> drains against its own bounded budget, and the call
returns <code>CanoError::WorkflowTimeout { elapsed, limit }</code> wrapped in
<code>CanoError::WithStateContext</code> (or <code>CanoError::CompensationFailed</code> if any
<code>compensate</code> also fails).
</p>

```rust
let workflow = Workflow::bare()
    .with_total_timeout(Duration::from_millis(200))
    // Optional override; defaults to min(remaining_budget / 2, 30s).
    .with_compensation_timeout(Duration::from_millis(50))
    .register_with_compensation(Step::Reserve, Reserve)
    .register_with_compensation(Step::Charge, Charge)
    .register(Step::Ship, Ship)
    .add_exit_state(Step::Done);
```

<p>
The budget compounds with every other resilience primitive — retries, per-attempt timeouts, circuit
breakers — because they all run <em>inside</em> the per-state dispatch the engine wraps in
<code>tokio::time::timeout_at</code>. A task that retries forever doesn't outlive the total budget.
</p>

<h3 id="workflow-total-timeout-compensation"><a href="#workflow-total-timeout-compensation" class="anchor-link" aria-hidden="true">#</a>Compensation drain budget</h3>
<p>
When a total-timeout trip drains the <a href="../saga/">compensation stack</a>, each
<code>compensate</code> call is wrapped in <code>tokio::time::timeout_at</code> against a derived
deadline. The default is <strong><code>min(remaining_budget / 2, 30s)</code></strong> — half of
whatever budget is left when the timeout fires, capped at 30 seconds (and 30s as the floor when
nothing is left). Set <code>Workflow::with_compensation_timeout(Duration)</code> to override this
explicitly. A <code>compensate</code> that exceeds the deadline is recorded as a timeout error and
the drain continues — remaining entries error fast under the now-elapsed deadline rather than
extending the rollback indefinitely.
</p>

<h3 id="workflow-total-timeout-observer"><a href="#workflow-total-timeout-observer" class="anchor-link" aria-hidden="true">#</a>Observer hook</h3>
<p>
A <code>WorkflowObserver</code> attached via <code>with_observer</code> receives one
<code>on_workflow_timeout(elapsed, limit)</code> call when the budget fires, before the compensation
drain runs. With the <a href="../tracing/"><code>tracing</code></a> feature,
<code>TracingObserver</code> re-emits it as a <code>WARN</code>-level
<code>"workflow total timeout exceeded"</code> event with <code>elapsed_ms</code> / <code>limit_ms</code>
fields under the <code>cano::observer</code> target. See <a href="../observers/#events">Observers →
Lifecycle Events</a> for the full hook reference.
</p>

<h3 id="workflow-total-timeout-vs"><a href="#workflow-total-timeout-vs" class="anchor-link" aria-hidden="true">#</a>The three timeout knobs</h3>
<table class="styled-table">
<thead><tr><th>API</th><th>Scope</th><th>On expiry</th><th>Compensation drain</th></tr></thead>
<tbody>
<tr><td><code>TaskConfig::with_attempt_timeout</code></td><td>One attempt of one task</td><td><code>CanoError::Timeout</code> — retried like any other failure; final timeout becomes <code>RetryExhausted</code></td><td>Triggered like any other terminal task error (unbounded)</td></tr>
<tr><td><code>Workflow::with_total_timeout</code></td><td>The entire <code>orchestrate</code> / <code>resume_from</code> call</td><td>In-flight task aborted; <code>CanoError::WorkflowTimeout</code> (wrapped in <code>WithStateContext</code>)</td><td>Bounded by <code>with_compensation_timeout</code> or the default <code>min(remaining/2, 30s)</code></td></tr>
<tr><td><code>Workflow::with_timeout</code> <em>(legacy)</em></td><td>The whole orchestration future</td><td><code>CanoError::Workflow("Workflow timeout exceeded")</code> — no graceful abort</td><td><strong>None</strong> — the future is dropped abruptly</td></tr>
</tbody>
</table>
<p>
Pick <code>with_total_timeout</code> for any new code that needs a workflow-wide budget. The legacy
<code>with_timeout</code> remains for backward compatibility and composes naturally — if both are
set, whichever fires first wins.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_total_timeout</code> — a 3-step saga where the
shipping step overruns the budget; the timeout fires, the prior steps' compensations run in reverse,
and the final error is the wrapped <code>WorkflowTimeout</code>.</p>
</div>
<hr class="section-divider">

<h2 id="circuit-breaker"><a href="#circuit-breaker" class="anchor-link" aria-hidden="true">#</a>Circuit Breakers</h2>
<p>
Retries help with <em>transient</em> faults; a <code>CircuitBreaker</code> helps when a dependency
is <em>down</em> — it stops hammering it. Once it has seen <code>failure_threshold</code> consecutive
failures the breaker <strong>opens</strong>: every subsequent call short-circuits with
<code>CanoError::CircuitOpen</code> <em>without invoking the task body</em> and without consuming a
retry attempt, so the unhealthy dependency gets a break to recover.
</p>
<p>
A breaker is cheap to clone (it's an <code>Arc</code> inside) — <strong>share one
<code>Arc&lt;CircuitBreaker&gt;</code> across every task that hits the same dependency</strong> so a
trip from any caller protects every caller, including tasks running in parallel inside a
<a href="../split-join/">split/join</a> state. Internally it's a synchronous
<code>std::sync::Mutex</code> with no awaits held across the critical section, so concurrent
acquires from split tasks are safe.
</p>

<h3 id="cb-state-machine"><a href="#cb-state-machine" class="anchor-link" aria-hidden="true">#</a>The state machine</h3>
<div class="diagram-frame">
<p class="diagram-label">CircuitBreaker state machine</p>
<div class="mermaid">
stateDiagram-v2
    [*] --> Closed
    Closed --> Open: failure_threshold consecutive failures
    Open --> HalfOpen: reset_timeout elapsed (lazy, on next acquire)
    HalfOpen --> Closed: half_open_max_calls consecutive successes
    HalfOpen --> Open: any failure (fresh cool-down)
</div>
</div>
<p>
The state is <code>Closed → Open { until } → HalfOpen → Closed</code>:
</p>
<ul>
<li><strong><code>Closed</code></strong> — calls flow through; consecutive failures count toward
<code>failure_threshold</code>. A success resets the counter.</li>
<li><strong><code>Open { until }</code></strong> — every <code>try_acquire</code> returns
<code>CanoError::CircuitOpen</code> until the clock passes <code>until</code>
(<code>= when_it_opened + reset_timeout</code>).</li>
<li><strong><code>HalfOpen</code></strong> — entered <em>lazily</em>: there is no background task; the
first <code>try_acquire</code> after <code>until</code> performs the <code>Open → HalfOpen</code>
transition. It admits up to <code>half_open_max_calls</code> trial calls; that many
<em>consecutive</em> successes close the breaker, and <em>any</em> failure re-opens it with a fresh
cool-down.</li>
</ul>

<h3 id="cb-policy"><a href="#cb-policy" class="anchor-link" aria-hidden="true">#</a><code>CircuitPolicy</code></h3>
<p>
A breaker is constructed from a <code>CircuitPolicy { failure_threshold, reset_timeout,
half_open_max_calls }</code>:
</p>
<table class="styled-table">
<thead><tr><th>Field</th><th>Type</th><th>Meaning</th></tr></thead>
<tbody>
<tr><td><code>failure_threshold</code></td><td><code>u32</code></td><td>Consecutive failures in <code>Closed</code> that trip the breaker to <code>Open</code>.</td></tr>
<tr><td><code>reset_timeout</code></td><td><code>Duration</code></td><td>How long the breaker stays <code>Open</code> before the next acquire is allowed to probe (<code>Open → HalfOpen</code>).</td></tr>
<tr><td><code>half_open_max_calls</code></td><td><code>u32</code></td><td>Does double duty: the cap on concurrent trial calls admitted in <code>HalfOpen</code>, <em>and</em> the number of consecutive successes needed to close. So <code>half_open_max_calls &gt; 1</code> means "admit up to N concurrent probes, and close only after N of them all succeed."</td></tr>
</tbody>
</table>
<p>
<code>CircuitBreaker::new</code> <strong>panics</strong> on a misconfigured policy at construction:
<code>half_open_max_calls == 0</code> would deadlock the breaker permanently in <code>HalfOpen</code>
(no probe could ever be admitted), and values approaching <code>u32::MAX</code> would either
saturate the success counter before reaching the threshold or take effectively forever to close.
Both are programmer errors, caught before any task runs.
</p>

<h3 id="cb-permits"><a href="#cb-permits" class="anchor-link" aria-hidden="true">#</a>Permits and the RAII API</h3>
<p>
The breaker's primitive operations are:
</p>
<ul>
<li><code>try_acquire() -&gt; Result&lt;Permit, CanoError&gt;</code> — <code>Err(CanoError::CircuitOpen)</code>
when the breaker is <code>Open</code> (or when <code>HalfOpen</code> has already handed out its
<code>half_open_max_calls</code> trial permits).</li>
<li><code>record_success(permit)</code> — the call succeeded.</li>
<li><code>record_failure(permit)</code> — the call failed.</li>
</ul>
<p>
A <code>Permit</code> that is <strong>dropped without being consumed</strong> counts as a
<strong>failure</strong> — so an early return, a <code>?</code> bail-out, or a panic doesn't leave
the breaker believing the call succeeded. That's the panic-safety guarantee for the manual path.
</p>
<p>
Permits also carry an <em>epoch</em> tag — a counter the breaker bumps on every state transition
(<code>Closed → Open</code>, <code>Open → HalfOpen</code>, <code>HalfOpen → Open</code>,
<code>HalfOpen → Closed</code>). When a permit is consumed (success, failure, or RAII drop) the
breaker compares the permit's epoch against the current one and <strong>silently discards stale
outcomes</strong>. This means a slow caller whose call straddles a state-machine session — e.g. a
request that started in <code>Closed</code> and only returned after the breaker tripped, cooled
down, and entered a fresh <code>HalfOpen</code> probe — cannot accidentally close the breaker on the
strength of a result that was never meant to count as a probe.
</p>

<h3 id="cb-via-config"><a href="#cb-via-config" class="anchor-link" aria-hidden="true">#</a>Wiring it in: <code>TaskConfig::with_circuit_breaker</code></h3>
<p>
The common path — attach the breaker to a task's config and the workflow's retry loop does the rest.
It consults the breaker before each attempt, records the outcome after the task body, and an open
breaker short-circuits the <em>whole</em> retry loop: the <code>CircuitOpen</code> error is returned
<strong>raw, never wrapped in <code>RetryExhausted</code></strong>. A per-attempt
<a href="#timeouts">timeout</a> firing is recorded as a circuit failure too, so the breaker also
guards against slow upstreams.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Call, Done }

#[derive(Clone)]
struct CallUpstream { breaker: Arc<CircuitBreaker> }

#[task(state = Step)]
impl CallUpstream {
    fn config(&self) -> TaskConfig {
        TaskConfig::new()
            .with_fixed_retry(2, Duration::from_millis(50))
            .with_circuit_breaker(Arc::clone(&self.breaker))
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        // ... call the dependency; an Err here counts against the breaker ...
        Ok(TaskResult::Single(Step::Done))
    }
}

let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
    failure_threshold: 3,
    reset_timeout: Duration::from_secs(5),
    half_open_max_calls: 1,
}));
let workflow = Workflow::bare()
    .register(Step::Call, CallUpstream { breaker: Arc::clone(&breaker) })
    .add_exit_state(Step::Done);
```

<div class="callout callout-warning">
<span class="callout-label">Recovery needs a fresh call</span>
<p>
A breaker that trips <em>mid-loop</em> ends that retry loop immediately — even when the remaining
retry attempts (with their backoff) could outlast the breaker's <code>reset_timeout</code>. Recovery
is the <em>next</em> dispatch of that state, after the cool-down; the loop will not silently re-probe
the breaker on its own. Retrying against an open breaker would only pile load onto the dependency the
breaker is already protecting.
</p>
</div>

<h3 id="cb-manual"><a href="#cb-manual" class="anchor-link" aria-hidden="true">#</a>Driving it by hand: <code>try_acquire</code> / <code>record_*</code></h3>
<p>
When wiring it through <code>TaskConfig</code> is awkward — e.g. you guard several distinct calls in
one task body, or the breaker is shared with non-task code — register the breaker as a
<a href="../resources/">resource</a> (it implements <code>Resource</code> with no-op lifecycle), look
it up by key inside the task, and drive the RAII API directly:
</p>

```rust
let permit = breaker.try_acquire()?;        // Err(CanoError::CircuitOpen) when open
match do_the_call().await {
    Ok(v)  => { breaker.record_success(permit); Ok(v) }
    Err(e) => { breaker.record_failure(permit); Err(e) }
}
```

<p>
The trade-off: this path <strong>bypasses both</strong> the retry-loop short-circuit (you decide
whether <code>CircuitOpen</code> aborts or retries) <strong>and</strong> the before-call /
after-call ordering guarantee the <code>with_circuit_breaker</code> integration gives you for free.
So prefer <a href="#cb-via-config"><code>TaskConfig::with_circuit_breaker</code></a> whenever a task
maps one-to-one to one guarded call; reach for the manual path only when it genuinely doesn't.
</p>

<div class="callout callout-info">
<span class="callout-label">Different layer</span>
<p>
The scheduler's flow-level <a href="../scheduler/#backoff-and-trip"><code>Status::Tripped</code></a>
(a scheduled flow that stops firing after a streak of failed runs) is a <em>separate</em> mechanism
from this task-level <code>CanoError::CircuitOpen</code> — they live at different layers and don't
interact. See <a href="../scheduler/#backoff-and-trip">Scheduler → Backoff &amp; Trip State</a>.
</p>
</div>

<p>Runnable demos: <code>cargo run --example circuit_breaker</code> (the
<code>TaskConfig::with_circuit_breaker</code> integration path) and
<code>cargo run --example circuit_breaker_manual</code> (the manual <code>try_acquire</code> /
<code>record_*</code> path shown above — breaker opens after a failure streak, rejects calls while
open, then closes via <code>HalfOpen</code>).</p>
<hr class="section-divider">

<h2 id="rate-limiter"><a href="#rate-limiter" class="anchor-link" aria-hidden="true">#</a>Rate Limiter</h2>
<p>
A <a href="#circuit-breaker">circuit breaker</a> stops calls to a dependency that's <em>down</em>.
A <code>RateLimiter</code> paces calls to a dependency that's <em>up but rate-sensitive</em> — a
third-party API with a per-second quota, a database, an LLM endpoint billed per request. It smooths
bursty traffic into a steady rate the downstream can absorb.
</p>
<p>
It's a <strong>token bucket</strong>: a bucket holds fractional <code>tokens</code>; each acquisition
spends one; tokens replenish at a fixed rate up to a capacity. Refill is <strong>lazy</strong> — there's
no background task. Every acquire reads a single <code>Instant</code>, adds
<code>elapsed × refill_per_sec</code> tokens (capped at capacity), then decides. A workflow that never
builds a limiter pays nothing. The bucket starts <strong>full</strong>, so a burst of up to
<code>capacity</code> calls is admitted instantly before sustained traffic settles to the refill rate.
</p>
<p>
Like a breaker, a limiter is cheap to clone (it's an <code>Arc</code> inside) — <strong>share one
<code>Arc&lt;RateLimiter&gt;</code> across every task that draws on the same quota</strong> so the budget
is enforced globally, including across tasks running in parallel inside a
<a href="../split-join/">split/join</a> state. Internally it's a synchronous
<code>parking_lot::Mutex</code> with no awaits held across the critical section.
</p>

<h3 id="rl-policy"><a href="#rl-policy" class="anchor-link" aria-hidden="true">#</a><code>RateLimiterPolicy</code></h3>
<p>
Build a policy with <code>RateLimiterPolicy::per_second(n)</code> (or <code>::new(tokens, period)</code>
for an arbitrary window) and tune it with the <code>with_max_tokens</code> / <code>with_burst</code>
builders. Total bucket capacity is <code>max_tokens + burst</code>.
</p>
<table class="styled-table">
<thead><tr><th>Field</th><th>Type</th><th>Meaning</th></tr></thead>
<tbody>
<tr><td><code>max_tokens</code></td><td><code>u32</code></td><td>Steady-state bucket ceiling — and the size of the instantaneous burst a fresh limiter admits, since the bucket starts full. Defaults to <code>tokens</code> (one period's worth).</td></tr>
<tr><td><code>tokens_per_period</code></td><td><code>u32</code></td><td>Tokens added per <code>refill_period</code>.</td></tr>
<tr><td><code>refill_period</code></td><td><code>Duration</code></td><td>How long it takes to add <code>tokens_per_period</code> tokens. <code>per_second(n)</code> sets this to one second.</td></tr>
<tr><td><code>burst</code></td><td><code>u32</code></td><td>Extra capacity above <code>max_tokens</code> for short spikes. Defaults to <code>0</code>.</td></tr>
</tbody>
</table>
<p>
<code>RateLimiter::new</code> <strong>panics</strong> on a misconfigured policy at construction:
<code>max_tokens == 0</code> (a zero-capacity bucket could never admit a call) or a zero refill rate
(<code>tokens_per_period == 0</code> or a zero <code>refill_period</code> — the bucket would never
replenish). Both are programmer errors, caught before any task runs.
</p>

<h3 id="rl-acquire"><a href="#rl-acquire" class="anchor-link" aria-hidden="true">#</a>Acquiring: <code>try_acquire</code> / <code>acquire</code></h3>
<ul>
<li><code>try_acquire() -&gt; Option&lt;Permit&gt;</code> — non-blocking. <code>Some</code> if a token was
available (and consumes it), <code>None</code> if the bucket is empty. Use it to <em>shed</em> load.</li>
<li><code>acquire().await -&gt; Permit</code> — if the bucket is empty it computes exactly how long until
the next token refills, <code>tokio::time::sleep</code>s that long, and retries. Use it to <em>pace</em>
work.</li>
</ul>
<p>
The returned <code>Permit</code> is a lightweight RAII marker for the call's scope. Unlike a
<a href="#cb-permits">circuit-breaker permit</a> (which records a success/failure outcome) or a semaphore
permit (which returns capacity on drop), a token-bucket permit's <strong>drop is a no-op</strong> — the
token was already spent at acquisition and the bucket refills on the clock, not on release.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Call, Done }

#[derive(Clone)]
struct CallUpstream { limiter: Arc<RateLimiter> }

#[task(state = Step)]
impl CallUpstream {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        // Park until the shared budget admits this call, then proceed.
        let _permit = self.limiter.acquire().await;
        // ... call the rate-sensitive dependency ...
        Ok(TaskResult::Single(Step::Done))
    }
}

// 20 req/s, shared across every task that constructs from this Arc.
let limiter = Arc::new(RateLimiter::new(RateLimiterPolicy::per_second(20)));
let workflow = Workflow::bare()
    .register(Step::Call, CallUpstream { limiter: Arc::clone(&limiter) })
    .add_exit_state(Step::Done);
```

<p>
<code>RateLimiter</code> also implements <code>Resource</code> (no-op lifecycle), so instead of threading
the <code>Arc</code> into each task you can register it once in <a href="../resources/">Resources</a> and
look it up by key inside the task body — handy when several tasks share one quota.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example rate_limiter</code> — two spawned workers share one
<code>5 req/s</code> limiter; the printed timestamps land at ~200ms intervals, making the pacing visible.</p>
</div>
<hr class="section-divider">

<h2 id="bulkhead"><a href="#bulkhead" class="anchor-link" aria-hidden="true">#</a>Bulkheads (split concurrency)</h2>
<p>
A <a href="../split-join/">split/join</a> state runs its tasks in parallel — but "parallel" can mean
"500 connections to a database that handles 50". <a href="../split-join/#bulkhead"><code>JoinConfig::with_bulkhead(n)</code></a>
gates the task bodies on a shared <code>Semaphore</code> so at most <code>n</code> run concurrently;
the rest queue. Tasks still all get spawned and the join strategy is unchanged — only their
<em>execution</em> is rate-limited. Full details on the
<a href="../split-join/#bulkhead">Split &amp; Join</a> page.
</p>

```rust
let join = JoinConfig::new(JoinStrategy::All, S::Join).with_bulkhead(8); // ≤ 8 at a time
let workflow = Workflow::bare()
    .register_split(S::Fan, (0..200).map(|_| W).collect(), join)
    .add_exit_state(S::Done);
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example split_bulkhead</code> — 8 split tasks behind a
<code>with_bulkhead(2)</code>; the start/end timestamps make the rate-limiting visible.</p>
</div>
<hr class="section-divider">

<h2 id="panic-safety"><a href="#panic-safety" class="anchor-link" aria-hidden="true">#</a>Panic Safety</h2>
<p>
A <code>panic!</code> inside a task body — or a `.unwrap()` that fired — does <strong>not</strong>
unwind through the workflow engine and abort the runtime worker. The dispatcher wraps each task in
<code>catch_unwind</code>; a panic becomes <code>CanoError::TaskExecution("panic: …")</code>, which
then flows through the normal retry / error / (with a saga) compensation path. Split tasks do the
same per spawned task, so a panic preserves its task index. There's nothing to configure — it's
always on.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example panic_safety</code> — a task that <code>panic!</code>s
mid-workflow; the engine returns <code>CanoError::TaskExecution("panic: …")</code> and the preceding
compensatable step's rollback still runs.</p>
</div>
<hr class="section-divider">

<h2 id="scheduler-backoff"><a href="#scheduler-backoff" class="anchor-link" aria-hidden="true">#</a>Scheduler Backoff &amp; Trip</h2>
<p class="feature-tag">Behind the <code>scheduler</code> feature gate.</p>
<p>
The primitives above act per <em>dispatch</em>. When a workflow runs on a timer there's a second,
flow-level layer: a per-flow <code>BackoffPolicy</code> stretches the gap between <em>failed</em>
runs and can <strong>trip</strong> a flow that keeps failing so it stops firing until
<code>RunningScheduler::reset_flow(id)</code> clears it — surfaced as
<code>Status::Backoff { … }</code> / <code>Status::Tripped { … }</code>. That's documented on its
own page: <a href="../scheduler/#backoff-and-trip">Scheduler → Backoff &amp; Trip State</a> (runnable
demo: <code>cargo run --example scheduler_backoff --features scheduler</code>).
</p>
<hr class="section-divider">

<h2 id="composing"><a href="#composing" class="anchor-link" aria-hidden="true">#</a>Composing It All</h2>
<p>
These stack cleanly. A typical "talk to a flaky external service" state:
</p>
<ul>
<li><strong>Circuit breaker</strong> (shared across every task hitting that service) — bail fast when it's down.</li>
<li><strong>Rate limiter</strong> (also shared) — stay under the service's quota when it's up.</li>
<li><strong>Per-attempt timeout</strong> — don't hang on a slow call.</li>
<li><strong>Retries</strong> with backoff — ride out transients.</li>
<li>If the call <em>mutated</em> something, make it a <a href="../saga/">compensatable task</a> so a downstream failure can undo it.</li>
<li>Attach a <a href="../recovery/">checkpoint store</a> so a crash mid-workflow doesn't lose progress.</li>
<li>Attach a <a href="../observers/"><code>WorkflowObserver</code></a> (or just <code>TracingObserver</code>) to see retries, circuit-opens, and checkpoints as they happen.</li>
</ul>
<p>
The <code>circuit_breaker</code>, <code>workflow_recovery</code>, <code>saga_payment</code>, and
<code>workflow_observer</code> / <code>observer_metrics</code> examples shipped with the crate each
exercise one of these in isolation.
</p>
</div>
