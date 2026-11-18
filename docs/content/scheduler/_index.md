+++
title = "Scheduler"
description = "Schedule Cano workflows with intervals, cron expressions, and manual triggers via the optional scheduler feature."
template = "section.html"
+++

<div class="content-wrapper">

<h1>Scheduler</h1>
<p class="subtitle">Run workflows on an interval, on a cron schedule, or on demand.</p>
<p class="feature-tag">Behind the <code>scheduler</code> feature gate (<code>features = ["scheduler"]</code>).</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#lifecycle"><code>Scheduler</code> and <code>RunningScheduler</code></a></li>
<li><a href="#overlap-prevention">Overlap prevention</a></li>
<li><a href="#scheduling-strategies">Scheduling strategies</a></li>
<li><a href="#strategy-examples">Strategy examples</a></li>
<li class="toc-sub"><a href="#interval-scheduling">Interval scheduling</a></li>
<li class="toc-sub"><a href="#cron-scheduling">Cron scheduling</a></li>
<li class="toc-sub"><a href="#manual-triggering">Manual triggering</a></li>
<li class="toc-sub"><a href="#mixed-scheduling">Mixed scheduling</a></li>
<li><a href="#backoff-guide">Backoff and trip state</a></li>
<li><a href="#several-instances">Several instances</a></li>
<li><a href="#checkpoint-resume">Checkpoints and auto-resume</a></li>
<li><a href="#cancellation">Cancelling a flow</a></li>
<li><a href="#graceful-shutdown">Graceful shutdown</a></li>
<li><a href="#multi-level-map-reduce">Multi-level map-reduce</a></li>
</ol>
</nav>

<p>
The scheduler runs workflows as background jobs on an interval, a cron expression, or a manual trigger.
Each registered workflow carries its own <a href="../resources/"><code>Resources</code></a>, whose
<code>setup()</code> and <code>teardown()</code> hooks run once per <code>Scheduler::start()</code> and
<code>RunningScheduler::stop()</code> pair, not once per run.
</p>

<div class="callout callout-info">
<div class="callout-label">Mixed state types</div>
<p>
<code>Scheduler</code> is not generic over the state type. One scheduler hosts workflows with different
state enums and different resources.
</p>

```rust
let running = Scheduler::new()
    .every("sync", sync_workflow, SyncState::Start, Duration::from_secs(300))
    .manual("export", export_workflow, ExportState::Start)
    .start()
    .await?;
```
</div>
<hr class="section-divider">

