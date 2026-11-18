#![cfg(all(feature = "scheduler", feature = "testing"))]
//! End-to-end resilience tests for `Scheduler` coordination, clock/outage
//! tolerance, status normalization, payload truncation, and `CheckpointStore`
//! auto-resume across restarts, cancellations, and failovers.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

use cano::prelude::*;
use cano::testing::{InMemoryCheckpointStore, InMemorySchedulerStore};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Start,
    Mid,
    Done,
}

#[derive(Default)]
struct FaultStore {
    inner: InMemorySchedulerStore,
    down: AtomicBool,
    fail_next_load: AtomicU32,
    lost_ack_next_claim_swap: AtomicBool,
    swap_delay_ms: AtomicU32,
}

impl FaultStore {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[cano::scheduler_store]
impl SchedulerStore for FaultStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        if self.down.load(SeqCst) {
            return Err(CanoError::scheduler_store("down"));
        }
        if self
            .fail_next_load
            .try_update(SeqCst, SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(CanoError::scheduler_store("transient load failure"));
        }
        self.inner.load(flow_id).await
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        let delay = self.swap_delay_ms.load(SeqCst);
        if delay > 0 {
            sleep(Duration::from_millis(u64::from(delay))).await;
        }
        if self.down.load(SeqCst) {
            return Err(CanoError::scheduler_store("down"));
        }
        if self.lost_ack_next_claim_swap.swap(false, SeqCst) {
            let _ = self
                .inner
                .compare_and_swap(flow_id, expected, value)
                .await?;
            return Err(CanoError::scheduler_store("lost ACK on claim"));
        }
        self.inner.compare_and_swap(flow_id, expected, value).await
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        if self.down.load(SeqCst) {
            return Err(CanoError::scheduler_store("down"));
        }
        self.inner.remove(flow_id).await
    }
}

/// Minimal view of the JSON `FlowRecord` for seeding/inspecting records in integration tests.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct RecordView {
    run_count: u64,
    last_run: Option<DateTime<Utc>>,
    last_tick: Option<DateTime<Utc>>,
    failure_streak: u32,
    next_eligible: Option<DateTime<Utc>>,
    last_error: Option<String>,
    state: String,
    lease: Option<LeaseView>,
    written_by: String,
    written_session: u64,
    written_at: DateTime<Utc>,
    write_seq: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseView {
    owner: String,
    session: u64,
    id: u64,
    expires_at: DateTime<Utc>,
}

fn read_record(store: &InMemorySchedulerStore, flow_id: &str) -> RecordView {
    let raw = store.record(flow_id).expect("record must exist");
    serde_json::from_slice(&raw.value).unwrap()
}

async fn wait_until(budget: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while !pred() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(10)).await;
    }
    true
}

#[derive(Clone)]
struct CountTask {
    runs: Arc<AtomicUsize>,
    delay: Duration,
}

