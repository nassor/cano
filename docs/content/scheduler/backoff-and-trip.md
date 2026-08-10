+++
title = "Scheduler Backoff, Trip State & Recovery"
description = "How the Cano scheduler backs off and trips a repeatedly-failing flow, overriding the BackoffPolicy, the Status variants, and recovering a tripped flow with reset_flow."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Scheduler Backoff &amp; Trip State</h1>
<p class="subtitle">What happens when a scheduled flow keeps failing — and how to recover it.</p>

<p>
See <a href="../">Scheduler</a> for scheduling strategies and lifecycle. This page covers what the
scheduler does when a flow fails repeatedly.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#backoff-policy">Overriding the Default Policy</a></li>
<li><a href="#status-variants">Status Variants</a></li>
<li><a href="#recovery">Recovery via <code>reset_flow</code></a></li>
</ol>
</nav>
<hr class="section-divider">

<p>
A flow that fails repeatedly shouldn't keep re-firing on its base schedule, so <strong>every flow has a
<code>BackoffPolicy</code></strong>. After a failure the scheduler parks the flow in <code>Status::Backoff</code>
for a growing delay; with a <code>streak_limit</code> set it eventually <code>Tripped</code>s and stops
dispatching until you intervene. Each flow starts with <code>BackoffPolicy::default()</code> — call
<code>set_backoff</code> to use a different one.
</p>

<div class="callout callout-info">
<div class="callout-label">Heads-up: failure delays are at least 1s by default</div>
<p>
Because <code>BackoffPolicy::default()</code> has a <strong>1s</strong> initial delay and is applied to
<em>every</em> flow, a flow that fails waits ~1s before its next attempt (the <code>Every</code> loop sleeps
<code>max(interval, next_eligible - now)</code>) — even if its base interval is shorter. If you run a flow
on a sub-second interval and want fast retries after a failure, lower
<code>BackoffPolicy { initial: … }</code> via <code>set_backoff</code>.
</p>
</div>

<div class="callout callout-warning">
<div class="callout-label">Distinct from CircuitBreaker</div>
<p>
Flow-level <code>Tripped</code> is scoped to the scheduler and is separate from the task-level
<code>CanoError::CircuitOpen</code> emitted by a <a href="../../resilience/circuit-breakers/"><code>CircuitBreaker</code></a>.
The breaker gates a single task's call to a dependency; this policy gates the scheduler from re-firing
an entire flow.
</p>
</div>

<h3 id="backoff-policy"><a href="#backoff-policy" class="anchor-link" aria-hidden="true">#</a>Overriding the Default Policy</h3>
<p>
Register the workflow normally, then call <code>set_backoff</code> <strong>before</strong> <code>start()</code>.
The policy controls the initial delay after the first failure, the multiplier applied per
additional consecutive failure, a hard cap on the computed delay, jitter, and an optional streak limit.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FlowState { Start, Done }

#[derive(Clone)]
struct NoopTask;

