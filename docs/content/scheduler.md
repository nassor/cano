+++
title = "Scheduler"
description = "Schedule Cano workflows with intervals, cron expressions, and manual triggers via the optional scheduler feature."
template = "page.html"
+++

<div class="content-wrapper">

<h1>Scheduler</h1>
<p class="subtitle">Automate your workflows with flexible scheduling and concurrency.</p>

<div class="feature-banner">
<div class="banner-icon" aria-hidden="true">⚙️</div>
<div class="banner-content">
<p><strong>Feature flag required</strong> -- The scheduler is behind the <code>scheduler</code> feature gate.
Enable it with <code>features = ["scheduler"]</code> or <code>features = ["all"]</code> in your
<code>Cargo.toml</code>.</p>
</div>
</div>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
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
<code>CanoError::CircuitOpen</code> emitted by <a href="../task/#config-retries"><code>CircuitBreaker</code></a>.
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
Combine manual workflow triggering with split/join to create powerful multi-level map-reduce patterns. 
Each workflow processes a batch of data in parallel (workflow-level map-reduce), and multiple workflows 
run concurrently with different parameters (scheduler-level map-reduce).
</p>

<h3 id="architecture-overview"><a href="#architecture-overview" class="anchor-link" aria-hidden="true">#</a>Architecture Overview</h3>
<div class="diagram-frame">
<p class="diagram-label">Multi-Level Map-Reduce Architecture</p>
<div class="mermaid">
graph TB
subgraph "Scheduler Level (Map-Reduce)"
S[Scheduler] -->|Trigger| W1[Workflow: Batch-A-Classics]
S -->|Trigger| W2[Workflow: Batch-B-Adventure]
end
subgraph "Batch-A-Classics Workflow (Split/Join)"
W1 --> Init1[Init: 2 Books]
Init1 --> D1[Split: Download]
D1 --> D1A["Download: Pride &amp; Prejudice"]
D1 --> D1B[Download: Alice in Wonderland]
D1A --> J1D[Join: All Downloads]
D1B --> J1D
J1D --> A1[Split: Analyze]
A1 --> A1A[Analyze: Book 1]
A1 --> A1B[Analyze: Book 2]
A1A --> J1A[Join: 75% Complete]
A1B --> J1A
J1A --> Sum1[Summarize Batch A]
end
subgraph "Batch-B-Adventure Workflow (Split/Join)"
W2 --> Init2[Init: 2 Books]
Init2 --> D2[Split: Download]
D2 --> D2A[Download: Moby Dick]
D2 --> D2B[Download: Huck Finn]
D2A --> J2D[Join: All Downloads]
D2B --> J2D
J2D --> A2[Split: Analyze]
A2 --> A2A[Analyze: Book 1]
A2 --> A2B[Analyze: Book 2]
A2A --> J2A[Join: 75% Complete]
A2B --> J2A
J2A --> Sum2[Summarize Batch B]
end
Sum1 --> R["Global Reduce: Aggregate All Batches"]
Sum2 --> R
R --> F["Final Rankings and Statistics"]
style S fill:#4CAF50
style R fill:#2196F3
style F fill:#FF9800
style D1A fill:#E3F2FD
style D1B fill:#E3F2FD
style D2A fill:#E3F2FD
style D2B fill:#E3F2FD
style A1A fill:#FFF9C4
style A1B fill:#FFF9C4
style A2A fill:#FFF9C4
style A2B fill:#FFF9C4
</div>
</div>

<h3 id="book-analysis"><a href="#book-analysis" class="anchor-link" aria-hidden="true">#</a>Complete Example: Multi-Batch Book Analysis</h3>
<p>
This example demonstrates analyzing books from Project Gutenberg using a two-level map-reduce pattern.
Each batch workflow downloads and analyzes multiple books in parallel, then all results are aggregated globally.
</p>

