+++
title = "Circuit breakers"
description = "Cano circuit breakers: the state machine, CircuitPolicy, RAII permits, wiring with TaskConfig::with_circuit_breaker, and the manual try_acquire and record_* API."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Circuit breakers</h1>
<p class="subtitle">Stop calling a dependency that is down, before the retry loop runs.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.11.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>

<p>
This page is a how-to and reference for <code>CircuitBreaker</code>. See <a href="../">Resilience</a>
for how it nests with retries, timeouts and the other primitives.
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
Retries help with transient faults. A <code>CircuitBreaker</code> helps when a dependency is down:
after <code>failure_threshold</code> consecutive failures it opens, and every later call fails with
<code>CanoError::CircuitOpen</code> without running the task body or using a retry attempt. The
dependency gets time to recover.
</p>
<p>
A breaker is cheap to clone, and clones share one state machine. Give every task that calls the same
dependency a clone of one breaker, so a trip from any caller protects all of them, including tasks
running in parallel inside a <a href="../../split-join/">split/join</a> state. The state sits behind
a <code>std::sync::Mutex</code> that is never held across an await.
</p>

<h3 id="cb-state-machine"><a href="#cb-state-machine" class="anchor-link" aria-hidden="true">#</a>The state machine</h3>
<div class="diagram-frame">
<p class="diagram-label">CircuitBreaker state machine</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 33 740 273" role="img" aria-labelledby="breaker-states-title breaker-states-desc">
<title id="breaker-states-title">CircuitBreaker state machine</title>
<desc id="breaker-states-desc">The breaker starts in Closed, where calls flow through. After failure_threshold consecutive failures it moves to Open, which rejects every call with CircuitOpen. Once reset_timeout has elapsed, the next try_acquire moves it to HalfOpen, which admits trial calls. In HalfOpen, half_open_max_calls consecutive successes close the breaker again, and any failure re-opens it with a fresh cool-down.</desc>
<defs><marker id="breaker-states-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<circle class="n" cx="24" cy="85" r="5"/>
<path class="e" d="M31,85 H44" marker-end="url(#breaker-states-ah)"/>
<path class="e" d="M244,85 H484" marker-end="url(#breaker-states-ah)"/>
<text class="t-code t-mut" x="364" y="58">failure_threshold</text>
<text class="t-mut" x="364" y="74">consecutive failures</text>
<path class="e e-dash" d="M572,114 V228" marker-end="url(#breaker-states-ah)"/>
<text class="t-code t-mut ta-e" x="558" y="158">reset_timeout</text>
<text class="t-mut ta-e" x="558" y="174">elapsed, lazy on next acquire</text>
<path class="e" d="M600,232 V118" marker-end="url(#breaker-states-ah)"/>
<text class="t-mut ta-s" x="612" y="158">any failure</text>
<text class="t-mut ta-s" x="612" y="174">fresh cool-down</text>
<path class="e e-hot" d="M488,261 H154 Q146,261 146,253 V118" marker-end="url(#breaker-states-ah)"/>
<text class="t-code t-hot" x="320" y="232">half_open_max_calls</text>
<text class="t-mut t-hot" x="320" y="248">consecutive successes close it</text>
<rect class="n-ok" x="48" y="56" width="196" height="58" rx="12"/>
<text class="t-strong" x="146" y="81">Closed</text>
<text class="t-mut" x="146" y="101">calls flow through</text>
<rect class="n-err" x="488" y="56" width="196" height="58" rx="12"/>
<text class="t-strong" x="586" y="81">Open { until }</text>
<text class="t-code t-mut" x="586" y="101">CircuitOpen, no call</text>
<rect class="n-warn" x="488" y="232" width="196" height="58" rx="12"/>
<text class="t-strong" x="586" y="257">HalfOpen</text>
<text class="t-mut" x="586" y="277">trial calls admitted</text>
</svg>
</div>
</div>
<ul>
<li><code>Closed</code>: calls flow through. Consecutive failures count toward <code>failure_threshold</code>, and a success resets the count.</li>
<li><code>Open { until }</code>: every <code>try_acquire</code> returns <code>CanoError::CircuitOpen</code> until the clock passes <code>until</code>, which is the moment it opened plus <code>reset_timeout</code>.</li>
<li><code>HalfOpen</code>: entered lazily. No background task exists; the first <code>try_acquire</code> after <code>until</code> performs the transition. Up to <code>half_open_max_calls</code> trial calls are admitted at once. That many consecutive successes close the breaker, and any failure re-opens it with a fresh cool-down.</li>
</ul>
<p>
<code>breaker.state()</code> returns the current <code>CircuitState</code> for dashboards and tests.
</p>