<h2 id="lifecycle"><a href="#lifecycle" class="anchor-link" aria-hidden="true">#</a><code>Scheduler</code> and <code>RunningScheduler</code></h2>
<p>
The scheduler has two halves, so a double start cannot compile.
</p>
<ul>
<li><code>Scheduler</code> is the builder. Every method consumes and returns it, so registration is one chain:
<code>every</code>, <code>cron</code> and <code>manual</code> register a workflow,
<code>with_backoff</code> overrides the <a href="backoff-and-trip/"><code>BackoffPolicy</code></a> of one flow,
<code>with_store</code>, <code>with_lease_ttl</code> and <code>with_instance_id</code> share the flows with other
<a href="#several-instances">scheduler instances</a>,
and <code>with_drain_timeout</code> and <code>with_cancel_timeout</code> tune
<a href="#graceful-shutdown">shutdown</a>. A zero interval, a bad cron expression, a duplicate id, or a
<code>with_backoff</code> for an unknown id is remembered while you chain, and <code>start()</code> returns
the first one before it validates or sets up anything.</li>
<li><code>RunningScheduler</code> is the live handle that <code>start().await?</code> returns. Clones are
cheap and control the same scheduler, so <code>trigger</code>, <code>status</code>, <code>list</code>,
<code>reset_flow</code>, <code>cancel_flow</code> and <code>stop</code> work from any task.</li>
</ul>
<p>
<code>start</code> consumes the builder. It validates every workflow and its initial state (rejecting any
workflow containing a <a href="../suspend-task/"><code>SuspendTask</code></a> state with
<code>CanoError::Configuration</code>, because scheduled runs have no per-run
<code>workflow_id</code> or <code>resume_with</code> caller), sets up each workflow's resources in
registration order (a setup failure tears down the flows already set up and returns the error), then
spawns one loop per interval or cron flow. Manual flows get no loop and run only on
<code>trigger</code>.
</p>
<p>The methods of <code>RunningScheduler</code>, each with its signature:</p>
<dl class="key-list">
<dt><code>async fn stop(&amp;self) -&gt; CanoResult&lt;()&gt;</code></dt>
<dd>Drains, cancels, aborts, then tears resources down, in bounded time. Idempotent. See <a href="#graceful-shutdown">Graceful shutdown</a>.</dd>
<dt><code>async fn wait(&amp;self) -&gt; CanoResult&lt;()&gt;</code></dt>
<dd>Blocks until some clone calls <code>stop()</code>, then returns its result. It never stops the scheduler itself.</dd>
<dt><code>async fn wait_idle(&amp;self, id: &amp;str) -&gt; Option&lt;FlowInfo&gt;</code></dt>
<dd>Waits until the flow is not <code>Running</code> and returns its <code>FlowInfo</code>, or <code>None</code> for an unknown id. Returns at once for a flow that is not running.</dd>
<dt><code>async fn trigger(&amp;self, id: &amp;str) -&gt; CanoResult&lt;()&gt;</code></dt>
<dd>Starts a run immediately. Errors when the id is unknown, the scheduler is stopping or stopped, the flow is already running, or the flow is tripped. A pending backoff window does not block it. With a store it also errors while another instance runs the flow.</dd>
<dt><code>fn cancel_flow(&amp;self, id: &amp;str) -&gt; CanoResult&lt;()&gt;</code></dt>
<dd>Cooperatively cancels the flow's in-flight run on this instance. A no-op when the flow is idle here; an unknown id is an error.</dd>
<dt><code>async fn reset_flow(&amp;self, id: &amp;str) -&gt; CanoResult&lt;()&gt;</code></dt>
<dd>Clears the failure streak and the backoff window. A flow that is not running returns to <code>Idle</code>. With a store the reset applies to every instance and clears any expired lease while leaving live ones intact.</dd>
<dt><code>fn instance_id(&amp;self) -&gt; &amp;str</code></dt>
<dd>Returns this instance's id: the one set with <code>with_instance_id</code>, or the random <code>cano-...</code> default. With a store it names the lease owner in stored records and <code>Status::RunningElsewhere</code>.</dd>
<dt><code>fn status(&amp;self, id: &amp;str) -&gt; Option&lt;FlowInfo&gt;</code></dt>
<dd>Snapshot of one flow, or <code>None</code> for an unknown id.</dd>
<dt><code>fn list(&amp;self) -&gt; Vec&lt;FlowInfo&gt;</code></dt>
<dd>Snapshot of every flow, in registration order.</dd>
</dl>
<div class="diagram-frame">
<p class="diagram-label">Scheduler dispatch lifecycle</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 336" role="img" aria-labelledby="sched-dispatch-title sched-dispatch-desc">
<title id="sched-dispatch-title">Scheduler dispatch lifecycle</title>
<desc id="sched-dispatch-desc">An interval tick, cron firing or manual trigger attempts to reserve the flow locally and, when a SchedulerStore is configured, via compare-and-swap. Refused attempts skip the tick or return an error. A won reservation runs Workflow::run_once under a run token and optional lease renewal. When a CheckpointStore is attached, unfinished checkpoint rows auto-resume and stale completed exit rows are cleared before starting fresh. Completion commits Completed, Idle on clean cancellation, or Backoff or Tripped on failure.</desc>
<defs><marker id="sched-dispatch-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="14" y="28" width="142" height="40" rx="8"/>
<text class="t-code" x="85" y="48">every(interval)</text>
<rect class="n" x="14" y="86" width="142" height="40" rx="8"/>
<text class="t-code" x="85" y="106">cron(expr)</text>
<rect class="n-cop" x="14" y="144" width="142" height="40" rx="8"/>
<text class="t-code" x="85" y="164">trigger(id)</text>
<path class="e" d="M156,48 H178 Q186,48 186,56 V106"/>
<path class="e" d="M156,106 H206" marker-end="url(#sched-dispatch-ah)"/>
<path class="e" d="M156,164 H178 Q186,164 186,156 V106"/>
<rect class="n-hot" x="210" y="58" width="154" height="96" rx="10"/>
<text class="t-strong" x="287" y="84">Reserve flow</text>
<text class="t-mut t-code" x="287" y="106">local cell / CAS</text>
<text class="t-mut" x="287" y="126">allocates run_token</text>
<path class="e e-dash" d="M287,154 V192" marker-end="url(#sched-dispatch-ah)"/>
<text class="t-mut ta-s" x="295" y="173">refused</text>
<rect class="n-ghost" x="210" y="196" width="154" height="48" rx="8"/>
<text class="t-strong" x="287" y="212">Skip / Err</text>
<text class="t-mut" x="287" y="230">busy &middot; not due &middot; tripped</text>
<path class="e" d="M364,106 H412" marker-end="url(#sched-dispatch-ah)"/>
<text class="t-mut t-ok" x="388" y="93">won</text>
<rect class="n" x="416" y="54" width="172" height="104" rx="10"/>
<text class="t-strong" x="502" y="76">Workflow::run_once</text>
<text class="t-mut" x="502" y="96">runs FSM to exit</text>
<text class="t-mut" x="502" y="114">optional lease renewal</text>
<rect class="n-cop" x="416" y="196" width="172" height="64" rx="8"/>
<text class="t-strong" x="502" y="214">CheckpointStore</text>
<text class="t-mut" x="502" y="232">unfinished: auto-resume</text>
<text class="t-mut" x="502" y="248">exit row: clear &amp; fresh</text>
<path class="e e-dash" d="M482,158 V192" marker-end="url(#sched-dispatch-ah)"/>
<path class="e e-dash" d="M522,196 V162" marker-end="url(#sched-dispatch-ah)"/>
<text class="t-mut ta-e" x="474" y="177">load</text>
<text class="t-mut ta-s" x="530" y="177">clear</text>
<path class="e" d="M588,84 H606 Q614,84 614,76 V58 Q614,50 622,50 H630" marker-end="url(#sched-dispatch-ah)"/>
<path class="e e-dash" d="M588,116 H630" marker-end="url(#sched-dispatch-ah)"/>
<path class="e" d="M588,136 H606 Q614,136 614,144 V174 Q614,182 622,182 H630" marker-end="url(#sched-dispatch-ah)"/>
<rect class="n-ok" x="634" y="26" width="132" height="48" rx="8"/>
<text class="t-strong" x="700" y="42">Completed</text>
<text class="t-mut t-ok" x="700" y="60">streak = 0</text>
<rect class="n-ghost" x="634" y="92" width="132" height="48" rx="8"/>
<text class="t-strong" x="700" y="108">Idle</text>
<text class="t-mut" x="700" y="126">clean cancel</text>
<rect class="n-err" x="634" y="158" width="132" height="48" rx="8"/>
<text class="t-strong" x="700" y="174">Backoff / Tripped</text>
<text class="t-mut t-err" x="700" y="192">streak + 1</text>
<g class="cd-loop"><animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.9333;0.9889;1" values="1;1;0;0"/>
<rect class="band-hot" x="430" y="132" width="144" height="12" rx="4"><animate attributeName="width" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.3111;0.6111;1" values="0;0;144;144"/></rect>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.0s" repeatCount="indefinite" path="M156,48 H178 Q186,48 186,56 V98 Q186,106 194,106 H210" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.0333;0.1556;1"/><animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.0222;0.0333;0.1556;0.1667;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.0s" repeatCount="indefinite" path="M364,106 H416" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.1667;0.2778;1"/><animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.1556;0.1667;0.2778;0.2889;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.0s" repeatCount="indefinite" path="M482,158 V196" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.2889;0.3778;1"/><animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.2778;0.2889;0.3778;0.3889;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.0s" repeatCount="indefinite" path="M522,196 V158" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.3889;0.4778;1"/><animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.3778;0.3889;0.4778;0.4889;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.0s" repeatCount="indefinite" path="M588,84 H606 Q614,84 614,76 V58 Q614,50 622,50 H634" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.6111;0.7222;1"/><animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.6;0.6111;0.7222;0.7333;1" values="0;0;1;1;0;0"/></circle>
<rect class="n-hot" x="14" y="282" width="752" height="36" rx="10"><animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.7222;0.7778;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="390" y="300">Triggers reserve a flow, execute run_once (auto-resuming checkpoints), and commit status<animate attributeName="opacity" dur="9.0s" repeatCount="indefinite" keyTimes="0;0.7222;0.7778;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>
<p>
<code>stop</code>, <code>wait</code>, <code>wait_idle</code>, <code>trigger</code> and <code>reset_flow</code> are <code>async</code>.
<code>trigger</code>, <code>cancel_flow</code> and <code>reset_flow</code> must be called inside a Tokio runtime, because <code>trigger</code> spawns the run.
Dropping the last clone without calling <code>stop()</code> aborts every loop and run and does not tear
resources down, so call <code>stop().await</code> for an orderly shutdown.
</p>
<hr class="section-divider">