#[task(state = FlowState)]
impl NoopTask {
    async fn run_bare(&self) -> Result<TaskResult<FlowState>, CanoError> {
        Ok(TaskResult::Single(FlowState::Done))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler: Scheduler<FlowState> = Scheduler::new();

    let workflow = Workflow::new(Resources::new())
        .register(FlowState::Start, NoopTask)
        .add_exit_state(FlowState::Done);

    scheduler.every(
        "flaky",
        workflow,
        FlowState::Start,
        Duration::from_millis(200),
    )?;

    scheduler.set_backoff(
        "flaky",
        BackoffPolicy {
            initial: Duration::from_millis(300),
            multiplier: 2.0,
            max_delay: Duration::from_secs(2),
            jitter: 0.1,
            streak_limit: Some(3),
        },
    )?;

    let running = scheduler.start().await?;
    running.wait().await?;
    Ok(())
}

```

<p>
Computed delay is <code>initial * multiplier^(streak-1)</code>, capped at <code>max_delay</code>, then
multiplied by a random factor in <code>1 ± jitter</code>. The <code>Every</code> loop's sleep extends to
<code>max(interval, next_eligible - now)</code>, and the <code>Cron</code> loop suppresses ticks inside the
backoff window. <code>BackoffPolicy::default()</code> gives 1s initial, 2.0× multiplier, 5min cap,
0.1 jitter, and <strong>no trip limit</strong>. Use <code>BackoffPolicy::with_trip(n)</code> to ask for a
trip after <code>n</code> consecutive failures.
</p>
<div class="diagram-frame">
<p class="diagram-label">Backoff timeline for a flaky flow</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 860 196" role="img" style="max-width: 860px">
<title>A flaky flow on a 1s interval: each consecutive failure parks it for a longer backoff window — 2s, 4s, 8s, then 8s capped at max_delay — and the first success clears the streak so the 1s cadence resumes.</title>
<defs><marker id="bt-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-code t-mut ta-s" x="16" y="28">initial 2s · multiplier 2.0 · max_delay 8s · jitter 0</text>
<text class="t-mut t-ok ta-e" x="816" y="28">success → failure_streak 0, next_eligible cleared</text>
<text class="t-mut ta-e" x="132" y="50">streak</text>
<text class="t-mut t-err" x="191" y="50">1</text>
<text class="t-mut t-err" x="241" y="50">2</text>
<text class="t-mut t-err" x="343" y="50">3</text>
<text class="t-mut t-err" x="545" y="50">4</text>
<text class="t-mut t-ok" x="748" y="50">0</text>
<text class="t-strong t-ok" x="140" y="68">✓</text>
<text class="t-strong t-ok" x="165" y="68">✓</text>
<text class="t-strong t-err" x="191" y="68">✗</text>
<text class="t-strong t-err" x="241" y="68">✗</text>
<text class="t-strong t-err" x="343" y="68">✗</text>
<text class="t-strong t-err" x="545" y="68">✗</text>
<text class="t-strong t-ok" x="748" y="68">✓</text>
<text class="t-strong t-ok" x="773" y="68">✓</text>
<text class="t-strong t-ok" x="799" y="68">✓</text>
<text class="t-strong ta-s" x="16" y="90">flaky</text>
<text class="t-code t-mut ta-s" x="16" y="106">every(1s)</text>
<rect class="band" x="197" y="78" width="38" height="26" rx="6"/>
<rect class="band" x="247" y="78" width="90" height="26" rx="6"/>
<rect class="band" x="349" y="78" width="190" height="26" rx="6"/>
<rect class="band" x="551" y="78" width="191" height="26" rx="6"/>
<text class="t-mut" x="216" y="92">2s</text>
<text class="t-mut" x="292" y="92">4s</text>
<text class="t-mut" x="444" y="92">8s</text>
<text class="t-mut" x="646" y="92">8s · max_delay cap</text>
<rect class="band-hot" x="134" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="159" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="185" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="235" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="337" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="539" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="742" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="767" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="793" y="78" width="12" height="26" rx="4"/>
<line class="axis" x1="140" y1="126" x2="812" y2="126"/>
<path class="e e-dim" d="M812,126 H830" marker-end="url(#bt-ah)"/>
<line class="tick" x1="140" y1="126" x2="140" y2="132"/>
<line class="tick" x1="241" y1="126" x2="241" y2="132"/>
<line class="tick" x1="343" y1="126" x2="343" y2="132"/>
<line class="tick" x1="444" y1="126" x2="444" y2="132"/>
<line class="tick" x1="545" y1="126" x2="545" y2="132"/>
<line class="tick" x1="647" y1="126" x2="647" y2="132"/>
<line class="tick" x1="748" y1="126" x2="748" y2="132"/>
<text class="t-mut" x="140" y="150">0s</text>
<text class="t-mut" x="241" y="150">4s</text>
<text class="t-mut" x="343" y="150">8s</text>
<text class="t-mut" x="444" y="150">12s</text>
<text class="t-mut" x="545" y="150">16s</text>
<text class="t-mut" x="647" y="150">20s</text>
<text class="t-mut" x="748" y="150">24s</text>
<text class="t-mut" x="483" y="176">failures push next_eligible out exponentially — capped at max_delay, cleared by the first success</text>
</svg>
</div>
</div>

<h3 id="status-variants"><a href="#status-variants" class="anchor-link" aria-hidden="true">#</a>Status Variants</h3>
<p>
<code>Status</code> is <code>#[non_exhaustive]</code> — external <code>match</code> arms must include
a wildcard. The variants are:
</p>
<ul>
<li><code>Idle</code> — registered, never run or finished cleanly.</li>
<li><code>Running</code> — currently executing.</li>
<li><code>Completed</code> — last run reached an exit state.</li>
<li><code>Backoff { until, streak, last_error }</code> — last run errored; the flow is waiting until
<code>until</code> before its next dispatch, per its <code>BackoffPolicy</code>.</li>
<li><code>Tripped { streak, last_error }</code> — streak reached <code>streak_limit</code>; the scheduler
will not dispatch this flow again until <code>reset_flow</code> is called.</li>
</ul>
<div class="diagram-frame">
<p class="diagram-label">Flow status: backoff, trip, and recovery</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 356" role="img">
<title>Scheduler flow status machine: a dispatched flow runs, success marks it Completed and clears the streak, a failure parks it in Backoff until the delay elapses, and a failure that reaches streak_limit trips it until reset_flow returns it to Idle.</title>
<defs><marker id="trip-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M636,48 V32 Q636,24 628,24 H374 Q366,24 366,32 V44" marker-end="url(#trip-ah)"/>
<text class="t-mut" x="496" y="41">next tick</text>
<circle class="n" cx="20" cy="64" r="5"/>
<path class="e" d="M27,64 H40" marker-end="url(#trip-ah)"/>
<path class="e" d="M194,77 H282" marker-end="url(#trip-ah)"/>
<text class="t-mut" x="238" y="63">dispatch</text>
<path class="e" d="M446,77 H542" marker-end="url(#trip-ah)"/>
<text class="t-mut t-ok" x="494" y="63">success</text>
<path class="e" d="M320,106 V152 Q320,160 312,160 H148 Q140,160 140,168 V228" marker-end="url(#trip-ah)"/>
<text class="t-mut t-err" x="230" y="134">failure</text>
<text class="t-mut" x="230" y="150">streak &lt; streak_limit</text>
<path class="e" d="M180,232 V196 Q180,188 188,188 H392 Q400,188 400,180 V110" marker-end="url(#trip-ah)"/>
<text class="t-mut" x="300" y="206">until elapsed · next dispatch</text>
<path class="e" d="M436,106 V202 Q436,210 444,210 H588 Q596,210 596,218 V228" marker-end="url(#trip-ah)"/>
<text class="t-mut t-err ta-s" x="450" y="176">failure</text>
<text class="t-mut ta-s" x="450" y="192">streak reaches streak_limit</text>
<path class="e e-hot" d="M700,308 V330 Q700,338 692,338 H28 Q20,338 20,330 V102 Q20,94 28,94 H40" marker-end="url(#trip-ah)"/>
<text class="t-code t-hot ta-e" x="316" y="326">reset_flow(id)</text>
<text class="t-mut t-hot ta-s" x="324" y="326">· clears the streak, un-trips the flow</text>
<rect class="n" x="44" y="48" width="150" height="58" rx="12"/>
<text class="t-strong" x="119" y="73">Idle</text>
<text class="t-mut" x="119" y="93">awaiting dispatch</text>
<rect class="n" x="286" y="48" width="160" height="58" rx="12"/>
<text class="t-strong" x="366" y="73">Running</text>
<text class="t-mut" x="366" y="93">workflow executing</text>
<rect class="n-ok" x="546" y="48" width="180" height="58" rx="12"/>
<text class="t-strong" x="636" y="73">Completed</text>
<text class="t-mut" x="636" y="93">reached an exit state</text>
<text class="t-code t-mut" x="636" y="130">streak = 0 · next_eligible = None</text>
<rect class="n-warn" x="40" y="232" width="250" height="76" rx="12"/>
<text class="t-strong" x="165" y="256">Backoff</text>
<text class="t-code t-mut" x="165" y="276">{ until, streak, last_error }</text>
<text class="t-mut" x="165" y="295">parked until the delay elapses</text>
<rect class="n-err" x="476" y="232" width="250" height="76" rx="12"/>
<text class="t-strong" x="601" y="256">Tripped</text>
<text class="t-code t-mut" x="601" y="276">{ streak, last_error }</text>
<text class="t-mut" x="601" y="295">scheduler stops dispatching</text>
</svg>
</div>
</div>
<p>
Outcome writes are atomic: a single write decides this run's terminal status (<code>Completed</code> on
success, otherwise <code>Backoff</code> or <code>Tripped</code>), so observers never see a transient
intermediate state. <code>FlowInfo</code> exposes <code>failure_streak</code> and
<code>next_eligible</code> for observability.
</p>

<h3 id="recovery"><a href="#recovery" class="anchor-link" aria-hidden="true">#</a>Recovery via <code>reset_flow</code></h3>
<p>
A <code>Tripped</code> flow stays parked until you clear it. <code>RunningScheduler::reset_flow(id)</code>
clears the failure streak and <code>next_eligible</code>, and (when the flow is not currently running) sets
the status back to <code>Idle</code>. Manual <code>trigger()</code> is rejected on a tripped flow — call
<code>reset_flow</code> first.
</p>

```rust
let snap = running.status("flaky").await.expect("flow exists");
if matches!(snap.status, Status::Tripped { .. }) {
    running.reset_flow("flaky").await?;
}

```

<p>
See the <code>scheduler_backoff</code> example
(<code>cargo run --example scheduler_backoff --features scheduler</code>) for an end-to-end walk-through
that exercises the trip and recovery path.
</p>
</div>
