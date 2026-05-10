+++
title = "Observers & Health Probes"
description = "Subscribe to Cano workflow lifecycle and failure events, and report on the health of a workflow's resources."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Observers &amp; Health Probes</h1>
<p class="subtitle">Lifecycle hooks for your workflows, and on-demand health for their dependencies.</p>

<p>
Cano gives you two opt-in observability surfaces that sit alongside the
<a href="../tracing/">tracing</a> integration:
</p>
<ul>
<li><strong><code>WorkflowObserver</code></strong> — synchronous callbacks fired at every
workflow lifecycle and failure event (state entry, task start/success/failure, retry,
circuit-open, and — when a <a href="../recovery/">checkpoint store</a> is attached —
checkpoint and resume). No feature flag, no <code>async-trait</code> overhead.</li>
<li><strong>Resource health</strong> — <code>Resource::health()</code> plus
<code>Resources::check_all_health()</code> / <code>aggregate_health()</code> to report on
the state of databases, HTTP clients, and other dependencies on demand.</li>
</ul>
<p>
Observers are <strong>additive</strong>: attaching one does not change or suppress the
<code>tracing</code> spans and events the engine already emits.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#attaching">Attaching an Observer</a></li>
<li><a href="#events">Lifecycle Events</a></li>
<li><a href="#task-ids">Task Identifiers</a></li>
<li><a href="#sync">Synchronous by Design</a></li>
<li><a href="#tracing-observer">Built-in: TracingObserver</a></li>
<li><a href="#health">Resource Health Probes</a></li>
<li><a href="#full-example">Full Example</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="attaching"><a href="#attaching" class="anchor-link" aria-hidden="true">#</a>Attaching an Observer</h2>
<p>
Implement <code>WorkflowObserver</code> for any <code>Send + Sync + 'static</code> type,
wrap it in an <code>Arc</code>, and register it with <code>Workflow::with_observer</code>.
Call it more than once to register several observers — each event is delivered to every
observer in registration order. Registration is O(1), and a workflow with no observers
pays nothing per dispatch.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Done }

#[derive(Clone)]
struct DoWork;

#[task(state = Step)]
impl DoWork {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[derive(Default)]
struct TaskCounter(AtomicUsize);

impl WorkflowObserver for TaskCounter {
    fn on_task_success(&self, task_id: &str) {
        let n = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        println!("task #{n} succeeded: {task_id}");
    }
}

let counter = Arc::new(TaskCounter::default());

let workflow = Workflow::bare()
    .register(Step::Start, DoWork)
    .add_exit_state(Step::Done)
    .with_observer(counter.clone());

workflow.orchestrate(Step::Start).await?;
assert_eq!(counter.0.load(Ordering::Relaxed), 1);
```
<hr class="section-divider">

<h2 id="events"><a href="#events" class="anchor-link" aria-hidden="true">#</a>Lifecycle Events</h2>
<p>
Every method on <code>WorkflowObserver</code> has a default no-op body — override only the
ones you care about.
</p>

<div class="card-stack">
<div class="card">
<h3 id="on-state-enter"><a href="#on-state-enter" class="anchor-link" aria-hidden="true">#</a><code>on_state_enter(state: &amp;str)</code></h3>
<p>Fired each time the FSM enters a state — including the initial state and the terminal
exit state. <code>state</code> is the <code>Debug</code> rendering of the state value.</p>
</div>
<div class="card">
<h3 id="on-task-start"><a href="#on-task-start" class="anchor-link" aria-hidden="true">#</a><code>on_task_start(task_id: &amp;str)</code></h3>
<p>Fired immediately before a task begins executing, before the retry loop. For a
split/join state, fired once per parallel task.</p>
</div>
<div class="card">
<h3 id="on-task-success"><a href="#on-task-success" class="anchor-link" aria-hidden="true">#</a><code>on_task_success(task_id: &amp;str)</code></h3>
<p>Fired when a task completes successfully (after any retries).</p>
</div>
<div class="card">
<h3 id="on-task-failure"><a href="#on-task-failure" class="anchor-link" aria-hidden="true">#</a><code>on_task_failure(task_id: &amp;str, err: &amp;CanoError)</code></h3>
<p>Fired when a task ultimately fails: retries exhausted, a non-retried per-attempt timeout,
a panic, or rejection by an open circuit breaker.</p>
</div>
<div class="card">
<h3 id="on-retry"><a href="#on-retry" class="anchor-link" aria-hidden="true">#</a><code>on_retry(task_id: &amp;str, attempt: u32)</code></h3>
<p>Fired before each retry. <code>attempt</code> is the 1-based number of the attempt that
just failed — the retry that follows is attempt <code>attempt + 1</code>.</p>
</div>
<div class="card">
<h3 id="on-circuit-open"><a href="#on-circuit-open" class="anchor-link" aria-hidden="true">#</a><code>on_circuit_open(task_id: &amp;str)</code></h3>
<p>Fired when a <code>CircuitBreaker</code> attached via
<code>TaskConfig::with_circuit_breaker</code> rejects a call because it is open. Followed by
an <code>on_task_failure</code> carrying a <code>CanoError::CircuitOpen</code>.</p>
</div>
<div class="card">
<h3 id="on-checkpoint"><a href="#on-checkpoint" class="anchor-link" aria-hidden="true">#</a><code>on_checkpoint(workflow_id: &amp;str, sequence: u64)</code></h3>
<p>Fired after each <a href="../recovery/"><code>CheckpointRow</code></a> is durably appended —
once per state entry, only on a workflow configured with <code>Workflow::with_checkpoint_store</code>.</p>
</div>
<div class="card">
<h3 id="on-resume"><a href="#on-resume" class="anchor-link" aria-hidden="true">#</a><code>on_resume(workflow_id: &amp;str, sequence: u64)</code></h3>
<p>Fired once at the start of <a href="../recovery/"><code>Workflow::resume_from</code></a>.
<code>sequence</code> is the last persisted row's sequence; execution continues from the state
that row recorded.</p>
</div>
</div>

<p>
On a single-task state the sequence is: <code>on_state_enter</code> → <code>on_task_start</code>
→ (zero or more <code>on_retry</code> / <code>on_circuit_open</code>) →
<code>on_task_success</code> <em>or</em> <code>on_task_failure</code> → <code>on_state_enter</code>
of the next state. Split states fire <code>on_task_start</code> for every parallel task and
then <code>on_task_success</code> / <code>on_task_failure</code> per task once the join
completes.
</p>
<hr class="section-divider">

<h2 id="task-ids"><a href="#task-ids" class="anchor-link" aria-hidden="true">#</a>Task Identifiers</h2>
<p>
The <code>task_id</code> passed to the hooks comes from <code>Task::name()</code>. The default
returns <code>std::any::type_name::&lt;Self&gt;()</code> (e.g.
<code>"my_crate::tasks::FetchTask"</code>); parallel split tasks are reported as
<code>"{name}[{index}]"</code>. Override <code>name()</code> to give a task a stable,
friendlier label:
</p>

```rust
use cano::prelude::*;
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Done }

