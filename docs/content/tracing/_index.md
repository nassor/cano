+++
title = "Tracing"
description = "Span and event instrumentation for Cano workflows via the optional tracing feature."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Tracing</h1>
<p class="subtitle"><code>tracing</code>-crate span instrumentation for your workflows.</p>
<p class="feature-tag">Behind the <code>tracing</code> feature gate (<code>features = ["tracing"]</code>).</p>

<div class="callout callout-info">
<span class="callout-label">See also</span>
<p>This page covers Cano's built-in <code>tracing</code> spans and events. For the callback API
(<code>WorkflowObserver</code>) and the ready-made <code>TracingObserver</code> that re-emits those
callbacks as <code>tracing</code> events, see <a href="../observers/">Observers</a>. For the
<code>metrics</code> feature, see <a href="../metrics/">Metrics</a>.</p>
</div>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#setup">Setup</a></li>
<li><a href="#what-gets-traced">What gets traced</a></li>
<li><a href="#error-tracing">Error tracing</a></li>
<li class="toc-sub"><a href="#filtering">Filtering trace output</a></li>
<li><a href="#scheduler-tracing">Scheduler tracing</a></li>
<li><a href="#custom-spans">Custom spans</a></li>
<li><a href="#custom-instrumentation">Custom instrumentation</a></li>
<li><a href="#correlating-metrics">Correlating with metrics</a></li>
<li><a href="#example-output">Example output</a></li>
<li><a href="#full-example">Full example</a></li>
</ol>
</nav>

<p>
This page is a how-to for turning on Cano's <code>tracing</code> output, with a reference for the spans
and events the engine emits. It uses the
<a href="https://docs.rs/tracing/latest/tracing/" target="_blank">tracing</a> library. All
instrumentation is behind conditional compilation, so it costs nothing when the feature is off.
</p>

<p>
To get callbacks on lifecycle and failure events without depending on the <code>tracing</code>
ecosystem, see <a href="../observers/">Observers</a>. Attach
<code>.with_observer(Arc::new(TracingObserver::new()))</code> to re-emit those events as
<code>tracing</code> events under the <code>cano::observer</code> target.
</p>
<hr class="section-divider">

<h2 id="setup"><a href="#setup" class="anchor-link" aria-hidden="true">#</a>Setup</h2>
<p>Enable the <code>tracing</code> feature in your <code>Cargo.toml</code>. <code>features = ["all"]</code>
enables every optional feature (<code>scheduler</code>, <code>tracing</code>, <code>store-redb</code>,
<code>store-postgres</code>, <code>store-nats</code>, <code>metrics</code> and <code>testing</code>).</p>

```toml
[dependencies]
cano = { version = "@CANO_MINOR@", features = ["tracing"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

<h3 id="basic-init"><a href="#basic-init" class="anchor-link" aria-hidden="true">#</a>Basic initialization</h3>
<p>For development, install the default formatter.</p>

```rust
use tracing_subscriber;

// Simple setup for development
tracing_subscriber::fmt::init();
```

<h3 id="production-setup"><a href="#production-setup" class="anchor-link" aria-hidden="true">#</a>Production setup with an environment filter</h3>
<p>In production, add an environment filter so the <code>RUST_LOG</code> environment variable
controls log levels at runtime.</p>

```rust
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Production setup with env filter
tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into()))
    .with(tracing_subscriber::fmt::layer())
    .init();
