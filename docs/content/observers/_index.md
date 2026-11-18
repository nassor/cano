+++
title = "Observers and health probes"
description = "Subscribe to Cano workflow lifecycle and failure events, and report on the health of a workflow's resources."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Observers and health probes</h1>
<p class="subtitle">Lifecycle hooks for your workflows, and on-demand health for their dependencies.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.12.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>

<p>
Cano has two opt-in observability surfaces next to the <a href="../tracing/">tracing</a> integration
and the <a href="../metrics/">metrics</a> feature. This page is a reference for both, with a short
how-to for attaching an observer.
</p>
<ul>
<li><code>WorkflowObserver</code> is a set of synchronous callbacks for state entry, task start,
success and failure, retries, open circuit breakers, checkpoints, resume, cancellation, total timeout,
checkpoint clear failures and unknown resume states. It needs no feature flag.</li>
<li>Resource health is <code>Resource::health()</code> plus <code>Resources::check_all_health()</code>
and <code>aggregate_health()</code>, for reporting on databases, HTTP clients and other dependencies
on demand.</li>
</ul>
<p>
Observers are additive: attaching one does not change or suppress the <code>tracing</code> spans and
events the engine already emits.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#attaching">Attaching an observer</a></li>
<li><a href="#events">Lifecycle events</a></li>
<li><a href="#task-ids">Task identifiers</a></li>
<li><a href="#sync">Synchronous by design</a></li>
<li><a href="#tracing-observer">Built-in: TracingObserver</a></li>
<li><a href="#health">Resource health probes</a></li>
<li><a href="#full-example">Full example</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="attaching"><a href="#attaching" class="anchor-link" aria-hidden="true">#</a>Attaching an observer</h2>
<p>
Implement <code>WorkflowObserver</code> for any <code>Send + Sync + 'static</code> type, wrap it in
an <code>Arc</code>, and register it with <code>Workflow::with_observer</code>. Call it more than once
to register several observers; each event reaches every observer in registration order. A workflow
with no observers pays nothing per dispatch.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Done }

struct DoWork;

#[task(state = Step)]
impl DoWork {
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Done)
    }
}

#[derive(Default)]
struct TaskCounter(AtomicUsize);

