#![cfg(all(feature = "scheduler", feature = "store-redb"))]
//! # Scheduler Cluster Example
//!
//! Two schedulers share one `SchedulerStore` and run the same flows. Each tick of the
//! `tick` flow runs on exactly one of them, and a manual `report` run on one instance is
//! refused on the other with `Status::RunningElsewhere`.
//!
//! Both instances live in one process and share one `RedbStore`, which is enough to show
//! the coordination. Instances in separate processes share a `PostgresStore` or a
//! `NatsStore` instead; the scheduler code is the same.
//!
//! Run with:
//! ```bash
//! cargo run --example scheduler_cluster --features scheduler
//! ```
use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tokio::time::{Duration, sleep};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FlowState {
    Start,
    Done,
}

/// Counts its runs in the counter of the instance that executes it.
#[derive(Clone)]
struct CountTick {
    runs: Arc<AtomicUsize>,
}

#[task(state = FlowState)]
impl CountTick {
    async fn run(&self) -> Result<FlowState, CanoError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(FlowState::Done)
    }
}

/// A manual run that takes long enough to still be running when the other node tries it.
#[derive(Clone)]
struct SlowReport;

#[task(state = FlowState)]
impl SlowReport {
    async fn run(&self) -> Result<FlowState, CanoError> {
        sleep(Duration::from_millis(800)).await;
        Ok(FlowState::Done)
    }
}

const TICK_INTERVAL: Duration = Duration::from_millis(200);

/// Builds one instance. Every instance registers the same flows and uses the same store.
async fn start_node(
    id: &str,
    store: Arc<RedbStore>,
    runs: Arc<AtomicUsize>,
) -> CanoResult<RunningScheduler> {
    let tick = Workflow::bare()
        .register(FlowState::Start, CountTick { runs })
        .add_exit_state(FlowState::Done);
    let report = Workflow::bare()
        .register(FlowState::Start, SlowReport)
        .add_exit_state(FlowState::Done);

    Scheduler::new()
        .every("tick", tick, FlowState::Start, TICK_INTERVAL)
        .manual("report", report, FlowState::Start)
        .with_store(store)
        .with_lease_ttl(Duration::from_secs(5))
        .with_instance_id(id)
        .start()
        .await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Scheduler Cluster Example");
    println!("=========================");

    // A unique path in the OS temp dir, so the example never touches a user state dir.
    let db_path = std::env::temp_dir().join(format!(
        "cano_scheduler_cluster_{}.redb",
        std::process::id()
    ));
    let store = Arc::new(RedbStore::new(&db_path)?);

    let runs_a = Arc::new(AtomicUsize::new(0));
    let runs_b = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let node_a = start_node("node-a", store.clone(), runs_a.clone()).await?;
    let node_b = start_node("node-b", store.clone(), runs_b.clone()).await?;
    println!("node-a: {}", node_a.instance_id());
    println!("node-b: {}", node_b.instance_id());

    // Both nodes dispatch "tick" every 200 ms, but the shared record admits one run per interval.
    sleep(Duration::from_millis(1500)).await;
    let elapsed = started.elapsed();
    let a = runs_a.load(Ordering::SeqCst);
    let b = runs_b.load(Ordering::SeqCst);
    let intervals = (elapsed.as_millis() / TICK_INTERVAL.as_millis()) as usize;
    println!(
        "\nAfter {elapsed:.1?}: node-a ran {a}, node-b ran {b}, total {} (at most {})",
        a + b,
        intervals + 1
    );
    assert!(a + b >= 3, "the cluster ran only {} ticks", a + b);
    assert!(
        a + b <= intervals + 1,
        "{} runs in {intervals} intervals: some tick ran on both nodes",
        a + b
    );

    // node-a starts the slow manual flow, so node-b is refused while it runs.
    node_a.trigger("report").await?;
    let refused = node_b.trigger("report").await;
    println!("\nnode-b trigger while node-a runs report: {refused:?}");
    assert!(refused.is_err(), "node-b must not start a run node-a holds");
    let status = node_b.status("report").expect("flow exists").status;
    println!("node-b sees report as {status:?}");
    assert!(matches!(status, Status::RunningElsewhere { .. }));

    node_a.wait_idle("report").await;
    node_b.trigger("report").await?;
    node_b.wait_idle("report").await;
    println!("node-b ran report once node-a finished");

    node_a.stop().await?;
    node_b.stop().await?;
    drop(store);
    std::fs::remove_file(&db_path).ok();

    println!("\n=== Done ===");
    Ok(())
}
