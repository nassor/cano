//! # Observer as a Metrics Collector
//!
//! Run with: `cargo run --example observer_metrics`
//!
//! A [`WorkflowObserver`] doesn't have to print a live trace (see `workflow_observer.rs`
//! for that). This one stays quiet during the run, just incrementing atomic counters, and
//! prints a one-shot summary at the end — the shape you'd use to feed Prometheus, StatsD,
//! or your logging pipeline (push the numbers from `report()` instead of `println!`).
//!
//! The `Start → Flaky → Guarded → Done` workflow is run twice. `FlakyTask` fails once then
//! succeeds (one retry). `GuardedTask` always fails and is protected by a `CircuitBreaker`
//! with `failure_threshold: 1` — run 1 trips it; run 2 is rejected fast (`on_circuit_open`
//! + an `on_task_failure` carrying `CanoError::CircuitOpen`), without invoking the task body.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Start,
    Flaky,
    Guarded,
    Done,
}

/// Counts every observer callback; `report()` prints the totals. Cheap and lock-free, so it
/// imposes nothing on the workflow's hot path.
#[derive(Default)]
struct Metrics {
    state_enters: AtomicUsize,
    task_starts: AtomicUsize,
    task_successes: AtomicUsize,
    task_failures: AtomicUsize,
    retries: AtomicUsize,
    circuit_opens: AtomicUsize,
}

impl WorkflowObserver for Metrics {
    fn on_state_enter(&self, _state: &str) {
        self.state_enters.fetch_add(1, Ordering::Relaxed);
    }
    fn on_task_start(&self, _task_id: &str) {
        self.task_starts.fetch_add(1, Ordering::Relaxed);
    }
    fn on_task_success(&self, _task_id: &str) {
        self.task_successes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_task_failure(&self, _task_id: &str, _err: &CanoError) {
        self.task_failures.fetch_add(1, Ordering::Relaxed);
    }
    fn on_retry(&self, _task_id: &str, _attempt: u32) {
        self.retries.fetch_add(1, Ordering::Relaxed);
    }
    fn on_circuit_open(&self, _task_id: &str) {
        self.circuit_opens.fetch_add(1, Ordering::Relaxed);
    }
}

impl Metrics {
    fn report(&self) {
        let n = |a: &AtomicUsize| a.load(Ordering::Relaxed);
        println!("\nworkflow metrics:");
        println!("  state entries     {}", n(&self.state_enters));
        println!("  tasks started     {}", n(&self.task_starts));
        println!("  tasks succeeded   {}", n(&self.task_successes));
        println!("  tasks failed      {}", n(&self.task_failures));
        println!("  retries           {}", n(&self.retries));
        println!("  circuit opens     {}", n(&self.circuit_opens));
    }
}

#[derive(Clone)]
struct StartTask;
#[derive(Clone)]
struct FlakyTask {
    calls: Arc<AtomicUsize>,
}
#[derive(Clone)]
struct GuardedTask {
    breaker: Arc<CircuitBreaker>,
}

#[task(state = Step)]
impl StartTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Flaky))
    }
}

#[task(state = Step)]
impl FlakyTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::new().with_fixed_retry(3, Duration::from_millis(10))
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(CanoError::task_execution("not warmed up")) // first call ever → one retry
        } else {
            Ok(TaskResult::Single(Step::Guarded))
        }
    }
}

#[task(state = Step)]
impl GuardedTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal().with_circuit_breaker(Arc::clone(&self.breaker))
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Err(CanoError::task_execution("dependency down")) // always fails → trips the breaker
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let metrics = Arc::new(Metrics::default());
    let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
        failure_threshold: 1,
        reset_timeout: Duration::from_secs(3600), // stays open for the duration of this demo
        half_open_max_calls: 1,
    }));

    let workflow = Workflow::bare()
        .register(Step::Start, StartTask)
        .register(
            Step::Flaky,
            FlakyTask {
                calls: Arc::new(AtomicUsize::new(0)),
            },
        )
        .register(
            Step::Guarded,
            GuardedTask {
                breaker: Arc::clone(&breaker),
            },
        )
        .add_exit_state(Step::Done)
        .with_observer(metrics.clone());

    for run in 1..=2 {
        match workflow.orchestrate(Step::Start).await {
            Ok(state) => println!("run {run}: reached {state:?}"),
            Err(error) => println!("run {run}: stopped — {error}"),
        }
    }

    metrics.report();
    Ok(())
}
