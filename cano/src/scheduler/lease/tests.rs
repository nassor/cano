//! Several scheduler instances over one store: exactly-once ticks, leases, restarts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use chrono::Utc;
use tokio::time::sleep;

use super::{FlowRecord, FlowState, Lease};
use crate::error::CanoError;
use crate::scheduler::test_support::TestState;
use crate::scheduler::{
    BackoffPolicy, CasOutcome, RunningScheduler, Scheduler, SchedulerStore, Status, VersionedRecord,
};
use crate::task;
use crate::task::Task;
use crate::testing::InMemorySchedulerStore;
use crate::workflow::Workflow;

/// A task that counts its runs, flags overlapping runs, and sleeps (so it is also a
/// cancellation point) before it succeeds or fails. Clones share the counters.
#[derive(Clone)]
struct Probe {
    runs: Arc<AtomicU32>,
    active: Arc<AtomicBool>,
    overlapped: Arc<AtomicBool>,
    duration: Duration,
    fail: bool,
}

impl Probe {
    fn new(duration: Duration) -> Self {
        Self {
            runs: Arc::new(AtomicU32::new(0)),
            active: Arc::new(AtomicBool::new(false)),
            overlapped: Arc::new(AtomicBool::new(false)),
            duration,
            fail: false,
        }
    }

    fn failing(duration: Duration) -> Self {
        Self {
            fail: true,
            ..Self::new(duration)
        }
    }

    fn runs(&self) -> u32 {
        self.runs.load(Ordering::SeqCst)
    }

    fn workflow(&self) -> Workflow<TestState> {
        Workflow::bare()
            .register(TestState::Start, self.clone())
            .add_exit_state(TestState::Complete)
            .add_exit_state(TestState::Error)
    }
}

#[task]
impl Task<TestState> for Probe {
    // One attempt per dispatch: the scheduler's policy is the only retry layer.
    fn config(&self) -> crate::task::TaskConfig {
        crate::task::TaskConfig::new()
    }

    async fn run(&self) -> Result<TestState, CanoError> {
        if self.active.swap(true, Ordering::SeqCst) {
            self.overlapped.store(true, Ordering::SeqCst);
        }
        self.runs.fetch_add(1, Ordering::SeqCst);
        sleep(self.duration).await;
        self.active.store(false, Ordering::SeqCst);
        if self.fail {
            Err(CanoError::task_execution("boom"))
        } else {
            Ok(TestState::Complete)
        }
    }
}

/// A store whose every call fails.
struct FailingStore;

#[crate::scheduler_store]
impl SchedulerStore for FailingStore {
    async fn load(&self, _flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        Err(CanoError::scheduler_store("store is down"))
    }

    async fn compare_and_swap(
        &self,
        _flow_id: &str,
        _expected: Option<u64>,
        _value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        Err(CanoError::scheduler_store("store is down"))
    }

    async fn remove(&self, _flow_id: &str) -> Result<(), CanoError> {
        Err(CanoError::scheduler_store("store is down"))
    }
}

fn store() -> Arc<InMemorySchedulerStore> {
    Arc::new(InMemorySchedulerStore::new())
}

/// An instance of `Scheduler` named `id` on `store`, to be given flows.
fn instance(store: &Arc<InMemorySchedulerStore>, id: &str) -> Scheduler {
    Scheduler::new()
        .with_store(Arc::clone(store) as Arc<dyn SchedulerStore>)
        .with_instance_id(id)
}

fn record(store: &InMemorySchedulerStore, flow_id: &str) -> FlowRecord {
    let stored = store.record(flow_id).expect("record must exist");
    FlowRecord::decode(flow_id, &stored.value).unwrap()
}

/// Write `record` as the first record of `flow_id`.
async fn seed(store: &InMemorySchedulerStore, flow_id: &str, record: &FlowRecord) {
    let bytes = serde_json::to_vec(record).unwrap();
    let outcome = store.compare_and_swap(flow_id, None, bytes).await.unwrap();
    assert!(matches!(outcome, CasOutcome::Written(_)));
}

/// Poll `cond` every 10 ms; `true` as soon as it holds, `false` after `budget`.
async fn wait_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while !cond() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(10)).await;
    }
    true
}