```
<hr class="section-divider">

<h2 id="what-gets-traced"><a href="#what-gets-traced" class="anchor-link" aria-hidden="true">#</a>What gets traced</h2>
<div class="card-grid">
<div class="card">
<h3>Workflow</h3>
<p>A <code>workflow_orchestrate</code> span (<code>workflow_resume</code> for a resume). INFO events
at start and completion, a DEBUG event for each state executed.</p>
</div>
<div class="card">
<h3>Task</h3>
<p>A <code>single_task_execution</code> span per single-task state, a <code>run_with_retries</code>
span with <code>max_attempts</code>, and a <code>task_attempt</code> span per attempt. Retry delays
and circuit breaker transitions are events.</p>
</div>
<div class="card">
<h3>Split and saga</h3>
<p>A <code>split_task</code> span per parallel branch, a <code>compensatable_task_execution</code>
span per saga task, and events for each compensation, a failed compensation and a failed checkpoint
log clear.</p>
</div>
<div class="card">
<h3>Scheduler</h3>
<p>An <code>execute_flow</code> span around each scheduled run, an ERROR event when a scheduled
flow panics, and a DEBUG event when a backoff window suppresses a cron tick.</p>
</div>
</div>

<div class="diagram-frame">
<p class="diagram-label">The span tree one run emits: spans nest and fields inherit</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 24 780 275" role="img" aria-labelledby="tracing-spans-title tracing-spans-desc">
<title id="tracing-spans-title">Nested span tree of one workflow run</title>
<desc id="tracing-spans-desc">The root span workflow_orchestrate, which carries workflow_id when one is set, contains one single_task_execution span for each single-task state the run dispatches. Each of those contains a run_with_retries span that records max_attempts, and every attempt of the task gets its own task_attempt child span. The first state in the picture retried once, so it shows two task_attempt spans, the first failed and the second successful. The second state succeeded on its first attempt, so it shows one. Every event carries the fields of all the spans above it.</desc>
<rect class="n-hot" x="16" y="40" width="748" height="192" rx="10"/>
<text class="t-code ta-s" x="30" y="60">workflow_orchestrate { workflow_id }</text>
<text class="t-mut ta-e" x="750" y="60">root span: every child inherits its fields</text>
<rect class="n" x="32" y="74" width="374" height="144" rx="10"/>
<text class="t-code ta-s" x="46" y="94">single_task_execution</text>
<text class="t-mut ta-e" x="392" y="94">state 1, retried once</text>
<rect class="n" x="48" y="108" width="342" height="96" rx="10"/>
<text class="t-code ta-s" x="62" y="128">run_with_retries { max_attempts = 2 }</text>
<rect class="n-err" x="64" y="142" width="152" height="48" rx="10"/>
<text class="t-code" x="140" y="158">task_attempt</text>
<text class="t-mut" x="140" y="176">attempt = 1</text>
<rect class="n-ok" x="226" y="142" width="152" height="48" rx="10"/>
<text class="t-code" x="302" y="158">task_attempt</text>
<text class="t-mut" x="302" y="176">attempt = 2</text>
<rect class="n" x="420" y="74" width="328" height="144" rx="10"/>
<text class="t-code ta-s" x="434" y="94">single_task_execution</text>
<text class="t-mut ta-e" x="734" y="94">state 2</text>
<rect class="n" x="434" y="108" width="300" height="96" rx="10"/>
<text class="t-code ta-s" x="446" y="128">run_with_retries { max_attempts = 1 }</text>
<rect class="n-ok" x="450" y="142" width="268" height="48" rx="10"/>
<text class="t-code" x="584" y="158">task_attempt</text>
<text class="t-mut" x="584" y="176">attempt = 1</text>
<text class="t-mut" x="390" y="254">Each event carries the fields of every span above it, so workflow_id reaches the innermost attempt.</text>
<text class="t-mut" x="390" y="276">Scheduled runs nest under execute_flow, resumes under workflow_resume, split branches under split_task.</text>
</svg>
</div>
</div>
<hr class="section-divider">

<h2 id="error-tracing"><a href="#error-tracing" class="anchor-link" aria-hidden="true">#</a>Error tracing</h2>
<p>
The retry loop logs each failed attempt, so a failure is diagnosable without custom logging. A failed
attempt is a WARN. The attempt that reaches <code>max_attempts</code> is an ERROR, and the error then
returns to the caller as a <code>CanoError</code>.
</p>

<div class="diagram-frame">
<p class="diagram-label">WARN per failed attempt, ERROR when max_attempts is reached</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 28 780 277" role="img" aria-labelledby="tracing-severity-title tracing-severity-desc">
<title id="tracing-severity-title">Severity across a retried task</title>
<desc id="tracing-severity-desc">A run_with_retries span with max_attempts of 4 holds four attempts. Attempts 1, 2 and 3 each log a WARN, Task execution failed, will retry, and the engine waits the retry delay between them. Attempt 4 reaches max_attempts and logs an ERROR, Task execution failed after all retry attempts. The error then leaves the loop and is returned to the caller as a CanoError wrapped in WithStateContext, with no further ERROR event. An attempt that succeeds logs an INFO event and ends the loop.</desc>
<defs><marker id="tracing-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M172,122 H220" marker-end="url(#tracing-ah)"/>
<text class="t-mut" x="198" y="108">retry</text>
<path class="e" d="M364,122 H412" marker-end="url(#tracing-ah)"/>
<text class="t-mut" x="390" y="108">retry</text>
<path class="e e-hot" d="M556,122 H604" marker-end="url(#tracing-ah)"/>
<path class="e" d="M678,176 V204" marker-end="url(#tracing-ah)"/>
<text class="t-mut ta-e" x="668" y="192">returned to the caller</text>
<rect class="n" x="16" y="44" width="748" height="132" rx="10"/>
<text class="t-code ta-s" x="30" y="64">run_with_retries { max_attempts = 4 }</text>
<text class="t-mut t-hot" x="582" y="74">attempt reaches max_attempts</text>
<rect class="n-warn" x="32" y="88" width="140" height="68" rx="10"/>
<text class="t-code" x="102" y="110">attempt = 1</text>
<text class="t-strong t-warn" x="102" y="134">WARN</text>
<rect class="n-warn" x="224" y="88" width="140" height="68" rx="10"/>
<text class="t-code" x="294" y="110">attempt = 2</text>
<text class="t-strong t-warn" x="294" y="134">WARN</text>
<rect class="n-warn" x="416" y="88" width="140" height="68" rx="10"/>
<text class="t-code" x="486" y="110">attempt = 3</text>
<text class="t-strong t-warn" x="486" y="134">WARN</text>
<rect class="n-err" x="608" y="88" width="140" height="68" rx="10"/>
<text class="t-code" x="678" y="110">attempt = 4</text>
<text class="t-strong t-err" x="678" y="134">ERROR</text>
<rect class="n-err" x="16" y="208" width="748" height="56" rx="10"/>
<text class="t-code" x="390" y="227">Err(CanoError::WithStateContext)</text>
<text class="t-mut" x="390" y="247">carries the failing state; the engine logs no second ERROR for it</text>
<text class="t-mut" x="390" y="282">An attempt that succeeds logs INFO and ends the loop. ERROR is reserved for exhaustion.</text>
</svg>
</div>
</div>

<div class="card-stack">
<div class="card">
<h3 id="failed-attempts"><a href="#failed-attempts" class="anchor-link" aria-hidden="true">#</a>Failed task attempts</h3>
<p>Each failed attempt is logged as a WARN with the error and the attempt number. Transient failures
that a retry recovers stay visible.</p>

```text
WARN ...task_attempt{attempt=2 max_attempts=4}: Task execution failed, will retry error=... attempt=2 max_attempts=4
```
</div>
<div class="card">
<h3 id="retry-exhaustion"><a href="#retry-exhaustion" class="anchor-link" aria-hidden="true">#</a>Retry exhaustion</h3>
<p>When the last attempt fails, the loop logs an ERROR with the final attempt number. A task with no
retry policy has <code>max_attempts = 1</code>, so its first failure logs this ERROR.</p>

```text
ERROR ...task_attempt{attempt=4 max_attempts=4}: Task execution failed after all retry attempts error=... final_attempt=4 max_attempts=4
```
</div>
<div class="card">
<h3 id="workflow-errors"><a href="#workflow-errors" class="anchor-link" aria-hidden="true">#</a>Workflow-level errors</h3>
<p>The workflow span logs an INFO event when a run starts and when it reaches an exit state. A failed
run logs no completion event. Its error reaches the caller wrapped in
<code>CanoError::WithStateContext</code>, which names the state that failed.</p>

```text
INFO workflow_orchestrate: Starting workflow execution initial_state=FetchData
INFO workflow_orchestrate: Workflow completed successfully final_state=Done
```
</div>
</div>

<h3 id="filtering"><a href="#filtering" class="anchor-link" aria-hidden="true">#</a>Filtering trace output</h3>
<p>The <code>RUST_LOG</code> environment variable selects which modules emit output, which cuts noise in
production.</p>

<p>Linux and macOS:</p>

```bash
# Show only Cano debug logs
RUST_LOG=cano=debug cargo run