<h2 id="overlap-prevention"><a href="#overlap-prevention" class="anchor-link" aria-hidden="true">#</a>Overlap prevention</h2>
<p>
The scheduler never runs the same flow twice at once. Interval and cron loops run their workflow inline, so
a loop cannot start a second run while one is in progress. A tick that finds the flow running because of a
manual trigger is skipped, and a <code>trigger()</code> on a running flow returns an error.
</p>
<p>
On a standalone scheduler (without a store), an interval flow is fixed-delay: if a workflow is set to run
every 30 seconds and one execution takes 45 seconds, the next run starts 30 seconds after that execution
finishes. With <a href="several-instances/"><code>with_store</code></a>, an interval flow is fixed-rate
anchored at <code>last_run + interval</code> (stamped when the lease is claimed), so after a 45 second
execution on a 30 second interval the next tick is already due and dispatches after the 10 ms minimum
poll floor.
</p>
<p>
A cron loop advances from <code>prev.max(now)</code> after each run or outage, skipping missed firings
rather than replaying a catch-up flood. On waking from sleep, it re-checks <code>Utc::now()</code> in a
loop so a backward NTP clock step cannot fire a cron expression early. With <code>with_store</code>, if a
due cron tick hits a transient store error, the loop retries after <code>(ttl / 3).clamp(50ms, 5s)</code>
while still strictly before the next scheduled firing.
</p>
<hr class="section-divider">

<h2 id="scheduling-strategies"><a href="#scheduling-strategies" class="anchor-link" aria-hidden="true">#</a>Scheduling strategies</h2>
<div class="mode-grid">
<div class="mode-card">
<div class="mode-icon" aria-hidden="true">⏱</div>
<h3>Interval</h3>
<p>Run workflows at fixed time intervals.</p>

```rust
.every(..., Duration::from_secs(30))

```
</div>
<div class="mode-card">
<div class="mode-icon" aria-hidden="true">📅</div>
<h3>Cron</h3>
<p>Run workflows based on cron expressions.</p>

```rust
.cron(..., "0 0 9 * * *")

```
</div>
<div class="mode-card">
<div class="mode-icon" aria-hidden="true">👆</div>
<h3>Manual</h3>
<p>Trigger workflows on demand.</p>

```rust
.manual(...)

```
</div>
</div>
<hr class="section-divider">

<h2 id="strategy-examples"><a href="#strategy-examples" class="anchor-link" aria-hidden="true">#</a>Strategy examples</h2>
<p>Each strategy has a complete example below.</p>

<h3 id="interval-scheduling"><a href="#interval-scheduling" class="anchor-link" aria-hidden="true">#</a>Interval scheduling</h3>
<p>Runs a workflow at a fixed interval. The first run starts as soon as the scheduler starts. Standalone, each later run starts one interval after the previous run finishes (fixed-delay); with <code>with_store</code>, cadence anchors at <code>last_run + interval</code> (fixed-rate). After a failure the wait stretches to the flow's <a href="backoff-and-trip/">backoff window</a> when that is longer.</p>

<div class="diagram-frame">
<p class="diagram-label">Interval scheduling timeline (standalone fixed-delay)</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 760 180" role="img" aria-labelledby="sched-interval-title sched-interval-desc">
<title id="sched-interval-title">Interval scheduling timeline</title>
<desc id="sched-interval-desc">A workflow scheduled every 30 seconds. Run 1 starts at 0 seconds and takes 2 seconds. The scheduler then waits a full 30 seconds, so run 2 starts 32 seconds after run 1 started, and run 3 starts 32 seconds after run 2.</desc>
<defs><marker id="sched-interval-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M150,36 H438" marker-start="url(#sched-interval-ah)" marker-end="url(#sched-interval-ah)"/>
<text class="t-mut" x="294" y="22">32s between run starts</text>
<text class="t-strong ta-s" x="20" y="78">Workflow</text>
<rect class="band" x="168" y="64" width="270" height="26" rx="6"/>
<text class="t-mut" x="303" y="78">wait 30s</text>
<rect class="band" x="456" y="64" width="270" height="26" rx="6"/>
<text class="t-mut" x="591" y="78">wait 30s</text>
<rect class="band-hot" x="150" y="64" width="18" height="26" rx="4"/>
<rect class="band-hot" x="438" y="64" width="18" height="26" rx="4"/>
<rect class="band-hot" x="726" y="64" width="18" height="26" rx="4"/>
<text class="t-mut t-hot" x="159" y="52">Run 1</text>
<text class="t-mut t-hot" x="447" y="52">Run 2</text>
<text class="t-mut t-hot" x="735" y="52">Run 3</text>
<line class="axis" x1="150" y1="112" x2="726" y2="112"/>
<path class="e e-dim" d="M726,112 H744" marker-end="url(#sched-interval-ah)"/>
<line class="tick" x1="150" y1="112" x2="150" y2="118"/>
<line class="tick" x1="240" y1="112" x2="240" y2="118"/>
<line class="tick" x1="330" y1="112" x2="330" y2="118"/>
<line class="tick" x1="420" y1="112" x2="420" y2="118"/>
<line class="tick" x1="510" y1="112" x2="510" y2="118"/>
<line class="tick" x1="600" y1="112" x2="600" y2="118"/>
<line class="tick" x1="690" y1="112" x2="690" y2="118"/>
<text class="t-mut" x="150" y="132">0s</text>
<text class="t-mut" x="240" y="132">10s</text>
<text class="t-mut" x="330" y="132">20s</text>
<text class="t-mut" x="420" y="132">30s</text>
<text class="t-mut" x="510" y="132">40s</text>
<text class="t-mut" x="600" y="132">50s</text>
<text class="t-mut" x="690" y="132">60s</text>
<text class="t-mut" x="438" y="160">A 2s run, then a full 30s wait: the next run starts one interval after the previous run finishes</text>
</svg>
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[derive(Clone)]
struct HealthCheckTask;