async fn wait_for_status(
    running: &RunningScheduler,
    id: &str,
    budget: Duration,
    pred: impl Fn(&Status) -> bool,
) -> bool {
    wait_until(budget, || pred(&running.status(id).unwrap().status)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn two_instances_share_an_every_flow() {
    let store = store();
    let probe = Probe::new(Duration::from_millis(20));
    let interval = Duration::from_millis(100);
    let a = instance(&store, "a")
        .every("f", probe.workflow(), TestState::Start, interval)
        .start()
        .await
        .unwrap();
    let b = instance(&store, "b")
        .every("f", probe.workflow(), TestState::Start, interval)
        .start()
        .await
        .unwrap();

    sleep(Duration::from_secs(1)).await;
    a.stop().await.unwrap();
    b.stop().await.unwrap();

    let runs = probe.runs();
    assert!(
        !probe.overlapped.load(Ordering::SeqCst),
        "two instances ran the flow at once"
    );
    assert_eq!(
        u64::from(runs),
        record(&store, "f").run_count,
        "every claimed run executed exactly once"
    );
    // One run per interval across both instances (two per interval would be ~20).
    assert!((6..=12).contains(&runs), "runs: {runs}");
    assert!(
        record(&store, "f").lease.is_none(),
        "lease released on stop"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_longer_than_the_interval_is_never_joined_by_another_instance() {
    let store = store();
    // Always due again by the time it ends, so the idle instance keeps trying.
    let probe = Probe::new(Duration::from_millis(150));
    let interval = Duration::from_millis(100);
    let a = instance(&store, "a")
        .every("f", probe.workflow(), TestState::Start, interval)
        .start()
        .await
        .unwrap();
    let b = instance(&store, "b")
        .every("f", probe.workflow(), TestState::Start, interval)
        .start()
        .await
        .unwrap();

    sleep(Duration::from_millis(1200)).await;
    a.stop().await.unwrap();
    b.stop().await.unwrap();

    assert!(
        !probe.overlapped.load(Ordering::SeqCst),
        "the lease must keep the second instance out while a run is in flight"
    );
    let runs = probe.runs();
    assert_eq!(u64::from(runs), record(&store, "f").run_count);
    assert!((4..=9).contains(&runs), "runs: {runs}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_instances_run_each_cron_firing_once() {
    let store = store();
    let probe = Probe::new(Duration::from_millis(20));
    let started = Utc::now();
    let a = instance(&store, "a")
        .cron("f", probe.workflow(), TestState::Start, "*/1 * * * * *")
        .start()
        .await
        .unwrap();
    let b = instance(&store, "b")
        .cron("f", probe.workflow(), TestState::Start, "*/1 * * * * *")
        .start()
        .await
        .unwrap();

    sleep(Duration::from_millis(3100)).await;
    a.stop().await.unwrap();
    b.stop().await.unwrap();

    let runs = probe.runs();
    let rec = record(&store, "f");
    assert!(!probe.overlapped.load(Ordering::SeqCst));
    assert_eq!(u64::from(runs), rec.run_count);
    // Three firings fit in the window; one run per firing, not one per instance.
    assert!((2..=4).contains(&runs), "runs: {runs}");
    assert!(
        rec.last_tick.is_some_and(|tick| tick > started),
        "the claimed firing is recorded: {:?}",
        rec.last_tick
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_is_refused_while_another_instance_runs_the_flow() {
    let store = store();
    let probe = Probe::new(Duration::from_secs(3));
    let ttl = Duration::from_millis(300);
    let a = instance(&store, "inst-a")
        .manual("f", probe.workflow(), TestState::Start)
        .with_lease_ttl(ttl)
        .start()
        .await
        .unwrap();
    let b = instance(&store, "inst-b")
        .manual("f", probe.workflow(), TestState::Start)
        .with_lease_ttl(ttl)
        .start()
        .await
        .unwrap();
    assert_eq!(a.instance_id(), "inst-a");

    a.trigger("f").await.unwrap();
    // Past two TTLs: only a renewed lease still blocks b.
    sleep(ttl * 2 + Duration::from_millis(100)).await;

    let err = b.trigger("f").await.expect_err("a holds the lease");
    assert_eq!(err.category(), "workflow");
    assert!(err.to_string().contains("inst-a"), "{err}");
    let info = b.status("f").unwrap();
    assert!(
        matches!(&info.status, Status::RunningElsewhere { owner, .. } if &**owner == "inst-a"),
        "{:?}",
        info.status
    );
    assert_eq!(probe.runs(), 1);

    a.cancel_flow("f").unwrap();
    let done = a.wait_idle("f").await.unwrap();
    assert_eq!(done.status, Status::Idle);
    assert!(record(&store, "f").lease.is_none());
    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_lease_cancels_the_run_and_keeps_the_new_owners_record() {
    let store = store();
    let probe = Probe::new(Duration::from_secs(5));
    let a = instance(&store, "a")
        .manual("f", probe.workflow(), TestState::Start)
        .with_lease_ttl(Duration::from_millis(300))
        .start()
        .await
        .unwrap();
    a.trigger("f").await.unwrap();
    assert!(wait_until(Duration::from_secs(2), || probe.runs() == 1).await);

    // Another instance takes the record over (e.g. after a long stall of `a`).
    let mut stolen = None;
    for _ in 0..5 {
        let current = store.record("f").unwrap();
        let mut rec = FlowRecord::decode("f", &current.value).unwrap();
        assert_eq!(rec.lease.as_ref().unwrap().owner, "a");
        rec.lease = Some(Lease {
            owner: "thief".into(),
            expires_at: Utc::now() + chrono::Duration::seconds(60),
        });
        rec.written_by = "thief".into();
        let bytes = serde_json::to_vec(&rec).unwrap();
        // `a` renews every 100 ms and may win the race for this version; retry.
        if let CasOutcome::Written(_) = store
            .compare_and_swap("f", Some(current.version), bytes.clone())
            .await
            .unwrap()
        {
            stolen = Some(bytes);
            break;
        }
    }
    let stolen = stolen.expect("the swap must land within a few tries");

    let info = tokio::time::timeout(Duration::from_secs(3), a.wait_idle("f"))
        .await
        .expect("the run must end once its lease is lost")
        .unwrap();
    assert_ne!(info.status, Status::Running);
    assert!(
        matches!(&info.status, Status::RunningElsewhere { owner, .. } if &**owner == "thief"),
        "{:?}",
        info.status
    );
    assert_eq!(
        store.record("f").unwrap().value,
        stolen,
        "the run that lost its lease must not write its outcome"
    );
    a.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_foreign_lease_counts_as_a_failure() {
    let store = store();
    let now = Utc::now();
    seed(
        &store,
        "f",
        &FlowRecord {
            run_count: 1,
            last_run: Some(now - chrono::Duration::seconds(60)),
            lease: Some(Lease {
                owner: "ghost".into(),
                expires_at: now - chrono::Duration::seconds(10),
            }),
            written_by: "ghost".into(),
            written_at: now - chrono::Duration::seconds(60),
            ..FlowRecord::default()
        },
    )
    .await;

    let probe = Probe::new(Duration::from_millis(10));
    let running = instance(&store, "a")
        .every(
            "f",
            probe.workflow(),
            TestState::Start,
            Duration::from_millis(50),
        )
        .with_backoff("f", BackoffPolicy::with_trip(1))
        .start()
        .await
        .unwrap();

    assert!(
        wait_for_status(&running, "f", Duration::from_secs(2), |s| matches!(
            s,
            Status::Tripped { .. }
        ))
        .await,
        "{:?}",
        running.status("f")
    );
    sleep(Duration::from_millis(200)).await;
    assert_eq!(probe.runs(), 0, "a tripped flow must not run");
    let rec = record(&store, "f");
    assert_eq!(rec.state, FlowState::Tripped);
    assert_eq!(rec.failure_streak, 1);
    assert!(rec.lease.is_none());
    assert!(
        rec.last_error
            .as_deref()
            .is_some_and(|e| e.contains("lease expired") && e.contains("ghost")),
        "{:?}",
        rec.last_error
    );
    running.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tripped_flow_stays_tripped_across_restarts_until_reset() {
    let store = store();
    let failing = Probe::failing(Duration::from_millis(10));
    let first = instance(&store, "one")
        .every(
            "f",
            failing.workflow(),
            TestState::Start,
            Duration::from_millis(50),
        )
        .with_backoff("f", BackoffPolicy::with_trip(1))
        .start()
        .await
        .unwrap();
    assert!(
        wait_for_status(&first, "f", Duration::from_secs(3), |s| matches!(
            s,
            Status::Tripped { .. }
        ))
        .await
    );
    first.stop().await.unwrap();
    assert_eq!(record(&store, "f").state, FlowState::Tripped);
    assert_eq!(failing.runs(), 1);

    let probe = Probe::new(Duration::from_millis(10));
    let second = instance(&store, "two")
        .every(
            "f",
            probe.workflow(),
            TestState::Start,
            Duration::from_millis(50),
        )
        .with_backoff("f", BackoffPolicy::with_trip(1))
        .start()
        .await
        .unwrap();
    assert!(
        matches!(second.status("f").unwrap().status, Status::Tripped { .. }),
        "a restarted instance starts from the shared state"
    );
    sleep(Duration::from_millis(300)).await;
    assert_eq!(probe.runs(), 0, "a tripped flow must not dispatch");

    second.reset_flow("f").await.unwrap();
    let rec = record(&store, "f");
    assert_eq!(rec.failure_streak, 0);
    assert_eq!(rec.state, FlowState::Idle);
    assert!(rec.next_eligible.is_none());
    assert!(
        wait_until(Duration::from_secs(3), || probe.runs() >= 1).await,
        "the flow runs again after the reset"
    );
    second.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_does_not_rerun_an_every_flow_within_its_interval() {
    let store = store();
    let probe = Probe::new(Duration::from_millis(10));
    let interval = Duration::from_secs(1);
    let first = instance(&store, "one")
        .every("f", probe.workflow(), TestState::Start, interval)
        .start()
        .await
        .unwrap();
    assert!(wait_until(Duration::from_secs(2), || probe.runs() == 1).await);
    first.stop().await.unwrap();

    let second = instance(&store, "two")
        .every("f", probe.workflow(), TestState::Start, interval)
        .start()
        .await
        .unwrap();
    sleep(Duration::from_millis(400)).await;
    assert_eq!(probe.runs(), 1, "the interval has not elapsed");

    assert!(
        wait_until(Duration::from_secs(3), || probe.runs() == 2).await,
        "the flow is due again once the interval elapsed"
    );
    second.stop().await.unwrap();
    assert_eq!(record(&store, "f").run_count, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_store_skips_ticks_without_stopping_the_scheduler() {
    let probe = Probe::new(Duration::from_millis(10));
    let running = Scheduler::new()
        .every(
            "f",
            probe.workflow(),
            TestState::Start,
            Duration::from_millis(50),
        )
        .manual("m", probe.workflow(), TestState::Start)
        .with_store(Arc::new(FailingStore))
        .start()
        .await
        .expect("an unreachable store must not fail start");

    sleep(Duration::from_millis(300)).await;
    assert_eq!(probe.runs(), 0, "no claim, no run");
    assert_eq!(running.status("f").unwrap().status, Status::Idle);

    let err = running.trigger("m").await.expect_err("the claim fails");
    assert_eq!(err.category(), "scheduler_store");
    assert_eq!(
        running.wait_idle("m").await.unwrap().status,
        Status::Idle,
        "a failed claim must not strand the flow in Running"
    );
    let err = running.reset_flow("m").await.expect_err("the reset fails");
    assert_eq!(err.category(), "scheduler_store");

    running.stop().await.unwrap();
}

#[tokio::test]
async fn zero_ttl_and_empty_instance_id_are_configuration_errors() {
    let probe = Probe::new(Duration::from_millis(1));
    let err = instance(&store(), "a")
        .manual("f", probe.workflow(), TestState::Start)
        .with_lease_ttl(Duration::ZERO)
        .start()
        .await
        .expect_err("a zero TTL must be rejected");
    assert!(matches!(err, CanoError::Configuration(_)), "{err}");

    let err = instance(&store(), "a")
        .manual("f", probe.workflow(), TestState::Start)
        .with_instance_id("")
        .start()
        .await
        .expect_err("an empty instance id must be rejected");
    assert!(matches!(err, CanoError::Configuration(_)), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_run_is_recorded_in_the_shared_record() {
    let store = store();
    let probe = Probe::failing(Duration::from_millis(5));
    let running = instance(&store, "a")
        .manual("f", probe.workflow(), TestState::Start)
        .start()
        .await
        .unwrap();

    running.trigger("f").await.unwrap();
    let info = running.wait_idle("f").await.unwrap();
    assert!(
        matches!(&info.status, Status::Backoff { streak: 1, .. }),
        "{:?}",
        info.status
    );
    let rec = record(&store, "f");
    assert_eq!(rec.state, FlowState::Backoff);
    assert_eq!(rec.failure_streak, 1);
    assert_eq!(rec.run_count, 1);
    assert!(rec.next_eligible.is_some());
    assert!(rec.lease.is_none());
    assert!(rec.last_error.is_some());
    running.stop().await.unwrap();
}
