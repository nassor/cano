+++
title = "Metrics"
description = "Optional metrics-crate counters, histograms, and gauges for Cano workflows via the metrics feature."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Metrics</h1>
<p class="subtitle"><code>metrics</code>-crate counters, histograms, and gauges for your workflows.</p>
<p class="feature-tag">Behind the <code>metrics</code> feature gate (<code>features = ["metrics"]</code>).</p>

<div class="callout callout-info">
<span class="callout-label">See also</span>
<p>This page covers Cano's built-in <code>metrics</code> instrumentation and the <code>MetricsObserver</code>.
For the callback API (<code>WorkflowObserver</code>) that <code>MetricsObserver</code> builds on,
see <a href="../observers/">Observers</a>. For <code>tracing</code>-crate span instrumentation, see
<a href="../tracing/">Tracing</a>.</p>
</div>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#setup">Setup</a></li>
<li><a href="#two-surfaces">Two surfaces</a></li>
<li><a href="#what-gets-measured">What gets measured</a></li>
<li><a href="#describe">Registering descriptions</a></li>
<li><a href="#cardinality">Cardinality</a></li>
<li><a href="#cost">Cost</a></li>
<li><a href="#correlating-traces">Correlating with traces</a></li>
<li><a href="#full-example">Full example</a></li>
</ol>
</nav>

<p>
This page is a reference for the metrics Cano emits, with a short how-to for installing a recorder.
Cano uses the <a href="https://docs.rs/metrics/latest/metrics/" target="_blank">metrics</a> facade
crate, so you install any compatible exporter (Prometheus, StatsD, a debugging snapshotter) and Cano
emits to it. All instrumentation is behind conditional compilation, so it costs nothing when the
feature is off.
</p>

<p>
To get callbacks on lifecycle and failure events without depending on the <code>metrics</code>
ecosystem, see <a href="../observers/">Observers</a>. Attach
<code>.with_observer(Arc::new(MetricsObserver::new()))</code> to re-emit those hooks as
<code>metrics</code> counters.
</p>
<hr class="section-divider">

<h2 id="setup"><a href="#setup" class="anchor-link" aria-hidden="true">#</a>Setup</h2>
<p>Enable the <code>metrics</code> feature in your <code>Cargo.toml</code>. <code>features = ["all"]</code>
enables every optional feature (<code>scheduler</code>, <code>tracing</code>, <code>store-redb</code>,
<code>store-postgres</code>, <code>store-nats</code>, <code>metrics</code> and <code>testing</code>).</p>

```toml
[dependencies]
cano = { version = "0.16", features = ["metrics"] }

# The metrics crate is a facade, so you also need a recorder or exporter:
metrics-exporter-prometheus = "0.18"  # for production
# or: metrics-util = "0.20"           # for testing and debugging snapshots
```

<p>
Your application picks the concrete recorder: for example
<code>metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder()</code> for a Prometheus
scrape endpoint, or <code>metrics_util::debugging::DebuggingRecorder</code> in tests. Call
<code>cano::metrics::describe()</code> once, after installing the recorder, so exporters receive help
text and units.
</p>
<hr class="section-divider">

<h2 id="two-surfaces"><a href="#two-surfaces" class="anchor-link" aria-hidden="true">#</a>Two surfaces</h2>
<p>
The feature has two surfaces, mirroring how <a href="../tracing/">Tracing</a> pairs engine spans with
the <code>TracingObserver</code> bridge.
</p>

<div class="card-stack">
<div class="card">
<h3 id="metrics-observer"><a href="#metrics-observer" class="anchor-link" aria-hidden="true">#</a><code>MetricsObserver</code>: opt-in lifecycle counters</h3>
<p>
<code>MetricsObserver</code> is a <code>WorkflowObserver</code> that re-emits observer hooks as
<code>metrics</code> counters. It is in the prelude behind the <code>metrics</code> feature.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;

Workflow::bare()
    .register(/* ... */)
    .add_exit_state(/* ... */)
    .with_observer(Arc::new(MetricsObserver::new()))