#[derive(Clone)]
struct FetchOrders;

#[task(state = Step)]
impl FetchOrders {
    fn name(&self) -> Cow<'static, str> {
        "fetch-orders".into()
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}
```
<hr class="section-divider">

<h2 id="sync"><a href="#sync" class="anchor-link" aria-hidden="true">#</a>Synchronous by Design</h2>
<p>
Observer methods are <strong>synchronous</strong> and run inline on the workflow's task. Keep
them cheap and non-blocking. When an event needs async work — writing to a database, pushing
to a metrics backend — hand it off to a channel and process it elsewhere:
</p>

```rust
use cano::prelude::*;
use std::sync::mpsc::{Sender, channel};

struct ChannelObserver {
    tx: Sender<String>,
}

impl WorkflowObserver for ChannelObserver {
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        // Push and return — a separate task drains the receiver.
        let _ = self.tx.send(format!("{task_id} failed: {err}"));
    }
}

let (tx, _rx) = channel();
let observer: std::sync::Arc<dyn WorkflowObserver> =
    std::sync::Arc::new(ChannelObserver { tx });
```
<hr class="section-divider">

<h2 id="tracing-observer"><a href="#tracing-observer" class="anchor-link" aria-hidden="true">#</a>Built-in: <code>TracingObserver</code></h2>
<p class="feature-tag">Behind the <code>tracing</code> feature gate (<code>features = ["tracing"]</code>).</p>

<p>
<code>TracingObserver</code> is a ready-made observer that re-emits every hook as a
<a href="../tracing/"><code>tracing</code></a> event under the <code>cano::observer</code> target — it's a
bridge from these callbacks to the <a href="../tracing/">Tracing</a> integration's flat events (it does
<em>not</em> reproduce the engine's nested span tree). Wire it up in one line — no custom observer needed:
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Done }

#[derive(Clone)]
struct DoWork;

#[task(state = Step)]
impl DoWork {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

Workflow::bare()
    .register(Step::Start, DoWork)
    .add_exit_state(Step::Done)
    .with_observer(Arc::new(TracingObserver::new()))
```

<p>It emits <strong>events, not spans</strong> — it does not reproduce the nested span tree the
<code>tracing</code> feature's built-in instrumentation creates
(<code>workflow_orchestrate</code> → <code>single_task_execution</code> → <code>task_attempt</code>, …).
Use it alongside that instrumentation, or on its own when the high-level events are all you need.
Because the events carry the <code>cano::observer</code> target, you can filter them separately:
<code>RUST_LOG=cano::observer=debug</code>.</p>

