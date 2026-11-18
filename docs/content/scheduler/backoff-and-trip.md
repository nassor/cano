+++
title = "Scheduler backoff and trip state"
description = "How the Cano scheduler backs off and trips a repeatedly failing flow, overriding the BackoffPolicy, the Status variants, and recovering a tripped flow with reset_flow."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Scheduler backoff and trip state</h1>
<p class="subtitle">How the scheduler delays a failing flow, when it stops dispatching it, and how to recover it.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.11.0</span>
<span class="meta-tag meta-tag-native">not wasm-supported</span>
</div>

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
waits about 1s before its next attempt, even when its base interval is shorter. On a standalone
scheduler the <code>Every</code> loop sleeps <code>max(interval, next_eligible - now)</code> after a run
finishes; with <a href="../several-instances/"><code>with_store</code></a> it waits until
<code>max(last_run + interval, next_eligible)</code>, where <code>last_run</code> is when the previous run
started. To retry a sub-second flow sooner after a failure, lower <code>initial</code> with
<code>with_backoff</code>.
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

struct FlakySyncTask;

#[task(state = FlowState)]
impl FlakySyncTask {
    // Tasks default to TaskConfig::new() (no retries), so each failure increments failure_streak.
    async fn run(&self) -> Result<FlowState, CanoError> {
        Err(CanoError::task_execution("upstream 503"))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let workflow = Workflow::bare()
        .register(FlowState::Start, FlakySyncTask)
        .add_exit_state(FlowState::Done);

    // Call with_backoff after registering the matching flow id.
    let running = Scheduler::new()
        .every("flaky", workflow, FlowState::Start, Duration::from_millis(200))
        .with_backoff(
            "flaky",
            BackoffPolicy {
                // Delay after the first failure (streak == 1).
                initial: Duration::from_millis(300),
                // Growth factor per consecutive failure.
                multiplier: 2.0,
                // Upper bound on the computed backoff delay.
                max_delay: Duration::from_secs(2),
                // Multiplicative jitter in 1.0 +/- 0.1 to spread synchronized retries.
                jitter: 0.1,
                // Transition to Status::Tripped after 3 consecutive failures (None = never trip).
                streak_limit: Some(3),
            },
        )
        .start()
        .await?;

    // Block until Ctrl-C (or SIGTERM on Unix), then drain and tear down.
    shutdown_signal().await?;
    running.stop().await?;
    Ok(())
}
```

<p>
The delay after a failure is <code>initial * multiplier^(streak-1)</code>, capped at <code>max_delay</code>,
then multiplied by a random factor in <code>1 ± jitter</code> and capped again. The <code>Every</code> loop
waits for the later of its next interval tick (see the callout above) and <code>next_eligible</code>, and the
<code>Cron</code> loop skips ticks that fall inside the backoff window.
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
<dt><code>RunningElsewhere</code> <small>fields <code>owner</code>, <code>lease_expires</code></small></dt>
<dd>Another scheduler instance sharing the store holds a live lease on the flow. Only a scheduler built with <code>with_store</code> reports it. Once <code>lease_expires</code> has passed, <code>status()</code> and <code>list()</code> show the stored state instead. See <a href="../several-instances/">Several scheduler instances</a>.</dd>
</dl>
<div class="diagram-frame">
<p class="diagram-label">Flow status: backoff, trip, leases and recovery</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 380" role="img" aria-labelledby="backoff-status-title backoff-status-desc">
<title id="backoff-status-title">Flow status transitions</title>
<desc id="backoff-status-desc">State machine for one scheduled flow. An Idle flow is dispatched and becomes Running. A run that reaches an exit state makes the flow Completed with the streak reset to 0, and the next tick runs it again. A failure while the streak stays below streak_limit parks the flow in Backoff until the delay elapses; a failure that brings the streak to streak_limit makes it Tripped. A cancelled run that rolls back cleanly returns to Idle. With a shared store, a flow another instance holds shows as RunningElsewhere until that lease ends; a lease that expires without a release counts as one failure. reset_flow clears the streak and returns a Tripped flow to Idle.</desc>
<defs><marker id="backoff-status-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dash" d="M375,60 V44 Q375,36 367,36 H143 Q135,36 135,44 V56" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="255" y="26">cancelled, rolled back</text>
<path class="e e-dim" d="M670,60 V44 Q670,36 662,36 H463 Q455,36 455,44 V56" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="562" y="26">next tick</text>
<circle class="n" cx="36" cy="80" r="5"/>
<path class="e" d="M41,80 H56" marker-end="url(#backoff-status-ah)"/>
<path class="e" d="M210,80 H331" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="272" y="68">dispatch</text>
<path class="e" d="M495,80 H576" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-ok" x="536" y="68">success</text>
<path class="e" d="M165,118 V246" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut ta-s" x="173" y="176">peer claims</text>
<path class="e e-dash" d="M95,250 V122" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut ta-e" x="87" y="176">lease ends</text>
<path class="e" d="M385,118 V246" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err ta-e" x="377" y="170">failure</text>
<text class="t-mut ta-e" x="377" y="184">streak &lt; limit</text>
<path class="e" d="M445,250 V122" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut ta-s" x="453" y="206">delay elapsed</text>
<text class="t-mut ta-s" x="453" y="220">next dispatch</text>
<path class="e" d="M495,104 H530 Q538,104 538,112 V170 Q538,178 546,178 H662 Q670,178 670,186 V246" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err ta-s" x="546" y="140">failure</text>
<text class="t-mut ta-s" x="546" y="154">streak reaches limit</text>
<path class="e" d="M204,287 H306" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err" x="255" y="275">lease expired</text>
<text class="t-mut" x="255" y="301">one failure</text>
<path class="e e-hot" d="M670,324 V358 Q670,366 662,366 H22 Q14,366 14,358 V112 Q14,104 22,104 H56" marker-end="url(#backoff-status-ah)"/>
<text class="t-code t-hot" x="400" y="350">reset_flow(id): clears the streak, un-trips the flow</text>
<rect class="n" x="60" y="60" width="150" height="58" rx="12"/>
<text class="t-strong" x="135" y="82">Idle</text>
<text class="t-mut" x="135" y="102">awaiting dispatch</text>
<rect class="n-hot" x="335" y="60" width="160" height="58" rx="12"/>
<text class="t-strong" x="415" y="82">Running</text>
<text class="t-mut" x="415" y="102">on this instance</text>
<rect class="n-ok" x="580" y="60" width="180" height="58" rx="12"/>
<text class="t-strong" x="670" y="82">Completed</text>
<text class="t-mut t-ok" x="670" y="102">streak reset to 0</text>
<rect class="n-cop" x="24" y="250" width="180" height="74" rx="12"/>
<text class="t-strong" x="114" y="272">RunningElsewhere</text>
<text class="t-code t-mut" x="114" y="291">{ owner, lease_expires }</text>
<text class="t-mut" x="114" y="309">a peer holds the lease</text>
<rect class="n-warn" x="310" y="250" width="210" height="74" rx="12"/>
<text class="t-strong" x="415" y="272">Backoff</text>
<text class="t-code t-mut" x="415" y="291">{ until, streak, last_error }</text>
<text class="t-mut" x="415" y="309">parked until the delay elapses</text>
<rect class="n-err" x="580" y="250" width="180" height="74" rx="12"/>
<text class="t-strong" x="670" y="272">Tripped</text>
<text class="t-code t-mut" x="670" y="291">{ streak, last_error }</text>
<text class="t-mut" x="670" y="309">not dispatched again</text>
<g class="cd-loop"><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.93;0.99;1" values="1;1;0;0"/>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10s" repeatCount="indefinite" path="M210,80 H335" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.04;0.12;1"/><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.03;0.04;0.12;0.13;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10s" repeatCount="indefinite" path="M385,118 V250" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.15;0.27;1"/><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.14;0.15;0.27;0.28;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10s" repeatCount="indefinite" path="M445,250 V118" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.3;0.42;1"/><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.29;0.3;0.42;0.43;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10s" repeatCount="indefinite" path="M495,104 H530 Q538,104 538,112 V170 Q538,178 546,178 H662 Q670,178 670,186 V250" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.45;0.6;1"/><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.44;0.45;0.6;0.61;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10s" repeatCount="indefinite" path="M670,324 V358 Q670,366 662,366 H22 Q14,366 14,358 V112 Q14,104 22,104 H60" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.64;0.84;1"/><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.63;0.64;0.84;0.85;1" values="0;0;1;1;0;0"/></circle>
</g>
</svg>
</div>
</div>
<p>
A single atomic write decides each run's terminal status: <code>Completed</code> on success, otherwise
<code>Backoff</code> or <code>Tripped</code>. Observers never see a transient intermediate state. The stored
<code>last_error</code> holds at most 4 KiB, cut on a UTF-8 character boundary. With a shared store, a lease
that expires without a release counts as one failure, dated at the lease's expiry. A run that ends
because it was cancelled (<code>cancel_flow</code> or a scheduler shutdown) and rolled back cleanly
leaves the streak untouched and returns the flow to <code>Idle</code>, or to <code>Backoff</code> when an
earlier backoff window is still open. If a saga rollback fails, the run ends with
<code>CompensationFailed</code>, which counts as a failure. <code>FlowInfo</code> exposes
<code>failure_streak</code> and <code>next_eligible</code>.
</p>

<h2 id="recovery"><a href="#recovery" class="anchor-link" aria-hidden="true">#</a>Recovery with <code>reset_flow</code></h2>
<p>
A <code>Tripped</code> flow stays parked until you reset it. <code>RunningScheduler::reset_flow(id).await</code>
clears the failure streak and <code>next_eligible</code>, and sets the status to <code>Idle</code> unless the
flow is running. Call it inside a Tokio runtime. With a <a href="../several-instances/">shared store</a> the
reset is written to the shared record and applies to every instance, clearing any expired lease while
leaving a live in-flight lease intact.
</p>
<p>
A manual <code>trigger()</code> ignores an active <code>Backoff</code> window but is rejected on a tripped
flow, so call <code>reset_flow</code> first. If the flow is mid-run and that run fails, the streak restarts
at 1, so reset after you observe <code>Tripped</code> or <code>Idle</code>.
</p>

```rust
// Read a point-in-time snapshot of the flow's state and failure streak.
let snap = running.status("flaky").expect("flow exists");
if let Status::Tripped { streak, last_error } = &snap.status {
    eprintln!("flaky tripped after {streak} failures: {last_error:?}");
    // Clears failure_streak and next_eligible (persisted if a store is attached), returning the flow to Idle.
    running.reset_flow("flaky").await?;
}
```

<p>
The <code>scheduler_backoff</code> example
(<code>cargo run --example scheduler_backoff --features scheduler</code>) walks through the trip and
recovery path end to end.
</p>
</div>
