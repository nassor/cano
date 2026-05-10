+++
title = "Resilience"
description = "The in-process resilience primitives Cano gives a workflow: retries, per-attempt timeouts, circuit breakers, bulkheads, panic safety, and scheduler backoff."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Resilience</h1>
<p class="subtitle">Recover from transient faults — retries, timeouts, circuit breakers, bulkheads, panic safety, scheduler backoff.</p>

<p>
"Resilient" in Cano's tagline isn't a vibe — it's a set of concrete, composable primitives that
sit on the FSM dispatch path. Every one of them is <strong>opt-in</strong>: a workflow that wires
none of them up pays nothing, and the hot path stays allocation-light. This page is the map; each
section links out to where the primitive lives in detail.
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
# use cano::prelude::*;
# use std::time::Duration;
# #[derive(Debug, Clone, PartialEq, Eq, Hash)] enum Step { Call, Done }
# #[derive(Clone)] struct CallTask;
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
bounds the whole orchestration, not one attempt.</p>
<hr class="section-divider">

<h2 id="circuit-breaker"><a href="#circuit-breaker" class="anchor-link" aria-hidden="true">#</a>Circuit Breakers</h2>
<p>
Retries help with <em>transient</em> faults; a <code>CircuitBreaker</code> helps when a dependency
is <em>down</em> — it stops hammering it. Once it has seen <code>failure_threshold</code> failures,
the breaker <strong>opens</strong>: every subsequent call short-circuits with
<code>CanoError::CircuitOpen</code> <em>without invoking the task body</em>. After
<code>reset_timeout</code> it goes <strong>half-open</strong> and admits up to
<code>half_open_max_calls</code> trial calls; that many consecutive successes <strong>close</strong>
it again, any failure re-opens it. The state machine is <code>Closed → Open { until } → HalfOpen →
Closed</code>; the <code>Open → HalfOpen</code> transition is lazy (checked on the next acquire — no
background task).
</p>
<p>
A breaker is cheap to clone (it's <code>Arc</code> inside) — <strong>share one breaker across every
task that calls the same dependency</strong> so they trip together.
</p>

<h3 id="cb-via-config"><a href="#cb-via-config" class="anchor-link" aria-hidden="true">#</a>Wiring it in: <code>TaskConfig::with_circuit_breaker</code></h3>
<p>The common path — the retry loop consults the breaker before each attempt; an open breaker
short-circuits the <em>whole</em> loop and the <code>CircuitOpen</code> error is returned raw (never
wrapped in <code>RetryExhausted</code>):</p>

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

# fn build() {
let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
    failure_threshold: 3,
    reset_timeout: Duration::from_secs(5),
    half_open_max_calls: 1,
}));
let workflow = Workflow::bare()
    .register(Step::Call, CallUpstream { breaker: Arc::clone(&breaker) })
    .add_exit_state(Step::Done);
# let _ = (workflow, breaker);
# }
```

<h3 id="cb-manual"><a href="#cb-manual" class="anchor-link" aria-hidden="true">#</a>Driving it by hand: <code>try_acquire</code> / <code>record_*</code></h3>
<p>
When wiring it through <code>TaskConfig</code> is awkward — e.g. you guard several distinct calls in
one task body — register the breaker as a <a href="../resources/">resource</a> (it implements
<code>Resource</code> with no-op lifecycle) and use the RAII API directly:
</p>

```rust
# use cano::prelude::*;
# async fn body(breaker: &CircuitBreaker) -> Result<(), CanoError> {
let permit = breaker.try_acquire()?;            // Err(CanoError::CircuitOpen) when open
match do_the_call().await {
    Ok(v)  => { breaker.record_success(permit); Ok(v).map(|_| ()) }
    Err(e) => { breaker.record_failure(permit); Err(e) }
}
# }
# async fn do_the_call() -> Result<(), CanoError> { Ok(()) }
```

<p>
Dropping a <code>Permit</code> without consuming it counts as a <strong>failure</strong> — so an
early return or a panic doesn't leave the breaker thinking the call succeeded. The breaker uses a
synchronous <code>std::sync::Mutex</code> with no awaits in the critical section, so it's safe for
parallel split tasks sharing one <code>Arc&lt;CircuitBreaker&gt;</code>.
</p>
<p>
Flow-level scheduler tripping (<a href="../scheduler/#backoff-and-trip"><code>Status::Tripped</code></a>)
is a <em>different layer</em> from this task-level <code>CanoError::CircuitOpen</code> — see the
scheduler page.
</p>
<p>Runnable demo: <code>cargo run --example circuit_breaker</code>.</p>
<hr class="section-divider">

<h2 id="bulkhead"><a href="#bulkhead" class="anchor-link" aria-hidden="true">#</a>Bulkheads (split concurrency)</h2>
<p>
A <a href="../workflows/">split/join</a> state runs its tasks in parallel — but "parallel" can mean
"500 connections to a database that handles 50". <code>JoinConfig::with_bulkhead(n)</code> gates the
task bodies on a shared <code>Semaphore</code> so at most <code>n</code> run concurrently; the rest
queue. Tasks still all get spawned and the join strategy is unchanged — only their <em>execution</em>
is rate-limited.
</p>

```rust
# use cano::prelude::*;
# #[derive(Debug, Clone, PartialEq, Eq, Hash)] enum S { Fan, Join, Done }
# #[derive(Clone)] struct W;
# #[task(state = S)] impl W { async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> { Ok(TaskResult::Single(S::Join)) } }
# fn build() {
let join = JoinConfig::new(JoinStrategy::All, S::Join).with_bulkhead(8); // ≤ 8 at a time
let workflow = Workflow::bare()
    .register_split(S::Fan, (0..200).map(|_| W).collect(), join)
    .add_exit_state(S::Done);
# let _ = workflow;
# }
```
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
<hr class="section-divider">

<h2 id="scheduler-backoff"><a href="#scheduler-backoff" class="anchor-link" aria-hidden="true">#</a>Scheduler Backoff &amp; Trip</h2>

<div class="feature-banner">
<div class="banner-icon" aria-hidden="true">⚙️</div>
<div class="banner-content">
<p><strong>Feature flag required</strong> — the <code>Scheduler</code> is behind the
<code>scheduler</code> feature gate.</p>
</div>
</div>

<p>
For workflows run on a timer, a per-flow <code>BackoffPolicy</code> stretches the gap between
<em>failed</em> runs (exponential, with jitter and a cap) and can <strong>trip</strong> the flow
after a streak of failures (<code>BackoffPolicy::with_trip(n)</code>) — a tripped flow stops
scheduling until <code>RunningScheduler::reset_flow(id)</code> clears it. Status reflects this:
<code>Status::Backoff { until, streak, last_error }</code> and <code>Status::Tripped { streak,
last_error }</code> (flows with no policy keep the legacy <code>Status::Failed</code> semantics —
opt in with <code>set_backoff</code>). Full details: <a href="../scheduler/#backoff-and-trip">Scheduler
→ Backoff &amp; Trip State</a>; runnable demo: <code>cargo run --example scheduler_backoff --features scheduler</code>.
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
