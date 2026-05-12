+++
title = "Metrics"
description = "Optional metrics-crate counters, histograms, and gauges for Cano workflows via the metrics feature."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Metrics</h1>
<p class="subtitle"><code>metrics</code>-crate counters, histograms, and gauges for your workflows.</p>
<p class="feature-tag">Behind the <code>metrics</code> feature gate (<code>features = ["metrics"]</code>).</p>

<div class="callout callout-info">
<span class="callout-label">See also</span>
<p>This page covers Cano's built-in <code>metrics</code> instrumentation and the <code>MetricsObserver</code>.
For the synchronous callback API (<code>WorkflowObserver</code>) that <code>MetricsObserver</code> builds on,
see <a href="../observers/">Observers</a>. For <code>tracing</code>-crate span instrumentation, the
sibling observability feature, see <a href="../tracing/">Tracing</a>.</p>
</div>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#setup">Setup</a></li>
<li><a href="#two-surfaces">Two Surfaces</a></li>
<li><a href="#what-gets-measured">What Gets Measured</a></li>
<li><a href="#describe">Registering Descriptions</a></li>
<li><a href="#cardinality">Cardinality</a></li>
<li><a href="#cost">Cost</a></li>
<li><a href="#known-limitation">Known Limitation</a></li>
<li><a href="#full-example">Full Example</a></li>
</ol>
</nav>

<p>
Cano provides optional metrics instrumentation through the <code>metrics</code> feature using the
<a href="https://docs.rs/metrics/latest/metrics/" target="_blank">metrics</a> facade crate.
The <code>metrics</code> crate is recorder-agnostic: you install any compatible exporter
(Prometheus, StatsD, a debugging snapshotter, …) and Cano emits to it. All instrumentation is
behind conditional compilation — zero overhead when the feature is disabled.
</p>

<p>
For a callback-style API — get notified on workflow lifecycle and failure events without depending
on the <code>metrics</code> ecosystem — see <a href="../observers/">Observers</a>. The
<code>metrics</code> feature ships a ready-made <code>MetricsObserver</code> that bridges the two:
attach it with <code>.with_observer(Arc::new(MetricsObserver::new()))</code> to re-emit those
observer hooks as <code>metrics</code>-crate counters.
</p>
<hr class="section-divider">

<h2 id="setup"><a href="#setup" class="anchor-link" aria-hidden="true">#</a>Setup</h2>
<p>Enable the <code>metrics</code> feature flag in your <code>Cargo.toml</code>. You can also use
<code>features = ["all"]</code> to enable everything (<code>scheduler</code> + <code>tracing</code> +
<code>recovery</code> + <code>metrics</code>) at once.</p>

```toml
[dependencies]
cano = { version = "0.12", features = ["metrics"] }

# The metrics crate is a facade — you also need a recorder/exporter:
metrics-exporter-prometheus = "0.16"  # for production
# or: metrics-util = "0.18"           # for testing / debugging snapshots

# Or enable everything (scheduler + tracing + recovery + metrics):
# cano = { version = "0.12", features = ["all"] }

```

<p>
Because <code>metrics</code> is a facade, Cano only depends on the shared interface. Your application
picks the concrete recorder (e.g. <code>metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder()</code>
for a Prometheus scrape endpoint, or <code>metrics_util::debugging::DebuggingRecorder</code> in tests).
Call <code>cano::metrics::describe()</code> once, after installing your recorder, so exporters receive
help text and units.
</p>
<hr class="section-divider">

<h2 id="two-surfaces"><a href="#two-surfaces" class="anchor-link" aria-hidden="true">#</a>Two Surfaces</h2>
<p>
The <code>metrics</code> feature exposes instrumentation through two complementary surfaces, mirroring how
<a href="../tracing/">Tracing</a> pairs engine spans with the <code>TracingObserver</code> bridge.
</p>

<div class="card-stack">
<div class="card">
<h3 id="metrics-observer"><a href="#metrics-observer" class="anchor-link" aria-hidden="true">#</a><code>MetricsObserver</code> — opt-in lifecycle counters</h3>
<p>
<code>MetricsObserver</code> is a <code>WorkflowObserver</code> that re-emits the observer hooks as
<code>metrics</code>-crate counters. Wire it up in one line:
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;