```

<p>Each metric below is emitted by the hook named after the colon:</p>
<ul>
<li><code>cano_state_enters_total{state}</code>: <code>on_state_enter</code></li>
<li><code>cano_observed_task_runs_total{task, outcome}</code>: <code>on_task_success</code> and <code>on_task_failure</code>, with <code>outcome</code> of <code>completed</code> or <code>failed</code></li>
<li><code>cano_task_retries_total{task}</code>: <code>on_retry</code></li>
<li><code>cano_circuit_open_events_total{task}</code>: <code>on_circuit_open</code></li>
<li><code>cano_checkpoints_observed_total</code>: <code>on_checkpoint</code></li>
<li><code>cano_resumes_total</code>: <code>on_resume</code></li>
<li><code>cano_observed_workflow_timeouts_total</code>: <code>on_workflow_timeout</code></li>
<li><code>cano_observed_workflow_timeout_limit_seconds</code> and <code>cano_observed_workflow_timeout_elapsed_seconds</code>: histograms recorded by <code>on_workflow_timeout</code></li>
<li><code>cano_observed_cancellations_total</code>: <code>on_cancelled</code>, a run cancelled through a <code>CancellationToken</code></li>
<li><code>cano_observed_unknown_resume_states_total</code>: <code>on_unknown_resume_state</code></li>
</ul>
<p>
<code>on_task_start</code> emits nothing: <code>cano_observed_task_runs_total</code> already counts every
dispatch by outcome. <code>on_checkpoint_clear_failed</code> emits nothing either, because
<code>cano_checkpoint_clears_total{result="err"}</code> already counts each failed clear.
</p>
</div>
<div class="card">
<h3 id="always-on"><a href="#always-on" class="anchor-link" aria-hidden="true">#</a>Always-on direct instrumentation</h3>
<p>
Compiled in whenever the <code>metrics</code> feature is on, whether or not a
<code>MetricsObserver</code> is attached. It covers engine internals the observer hooks do not reach:
workflow run duration, circuit breaker state transitions, per-attempt retry loop outcomes,
poll, batch and step iteration counts, scheduler flow telemetry, and checkpoint store operations.
See <a href="#what-gets-measured">What gets measured</a> for the list.
</p>
</div>
</div>
<hr class="section-divider">

<h2 id="what-gets-measured"><a href="#what-gets-measured" class="anchor-link" aria-hidden="true">#</a>What gets measured</h2>
<p>
Histograms record raw <code>f64</code> seconds. Bucketing and quantiles are the exporter's job. Metric
names follow the <code>metrics</code> crate's underscore convention.
</p>

<div class="diagram-frame">
<p class="diagram-label">Where the counters fire along one run</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 510" role="img" aria-labelledby="metrics-emit-title metrics-emit-desc">
<title id="metrics-emit-title">Emission points along one workflow run</title>
<desc id="metrics-emit-desc">Six steps of a run listed top to bottom, each with a metric it emits into the metrics facade, which forwards to the recorder your application installed. Calling orchestrate sets the cano_workflow_active gauge. Entering a state increments cano_state_enters_total, which needs the MetricsObserver. Acquiring the circuit breaker and rate limiter increments cano_circuit_acquires_total and cano_rate_limiter_throttled_total. Each task attempt inside the retry loop increments cano_task_attempts_total, and each retry increments cano_task_retries_total, which needs the MetricsObserver. A split and join increments cano_split_branch_results_total. Reaching the exit state records cano_workflow_runs_total and cano_workflow_duration_seconds.</desc>
<defs><marker id="metrics-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-mut" x="110" y="34">one run, top to bottom</text>
<text class="t-mut" x="556" y="34">emit point</text>
<path class="e" d="M200,82 H592" marker-end="url(#metrics-ah)"/>
<text class="t-code t-mut" x="380" y="66">cano_workflow_active</text>
<path class="e" d="M200,150 H592" marker-end="url(#metrics-ah)"/>
<text class="t-code t-mut" x="380" y="134">cano_state_enters_total{state}</text>
<path class="e" d="M200,218 H592" marker-end="url(#metrics-ah)"/>
<text class="t-code t-mut" x="380" y="185">cano_circuit_acquires_total{result}</text>
<text class="t-code t-mut" x="380" y="202">cano_rate_limiter_throttled_total{result}</text>
<path class="e" d="M200,286 H592" marker-end="url(#metrics-ah)"/>
<text class="t-code t-mut" x="380" y="253">cano_task_attempts_total{outcome}</text>
<text class="t-code t-mut" x="380" y="270">cano_task_retries_total{task}</text>
<path class="e" d="M200,354 H592" marker-end="url(#metrics-ah)"/>
<text class="t-code t-mut" x="380" y="338">cano_split_branch_results_total{result}</text>
<path class="e" d="M200,422 H592" marker-end="url(#metrics-ah)"/>
<text class="t-code t-mut" x="380" y="389">cano_workflow_runs_total{outcome}</text>
<text class="t-code t-mut" x="380" y="406">cano_workflow_duration_seconds{outcome}</text>
<path class="e e-dim" d="M110,106 V122" marker-end="url(#metrics-ah)"/>
<path class="e e-dim" d="M110,174 V190" marker-end="url(#metrics-ah)"/>
<path class="e e-dim" d="M110,242 V258" marker-end="url(#metrics-ah)"/>
<path class="e e-dim" d="M110,310 V326" marker-end="url(#metrics-ah)"/>
<path class="e e-dim" d="M110,378 V394" marker-end="url(#metrics-ah)"/>
<rect class="n" x="20" y="58" width="180" height="48" rx="10"/>
<text class="t-code" x="110" y="83">orchestrate()</text>
<rect class="n" x="20" y="126" width="180" height="48" rx="10"/>
<text class="t-strong" x="110" y="151">state enter</text>
<rect class="n" x="20" y="194" width="180" height="48" rx="10"/>
<text class="t-strong" x="110" y="208">circuit breaker</text>
<text class="t-mut" x="110" y="229">&middot; rate limiter</text>
<rect class="n" x="20" y="262" width="180" height="48" rx="10"/>
<text class="t-strong" x="110" y="276">task attempt</text>
<text class="t-mut" x="110" y="297">inside the retry loop</text>
<rect class="n" x="20" y="330" width="180" height="48" rx="10"/>
<text class="t-strong" x="110" y="355">split &rarr; join</text>
<rect class="n" x="20" y="398" width="180" height="48" rx="10"/>
<text class="t-strong" x="110" y="412">exit state</text>
<text class="t-mut" x="110" y="433">workflow returns</text>
<circle class="n-hot" cx="556" cy="82" r="7"/>
<circle class="n-hot" cx="556" cy="150" r="7"/>
<circle class="n-hot" cx="556" cy="218" r="7"/>
<circle class="n-hot" cx="556" cy="286" r="7"/>
<circle class="n-hot" cx="556" cy="354" r="7"/>
<circle class="n-hot" cx="556" cy="422" r="7"/>
<rect class="n-cop" x="600" y="58" width="160" height="388" rx="12"/>
<text class="t-strong" x="680" y="242">metrics facade</text>
<text class="t-mut" x="680" y="264">&rarr; your recorder</text>
<text class="t-mut" x="390" y="470">Dots mark emission points. Recovery, scheduler and poll, batch and step counters emit on the same rail.</text>
<text class="t-mut" x="390" y="490">MetricsObserver supplies the state and retry counters. The rest is always-on engine instrumentation.</text>
</svg>
</div>
</div>

<div class="card-grid">
<div class="card">
<h3>Workflow</h3>
<ul>
<li><code>cano_workflow_runs_total{outcome}</code>: counter; <code>outcome</code> is <code>completed</code>, <code>suspended</code>, <code>cancelled</code> or <code>failed</code>. A run that trips its total timeout counts as <code>failed</code>.</li>
<li><code>cano_workflow_duration_seconds{outcome}</code>: histogram (seconds)</li>
<li><code>cano_workflow_active</code>: gauge; workflows currently executing</li>
</ul>
</div>
<div class="card">
<h3>Task dispatch</h3>
<ul>
<li><code>cano_task_duration_seconds{state, kind}</code>: histogram (seconds); <code>kind</code> is <code>single</code>, <code>router</code>, <code>split</code>, <code>compensatable</code>, <code>stepped</code>, <code>stream</code> or <code>suspend</code></li>
<li><code>cano_task_attempts_total{outcome}</code>: counter; per attempt inside the retry loop; <code>outcome</code> is <code>completed</code> or <code>failed</code></li>
<li><code>cano_circuit_rejections_total</code>: counter; attempts short-circuited by an open breaker</li>
</ul>
</div>
<div class="card">
<h3>Split and join</h3>
<ul>
<li><code>cano_split_branch_results_total{result}</code>: counter; <code>result</code> is <code>success</code>, <code>failure</code> or <code>cancelled</code></li>
</ul>
</div>
</div>

<div class="card-stack">
<div class="card">
<h3 id="circuit-breaker-metrics"><a href="#circuit-breaker-metrics" class="anchor-link" aria-hidden="true">#</a>Circuit breaker</h3>
<ul>
<li><code>cano_circuit_transitions_total{transition}</code>: counter; <code>transition</code> is <code>closed_to_open</code>, <code>open_to_halfopen</code>, <code>halfopen_to_closed</code> or <code>halfopen_to_open</code></li>
<li><code>cano_circuit_acquires_total{result}</code>: counter; <code>result</code> is <code>acquired</code> or <code>rejected</code></li>
<li><code>cano_circuit_outcomes_total{outcome}</code>: counter; <code>outcome</code> is <code>success</code> or <code>failure</code></li>
</ul>
</div>
<div class="card">
<h3 id="rate-limiter-metrics"><a href="#rate-limiter-metrics" class="anchor-link" aria-hidden="true">#</a>Rate limiter</h3>
<ul>
<li><code>cano_rate_limiter_acquired_total</code>: counter; successful acquisitions through <code>try_acquire</code>, <code>acquire</code> and their <code>_n</code> forms</li>
<li><code>cano_rate_limiter_tokens_consumed_total</code>: counter; weighted units consumed (the sum of acquire costs)</li>
<li><code>cano_rate_limiter_throttled_total{result}</code>: counter; <code>result</code> is <code>waited</code> or <code>rejected</code></li>
<li><code>cano_rate_limiter_wait_seconds</code>: histogram (seconds); wall-clock time <code>acquire</code> blocked</li>
<li><code>cano_multi_rate_limiter_throttled_total{tier}</code>: counter; <code>MultiRateLimiter</code> rejections, by the tier that blocked them</li>
</ul>
</div>
<div class="card">
<h3 id="processing-loops"><a href="#processing-loops" class="anchor-link" aria-hidden="true">#</a>Processing loops</h3>
<ul>
<li><code>cano_poll_iterations_total{outcome}</code>: counter; <code>outcome</code> is <code>ready</code> or <code>pending</code></li>
<li><code>cano_batch_runs_total{outcome}</code>: counter; <code>outcome</code> is <code>completed</code> or <code>failed</code></li>
<li><code>cano_batch_items_total{result}</code>: counter; <code>result</code> is <code>ok</code> or <code>err</code></li>
<li><code>cano_step_iterations_total{outcome}</code>: counter; <code>outcome</code> is <code>more</code> or <code>done</code></li>
<li><code>cano_stream_runs_total{outcome}</code>: counter; <code>outcome</code> is <code>completed</code>, <code>cancelled</code> or <code>failed</code></li>
<li><code>cano_stream_windows_total</code>: counter; windows flushed</li>
<li><code>cano_stream_items_total{result}</code>: counter; <code>result</code> is <code>ok</code> or <code>err</code></li>
</ul>
</div>
<div class="card">
<h3 id="recovery-saga-metrics"><a href="#recovery-saga-metrics" class="anchor-link" aria-hidden="true">#</a>Recovery and saga</h3>
<ul>
<li><code>cano_checkpoint_appends_total{result}</code>: counter; <code>result</code> is <code>ok</code> or <code>err</code></li>
<li><code>cano_checkpoint_clears_total{result}</code>: counter; <code>result</code> is <code>ok</code> or <code>err</code></li>
<li><code>cano_compensations_run_total{result}</code>: counter; <code>result</code> is <code>ok</code> or <code>err</code></li>
<li><code>cano_compensation_drains_total{outcome}</code>: counter; <code>outcome</code> is <code>clean</code> or <code>partial</code></li>
</ul>
</div>
<div class="card">
<h3 id="scheduler-metrics"><a href="#scheduler-metrics" class="anchor-link" aria-hidden="true">#</a>Scheduler</h3>
<p class="feature-tag">Also requires the <code>scheduler</code> feature.</p>
<ul>
<li><code>cano_scheduler_flow_runs_total{flow, outcome}</code>: counter; <code>outcome</code> is <code>completed</code>, <code>cancelled</code> or <code>failed</code></li>
<li><code>cano_scheduler_flow_duration_seconds{flow}</code>: histogram (seconds)</li>
<li><code>cano_scheduler_flow_backoff_total{flow}</code>: counter</li>
<li><code>cano_scheduler_flow_tripped_total{flow}</code>: counter</li>
<li><code>cano_scheduler_active_flows</code>: gauge; flows currently executing</li>
<li>Each scheduled run also records <code>cano_workflow_runs_total</code> and <code>cano_workflow_duration_seconds</code>, like a direct <code>orchestrate</code> call.</li>
</ul>
</div>
</div>
<hr class="section-divider">

<h2 id="describe"><a href="#describe" class="anchor-link" aria-hidden="true">#</a>Registering descriptions</h2>
<p>
<code>cano::metrics::describe()</code> registers a description and unit for every metric Cano emits.
Call it once, after installing your recorder, so exporters include help text (for Prometheus, the
<code># HELP</code> and <code># TYPE</code> lines).
</p>

```rust
// Install your recorder first, then describe:
metrics_exporter_prometheus::PrometheusBuilder::new()
    .install()
    .expect("install prometheus exporter");

