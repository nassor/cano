//! # Circuit Breaker — Manual API (try_acquire / record_success / record_failure)
//!
//! Run with: `cargo run --example circuit_breaker_manual`
//!
//! Demonstrates driving a [`CircuitBreaker`] **by hand** inside a task body, without
//! the `TaskConfig::with_circuit_breaker` integration. The task retrieves an
//! `Arc<CircuitBreaker>` from [`Resources`] and calls:
//!
//! - [`CircuitBreaker::try_acquire`] — obtain a [`CircuitPermit`] or receive
//!   [`CanoError::CircuitOpen`] when the breaker is open/saturated.
//! - [`CircuitBreaker::record_success`] / [`CircuitBreaker::record_failure`] — consume
//!   the permit after the call, updating the breaker's internal counters.
//!
//! Dropping a permit without consuming it counts as a failure (RAII guard), which
//! provides safety on early returns and panics.
//!
//! State machine shown end-to-end:
//!
//! ```text
//! Closed (0 failures)
//!   │  3 consecutive failures
//!   ▼
//! Open (calls rejected until reset_timeout elapses)
//!   │  reset_timeout elapsed + next try_acquire
//!   ▼
//! HalfOpen (1 trial call admitted)
//!   │  trial succeeds
//!   ▼
//! Closed (reset)
//! ```
//!
//! The "integrated" path (wiring the breaker into [`TaskConfig`]) is shown in the
//! `circuit_breaker` example. Use this manual approach when one task body needs to
//! wrap several distinct sub-calls behind the same breaker.

use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Call,
    Done,
}

// ---------------------------------------------------------------------------
// Simulated flaky dependency
// ---------------------------------------------------------------------------

/// A fake external client. Fails while `healthy` is `false`.
struct FlakyClient {
    healthy: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

#[resource]
impl Resource for FlakyClient {}

impl FlakyClient {
    fn new() -> Self {
        Self {
            healthy: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    async fn fetch(&self) -> Result<String, CanoError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.healthy.load(Ordering::SeqCst) {
            Ok(format!("data-{n}"))
        } else {
            Err(CanoError::task_execution(format!(
                "upstream call #{n} failed"
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Task — manually drives the circuit breaker
// ---------------------------------------------------------------------------

struct ManualTask;

#[task]
impl Task<Step> for ManualTask {
    fn config(&self) -> TaskConfig {
        // No automatic retry or circuit-breaker wiring — we do it ourselves.
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let breaker = res.get::<CircuitBreaker, _>("breaker")?;
        let client = res.get::<FlakyClient, _>("client")?;

        // Step 1: request a permit. Returns Err(CircuitOpen) if the breaker is open.
        let permit = breaker.try_acquire()?;

        // Step 2: perform the actual call.
        match client.fetch().await {
            Ok(payload) => {
                // Step 3a: success — tell the breaker; this resets the failure
                // counter in Closed, or advances the success counter in HalfOpen.
                breaker.record_success(permit);
                println!("    -> got: {payload}");
                Ok(TaskResult::Single(Step::Done))
            }
            Err(e) => {
                // Step 3b: failure — tell the breaker; this increments the failure
                // counter in Closed, or immediately reopens in HalfOpen.
                breaker.record_failure(permit);
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("=== Circuit Breaker Manual API Demo ===\n");

    let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
        failure_threshold: 3,
        reset_timeout: Duration::from_millis(150),
        half_open_max_calls: 1,
    }));

    let client = FlakyClient::new();
    let healthy = Arc::clone(&client.healthy);
    let call_count = Arc::clone(&client.calls);

    let resources = Resources::new()
        .insert("breaker", (*breaker).clone()) // store the breaker as a Resource
        .insert("client", client);

    let workflow = Workflow::new(resources)
        .register(Step::Call, ManualTask)
        .add_exit_state(Step::Done);

    // ------------------------------------------------------------------
    // Phase 1 — dependency unhealthy; accumulate failures until trip.
    // ------------------------------------------------------------------
    println!("Phase 1: dependency unhealthy (threshold = 3 consecutive failures)");
    for attempt in 1..=5 {
        let outcome = workflow.orchestrate(Step::Call).await;
        let label = match &outcome {
            Ok(_) => "ok".to_string(),
            Err(CanoError::CircuitOpen(_)) => "rejected (breaker open)".to_string(),
            Err(other) => format!("err: {other}"),
        };
        println!("  call {attempt}: {label} | breaker={:?}", breaker.state());
    }

    println!(
        "\nUpstream hit count: {} (first 3 reached the dependency; 2 were rejected by open breaker)",
        call_count.load(Ordering::SeqCst)
    );

    // ------------------------------------------------------------------
    // Phase 2 — heal the dependency and wait for the cool-down.
    // ------------------------------------------------------------------
    println!("\nPhase 2: heal dependency and sleep past reset_timeout (150ms)");
    healthy.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    println!("  breaker after sleep: {:?}", breaker.state());

    // ------------------------------------------------------------------
    // Phase 3 — HalfOpen trial probe closes the breaker.
    // ------------------------------------------------------------------
    println!("\nPhase 3: half-open trial — one probe closes the breaker");
    for attempt in 1..=3 {
        match workflow.orchestrate(Step::Call).await {
            Ok(_) => println!("  call {attempt}: ok | breaker={:?}", breaker.state()),
            Err(e) => println!("  call {attempt}: err: {e} | breaker={:?}", breaker.state()),
        }
    }

    println!(
        "\nFinal upstream hit count: {}",
        call_count.load(Ordering::SeqCst)
    );
    println!("Final breaker state: {:?}", breaker.state());
    assert_eq!(
        breaker.state(),
        CircuitState::Closed,
        "breaker should be closed after a successful HalfOpen probe"
    );

    println!("\n=== Done ===");
    Ok(())
}