#[task(state = State)]
impl HealthCheckTask {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        println!("Running health check...");

        // Check system health
        let board = res.get::<Blackboard>()?;
        let status = "healthy".to_string();
        board.put("last_health_check", status);

        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();

    let workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, HealthCheckTask)
        .add_exit_state(State::Complete);

    // Run every 30 seconds. start() consumes the builder and returns a
    // clone-able RunningScheduler.
    let running = Scheduler::new()
        .every("health_check", workflow, State::Start, Duration::from_secs(30))
        .start()
        .await?;

    // Block until Ctrl-C (or SIGTERM on Unix), then drain in-flight runs and tear down.
    cano::scheduler::shutdown_signal().await?;
    running.stop().await?;
    Ok(())
}

```

<div class="callout callout-tip">
<p>Runnable examples: <code>cargo run --example scheduler_duration_scheduling --features scheduler</code>
(interval-only) and <code>cargo run --example scheduler_scheduling --features scheduler</code> (intervals
plus cron and manual flows).</p>
</div>

<h3 id="cron-scheduling"><a href="#cron-scheduling" class="anchor-link" aria-hidden="true">#</a>Cron scheduling</h3>
<p>Runs a workflow on a cron expression (a leading seconds field, parsed by the <code>cron</code> crate and evaluated in UTC). A tick that falls inside a pending backoff window is skipped, missed firings during an outage are skipped without a catch-up flood, and waking re-checks <code>Utc::now()</code> against backward NTP clock steps. With <code>with_store</code>, a transient store error on a due tick retries every <code>(ttl / 3).clamp(50ms, 5s)</code> while still before the next firing.</p>

<div class="diagram-frame">
<p class="diagram-label">Cron scheduling timeline</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 760 180" role="img" aria-labelledby="sched-cron-title sched-cron-desc">
<title id="sched-cron-title">Cron scheduling timeline</title>
<desc id="sched-cron-desc">One day on a UTC clock. The workflow is idle most of the day. It runs at 09:00 (expression 0 0 9 * * *) and again at 18:00 (expression 0 0 18 * * *), drawn here as one-hour runs, and repeats at the same wall-clock times each day.</desc>
<defs><marker id="sched-cron-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-code t-mut" x="378" y="26">0 0 9 * * *</text>
<text class="t-code t-mut" x="594" y="26">0 0 18 * * *</text>
<text class="t-mut t-hot" x="378" y="48">Run 1 · 09:00</text>
<text class="t-mut t-hot" x="594" y="48">Run 2 · 18:00</text>
<text class="t-strong ta-s" x="20" y="78">Workflow</text>
<rect class="band" x="150" y="64" width="576" height="26" rx="6"/>
<text class="t-mut" x="486" y="78">idle</text>
<rect class="band-hot" x="366" y="64" width="24" height="26" rx="4"/>
<rect class="band-hot" x="582" y="64" width="24" height="26" rx="4"/>
<text class="t-mut" x="378" y="104">1h</text>
<text class="t-mut" x="594" y="104">1h</text>
<line class="axis" x1="150" y1="122" x2="726" y2="122"/>
<path class="e e-dim" d="M726,122 H744" marker-end="url(#sched-cron-ah)"/>
<line class="tick" x1="150" y1="122" x2="150" y2="128"/>
<line class="tick" x1="222" y1="122" x2="222" y2="128"/>
<line class="tick" x1="294" y1="122" x2="294" y2="128"/>
<line class="tick" x1="366" y1="122" x2="366" y2="128"/>
<line class="tick" x1="438" y1="122" x2="438" y2="128"/>
<line class="tick" x1="510" y1="122" x2="510" y2="128"/>
<line class="tick" x1="582" y1="122" x2="582" y2="128"/>
<line class="tick" x1="654" y1="122" x2="654" y2="128"/>
<line class="tick" x1="726" y1="122" x2="726" y2="128"/>
<text class="t-mut" x="150" y="142">00:00</text>
<text class="t-mut" x="222" y="142">03:00</text>
<text class="t-mut" x="294" y="142">06:00</text>
<text class="t-mut" x="366" y="142">09:00</text>
<text class="t-mut" x="438" y="142">12:00</text>
<text class="t-mut" x="510" y="142">15:00</text>
<text class="t-mut" x="582" y="142">18:00</text>
<text class="t-mut" x="654" y="142">21:00</text>
<text class="t-mut" x="726" y="142">24:00</text>
<text class="t-mut" x="438" y="166">Two runs a day at 09:00 and 18:00 UTC, idle the rest of the time, the same times tomorrow</text>
</svg>
</div>
</div>

```rust
use cano::prelude::*;
use chrono::Utc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[derive(Clone)]
struct DailyReport {
    report_type: String,
}

#[task(state = State)]
impl DailyReport {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        println!("Preparing {} report...", self.report_type);

        let board = res.get::<Blackboard>()?;