cano::metrics::describe();
```

<p>
Without <code>describe()</code>, metrics still flow. Only the help text and units are missing from the
exporter output.
</p>
<hr class="section-divider">

<h2 id="cardinality"><a href="#cardinality" class="anchor-link" aria-hidden="true">#</a>Cardinality</h2>
<p>
Labels are minimal, which keeps cardinality bounded:
</p>
<ul>
<li><code>state</code> is <code>format!("{:?}")</code> of your FSM state enum, bounded by the registered states.</li>
<li><code>task</code> is <code>Task::name()</code>, which defaults to <code>std::any::type_name</code>, bounded by the registered task types.</li>
<li><code>flow</code> is the scheduler flow id, bounded by the registered flows.</li>
<li>All other label values (<code>outcome</code>, <code>kind</code>, <code>result</code>, <code>transition</code>, <code>tier</code>) come from fixed sets.</li>
</ul>
<p>
The hot-path metrics carry no per-state label: per-attempt counters
(<code>cano_task_attempts_total</code>), circuit breaker internals, and poll, batch and step iteration
counters. Their cardinality stays constant however many states a workflow defines.
</p>
<hr class="section-divider">

<h2 id="cost"><a href="#cost" class="anchor-link" aria-hidden="true">#</a>Cost</h2>
<p>
With the <code>metrics</code> feature compiled in, every state transition formats the state label as a
string, even when no recorder is installed. The cost is small and bounded, but a latency-critical
service that collects no metrics should leave the feature off. With a recorder installed, each
emission costs a hash-map lookup plus an atomic increment, comparable to a log line.
</p>
<hr class="section-divider">

<h2 id="correlating-traces"><a href="#correlating-traces" class="anchor-link" aria-hidden="true">#</a>Correlating with traces</h2>
<p>
With the <a href="../tracing/"><code>tracing</code></a> feature also on, <code>metrics</code> and
<code>tracing</code> interoperate through spans. The
<a href="https://docs.rs/metrics-tracing-context/" target="_blank"><code>metrics-tracing-context</code></a>
crate makes any metric emitted inside a span inherit that span's fields as labels. You wire it up in
your application. Cano never depends on it, in the same way it never depends on
<code>tracing-subscriber</code>.
</p>

```toml
[dependencies]
cano = { version = "0.16", features = ["metrics", "tracing"] }
metrics-tracing-context = "0.18"
tracing-subscriber = "0.3"
```

```rust
use metrics_tracing_context::{MetricsLayer, TracingContextLayer};
use metrics_util::layers::Layer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// 1. Wrap your metrics recorder so it reads the current span's fields.
let recorder = TracingContextLayer::all().layer(your_recorder);
metrics::set_global_recorder(recorder).expect("install metrics recorder");
cano::metrics::describe();

