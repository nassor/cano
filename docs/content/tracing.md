+++
title = "Tracing"
description = "Comprehensive observability for Cano workflows via the optional tracing feature."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Tracing</h1>
<p class="subtitle"><code>tracing</code>-crate span instrumentation for your workflows.</p>
<p class="feature-tag">Behind the <code>tracing</code> feature gate (<code>features = ["tracing"]</code>).</p>

<div class="callout callout-info">
<span class="callout-label">See also</span>
<p>This page covers Cano's built-in <code>tracing</code> <em>spans</em>. For the synchronous callback API
(<code>WorkflowObserver</code>) and the ready-made <code>TracingObserver</code> that re-emits those callbacks
as <code>tracing</code> events, see <a href="../observers/">Observers</a>.</p>
</div>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#setup">Setup</a></li>
<li><a href="#what-gets-traced">What Gets Traced</a></li>
<li><a href="#error-tracing">Error Tracing</a></li>
<li class="toc-sub"><a href="#filtering">Filtering Trace Output</a></li>
<li><a href="#scheduler-tracing">Scheduler Tracing</a></li>
<li><a href="#custom-spans">Custom Spans</a></li>
<li><a href="#custom-instrumentation">Custom Instrumentation</a></li>
<li><a href="#example-output">Example Output</a></li>
<li><a href="#full-example">Full Example</a></li>
</ol>
</nav>

<p>
Cano provides comprehensive observability through the optional <code>tracing</code> feature using the
<a href="https://docs.rs/tracing/latest/tracing/" target="_blank">tracing</a> library.
All tracing instrumentation is behind conditional compilation, so it adds zero overhead when disabled.
</p>

<p>
For a callback-style API — get notified on workflow lifecycle and failure events without depending
on the <code>tracing</code> ecosystem — see <a href="../observers/">Observers</a>. The
<code>tracing</code> feature also ships a ready-made <code>TracingObserver</code> that bridges the
two: attach it with <code>.with_observer(Arc::new(TracingObserver::new()))</code> to re-emit those
observer events as <code>tracing</code> events under the <code>cano::observer</code> target.
</p>
<hr class="section-divider">

<h2 id="setup"><a href="#setup" class="anchor-link" aria-hidden="true">#</a>Setup</h2>
<p>Enable the <code>tracing</code> feature flag in your <code>Cargo.toml</code>. You can also use
<code>features = ["all"]</code> to enable everything (<code>scheduler</code> + <code>tracing</code> +
<code>recovery</code>) at once.</p>

```toml
[dependencies]
cano = { version = "0.12", features = ["tracing"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Or enable everything (scheduler + tracing + recovery):
# cano = { version = "0.12", features = ["all"] }

```

<h3 id="basic-init"><a href="#basic-init" class="anchor-link" aria-hidden="true">#</a>Basic Initialization</h3>
<p>For quick setup during development, use the default formatter.</p>

```rust
use tracing_subscriber;

// Simple setup for development
tracing_subscriber::fmt::init();

```

<h3 id="production-setup"><a href="#production-setup" class="anchor-link" aria-hidden="true">#</a>Production Setup with Environment Filter</h3>
<p>For production use, configure an environment filter so you can control log levels
at runtime via the <code>RUST_LOG</code> environment variable.</p>

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

<h2 id="what-gets-traced"><a href="#what-gets-traced" class="anchor-link" aria-hidden="true">#</a>What Gets Traced</h2>
<div class="card-grid">
<div class="card">
<h3>Workflow Level</h3>
<p>Orchestration start/completion, state transitions, final states.</p>
</div>
<div class="card">
<h3>Task Level</h3>
<p>Execution attempts, retry logic, delays, success/failure outcomes.</p>
</div>
<div class="card">
<h3>Scheduler Level</h3>
<p>Workflow scheduling, concurrent execution, run counts, durations.</p>
</div>
</div>
<hr class="section-divider">