# Show Cano info and your app's debug logs
RUST_LOG=cano=info,my_app=debug cargo run

# Show retry-loop details
RUST_LOG=cano::task=debug cargo run

# Silence everything except errors
RUST_LOG=error cargo run
```

<p>Windows (PowerShell):</p>

```powershell
# Show only Cano debug logs
$env:RUST_LOG = "cano=debug"; cargo run

# Show Cano info and your app's debug logs
$env:RUST_LOG = "cano=info,my_app=debug"; cargo run

# Show retry-loop details
$env:RUST_LOG = "cano::task=debug"; cargo run

# Silence everything except errors
$env:RUST_LOG = "error"; cargo run
```
<hr class="section-divider">

<h2 id="scheduler-tracing"><a href="#scheduler-tracing" class="anchor-link" aria-hidden="true">#</a>Scheduler tracing</h2>
<p>
With the <code>scheduler</code> and <code>tracing</code> features both on, every scheduled run executes
inside an <code>execute_flow</code> span. The workflow's own events (<code>Starting workflow execution</code>,
the task spans, <code>Workflow completed successfully</code>) nest under it. A scheduled run has no
<code>workflow_orchestrate</code> span.
</p>
<ul>
<li>A panic in a scheduled flow logs an ERROR, <code>scheduled flow panicked</code>, with the panic message.</li>
<li>A cron tick that falls inside a backoff window logs a DEBUG event, <code>cron tick suppressed by backoff window</code>, with the flow id and the time the flow is eligible again.</li>
<li>For a split state, each parallel branch runs in a <code>split_task</code> span whose <code>task_id</code> field is the branch index.</li>
</ul>

```toml
[dependencies]
cano = { version = "@CANO_MINOR@", features = ["scheduler", "tracing"] }
```
<hr class="section-divider">

<h2 id="custom-spans"><a href="#custom-spans" class="anchor-link" aria-hidden="true">#</a>Custom spans</h2>
<p>Attach your own span to a workflow to add business context. It replaces the default
<code>workflow_orchestrate</code> span, and everything the run logs happens inside it.</p>

```rust
use tracing::info_span;
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MyState { Start, Complete }

