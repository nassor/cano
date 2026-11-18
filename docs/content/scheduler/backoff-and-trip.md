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
waits about 1s before its next attempt, even when its base interval is shorter. On a standalone
scheduler, the <code>Every</code> loop sleeps <code>max(interval, next_eligible - now)</code> after a
run finishes; with <a href="../several-instances/"><code>with_store</code></a>, it anchors at claim time
and sleeps until <code>max(last_run + interval, next_eligible)</code>. To retry a sub-second flow sooner
after a failure, lower <code>initial</code> with <code>with_backoff</code>.
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
then multiplied by a random factor in <code>1 ± jitter</code> and capped again. Standalone, the
<code>Every</code> loop sleeps <code>max(interval, next_eligible - now)</code> after completion; with
<code>with_store</code>, it anchors at <code>last_run + interval</code> and sleeps until
<code>max(last_run + interval, next_eligible)</code>. The <code>Cron</code> loop skips ticks that fall
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
<dt><code>RunningElsewhere</code> <small>fields <code>owner</code>, <code>lease_expires</code></small></dt>
<dd>Another scheduler instance sharing the store holds a live lease on the flow. Only a scheduler built with <code>with_store</code> reports it; <code>status()</code> and <code>list()</code> dynamically normalize an expired lease (<code>lease_expires &lt;= now</code>) back to the underlying record status, and a peer claim that reaps an expired lease records the failure anchored at <code>lease.expires_at</code>. See <a href="../several-instances/">Several scheduler instances</a>.</dd>
</dl>
<div class="diagram-frame">
<p class="diagram-label">Flow status: backoff, trip, leases and recovery</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 456" role="img" aria-labelledby="backoff-status-title backoff-status-desc">
<title id="backoff-status-title">Flow status transitions</title>
<desc id="backoff-status-desc">State machine for one scheduled flow. An Idle flow is dispatched and becomes Running, or transitions to RunningElsewhere when a peer instance claims the store lease. A run that reaches an exit state makes the flow Completed and clears the streak. A run that fails, or a store lease that expires and is reaped anchored at lease.expires_at, parks the flow in Backoff while streak is below streak_limit, or moves it to Tripped when streak reaches streak_limit. When an expired RunningElsewhere lease is inspected by status() or list() before a peer reaps it, it normalizes back to the underlying record state. A run cancelled with clean rollback returns to Idle (or Backoff if an earlier window remains open). Calling reset_flow clears the streak and any expired lease and returns the flow to Idle.</desc>
<defs><marker id="backoff-status-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M660,52 V32 Q660,24 652,24 H378 Q370,24 370,32 V48" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="515" y="38">next tick</text>
<path class="e e-dash" d="M330,52 V18 Q330,10 322,10 H132 Q124,10 124,18 V48" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="227" y="24">cancelled, rolled back</text>
<circle class="n" cx="20" cy="81" r="5"/>
<path class="e" d="M25,81 H50" marker-end="url(#backoff-status-ah)"/>
<path class="e" d="M194,81 H290" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut" x="242" y="67">dispatch</text>
<path class="e" d="M444,81 H560" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-ok" x="502" y="67">success</text>
<path class="e" d="M172,110 V168" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut ta-s" x="180" y="139">peer lease</text>
<path class="e e-dash" d="M86,172 V114" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut ta-s" x="94" y="132">expired:</text>
<text class="t-mut ta-s" x="94" y="146">normalized</text>
<path class="e" d="M342,110 V298" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err ta-e" x="334" y="184">failure</text>
<text class="t-mut ta-e" x="334" y="200">streak &lt; limit</text>
<path class="e" d="M412,302 V114" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut ta-s" x="420" y="184">until elapsed</text>
<text class="t-mut ta-s" x="420" y="200">next dispatch</text>
<path class="e" d="M444,96 H520 Q528,96 528,104 V258 Q528,266 536,266 H630 Q638,266 638,274 V298" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err ta-s" x="538" y="242">failure</text>
<text class="t-mut ta-s" x="538" y="256">streak &gt;= limit</text>
<path class="e" d="M184,236 V331 Q184,339 192,339 H272" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err ta-s" x="192" y="268">lease expired</text>
<text class="t-mut ta-s" x="192" y="282">(at expires_at)</text>
<path class="e" d="M108,236 V392 Q108,400 116,400 H642 Q650,400 650,380" marker-end="url(#backoff-status-ah)"/>
<text class="t-mut t-err" x="383" y="388">expired lease reaped -&gt; streak_limit</text>
<path class="e e-hot" d="M756,339 H764 Q772,339 772,347 V428 Q772,436 764,436 H26 Q18,436 18,428 V90 Q18,82 26,82 H50" marker-end="url(#backoff-status-ah)"/>
<text class="t-code t-hot" x="390" y="422">reset_flow(id): clears streak and expired lease</text>
<rect class="n" x="54" y="52" width="140" height="58" rx="12"/>
<text class="t-strong" x="124" y="73">Idle</text>
<text class="t-mut" x="124" y="93">awaiting dispatch</text>
<rect class="n-hot" x="294" y="52" width="150" height="58" rx="12"/>
<text class="t-strong" x="369" y="73">Running</text>
<text class="t-mut" x="369" y="93">workflow executing</text>
<rect class="n-ok" x="564" y="52" width="192" height="58" rx="12"/>
<text class="t-strong" x="660" y="73">Completed</text>
<text class="t-mut" x="660" y="93">reached an exit state</text>
<text class="t-code t-mut" x="660" y="130">streak = 0 &middot; next_eligible = None</text>
<rect class="n-cop" x="54" y="172" width="206" height="64" rx="12"/>
<text class="t-strong" x="157" y="190">RunningElsewhere</text>
<text class="t-code t-mut" x="157" y="208">{ owner, lease_expires }</text>
<text class="t-mut" x="157" y="224">peer holds lease (store)</text>
<rect class="n-warn" x="276" y="302" width="214" height="74" rx="12"/>
<text class="t-strong" x="383" y="322">Backoff</text>
<text class="t-code t-mut" x="383" y="341">{ until, streak, last_error }</text>
<text class="t-mut" x="383" y="360">parked until delay elapses</text>
<rect class="n-err" x="544" y="302" width="212" height="74" rx="12"/>
<text class="t-strong" x="650" y="322">Tripped</text>
<text class="t-code t-mut" x="650" y="341">{ streak, last_error }</text>
<text class="t-mut" x="650" y="360">scheduler stops dispatching</text>
<g class="cd-loop"><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;1" values="1;1"/>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.0s" repeatCount="indefinite" path="M194,81 H294" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.03;0.13;1"/><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;0.02;0.03;0.13;0.14;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.0s" repeatCount="indefinite" path="M444,81 H564" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.15;0.24;1"/><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;0.14;0.15;0.24;0.25;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.0s" repeatCount="indefinite" path="M342,110 V302" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.27;0.39;1"/><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;0.26;0.27;0.39;0.4;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.0s" repeatCount="indefinite" path="M412,302 V110" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.41;0.52;1"/><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;0.4;0.41;0.52;0.53;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.0s" repeatCount="indefinite" path="M172,110 V172" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.54;0.61;1"/><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;0.53;0.54;0.61;0.62;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.0s" repeatCount="indefinite" path="M108,236 V392 Q108,400 116,400 H642 Q650,400 650,376" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.62;0.73;1"/><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;0.61;0.62;0.73;0.74;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.0s" repeatCount="indefinite" path="M756,339 H764 Q772,339 772,347 V428 Q772,436 764,436 H26 Q18,436 18,428 V90 Q18,82 26,82 H54" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.74;0.85;1"/><animate attributeName="opacity" dur="10.0s" repeatCount="indefinite" keyTimes="0;0.73;0.74;0.85;0.86;1" values="0;0;1;1;0;0"/></circle>
</g>
</svg>
</div>
</div>
<p>
A single atomic write decides each run's terminal status: <code>Completed</code> on success, otherwise
<code>Backoff</code> or <code>Tripped</code>. Observers never see a transient intermediate state. With a
shared store, <code>last_error</code> in <code>FlowRecord</code> is UTF-8-safely capped at 4 KiB
(<code>4096</code> bytes), and an expired lease reaped by another instance counts as a failure anchored
at <code>lease.expires_at</code> rather than the later read time. A run that ends because it was cancelled
(<code>cancel_flow</code> or a scheduler shutdown) and rolled back cleanly returns the flow to
<code>Idle</code> (or restores an active <code>Backoff</code> window) and leaves the streak untouched.
If a saga rollback fails, the run ends with <code>CompensationFailed</code>, which counts as a failure.
<code>FlowInfo</code> exposes <code>failure_streak</code> and <code>next_eligible</code>.
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
let snap = running.status("flaky").expect("flow exists");
if matches!(snap.status, Status::Tripped { .. }) {
    running.reset_flow("flaky").await?;
}

```

<p>
The <code>scheduler_backoff</code> example
(<code>cargo run --example scheduler_backoff --features scheduler</code>) walks through the trip and
recovery path end to end.
</p>
</div>
