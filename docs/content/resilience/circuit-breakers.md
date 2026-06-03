+++
title = "Circuit Breakers"
description = "Cano circuit breakers: the state machine, CircuitPolicy, RAII permits, wiring with TaskConfig::with_circuit_breaker, and the manual try_acquire/record_* API."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Circuit Breakers</h1>
<p class="subtitle">Stop calling a dependency that is down — before the retry loop even runs.</p>

<p>
This page covers the circuit breaker in depth. See <a href="../">Resilience</a> for how it composes
with retries, timeouts, and the other primitives.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#cb-state-machine">The state machine</a></li>
<li><a href="#cb-policy"><code>CircuitPolicy</code></a></li>
<li><a href="#cb-permits">Permits and the RAII API</a></li>
<li><a href="#cb-via-config">Wiring: <code>with_circuit_breaker</code></a></li>
<li><a href="#cb-manual">Driving it by hand</a></li>
</ol>
</nav>
<hr class="section-divider">

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
<a href="../../split-join/">split/join</a> state. Internally it's a synchronous
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
<a href="../#timeouts">timeout</a> firing is recorded as a circuit failure too, so the breaker also
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
<a href="../../resources/">resource</a> (it implements <code>Resource</code> with no-op lifecycle), look
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
The scheduler's flow-level <a href="../../scheduler/backoff-and-trip/"><code>Status::Tripped</code></a>
(a scheduled flow that stops firing after a streak of failed runs) is a <em>separate</em> mechanism
from this task-level <code>CanoError::CircuitOpen</code> — they live at different layers and don't
interact. See <a href="../../scheduler/backoff-and-trip/">Scheduler → Backoff &amp; Trip State</a>.
</p>
</div>

<p>Runnable demos: <code>cargo run --example circuit_breaker</code> (the
<code>TaskConfig::with_circuit_breaker</code> integration path) and
<code>cargo run --example circuit_breaker_manual</code> (the manual <code>try_acquire</code> /
<code>record_*</code> path shown above — breaker opens after a failure streak, rejects calls while
open, then closes via <code>HalfOpen</code>).</p>
</div>