// Override only the hooks you care about; the remaining eleven default to no-ops.
impl WorkflowObserver for TaskCounter {
    fn on_task_success(&self, task_id: &str) {
        let n = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        println!("task #{n} succeeded: {task_id}");
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let counter = Arc::new(TaskCounter::default());

    // Keep an Arc clone so main can read the counter after orchestrate() completes.
    let workflow = Workflow::bare()
        .register(Step::Start, DoWork)
        .add_exit_state(Step::Done)
        .with_observer(counter.clone());

    workflow.orchestrate(Step::Start).await?;
    assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    Ok(())
}
```
<hr class="section-divider">

<h2 id="events"><a href="#events" class="anchor-link" aria-hidden="true">#</a>Lifecycle events</h2>
<div class="meta-tags">
<span class="meta-tag">since 0.12.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>
<p>
<code>WorkflowObserver</code> has twelve hooks. Each has a default no-op body, so you override only the
ones you need.
</p>

<div class="diagram-frame">
<p class="diagram-label">Hook order across one state of an observed run</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 478" role="img" aria-labelledby="observers-order-title observers-order-desc">
<title id="observers-order-title">Hook order across one state</title>
<desc id="observers-order-desc">Three lifelines: the engine, the task and the workflow observer. The engine reports on_state_enter to the observer. When a checkpoint store is attached it then reports on_checkpoint. It reports on_task_start and runs attempt 1 on the task, which returns an error. The engine reports on_retry, runs attempt 2, which returns the next state, and reports on_task_success. Entering the next state reports on_state_enter again and the cycle repeats. A task that fails for good reports on_task_failure instead of on_task_success.</desc>
<defs><marker id="observers-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="120" y1="46" x2="120" y2="430"/>
<line class="lifeline" x1="390" y1="46" x2="390" y2="430"/>
<line class="lifeline" x1="650" y1="46" x2="650" y2="430"/>
<rect class="n" x="50" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="120" y="28">Engine</text>
<rect class="n" x="320" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="390" y="28">Task</text>
<rect class="n-hot" x="560" y="8" width="180" height="38" rx="8"/>
<text class="t-strong" x="650" y="28">WorkflowObserver</text>
<path class="e" d="M120,80 H646" marker-end="url(#observers-ah)"/>
<text class="t-code ta-s" x="136" y="68">on_state_enter("Load")</text>
<path class="e e-dash" d="M120,134 H646" marker-end="url(#observers-ah)"/>
<text class="t-mut ta-s" x="136" y="104">only with a checkpoint store</text>
<text class="t-code ta-s" x="136" y="122">on_checkpoint(id, seq)</text>
<path class="e" d="M120,174 H646" marker-end="url(#observers-ah)"/>
<text class="t-code ta-s" x="136" y="162">on_task_start("load")</text>
<path class="e" d="M120,208 H386" marker-end="url(#observers-ah)"/>
<text class="t-mut ta-s" x="136" y="196">attempt 1</text>
<path class="e e-dash" d="M390,242 H124" marker-end="url(#observers-ah)"/>
<text class="t-code t-err ta-e" x="374" y="230">Err(CanoError)</text>
<path class="e" d="M120,276 H646" marker-end="url(#observers-ah)"/>
<text class="t-code t-warn ta-s" x="136" y="264">on_retry("load", 1)</text>
<path class="e" d="M120,310 H386" marker-end="url(#observers-ah)"/>
<text class="t-mut ta-s" x="136" y="298">attempt 2</text>
<path class="e e-dash" d="M390,344 H124" marker-end="url(#observers-ah)"/>
<text class="t-code t-ok ta-e" x="374" y="332">Ok(next state)</text>
<path class="e" d="M120,378 H646" marker-end="url(#observers-ah)"/>
<text class="t-code t-ok ta-s" x="136" y="366">on_task_success("load")</text>
<path class="e" d="M120,418 H646" marker-end="url(#observers-ah)"/>
<text class="t-code ta-s" x="136" y="406">on_state_enter("Done")</text>
<text class="t-mut" x="390" y="458">A task that fails for good reports on_task_failure instead of on_task_success.</text>
</svg>
</div>
</div>

<div class="card-stack">
<div class="card">
<h3 id="on-state-enter"><a href="#on-state-enter" class="anchor-link" aria-hidden="true">#</a><code>on_state_enter(state: &amp;str)</code></h3>
<p>Fired each time the FSM enters a state, including the initial state and the exit state.
<code>state</code> is the <code>Debug</code> rendering of the state value.</p>
</div>
<div class="card">
<h3 id="on-task-start"><a href="#on-task-start" class="anchor-link" aria-hidden="true">#</a><code>on_task_start(task_id: &amp;str)</code></h3>
<p>Fired immediately before a task begins executing, before the retry loop. For a
split/join state it fires once per parallel task.</p>
</div>
<div class="card">
<h3 id="on-task-success"><a href="#on-task-success" class="anchor-link" aria-hidden="true">#</a><code>on_task_success(task_id: &amp;str)</code></h3>
<p>Fired when a task completes successfully, after any retries.</p>
</div>
<div class="card">
<h3 id="on-task-failure"><a href="#on-task-failure" class="anchor-link" aria-hidden="true">#</a><code>on_task_failure(task_id: &amp;str, err: &amp;CanoError)</code></h3>
<p>Fired when a task fails for good: retries exhausted, a per-attempt timeout that is not retried,
a panic, or rejection by an open circuit breaker. It also fires for an in-flight task that a
cancellation or the total timeout drops, before <code>on_cancelled</code> or <code>on_workflow_timeout</code>.</p>
</div>
<div class="card">
<h3 id="on-retry"><a href="#on-retry" class="anchor-link" aria-hidden="true">#</a><code>on_retry(task_id: &amp;str, attempt: u32)</code></h3>
<p>Fired before each retry. <code>attempt</code> is the 1-based number of the attempt that
just failed, so the retry that follows is attempt <code>attempt + 1</code>. A task with no retry
policy never fires it.</p>
</div>
<div class="card">
<h3 id="on-circuit-open"><a href="#on-circuit-open" class="anchor-link" aria-hidden="true">#</a><code>on_circuit_open(task_id: &amp;str)</code></h3>
<p>Fired when a <a href="../resilience/circuit-breakers/"><code>CircuitBreaker</code></a> attached via
<code>TaskConfig::with_circuit_breaker</code> rejects a call because it is open. An
<code>on_task_failure</code> carrying a <code>CanoError::CircuitOpen</code> follows. For retries,
timeouts, circuit breakers and bulkheads, see <a href="../resilience/">Resilience</a>.</p>
</div>
<div class="card">
<h3 id="on-checkpoint"><a href="#on-checkpoint" class="anchor-link" aria-hidden="true">#</a><code>on_checkpoint(workflow_id: &amp;str, sequence: u64)</code></h3>
<p>Fired after each <a href="../recovery/"><code>CheckpointRow</code></a> is durably appended. That
is one row per state entry (router states write none), plus a row for each saga completion and each
stepped or stream cursor save. It never fires without <code>Workflow::with_checkpoint_store</code>.</p>
</div>
<div class="card">
<h3 id="on-resume"><a href="#on-resume" class="anchor-link" aria-hidden="true">#</a><code>on_resume(workflow_id: &amp;str, sequence: u64)</code></h3>
<p>Fired once at the start of <a href="../recovery/"><code>Workflow::resume_from</code></a>, after
the run is rehydrated. <code>sequence</code> is the last persisted row's sequence; execution continues
from the state that row recorded.</p>
</div>
<div class="card">
<h3 id="on-workflow-timeout"><a href="#on-workflow-timeout" class="anchor-link" aria-hidden="true">#</a><code>on_workflow_timeout(elapsed: Duration, limit: Duration)</code></h3>
<p>Fired when the budget of <a href="../resilience/#workflow-total-timeout"><code>Workflow::with_total_timeout</code></a>
runs out before the FSM reaches an exit state. It fires once per trip, before the compensation drain.
The call then returns <code>CanoError::WithStateContext</code> wrapping a
<code>CanoError::WorkflowTimeout</code> after a clean rollback, or
<code>CanoError::CompensationFailed</code> when a <code>compensate</code> also fails
(its <code>errors[0]</code> is the wrapped timeout).</p>
</div>
<div class="card">
<h3 id="on-cancelled"><a href="#on-cancelled" class="anchor-link" aria-hidden="true">#</a><code>on_cancelled(state: &amp;str)</code></h3>
<p>Fired when a run is cancelled through a
<a href="../resilience/#cancellation"><code>CancellationToken</code></a>, whether the cancel is seen at a
state boundary or while a cancellable task is in flight. <code>state</code> is the <code>Debug</code>
rendering of the state where it was seen. It fires exactly once per cancelled run, before the
compensation drain. The call then returns <code>CanoError::WithStateContext</code> wrapping a
<code>CanoError::Cancelled</code> after a clean rollback, or <code>CanoError::CompensationFailed</code>
whose <code>errors[0]</code> is the wrapped cancel.</p>
</div>
<div class="card">
<h3 id="on-checkpoint-clear-failed"><a href="#on-checkpoint-clear-failed" class="anchor-link" aria-hidden="true">#</a><code>on_checkpoint_clear_failed(workflow_id: &amp;str, error: &amp;CanoError)</code></h3>
<p>Fired when clearing the checkpoint log fails, after a successful run or after a clean compensation
drain. The engine does not return the error, so the run keeps its own result. A stranded log can
later cause a duplicate-sequence error on a fresh run with the same workflow id, or a second
compensation on <code>resume_from</code>. Use this hook to log or count the failure.</p>
</div>
<div class="card">
<h3 id="on-unknown-resume-state"><a href="#on-unknown-resume-state" class="anchor-link" aria-hidden="true">#</a><code>on_unknown_resume_state(workflow_id: &amp;str, sequence: u64, unknown_state_label: &amp;str)</code></h3>
<p>Fired during <a href="../recovery/"><code>Workflow::resume_from</code></a> for each checkpoint row
whose state label is not registered on the current workflow, usually because a state was renamed or
removed without bumping <code>Workflow::with_workflow_version</code>. It fires while the run is
rehydrated, before <code>on_resume</code>. The engine drops that row from the rehydrated transition
path and continues the resume.</p>
</div>
</div>

<p>On a single-task state the hooks fire in this order:</p>
<ol>
<li><code>on_state_enter</code></li>
<li><code>on_checkpoint</code>, when a checkpoint store is attached and the state is not a router</li>
<li><code>on_task_start</code></li>
<li><code>on_retry</code> or <code>on_circuit_open</code>, zero or more times</li>
<li><code>on_task_success</code> or <code>on_task_failure</code></li>
</ol>
<p>
The next state then starts with its own <code>on_state_enter</code>. A split state fires
<code>on_task_start</code> for every parallel task, and <code>on_task_success</code> or
<code>on_task_failure</code> per task.
</p>
<hr class="section-divider">

<h2 id="task-ids"><a href="#task-ids" class="anchor-link" aria-hidden="true">#</a>Task identifiers</h2>
<p>
The <code>task_id</code> passed to the hooks comes from <code>Task::name()</code>. The default is
<code>std::any::type_name::&lt;Self&gt;()</code>, for example <code>"my_crate::tasks::FetchTask"</code>.
Parallel split tasks are reported as <code>"{name}[{index}]"</code>. Override <code>name()</code> to
give a task a stable, shorter label:
</p>

```rust
use cano::prelude::*;
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Done }

