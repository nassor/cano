//! Correlating Cano's `metrics` with `tracing` **spans**.
//!
//! Run with: `cargo run --example metrics_tracing_context --features metrics,tracing`
//!
//! `metrics` and `tracing` interop through tracing spans via the
//! [`metrics-tracing-context`](https://docs.rs/metrics-tracing-context) crate: install its
//! `TracingContextLayer` around your metrics recorder and add its `MetricsLayer` to your
//! tracing subscriber, and then every `cano_*` metric emitted *inside* a span inherits that
//! span's fields as labels. This is wiring you do in your app — Cano never depends on
//! `metrics-tracing-context` itself (same way it never depends on `tracing-subscriber`).
//!
//! Two paths are demonstrated:
//!   1. Cano's own default `workflow_orchestrate` span carries `workflow_id` (when set via
//!      `Workflow::with_workflow_id`), so `workflow_id` shows up as a metric label.
//!   2. A span *you* open around `orchestrate()` (here `api_request` with a `request_id`)
//!      also flows onto the metrics recorded during that run.
//!
//! `install_metrics_with_tracing_context()` below is a drop-in helper you can copy.

use cano::prelude::*;
use metrics_tracing_context::{MetricsLayer, TracingContextLayer};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use metrics_util::layers::Layer;
use std::sync::Arc;
use std::time::Duration;
use tracing::info_span;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Fetch,
    Process,
    Done,
}

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

/// Install a metrics recorder wrapped so it reads the current tracing span's fields, plus
/// the matching tracing-subscriber layer. Returns a `Snapshotter` for the demo; in a real
/// app you'd swap `DebuggingRecorder` for an exporter (e.g. `metrics_exporter_prometheus`).
fn install_metrics_with_tracing_context() -> Snapshotter {
    let inner = DebuggingRecorder::new();
    let snapshotter = inner.snapshotter();

    // 1. wrap the recorder so metric calls pick up the current span's fields as labels.
    //    `all()` promotes *every* field of every entered span — including Cano-internal ones
    //    like `max_attempts` from the retry-loop spans. In production prefer
    //    `TracingContextLayer::new(filter)` with a `LabelFilter` that allow-lists just the
    //    fields you author, to keep metric cardinality under control.
    let recorder = TracingContextLayer::all().layer(inner);
    metrics::set_global_recorder(recorder).expect("install metrics recorder");
    cano::metrics::describe();

    // 2. add MetricsLayer to the tracing subscriber so spans expose their fields
    tracing_subscriber::registry()
        .with(MetricsLayer::new())
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();

    snapshotter
}

#[tokio::main]
async fn main() {
    let snapshotter = install_metrics_with_tracing_context();

    // Path 1: Cano's own `workflow_orchestrate` span carries `workflow_id`.
    workflow()
        .with_workflow_id("demo-run-1")
        .orchestrate(Step::Fetch)
        .await
        .expect("workflow run");

    // Path 2: a user span around orchestrate() — `request_id` flows onto the metrics too.
    {
        let span = info_span!("api_request", request_id = "abc");
        let _enter = span.enter();
        workflow()
            .orchestrate(Step::Fetch)
            .await
            .expect("workflow run");
    }

    // Dump every captured metric, sorted by name — note `workflow_id=` / `request_id=`
    // labels on the `cano_*` metrics, contributed purely by span context.
    println!("\n=== Cano metrics (labels include span context) ===");
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
            DebugValue::Gauge(v) => println!("  {}{label_str} = {}", key.name(), v.into_inner()),
            DebugValue::Histogram(s) => {
                let n = s.len();
                let sum: f64 = s.iter().map(|x| x.into_inner()).sum();
                println!("  {}{label_str} = {{count={n}, sum={sum:.6}s}}", key.name());
            }
        }
    }
}
