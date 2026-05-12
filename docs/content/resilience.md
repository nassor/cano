+++
title = "Resilience"
description = "The resilience guide for Cano workflows: the full circuit breaker treatment plus retries, per-attempt timeouts, bulkheads, panic safety, and scheduler backoff — how each primitive works and how they compose."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Resilience</h1>
<p class="subtitle">Recover from transient faults — retries, timeouts, circuit breakers, bulkheads, panic safety, scheduler backoff.</p>

<p>
"Resilient" in Cano's tagline isn't a vibe — it's a set of concrete, composable primitives that
sit on the FSM dispatch path. Every one of them is <strong>opt-in</strong>: a workflow that wires
none of them up pays nothing, and the hot path stays allocation-light. This is the guide: the
<a href="#circuit-breaker">circuit breaker</a> gets its full treatment here; retries, per-attempt
timeouts, and bulkheads get an overview with a pointer to the API reference page that owns the
builder methods.
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
<li><a href="#circuit-breaker">Circuit Breakers</a></li>
<li class="toc-sub"><a href="#cb-state-machine">The state machine</a></li>
<li class="toc-sub"><a href="#cb-policy"><code>CircuitPolicy</code></a></li>
<li class="toc-sub"><a href="#cb-permits">Permits and the RAII API</a></li>
<li class="toc-sub"><a href="#cb-via-config">Wiring: <code>with_circuit_breaker</code></a></li>
<li class="toc-sub"><a href="#cb-manual">Driving it by hand</a></li>
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
<code>RetryMode</code> / <code>TaskConfig</code> reference. (A <a href="../nodes/">Node</a>'s
<code>prep</code>/<code>post</code> are retried as a unit; <code>exec</code> is infallible.)</p>
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

<p>Distinct from the <em>workflow-wide</em> timeout (<code>Workflow::with_timeout</code>), which
bounds the whole orchestration, not one attempt. The full <code>TaskConfig</code> /
<code>RetryMode</code> API — including how attempt timeouts compose with each retry mode — lives in
<a href="../task/#config-retries">Tasks → Configuration &amp; Retries</a>.</p>
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
