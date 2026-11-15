+++
title = "Scheduler"
description = "Schedule Cano workflows with intervals, cron expressions, and manual triggers via the optional scheduler feature."
template = "section.html"
+++

<div class="content-wrapper">

<h1>Scheduler</h1>
<p class="subtitle">Automate your workflows with flexible scheduling and concurrency.</p>
<p class="feature-tag">Behind the <code>scheduler</code> feature gate (<code>features = ["scheduler"]</code>).</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#lifecycle">Lifecycle: Scheduler &rarr; RunningScheduler</a></li>
<li><a href="#overlap-prevention">Overlap Prevention</a></li>
<li><a href="#scheduling-strategies">Scheduling Strategies</a></li>
<li><a href="#strategy-examples">Strategy Examples</a></li>
<li class="toc-sub"><a href="#interval-scheduling">Interval Scheduling</a></li>
<li class="toc-sub"><a href="#cron-scheduling">Cron Scheduling</a></li>
<li class="toc-sub"><a href="#manual-triggering">Manual Triggering</a></li>
<li class="toc-sub"><a href="#mixed-scheduling">Mixed Scheduling</a></li>
<li><a href="#backoff-guide">Backoff &amp; Trip State</a></li>
<li><a href="#cancellation">Cancelling a Flow</a></li>
<li><a href="#graceful-shutdown">Graceful Shutdown</a></li>
<li><a href="#multi-level-map-reduce">Advanced: Multi-Level Map-Reduce</a></li>
</ol>
</nav>

<p>
The Scheduler provides workflow scheduling capabilities for background jobs and automated workflows.
It supports intervals, cron expressions, and manual triggers. Each registered workflow carries a
<a href="../resources/"><code>Resources</code></a> dictionary whose <code>setup()</code> and
<code>teardown()</code> lifecycle hooks run once per <code>Scheduler::start()</code> /
<code>RunningScheduler::stop()</code> pair — not once per scheduled run.
</p>

<div class="callout callout-info">
<div class="callout-label">Mixed State Types</div>
<p>
<code>Scheduler</code> is not generic over the state type. One scheduler can host workflows with
different state enums and different resources; each flow keeps its own state type behind the scheduler's
type-erased registry.
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

<h2 id="lifecycle"><a href="#lifecycle" class="anchor-link" aria-hidden="true">#</a>Lifecycle: <code>Scheduler</code> &rarr; <code>RunningScheduler</code></h2>
<p>
The scheduler is split into two halves to make a double-start impossible at the type level:
</p>
<ul>
<li><strong><code>Scheduler</code></strong> is the <em>builder</em>. Every method consumes and returns it, so
registration is one chain: <code>every</code> / <code>cron</code> / <code>manual</code> register a workflow,
<code>with_backoff</code> overrides the <code>BackoffPolicy</code> of one flow, and
<code>with_drain_timeout</code> / <code>with_cancel_timeout</code> tune <a href="#graceful-shutdown">shutdown</a>.
Nothing fails while you chain: a zero interval, a bad cron expression, a duplicate id, or a
<code>with_backoff</code> for an unknown id is remembered, and <code>start()</code> returns the first one
before anything is validated or set up.</li>
<li><strong><code>RunningScheduler</code></strong> is the <em>live handle</em> returned by
<code>start().await?</code>. It is cheap to clone — every clone controls the same scheduler — so you can call
<code>trigger</code>, <code>status</code>, <code>list</code>, <code>reset_flow</code>, <code>cancel_flow</code>,
and <code>stop</code> from any task.</li>
</ul>
<p>
<code>start</code> consumes the builder, so the compiler prevents you from starting the same scheduler
twice or changing the registry mid-flight. It validates every workflow and its initial state, sets up each
workflow's resources in registration order (a setup failure tears down the flows already set up and
returns the error), then spawns one loop per interval or cron flow. Manual flows get no loop; they run only
on <code>trigger</code>.
</p>
<table class="styled-table">
<thead>
<tr>
<th>RunningScheduler method</th>
<th>What it does</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>async stop()</code></td>
<td>Shut down in bounded time: drain, cancel, abort, then tear resources down. Idempotent; see <a href="#graceful-shutdown">Graceful Shutdown</a>.</td>
</tr>
<tr>
<td><code>async wait()</code></td>
<td>Block until some clone calls <code>stop()</code>, then return its result. It never stops the scheduler itself.</td>
</tr>
<tr>
<td><code>async wait_idle(id)</code></td>
<td>Wait until the flow is not <code>Running</code> and return its <code>FlowInfo</code> (<code>None</code> for an unknown id). Returns at once for an idle flow.</td>
</tr>
<tr>
<td><code>trigger(id)</code></td>
<td>Start a run now. Errors when the id is unknown, the scheduler is stopping or stopped, the flow is already running, or the flow is tripped.</td>
</tr>
<tr>
<td><code>cancel_flow(id)</code></td>
<td>Cooperatively cancel the flow's in-flight run; a no-op when the flow is idle.</td>
</tr>
<tr>
<td><code>reset_flow(id)</code></td>
<td>Clear the failure streak and the backoff window; a tripped flow returns to <code>Idle</code>.</td>
</tr>
<tr>
<td><code>status(id)</code></td>
<td>Snapshot of one flow as <code>Option&lt;FlowInfo&gt;</code> (<code>None</code> for an unknown id).</td>
</tr>
<tr>
<td><code>list()</code></td>
<td>Snapshot of every flow as <code>Vec&lt;FlowInfo&gt;</code>, in registration order.</td>
</tr>
</tbody>
</table>
<p>
Only the three waiting methods are <code>async</code>. <code>trigger</code>, <code>cancel_flow</code> and
<code>reset_flow</code> must be called inside a Tokio runtime (<code>trigger</code> spawns the run).
Dropping the last clone without calling <code>stop()</code> cancels every in-flight run and aborts the
scheduler's tasks; resources are <strong>not</strong> torn down on that path, so call
<code>stop().await</code> for an orderly shutdown.
</p>
<hr class="section-divider">

