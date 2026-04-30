//! # Circuit Breaker Example — Protecting a Flaky Dependency
//!
//! Simulates an upstream service that is intermittently broken. A
//! [`CircuitBreaker`] is shared by every retry attempt; once the failure
//! threshold is reached the breaker opens and subsequent calls fail fast with
//! [`CanoError::CircuitOpen`] without invoking the task body. After the
//! `reset_timeout` cool-down, the breaker enters HalfOpen and probes the
//! dependency with one trial call; a success closes it again.
//!
//! Run with:
//! ```bash
//! cargo run --example circuit_breaker
//! ```

use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Call,
    Done,
}

/// A pretend HTTP client that fails until we flip a switch.
struct FlakyClient {
    healthy: Arc<AtomicBool>,
    call_count: Arc<AtomicUsize>,
}

#[resource]
impl Resource for FlakyClient {}

impl FlakyClient {
    fn new() -> Self {
        Self {
            healthy: Arc::new(AtomicBool::new(false)),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    async fn fetch(&self) -> Result<String, CanoError> {
        let n = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
        if self.healthy.load(Ordering::SeqCst) {
            Ok(format!("payload #{n}"))
        } else {
            Err(CanoError::task_execution(format!(
                "upstream call #{n} failed"
            )))
        }
    }
}

struct CallTask {
    breaker: Arc<CircuitBreaker>,
}

#[task]
impl Task<Step> for CallTask {
    fn config(&self) -> TaskConfig {
        // Disable retries so each orchestrate() = exactly one call against the breaker.
        // Real workloads usually keep a small retry budget here; we drop it for clarity.
        TaskConfig::minimal().with_circuit_breaker(Arc::clone(&self.breaker))
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let client = res.get::<FlakyClient, str>("client")?;
        let payload = client.fetch().await?;
        println!("    → got: {payload}");
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("=== Circuit breaker demo ===\n");

    let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
        failure_threshold: 3,
        reset_timeout: Duration::from_millis(150),
        half_open_max_calls: 1,
    }));

    let client = FlakyClient::new();
    let healthy_handle = Arc::clone(&client.healthy);
    let call_count = Arc::clone(&client.call_count);

    let resources = Resources::new().insert("client", client);
    let workflow = Workflow::new(resources)
        .register(
            Step::Call,
            CallTask {
                breaker: Arc::clone(&breaker),
            },
        )
        .add_exit_state(Step::Done);

    println!("Phase 1 — dependency unhealthy, threshold = 3.");
    for attempt in 1..=5 {
        let outcome = workflow.orchestrate(Step::Call).await;
        let label = match outcome {
            Ok(_) => "ok".to_string(),
            Err(CanoError::CircuitOpen(_)) => "rejected (breaker open)".to_string(),
            Err(other) => format!("err: {other}"),
        };
        println!("  call {attempt}: {label} | state={:?}", breaker.state());
    }

    println!(
        "\nUpstream invocations so far: {} (only the first 3 hit the dependency).",
        call_count.load(Ordering::SeqCst)
    );

    println!("\nPhase 2 — heal the dependency, wait for the cool-down.");
    healthy_handle.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;

    println!("Phase 3 — half-open trial probes the dependency, then closes the breaker.");
    for attempt in 1..=3 {
        match workflow.orchestrate(Step::Call).await {
            Ok(_) => println!(
                "  recovery call {attempt}: ok | state={:?}",
                breaker.state()
            ),
            Err(e) => println!("  recovery call {attempt}: err: {e}"),
        }
    }

    println!(
        "\nFinal upstream invocation count: {}",
        call_count.load(Ordering::SeqCst)
    );
    println!("Final breaker state: {:?}", breaker.state());
    Ok(())
}