        // Load data for report
        let data = vec!["metric1".to_string(), "metric2".to_string(), "metric3".to_string()];
        board.put("report_start", Utc::now().to_rfc3339());

        println!("Generating report with {} records", data.len());
        let result = format!("{} report: {} records processed", self.report_type, data.len());

        println!("Report completed: {}", result);
        board.put("last_report", result);

        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();

    // Morning report workflow
    let morning_report = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, DailyReport {
            report_type: "Morning".to_string()
        })
        .add_exit_state(State::Complete);

    // Evening report workflow
    let evening_report = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, DailyReport {
            report_type: "Evening".to_string()
        })
        .add_exit_state(State::Complete);

    let running = Scheduler::new()
        // Run daily at 9 AM: "0 0 9 * * *"
        .cron("morning_report", morning_report, State::Start, "0 0 9 * * *")
        // Run daily at 6 PM: "0 0 18 * * *"
        .cron("evening_report", evening_report, State::Start, "0 0 18 * * *")
        .start()
        .await?;

    cano::scheduler::shutdown_signal().await?;
    running.stop().await?;
    Ok(())
}

```

<h3 id="manual-triggering"><a href="#manual-triggering" class="anchor-link" aria-hidden="true">#</a>Manual triggering</h3>
<p>Runs a workflow only when <code>trigger</code> is called. Suits user-initiated tasks and event-driven processing.</p>

<div class="callout callout-info">
<div class="callout-label">Triggering needs a running scheduler</div>
<p>
<code>trigger()</code> lives on <code>RunningScheduler</code>, so the compiler rejects a trigger before
<code>start()</code>. It starts the run and returns without waiting for it. Use
<code>wait_idle(id)</code> or <code>status(id)</code> to see the outcome.
</p>
</div>

<div class="diagram-frame">
<p class="diagram-label">Manual trigger sequence</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 322" role="img" aria-labelledby="sched-manual-title sched-manual-desc">
<title id="sched-manual-title">Manual trigger sequence</title>
<desc id="sched-manual-desc">Sequence diagram with three participants: the caller, the running scheduler and the workflow. The caller calls trigger on the scheduler, which spawns the workflow run and returns at once. The caller then calls wait_idle. When the workflow finishes, the scheduler answers wait_idle with the flow's info, whose status is Completed. A manual flow runs only when trigger is called.</desc>
<defs><marker id="sched-manual-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="110" y1="46" x2="110" y2="268"/>
<line class="lifeline" x1="360" y1="46" x2="360" y2="268"/>
<line class="lifeline" x1="610" y1="46" x2="610" y2="268"/>
<rect class="n-cop" x="35" y="8" width="150" height="38" rx="8"/>
<text class="t-strong" x="110" y="28">Caller</text>
<rect class="n-hot" x="285" y="8" width="150" height="38" rx="8"/>
<text class="t-strong" x="360" y="28">Scheduler</text>
<rect class="n" x="535" y="8" width="150" height="38" rx="8"/>
<text class="t-strong" x="610" y="28">Workflow</text>
<path class="e" d="M110,78 H356" marker-end="url(#sched-manual-ah)"/>
<text class="t-code" x="233" y="66">trigger("data_export")</text>
<path class="e" d="M360,122 H606" marker-end="url(#sched-manual-ah)"/>
<text class="t-mut" x="483" y="110">spawn the run</text>
<path class="e" d="M110,166 H356" marker-end="url(#sched-manual-ah)"/>
<text class="t-code" x="233" y="154">wait_idle("data_export")</text>
<path class="e e-dash" d="M610,210 H364" marker-end="url(#sched-manual-ah)"/>
<text class="t-mut t-ok" x="487" y="198">run finished</text>
<path class="e e-dash" d="M360,254 H114" marker-end="url(#sched-manual-ah)"/>
<text class="t-mut t-ok" x="237" y="242">FlowInfo: Completed</text>
<rect class="n-hot" x="35" y="278" width="650" height="34" rx="10"/>
<text class="t-strong" x="360" y="296">A manual flow runs only when trigger() is called</text>
</svg>
</div>
</div>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[derive(Clone)]
struct DataExportTask;

#[task(state = State)]
impl DataExportTask {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        println!("Starting data export...");

        // Export data to CSV
        let board = res.get::<Blackboard>()?;
        let export_path = "/tmp/export.csv".to_string();
        board.put("export_path", export_path);

        println!("Export completed");
        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();

    let export_workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, DataExportTask)
        .add_exit_state(State::Complete);

    // Register as manual-only workflow. start() returns a live, clone-able handle.
    let running = Scheduler::new()
        .manual("data_export", export_workflow, State::Start)
        .start()
        .await?;

    // trigger() starts the run and returns.
    println!("Triggering export...");
    running.trigger("data_export").await?;

    // wait_idle() returns once the run has finished.
    let info = running.wait_idle("data_export").await.expect("flow exists");
    println!("Export finished: {:?}", info.status);

    // Can be triggered again later
    running.trigger("data_export").await?;
    running.wait_idle("data_export").await;

    // stop() drains in-flight runs, then tears the resources down.
    running.stop().await?;
    Ok(())
}

```

<h3 id="mixed-scheduling"><a href="#mixed-scheduling" class="anchor-link" aria-hidden="true">#</a>Mixed scheduling</h3>
<p>One scheduler can combine all three strategies.</p>