#[derive(Clone)]
struct MyProcessor;

#[task(state = MyState)]
impl MyProcessor {
    async fn run(&self) -> Result<MyState, CanoError> {
        Ok(MyState::Complete)
    }
}

fn build_workflow() -> Workflow<MyState> {
    // Create workflow with custom tracing span
    let workflow_span = info_span!(
        "user_data_processing",
        user_id = "12345",
        batch_id = "batch_001"
    );

    let board = Blackboard::new();
    Workflow::new(Resources::new().insert(board))
        .with_tracing_span(workflow_span)
        .register(MyState::Start, MyProcessor)
        .add_exit_state(MyState::Complete)
}
```
<hr class="section-divider">

<h2 id="custom-instrumentation"><a href="#custom-instrumentation" class="anchor-link" aria-hidden="true">#</a>Custom instrumentation</h2>
<p>Use the <code>tracing</code> macros directly inside a task to add domain context that Cano's own
instrumentation does not cover. Events logged in <code>run</code> nest under the
<code>task_attempt</code> span.</p>

<h3 id="instrument-task"><a href="#instrument-task" class="anchor-link" aria-hidden="true">#</a>Instrumenting a task</h3>

```rust
use cano::prelude::*;
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AppState { Process, Confirm }

async fn process_payment(_order_id: &str) -> Result<String, CanoError> {
    Ok("receipt-1234".to_string())
}

#[derive(Clone)]
struct PaymentTask;