// 2. Add MetricsLayer to your tracing subscriber so spans expose their fields.
tracing_subscriber::registry()
    .with(MetricsLayer::new())
    .with(tracing_subscriber::fmt::layer())
    .init();
```

<p>
With both layers installed, a span attached to the <code>Workflow::orchestrate</code> future through
<code>.instrument(span)</code> tags every <code>cano_*</code> metric recorded during that run. For
example, <code>info_span!("api_request", request_id = "abc")</code> adds a <code>request_id</code>
label. Use <code>tracing::Instrument</code>, and never hold a <code>span.enter()</code> guard across
the <code>.await</code>.
</p>
<p>
Cano's default <code>workflow_orchestrate</code> and <code>workflow_resume</code> spans carry a
<code>workflow_id</code> field when <code>with_workflow_id</code> is set, so it becomes a label too.
Span fields merge with the labels Cano already attaches (<code>state</code>, <code>task</code>,
<code>flow</code>, <code>outcome</code>). No Cano span field reuses one of those names, so the merge is
unambiguous.
</p>

<div class="callout callout-warning">
<span class="callout-label">Cardinality</span>
<p>
<code>TracingContextLayer::all()</code> promotes every field of every entered span to a label,
including Cano's own span fields such as <code>max_attempts</code> from the retry loop. In production,
use <code>TracingContextLayer::new(filter)</code> with a <code>LabelFilter</code> that allow-lists the
fields you author.
</p>
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example metrics_tracing_context --features "metrics tracing"</code>
wires the two layers, runs a workflow under a <code>workflow_id</code> and another inside a user
<code>api_request</code> span, and prints the captured metrics so you can see the
<code>workflow_id</code> and <code>request_id</code> labels that came from span context.</p>
</div>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full example</h2>
<p>
This program installs a <code>DebuggingRecorder</code> (useful in tests and self-contained demos),
calls <code>cano::metrics::describe()</code>, attaches a <code>MetricsObserver</code>, runs a workflow
directly and then under the scheduler, and dumps the captured snapshot. It mirrors the
<code>metrics_demo</code> example shipped with the crate.
</p>

```rust
use cano::prelude::*;
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Fetch, Process, Done }