<h2 id="error-tracing"><a href="#error-tracing" class="anchor-link" aria-hidden="true">#</a>Error Tracing</h2>
<p>
When the <code>tracing</code> feature is enabled, Cano automatically instruments error paths
at appropriate severity levels so you can diagnose failures without adding custom logging.
</p>

<div class="card-stack">
<div class="card">
<h3 id="failed-attempts"><a href="#failed-attempts" class="anchor-link" aria-hidden="true">#</a>Failed Task Attempts</h3>
<p>Each failed attempt is logged as a <strong>warning</strong> with the error details and current
attempt number. This lets you see transient failures that are recovered by retry logic.</p>

```bash
WARN task_attempt{attempt=2 max_attempts=4}: Task execution failed, will retry error="connection timeout"

```
</div>
<div class="card">
<h3 id="retry-exhaustion"><a href="#retry-exhaustion" class="anchor-link" aria-hidden="true">#</a>Retry Exhaustion</h3>
<p>When all retry attempts are exhausted, the final failure is logged as an <strong>error</strong>
with the total attempt count. This indicates a permanent failure that bubbles up as
<code>CanoError</code>.</p>

```bash
ERROR task_attempt{attempt=4 max_attempts=4}: Task execution failed after all retry attempts error="connection timeout"

```
</div>
<div class="card">
<h3 id="workflow-errors"><a href="#workflow-errors" class="anchor-link" aria-hidden="true">#</a>Workflow-Level Errors</h3>
<p>Workflow orchestration traces include the current state context, so errors are always
associated with the state that produced them.</p>

```bash
INFO workflow_orchestrate: Starting workflow execution initial_state=FetchData
ERROR workflow_orchestrate: Task failed in state FetchData after exhausting retries

```
</div>
</div>

<h3 id="filtering"><a href="#filtering" class="anchor-link" aria-hidden="true">#</a>Filtering Trace Output</h3>
<p>Use the <code>RUST_LOG</code> environment variable to control which modules emit trace output.
This is especially useful in production to reduce noise.</p>

```bash
# Show only Cano debug logs
RUST_LOG=cano=debug cargo run

# Show Cano info + your app's debug logs
RUST_LOG=cano=info,my_app=debug cargo run

# Show retry-related details only
RUST_LOG=cano::task=debug cargo run

# Silence everything except errors
RUST_LOG=error cargo run

```
<hr class="section-divider">

<h2 id="scheduler-tracing"><a href="#scheduler-tracing" class="anchor-link" aria-hidden="true">#</a>Scheduler Tracing</h2>
<p>
When both the <code>scheduler</code> and <code>tracing</code> features are enabled, the scheduler
produces trace output for workflow lifecycle events. You can enable both with
<code>features = ["all"]</code>.
</p>

<div class="card-stack">
<div class="card">
<h3 id="scheduler-workflow-exec"><a href="#scheduler-workflow-exec" class="anchor-link" aria-hidden="true">#</a>Workflow Execution</h3>
<p>The scheduler traces when each managed workflow starts and completes, including the
workflow identifier and scheduling trigger type.</p>
</div>
<div class="card">
<h3 id="scheduler-retry"><a href="#scheduler-retry" class="anchor-link" aria-hidden="true">#</a>Retry Attempts</h3>
<p>Retry delay durations are logged at debug level, so you can see the backoff progression
for failing tasks within scheduled workflows.</p>
</div>
<div class="card">
<h3 id="scheduler-split"><a href="#scheduler-split" class="anchor-link" aria-hidden="true">#</a>Split Task Execution</h3>
<p>When a scheduled workflow uses split/join, each parallel task is traced with its task ID
within a <code>split_task</code> span, making it straightforward to correlate concurrent
execution in log output.</p>
</div>
</div>

```toml
# Enable everything: scheduler + tracing + recovery
[dependencies]
cano = { version = "0.12", features = ["all"] }

```
<hr class="section-divider">