#[task(state = AppState)]
impl PaymentTask {
    async fn run(&self, res: &Resources) -> Result<AppState, CanoError> {
        let board = res.get::<Blackboard>()?;
        let order_id: String = board.get("order_id")?;
        info!(order_id = %order_id, "Processing payment");

        match process_payment(&order_id).await {
            Ok(receipt) => {
                info!(order_id = %order_id, receipt = %receipt, "Payment succeeded");
                board.put("receipt", receipt);
                Ok(AppState::Confirm)
            }
            Err(e) => {
                warn!(order_id = %order_id, error = %e, "Payment failed");
                Err(CanoError::task_execution(format!("payment failed: {e}")))
            }
        }
    }
}
```
<hr class="section-divider">

<h2 id="correlating-metrics"><a href="#correlating-metrics" class="anchor-link" aria-hidden="true">#</a>Correlating with metrics</h2>
<p>
With the <a href="../metrics/"><code>metrics</code></a> feature also on, spans can label metrics. Wire the
<a href="https://docs.rs/metrics-tracing-context/" target="_blank"><code>metrics-tracing-context</code></a>
crate into your application: a <code>TracingContextLayer</code> around the metrics recorder and a
<code>MetricsLayer</code> on the <code>tracing-subscriber</code> registry. Every <code>cano_*</code>
metric emitted while a span is entered then inherits that span's fields as labels.
</p>
<p>
A custom span from <code>with_tracing_span</code>, or one attached to the <code>orchestrate()</code>
future with <code>.instrument(span)</code>, therefore puts fields such as <code>request_id</code> or
<code>tenant</code> on the metrics of that run. Cano's default <code>workflow_orchestrate</code> and
<code>workflow_resume</code> spans carry <code>workflow_id</code> when <code>with_workflow_id</code> is
set, so it appears in the trace output and as a metric label.
</p>
<p>
Attach a span with <code>tracing::Instrument</code>. Never hold a <code>span.enter()</code> guard
across the <code>.await</code>.
</p>

```rust
use tracing::Instrument;

workflow
    .orchestrate(State::Start)
    .instrument(tracing::info_span!("api_request", request_id = "abc"))
    .await?;
```

<div class="callout callout-info">
<span class="callout-label">See also</span>
<p>
<a href="../metrics/#correlating-traces">Metrics: Correlating with traces</a> has the full layer
wiring, the cardinality caveat for <code>TracingContextLayer::all()</code>, and the runnable
<code>metrics_tracing_context</code> example
(<code>cargo run --example metrics_tracing_context --features "metrics tracing"</code>).
</p>
</div>
<hr class="section-divider">

<h2 id="example-output"><a href="#example-output" class="anchor-link" aria-hidden="true">#</a>Example output</h2>
<p>This is the output of a one-state workflow at <code>RUST_LOG=info</code>, with timestamps removed:</p>
<div class="trace-output">

```text
INFO workflow_orchestrate: Starting workflow execution initial_state=Start
INFO workflow_orchestrate:single_task_execution:run_with_retries{max_attempts=1}: Starting task execution with retry logic max_attempts=1
INFO workflow_orchestrate:single_task_execution:run_with_retries{max_attempts=1}:task_attempt{attempt=1 max_attempts=1}: Processing order...
INFO workflow_orchestrate:single_task_execution:run_with_retries{max_attempts=1}:task_attempt{attempt=1 max_attempts=1}: Task execution successful attempt=1
INFO workflow_orchestrate: Workflow completed successfully final_state=Complete
```
</div>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full example</h2>

```rust
use cano::prelude::*;
use tracing::{info, info_span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State {
    Start,
    Complete,
}

#[derive(Clone)]
struct ProcessOrder;

#[task(state = State)]
impl ProcessOrder {
    async fn run(&self) -> Result<State, CanoError> {
        info!("Processing order...");
        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // 1. Set up the subscriber with an env filter
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let board = Blackboard::new();

    // 2. Create the workflow with a custom span.
    // The span wraps every event this workflow logs.
    let workflow_span = info_span!(
        "order_processing",
        order_id = "ORD-2025-001",
        customer = "Acme Corp"
    );

    let workflow = Workflow::new(Resources::new().insert(board))
        .register(State::Start, ProcessOrder)
        .add_exit_state(State::Complete)
        .with_tracing_span(workflow_span);

    // 3. Run
    info!("Submitting order...");
    workflow.orchestrate(State::Start).await?;

    Ok(())
}
```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example tracing_demo --features "tracing scheduler"</code> sets
up a <code>tracing-subscriber</code>, runs a workflow and a scheduled flow, and prints the structured
span output. Pair it with <code>TracingObserver</code> (see <a href="../observers/#tracing-observer">Observers</a>)
for flat lifecycle events alongside the spans.</p>
</div>