Workflow::bare()
    .register(/* ... */)
    .add_exit_state(/* ... */)
    .with_observer(Arc::new(MetricsObserver::new()))
```

<p>It emits these counters (each incremented on the corresponding observer hook):</p>
<ul>
<li><code>cano_state_enters_total{state}</code> — on <code>on_state_enter</code></li>
<li><code>cano_observed_task_runs_total{task, outcome}</code> — on <code>on_task_success</code> / <code>on_task_failure</code> (<code>outcome</code> ∈ <code>success</code>|<code>failure</code>)</li>
<li><code>cano_task_retries_total{task}</code> — on <code>on_retry</code></li>
<li><code>cano_circuit_open_events_total{task}</code> — on <code>on_circuit_open</code></li>
<li><code>cano_checkpoints_observed_total</code> — on <code>on_checkpoint</code></li>
<li><code>cano_resumes_total</code> — on <code>on_resume</code></li>
</ul>
<p>
<code>MetricsObserver</code> is in the prelude behind the <code>metrics</code> feature — no extra import needed
when you use <code>use cano::prelude::*</code>.
</p>
</div>
<div class="card">
<h3 id="always-on"><a href="#always-on" class="anchor-link" aria-hidden="true">#</a>Always-on direct instrumentation — engine internals</h3>
<p>
Compiled in whenever the <code>metrics</code> feature is on, regardless of whether a
<code>MetricsObserver</code> is attached. Covers engine internals the observer hooks do not reach:
workflow run duration, circuit-breaker state transitions, per-attempt retry-loop outcomes,
poll/batch/step iteration counts, scheduler flow telemetry, and checkpoint store operations.
See <a href="#what-gets-measured">What Gets Measured</a> for the full list.
</p>
</div>
</div>
<hr class="section-divider">

<h2 id="what-gets-measured"><a href="#what-gets-measured" class="anchor-link" aria-hidden="true">#</a>What Gets Measured</h2>
<p>
Histograms record raw <code>f64</code> seconds samples — bucketing and quantile computation are the
exporter's responsibility. Metric names follow <code>metrics</code>-crate underscore conventions.
</p>

<div class="card-grid">
<div class="card">
<h3>Workflow</h3>
<ul>
<li><code>cano_workflow_runs_total{outcome}</code> — counter; <code>outcome</code> ∈ <code>completed</code>|<code>failed</code>|<code>timeout</code></li>
<li><code>cano_workflow_duration_seconds{outcome}</code> — histogram (seconds)</li>
<li><code>cano_workflow_active</code> — gauge; workflows currently executing</li>
</ul>
</div>
<div class="card">
<h3>Task Dispatch</h3>
<ul>
<li><code>cano_task_duration_seconds{state, kind}</code> — histogram (seconds); <code>kind</code> ∈ <code>single</code>|<code>router</code>|<code>split</code>|<code>compensatable</code>|<code>stepped</code></li>
<li><code>cano_task_attempts_total{outcome}</code> — counter; per-attempt inside the retry loop; <code>outcome</code> ∈ <code>completed</code>|<code>failed</code></li>
<li><code>cano_circuit_rejections_total</code> — counter; attempts short-circuited by an open breaker</li>
</ul>
</div>
<div class="card">
<h3>Split / Join</h3>
<ul>
<li><code>cano_split_branch_results_total{result}</code> — counter; <code>result</code> ∈ <code>success</code>|<code>failure</code>|<code>cancelled</code></li>
</ul>
</div>
</div>

<div class="card-stack">
<div class="card">
<h3 id="circuit-breaker-metrics"><a href="#circuit-breaker-metrics" class="anchor-link" aria-hidden="true">#</a>Circuit Breaker</h3>
<ul>
<li><code>cano_circuit_transitions_total{transition}</code> — counter; <code>transition</code> ∈ <code>closed_to_open</code>|<code>open_to_halfopen</code>|<code>halfopen_to_closed</code>|<code>halfopen_to_open</code></li>
<li><code>cano_circuit_acquires_total{result}</code> — counter; <code>result</code> ∈ <code>acquired</code>|<code>rejected</code></li>
<li><code>cano_circuit_outcomes_total{outcome}</code> — counter; <code>outcome</code> ∈ <code>success</code>|<code>failure</code></li>
</ul>
</div>
<div class="card">
<h3 id="processing-loops"><a href="#processing-loops" class="anchor-link" aria-hidden="true">#</a>Processing Loops</h3>
<ul>
<li><code>cano_poll_iterations_total{outcome}</code> — counter; <code>outcome</code> ∈ <code>ready</code>|<code>pending</code></li>
<li><code>cano_batch_runs_total{outcome}</code> — counter; <code>outcome</code> ∈ <code>completed</code>|<code>failed</code></li>
<li><code>cano_batch_items_total{result}</code> — counter; <code>result</code> ∈ <code>ok</code>|<code>err</code></li>
<li><code>cano_step_iterations_total{outcome}</code> — counter; <code>outcome</code> ∈ <code>more</code>|<code>done</code></li>
</ul>
</div>
<div class="card">
<h3 id="recovery-saga-metrics"><a href="#recovery-saga-metrics" class="anchor-link" aria-hidden="true">#</a>Recovery &amp; Saga</h3>
<ul>
<li><code>cano_checkpoint_appends_total{result}</code> — counter; <code>result</code> ∈ <code>ok</code>|<code>err</code></li>
<li><code>cano_checkpoint_clears_total{result}</code> — counter; <code>result</code> ∈ <code>ok</code>|<code>err</code></li>
<li><code>cano_compensations_run_total{result}</code> — counter; <code>result</code> ∈ <code>ok</code>|<code>err</code></li>
<li><code>cano_compensation_drains_total{outcome}</code> — counter; <code>outcome</code> ∈ <code>clean</code>|<code>partial</code></li>
</ul>
</div>
<div class="card">
<h3 id="scheduler-metrics"><a href="#scheduler-metrics" class="anchor-link" aria-hidden="true">#</a>Scheduler</h3>
<p class="feature-tag" style="margin-top: 0; margin-bottom: 0.5rem;">Also requires the <code>scheduler</code> feature.</p>
<ul>
<li><code>cano_scheduler_flow_runs_total{flow, outcome}</code> — counter; <code>outcome</code> ∈ <code>completed</code>|<code>failed</code></li>
<li><code>cano_scheduler_flow_duration_seconds{flow}</code> — histogram (seconds)</li>
<li><code>cano_scheduler_flow_backoff_total{flow}</code> — counter</li>
<li><code>cano_scheduler_flow_tripped_total{flow}</code> — counter</li>
<li><code>cano_scheduler_active_flows</code> — gauge; flows currently executing</li>
</ul>
</div>
</div>
<hr class="section-divider">

<h2 id="describe"><a href="#describe" class="anchor-link" aria-hidden="true">#</a>Registering Descriptions</h2>
<p>
<code>cano::metrics::describe()</code> registers a human-readable description and unit for every metric
Cano emits. Call it once, after installing your recorder, so exporters receive help text in their
output (e.g. Prometheus <code># HELP</code> / <code># TYPE</code> lines).
</p>

```rust
// Install your recorder first, then describe:
metrics_exporter_prometheus::PrometheusBuilder::new()
    .install()
    .expect("install prometheus exporter");

