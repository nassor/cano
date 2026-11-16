+++
title = "Scheduler backoff and trip state"
description = "How the Cano scheduler backs off and trips a repeatedly failing flow, overriding the BackoffPolicy, the Status variants, and recovering a tripped flow with reset_flow."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Scheduler backoff and trip state</h1>
<p class="subtitle">How the scheduler delays a failing flow, when it stops dispatching it, and how to recover it.</p>

<p>
See <a href="../">Scheduler</a> for scheduling strategies and lifecycle. This page covers what the
scheduler does when a flow fails repeatedly.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#backoff-policy">Overriding the default policy</a></li>
<li><a href="#status-variants">Status variants</a></li>
<li><a href="#recovery">Recovery with <code>reset_flow</code></a></li>
</ol>
</nav>
<hr class="section-divider">

<p>
Every flow has a <code>BackoffPolicy</code>. After a failure the scheduler parks the flow in
<code>Status::Backoff</code> for a growing delay. When the policy sets a <code>streak_limit</code>, the flow
becomes <code>Tripped</code> after that many consecutive failures, and the scheduler stops dispatching it
until you call <code>reset_flow</code>. Each flow starts with <code>BackoffPolicy::default()</code>;
<code>Scheduler::with_backoff</code> replaces it.
</p>

<div class="callout callout-info">
<div class="callout-label">The default delay after a failure is about 1s</div>
<p>
<code>BackoffPolicy::default()</code> has a 1s initial delay and applies to every flow, so a failed flow
waits about 1s before its next attempt, even when its base interval is shorter. The <code>Every</code> loop
sleeps <code>max(interval, next_eligible - now)</code>. To retry a sub-second flow sooner after a failure,
lower <code>initial</code> with <code>with_backoff</code>.
</p>
</div>

<div class="callout callout-info">
<div class="callout-label">Not a CircuitBreaker</div>
<p>
Flow-level <code>Tripped</code> belongs to the scheduler. The task-level <code>CanoError::CircuitOpen</code>
comes from a <a href="../../resilience/circuit-breakers/"><code>CircuitBreaker</code></a>, which gates one
task's call to a dependency. The backoff policy gates the scheduler from re-firing an entire flow.
</p>
</div>