struct FetchTask;

#[task]
impl Task<Step> for FetchTask {
    async fn run(&self) -> Result<Step, CanoError> {
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(Step::Process)
    }
}

struct ProcessTask;

#[task]
impl Task<Step> for ProcessTask {
    async fn run(&self) -> Result<Step, CanoError> {
        tokio::time::sleep(Duration::from_millis(3)).await;
        Ok(Step::Done)
    }
}

fn workflow() -> Workflow<Step> {
    Workflow::bare()
        .with_observer(Arc::new(MetricsObserver::new()))
        .register(Step::Fetch, FetchTask)
        .register(Step::Process, ProcessTask)
        .add_exit_state(Step::Done)
}

#[tokio::main]
async fn main() {
    // 1. Install recorder and register descriptions.
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::set_global_recorder(recorder).expect("install metrics recorder");
    cano::metrics::describe();

    // In production, use a real exporter instead:
    // metrics_exporter_prometheus::PrometheusBuilder::new()
    //     .install().expect("install prometheus exporter");

    // 2. Run the workflow a few times directly.
    for _ in 0..3 {
        workflow()
            .orchestrate(Step::Fetch)
            .await
            .expect("workflow run");
    }

    // 3. Run the same workflow under the scheduler for about 1.2 s, firing every second.
    let running = Scheduler::new()
        .every("demo_flow", workflow(), Step::Fetch, Duration::from_secs(1))
        .start()
        .await
        .expect("start scheduler");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    running.stop().await.expect("stop scheduler");

    // 4. Dump every captured metric (sorted alphabetically).
    println!("\n=== Cano metrics ===");
    let mut rows = snapshotter.snapshot().into_vec();
    rows.sort_by(|a, b| a.0.key().name().cmp(b.0.key().name()));
    for (ck, _unit, _desc, value) in rows {
        let key = ck.key();
        let labels: Vec<String> = key
            .labels()
            .map(|l| format!("{}={}", l.key(), l.value()))
            .collect();
        let label_str = if labels.is_empty() {
            String::new()
        } else {
            format!("{}{}{}", '{', labels.join(","), '}')
        };
        match value {
            DebugValue::Counter(v) => println!("  {}{label_str} = {v}", key.name()),
            DebugValue::Gauge(v)   => println!("  {}{label_str} = {}", key.name(), v.into_inner()),
            DebugValue::Histogram(s) => {
                let n = s.len();
                let sum: f64 = s.iter().map(|x| x.into_inner()).sum();
                let summary = format!("count={n}, sum={sum:.6}s");
                println!("  {}{label_str} = {}{summary}{}", key.name(), '{', '}');
            }
        }
    }
}
```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example metrics_demo --features "metrics scheduler"</code> installs a
<code>DebuggingRecorder</code>, attaches a <code>MetricsObserver</code>, runs a workflow and a scheduled
flow, and prints every emitted metric. For a real Prometheus exporter, swap in
<code>metrics_exporter_prometheus::PrometheusBuilder::new().install()</code> before
<code>cano::metrics::describe()</code>.</p>
</div>
