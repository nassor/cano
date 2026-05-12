//! # Observer & Health-Probe Example
//!
//! Demonstrates the two opt-in observability hooks Cano exposes besides `tracing`:
//!
//! 1. [`WorkflowObserver`] — synchronous callbacks for workflow lifecycle and
//!    failure events (`on_state_enter`, `on_task_start`, `on_task_success`,
//!    `on_task_failure`, `on_retry`, `on_circuit_open`). Attach one (or many)
//!    with [`Workflow::with_observer`].
//! 2. [`Resource::health`] / [`Resources::check_all_health`] /
//!    [`Resources::aggregate_health`] — on-demand health reporting for the
//!    resources a workflow depends on.
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_observer
//! ```

use cano::prelude::*;
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// A small observer that prints every event and tallies them.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PrintingObserver {
    total: AtomicUsize,
    failures: AtomicUsize,
    log: Mutex<Vec<String>>,
}

impl PrintingObserver {
    fn note(&self, line: String) {
        self.total.fetch_add(1, Ordering::Relaxed);
        println!("  observer ▸ {line}");
        self.log.lock().unwrap().push(line);
    }
}

impl WorkflowObserver for PrintingObserver {
    fn on_state_enter(&self, state: &str) {
        self.note(format!("enter state {state}"));
    }
    fn on_task_start(&self, task_id: &str) {
        self.note(format!("start  task  {task_id}"));
    }
    fn on_task_success(&self, task_id: &str) {
        self.note(format!("ok     task  {task_id}"));
    }
    fn on_task_failure(&self, task_id: &str, err: &CanoError) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        self.note(format!("FAIL   task  {task_id}: {err}"));
    }
    fn on_retry(&self, task_id: &str, attempt: u32) {
        self.note(format!("retry  task  {task_id} (attempt {attempt} failed)"));
    }
    fn on_circuit_open(&self, task_id: &str) {
        self.note(format!("circuit open — rejecting {task_id}"));
    }
}

// ---------------------------------------------------------------------------
// Workflow states.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Load,
    Probe,
    Done,
}

// ---------------------------------------------------------------------------
// Scenario A: a flaky task that fails twice, then succeeds → fires `on_retry`.
// ---------------------------------------------------------------------------

struct FlakyLoad {
    remaining_failures: Arc<AtomicUsize>,
}

#[task]
impl Task<Step> for FlakyLoad {
    fn config(&self) -> TaskConfig {
        TaskConfig::new().with_fixed_retry(3, Duration::from_millis(20))
    }
    fn name(&self) -> Cow<'static, str> {
        "load".into()
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        if self.remaining_failures.fetch_sub(1, Ordering::SeqCst) > 0 {
            return Err(CanoError::task_execution("upstream not ready yet"));
        }
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Scenario B: a task guarded by a circuit breaker that is already open →
// fires `on_circuit_open` then `on_task_failure`.
// ---------------------------------------------------------------------------

struct GuardedProbe {
    breaker: Arc<CircuitBreaker>,
}

#[task]
impl Task<Step> for GuardedProbe {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal().with_circuit_breaker(Arc::clone(&self.breaker))
    }
    fn name(&self) -> Cow<'static, str> {
        "probe".into()
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        // Never reached while the breaker is open.
        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Scenario C: resources that report their own health.
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct PrimaryDb; // default health() → Healthy

struct ReadReplica;
#[resource]
impl Resource for ReadReplica {
    async fn health(&self) -> HealthStatus {
        HealthStatus::Degraded("replication lag 8s".to_string())
    }
}

struct PaymentsApi;
#[resource]
impl Resource for PaymentsApi {
    async fn health(&self) -> HealthStatus {
        HealthStatus::Unhealthy("503 from gateway".to_string())
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let observer = Arc::new(PrintingObserver::default());

    // -- Scenario A -------------------------------------------------------
    println!("Scenario A — flaky task recovers after retries\n");
    let workflow = Workflow::bare()
        .register(
            Step::Load,
            FlakyLoad {
                remaining_failures: Arc::new(AtomicUsize::new(2)),
            },
        )
        .add_exit_state(Step::Done)
        .with_observer(observer.clone());
    let final_state = workflow.orchestrate(Step::Load).await?;
    println!("  → workflow finished in state {final_state:?}\n");

    // -- Scenario B -------------------------------------------------------
    println!("Scenario B — circuit breaker is open, the task is rejected\n");
    let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
        failure_threshold: 1,
        reset_timeout: Duration::from_secs(60),
        half_open_max_calls: 1,
    }));
    // Trip it: one recorded failure at threshold 1.
    let permit = breaker.try_acquire().expect("closed breaker admits");
    breaker.record_failure(permit);

    let guarded = Workflow::bare()
        .register(
            Step::Probe,
            GuardedProbe {
                breaker: Arc::clone(&breaker),
            },
        )
        .add_exit_state(Step::Done)
        .with_observer(observer.clone());
    match guarded.orchestrate(Step::Probe).await {
        Ok(s) => println!("  → unexpectedly finished in {s:?}\n"),
        Err(e) => println!("  → workflow errored as expected: {e}\n"),
    }

    println!(
        "Observer saw {} events ({} failures).\n",
        observer.total.load(Ordering::Relaxed),
        observer.failures.load(Ordering::Relaxed),
    );

    // -- Scenario C -------------------------------------------------------
    println!("Scenario C — resource health probes\n");
    let resources: Resources = Resources::new()
        .insert("db", PrimaryDb)
        .insert("replica", ReadReplica)
        .insert("payments", PaymentsApi);

    let mut report: Vec<_> = resources.check_all_health().await.into_iter().collect();
    report.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, status) in &report {
        println!("  {key:>9}: {status:?}");
    }
    println!(
        "  → aggregate health: {:?}",
        resources.aggregate_health().await
    );

    Ok(())
}