cano::metrics::describe();
```

<p>
If you skip <code>describe()</code>, metrics still flow — only the help text and units are missing
from the exporter output.
</p>
<hr class="section-divider">

<h2 id="cardinality"><a href="#cardinality" class="anchor-link" aria-hidden="true">#</a>Cardinality</h2>
<p>
Labels are deliberately minimal to keep cardinality bounded:
</p>
<ul>
<li><code>state</code> — <code>format!("{:?}")</code> of your FSM state enum; bounded by registered states.</li>
<li><code>task</code> — <code>Task::name()</code>, which defaults to <code>std::any::type_name</code>; bounded by registered task types.</li>
<li><code>flow</code> — the scheduler flow id string; bounded by registered flows.</li>
<li>All other label values (<code>outcome</code>, <code>kind</code>, <code>result</code>, <code>transition</code>) are fixed, bounded enum labels.</li>
</ul>
<p>
The deepest hot-path metrics — per-attempt retry-loop counters (<code>cano_task_attempts_total</code>),
circuit-breaker internals, and poll/batch/step iteration counters — carry no per-state label,
keeping their cardinality constant regardless of how many states your workflow defines.
</p>
<hr class="section-divider">

<h2 id="cost"><a href="#cost" class="anchor-link" aria-hidden="true">#</a>Cost</h2>
<p>
Compiling the <code>metrics</code> feature in adds a small, bounded per-state-transition cost (formatting
the state label as a string for the <code>state</code> label) even when no recorder is installed. If
you are building a latency-critical service that does not collect metrics, leave the feature off.
Otherwise, when a recorder is installed, the overhead is the same as any other <code>metrics</code>-crate
emission — a hash-map lookup plus atomic increment, comparable to a log line.
</p>
<hr class="section-divider">

<h2 id="known-limitation"><a href="#known-limitation" class="anchor-link" aria-hidden="true">#</a>Known Limitation</h2>

<div class="callout callout-info">
<span class="callout-label">Note</span>
<p>
The <code>#[task::poll]</code> and <code>#[task::stepped]</code> macros have two usage forms.
The <strong>trait-impl</strong> form (<code>impl PollTask&lt;S&gt; for T</code> /
<code>impl SteppedTask&lt;S&gt; for T</code>) inlines the loop body into the synthesised
<code>Task::run</code>, so <code>cano_poll_iterations_total</code> and
<code>cano_step_iterations_total</code> are <em>not</em> emitted for that form.
</p>
<p>
The <strong>inherent-impl</strong> form (<code>#[task::poll(state = S)] impl T { async fn poll ... }</code>
/ <code>#[task::stepped(state = S)] impl T { async fn step ... }</code>, the recommended form) and
<code>Workflow::register_stepped</code> (engine-owned loop) both emit the iteration counters as
expected.
</p>
</div>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full Example</h2>
<p>
Install a <code>DebuggingRecorder</code> (useful for tests and self-contained demos), call
<code>cano::metrics::describe()</code>, attach a <code>MetricsObserver</code>, run a workflow directly
and then under the scheduler, then dump the captured snapshot. This mirrors the
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
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(TaskResult::Single(Step::Process))
    }
}

struct ProcessTask;

#[task]
impl Task<Step> for ProcessTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        tokio::time::sleep(Duration::from_millis(3)).await;
        Ok(TaskResult::Single(Step::Done))
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

    // 3. Run the same workflow under the scheduler for ~1.2s, firing every 1s.
    let mut scheduler = Scheduler::new();
    scheduler
        .every_seconds("demo_flow", workflow(), Step::Fetch, 1)
        .expect("register flow");
    let running = scheduler.start().await.expect("start scheduler");
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
            format!("{{{}}}", labels.join(","))
        };
        match value {
            DebugValue::Counter(v) => println!("  {}{label_str} = {v}", key.name()),
            DebugValue::Gauge(v)   => println!("  {}{label_str} = {}", key.name(), v.into_inner()),
            DebugValue::Histogram(s) => {
                let n = s.len();
                let sum: f64 = s.iter().map(|x| x.into_inner()).sum();
                println!("  {}{label_str} = {{count={n}, sum={sum:.6}s}}", key.name());
            }
        }
    }
}

```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example metrics_demo --features "metrics scheduler"</code> — installs a
<code>DebuggingRecorder</code>, attaches a <code>MetricsObserver</code>, runs a workflow and a scheduled
flow, and prints every emitted metric. For a real Prometheus exporter, swap in
<code>metrics_exporter_prometheus::PrometheusBuilder::new().install()</code> before
<code>cano::metrics::describe()</code>.</p>
</div>