<h2 id="overlap-prevention"><a href="#overlap-prevention" class="anchor-link" aria-hidden="true">#</a>Overlap Prevention</h2>
<p>
The scheduler never runs the same flow twice at once. Interval and cron loops run their workflow inline,
so a loop cannot start a second run while one is in progress, and a <code>trigger()</code> on a running
flow returns an error. An interval or cron tick that finds the flow already running because of a manual
trigger is skipped. This prevents resource exhaustion from slow-running workflows that would otherwise
accumulate concurrent instances.
</p>
<p>
For example, if a workflow is configured to run every 30 seconds but a particular execution takes
45 seconds, the next run starts 30 seconds after that execution finishes. A cron loop works the same way:
once a run returns, it waits for the next firing after that moment, so firings that fell inside the run
are skipped.
</p>
<hr class="section-divider">

<h2 id="scheduling-strategies"><a href="#scheduling-strategies" class="anchor-link" aria-hidden="true">#</a>Scheduling Strategies</h2>
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
<p>Trigger workflows on-demand via API.</p>

```rust
.manual(...)

```
</div>
</div>
<hr class="section-divider">

<h2 id="strategy-examples"><a href="#strategy-examples" class="anchor-link" aria-hidden="true">#</a>Scheduling Strategy Examples</h2>
<p>The Scheduler supports multiple scheduling strategies. Here are complete examples for each.</p>

<h3 id="interval-scheduling"><a href="#interval-scheduling" class="anchor-link" aria-hidden="true">#</a>1. Interval Scheduling - Fixed Time Intervals</h3>
<p>Run workflows at regular time intervals. The first run starts as soon as the scheduler starts; each later run starts one interval after the previous run finishes. Best for periodic tasks like health checks or data syncing.</p>

<div class="diagram-frame">
<p class="diagram-label">Interval Scheduling Timeline</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 760 180" role="img">
<title>Interval scheduling every 30 seconds: a 2-second run is followed by a 30-second wait, so each run starts 32 seconds after the previous one started.</title>
<defs><marker id="interval-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M150,36 H438" marker-start="url(#interval-ah)" marker-end="url(#interval-ah)"/>
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
<path class="e e-dim" d="M726,112 H744" marker-end="url(#interval-ah)"/>
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
<text class="t-mut" x="438" y="160">2s busy, then a full 30s sleep — the next run starts one interval after the previous run finishes</text>
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
        let store = res.get::<MemoryStore>()?;
        let status = "healthy".to_string();
        store.put("last_health_check", status);

        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    let workflow = Workflow::new(Resources::new().insert(store.clone()))
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

