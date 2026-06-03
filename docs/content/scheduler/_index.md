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
<li><a href="#graceful-shutdown">Graceful Shutdown</a></li>
<li><a href="#multi-level-map-reduce">Advanced: Multi-Level Map-Reduce</a></li>
</ol>
</nav>

<p>
The Scheduler provides workflow scheduling capabilities for background jobs and automated workflows.
It supports intervals, cron expressions, and manual triggers. Each registered workflow carries a
<a href="../resources/"><code>Resources</code></a> dictionary whose <code>setup()</code> and
<code>teardown()</code> lifecycle hooks run once per <code>scheduler.start()</code> /
<code>scheduler.stop()</code> call — not once per scheduled run.
</p>

<div class="callout callout-info">
<div class="callout-label">Type Constraint</div>
<p>
All workflows registered with a single <code>Scheduler</code> instance must share the same
<code>TState</code> type. The scheduler is generic over <code>Scheduler&lt;TState&gt;</code>,
so all registered workflows use the same state enum. For workflows with different state enums,
create separate <code>Scheduler</code> instances.
</p>
</div>
<hr class="section-divider">

<h2 id="lifecycle"><a href="#lifecycle" class="anchor-link" aria-hidden="true">#</a>Lifecycle: <code>Scheduler</code> &rarr; <code>RunningScheduler</code></h2>
<p>
The scheduler is split into two halves to make a double-start impossible at the type level:
</p>
<ul>
<li><strong><code>Scheduler</code></strong> is the <em>builder</em>. Register workflows with <code>every</code> /
<code>cron</code> / <code>manual</code> and, if you want something other than <code>BackoffPolicy::default()</code>,
override it per flow via <code>set_backoff</code>. <code>Scheduler</code> is <strong>not</strong> <code>Clone</code>.</li>
<li><strong><code>RunningScheduler</code></strong> is the <em>live handle</em> returned by
<code>scheduler.start().await?</code>. It owns the spawned driver and per-flow loop tasks. It is cheap to
clone — every clone shares the same command channel and flow registry, so you can call <code>trigger</code>,
<code>status</code>, <code>list</code>, <code>reset_flow</code>, and <code>stop</code> from any task.</li>
</ul>
<p>
<code>start</code> consumes the builder, so the compiler prevents you from starting the same scheduler
twice or mutating the registry mid-flight. <code>stop().await</code> on any clone signals graceful
shutdown and waits for it to complete; <code>wait().await</code> blocks without sending Stop, useful for
"main blocks until Ctrl+C handler stops the scheduler" patterns.
</p>
<hr class="section-divider">

<h2 id="overlap-prevention"><a href="#overlap-prevention" class="anchor-link" aria-hidden="true">#</a>Overlap Prevention</h2>
<p>
The scheduler prevents overlapping executions of the same workflow. If a previous execution is still
running when the next interval or cron trigger fires, the new run is skipped. This prevents resource
exhaustion from slow-running workflows that accumulate concurrent instances over time.
</p>
<p>
For example, if a workflow is configured to run every 30 seconds but a particular execution takes
45 seconds, the scheduler will skip the trigger at the 30-second mark and wait for the next interval
after the current run completes.
</p>
<hr class="section-divider">

<h2 id="scheduling-strategies"><a href="#scheduling-strategies" class="anchor-link" aria-hidden="true">#</a>Scheduling Strategies</h2>
<div class="mode-grid">
<div class="mode-card">
<div class="mode-icon" aria-hidden="true">⏱</div>
<h3>Interval</h3>
<p>Run workflows at fixed time intervals.</p>

```rust
scheduler.every_seconds(...)

```
</div>
<div class="mode-card">
<div class="mode-icon" aria-hidden="true">📅</div>
<h3>Cron</h3>
<p>Run workflows based on cron expressions.</p>

```rust
scheduler.cron(..., "0 0 9 * * *")

```
</div>
<div class="mode-card">
<div class="mode-icon" aria-hidden="true">👆</div>
<h3>Manual</h3>
<p>Trigger workflows on-demand via API.</p>

```rust
scheduler.manual(...)

```
</div>
</div>
<hr class="section-divider">

<h2 id="strategy-examples"><a href="#strategy-examples" class="anchor-link" aria-hidden="true">#</a>Scheduling Strategy Examples</h2>
<p>The Scheduler supports multiple scheduling strategies. Here are complete examples for each.</p>

<h3 id="interval-scheduling"><a href="#interval-scheduling" class="anchor-link" aria-hidden="true">#</a>1. Interval Scheduling - Fixed Time Intervals</h3>
<p>Run workflows at regular time intervals. Best for periodic tasks like health checks or data syncing.</p>