<div class="diagram-frame">
<p class="diagram-label">Mixed strategy overview</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 13 760 244" role="img" aria-labelledby="sched-mixed-title sched-mixed-desc">
<title id="sched-mixed-title">Mixed strategy overview</title>
<desc id="sched-mixed-desc">Four flows in one scheduler over one day. An interval flow runs every 5 minutes, about 288 runs a day; the pulses drawn show the cadence, not every run. Two cron flows are drawn as one-hour bands, the daily backup at 03:00 and the weekly report at 09:00 on Mondays. The manual emergency export is drawn at 14:30, when something calls trigger.</desc>
<defs><marker id="sched-mixed-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-mut t-hot" x="438" y="38">every 5 min · 288 runs/day (cadence illustrated)</text>
<text class="t-strong ta-s" x="20" y="61">Interval</text>
<rect class="band" x="150" y="48" width="576" height="26" rx="6"/>
<path class="band-hot" d="M150,48h4v26h-4z M174,48h4v26h-4z M198,48h4v26h-4z M222,48h4v26h-4z M246,48h4v26h-4z M270,48h4v26h-4z M294,48h4v26h-4z M318,48h4v26h-4z M342,48h4v26h-4z M366,48h4v26h-4z M390,48h4v26h-4z M414,48h4v26h-4z M438,48h4v26h-4z M462,48h4v26h-4z M486,48h4v26h-4z M510,48h4v26h-4z M534,48h4v26h-4z M558,48h4v26h-4z M582,48h4v26h-4z M606,48h4v26h-4z M630,48h4v26h-4z M654,48h4v26h-4z M678,48h4v26h-4z M702,48h4v26h-4z"/>
<text class="t-mut t-hot" x="234" y="84">Daily Backup</text>
<text class="t-mut t-hot" x="378" y="84">Weekly Report</text>
<text class="t-strong ta-s" x="20" y="107">Cron</text>
<rect class="band" x="150" y="94" width="576" height="26" rx="6"/>
<rect class="band-hot" x="222" y="94" width="24" height="26" rx="4"/>
<rect class="band-hot" x="366" y="94" width="24" height="26" rx="4"/>
<text class="t-mut t-hot" x="501" y="130">Emergency Export · 14:30</text>
<text class="t-strong ta-s" x="20" y="153">Manual</text>
<rect class="band" x="150" y="140" width="576" height="26" rx="6"/>
<rect class="band-hot" x="498" y="140" width="6" height="26" rx="2"/>
<line class="axis" x1="150" y1="190" x2="726" y2="190"/>
<path class="e e-dim" d="M726,190 H744" marker-end="url(#sched-mixed-ah)"/>
<line class="tick" x1="150" y1="190" x2="150" y2="196"/>
<line class="tick" x1="222" y1="190" x2="222" y2="196"/>
<line class="tick" x1="294" y1="190" x2="294" y2="196"/>
<line class="tick" x1="366" y1="190" x2="366" y2="196"/>
<line class="tick" x1="438" y1="190" x2="438" y2="196"/>
<line class="tick" x1="510" y1="190" x2="510" y2="196"/>
<line class="tick" x1="582" y1="190" x2="582" y2="196"/>
<line class="tick" x1="654" y1="190" x2="654" y2="196"/>
<line class="tick" x1="726" y1="190" x2="726" y2="196"/>
<text class="t-mut" x="150" y="210">00:00</text>
<text class="t-mut" x="222" y="210">03:00</text>
<text class="t-mut" x="294" y="210">06:00</text>
<text class="t-mut" x="366" y="210">09:00</text>
<text class="t-mut" x="438" y="210">12:00</text>
<text class="t-mut" x="510" y="210">15:00</text>
<text class="t-mut" x="582" y="210">18:00</text>
<text class="t-mut" x="654" y="210">21:00</text>
<text class="t-mut" x="726" y="210">24:00</text>
<text class="t-mut" x="438" y="234">One scheduler, three trigger sources: interval, cron and on demand</text>
</svg>
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();

    // Define simple tasks
    #[derive(Clone)]
    struct DataSyncTask;

    #[task(state = State)]
    impl DataSyncTask {
        async fn run(&self) -> Result<State, CanoError> {
            println!("Syncing data...");
            Ok(State::Complete)
        }
    }

    #[derive(Clone)]
    struct BackupTask;
    #[task(state = State)]
    impl BackupTask {
        async fn run(&self) -> Result<State, CanoError> {
            println!("Running backup...");
            Ok(State::Complete)
        }
    }

    #[derive(Clone)]
    struct WeeklyReportTask;

    #[task(state = State)]
    impl WeeklyReportTask {
        async fn run(&self) -> Result<State, CanoError> {
            println!("Generating weekly report...");
            Ok(State::Complete)
        }
    }

    #[derive(Clone)]
    struct EmergencyExportTask;

    #[task(state = State)]
    impl EmergencyExportTask {
        async fn run(&self) -> Result<State, CanoError> {
            println!("Emergency export...");
            Ok(State::Complete)
        }
    }

    // 1. Interval: Data sync every 5 minutes
    let sync_workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, DataSyncTask)
        .add_exit_state(State::Complete);

    // 2. Cron: Daily backup at 3 AM
    let backup_workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, BackupTask)
        .add_exit_state(State::Complete);

    // 3. Cron: Weekly report on Mondays at 9 AM
    let report_workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, WeeklyReportTask)
        .add_exit_state(State::Complete);

    // 4. Manual: Emergency data export
    let export_workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(State::Start, EmergencyExportTask)
        .add_exit_state(State::Complete);

    // One chain registers every flow; start() returns the live handle.
    let running = Scheduler::new()
        .every("data_sync", sync_workflow, State::Start, Duration::from_secs(300))
        .cron("daily_backup", backup_workflow, State::Start, "0 0 3 * * *")
        .cron("weekly_report", report_workflow, State::Start, "0 0 9 * * MON")
        .manual("emergency_export", export_workflow, State::Start)
        .start()
        .await?;

    // Report on every flow once a minute until Ctrl-C (or SIGTERM on Unix).
    let shutdown = cano::scheduler::shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                for info in running.list() {
                    println!("{}: {:?} (runs: {})", info.id, info.status, info.run_count);
                }

                // Example: trigger the emergency export if some condition holds.
                // running.trigger("emergency_export").await?;
            }
        }
    }

    running.stop().await?;
    Ok(())
}