<h3 id="cron-scheduling"><a href="#cron-scheduling" class="anchor-link" aria-hidden="true">#</a>2. Cron Scheduling - Time-Based Expressions</h3>
<p>Run workflows based on cron expressions (seconds resolution, as parsed by the <code>cron</code> crate). A tick that falls inside a pending backoff window is skipped. Perfect for scheduled reports, backups, or time-specific tasks.</p>

<div class="diagram-frame">
<p class="diagram-label">Cron Scheduling Timeline</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 760 180" role="img">
<title>Cron scheduling: the workflow is idle through the day and runs for one hour at 09:00 and again at 18:00, repeating at the same wall-clock times daily.</title>
<defs><marker id="cron-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
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
<path class="e e-dim" d="M726,122 H744" marker-end="url(#cron-ah)"/>
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
<text class="t-mut" x="438" y="166">two 1-hour runs a day — idle the other 22 hours, then the same times tomorrow</text>
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

        let store = res.get::<MemoryStore>()?;

        // Load data for report
        let data = vec!["metric1".to_string(), "metric2".to_string(), "metric3".to_string()];
        store.put("report_start", Utc::now().to_rfc3339());

        println!("Generating report with {} records", data.len());
        let result = format!("{} report: {} records processed", self.report_type, data.len());

        println!("Report completed: {}", result);
        store.put("last_report", result);

        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Morning report workflow
    let morning_report = Workflow::new(Resources::new().insert(store.clone()))
        .register(State::Start, DailyReport {
            report_type: "Morning".to_string()
        })
        .add_exit_state(State::Complete);

    // Evening report workflow
    let evening_report = Workflow::new(Resources::new().insert(store.clone()))
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

<h3 id="manual-triggering"><a href="#manual-triggering" class="anchor-link" aria-hidden="true">#</a>3. Manual Triggering - On-Demand Execution</h3>
<p>Trigger workflows manually via API. Ideal for user-initiated tasks or event-driven processing.</p>

<div class="callout callout-info">
<div class="callout-label">Type-safe lifecycle</div>
<p>
<code>trigger()</code> lives on <code>RunningScheduler</code>, which is only obtained by calling
<code>start().await?</code>. The compiler will not let you trigger a workflow before the
scheduler is running. <code>trigger()</code> is synchronous: it starts the run and returns without waiting
for it to finish, so use <code>wait_idle(id)</code> or <code>status(id)</code> to see the outcome.
</p>
</div>

<div class="diagram-frame">
<p class="diagram-label">Manual Trigger Sequence</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 284" role="img">
<title>Manual triggering: an API request calls trigger on the running scheduler, which starts the workflow; the caller later sees the outcome through wait_idle.</title>
<defs><marker id="manual-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="110" y1="46" x2="110" y2="222"/>
<line class="lifeline" x1="360" y1="46" x2="360" y2="222"/>
<line class="lifeline" x1="610" y1="46" x2="610" y2="222"/>
<rect class="n" x="35" y="8" width="150" height="38" rx="8"/>
<text class="t-strong" x="110" y="28">API Request</text>
<rect class="n-hot" x="285" y="8" width="150" height="38" rx="8"/>
<text class="t-strong" x="360" y="28">Scheduler</text>
<rect class="n" x="535" y="8" width="150" height="38" rx="8"/>
<text class="t-strong" x="610" y="28">Workflow</text>
<path class="e" d="M110,78 H356" marker-end="url(#manual-ah)"/>
<text class="t-code" x="233" y="66">trigger("data_export")</text>
<path class="e" d="M360,122 H606" marker-end="url(#manual-ah)"/>
<text class="t-mut" x="483" y="110">Start Workflow</text>
<path class="e e-dash" d="M610,166 H364" marker-end="url(#manual-ah)"/>
<text class="t-mut t-ok" x="487" y="154">Complete</text>
<path class="e e-dash" d="M360,210 H114" marker-end="url(#manual-ah)"/>
<text class="t-mut t-ok" x="237" y="198">wait_idle(id) → Completed</text>
<rect class="n-hot" x="35" y="230" width="650" height="34" rx="10"/>
<text class="t-strong" x="360" y="248">Nothing runs until trigger() is called — no interval, no cron</text>
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
        let store = res.get::<MemoryStore>()?;
        let export_path = "/tmp/export.csv".to_string();
        store.put("export_path", export_path);

        println!("Export completed");
        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    let export_workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(State::Start, DataExportTask)
        .add_exit_state(State::Complete);

    // Register as manual-only workflow. start() returns a live, clone-able handle.
    let running = Scheduler::new()
        .manual("data_export", export_workflow, State::Start)
        .start()
        .await?;

    // trigger() is synchronous: it starts the run and returns.
    println!("Triggering export...");
    running.trigger("data_export")?;

    // wait_idle() returns once the run has finished.
    let info = running.wait_idle("data_export").await.expect("flow exists");
    println!("Export finished: {:?}", info.status);

    // Can be triggered again later
    running.trigger("data_export")?;
    running.wait_idle("data_export").await;

    // stop() drains in-flight runs, then tears the resources down.
    running.stop().await?;
    Ok(())
}