<table>
<thead><tr><th>Hook</th><th>Level</th><th>Message</th><th>Fields</th></tr></thead>
<tbody>
<tr><td><code>on_state_enter</code></td><td><code>DEBUG</code></td><td><code>"workflow entered state"</code></td><td><code>state</code></td></tr>
<tr><td><code>on_task_start</code></td><td><code>DEBUG</code></td><td><code>"task started"</code></td><td><code>task_id</code></td></tr>
<tr><td><code>on_task_success</code></td><td><code>INFO</code></td><td><code>"task succeeded"</code></td><td><code>task_id</code></td></tr>
<tr><td><code>on_task_failure</code></td><td><code>ERROR</code></td><td><code>"task failed"</code></td><td><code>task_id</code>, <code>error</code></td></tr>
<tr><td><code>on_retry</code></td><td><code>WARN</code></td><td><code>"task retry"</code></td><td><code>task_id</code>, <code>attempt</code></td></tr>
<tr><td><code>on_circuit_open</code></td><td><code>WARN</code></td><td><code>"circuit breaker rejected task"</code></td><td><code>task_id</code></td></tr>
<tr><td><code>on_checkpoint</code></td><td><code>DEBUG</code></td><td><code>"checkpoint appended"</code></td><td><code>workflow_id</code>, <code>sequence</code></td></tr>
<tr><td><code>on_resume</code></td><td><code>INFO</code></td><td><code>"workflow resumed from checkpoint"</code></td><td><code>workflow_id</code>, <code>sequence</code></td></tr>
</tbody>
</table>
<hr class="section-divider">

<h2 id="health"><a href="#health" class="anchor-link" aria-hidden="true">#</a>Resource Health Probes</h2>
<p>
Every <a href="../resources/">Resource</a> has a <code>health()</code> method that defaults
to <code>HealthStatus::Healthy</code>. Override it to report on a connection pool, a
downstream API, or anything else the workflow depends on. <code>health()</code> is pure
observability — the engine never calls it during normal execution.
</p>

```rust
use cano::prelude::*;

struct ReadReplica;

#[resource]
impl Resource for ReadReplica {
    async fn health(&self) -> HealthStatus {
        // ... measure replication lag ...
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
<code>HealthStatus</code> is <code>#[non_exhaustive]</code> with three variants —
<code>Healthy</code>, <code>Degraded(String)</code>, <code>Unhealthy(String)</code> — so
always include a wildcard arm when matching it.
</p>

<p>
Aggregate the whole dictionary with <code>Resources::check_all_health()</code> (a
<code>HashMap&lt;TResourceKey, HealthStatus&gt;</code> — needs <code>TResourceKey: Clone</code>)
or fold straight to the worst status with <code>aggregate_health()</code>
(<code>Healthy</code> &lt; <code>Degraded</code> &lt; <code>Unhealthy</code>). Both run the
checks sequentially.
</p>

```rust
use cano::prelude::*;

// PrimaryDb and ReadReplica are Resource impls (see ReadReplica above).
let resources: Resources = Resources::new()
    .insert("db", PrimaryDb)
    .insert("replica", ReadReplica);

for (key, status) in resources.check_all_health().await {
    println!("{key}: {status:?}");
}

match resources.aggregate_health().await {
    HealthStatus::Healthy => println!("all good"),
    other => println!("attention needed: {other:?}"),
}
```

<p>
A natural use is a readiness endpoint: build the workflow's <code>Resources</code> once,
expose <code>aggregate_health()</code> on an HTTP route, and return <code>503</code> when it
is not <code>Healthy</code>.
</p>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full Example</h2>
<p>
A workflow with a flaky task and a circuit-breaker-guarded task, observed end to end, plus
resource health probes. This is the <code>workflow_observer</code> example shipped with the
crate — run it with <code>cargo run --example workflow_observer</code>.
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

struct FlakyLoad { remaining_failures: Arc<AtomicUsize> }

#[task(state = Step)]
impl FlakyLoad {
    fn config(&self) -> TaskConfig {
        TaskConfig::new().with_fixed_retry(3, Duration::from_millis(20))
    }
    fn name(&self) -> Cow<'static, str> { "load".into() }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        if self.remaining_failures.fetch_sub(1, Ordering::SeqCst) > 0 {
            return Err(CanoError::task_execution("upstream not ready yet"));
        }
        Ok(TaskResult::Single(Step::Done))
    }
}

let observer = Arc::new(PrintingObserver::default());

let workflow = Workflow::bare()
    .register(Step::Load, FlakyLoad { remaining_failures: Arc::new(AtomicUsize::new(2)) })
    .add_exit_state(Step::Done)
    .with_observer(observer.clone());

workflow.orchestrate(Step::Load).await?;
assert_eq!(observer.failures.load(Ordering::Relaxed), 0);
```
</div>
