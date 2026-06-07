+++
title = "Resilience"
description = "The resilience guide for Cano workflows: the full circuit breaker treatment plus retries, per-attempt timeouts, rate limiting, bulkheads, panic safety, and scheduler backoff — how each primitive works and how they compose."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Resilience</h1>
<p class="subtitle">Recover from transient faults — retries, timeouts, circuit breakers, rate limiting, bulkheads, panic safety, scheduler backoff.</p>

<p>
"Resilient" in Cano's tagline isn't a vibe — it's a set of concrete, composable primitives that
sit on the FSM dispatch path. Every one of them is <strong>opt-in</strong>: a workflow that wires
none of them up pays nothing, and the hot path stays allocation-light. This is the guide: the
<a href="circuit-breakers/">circuit breaker</a> and <a href="rate-limiting/">rate limiter</a> get their
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
<li class="toc-sub"><a href="#workflow-total-timeout-compensation">Compensation drain budget</a></li>
<li class="toc-sub"><a href="#workflow-total-timeout-observer">Observer hook</a></li>
<li class="toc-sub"><a href="#workflow-total-timeout-vs">The two timeout knobs</a></li>
<li><a href="#cancellation">Cooperative Cancellation</a></li>
<li><a href="#cb-rl-guides">Circuit Breakers &amp; Rate Limiting</a></li>
<li><a href="#bulkhead">Bulkheads (split concurrency)</a></li>
<li><a href="#panic-safety">Panic Safety</a></li>
<li><a href="#scheduler-backoff">Scheduler Backoff &amp; Trip</a></li>
<li><a href="#composing">Composing It All</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="retries"><a href="#retries" class="anchor-link" aria-hidden="true">#</a>Retries</h2>
<p>
A task's <code>config()</code> returns a <a href="../task/configuration/"><code>TaskConfig</code></a>
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

<p>See <a href="../task/configuration/">Tasks → Configuration &amp; Retries</a> for the full
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
orchestration — see below). The full <code>TaskConfig</code> /
<code>RetryMode</code> API — including how attempt timeouts compose with each retry mode — lives in
<a href="../task/configuration/">Tasks → Configuration &amp; Retries</a>.</p>
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

<h3 id="workflow-total-timeout-vs"><a href="#workflow-total-timeout-vs" class="anchor-link" aria-hidden="true">#</a>The two timeout knobs</h3>
<table class="styled-table">
<thead><tr><th>API</th><th>Scope</th><th>On expiry</th><th>Compensation drain</th></tr></thead>
<tbody>
<tr><td><code>TaskConfig::with_attempt_timeout</code></td><td>One attempt of one task</td><td><code>CanoError::Timeout</code> — retried like any other failure; final timeout becomes <code>RetryExhausted</code></td><td>Triggered like any other terminal task error (unbounded)</td></tr>
<tr><td><code>Workflow::with_total_timeout</code></td><td>The entire <code>orchestrate</code> / <code>resume_from</code> call</td><td>In-flight task aborted; <code>CanoError::WorkflowTimeout</code> (wrapped in <code>WithStateContext</code>)</td><td>Bounded by <code>with_compensation_timeout</code> or the default <code>min(remaining/2, 30s)</code></td></tr>
</tbody>
</table>
<p>
Reach for <code>with_attempt_timeout</code> to bound a single call and <code>with_total_timeout</code>
for a workflow-wide budget; they compose. To stop a run on an external signal rather than a deadline,
use <a href="#cancellation">cooperative cancellation</a>.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_total_timeout</code> — a 3-step saga where the
shipping step overruns the budget; the timeout fires, the prior steps' compensations run in reverse,
and the final error is the wrapped <code>WorkflowTimeout</code>.</p>
</div>
<hr class="section-divider">