```

<h3 id="mixed-scheduling"><a href="#mixed-scheduling" class="anchor-link" aria-hidden="true">#</a>4. Mixed Scheduling - Combining Strategies</h3>
<p>Use multiple scheduling strategies together for complex automation scenarios.</p>

<div class="diagram-frame">
<p class="diagram-label">Mixed Strategy Overview</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 760 254" role="img">
<title>Mixed scheduling over one day: an interval flow pulses all day long, two cron flows take one hour each at 03:00 and 09:00, and a manual export fires once at 14:30.</title>
<defs><marker id="mixed-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
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
<path class="e e-dim" d="M726,190 H744" marker-end="url(#mixed-ah)"/>
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
<text class="t-mut" x="438" y="234">one scheduler, three trigger sources — interval, cron, and on-demand</text>
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
    let store = MemoryStore::new();

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
    let sync_workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(State::Start, DataSyncTask)
        .add_exit_state(State::Complete);

    // 2. Cron: Daily backup at 3 AM
    let backup_workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(State::Start, BackupTask)
        .add_exit_state(State::Complete);

    // 3. Cron: Weekly report on Mondays at 9 AM
    let report_workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(State::Start, WeeklyReportTask)
        .add_exit_state(State::Complete);

    // 4. Manual: Emergency data export
    let export_workflow = Workflow::new(Resources::new().insert(store.clone()))
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
                // running.trigger("emergency_export")?;
            }
        }
    }

    running.stop().await?;
    Ok(())
}

```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example scheduler_mixed_workflows --features scheduler</code> —
interval, cron, and manual flows side by side, plus a <code>trigger</code> on the manual one.</p>
</div>
<hr class="section-divider">

<h2 id="backoff-guide"><a href="#backoff-guide" class="anchor-link" aria-hidden="true">#</a>Backoff &amp; Trip State</h2>
<p>When a scheduled flow keeps failing, the scheduler backs it off and can trip it. How that works,
how to override the policy, the <code>Status</code> variants, and how to recover a tripped flow live
on a dedicated page:</p>
<ul>
<li><a href="backoff-and-trip/">Scheduler Backoff, Trip State &amp; Recovery</a></li>
</ul>
<hr class="section-divider">

<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>Cancelling a Flow</h2>
<p>
<code>RunningScheduler::cancel_flow(id)</code> requests cooperative cancellation of a flow's in-flight
run and returns immediately. The run's <code>CancellationToken</code> fires: the running workflow aborts at
its next await point, its <a href="../saga/">saga compensation stack</a> drains (rolling back completed
steps), and the flow returns to <code>Status::Idle</code>. A <code>CompensatableTask</code> that is
mid-run is never interrupted, and the compensation drain itself always completes. A deliberate cancel is
<strong>not</strong> counted as a failure against the
<a href="#backoff-guide">BackoffPolicy</a> — the streak is left untouched and the flow never trips, so
its next scheduled run fires normally. Cancelling a flow that isn't currently running is an idempotent
no-op; an unknown id is an error. Use <code>wait_idle</code> to wait for the run to wind down.
</p>