struct FetchOrders;

#[task(state = Step)]
impl FetchOrders {
    // Stable identifier passed as task_id to observer hooks, checkpoints, and metrics.
    fn name(&self) -> Cow<'static, str> {
        "fetch-orders".into()
    }
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Done)
    }
}
```
<hr class="section-divider">

<h2 id="sync"><a href="#sync" class="anchor-link" aria-hidden="true">#</a>Synchronous by design</h2>
<p>
Observer methods are synchronous and run inline on the workflow's task, so keep them cheap and
non-blocking. Every hook, including <code>on_retry</code> and <code>on_circuit_open</code>, runs under
<code>catch_unwind</code>. A panicking observer is reported as a <code>tracing</code> error event (a
one-line stderr notice without the feature) and never fails the task or the workflow.
</p>
<p>
When an event needs async work, such as a database write or a push to a metrics backend, hand it to a
channel and process it elsewhere:
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

struct ChannelObserver {
    tx: UnboundedSender<String>,
}

impl WorkflowObserver for ChannelObserver {
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        // Enqueue non-blockingly and return immediately; the async consumer handles I/O.
        let _ = self.tx.send(format!("{task_id} failed: {err}"));
    }
}

#[tokio::main]
async fn main() {
    let (tx, mut rx) = unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            eprintln!("alert sink: {msg}");
        }
    });

    // Attach this Arc<dyn WorkflowObserver> to any Workflow via .with_observer(observer).
    let _observer: Arc<dyn WorkflowObserver> = Arc::new(ChannelObserver { tx });
}
```
<hr class="section-divider">