```rust
use cano::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, timeout};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BookAnalysisState {
    Start,
    DownloadBatch,
    AnalyzeBatch,
    SummarizeBatch,
    Complete,
    Error,
}

// Book data structures
#[derive(Debug, Clone)]
struct Book {
    id: u32,
    title: String,
    content: String,
    batch_name: String,
}

#[derive(Debug, Clone)]
struct BookAnalysis {
    #[allow(dead_code)]
    book_id: u32,
    title: String,
    batch_name: String,
    preposition_count: usize,
    total_words: usize,
    unique_prepositions: HashSet<String>,
}

#[derive(Debug, Clone)]
struct BatchSummary {
    batch_name: String,
    total_books: usize,
    avg_prepositions: f64,
    total_unique_prepositions: usize,
    book_analyses: Vec<BookAnalysis>,
}

// Shared global state for collecting results from all batches
#[derive(Debug, Clone)]
struct GlobalResults {
    batch_summaries: Arc<RwLock<Vec<BatchSummary>>>,
}

impl GlobalResults {
    fn new() -> Self {
        Self {
            batch_summaries: Arc::new(RwLock::new(Vec::new())),
        }
    }

    async fn add_batch(&self, summary: BatchSummary) {
        let mut summaries = self.batch_summaries.write().await;
        summaries.push(summary);
    }

    async fn get_all_batches(&self) -> Vec<BatchSummary> {
        let summaries = self.batch_summaries.read().await;
        summaries.clone()
    }
}

const PREPOSITIONS: &[&str] = &[
    "about", "above", "across", "after", "against", "along", "among", "around",
    "at", "before", "behind", "below", "beneath", "beside", "between", "beyond",
    "by", "down", "during", "for", "from", "in", "into", "near", "of", "off",
    "on", "over", "through", "to", "toward", "under", "up", "with", "within",
];

type BookMetadata = (u32, String, String);

// Download a book from Project Gutenberg
async fn download_book(
    id: u32,
    title: String,
    url: String,
    batch_name: String,
) -> Result<Book, String> {
    println!("  📥 [{batch_name}] Downloading: {title}");

    let client = reqwest::Client::new();

    let download_future = async {
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch {url}: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error for {title}: {}", response.status()));
        }

        let content = response
            .text()
            .await
            .map_err(|e| format!("Failed to read content for {title}: {e}"))?;

        if content.len() < 1000 {
            return Err(format!("Content too short for {title}"));
        }

        println!(
            "  ✅ [{batch_name}] Downloaded: {title} ({} KB)",
            content.len() / 1024
        );

        Ok(Book {
            id,
            title: title.clone(),
            content,
            batch_name,
        })
    };

    timeout(Duration::from_secs(30), download_future)
        .await
        .map_err(|_| format!("Timeout downloading {title}"))?
}

// Analyze prepositions in a book
fn analyze_prepositions(book: &Book) -> BookAnalysis {
    let preposition_set: HashSet<&str> = PREPOSITIONS.iter().copied().collect();
    let mut found_prepositions = HashSet::new();

    let content_lower = book.content.to_lowercase();
    let words: Vec<&str> = content_lower
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphabetic()))
        .filter(|word| !word.is_empty())
        .collect();

    let total_words = words.len();

    for word in words {
        if preposition_set.contains(word) {
            found_prepositions.insert(word.to_string());
        }
    }

    BookAnalysis {
        book_id: book.id,
        title: book.title.clone(),
        batch_name: book.batch_name.clone(),
        preposition_count: found_prepositions.len(),
        total_words,
        unique_prepositions: found_prepositions,
    }
}

// Task: Initialize batch processing
#[derive(Clone)]
struct InitBatchTask {
    batch_name: String,
    books: Vec<BookMetadata>,
}

#[task(state = BookAnalysisState)]
impl InitBatchTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        println!(
            "\n🎯 [{0}] Initializing batch with {1} books",
            self.batch_name,
            self.books.len()
        );

        let store = res.get::<MemoryStore, _>("store")?;
        store.put("batch_name", self.batch_name.clone())?;
        store.put("book_metadata", self.books.clone())?;

        Ok(TaskResult::Single(BookAnalysisState::DownloadBatch))
    }
}

// Task: Download a single book (used in split)
#[derive(Clone)]
struct DownloadTask {
    book_id: u32,
    title: String,
    url: String,
    batch_name: String,
}

#[task(state = BookAnalysisState)]
impl DownloadTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        match download_book(
            self.book_id,
            self.title.clone(),
            self.url.clone(),
            self.batch_name.clone(),
        )
        .await
        {
            Ok(book) => {
                // Store individual book
                let store = res.get::<MemoryStore, _>("store")?;
                store.put(&format!("book_{}", self.book_id), book)?;
                Ok(TaskResult::Single(BookAnalysisState::AnalyzeBatch))
            }
            Err(e) => Err(CanoError::task_execution(format!("Download failed: {e}"))),
        }
    }
}

// Task: Analyze a single book (used after split)
#[derive(Clone)]
struct AnalyzeTask {
    book_id: u32,
}

#[task(state = BookAnalysisState)]
impl AnalyzeTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let book: Book = store
            .get(&format!("book_{}", self.book_id))
            .map_err(|e| CanoError::task_execution(format!("Book not found: {e}")))?;

        let analysis = analyze_prepositions(&book);

        println!(
            "  🔍 [{}] Analyzed '{}': {} prepositions",
            analysis.batch_name, analysis.title, analysis.preposition_count
        );

        // Store analysis
        store.put(&format!("analysis_{}", self.book_id), analysis)?;

        Ok(TaskResult::Single(BookAnalysisState::SummarizeBatch))
    }
}

// Task: Collect all analyses and create batch summary
#[derive(Clone)]
struct SummarizeBatchTask {
    global_results: GlobalResults,
}

#[task(state = BookAnalysisState)]
impl SummarizeBatchTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let batch_name: String = store.get("batch_name")?;
        let books: Vec<BookMetadata> = store.get("book_metadata")?;

        println!("  📊 [{batch_name}] Summarizing batch results...");

        // Collect all analyses
        let mut analyses = Vec::new();
        for (book_id, _, _) in &books {
            if let Ok(analysis) = store.get::<BookAnalysis>(&format!("analysis_{}", book_id)) {
                analyses.push(analysis);
            }
        }

        if analyses.is_empty() {
            return Err(CanoError::task_execution("No analyses found for batch"));
        }

        // Calculate batch statistics
        let total_books = analyses.len();
        let avg_prepositions = analyses
            .iter()
            .map(|a| a.preposition_count as f64)
            .sum::<f64>()
            / total_books as f64;

        // Collect all unique prepositions across batch
        let mut all_prepositions = HashSet::new();
        for analysis in &analyses {
            all_prepositions.extend(analysis.unique_prepositions.iter().cloned());
        }

        let summary = BatchSummary {
            batch_name: batch_name.clone(),
            total_books,
            avg_prepositions,
            total_unique_prepositions: all_prepositions.len(),
            book_analyses: analyses,
        };

        println!(
            "  ✅ [{batch_name}] Batch complete: {total_books} books, avg {avg_prepositions:.1} prepositions"
        );

        // Add to global results
        self.global_results.add_batch(summary).await;

        Ok(TaskResult::Single(BookAnalysisState::Complete))
    }
}

// Create workflow for a specific batch
fn create_batch_workflow(
    batch_name: String,
    books: Vec<BookMetadata>,
    global_results: GlobalResults,
) -> Workflow<BookAnalysisState> {
    let store = MemoryStore::new();

    Workflow::new(Resources::new().insert("store", store))
        .register(
            BookAnalysisState::Start,
            InitBatchTask {
                batch_name: batch_name.clone(),
                books: books.clone(),
            },
        )
        // Split: Download all books in parallel
        .register_split(
            BookAnalysisState::DownloadBatch,
            books
                .iter()
                .map(|(id, title, url)| DownloadTask {
                    book_id: *id,
                    title: title.clone(),
                    url: url.clone(),
                    batch_name: batch_name.clone(),
                })
                .collect::<Vec<_>>(),
            JoinConfig::new(
                JoinStrategy::All,
                BookAnalysisState::AnalyzeBatch,
            )
            .with_timeout(Duration::from_secs(120)),
        )
        // Split: Analyze all books in parallel
        .register_split(
            BookAnalysisState::AnalyzeBatch,
            books
                .iter()
                .map(|(id, _, _)| AnalyzeTask { book_id: *id })
                .collect::<Vec<_>>(),
            JoinConfig::new(
                JoinStrategy::Percentage(0.75), // Proceed if 75% complete
                BookAnalysisState::SummarizeBatch,
            )
            .with_timeout(Duration::from_secs(60)),
        )
        .register(
            BookAnalysisState::SummarizeBatch,
            SummarizeBatchTask {
                global_results: global_results.clone(),
            },
        )
        .add_exit_states(vec![BookAnalysisState::Complete, BookAnalysisState::Error])
}

// Reduce: Aggregate all batch results and display global rankings
async fn reduce_global_results(global_results: &GlobalResults) -> Result<(), CanoError> {
    println!("\n🌐 GLOBAL REDUCE: Aggregating results from all batches");
    println!("{}", "=".repeat(60));

    let batches = global_results.get_all_batches().await;

    if batches.is_empty() {
        return Err(CanoError::task_execution("No batches completed successfully"));
    }

    // Collect all book analyses
    let mut all_books: Vec<BookAnalysis> = batches
        .iter()
        .flat_map(|b| b.book_analyses.clone())
        .collect();

    // Sort by preposition count
    all_books.sort_by(|a, b| b.preposition_count.cmp(&a.preposition_count));

    // Display batch summaries
    println!("\n📦 Batch Summaries:");
    println!("{}", "-".repeat(60));
    for batch in &batches {
        println!("  Batch: {}", batch.batch_name);
        println!("    • Books processed: {}", batch.total_books);
        println!(
            "    • Avg prepositions: {:.1}",
            batch.avg_prepositions
        );
        println!(
            "    • Total unique prepositions: {}",
            batch.total_unique_prepositions
        );
    }

    // Display global rankings
    println!("\n🏆 Global Book Rankings (Top 10):");
    println!("{}", "-".repeat(60));
    for (rank, book) in all_books.iter().take(10).enumerate() {
        println!(
            "  #{}: {} [{}]",
            rank + 1,
            book.title,
            book.batch_name
        );
        println!(
            "      {} unique prepositions | {} total words",
            book.preposition_count, book.total_words
        );
    }

    // Global statistics
    let total_books = all_books.len();
    let avg_prepositions = all_books
        .iter()
        .map(|b| b.preposition_count as f64)
        .sum::<f64>()
        / total_books as f64;

    let mut all_unique_prepositions = HashSet::new();
    for book in &all_books {
        all_unique_prepositions.extend(book.unique_prepositions.iter().cloned());
    }

    println!("\n📈 Global Statistics:");
    println!("{}", "-".repeat(60));
    println!("  Total batches processed: {}", batches.len());
    println!("  Total books analyzed: {}", total_books);
    println!("  Average prepositions per book: {:.1}", avg_prepositions);
    println!(
        "  Total unique prepositions found: {}",
        all_unique_prepositions.len()
    );

    if let (Some(top), Some(bottom)) = (all_books.first(), all_books.last()) {
        println!("\n🥇 Most diverse: {} ({} prepositions)", top.title, top.preposition_count);
        println!("🥉 Least diverse: {} ({} prepositions)", bottom.title, bottom.preposition_count);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🚀 Multi-Level Map-Reduce Book Analysis");
    println!("{}", "=".repeat(60));
    println!("📚 Level 1: Workflow-level Split/Join (within each batch)");
    println!("🌐 Level 2: Scheduler-level Map-Reduce (across all batches)");
    println!("{}", "=".repeat(60));

    let mut scheduler = Scheduler::new();
    let global_results = GlobalResults::new();

    // Define book batches (different parameters per workflow)
    let batches = vec![
        (
            "Batch-A-Classics".to_string(),
            vec![
                (1342, "Pride and Prejudice".to_string(), 
                 "https://www.gutenberg.org/files/1342/1342-0.txt".to_string()),
                (11, "Alice's Adventures in Wonderland".to_string(), 
                 "https://www.gutenberg.org/files/11/11-0.txt".to_string()),
            ],
        ),
        (
            "Batch-B-Adventure".to_string(),
            vec![
                (2701, "Moby Dick".to_string(), 
                 "https://www.gutenberg.org/files/2701/2701-0.txt".to_string()),
                (76, "Huckleberry Finn".to_string(), 
                 "https://www.gutenberg.org/files/76/76-0.txt".to_string()),
            ],
        ),
    ];

    println!("\n📦 Preparing {} batches for processing\n", batches.len());

    // Register a workflow for each batch
    for (batch_name, books) in &batches {
        let workflow = create_batch_workflow(
            batch_name.clone(),
            books.clone(),
            global_results.clone(),
        );

        scheduler.manual(batch_name, workflow, BookAnalysisState::Start)?;
        println!(
            "  ✅ Registered workflow: {} ({} books)",
            batch_name,
            books.len()
        );
    }

    println!("\n🎬 Starting scheduler...\n");

    // Start consumes the builder and returns a clone-able handle.
    let running = scheduler.start().await?;

    // MAP PHASE: Trigger all batch workflows
    println!("🗺️  MAP PHASE: Triggering all batch workflows...\n");
    for (batch_name, _) in &batches {
        running.trigger(batch_name).await?;
    }

    // Wait for all workflows to complete
    println!("\n⏳ Waiting for all workflows to complete...\n");

    let mut all_complete = false;
    for attempt in 0..60 {
        tokio::time::sleep(Duration::from_secs(5)).await;

        let workflows = running.list().await;
        let completed_count = workflows
            .iter()
            .filter(|w| w.status == Status::Completed)
            .count();

        println!(
            "  📊 Progress: {}/{} workflows completed (attempt {})",
            completed_count,
            workflows.len(),
            attempt + 1
        );

        if completed_count == workflows.len() {
            all_complete = true;
            println!("  ✅ All workflows completed!");
            break;
        }
    }

    if !all_complete {
        println!("\n⚠️  Warning: Not all workflows completed in time");
    }

    // REDUCE phase: Aggregate results from all batches
    println!("\n🔄 REDUCE PHASE: Aggregating results from all batches...\n");
    reduce_global_results(&global_results).await?;

    // Stop scheduler — sends Stop and waits for graceful shutdown.
    println!("\n🛑 Stopping scheduler...");
    running.stop().await?;

    println!("\n✅ Multi-level map-reduce analysis complete!");

    Ok(())
}

```