#[task(state = Step)]
impl CountTask {
    async fn run(&self) -> Result<Step, CanoError> {
        self.runs.fetch_add(1, SeqCst);
        if !self.delay.is_zero() {
            sleep(self.delay).await;
        }
        Ok(Step::Done)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_and_cron_recover_cleanly_after_claim_lost_ack_without_skipping_or_tripping() {
    let store_every = FaultStore::new();
    store_every.lost_ack_next_claim_swap.store(true, SeqCst);
    let every_runs = Arc::new(AtomicUsize::new(0));
    let every_wf = Workflow::bare()
        .register(
            Step::Start,
            CountTask {
                runs: Arc::clone(&every_runs),
                delay: Duration::ZERO,
            },
        )
        .add_exit_state(Step::Done);

    let sched_every = Scheduler::new()
        .with_store(Arc::clone(&store_every) as Arc<dyn SchedulerStore>)
        .with_instance_id("node-every")
        .with_lease_ttl(Duration::from_millis(150))
        .every("e", every_wf, Step::Start, Duration::from_secs(3600))
        .with_backoff("e", BackoffPolicy::with_trip(1))
        .start()
        .await
        .unwrap();

    // First claim hits lost ACK, abort_claim rolls back, and every retries in ~ttl/3 (50ms)
    // without tripping or waiting 3600s.
    assert!(
        wait_until(Duration::from_secs(2), || every_runs.load(SeqCst) == 1).await,
        "every flow must recover cleanly after lost-ACK claim without tripping"
    );
    assert!(
        wait_until(Duration::from_secs(1), || {
            sched_every.status("e").unwrap().status == Status::Completed
        })
        .await
    );
    assert_eq!(read_record(&store_every.inner, "e").run_count, 1);
    assert_eq!(read_record(&store_every.inner, "e").failure_streak, 0);
    sched_every.stop().await.unwrap();

    // Cron flow with lost ACK on first firing swap.
    let store_cron = FaultStore::new();
    store_cron.lost_ack_next_claim_swap.store(true, SeqCst);
    let cron_runs = Arc::new(AtomicUsize::new(0));
    let cron_wf = Workflow::bare()
        .register(
            Step::Start,
            CountTask {
                runs: Arc::clone(&cron_runs),
                delay: Duration::ZERO,
            },
        )
        .add_exit_state(Step::Done);

    let sched_cron = Scheduler::new()
        .with_store(Arc::clone(&store_cron) as Arc<dyn SchedulerStore>)
        .with_instance_id("node-cron")
        .with_lease_ttl(Duration::from_millis(150))
        .cron("c", cron_wf, Step::Start, "* * * * * *")
        .with_backoff("c", BackoffPolicy::with_trip(1))
        .start()
        .await
        .unwrap();

    assert!(
        wait_until(Duration::from_secs(3), || cron_runs.load(SeqCst) >= 1).await,
        "cron flow must retry due tick after lost ACK without tripping"
    );
    assert!(!matches!(
        sched_cron.status("c").unwrap().status,
        Status::Tripped { .. }
    ));
    sched_cron.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cron_retries_due_tick_on_transient_store_error_and_every_uses_ttl_third_retry() {
    // 1. Every flow with 1h interval + transient startup outage retries at ttl/3 (50ms).
    let store = FaultStore::new();
    store.down.store(true, SeqCst);
    let runs = Arc::new(AtomicUsize::new(0));
    let wf = Workflow::bare()
        .register(
            Step::Start,
            CountTask {
                runs: Arc::clone(&runs),
                delay: Duration::ZERO,
            },
        )
        .add_exit_state(Step::Done);

    let running = Scheduler::new()
        .with_store(Arc::clone(&store) as Arc<dyn SchedulerStore>)
        .with_instance_id("node-1")
        .with_lease_ttl(Duration::from_millis(150))
        .every("long", wf, Step::Start, Duration::from_secs(3600))
        .start()
        .await
        .unwrap();

    sleep(Duration::from_millis(80)).await;
    assert_eq!(runs.load(SeqCst), 0);
    store.down.store(false, SeqCst);
    assert!(
        wait_until(Duration::from_secs(2), || runs.load(SeqCst) == 1).await,
        "every flow with long interval must retry in ttl/3 after store recovery"
    );
    running.stop().await.unwrap();

    // 2. Cron flow hitting a transient store error on a due firing retries within the
    // same firing window and executes that tick.
    let cron_store = FaultStore::new();
    // 1 failure consumed by preload, 2 consumed by cron_loop's first two claim attempts
    // before recovering within the same 1-second firing.
    cron_store.fail_next_load.store(3, SeqCst);
    let cron_runs = Arc::new(AtomicUsize::new(0));
    let cron_wf = Workflow::bare()
        .register(
            Step::Start,
            CountTask {
                runs: Arc::clone(&cron_runs),
                delay: Duration::ZERO,
            },
        )
        .add_exit_state(Step::Done);
    let cron_sched = Scheduler::new()
        .with_store(Arc::clone(&cron_store) as Arc<dyn SchedulerStore>)
        .with_instance_id("cron-node")
        .with_lease_ttl(Duration::from_millis(150))
        .cron("c", cron_wf, Step::Start, "* * * * * *")
        .start()
        .await
        .unwrap();

    assert!(
        wait_until(Duration::from_secs(3), || cron_runs.load(SeqCst) >= 1).await,
        "cron must retry due tick on transient store outage"
    );
    assert_eq!(cron_store.fail_next_load.load(SeqCst), 0);
    cron_sched.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn dispatch_racing_stop_abandons_claim_without_running_workflow() {
    let store = FaultStore::new();
    store.swap_delay_ms.store(180, SeqCst);
    let runs = Arc::new(AtomicUsize::new(0));
    let wf = Workflow::bare()
        .register(
            Step::Start,
            CountTask {
                runs: Arc::clone(&runs),
                delay: Duration::ZERO,
            },
        )
        .add_exit_state(Step::Done);

    let running = Scheduler::new()
        .with_store(Arc::clone(&store) as Arc<dyn SchedulerStore>)
        .with_instance_id("node-1")
        .with_lease_ttl(Duration::from_secs(5))
        .with_drain_timeout(Duration::from_secs(2))
        .every("f", wf, Step::Start, Duration::from_secs(60))
        .start()
        .await
        .unwrap();

    // Let every_loop enter its claim swap, then call stop() while swap is still in flight.
    sleep(Duration::from_millis(60)).await;
    running.stop().await.expect("stop must drain cleanly");

    assert_eq!(
        runs.load(SeqCst),
        0,
        "workflow must not start after stop() fired mid-claim"
    );
    let rec = read_record(&store.inner, "f");
    assert!(rec.lease.is_none());
    assert_eq!(rec.run_count, 0);
    assert!(rec.last_run.is_none());
    assert_eq!(running.status("f").unwrap().status, Status::Idle);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_and_list_normalize_expired_running_elsewhere_on_manual_and_cron_flows() {
    let store = Arc::new(InMemorySchedulerStore::new());
    let now = Utc::now();
    let expires = now + chrono::Duration::milliseconds(120);

    let completed_seed = RecordView {
        run_count: 1,
        last_run: Some(now),
        state: "completed".into(),
        lease: Some(LeaseView {
            owner: "other-a".into(),
            session: 99,
            id: 1,
            expires_at: expires,
        }),
        written_by: "other-a".into(),
        written_at: now,
        ..RecordView::default()
    };
    let backoff_seed = RecordView {
        run_count: 2,
        last_run: Some(now),
        failure_streak: 1,
        next_eligible: Some(now + chrono::Duration::seconds(60)),
        last_error: Some("boom".into()),
        state: "backoff".into(),
        lease: Some(LeaseView {
            owner: "other-b".into(),
            session: 88,
            id: 1,
            expires_at: expires,
        }),
        written_by: "other-b".into(),
        written_at: now,
        ..RecordView::default()
    };
    store
        .compare_and_swap(
            "manual_done",
            None,
            serde_json::to_vec(&completed_seed).unwrap(),
        )
        .await
        .unwrap();
    store
        .compare_and_swap(
            "manual_bo",
            None,
            serde_json::to_vec(&backoff_seed).unwrap(),
        )
        .await
        .unwrap();
    store
        .compare_and_swap(
            "cron_done",
            None,
            serde_json::to_vec(&completed_seed).unwrap(),
        )
        .await
        .unwrap();

    let wf = || {
        Workflow::bare()
            .register(
                Step::Start,
                CountTask {
                    runs: Arc::new(AtomicUsize::new(0)),
                    delay: Duration::ZERO,
                },
            )
            .add_exit_state(Step::Done)
    };
    let running = Scheduler::new()
        .with_store(Arc::clone(&store) as Arc<dyn SchedulerStore>)
        .with_instance_id("observer")
        .manual("manual_done", wf(), Step::Start)
        .manual("manual_bo", wf(), Step::Start)
        .cron("cron_done", wf(), Step::Start, "0 0 0 * * *")
        .start()
        .await
        .unwrap();

    assert!(matches!(
        running.status("manual_done").unwrap().status,
        Status::RunningElsewhere { .. }
    ));
    assert!(matches!(
        running.status("manual_bo").unwrap().status,
        Status::RunningElsewhere { .. }
    ));
    assert!(matches!(
        running.status("cron_done").unwrap().status,
        Status::RunningElsewhere { .. }
    ));

    // Wait past lease_expires without triggering or hitting the store again.
    sleep(Duration::from_millis(180)).await;
    assert_eq!(
        running.status("manual_done").unwrap().status,
        Status::Completed
    );
    assert!(matches!(
        running.status("manual_bo").unwrap().status,
        Status::Backoff { streak: 1, .. }
    ));
    assert_eq!(
        running.status("cron_done").unwrap().status,
        Status::Completed
    );

    let listed = running.list();
    assert_eq!(listed[0].status, Status::Completed);
    assert!(matches!(
        listed[1].status,
        Status::Backoff { streak: 1, .. }
    ));
    assert_eq!(listed[2].status, Status::Completed);

    running.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_checkpointed_stepped_flow_auto_resumes_across_failover_and_clears_exit_rows() {
    #[derive(Clone)]
    struct UncancellableSteppedCrash {
        seen: Arc<Mutex<Vec<Option<u32>>>>,
        stall_on_step1: Arc<AtomicBool>,
        stalled: Arc<AtomicBool>,
    }

    #[task::stepped(state = Step)]
    impl UncancellableSteppedCrash {
        async fn step(
            &self,
            _res: &Resources,
            cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, Step>, CanoError> {
            self.seen.lock().unwrap().push(cursor);
            match cursor {
                None => Ok(StepOutcome::More(0)),
                Some(0) => {
                    if self.stall_on_step1.load(SeqCst) {
                        self.stalled.store(true, SeqCst);
                        sleep(Duration::from_secs(60)).await;
                    }
                    Ok(StepOutcome::More(1))
                }
                Some(_) => Ok(StepOutcome::Done(Step::Done)),
            }
        }
    }

    let sched_store = Arc::new(InMemorySchedulerStore::new());
    let ckpt_store = Arc::new(InMemoryCheckpointStore::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stall = Arc::new(AtomicBool::new(true));
    let stalled = Arc::new(AtomicBool::new(false));
    let ttl = Duration::from_millis(180);

    // Note: no explicit .with_workflow_id(...) — defaults to flow_id ("stepped-flow").
    let build_wf = || {
        Workflow::bare()
            .with_checkpoint_store(Arc::clone(&ckpt_store) as Arc<dyn CheckpointStore>)
            .register(
                Step::Start,
                UncancellableSteppedCrash {
                    seen: Arc::clone(&seen),
                    stall_on_step1: Arc::clone(&stall),
                    stalled: Arc::clone(&stalled),
                },
            )
            .add_exit_state(Step::Done)
    };

    let inst_a = Scheduler::new()
        .with_store(Arc::clone(&sched_store) as Arc<dyn SchedulerStore>)
        .with_instance_id("inst-a")
        .with_lease_ttl(ttl)
        .manual("stepped-flow", build_wf(), Step::Start)
        .start()
        .await
        .unwrap();

    inst_a.trigger("stepped-flow").await.unwrap();
    assert!(wait_until(Duration::from_secs(2), || stalled.load(SeqCst)).await);
    // Simulate sudden process crash of inst-a (drop without stop()).
    drop(inst_a);

    sleep(ttl + Duration::from_millis(60)).await;
    stall.store(false, SeqCst);

    // Instance B takes over the expired lease and auto-resumes from persisted cursor 0.
    let inst_b = Scheduler::new()
        .with_store(Arc::clone(&sched_store) as Arc<dyn SchedulerStore>)
        .with_instance_id("inst-b")
        .with_lease_ttl(ttl)
        .manual("stepped-flow", build_wf(), Step::Start)
        .start()
        .await
        .unwrap();

    inst_b.trigger("stepped-flow").await.unwrap();
    assert_eq!(
        inst_b.wait_idle("stepped-flow").await.unwrap().status,
        Status::Completed
    );
    assert!(
        ckpt_store.rows("stepped-flow").is_empty(),
        "completed resumed flow must clear checkpoint rows"
    );
    assert_eq!(*seen.lock().unwrap(), vec![None, Some(0), Some(0), Some(1)]);

    // Subsequent trigger runs fresh from cursor None.
    inst_b.trigger("stepped-flow").await.unwrap();
    assert_eq!(
        inst_b.wait_idle("stepped-flow").await.unwrap().status,
        Status::Completed
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![None, Some(0), Some(0), Some(1), None, Some(0), Some(1)]
    );

    inst_b.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_checkpointed_saga_resumes_and_compensates_after_abort_and_failover() {
    #[derive(Clone)]
    struct SagaStep1 {
        log: Arc<Mutex<Vec<&'static str>>>,
    }
    #[cano::saga::task(state = Step)]
    impl SagaStep1 {
        type Output = u32;
        async fn run(&self) -> Result<(Step, u32), CanoError> {
            self.log.lock().unwrap().push("run:step1");
            Ok((Step::Mid, 7))
        }
        async fn compensate(&self, output: u32) -> Result<(), CanoError> {
            assert_eq!(output, 7);
            self.log.lock().unwrap().push("comp:step1");
            Ok(())
        }
    }

    #[derive(Clone)]
    struct MidTask {
        entered: Arc<AtomicBool>,
        should_hang: Arc<AtomicBool>,
        should_fail: Arc<AtomicBool>,
    }
    #[task(state = Step)]
    impl MidTask {
        async fn run(&self) -> Result<Step, CanoError> {
            self.entered.store(true, SeqCst);
            if self.should_hang.load(SeqCst) {
                sleep(Duration::from_secs(60)).await;
            }
            if self.should_fail.load(SeqCst) {
                return Err(CanoError::task_execution("mid failed on failover"));
            }
            Ok(Step::Done)
        }
    }

    let sched_store = Arc::new(InMemorySchedulerStore::new());
    let ckpt_store = Arc::new(InMemoryCheckpointStore::new());
    let log = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(AtomicBool::new(false));
    let should_hang = Arc::new(AtomicBool::new(true));
    let should_fail = Arc::new(AtomicBool::new(false));
    let ttl = Duration::from_millis(180);

    let build_wf = || {
        Workflow::bare()
            .with_checkpoint_store(Arc::clone(&ckpt_store) as Arc<dyn CheckpointStore>)
            .with_workflow_id("saga-ckpt")
            .register(
                Step::Start,
                SagaStep1 {
                    log: Arc::clone(&log),
                },
            )
            .register(
                Step::Mid,
                MidTask {
                    entered: Arc::clone(&entered),
                    should_hang: Arc::clone(&should_hang),
                    should_fail: Arc::clone(&should_fail),
                },
            )
            .add_exit_state(Step::Done)
    };

    // Instance A commits SagaStep1, enters Mid, then drops (crash) before releasing lease.
    let inst_a = Scheduler::new()
        .with_store(Arc::clone(&sched_store) as Arc<dyn SchedulerStore>)
        .with_instance_id("saga-a")
        .with_lease_ttl(ttl)
        .manual("saga", build_wf(), Step::Start)
        .start()
        .await
        .unwrap();
    inst_a.trigger("saga").await.unwrap();
    assert!(wait_until(Duration::from_secs(2), || entered.load(SeqCst)).await);
    drop(inst_a);

    sleep(ttl + Duration::from_millis(60)).await;
    should_hang.store(false, SeqCst);
    should_fail.store(true, SeqCst);

    // Instance B takes over, auto-resumes from Mid with SagaStep1's persisted compensation
    // rehydrated, fails Mid, compensates SagaStep1, and clears the checkpoint log.
    let inst_b = Scheduler::new()
        .with_store(Arc::clone(&sched_store) as Arc<dyn SchedulerStore>)
        .with_instance_id("saga-b")
        .with_lease_ttl(ttl)
        .manual("saga", build_wf(), Step::Start)
        .start()
        .await
        .unwrap();
    inst_b.trigger("saga").await.unwrap();
    let info = inst_b.wait_idle("saga").await.unwrap();
    assert!(matches!(info.status, Status::Backoff { streak: 2, .. }));
    assert_eq!(*log.lock().unwrap(), vec!["run:step1", "comp:step1"]);
    assert!(
        ckpt_store.rows("saga-ckpt").is_empty(),
        "clean compensation on resumed saga must clear checkpoint log"
    );

    // Next trigger runs cleanly from Start and reaches Done.
    should_fail.store(false, SeqCst);
    inst_b.trigger("saga").await.unwrap();
    assert_eq!(
        inst_b.wait_idle("saga").await.unwrap().status,
        Status::Completed
    );
    assert_eq!(
        *log.lock().unwrap(),
        vec!["run:step1", "comp:step1", "run:step1"]
    );
    assert!(ckpt_store.rows("saga-ckpt").is_empty());

    inst_b.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_checkpointed_flow_clears_stale_exit_rows_before_fresh_run() {
    let ckpt_store = Arc::new(InMemoryCheckpointStore::new());
    ckpt_store
        .append("stale-wf", CheckpointRow::new(0, "Start", "CountTask"))
        .await
        .unwrap();
    ckpt_store
        .append("stale-wf", CheckpointRow::new(1, "Done", ""))
        .await
        .unwrap();

    let runs = Arc::new(AtomicUsize::new(0));
    let wf = Workflow::bare()
        .with_checkpoint_store(Arc::clone(&ckpt_store) as Arc<dyn CheckpointStore>)
        .with_workflow_id("stale-wf")
        .register(
            Step::Start,
            CountTask {
                runs: Arc::clone(&runs),
                delay: Duration::ZERO,
            },
        )
        .add_exit_state(Step::Done);

    let running = Scheduler::new()
        .manual("flow", wf, Step::Start)
        .start()
        .await
        .unwrap();

    running.trigger("flow").await.unwrap();
    assert_eq!(
        running.wait_idle("flow").await.unwrap().status,
        Status::Completed
    );
    assert_eq!(
        runs.load(SeqCst),
        1,
        "stale exit rows must be cleared and a fresh run executed"
    );
    assert!(ckpt_store.rows("stale-wf").is_empty());

    running.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn standalone_last_error_over_1mib_is_truncated_safely() {
    #[derive(Clone)]
    struct BigErrTask;

    #[task(state = Step)]
    impl BigErrTask {
        async fn run(&self) -> Result<Step, CanoError> {
            let mut msg = "x".repeat(4091);
            msg.push('🦀');
            msg.push_str(&"y".repeat(1_100_000));
            Err(CanoError::task_execution(msg))
        }
    }

    let wf = Workflow::bare()
        .register(Step::Start, BigErrTask)
        .add_exit_state(Step::Done);
    let running = Scheduler::new()
        .manual("big-err", wf, Step::Start)
        .with_backoff("big-err", BackoffPolicy::with_trip(2))
        .start()
        .await
        .unwrap();

    running.trigger("big-err").await.unwrap();
    let info1 = running.wait_idle("big-err").await.unwrap();
    let Status::Backoff { last_error, .. } = info1.status else {
        panic!("expected Backoff, got {:?}", info1.status);
    };
    assert!(last_error.len() <= 4096);
    assert!(last_error.ends_with("..."));

    running.trigger("big-err").await.unwrap();
    let info2 = running.wait_idle("big-err").await.unwrap();
    let Status::Tripped { last_error, .. } = info2.status else {
        panic!("expected Tripped, got {:?}", info2.status);
    };
    assert!(last_error.len() <= 4096);
    assert!(last_error.ends_with("..."));

    running.stop().await.unwrap();
}
