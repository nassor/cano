+++
title = "Resilience"
description = "Retries, per-attempt and total timeouts, cancellation, circuit breakers, rate limiting, bulkheads, panic safety and scheduler backoff in Cano workflows, and how they nest."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Resilience</h1>
<p class="subtitle">Retries, timeouts, cancellation, circuit breakers, rate limiting, bulkheads, panic safety and scheduler backoff.</p>

<p>
Cano's resilience features are separate, opt-in primitives that sit on the dispatch path of each
state. This page explains each one briefly and shows how they nest. The
<a href="circuit-breakers/">circuit breaker</a> and <a href="rate-limiting/">rate limiter</a> have
their own pages.
</p>
<p>
Checkpoint and resume, sagas and observers cover recovery: see <a href="../recovery/">Recovery</a>,
<a href="../saga/">Saga</a> and <a href="../observers/">Observers</a>.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#retries">Retries</a></li>
<li><a href="#timeouts">Per-attempt timeouts</a></li>
<li><a href="#workflow-total-timeout">Workflow total timeout</a></li>
<li class="toc-sub"><a href="#workflow-total-timeout-compensation">Compensation drain budget</a></li>
<li class="toc-sub"><a href="#workflow-total-timeout-observer">Observer hook</a></li>
<li class="toc-sub"><a href="#workflow-total-timeout-vs">The two timeout knobs</a></li>
<li><a href="#cancellation">Cooperative cancellation</a></li>
<li><a href="#cb-rl-guides">Circuit breakers and rate limiting</a></li>
<li><a href="#bulkhead">Bulkheads</a></li>
<li><a href="#panic-safety">Panic safety</a></li>
<li><a href="#scheduler-backoff">Scheduler backoff and trip</a></li>
<li><a href="#composing">Composing the layers</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="retries"><a href="#retries" class="anchor-link" aria-hidden="true">#</a>Retries</h2>
<p>
A task's <code>config()</code> returns a <a href="../task/configuration/"><code>TaskConfig</code></a>
whose <code>retry_mode</code> drives the retry loop. The default is no retries:
<code>TaskConfig::new()</code> runs a task once (<code>RetryMode::None</code>). Opt in with
<code>.with_exponential_retry(n)</code> (100ms base, 2.0 multiplier, 30s cap, 0.1 jitter) or
<code>.with_fixed_retry(n, delay)</code>. Both take the number of retries, so a task makes
<code>n + 1</code> attempts.
</p>
<p>
When every attempt fails, the loop returns <code>CanoError::RetryExhausted</code> wrapping the last
error. A task with a single attempt returns its raw error.
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
        // One attempt plus four retries, 200ms apart.
        TaskConfig::new().with_fixed_retry(4, Duration::from_millis(200))
    }
    async fn run(&self) -> Result<Step, CanoError> {
        // ... call the flaky thing ...
        Ok(Step::Done)
    }
}
```

<p>See <a href="../task/configuration/">Tasks: Configuration and retries</a> for the full
<code>RetryMode</code> and <code>TaskConfig</code> reference.</p>
<hr class="section-divider">

<h2 id="timeouts"><a href="#timeouts" class="anchor-link" aria-hidden="true">#</a>Per-attempt timeouts</h2>
<p>
<code>TaskConfig::with_attempt_timeout(Duration)</code> wraps each attempt in
<code>tokio::time::timeout</code>. A blown deadline produces <code>CanoError::Timeout</code>, which
the retry loop treats like any other failure: the attempt is retried, and when every attempt times
out the last timeout is wrapped in <code>CanoError::RetryExhausted</code>. With three attempts and a
2s timeout, the worst case is about 6s plus backoff.
</p>

```rust
#[task(state = Step)]
impl CallTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::new()
            .with_fixed_retry(2, Duration::from_millis(100))
            .with_attempt_timeout(Duration::from_secs(2)) // each attempt gets 2s
    }
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Done)
    }
}
```

<p>The wall-clock budget for a whole run is a different knob,
<a href="#workflow-total-timeout"><code>Workflow::with_total_timeout</code></a>.</p>
<hr class="section-divider">

<h2 id="workflow-total-timeout"><a href="#workflow-total-timeout" class="anchor-link" aria-hidden="true">#</a>Workflow total timeout</h2>
<p>
<code>Workflow::with_total_timeout(Duration)</code> sets a wall-clock budget for the entire
<code>orchestrate</code> or <a href="../recovery/"><code>resume_from</code></a> call. When the budget
elapses, the in-flight task is aborted at its next await point and the
<a href="../saga/">saga compensation stack</a> drains against its own bounded budget. The call
returns <code>CanoError::WorkflowTimeout { elapsed, limit }</code> wrapped in
<code>CanoError::WithStateContext</code>, or <code>CanoError::CompensationFailed</code> if a
<code>compensate</code> also fails.
</p>

```rust
// Reserve, Charge and Ship are compensatable tasks.
let workflow = Workflow::bare()
    .with_total_timeout(Duration::from_millis(200))
    // Optional override; the default is min(total_timeout / 2, 30s).
    .with_compensation_timeout(Duration::from_millis(50))
    .register(Step::Reserve, Reserve)
    .register(Step::Charge, Charge)
    .register(Step::Ship, Ship)
    .add_exit_state(Step::Done);