<h2 id="tracing-observer"><a href="#tracing-observer" class="anchor-link" aria-hidden="true">#</a>Built-in: <code>TracingObserver</code></h2>
<div class="meta-tags">
<span class="meta-tag">since 0.12.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>
<p class="feature-tag">Behind the <code>tracing</code> feature gate (<code>features = ["tracing"]</code>).</p>

<p>
<code>TracingObserver</code> re-emits every hook as a flat <a href="../tracing/"><code>tracing</code></a>
event under the <code>cano::observer</code> target. It does not reproduce the nested span tree of the
engine's built-in instrumentation (<code>workflow_orchestrate</code>, <code>single_task_execution</code>,
<code>task_attempt</code>), so use it alongside that instrumentation, or alone when the high-level events
are enough. Filter its output with <code>RUST_LOG=cano::observer=debug</code>.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Done }

struct DoWork;

#[task(state = Step)]
impl DoWork {
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // Install a tracing subscriber in main() (e.g. tracing_subscriber::fmt::init()).
    // TracingObserver emits flat events under the `cano::observer` target.
    let workflow = Workflow::bare()
        .register(Step::Start, DoWork)
        .add_exit_state(Step::Done)
        .with_observer(Arc::new(TracingObserver::new()));

    workflow.orchestrate(Step::Start).await?;
    Ok(())
}
```

<p>
Each entry below shows the event level and fields. The message text follows in the definition.
</p>
<dl class="key-list">
<dt><code>on_state_enter</code> <small>DEBUG, field <code>state</code></small></dt>
<dd><code>"workflow entered state"</code></dd>

<dt><code>on_task_start</code> <small>DEBUG, field <code>task_id</code></small></dt>
<dd><code>"task started"</code></dd>

<dt><code>on_task_success</code> <small>INFO, field <code>task_id</code></small></dt>
<dd><code>"task succeeded"</code></dd>

<dt><code>on_task_failure</code> <small>ERROR, fields <code>task_id</code>, <code>error</code></small></dt>
<dd><code>"task failed"</code></dd>

<dt><code>on_retry</code> <small>WARN, fields <code>task_id</code>, <code>attempt</code></small></dt>
<dd><code>"task retry"</code></dd>

<dt><code>on_circuit_open</code> <small>WARN, field <code>task_id</code></small></dt>
<dd><code>"circuit breaker rejected task"</code></dd>

<dt><code>on_checkpoint</code> <small>DEBUG, fields <code>workflow_id</code>, <code>sequence</code></small></dt>
<dd><code>"checkpoint appended"</code></dd>

<dt><code>on_resume</code> <small>INFO, fields <code>workflow_id</code>, <code>sequence</code></small></dt>
<dd><code>"workflow resumed from checkpoint"</code></dd>

<dt><code>on_workflow_timeout</code> <small>WARN, fields <code>elapsed_ms</code>, <code>limit_ms</code></small></dt>
<dd><code>"workflow total timeout exceeded"</code></dd>

<dt><code>on_cancelled</code> <small>WARN, field <code>state</code></small></dt>
<dd><code>"workflow cancelled"</code></dd>

<dt><code>on_checkpoint_clear_failed</code> <small>WARN, fields <code>workflow_id</code>, <code>error</code></small></dt>
<dd><code>"checkpoint log clear failed"</code></dd>

<dt><code>on_unknown_resume_state</code> <small>WARN, fields <code>workflow_id</code>, <code>sequence</code>, <code>unknown_state_label</code></small></dt>
<dd>A message that starts <code>"checkpoint row references a state not registered on the current workflow"</code>.</dd>
</dl>
<hr class="section-divider">

<h2 id="health"><a href="#health" class="anchor-link" aria-hidden="true">#</a>Resource health probes</h2>
<div class="meta-tags">
<span class="meta-tag">since 0.12.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>
<p>
Every <a href="../resources/">Resource</a> has a <code>health()</code> method that defaults to
<code>HealthStatus::Healthy</code>. Override it to report on a connection pool, a downstream API or
anything else the workflow depends on. The engine never calls <code>health()</code> during normal
execution.
</p>

```rust
use cano::prelude::*;