<h3 id="cb-policy"><a href="#cb-policy" class="anchor-link" aria-hidden="true">#</a><code>CircuitPolicy</code></h3>
<p>
A breaker is built from a <code>CircuitPolicy</code>. Each entry below shows the field, its type and
its value in <code>CircuitPolicy::default()</code>.
</p>
<dl class="key-list">
<dt><code>failure_threshold: u32</code> <small>default <code>5</code></small></dt>
<dd>Consecutive failures in <code>Closed</code> that trip the breaker to <code>Open</code>. Must be at least 1.</dd>

<dt><code>reset_timeout: Duration</code> <small>default <code>30s</code></small></dt>
<dd>How long the breaker stays <code>Open</code> before the next acquire may probe it.</dd>

<dt><code>half_open_max_calls: u32</code> <small>default <code>1</code></small></dt>
<dd>Both the cap on concurrent trial calls in <code>HalfOpen</code> and the number of consecutive successes needed to close. With a value of <code>N</code>, up to <code>N</code> probes run at once and the breaker closes only after all <code>N</code> succeed.</dd>
</dl>
<p>
<code>CircuitBreaker::new</code> panics on a policy that would break the state machine:
<code>failure_threshold == 0</code>, <code>half_open_max_calls == 0</code> (no probe could ever be
admitted), or <code>half_open_max_calls</code> above <code>u32::MAX / 2</code> (the breaker might
never close). These are programmer errors, caught before any task runs.
</p>

<h3 id="cb-permits"><a href="#cb-permits" class="anchor-link" aria-hidden="true">#</a>Permits and the RAII API</h3>
<p>
The breaker has three primitive operations. The permit type is <code>CircuitPermit</code> at the
crate root and in the prelude.
</p>
<dl class="key-list">
<dt><code>try_acquire(&amp;self) -&gt; Result&lt;Permit, CanoError&gt;</code></dt>
<dd>Asks to make a call. Returns <code>CanoError::CircuitOpen</code> while <code>Open</code>, and <code>CanoError::CircuitHalfOpenBusy</code> when <code>HalfOpen</code> has already handed out all its trial permits.</dd>

<dt><code>record_success(&amp;self, permit: Permit)</code></dt>
<dd>Reports that the call succeeded and consumes the permit.</dd>

<dt><code>record_failure(&amp;self, permit: Permit)</code></dt>
<dd>Reports that the call failed and consumes the permit.</dd>
</dl>
<p>
A permit dropped without being consumed counts as a failure. An early return, a <code>?</code>
bail-out or a panic therefore never leaves the breaker believing the call succeeded.
</p>
<p>
Each permit also carries an epoch, a counter the breaker bumps on every state transition. A permit
consumed in a later epoch is silently discarded. A slow call that started in <code>Closed</code> and
returns after the breaker tripped, cooled down and entered a new <code>HalfOpen</code> probe cannot
close the breaker, because its result was never a probe.
</p>

<h3 id="cb-via-config"><a href="#cb-via-config" class="anchor-link" aria-hidden="true">#</a>Wiring it in: <code>TaskConfig::with_circuit_breaker</code></h3>
<p>
Attach the breaker to a task's config and the retry loop does the rest. It calls
<code>try_acquire</code> before each attempt and records the outcome after the task body. An open
breaker ends the whole retry loop at once, and the <code>CircuitOpen</code> or
<code>CircuitHalfOpenBusy</code> error is returned as is, never wrapped in
<code>RetryExhausted</code>. A per-attempt <a href="../#timeouts">timeout</a> counts as a failure
too, so the breaker also guards against slow upstreams. A rejection fires the observer's
<code>on_circuit_open</code> hook, and a panic inside the hook never fails the task.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Call, Done }