```

<p>
Retries, per-attempt timeouts and circuit breakers all run inside the per-state dispatch, which the
engine wraps in <code>tokio::time::timeout_at</code>. A task that retries forever cannot outlive the
total budget.
</p>

<h3 id="workflow-total-timeout-compensation"><a href="#workflow-total-timeout-compensation" class="anchor-link" aria-hidden="true">#</a>Compensation drain budget</h3>
<p>
When a run that has a total timeout fails, whatever the cause, the compensation stack drains against
a deadline of <code>min(total_timeout / 2, 30s)</code>: half the configured total, capped at 30
seconds. <code>Workflow::with_compensation_timeout(Duration)</code> overrides it.
</p>
<p>
Each <code>compensate</code> call runs under that deadline. Once it passes, the drain stops and keeps
every remaining entry as a <code>CanoError::OrphanedCompensation</code> (task id and output blob) for
manual recovery, followed by one timeout error.
</p>

<h3 id="workflow-total-timeout-observer"><a href="#workflow-total-timeout-observer" class="anchor-link" aria-hidden="true">#</a>Observer hook</h3>
<p>
A <code>WorkflowObserver</code> attached with <code>with_observer</code> receives one
<code>on_workflow_timeout(elapsed, limit)</code> call when the budget fires, before the compensation
drain runs. With the <a href="../tracing/"><code>tracing</code></a> feature,
<code>TracingObserver</code> re-emits it as a <code>WARN</code> event
<code>"workflow total timeout exceeded"</code> with <code>elapsed_ms</code> and <code>limit_ms</code>
fields under the <code>cano::observer</code> target. The hook reference is in
<a href="../observers/#events">Observers: Lifecycle events</a>.
</p>

<h3 id="workflow-total-timeout-vs"><a href="#workflow-total-timeout-vs" class="anchor-link" aria-hidden="true">#</a>The two timeout knobs</h3>
<div class="diagram-frame">
<p class="diagram-label">Two knobs, two scopes: the total budget wins</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 232" role="img" aria-labelledby="resilience-timeouts-title resilience-timeouts-desc">
<title id="resilience-timeouts-title">Attempt timeout inside a 7 second total budget</title>
<desc id="resilience-timeouts-desc">A timeline for a workflow with a 7 second total budget. State A finishes in 1 second. State B then runs attempt 1 under a 2 second attempt timeout, which expires at 3 seconds and is retried after a short backoff. Attempt 2 also expires, at 5.5 seconds. Attempt 3 starts at 6 seconds and is cut short when the total budget fires at 7 seconds, before its own 8 second deadline. The workflow ends with WorkflowTimeout.</desc>
<path class="e e-dim e-dash" d="M664,34 V148"/>
<text class="t-code t-err" x="664" y="22">WorkflowTimeout</text>
<text class="t-strong ta-s" x="16" y="54">Total budget</text>
<rect class="band" x="132" y="40" width="532" height="26" rx="6"/>
<text class="t-code t-mut" x="398" y="54">with_total_timeout(7s)</text>
<text class="t-strong ta-s" x="16" y="108">Workflow</text>
<text class="t-code t-err" x="360" y="80">Timeout</text>
<text class="t-code t-err" x="550" y="80">Timeout</text>
<text class="t-mut" x="626" y="80">attempt 3</text>
<text class="t-mut t-warn" x="702" y="80">cut short</text>
<rect class="band-hot" x="132" y="94" width="76" height="26" rx="6"/>
<text class="t-mut" x="170" y="108">State A</text>
<rect class="band-hot" x="208" y="94" width="152" height="26" rx="6"/>
<text class="t-mut" x="284" y="108">attempt 1</text>
<rect class="band" x="360" y="94" width="38" height="26" rx="6"/>
<rect class="band-hot" x="398" y="94" width="152" height="26" rx="6"/>
<text class="t-mut" x="474" y="108">attempt 2</text>
<rect class="band" x="550" y="94" width="38" height="26" rx="6"/>
<rect class="band-hot" x="588" y="94" width="76" height="26" rx="6"/>
<rect class="n-ghost" x="664" y="94" width="76" height="26" rx="6"/>
<line class="axis" x1="132" y1="140" x2="740" y2="140"/>
<line class="tick" x1="132" y1="140" x2="132" y2="146"/>
<line class="tick" x1="208" y1="140" x2="208" y2="146"/>
<line class="tick" x1="284" y1="140" x2="284" y2="146"/>
<line class="tick" x1="360" y1="140" x2="360" y2="146"/>
<line class="tick" x1="436" y1="140" x2="436" y2="146"/>
<line class="tick" x1="512" y1="140" x2="512" y2="146"/>
<line class="tick" x1="588" y1="140" x2="588" y2="146"/>
<line class="tick" x1="664" y1="140" x2="664" y2="146"/>
<line class="tick" x1="740" y1="140" x2="740" y2="146"/>
<text class="t-mut" x="132" y="164">0s</text>
<text class="t-mut" x="208" y="164">1s</text>
<text class="t-mut" x="284" y="164">2s</text>
<text class="t-mut" x="360" y="164">3s</text>
<text class="t-mut" x="436" y="164">4s</text>
<text class="t-mut" x="512" y="164">5s</text>
<text class="t-mut" x="588" y="164">6s</text>
<text class="t-mut" x="664" y="164">7s</text>
<text class="t-mut" x="740" y="164">8s</text>
<text class="t-mut" x="436" y="192">attempt_timeout(2s) fires twice and each attempt is retried after a short backoff (grey gaps)</text>
<text class="t-mut" x="436" y="210">the 7s budget then aborts attempt 3 mid-flight: no retry policy outlives the total budget</text>
</svg>
</div>
</div>
<p>
The two knobs differ in scope and in what follows expiry:
</p>
<dl class="key-list">
<dt><code>TaskConfig::with_attempt_timeout</code> <small>scope: one attempt of one task</small></dt>
<dd>Expiry gives <code>CanoError::Timeout</code>, retried like any failure; a final timeout becomes <code>RetryExhausted</code>. Compensation then drains with no overall deadline unless a total timeout is also set.</dd>

<dt><code>Workflow::with_total_timeout</code> <small>scope: the whole <code>orchestrate</code> or <code>resume_from</code> call</small></dt>
<dd>The in-flight task is aborted and the call returns <code>WorkflowTimeout</code>, wrapped in <code>WithStateContext</code>. Compensation drains under <code>with_compensation_timeout</code> or the default <code>min(total_timeout / 2, 30s)</code>.</dd>
</dl>
<p>
Use <code>with_attempt_timeout</code> to bound one call and <code>with_total_timeout</code> for a
workflow-wide budget; they compose. To stop a run on an external signal instead of a deadline, use
<a href="#cancellation">cooperative cancellation</a>.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_total_timeout</code>. A three-step saga whose
shipping step overruns the budget: the timeout fires, the earlier compensations run in reverse, and
the final error is the wrapped <code>WorkflowTimeout</code>.</p>
</div>
<hr class="section-divider">

<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>Cooperative cancellation</h2>
<p>
A total timeout aborts a run on a deadline. Cancellation aborts it on a signal you control: a
shutdown handler, a stop button, a parent task giving up.
<code>Workflow::orchestrate_with_token(start, &amp;token)</code> and
<code>resume_from_with_token(id, &amp;token)</code> take a <code>CancellationToken</code>, a re-export
of <code>tokio_util::sync::CancellationToken</code>. Calling <code>token.cancel()</code> aborts the
in-flight task at its next await point, drains the <a href="../saga/">saga compensation stack</a>, and
returns <code>CanoError::Cancelled</code> wrapped in <code>CanoError::WithStateContext</code>, or
<code>CanoError::CompensationFailed</code> if a <code>compensate</code> also fails.
</p>

```rust
use cano::CancellationToken;
use std::time::Duration;