struct ReadReplica;

#[resource]
impl Resource for ReadReplica {
    // setup() and teardown() keep their no-op defaults; health() runs only when queried.
    async fn health(&self) -> HealthStatus {
        let lag_secs = 8;
        if lag_secs > 30 {
            HealthStatus::Unhealthy(format!("replication lag {lag_secs}s"))
        } else if lag_secs > 2 {
            HealthStatus::Degraded(format!("replication lag {lag_secs}s"))
        } else {
            HealthStatus::Healthy
        }
    }
}
```

<p>
<code>HealthStatus</code> is <code>#[non_exhaustive]</code> with three variants,
<code>Healthy</code>, <code>Degraded(String)</code> and <code>Unhealthy(String)</code>, so a
<code>match</code> needs a wildcard arm.
</p>

<p>
<code>Resources::check_all_health()</code> returns a <code>Vec&lt;(Cow&lt;'static, str&gt;, HealthStatus)&gt;</code>
in insertion order. Each entry is labelled with its name, or with its type name when it was inserted
without one. <code>aggregate_health()</code> folds the same checks to the worst status
(<code>Healthy</code> &lt; <code>Degraded</code> &lt; <code>Unhealthy</code>). Both run the checks one
after another.
</p>

```rust
use cano::prelude::*;