struct CallUpstream {
    breaker: CircuitBreaker,
}

#[task(state = Step)]
impl CallUpstream {
    fn config(&self) -> TaskConfig {
        // Consults the breaker before each attempt; an open breaker short-circuits without retrying.
        TaskConfig::new()
            .with_fixed_retry(2, Duration::from_millis(50))
            .with_circuit_breaker(self.breaker.clone())
    }

    async fn run(&self) -> Result<Step, CanoError> {
        // Returning Err counts against the breaker's failure_threshold.
        Ok(Step::Done)
    }
}

// Clones share one state machine, so a trip from any caller protects every caller.
let breaker = CircuitBreaker::new(CircuitPolicy {
    failure_threshold: 3,
    reset_timeout: Duration::from_secs(5),
    half_open_max_calls: 1,
});
let workflow = Workflow::bare()
    .register(Step::Call, CallUpstream { breaker: breaker.clone() })
    .add_exit_state(Step::Done);
```

<div class="callout callout-warning">
<span class="callout-label">Recovery needs a fresh call</span>
<p>
A breaker that trips in the middle of a retry loop ends that loop immediately, even when the
remaining attempts and their backoff would outlast <code>reset_timeout</code>. Recovery happens on
the next dispatch of the state, after the cool-down. Retrying against an open breaker would only add
load to the dependency the breaker protects.
</p>
</div>

<h3 id="cb-manual"><a href="#cb-manual" class="anchor-link" aria-hidden="true">#</a>Driving it by hand: <code>try_acquire</code> and <code>record_*</code></h3>
<p>
Use the manual path when <code>TaskConfig</code> does not fit: one task body guards several distinct
calls, or the breaker is shared with code outside the task. Register the breaker as a
<a href="../../resources/">resource</a> (it implements <code>Resource</code> with a no-op lifecycle),
look it up by type with <code>res.get::&lt;CircuitBreaker&gt;()?</code>, and drive the permit API
yourself:
</p>

```rust
// Borrow the shared CircuitBreaker registered in Resources.
let breaker = res.get::<CircuitBreaker>()?;
let permit = breaker.try_acquire()?; // Err(CircuitOpen) or Err(CircuitHalfOpenBusy) when refused
// Consume the RAII permit explicitly; dropping an unconsumed permit counts as a failure.
match do_the_call().await {
    Ok(v)  => { breaker.record_success(permit); Ok(v) }
    Err(e) => { breaker.record_failure(permit); Err(e) }
}
```

<p>
This path skips the retry-loop short-circuit, so you decide whether a refusal aborts or retries. It
also loses the guarantee that the breaker is consulted before and updated after every attempt. Prefer
<a href="#cb-via-config"><code>TaskConfig::with_circuit_breaker</code></a> whenever a task maps to
one guarded call.
</p>

<div class="callout callout-info">
<span class="callout-label">Different layer</span>
<p>
The scheduler's flow-level <a href="../../scheduler/backoff-and-trip/"><code>Status::Tripped</code></a>
stops a scheduled flow after a streak of failed runs. It is a separate mechanism from the task-level
<code>CanoError::CircuitOpen</code>, and the two do not interact. See
<a href="../../scheduler/backoff-and-trip/">Scheduler: Backoff and trip state</a>.
</p>
</div>

<p>Runnable demos: <code>cargo run --example circuit_breaker</code> uses
<code>TaskConfig::with_circuit_breaker</code>. <code>cargo run --example circuit_breaker_manual</code>
uses the manual path: the breaker opens after a failure streak, rejects calls while open, then closes
through <code>HalfOpen</code>.</p>
</div>