<h2 id="backoff-policy"><a href="#backoff-policy" class="anchor-link" aria-hidden="true">#</a>Overriding the default policy</h2>
<p>
Register the workflow, then call <code>with_backoff</code> with the flow's id later in the same chain,
before <code>start()</code>. An unknown id makes <code>start()</code> return a configuration error. The
policy sets the initial delay after the first failure, the multiplier for each further consecutive failure,
a cap on the delay, the jitter, and an optional streak limit.
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
    async fn run(&self) -> Result<FlowState, CanoError> {
        Ok(FlowState::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let workflow = Workflow::bare()
        .register(FlowState::Start, NoopTask)
        .add_exit_state(FlowState::Done);

    let running = Scheduler::new()
        .every("flaky", workflow, FlowState::Start, Duration::from_millis(200))
        .with_backoff(
            "flaky",
            BackoffPolicy {
                initial: Duration::from_millis(300),
                multiplier: 2.0,
                max_delay: Duration::from_secs(2),
                jitter: 0.1,
                streak_limit: Some(3),
            },
        )
        .start()
        .await?;

    // Block until Ctrl-C (or SIGTERM on Unix), then shut down.
    cano::scheduler::shutdown_signal().await?;
    running.stop().await?;
    Ok(())
}

```

<p>
The delay after a failure is <code>initial * multiplier^(streak-1)</code>, capped at <code>max_delay</code>,
then multiplied by a random factor in <code>1 ± jitter</code> and capped again. The <code>Every</code> loop
sleeps <code>max(interval, next_eligible - now)</code>, and the <code>Cron</code> loop skips ticks that fall
inside the backoff window.
</p>
<p>
<code>BackoffPolicy::default()</code> is a 1s initial delay, a 2.0 multiplier, a 5 minute cap, 0.1 jitter,
and no trip limit. <code>BackoffPolicy::with_trip(n)</code> is the default policy with a streak limit of
<code>n</code>.
</p>
<div class="diagram-frame">
<p class="diagram-label">Backoff timeline for a flaky flow</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 200" role="img" aria-labelledby="backoff-timeline-title backoff-timeline-desc">
<title id="backoff-timeline-title">Backoff timeline for a flaky flow</title>
<desc id="backoff-timeline-desc">A flow scheduled every 1 second, with initial 2 seconds, multiplier 2.0, max_delay 8 seconds and no jitter. Runs are drawn as instant. The runs at 0 and 1 seconds succeed. The run at 2 seconds fails with streak 1 and parks the flow for 2 seconds. The run at 4 seconds fails with streak 2 and parks it for 4 seconds. The run at 8 seconds fails with streak 3 and parks it for 8 seconds. The run at 16 seconds fails with streak 4, and the computed 16 second delay is capped at 8 seconds. The run at 24 seconds succeeds, which resets the failure streak to 0 and clears next_eligible, so the 1 second cadence resumes at 25 and 26 seconds.</desc>
<defs><marker id="backoff-timeline-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-code t-mut ta-s" x="16" y="24">initial 2s · multiplier 2.0 · max_delay 8s · jitter 0</text>
<text class="t-mut ta-e" x="106" y="50">streak</text>
<text class="t-mut t-err" x="164" y="50">1</text>
<text class="t-mut t-err" x="208" y="50">2</text>
<text class="t-mut t-err" x="296" y="50">3</text>
<text class="t-mut t-err" x="472" y="50">4</text>
<text class="t-mut t-ok" x="648" y="50">0</text>
<text class="t-strong t-ok" x="120" y="68">✓</text>
<text class="t-strong t-ok" x="142" y="68">✓</text>
<text class="t-strong t-err" x="164" y="68">✗</text>
<text class="t-strong t-err" x="208" y="68">✗</text>
<text class="t-strong t-err" x="296" y="68">✗</text>
<text class="t-strong t-err" x="472" y="68">✗</text>
<text class="t-strong t-ok" x="648" y="68">✓</text>
<text class="t-strong t-ok" x="670" y="68">✓</text>
<text class="t-strong t-ok" x="692" y="68">✓</text>
<text class="t-strong ta-s" x="16" y="90">flaky</text>
<text class="t-code t-mut ta-s" x="16" y="106">every(1s)</text>
<rect class="band" x="170" y="78" width="32" height="26" rx="6"/>
<rect class="band" x="214" y="78" width="76" height="26" rx="6"/>
<rect class="band" x="302" y="78" width="164" height="26" rx="6"/>
<rect class="band" x="478" y="78" width="164" height="26" rx="6"/>
<text class="t-mut" x="186" y="92">2s</text>
<text class="t-mut" x="252" y="92">4s</text>
<text class="t-mut" x="384" y="92">8s</text>
<text class="t-mut" x="560" y="92">8s, capped</text>
<rect class="band-hot" x="114" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="136" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="158" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="202" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="290" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="466" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="642" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="664" y="78" width="12" height="26" rx="4"/>
<rect class="band-hot" x="686" y="78" width="12" height="26" rx="4"/>
<line class="axis" x1="120" y1="126" x2="714" y2="126"/>
<path class="e e-dim" d="M714,126 H732" marker-end="url(#backoff-timeline-ah)"/>
<line class="tick" x1="120" y1="126" x2="120" y2="132"/>
<line class="tick" x1="208" y1="126" x2="208" y2="132"/>
<line class="tick" x1="296" y1="126" x2="296" y2="132"/>
<line class="tick" x1="384" y1="126" x2="384" y2="132"/>
<line class="tick" x1="472" y1="126" x2="472" y2="132"/>
<line class="tick" x1="560" y1="126" x2="560" y2="132"/>
<line class="tick" x1="648" y1="126" x2="648" y2="132"/>
<text class="t-mut" x="120" y="150">0s</text>
<text class="t-mut" x="208" y="150">4s</text>
<text class="t-mut" x="296" y="150">8s</text>
<text class="t-mut" x="384" y="150">12s</text>
<text class="t-mut" x="472" y="150">16s</text>
<text class="t-mut" x="560" y="150">20s</text>
<text class="t-mut" x="648" y="150">24s</text>
<text class="t-mut" x="390" y="182">Each failure widens the wait up to max_delay. The first success clears the streak.</text>
</svg>
</div>
</div>

<h2 id="status-variants"><a href="#status-variants" class="anchor-link" aria-hidden="true">#</a>Status variants</h2>
<p>
<code>Status</code> is <code>#[non_exhaustive]</code>, so an external <code>match</code> needs a wildcard arm.
The variants, with the fields they carry:
</p>
<dl class="key-list">
<dt><code>Idle</code></dt>
<dd>Registered but not run yet, the last run was cancelled and rolled back cleanly, or the flow was reset.</dd>
<dt><code>Running</code></dt>
<dd>A run is executing.</dd>
<dt><code>Completed</code></dt>
<dd>The last run reached an exit state.</dd>
<dt><code>Backoff</code> <small>fields <code>until</code>, <code>streak</code>, <code>last_error</code></small></dt>
<dd>The last run failed. The flow waits until <code>until</code> before its next dispatch, per its <code>BackoffPolicy</code>.</dd>
<dt><code>Tripped</code> <small>fields <code>streak</code>, <code>last_error</code></small></dt>
<dd>The streak reached <code>streak_limit</code>. The scheduler does not dispatch the flow again until <code>reset_flow</code> is called.</dd>
</dl>
<div class="diagram-frame">
<p class="diagram-label">Flow status: backoff, trip and recovery</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 356" role="img" aria-labelledby="backoff-status-title backoff-status-desc">
<title id="backoff-status-title">Flow status transitions</title>
<desc id="backoff-status-desc">State machine for one scheduled flow. An Idle flow is dispatched and becomes Running. A run that reaches an exit state makes the flow Completed, with the failure streak set to 0 and next_eligible cleared, and the next tick runs it again. A run that fails while the streak stays below streak_limit parks the flow in Backoff until the delay elapses, then the next dispatch runs it again. A run that fails with the streak at streak_limit makes the flow Tripped, and the scheduler stops dispatching it. A run that is cancelled and rolls back cleanly returns the flow to Idle without changing the streak; a failed rollback counts as a failure. Calling reset_flow clears the streak and returns a Tripped flow to Idle.</desc>
<defs><marker id="backoff-status-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M636,48 V32 Q636,24 628,24 H374 Q366,24 366,32 V44" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="496" y="41">next tick</text>
<path class="e e-dash" d="M300,48 V18 Q300,10 292,10 H108 Q100,10 100,18 V44" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="200" y="29">cancelled, rolled back</text>
<circle class="n" cx="20" cy="64" r="5"/>
<path class="e" d="M27,64 H40" marker-end="url(#backoff-status-ah)"/>
<path class="e" d="M194,77 H282" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="238" y="63">dispatch</text>
<path class="e" d="M446,77 H542" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-ok" x="494" y="63">success</text>
<path class="e" d="M320,106 V152 Q320,160 312,160 H148 Q140,160 140,168 V228" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err" x="230" y="134">failure</text>
<text class="t-mut" x="230" y="150">streak &lt; streak_limit</text>
<path class="e" d="M180,232 V196 Q180,188 188,188 H392 Q400,188 400,180 V110" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="300" y="206">until elapsed · next dispatch</text>
<path class="e" d="M436,106 V202 Q436,210 444,210 H588 Q596,210 596,218 V228" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err ta-s" x="450" y="176">failure</text>
<text class="t-mut ta-s" x="450" y="192">streak reaches streak_limit</text>
<path class="e e-hot" d="M700,308 V330 Q700,338 692,338 H28 Q20,338 20,330 V102 Q20,94 28,94 H40" marker-end="url(#backoff-status-ah)"/>
<text class="t-code t-hot ta-e" x="316" y="326">reset_flow(id)</text>
<text class="t-mut t-hot ta-s" x="324" y="326">clears the streak, un-trips the flow</text>
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
A single atomic write decides each run's terminal status: <code>Completed</code> on success, otherwise
<code>Backoff</code> or <code>Tripped</code>. Observers never see a transient intermediate state. A run that
ends because it was cancelled (<code>cancel_flow</code> or a scheduler shutdown) and rolled back cleanly
returns the flow to <code>Idle</code> and leaves the streak untouched. If a saga rollback fails, the run
ends with <code>CompensationFailed</code>, which counts as a failure. <code>FlowInfo</code> exposes
<code>failure_streak</code> and <code>next_eligible</code>.
</p>

<h2 id="recovery"><a href="#recovery" class="anchor-link" aria-hidden="true">#</a>Recovery with <code>reset_flow</code></h2>
<p>
A <code>Tripped</code> flow stays parked until you reset it. <code>RunningScheduler::reset_flow(id)</code>
clears the failure streak and <code>next_eligible</code>, and sets the status to <code>Idle</code> unless the
flow is running. It is synchronous; call it inside a Tokio runtime.
</p>
<p>
A manual <code>trigger()</code> ignores an active <code>Backoff</code> window but is rejected on a tripped
flow, so call <code>reset_flow</code> first. If the flow is mid-run and that run fails, the streak restarts
at 1, so reset after you observe <code>Tripped</code> or <code>Idle</code>.
</p>

```rust
let snap = running.status("flaky").expect("flow exists");
if matches!(snap.status, Status::Tripped { .. }) {
    running.reset_flow("flaky")?;
}

```

<p>
The <code>scheduler_backoff</code> example
(<code>cargo run --example scheduler_backoff --features scheduler</code>) walks through the trip and
recovery path end to end.
</p>
</div>
