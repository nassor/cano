+++
title = "Scheduler"
description = "Schedule Cano workflows with intervals, cron expressions, and manual triggers via the optional scheduler feature."
template = "page.html"
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
<li><a href="#backoff-and-trip">Backoff &amp; Trip State</a></li>
<li class="toc-sub"><a href="#backoff-policy">Attaching a Policy</a></li>
<li class="toc-sub"><a href="#status-variants">Status Variants</a></li>
<li class="toc-sub"><a href="#recovery">Recovery via reset_flow</a></li>
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
<code>cron</code> / <code>manual</code> and attach optional <code>BackoffPolicy</code> via
<code>set_backoff</code>. <code>Scheduler</code> is <strong>not</strong> <code>Clone</code>.</li>
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
struct DailyReportNode {
    report_type: String,
}

#[node(state = State)]
impl DailyReportNode {
    type PrepResult = Vec<String>;
    type ExecResult = String;

    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        println!("📊 Preparing {} report...", self.report_type);

        let store = res.get::<MemoryStore, _>("store")?;

        // Load data for report
        let data = vec!["metric1".to_string(), "metric2".to_string(), "metric3".to_string()];
        store.put("report_start", Utc::now().to_rfc3339())?;

        Ok(data)
    }

    async fn exec(&self, data: Self::PrepResult) -> Self::ExecResult {
        println!("📊 Generating report with {} records", data.len());

        // Generate report
        format!("{} report: {} records processed", self.report_type, data.len())
    }

    async fn post(&self, res: &Resources, result: Self::ExecResult) -> Result<State, CanoError> {
        println!("📊 Report completed: {}", result);
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("last_report", result)?;

        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Morning report workflow
    let morning_report = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, DailyReportNode {
            report_type: "Morning".to_string()
        })
        .add_exit_state(State::Complete);

    // Evening report workflow
    let evening_report = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, DailyReportNode { 
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
<hr class="section-divider">

<h2 id="backoff-and-trip"><a href="#backoff-and-trip" class="anchor-link" aria-hidden="true">#</a>Backoff &amp; Trip State</h2>
<p>
A flow that fails repeatedly can waste resources by re-firing on its base schedule.
Attach a <code>BackoffPolicy</code> to stretch the gap between failed runs and, optionally, trip the flow
after a streak limit so the scheduler stops dispatching it until you intervene.
</p>

<div class="callout callout-info">
<div class="callout-label">Opt-in</div>
<p>
Flows registered without a policy keep the existing <code>Status::Failed(err)</code> behavior — the next
scheduled tick fires per the base schedule. Defaults are <strong>never</strong> applied silently; you must
call <code>set_backoff</code> to activate the new code path.
</p>
</div>

<div class="callout callout-warning">
<div class="callout-label">Distinct from CircuitBreaker</div>
<p>
Flow-level <code>Tripped</code> is scoped to the scheduler and is separate from the task-level
<code>CanoError::CircuitOpen</code> emitted by a <a href="../resilience/#circuit-breaker"><code>CircuitBreaker</code></a>.
The breaker gates a single task's call to a dependency; this policy gates the scheduler from re-firing
an entire flow.
</p>
</div>

<h3 id="backoff-policy"><a href="#backoff-policy" class="anchor-link" aria-hidden="true">#</a>Attaching a Policy</h3>
<p>
Register the workflow normally, then call <code>set_backoff</code> <strong>before</strong> <code>start()</code>.
The policy controls four things: the initial delay after the first failure, the multiplier applied per
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

<h3 id="status-variants"><a href="#status-variants" class="anchor-link" aria-hidden="true">#</a>Status Variants</h3>
<p>
<code>Status</code> is <code>#[non_exhaustive]</code> — external <code>match</code> arms must include
a wildcard. The variants are:
</p>
<ul>
<li><code>Idle</code> — registered, never run or finished cleanly.</li>
<li><code>Running</code> — currently executing.</li>
<li><code>Completed</code> — last run reached an exit state.</li>
<li><code>Failed(String)</code> — last run errored. Used only when no policy is attached.</li>
<li><code>Backoff { until, streak, last_error }</code> — flow failed and is waiting until <code>until</code>
before its next dispatch. Set only when a policy is attached.</li>
<li><code>Tripped { streak, last_error }</code> — streak reached <code>streak_limit</code>; the scheduler
will not dispatch this flow again until <code>reset_flow</code> is called.</li>
</ul>
<p>
Outcome writes are atomic: observers never see a transient <code>Failed</code> flicker for a flow that
has a policy attached. <code>FlowInfo</code> exposes <code>failure_streak</code> and
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
running.trigger("batch-a")?;
running.trigger("batch-b")?;
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