<div class="diagram-frame">
<p class="diagram-label">Interval Scheduling Timeline</p>
<div class="mermaid">
gantt
title Interval Scheduling (Every 30 seconds)
dateFormat ss
axisFormat %Ss
section Workflow
Run 1 :0, 2s
Wait  :2, 28s
Run 2 :30, 2s
Wait  :32, 28s
Run 3 :60, 2s
</div>
</div>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[derive(Clone)]
struct HealthCheckTask;

#[task(state = State)]
impl HealthCheckTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        println!("Running health check...");

        // Check system health
        let store = res.get::<MemoryStore, _>("store")?;
        let status = "healthy".to_string();
        store.put("last_health_check", status)?;

        Ok(TaskResult::Single(State::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, HealthCheckTask)
        .add_exit_state(State::Complete);

    // Run every 30 seconds
    scheduler.every_seconds("health_check", workflow, State::Start, 30)?;

    // start() consumes the builder and returns a clone-able RunningScheduler.
    // wait() blocks until somebody calls stop() on a clone.
    let running = scheduler.start().await?;
    running.wait().await?;
    Ok(())
}

```

<div class="callout callout-tip">
<p>Runnable examples: <code>cargo run --example scheduler_duration_scheduling --features scheduler</code>
(interval-only) and <code>cargo run --example scheduler_scheduling --features scheduler</code> (intervals
plus cron and manual flows).</p>
</div>

<h3 id="cron-scheduling"><a href="#cron-scheduling" class="anchor-link" aria-hidden="true">#</a>2. Cron Scheduling - Time-Based Expressions</h3>
<p>Run workflows based on cron expressions. Perfect for scheduled reports, backups, or time-specific tasks.</p>

<div class="diagram-frame">
<p class="diagram-label">Cron Scheduling Timeline</p>
<div class="mermaid">
gantt
title Cron Scheduling (Daily at 9 AM and 6 PM)
dateFormat HH
axisFormat %H:00
section Workflow
Run 1 :09, 1h
Run 2 :18, 1h
%% Add empty space to ensure full visibility
Space :20, 0h
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
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        println!("Preparing {} report...", self.report_type);

        let store = res.get::<MemoryStore, _>("store")?;

        // Load data for report
        let data = vec!["metric1".to_string(), "metric2".to_string(), "metric3".to_string()];
        store.put("report_start", Utc::now().to_rfc3339())?;

        println!("Generating report with {} records", data.len());
        let result = format!("{} report: {} records processed", self.report_type, data.len());

        println!("Report completed: {}", result);
        store.put("last_report", result)?;

        Ok(TaskResult::Single(State::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Morning report workflow
    let morning_report = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, DailyReport {
            report_type: "Morning".to_string()
        })
        .add_exit_state(State::Complete);

    // Evening report workflow
    let evening_report = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, DailyReport { 
            report_type: "Evening".to_string() 
        })
        .add_exit_state(State::Complete);

    // Run daily at 9 AM: "0 0 9 * * *"
    scheduler.cron("morning_report", morning_report, State::Start, "0 0 9 * * *")?;

    // Run daily at 6 PM: "0 0 18 * * *"
    scheduler.cron("evening_report", evening_report, State::Start, "0 0 18 * * *")?;

    let running = scheduler.start().await?;
    running.wait().await?;
    Ok(())
}

```

<h3 id="manual-triggering"><a href="#manual-triggering" class="anchor-link" aria-hidden="true">#</a>3. Manual Triggering - On-Demand Execution</h3>
<p>Trigger workflows manually via API. Ideal for user-initiated tasks or event-driven processing.</p>

<div class="callout callout-info">
<div class="callout-label">Type-safe lifecycle</div>
<p>
<code>trigger()</code> lives on <code>RunningScheduler</code>, which is only obtained by calling
<code>scheduler.start().await?</code>. The compiler will not let you trigger a workflow before the
scheduler is running.
</p>
</div>

<div class="diagram-frame">
<p class="diagram-label">Manual Trigger Sequence</p>
<div class="mermaid">
sequenceDiagram
participant API as API Request
participant S as Scheduler
participant W as Workflow
API->>S: trigger("data_export")
S->>W: Start Workflow
W-->>S: Complete
S-->>API: Success
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[derive(Clone)]
struct DataExportTask;

#[task(state = State)]
impl DataExportTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<State>, CanoError> {
        println!("Starting data export...");

        // Export data to CSV
        let store = res.get::<MemoryStore, _>("store")?;
        let export_path = "/tmp/export.csv".to_string();
        store.put("export_path", export_path)?;