#[derive(Resource)]
struct PrimaryDb;

// ReadReplica is defined above with a custom health() check.
#[tokio::main]
async fn main() {
    let resources = Resources::new()
        .insert(PrimaryDb)
        .insert(ReadReplica);

    // Sequential per-resource report in insertion order.
    for (label, status) in resources.check_all_health().await {
        println!("{label}: {status:?}");
    }

    // Worst status across all resources: Healthy < Degraded < Unhealthy.
    match resources.aggregate_health().await {
        HealthStatus::Healthy => println!("all good"),
        other => println!("attention needed: {other:?}"),
    }
}
```

<p>
A readiness endpoint is the usual consumer: build the workflow's <code>Resources</code> once, expose
<code>aggregate_health()</code> on an HTTP route, and return <code>503</code> when the result is not
<code>Healthy</code>.
</p>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full example</h2>
<p>
A task that fails twice and then succeeds, watched through every retry. The <code>workflow_observer</code>
example shipped with the crate is a fuller version that adds a circuit-breaker-guarded task and health
probes. Run it with <code>cargo run --example workflow_observer</code>.
</p>

```rust
use cano::prelude::*;
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
struct PrintingObserver {
    failures: AtomicUsize,
}

impl WorkflowObserver for PrintingObserver {
    fn on_state_enter(&self, state: &str) { println!("enter {state}"); }
    fn on_task_start(&self, task_id: &str) { println!("start {task_id}"); }
    fn on_task_success(&self, task_id: &str) { println!("ok    {task_id}"); }
    fn on_retry(&self, task_id: &str, attempt: u32) {
        println!("retry {task_id} (attempt {attempt} failed)");
    }
    fn on_circuit_open(&self, task_id: &str) { println!("circuit open: {task_id}"); }
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        println!("FAIL  {task_id}: {err}");
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Load, Done }

struct FlakyLoad {
    attempts: AtomicUsize,
}

#[task(state = Step)]
impl FlakyLoad {
    fn config(&self) -> TaskConfig {
        // 1 initial attempt + up to 3 retries, 20 ms apart.
        TaskConfig::new().with_fixed_retry(3, Duration::from_millis(20))
    }
    fn name(&self) -> Cow<'static, str> { "load".into() }
    async fn run(&self) -> Result<Step, CanoError> {
        // Fail the first 2 attempts, succeed on attempt 3 without underflowing the counter.
        if self.attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            return Err(CanoError::task_execution("upstream not ready yet"));
        }
        Ok(Step::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let observer = Arc::new(PrintingObserver::default());

    let workflow = Workflow::bare()
        .register(Step::Load, FlakyLoad { attempts: AtomicUsize::new(0) })
        .add_exit_state(Step::Done)
        .with_observer(observer.clone());

    // Attempts 1 & 2 fire on_retry; attempt 3 recovers and fires on_task_success (not on_task_failure).
    workflow.orchestrate(Step::Load).await?;
    assert_eq!(observer.failures.load(Ordering::Relaxed), 0);
    Ok(())
}
```
</div>