```rust
// Stop the in-flight run of a flow; its saga rolls back and the flow goes Idle.
running.cancel_flow("order")?;

// cancel_flow returns at once; wait for the cancelled run to settle.
let info = running.wait_idle("order").await.expect("flow exists");
assert_eq!(info.status, Status::Idle);
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example scheduler_cancellation --features scheduler</code> —
triggers a saga, cancels it mid-flight, and watches the compensators roll back in reverse.</p>
</div>
<hr class="section-divider">

<h2 id="graceful-shutdown"><a href="#graceful-shutdown" class="anchor-link" aria-hidden="true">#</a>Graceful Shutdown</h2>
<p>
<code>stop()</code> shuts the scheduler down in bounded time. Every flow goes through each phase together,
so the worst case is the two timeouts plus teardown, no matter how many flows are registered.
</p>
<ol>
<li><strong>Drain</strong> — stop dispatching (the interval and cron loops exit, <code>trigger</code> is
rejected) and wait up to the drain timeout for in-flight runs to finish on their own. Default 20 seconds,
set with <code>with_drain_timeout</code>.</li>
<li><strong>Cancel</strong> — fire every in-flight run's <code>CancellationToken</code> and wait up to the
cancel timeout. Cancelled runs abort at their next await and drain their saga compensation stacks.
Default 5 seconds, set with <code>with_cancel_timeout</code>.</li>
<li><strong>Abort</strong> — drop whatever is still running, return those flows to <code>Status::Idle</code>,
and make <code>stop()</code> return a timeout error that names the aborted flows.</li>
<li><strong>Teardown</strong> — run every flow's resource <code>teardown</code> in reverse registration
order. This happens on every path, including after an abort.</li>
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
<p>Runnable example: <code>cargo run --example scheduler_graceful_shutdown --features scheduler</code> —
waits for Ctrl-C (or SIGTERM on Unix) with <code>shutdown_signal()</code>, then calls <code>stop()</code>
and shuts down cleanly while flows are running.</p>
</div>
<hr class="section-divider">


<h2 id="multi-level-map-reduce"><a href="#multi-level-map-reduce" class="anchor-link" aria-hidden="true">#</a>Advanced Pattern: Multi-Level Map-Reduce</h2>
<p>
The scheduler composes naturally with <a href="../split-join/">split/join</a> to give you
a two-level map-reduce. <strong>Level 1</strong> lives inside a single workflow: a state registered with
<code>register_split</code> fans a batch out across parallel tasks, and a <code>JoinConfig</code> reduces
their results back into one summary state. <strong>Level 2</strong> lives at the scheduler: register several
of those batch workflows — each with different parameters (a different batch of records, a different region,
a different tenant) — as <code>manual</code> flows or interval flows, and trigger them concurrently. Because
every workflow carries its own <a href="../resources/"><code>Resources</code></a> dictionary, you hand each
one a shared accumulator (an <code>Arc&lt;RwLock&lt;…&gt;&gt;</code> wrapped in a <code>Resource</code>); every
batch independently appends its summary, and a final reduce step folds them together.
</p>

<ul>
<li><strong>Map (level 1)</strong> — <code>register_split</code> runs N tasks over a batch in parallel.</li>
<li><strong>Reduce (level 1)</strong> — <code>JoinConfig</code> (e.g. <code>JoinStrategy::All</code> or
<code>Percentage(0.75)</code>) merges the parallel results into a single batch summary.</li>
<li><strong>Map (level 2)</strong> — the scheduler holds several batch workflows and fires them concurrently
via <code>trigger</code> (or on intervals), each with its own parameters and <code>Resources</code>.</li>
<li><strong>Reduce (level 2)</strong> — a shared accumulator resource collects every batch summary; once all
flows finish, a final pass aggregates across batches.</li>
</ul>

<p>The skeleton — one batch workflow with a parallel state, plus a scheduler wiring up a couple of batches:</p>

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
running.trigger("batch-a")?;
running.trigger("batch-b")?;
// Wait for both flows to finish, then reduce across all batch summaries in `results`.
running.wait_idle("batch-a").await;
running.wait_idle("batch-b").await;
running.stop().await?;

```

<div class="callout callout-tip">
<p>
The full runnable program is <code>examples/scheduler_mapreduce_books.rs</code> —
<code>cargo run --example scheduler_mapreduce_books --features scheduler</code>. It downloads several books
from Project Gutenberg, splits download + analysis across parallel tasks within each batch workflow, runs
multiple batch workflows concurrently, and reduces all results into global rankings. See also
<code>examples/scheduler_book_prepositions.rs</code> for the single-workflow variant.</p>
</div>

</div>