```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example scheduler_mixed_workflows --features scheduler</code>.
It runs interval, cron, and manual flows side by side and triggers the manual one.</p>
</div>
<hr class="section-divider">

<h2 id="backoff-guide"><a href="#backoff-guide" class="anchor-link" aria-hidden="true">#</a>Backoff and trip state</h2>
<p>A flow that keeps failing is backed off and can be tripped. The
<a href="backoff-and-trip/">backoff and trip page</a> covers the policy, the <code>Status</code> variants,
and how to recover a tripped flow.</p>
<hr class="section-divider">

<h2 id="several-instances"><a href="#several-instances" class="anchor-link" aria-hidden="true">#</a>Several instances</h2>
<p>Give several processes the same flows and one shared store, and each tick of each flow runs on exactly one
of them. <code>Scheduler::with_store</code> turns it on; see
<a href="several-instances/">Several scheduler instances</a> for leases, failure behaviour and the stores.</p>
<hr class="section-divider">

<h2 id="checkpoint-resume"><a href="#checkpoint-resume" class="anchor-link" aria-hidden="true">#</a>Checkpoints and auto-resume</h2>
<p>
A scheduled workflow can carry a <a href="../recovery/"><code>CheckpointStore</code></a> via
<code>Workflow::with_checkpoint_store</code>. When <code>Workflow::with_workflow_id</code> is omitted, the
scheduler defaults the checkpoint id to the registered <code>flow_id</code>, so distinct flow ids remain isolated.
</p>
<ul>
<li>If <code>load_run</code> finds no rows, the workflow runs from <code>initial_state</code> at sequence <code>0</code>.</li>
<li>If the log ends at a registered exit state with a matching <code>workflow_version</code> (for example after a crash between writing the exit row and clearing it), the scheduler clears the stale log and starts a fresh run.</li>
<li>If the log holds an unfinished run (after a process crash, shutdown abort, non-saga failure or incomplete rollback), the next scheduled run or cluster failover automatically resumes from those checkpoint rows, rebuilding any <a href="../saga/">saga compensation stack</a> and <a href="../stepped-task/">SteppedTask</a> or <a href="../stream-task/">StreamTask</a> cursor.</li>
</ul>
<p>
Resource <code>setup()</code> and <code>teardown()</code> stay owned by the scheduler, so an auto-resumed run reuses the scheduler's active resources instead of running per-run setup or teardown. Workflows containing <a href="../suspend-task/"><code>SuspendTask</code></a> states are rejected at <code>Scheduler::start()</code> with <code>CanoError::Configuration</code>.
</p>

```rust
use cano::prelude::*;
use cano::RedbStore;
use std::sync::Arc;
use std::time::Duration;

let checkpoints = Arc::new(RedbStore::new("scheduled.redb")?);

// Without with_workflow_id, checkpoints use the flow id ("sync").
// An interrupted run resumes automatically on the next tick or failover.
let workflow = Workflow::new(Resources::new())
    .register(SyncState::Start, StartTask)
    .register(SyncState::Apply, ApplyTask)
    .add_exit_state(SyncState::Done)
    .with_checkpoint_store(checkpoints);

let running = Scheduler::new()
    .every("sync", workflow, SyncState::Start, Duration::from_secs(60))
    .start()
    .await?;
```
<hr class="section-divider">

<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>Cancelling a flow</h2>
<p>
<code>RunningScheduler::cancel_flow(id)</code> asks the flow's in-flight run to stop and returns at once.
The run's <code>CancellationToken</code> fires, the workflow stops at its next await point, its
<a href="../saga/">saga compensation stack</a> rolls back the completed steps, and the flow returns to
<code>Status::Idle</code>. A <code>CompensatableTask</code> that is mid-run is never interrupted, and the
compensation drain always completes. A cancelled run that rolls back cleanly does not count as a failure:
the streak and any backoff window stay as they were. If a compensator fails, the run ends with
<code>CompensationFailed</code> and is handled as a failure (backoff or trip).
</p>
<p>
Cancelling a flow that is not running is a no-op; an unknown id is an error. Use <code>wait_idle</code> to
wait for the run to wind down.
</p>

```rust
// Stop the in-flight run of a flow; its saga rolls back and the flow goes Idle.
running.cancel_flow("order")?;

// cancel_flow returns at once; wait for the cancelled run to settle.
let info = running.wait_idle("order").await.expect("flow exists");
assert_eq!(info.status, Status::Idle);
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example scheduler_cancellation --features scheduler</code>.
It triggers a saga, cancels it mid-flight, and watches the compensators roll back in reverse.</p>
</div>
<hr class="section-divider">