let token = CancellationToken::new();

// Cancel from anywhere: a signal handler, a sibling task, a timer.
tokio::spawn({
    let token = token.clone();
    async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        token.cancel(); // idempotent; clones share the signal
    }
});

let result = workflow.orchestrate_with_token(Step::Reserve, &token).await;
assert!(matches!(result, Err(e) if e.is_cancelled()));
```

<p>
<code>CanoError::is_cancelled()</code> reports a cancelled run. Plain
<code>workflow.orchestrate(start)</code> takes no token and skips the cancellation check entirely.
</p>

<div class="callout callout-warning">
<p><strong>Cancellation is cooperative.</strong> The engine drops the running task's future at its next
<code>.await</code>. A task spinning in a synchronous loop with no <code>.await</code> is not
interrupted until it yields, so a cancellable task body should <code>.await</code> periodically.</p>
</div>

<h3 id="cancellation-saga"><a href="#cancellation-saga" class="anchor-link" aria-hidden="true">#</a>Saga safety</h3>
<p>
The engine never interrupts a <a href="../saga/">compensatable task</a> mid-run. Aborting it after a
side effect committed but before its <code>Output</code> reached the compensation stack would orphan
that side effect. A <code>CompensatableTask</code> therefore runs to completion and records its
rollback entry, and the cancellation is honoured at the next state boundary, which drains the
complete stack. The drain itself is uncancellable: a cancel that lands during rollback does not stop
the remaining compensators.
</p>

<h3 id="cancellation-observer"><a href="#cancellation-observer" class="anchor-link" aria-hidden="true">#</a>Observer hook and precedence</h3>
<p>
A <code>WorkflowObserver</code> receives one <code>on_cancelled(state)</code> call when the cancel is
observed, before the drain runs. <code>TracingObserver</code> re-emits it as a <code>WARN</code> event
and <code>MetricsObserver</code> increments <code>cano_observed_cancellations_total</code>. When
cancellation and <code>with_total_timeout</code> fire together, cancellation wins.
</p>

<div class="callout callout-tip">
<p>The scheduler builds on this. <a href="../scheduler/#cancellation"><code>RunningScheduler::cancel_flow(id)</code></a>
cancels an in-flight scheduled run, and <code>stop()</code> lets in-flight runs finish (the drain
timeout), cancels the ones still running (their sagas roll back), then aborts what remains after the
cancel timeout.</p>
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_cancellation</code>. A three-step saga where a
sibling task cancels the shipping step mid-flight: the earlier compensations run in reverse and the
final error is the wrapped <code>Cancelled</code>.</p>
</div>
<hr class="section-divider">

<h2 id="cb-rl-guides"><a href="#cb-rl-guides" class="anchor-link" aria-hidden="true">#</a>Circuit breakers and rate limiting</h2>
<p>Two primitives for dependencies have their own pages:</p>
<ul>
<li><a href="circuit-breakers/">Circuit breakers</a> stop calls to a dependency that is down, before the retry loop runs.</li>
<li><a href="rate-limiting/">Rate limiting</a> paces or sheds calls to a rate-sensitive dependency, including multi-tier limits.</li>
</ul>
<hr class="section-divider">

<h2 id="bulkhead"><a href="#bulkhead" class="anchor-link" aria-hidden="true">#</a>Bulkheads</h2>
<p>
A <a href="../split-join/">split/join</a> state runs its tasks in parallel, which can mean 500
connections to a database that handles 50. <a href="../split-join/#bulkhead"><code>JoinConfig::with_bulkhead(n)</code></a>
gates the task bodies on a shared <code>Semaphore</code>, so at most <code>n</code> run at once and
the rest queue. All tasks are still spawned and the join strategy is unchanged. A bulkhead of
<code>0</code> is a configuration error that <code>validate()</code> reports before any run starts.
</p>

```rust
let join = JoinConfig::new(JoinStrategy::All, S::Join).with_bulkhead(8); // at most 8 at a time
let workflow = Workflow::bare()
    .register_split(S::Fan, (0..200).map(|_| W).collect(), join)
    .add_exit_state(S::Done);
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example split_bulkhead</code>. Eight split tasks behind
<code>with_bulkhead(2)</code>; the start and end timestamps show the limit.</p>
</div>
<hr class="section-divider">