<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>Cooperative Cancellation</h2>
<p>
Where a total timeout aborts a run on a <em>deadline</em>, cancellation aborts it on a <em>signal</em>
you control — a shutdown handler, a user "stop" button, a parent task giving up.
<code>Workflow::orchestrate(start, token)</code> (and <code>resume_from(id, token)</code>) always take
a <code>CancellationToken</code>; firing the paired <code>CancellationHandle</code> aborts the
in-flight task at its next await point, drains the
<a href="../saga/">saga compensation stack</a>, and returns <code>CanoError::Cancelled</code> wrapped
in <code>CanoError::WithStateContext</code> (or <code>CanoError::CompensationFailed</code> if a
<code>compensate</code> also fails).
</p>

```rust
use cano::CancellationToken;

let (handle, token) = CancellationToken::new();

// Cancel from anywhere — a signal handler, a sibling task, a timer:
let canceller = tokio::spawn(async move {
    shutdown_signal().await;
    handle.cancel(); // idempotent; the handle is Clone, so many owners can trigger it
});

let result = workflow.orchestrate(Step::Reserve, token).await;
assert!(matches!(result, Err(e) if e.category() == "cancelled"));
```

<p>
To opt a run out of cancellation, pass <code>CancellationToken::disabled()</code> instead of a live
token: <code>workflow.orchestrate(start, CancellationToken::disabled())</code>. A disabled token
never fires and is zero-cost — the cancellation <code>select!</code> is skipped entirely.
</p>

<div class="callout callout-warning">
<p><strong>Cancellation is cooperative.</strong> The engine drops the running task's future at its next
<code>.await</code>. A task spinning in a tight synchronous/CPU loop with no <code>.await</code> is not
interrupted until it next yields — design long-running task bodies to <code>.await</code> periodically
if they must be cancellable.</p>
</div>

<h3 id="cancellation-saga"><a href="#cancellation-saga" class="anchor-link" aria-hidden="true">#</a>Saga safety</h3>
<p>
A <a href="../saga/">compensatable task</a> is <strong>never</strong> interrupted mid-run. Aborting it
after an in-task side effect committed but before its <code>Output</code> reached the compensation
stack would orphan that side effect with nothing to roll back. So a <code>CompensatableTask</code>
always runs to completion (recording its rollback entry); the cancellation is honoured at the
<em>next</em> state boundary, which then drains the now-complete stack. The compensation drain itself
is uncancellable — a cancel that lands during rollback does not abort the remaining compensators.
</p>

<h3 id="cancellation-observer"><a href="#cancellation-observer" class="anchor-link" aria-hidden="true">#</a>Observer hook &amp; precedence</h3>
<p>
A <code>WorkflowObserver</code> receives one <code>on_cancelled(state)</code> call when the cancel is
observed, before the drain runs; <code>TracingObserver</code> re-emits it as a <code>WARN</code> event
and <code>MetricsObserver</code> increments <code>cano_observed_cancellations_total</code>. Against
<code>with_total_timeout</code>, cancellation wins: it is checked deterministically at each state
boundary and biased ahead of the per-state budget mid-task.
</p>

<div class="callout callout-tip">
<p>The scheduler builds on this: <a href="../scheduler/#cancellation"><code>RunningScheduler::cancel_flow(id)</code></a>
cancels an in-flight scheduled run, and graceful <code>stop()</code> cancels every in-flight flow
(rolling back their sagas) instead of waiting for them to finish.</p>
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_cancellation</code> — a 3-step saga where a
sibling task cancels the shipping step mid-flight; the prior steps' compensations run in reverse and
the final error is the wrapped <code>Cancelled</code>.</p>
</div>
<hr class="section-divider">

<h2 id="cb-rl-guides"><a href="#cb-rl-guides" class="anchor-link" aria-hidden="true">#</a>Circuit Breakers &amp; Rate Limiting</h2>
<p>Two of the most-used resilience primitives have their own dedicated pages:</p>
<ul>
<li><a href="circuit-breakers/">Circuit Breakers</a> — stop calling a dependency that is down, before the retry loop even runs.</li>
<li><a href="rate-limiting/">Rate Limiting</a> — pace or shed calls to a rate-sensitive dependency, including multi-tier limits.</li>
</ul>
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
own page: <a href="../scheduler/backoff-and-trip/">Scheduler → Backoff &amp; Trip State</a> (runnable
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