<h2 id="graceful-shutdown"><a href="#graceful-shutdown" class="anchor-link" aria-hidden="true">#</a>Graceful shutdown</h2>
<p>
<code>stop()</code> shuts the scheduler down in bounded time. Every flow goes through each phase together,
so the worst case is the two timeouts plus teardown, however many flows are registered. When every run
finishes during the drain or the cancel phase, the later phases are skipped and teardown follows.
</p>
<div class="diagram-frame">
<p class="diagram-label">Stop sequence</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 44 780 173" role="img" aria-labelledby="sched-stop-title sched-stop-desc">
<title id="sched-stop-title">Stop sequence</title>
<desc id="sched-stop-desc">stop() runs four phases in order. Drain stops dispatching and waits up to the drain timeout, 20 seconds by default, for in-flight runs to finish. If runs remain, Cancel fires every run's token and waits up to the cancel timeout, 5 seconds by default. If runs still remain, Abort drops them, returns their flows to Idle, and makes stop return a timeout error. Teardown then runs every flow's resource teardown in reverse registration order. When every run has finished during Drain or Cancel, the sequence skips ahead to Teardown.</desc>
<defs><marker id="sched-stop-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<circle class="n" cx="14" cy="98" r="5"/>
<path class="e" d="M21,98 H36" marker-end="url(#sched-stop-ah)"/>
<rect class="n" x="40" y="60" width="150" height="76" rx="12"/>
<text class="t-strong" x="115" y="84">Drain</text>
<text class="t-mut" x="115" y="104">stops dispatching</text>
<text class="t-mut" x="115" y="122">waits for runs, 20s</text>
<path class="e" d="M190,98 H226" marker-end="url(#sched-stop-ah)"/>
<rect class="n-warn" x="230" y="60" width="150" height="76" rx="12"/>
<text class="t-strong" x="305" y="84">Cancel</text>
<text class="t-mut" x="305" y="104">fires run tokens</text>
<text class="t-mut" x="305" y="122">waits for runs, 5s</text>
<path class="e" d="M380,98 H416" marker-end="url(#sched-stop-ah)"/>
<rect class="n-err" x="420" y="60" width="150" height="76" rx="12"/>
<text class="t-strong" x="495" y="84">Abort</text>
<text class="t-mut" x="495" y="104">drops what is left</text>
<text class="t-mut" x="495" y="122">flows go Idle, Err</text>
<path class="e" d="M570,98 H606" marker-end="url(#sched-stop-ah)"/>
<rect class="n-ok" x="610" y="60" width="150" height="76" rx="12"/>
<text class="t-strong" x="685" y="84">Teardown</text>
<text class="t-mut" x="685" y="104">resources, reverse</text>
<text class="t-mut" x="685" y="122">registration order</text>
<path class="e e-dash" d="M115,136 V164 Q115,172 123,172 H677 Q685,172 685,164 V140" marker-end="url(#sched-stop-ah)"/>
<path class="e e-dash" d="M305,136 V172"/>
<text class="t-mut" x="400" y="194">every run finished: the later phases are skipped</text>
</svg>
</div>
</div>
<ol>
<li>Drain: stop dispatching (the interval and cron loops exit, <code>trigger</code> is rejected) and wait up
to the drain timeout for in-flight runs to finish on their own. The default is 20 seconds; set it with
<code>with_drain_timeout</code>.</li>
<li>Cancel: fire every in-flight run's <code>CancellationToken</code> and wait up to the cancel timeout.
Cancelled runs abort at their next await and drain their saga compensation stacks. The default is 5
seconds; set it with <code>with_cancel_timeout</code>.</li>
<li>Abort: drop whatever is still running, return those flows to <code>Status::Idle</code>, and make
<code>stop()</code> return a timeout error that names the aborted flows.</li>
<li>Teardown: run every flow's resource <code>teardown</code> in reverse registration order. This happens on
every path, including after an abort.</li>
</ol>

```rust
use std::time::Duration;

let running = Scheduler::new()
    .every("fetch", workflow, State::Start, Duration::from_secs(60))
    .with_drain_timeout(Duration::from_secs(20))
    .with_cancel_timeout(Duration::from_secs(5))
    .start()
    .await?;

// Resolves on Ctrl-C, or on SIGTERM on Unix.
cano::scheduler::shutdown_signal().await?;

// Drain, cancel, abort, teardown.
running.stop().await?;
```

<p>
<code>stop()</code> is idempotent: later calls from any clone return the first result, and
<code>wait()</code> returns that same result once some clone has called <code>stop()</code>. To cap the
whole sequence, wrap the call: <code>tokio::time::timeout(d, running.stop())</code>. If that future is
dropped mid-sequence, the remaining tasks are aborted, and a later <code>stop()</code> still tears the
resources down.
</p>
<p>
A <code>StreamTask</code> ends only when its token fires or its input ends, so a stream flow waits out the
whole drain timeout before the cancel phase reaches it. A scheduler that hosts stream flows usually sets
<code>with_drain_timeout(Duration::ZERO)</code>.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example scheduler_graceful_shutdown --features scheduler</code>.
It waits for Ctrl-C (or SIGTERM on Unix) with <code>shutdown_signal()</code>, then calls <code>stop()</code>
while flows are running.</p>
</div>
<hr class="section-divider">

<h2 id="multi-level-map-reduce"><a href="#multi-level-map-reduce" class="anchor-link" aria-hidden="true">#</a>Multi-level map-reduce</h2>
<p>
The scheduler combines with <a href="../split-join/">split/join</a> into a two-level map-reduce. Level 1
lives inside one workflow: a state registered with <code>register_split</code> fans a batch out across
parallel tasks, and a <code>JoinConfig</code> (for example <code>JoinStrategy::All</code> or
<code>Percentage(0.75)</code>) reduces their results into one summary state. Level 2 lives at the
scheduler: several such batch workflows, each with its own parameters, run as <code>manual</code> or
interval flows and are triggered concurrently. Each workflow carries its own
<a href="../resources/"><code>Resources</code></a>, so a shared accumulator (an
<code>Arc&lt;RwLock&lt;…&gt;&gt;</code> wrapped in a <code>Resource</code>) lets every batch append its
summary for a final reduce.
</p>

<p>The skeleton: one batch workflow with a parallel state, and a scheduler wiring up two batches.</p>

```rust
// Level 1: a workflow that map-reduces over one batch.
fn batch_workflow(batch: Vec<Item>, results: SharedResults) -> Workflow<State> {
    Workflow::new(Resources::new().insert(results))
        .register_split(
            State::Process,
            batch.iter().map(|item| ProcessTask::new(item)).collect::<Vec<_>>(),
            JoinConfig::new(JoinStrategy::All, State::Summarize)
                .with_timeout(Duration::from_secs(60)),
        )
        .register(State::Summarize, SummarizeTask) // appends a batch summary into `results`
        .add_exit_states(vec![State::Done, State::Error])
}

// Level 2: the scheduler runs several batch workflows concurrently.
let results = SharedResults::default();
let running = Scheduler::new()
    .manual("batch-a", batch_workflow(batch_a, results.clone()), State::Process)
    .manual("batch-b", batch_workflow(batch_b, results.clone()), State::Process)
    .start()
    .await?;
running.trigger("batch-a").await?;
running.trigger("batch-b").await?;
// Wait for both flows to finish, then reduce across all batch summaries in `results`.
running.wait_idle("batch-a").await;
running.wait_idle("batch-b").await;
running.stop().await?;

```

<div class="callout callout-tip">
<p>
The full runnable program is <code>examples/scheduler_mapreduce_books.rs</code>
(<code>cargo run --example scheduler_mapreduce_books --features scheduler</code>). It downloads several books
from Project Gutenberg, splits download and analysis across parallel tasks within each batch workflow, runs
multiple batch workflows concurrently, and reduces all results into global rankings. See also
<code>examples/scheduler_book_prepositions.rs</code> for the single-workflow variant.</p>
</div>

</div>