<h2 id="panic-safety"><a href="#panic-safety" class="anchor-link" aria-hidden="true">#</a>Panic safety</h2>
<p>
A <code>panic!</code> inside a task body, or an <code>.unwrap()</code> that fires, does not unwind
through the workflow engine. The dispatcher wraps each task in <code>catch_unwind</code> and turns the
panic into <code>CanoError::TaskExecution("panic: &lt;message&gt;")</code>, which then follows the
normal retry, error and compensation path. Split tasks are wrapped per spawned task. There is nothing
to configure. Observer hooks, including <code>on_retry</code> and <code>on_circuit_open</code>, are
isolated the same way, so a panicking observer never fails the task.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example panic_safety</code>. A task that <code>panic!</code>s
mid-workflow: the engine returns <code>CanoError::TaskExecution</code> and the preceding compensatable
step's rollback still runs.</p>
</div>
<hr class="section-divider">

<h2 id="scheduler-backoff"><a href="#scheduler-backoff" class="anchor-link" aria-hidden="true">#</a>Scheduler backoff and trip</h2>
<p class="feature-tag">Behind the <code>scheduler</code> feature gate.</p>
<p>
The primitives above act per dispatch. A workflow that runs on a timer has a second, flow-level
layer: a per-flow <code>BackoffPolicy</code> lengthens the gap between failed runs and can trip a flow
that keeps failing, so it stops firing until <code>RunningScheduler::reset_flow(id)</code> clears it.
The flow reports <code>Status::Backoff { .. }</code> or <code>Status::Tripped { .. }</code>. See
<a href="../scheduler/backoff-and-trip/">Scheduler: Backoff and trip state</a>, or run
<code>cargo run --example scheduler_backoff --features scheduler</code>.
</p>
<hr class="section-divider">

