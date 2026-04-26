+++
title = "Tracing"
description = "Comprehensive observability for Cano workflows via the optional tracing feature."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Tracing</h1>
<p class="subtitle">Comprehensive observability for your workflows.</p>

<div class="feature-banner">
<div class="banner-icon" aria-hidden="true">⚙️</div>
<div class="banner-content">
<p><strong>Feature flag required</strong> -- Tracing is behind the <code>tracing</code> feature gate.
Enable it with <code>features = ["tracing"]</code> or <code>features = ["all"]</code> in your
<code>Cargo.toml</code>. Zero overhead when disabled.</p>
</div>
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
<hr class="section-divider">

<h2 id="setup"><a href="#setup" class="anchor-link" aria-hidden="true">#</a>Setup</h2>
<p>Enable the <code>tracing</code> feature flag in your <code>Cargo.toml</code>. You can also use
<code>features = ["all"]</code> to enable both <code>tracing</code> and <code>scheduler</code> at once.</p>

<pre><code class="language-toml">[dependencies]
cano = { version = "0.8", features = ["tracing"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
<!--blank-->
# Or enable everything (tracing + scheduler):
# cano = { version = "0.8", features = ["all"] }</code></pre>

<h3 id="basic-init"><a href="#basic-init" class="anchor-link" aria-hidden="true">#</a>Basic Initialization</h3>
<p>For quick setup during development, use the default formatter.</p>
<pre><code class="language-rust">use tracing_subscriber;
<!--blank-->
// Simple setup for development
tracing_subscriber::fmt::init();</code></pre>

<h3 id="production-setup"><a href="#production-setup" class="anchor-link" aria-hidden="true">#</a>Production Setup with Environment Filter</h3>
<p>For production use, configure an environment filter so you can control log levels
at runtime via the <code>RUST_LOG</code> environment variable.</p>
<pre><code class="language-rust">use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
<!--blank-->
// Production setup with env filter
tracing_subscriber::registry()
    .with(tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into()))
    .with(tracing_subscriber::fmt::layer())
    .init();</code></pre>
<hr class="section-divider">

<h2 id="what-gets-traced"><a href="#what-gets-traced" class="anchor-link" aria-hidden="true">#</a>What Gets Traced</h2>
<div class="card-grid">
<div class="card">
<h3>Workflow Level</h3>
<p>Orchestration start/completion, state transitions, final states.</p>
</div>
<div class="card">
<h3>Task/Node Level</h3>
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
<pre><code class="language-bash">WARN task_attempt{attempt=2 max_attempts=4}: Task execution failed, will retry error="connection timeout"</code></pre>
</div>
<div class="card">
<h3 id="retry-exhaustion"><a href="#retry-exhaustion" class="anchor-link" aria-hidden="true">#</a>Retry Exhaustion</h3>
<p>When all retry attempts are exhausted, the final failure is logged as an <strong>error</strong>
with the total attempt count. This indicates a permanent failure that bubbles up as
<code>CanoError</code>.</p>
<pre><code class="language-bash">ERROR task_attempt{attempt=4 max_attempts=4}: Task execution failed after all retry attempts error="connection timeout"</code></pre>
</div>
<div class="card">
<h3 id="workflow-errors"><a href="#workflow-errors" class="anchor-link" aria-hidden="true">#</a>Workflow-Level Errors</h3>
<p>Workflow orchestration traces include the current state context, so errors are always
associated with the state that produced them.</p>
<pre><code class="language-bash">INFO workflow_orchestrate: Starting workflow execution initial_state=FetchData
ERROR workflow_orchestrate: Task failed in state FetchData after exhausting retries</code></pre>
</div>
</div>

<h3 id="filtering"><a href="#filtering" class="anchor-link" aria-hidden="true">#</a>Filtering Trace Output</h3>
<p>Use the <code>RUST_LOG</code> environment variable to control which modules emit trace output.
This is especially useful in production to reduce noise.</p>
<pre><code class="language-bash"># Show only Cano debug logs
RUST_LOG=cano=debug cargo run
<!--blank-->
# Show Cano info + your app's debug logs
RUST_LOG=cano=info,my_app=debug cargo run
<!--blank-->
# Show retry-related details only
RUST_LOG=cano::task=debug cargo run
<!--blank-->
# Silence everything except errors
RUST_LOG=error cargo run</code></pre>
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

<pre><code class="language-toml"># Enable both scheduler and tracing
[dependencies]
cano = { version = "0.8", features = ["all"] }</code></pre>
<hr class="section-divider">

<h2 id="custom-spans"><a href="#custom-spans" class="anchor-link" aria-hidden="true">#</a>Custom Spans</h2>
<p>Attach custom tracing spans to your workflows to include business-specific context.
The span wraps all trace output generated during that workflow's execution.</p>

<pre><code class="language-rust">use tracing::info_span;
use cano::prelude::*;
<!--blank-->
// Create workflow with custom tracing span
let workflow_span = info_span!(
    "user_data_processing",
    user_id = "12345",
    batch_id = "batch_001"
);
<!--blank-->
let store = MemoryStore::new();
let workflow = Workflow::new(Resources::new().insert("store", store))
    .with_tracing_span(workflow_span)
    .register(MyState::Start, MyProcessorNode)
    .add_exit_state(MyState::Complete);</code></pre>
<hr class="section-divider">

<h2 id="custom-instrumentation"><a href="#custom-instrumentation" class="anchor-link" aria-hidden="true">#</a>Custom Instrumentation</h2>
<p>You can add your own tracing to Task and Node implementations using the
<code>tracing</code> crate's macros directly. This is useful for adding domain-specific
context that Cano's built-in instrumentation does not cover.</p>

<h3 id="instrument-task"><a href="#instrument-task" class="anchor-link" aria-hidden="true">#</a>Instrumenting a Task</h3>
<pre><code class="language-rust">use cano::prelude::*;
use tracing::{info, warn};
<!--blank-->
#[derive(Clone)]
struct PaymentTask;
<!--blank-->
#[async_trait]
impl Task&lt;AppState&gt; for PaymentTask {
    async fn run(&self, res: &Resources) -> Result&lt;TaskResult&lt;AppState&gt;, CanoError&gt; {
        let store = res.get::&lt;MemoryStore, str&gt;("store")?;
        let order_id: String = store.get("order_id")?;
        info!(order_id = %order_id, "Processing payment");
<!--blank-->
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
}</code></pre>

<h3 id="instrument-node"><a href="#instrument-node" class="anchor-link" aria-hidden="true">#</a>Instrumenting a Node</h3>
<pre><code class="language-rust">use cano::prelude::*;
use tracing::{info, debug, instrument};
<!--blank-->
#[derive(Clone)]
struct DataEnrichmentNode {
    source: String,
}
<!--blank-->
#[async_trait]
impl Node&lt;PipelineState&gt; for DataEnrichmentNode {
    type PrepResult = Vec&lt;String&gt;;
    type ExecResult = Vec&lt;String&gt;;
<!--blank-->
    #[instrument(skip(self, res), fields(source = %self.source))]
    async fn prep(&self, res: &Resources) -> Result&lt;Self::PrepResult, CanoError&gt; {
        let store = res.get::&lt;MemoryStore, str&gt;("store")?;
        let keys: Vec&lt;String&gt; = store.get("pending_keys")?;
        debug!(count = keys.len(), "Loaded keys for enrichment");
        Ok(keys)
    }
<!--blank-->
    async fn exec(&self, keys: Self::PrepResult) -> Self::ExecResult {
        info!(count = keys.len(), source = %self.source, "Enriching records");
        // ... enrichment logic
        keys
    }
<!--blank-->
    async fn post(
        &self,
        res: &Resources,
        results: Self::ExecResult,
    ) -> Result&lt;PipelineState, CanoError&gt; {
        info!(enriched = results.len(), "Enrichment complete");
        let store = res.get::&lt;MemoryStore, str&gt;("store")?;
        store.put("enriched_data", results)?;
        Ok(PipelineState::Validate)
    }
}</code></pre>
<hr class="section-divider">

<h2 id="example-output"><a href="#example-output" class="anchor-link" aria-hidden="true">#</a>Example Output</h2>
<p>Running with <code>RUST_LOG=info</code> produces structured logs:</p>
<div class="trace-output">
<pre><code class="language-bash">INFO user_data_processing{user_id="12345"}: Starting workflow orchestration
  INFO user_data_processing{user_id="12345"}:task_execution: Starting task execution
    INFO user_data_processing{user_id="12345"}:task_attempt{attempt=1}: Node execution completed success=true
    INFO user_data_processing{user_id="12345"}:task_attempt{attempt=1}: processor_id=basic_processor input_records=3: Data processing completed
  INFO user_data_processing{user_id="12345"}: Workflow completed successfully</code></pre>
</div>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full Example</h2>
<pre><code class="language-rust">use cano::prelude::*;
use async_trait::async_trait;
use tracing::{info, info_span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
<!--blank-->
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State {
    Start,
    Complete,
}
<!--blank-->
#[derive(Clone)]
struct ProcessOrderNode;
<!--blank-->
#[async_trait]
impl Task&lt;State&gt; for ProcessOrderNode {
    async fn run(&self, _res: &Resources) -> Result&lt;TaskResult&lt;State&gt;, CanoError&gt; {
        info!("Processing order...");
        Ok(TaskResult::Single(State::Complete))
    }
}
<!--blank-->
#[tokio::main]
async fn main() -> Result&lt;(), CanoError&gt; {
    // 1. Setup Subscriber with env filter
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();
<!--blank-->
    let store = MemoryStore::new();
<!--blank-->
    // 2. Create Workflow with Custom Span
    // This span will wrap all logs generated by this workflow
    let workflow_span = info_span!(
        "order_processing",
        order_id = "ORD-2025-001",
        customer = "Acme Corp"
    );
<!--blank-->
    let workflow = Workflow::new(Resources::new().insert("store", store))
        .register(State::Start, ProcessOrderNode)
        .add_exit_state(State::Complete)
        .with_tracing_span(workflow_span); // Attach span
<!--blank-->
    // 3. Run
    info!("Submitting order...");
    workflow.orchestrate(State::Start).await?;
<!--blank-->
    Ok(())
}</code></pre>
</div>