<h2 id="custom-spans"><a href="#custom-spans" class="anchor-link" aria-hidden="true">#</a>Custom Spans</h2>
<p>Attach custom tracing spans to your workflows to include business-specific context.
The span wraps all trace output generated during that workflow's execution.</p>

```rust
use tracing::info_span;
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MyState { Start, Complete }

#[derive(Clone)]
struct MyProcessor;

#[task(state = MyState)]
impl MyProcessor {
    async fn run_bare(&self) -> Result<TaskResult<MyState>, CanoError> {
        Ok(TaskResult::Single(MyState::Complete))
    }
}

fn build_workflow() -> Workflow<MyState> {
    // Create workflow with custom tracing span
    let workflow_span = info_span!(
        "user_data_processing",
        user_id = "12345",
        batch_id = "batch_001"
    );

    let store = MemoryStore::new();
    Workflow::new(Resources::new().insert("store", store))
        .with_tracing_span(workflow_span)
        .register(MyState::Start, MyProcessor)
        .add_exit_state(MyState::Complete)
}
```
<hr class="section-divider">

<h2 id="custom-instrumentation"><a href="#custom-instrumentation" class="anchor-link" aria-hidden="true">#</a>Custom Instrumentation</h2>
<p>You can add your own tracing to Task implementations using the
<code>tracing</code> crate's macros directly. This is useful for adding domain-specific
context that Cano's built-in instrumentation does not cover.</p>

<h3 id="instrument-task"><a href="#instrument-task" class="anchor-link" aria-hidden="true">#</a>Instrumenting a Task</h3>

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
    async fn run(&self, res: &Resources) -> Result<TaskResult<AppState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let order_id: String = store.get("order_id")?;
        info!(order_id = %order_id, "Processing payment");

        match process_payment(&order_id).await {
            Ok(receipt) => {
                info!(order_id = %order_id, receipt = %receipt, "Payment succeeded");
                store.put("receipt", receipt)?;
                Ok(TaskResult::Single(AppState::Confirm))
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

<h2 id="example-output"><a href="#example-output" class="anchor-link" aria-hidden="true">#</a>Example Output</h2>
<p>Running with <code>RUST_LOG=info</code> produces structured logs:</p>
<div class="trace-output">

```bash
INFO user_data_processing{user_id="12345"}: Starting workflow orchestration
  INFO user_data_processing{user_id="12345"}:task_execution: Starting task execution
    INFO user_data_processing{user_id="12345"}:task_attempt{attempt=1}: Task execution completed success=true
    INFO user_data_processing{user_id="12345"}:task_attempt{attempt=1}: processor_id=basic_processor input_records=3: Data processing completed
  INFO user_data_processing{user_id="12345"}: Workflow completed successfully

```
</div>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full Example</h2>

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
    async fn run(&self, _res: &Resources) -> Result<TaskResult<State>, CanoError> {
        info!("Processing order...");
        Ok(TaskResult::Single(State::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // 1. Setup Subscriber with env filter
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let store = MemoryStore::new();

    // 2. Create Workflow with Custom Span
    // This span will wrap all logs generated by this workflow
    let workflow_span = info_span!(
        "order_processing",
        order_id = "ORD-2025-001",
        customer = "Acme Corp"
    );

    let workflow = Workflow::new(Resources::new().insert("store", store))
        .register(State::Start, ProcessOrder)
        .add_exit_state(State::Complete)
        .with_tracing_span(workflow_span); // Attach span

    // 3. Run
    info!("Submitting order...");
    workflow.orchestrate(State::Start).await?;

    Ok(())
}

```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example tracing_demo --features "tracing scheduler"</code> — sets
up a <code>tracing-subscriber</code>, runs a workflow and a scheduled flow, and prints the structured
span output. Pair it with <code>TracingObserver</code> (see <a href="../observers/#tracing-observer">Observers</a>)
for flat lifecycle events alongside the spans.</p>
</div>