<h2 id="composing"><a href="#composing" class="anchor-link" aria-hidden="true">#</a>Composing the layers</h2>
<p>
The primitives nest in a fixed order. A state that talks to a flaky external service passes through
all of them on each dispatch:
</p>
<div class="diagram-frame">
<p class="diagram-label">One dispatch, four nested layers, and the error each one surfaces</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 392" role="img" aria-labelledby="resilience-layers-title resilience-layers-desc">
<title id="resilience-layers-title">Nesting of resilience layers in one task dispatch</title>
<desc id="resilience-layers-desc">One task dispatch passes through four nested layers. The workflow total timeout wraps the retry loop. Inside the retry loop, each attempt first asks the circuit breaker for a permit. Then the per-attempt timeout wraps the task body, where the rate limiter permit is taken before the upstream call. An open breaker ends the dispatch at once with CircuitOpen, without running the task. A Timeout or any task error is retried after a backoff sleep, and exhausting the attempts returns RetryExhausted. A successful call returns the next state and the workflow advances. If the total timeout fires first, the dispatch aborts with WorkflowTimeout.</desc>
<defs><marker id="resilience-layers-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n-ghost" x="22" y="12" width="736" height="278" rx="14"/>
<text class="t-strong ta-s" x="36" y="34">Workflow total timeout</text>
<text class="t-code t-mut ta-s" x="228" y="34">timeout_at(start + limit)</text>
<text class="t-code t-err ta-s" x="432" y="34">→ WorkflowTimeout</text>
<text class="t-mut ta-s" x="572" y="34">aborts mid-flight</text>
<rect class="n-ghost" x="44" y="48" width="700" height="222" rx="12"/>
<text class="t-strong ta-s" x="58" y="70">retry loop</text>
<text class="t-code t-mut ta-s" x="152" y="70">run_with_retries, up to max_attempts</text>
<rect class="n-ghost" x="280" y="84" width="444" height="112" rx="12"/>
<text class="t-code t-mut ta-s" x="298" y="104">tokio::time::timeout(attempt_timeout)</text>
<text class="t-mut ta-e" x="712" y="104">your task body</text>
<circle class="n" cx="10" cy="151" r="5"/>
<path class="e" d="M17,151 H64" marker-end="url(#resilience-layers-ah)"/>
<path class="e" d="M240,151 H300" marker-end="url(#resilience-layers-ah)"/>
<path class="e" d="M480,151 H516" marker-end="url(#resilience-layers-ah)"/>
<path class="e e-dim" d="M502,196 V238 Q502,246 494,246 H162 Q154,246 154,238 V182" marker-end="url(#resilience-layers-ah)"/>
<text class="t-code t-err" x="328" y="212">Timeout · any task Err</text>
<text class="t-mut" x="328" y="234">retried per RetryMode, then a backoff sleep</text>
<path class="e" d="M110,178 V308" marker-end="url(#resilience-layers-ah)"/>
<path class="e" d="M305,246 V308" marker-end="url(#resilience-layers-ah)"/>
<path class="e e-hot" d="M610,178 V308" marker-end="url(#resilience-layers-ah)"/>
<text class="t-mut t-hot ta-s" x="622" y="250">success</text>
<rect class="n" x="68" y="124" width="172" height="54" rx="10"/>
<text class="t-strong" x="154" y="145">circuit breaker</text>
<text class="t-code t-mut" x="154" y="165">try_acquire()</text>
<rect class="n" x="304" y="124" width="176" height="54" rx="10"/>
<text class="t-strong" x="392" y="145">rate limiter</text>
<text class="t-code t-mut" x="392" y="165">acquire()</text>
<rect class="n-hot" x="520" y="124" width="180" height="54" rx="10"/>
<text class="t-strong" x="610" y="145">upstream call</text>
<text class="t-mut" x="610" y="165">the flaky thing</text>
<rect class="n-err" x="30" y="312" width="160" height="44" rx="10"/>
<text class="t-code" x="110" y="335">CircuitOpen</text>
<text class="t-mut" x="110" y="374">no attempt, no retry burned</text>
<rect class="n-err" x="220" y="312" width="170" height="44" rx="10"/>
<text class="t-code" x="305" y="335">RetryExhausted</text>
<text class="t-mut" x="305" y="374">after max_attempts</text>
<rect class="n-ok" x="530" y="312" width="160" height="44" rx="10"/>
<text class="t-code" x="610" y="335">Ok(next state)</text>
<text class="t-mut" x="610" y="374">workflow advances</text>
</svg>
</div>
</div>
<p>From the outside in:</p>
<ol>
<li>The workflow total timeout bounds the whole dispatch.</li>
<li>Retries with backoff ride out transient faults.</li>
<li>The circuit breaker, shared by every task that calls the service, rejects calls while it is down.</li>
<li>The per-attempt timeout bounds a slow call.</li>
<li>The rate limiter, also shared, keeps calls under the service's quota. It runs inside the task body, so time spent parked in <code>acquire()</code> counts against the attempt timeout.</li>
</ol>
<p>
Around the dispatch, make a call that mutates something a <a href="../saga/">compensatable task</a>
so a later failure can undo it. Attach a <a href="../recovery/">checkpoint store</a> so a crash
mid-workflow keeps its progress, and a <a href="../observers/"><code>WorkflowObserver</code></a> (or
<code>TracingObserver</code>) to see retries, circuit opens and checkpoints as they happen. The
<code>circuit_breaker</code>, <code>workflow_recovery</code>, <code>saga_payment</code>,
<code>workflow_observer</code> and <code>observer_metrics</code> examples each exercise one of these.
</p>
</div>
