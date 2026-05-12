//! Demonstrates Cano's `metrics` feature: attach a `MetricsObserver`, run a workflow several
//! times and once under the scheduler, then dump every metric Cano emitted.
//!
//! Run with: `cargo run --example metrics_demo --features metrics,scheduler`
//!
//! In a real application you would install an exporter instead of the debugging recorder —
//! e.g. `metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder()` — and call
//! `cano::metrics::describe()` once so help text and units reach your backend. Attaching the
//! `MetricsObserver` is what enables the workflow-level counters (state enters, task runs,
//! retries, …); the engine-internal metrics (workflow run duration, circuit internals,
//! scheduler flows, …) are always-on once the `metrics` feature is compiled in.

use cano::prelude::*;
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use std::sync::Arc;
use std::time::Duration;

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

#[tokio::main]
async fn main() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::set_global_recorder(recorder).expect("install metrics recorder");
    cano::metrics::describe();

    // Run the workflow 3 times directly.
    for _ in 0..3 {
        workflow()
            .orchestrate(Step::Fetch)
            .await
            .expect("workflow run");
    }

    // Run the same workflow under the scheduler for ~1.2s, firing every 1s.
    let mut scheduler = Scheduler::new();
    scheduler
        .every_seconds("demo_flow", workflow(), Step::Fetch, 1)
        .expect("register flow");
    let running = scheduler.start().await.expect("start scheduler");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    running.stop().await.expect("stop scheduler");

    // Dump every captured metric, sorted alphabetically by name.
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
            DebugValue::Gauge(v) => println!("  {}{label_str} = {}", key.name(), v.into_inner()),
            DebugValue::Histogram(s) => {
                let n = s.len();
                let sum: f64 = s.iter().map(|x| x.into_inner()).sum();
                println!("  {}{label_str} = {{count={n}, sum={sum:.6}s}}", key.name());
            }
        }
    }
}