        println!("Export completed");
        Ok(TaskResult::Single(State::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    let export_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, DataExportTask)
        .add_exit_state(State::Complete);

    // Register as manual-only workflow
    scheduler.manual("data_export", export_workflow, State::Start)?;

    // Start consumes the builder and returns a live, clone-able handle.
    let running = scheduler.start().await?;

    // Trigger manually when needed
    println!("Triggering export...");
    running.trigger("data_export").await?;

    // Can be triggered again later
    tokio::time::sleep(Duration::from_secs(5)).await;
    running.trigger("data_export").await?;

    // stop() sends the Stop command and waits for graceful shutdown.
    running.stop().await?;
    Ok(())
}

```

<h3 id="mixed-scheduling"><a href="#mixed-scheduling" class="anchor-link" aria-hidden="true">#</a>4. Mixed Scheduling - Combining Strategies</h3>
<p>Use multiple scheduling strategies together for complex automation scenarios.</p>

<div class="diagram-frame">
<p class="diagram-label">Mixed Strategy Overview</p>
<div class="mermaid">
gantt
title Mixed Scheduling Strategies
dateFormat HH:mm
axisFormat %H:%M
section Interval Tasks
Sync Every 5min :00:00, 24h
section Cron Tasks
Daily Backup :03:00, 1h
Weekly Report :09:00, 1h
section Manual Tasks
Emergency Export :done, 14:30, 15m
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Define simple tasks
    #[derive(Clone)]
    struct DataSyncTask;

    #[task(state = State)]
    impl DataSyncTask {
        async fn run(&self, _res: &Resources) -> Result<TaskResult<State>, CanoError> {
            println!("Syncing data...");
            Ok(TaskResult::Single(State::Complete))
        }
    }

    #[derive(Clone)]
    struct BackupTask;
    #[task(state = State)]
    impl BackupTask {
        async fn run(&self, _res: &Resources) -> Result<TaskResult<State>, CanoError> {
            println!("Running backup...");
            Ok(TaskResult::Single(State::Complete))
        }
    }

    #[derive(Clone)]
    struct WeeklyReportTask;

    #[task(state = State)]
    impl WeeklyReportTask {
        async fn run(&self, _res: &Resources) -> Result<TaskResult<State>, CanoError> {
            println!("Generating weekly report...");
            Ok(TaskResult::Single(State::Complete))
        }
    }

    #[derive(Clone)]
    struct EmergencyExportTask;

    #[task(state = State)]
    impl EmergencyExportTask {
        async fn run(&self, _res: &Resources) -> Result<TaskResult<State>, CanoError> {
            println!("Emergency export...");
            Ok(TaskResult::Single(State::Complete))
        }
    }

    // 1. Interval: Data sync every 5 minutes
    let sync_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, DataSyncTask)
        .add_exit_state(State::Complete);

    scheduler.every_seconds("data_sync", sync_workflow, State::Start, 300)?;

    // 2. Cron: Daily backup at 3 AM
    let backup_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, BackupTask)
        .add_exit_state(State::Complete);

    scheduler.cron("daily_backup", backup_workflow, State::Start, "0 0 3 * * *")?;

    // 3. Cron: Weekly report on Mondays at 9 AM
    let report_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, WeeklyReportTask)
        .add_exit_state(State::Complete);

    scheduler.cron("weekly_report", report_workflow, State::Start, "0 0 9 * * MON")?;

    // 4. Manual: Emergency data export
    let export_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, EmergencyExportTask)
        .add_exit_state(State::Complete);

    scheduler.manual("emergency_export", export_workflow, State::Start)?;

    // Start consumes the builder and returns a live handle.
    let running = scheduler.start().await?;

    // Monitor and trigger as needed
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;

        // Check status of all workflows
        let workflows = running.list().await;
        for info in workflows {
            println!("{}: {:?} (runs: {})", info.id, info.status, info.run_count);
        }

        // Example: Trigger emergency export if needed based on some condition
        // running.trigger("emergency_export").await?;
    }
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

<h2 id="graceful-shutdown"><a href="#graceful-shutdown" class="anchor-link" aria-hidden="true">#</a>Graceful Shutdown</h2>
<p>
The scheduler supports graceful shutdown, allowing currently running workflows to complete before stopping.
This includes workflows started by interval or cron triggers as well as manually-triggered workflows.
All active executions are tracked and included in the shutdown wait.
</p>

```rust
// Stop the scheduler and wait for running flows to finish.
running.stop().await?;

```

<p>
When <code>stop()</code> is called, the scheduler signals all scheduling loops to stop,
waits up to 30 seconds for any in-progress workflow executions to finish, and runs each
workflow's resource <code>teardown_all</code> in reverse registration order before returning.
A second <code>stop()</code> call after success is idempotent — it returns the same cached result.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example scheduler_graceful_shutdown --features scheduler</code> —
spawns a Ctrl-C handler, runs scheduled flows, and shuts down cleanly on signal.</p>
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
    Workflow::new(Resources::new().insert("results", results))
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
let mut scheduler = Scheduler::new();
scheduler.manual("batch-a", batch_workflow(batch_a, results.clone()), State::Start)?;
scheduler.manual("batch-b", batch_workflow(batch_b, results.clone()), State::Start)?;
let running = scheduler.start().await?;
running.trigger("batch-a").await?;
running.trigger("batch-b").await?;
// ...wait for both flows to finish, then reduce across all batch summaries in `results`.

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