<h3 id="key-concepts"><a href="#key-concepts" class="anchor-link" aria-hidden="true">#</a>Key Concepts</h3>
<div class="card-stack">
<div class="card">
<h3>Level 1: Workflow Split/Join</h3>
<p><strong>Map:</strong> Each workflow splits its work into parallel tasks (e.g., 3 records processed simultaneously)</p>
<p><strong>Reduce:</strong> Results join back together into a region summary</p>
</div>
<div class="card">
<h3>Level 2: Scheduler Map/Reduce</h3>
<p><strong>Map:</strong> Multiple workflows run concurrently, each with different parameters (different regions)</p>
<p><strong>Reduce:</strong> After all workflows complete, aggregate results across all regions</p>
</div>
<div class="card">
<h3>Shared State</h3>
<p>Use <code>Arc&lt;RwLock&lt;&gt;&gt;</code> to collect results from all workflows into a shared global state</p>
<p>Each workflow independently adds its summary to the global collection</p>
</div>
</div>

<h3 id="example-output"><a href="#example-output" class="anchor-link" aria-hidden="true">#</a>Example Output</h3>

```text
🚀 Multi-Level Map-Reduce Book Analysis
============================================================
📚 Level 1: Workflow-level Split/Join (within each batch)
🌐 Level 2: Scheduler-level Map-Reduce (across all batches)
============================================================

📦 Preparing 2 batches for processing

  ✅ Registered workflow: Batch-A-Classics (2 books)
  ✅ Registered workflow: Batch-B-Adventure (2 books)

🎬 Starting scheduler...

🗺️  MAP PHASE: Triggering all batch workflows...

🎯 [Batch-A-Classics] Initializing batch with 2 books
🎯 [Batch-B-Adventure] Initializing batch with 2 books
  📥 [Batch-A-Classics] Downloading: Pride and Prejudice
  📥 [Batch-A-Classics] Downloading: Alice's Adventures in Wonderland
  📥 [Batch-B-Adventure] Downloading: Moby Dick
  📥 [Batch-B-Adventure] Downloading: Huckleberry Finn
  ✅ [Batch-A-Classics] Downloaded: Pride and Prejudice (717 KB)
  ✅ [Batch-B-Adventure] Downloaded: Moby Dick (1246 KB)
  ✅ [Batch-A-Classics] Downloaded: Alice's Adventures in Wonderland (173 KB)
  ✅ [Batch-B-Adventure] Downloaded: Huckleberry Finn (419 KB)
  🔍 [Batch-A-Classics] Analyzed 'Pride and Prejudice': 34 prepositions
  🔍 [Batch-B-Adventure] Analyzed 'Moby Dick': 35 prepositions
  🔍 [Batch-A-Classics] Analyzed 'Alice's Adventures in Wonderland': 33 prepositions
  🔍 [Batch-B-Adventure] Analyzed 'Huckleberry Finn': 35 prepositions
  📊 [Batch-A-Classics] Summarizing batch results...
  ✅ [Batch-A-Classics] Batch complete: 2 books, avg 33.5 prepositions
  📊 [Batch-B-Adventure] Summarizing batch results...
  ✅ [Batch-B-Adventure] Batch complete: 2 books, avg 35.0 prepositions

⏳ Waiting for all workflows to complete...

  📊 Progress: 2/2 workflows completed (attempt 1)
  ✅ All workflows completed!

🔄 REDUCE PHASE: Aggregating results from all batches...

🌐 GLOBAL REDUCE: Aggregating results from all batches
============================================================

📦 Batch Summaries:
------------------------------------------------------------
  Batch: Batch-A-Classics
    • Books processed: 2
    • Avg prepositions: 33.5
    • Total unique prepositions: 34
  Batch: Batch-B-Adventure
    • Books processed: 2
    • Avg prepositions: 35.0
    • Total unique prepositions: 35

🏆 Global Book Rankings (Top 10):
------------------------------------------------------------
  #1: Moby Dick [Batch-B-Adventure]
      35 unique prepositions | 215136 total words
  #2: Huckleberry Finn [Batch-B-Adventure]
      35 unique prepositions | 111035 total words
  #3: Pride and Prejudice [Batch-A-Classics]
      34 unique prepositions | 122685 total words
  #4: Alice's Adventures in Wonderland [Batch-A-Classics]
      33 unique prepositions | 26444 total words

📈 Global Statistics:
------------------------------------------------------------
  Total batches processed: 2
  Total books analyzed: 4
  Average prepositions per book: 34.2
  Total unique prepositions found: 35

🥇 Most diverse: Moby Dick (35 prepositions)
🥉 Least diverse: Alice's Adventures in Wonderland (33 prepositions)

🛑 Stopping scheduler...

✅ Multi-level map-reduce analysis complete!

```

<div class="callout callout-tip">
<p>
<strong>💡 Use Case:</strong> This pattern is perfect for distributed data processing, multi-tenant systems, 
batch ETL jobs, or any scenario where you need to process independent datasets in parallel and then 
aggregate the results. Each workflow can have completely different parameters while sharing the aggregation logic.</p>
</div>

</div>

