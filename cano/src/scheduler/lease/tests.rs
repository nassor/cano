//! Several scheduler instances over one store: exactly-once ticks, leases, restarts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use tokio::time::sleep;

use super::{Due, FlowRecord, FlowState, Lease};
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

/// An [`InMemorySchedulerStore`] with faults a test switches on and off while the
/// scheduler runs. Every check happens before the inner call, so a faulted call
/// never reaches the records.
#[derive(Default)]
struct FaultyStore {
    inner: InMemorySchedulerStore,
    /// Every call fails.
    down: AtomicBool,
    /// The next `n` calls fail.
    fail_next: AtomicU32,
    /// Every call hangs forever.
    stall: AtomicBool,
    /// Every call panics.
    panic: AtomicBool,
    /// Every swap reports a conflict without writing.
    conflict: AtomicBool,
    /// Every swap waits this many milliseconds first.
    swap_delay_ms: AtomicU64,
}

impl FaultyStore {
    async fn gate(&self) -> Result<(), CanoError> {
        assert!(!self.panic.load(Ordering::SeqCst), "store bug");
        if self.stall.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let consumed = self
            .fail_next
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if consumed || self.down.load(Ordering::SeqCst) {
            return Err(CanoError::scheduler_store("store is down"));
        }
        Ok(())
    }
}

#[crate::scheduler_store]
impl SchedulerStore for FaultyStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        self.gate().await?;
        self.inner.load(flow_id).await
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        self.gate().await?;
        let delay = self.swap_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            sleep(Duration::from_millis(delay)).await;
        }
        if self.conflict.load(Ordering::SeqCst) {
            return Ok(CasOutcome::Conflict);
        }
        self.inner.compare_and_swap(flow_id, expected, value).await
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        self.gate().await?;
        self.inner.remove(flow_id).await
    }
}

fn faulty() -> Arc<FaultyStore> {
    Arc::new(FaultyStore::default())
}

fn store() -> Arc<InMemorySchedulerStore> {
    Arc::new(InMemorySchedulerStore::new())
}

/// An instance of `Scheduler` named `id` on `store`, to be given flows.
fn instance(store: &Arc<InMemorySchedulerStore>, id: &str) -> Scheduler {
    on(Arc::clone(store) as Arc<dyn SchedulerStore>, id)
}

fn on(store: Arc<dyn SchedulerStore>, id: &str) -> Scheduler {
    Scheduler::new().with_store(store).with_instance_id(id)
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
    let store = faulty();
    store.down.store(true, Ordering::SeqCst);
    let running = Scheduler::new()
        .every(
            "f",
            probe.workflow(),
            TestState::Start,
            Duration::from_millis(50),
        )
        .manual("m", probe.workflow(), TestState::Start)
        .with_store(store)
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

/// A manual flow `f` on instance `id` of `store`, with lease `ttl`.
async fn manual_on(
    store: &Arc<FaultyStore>,
    id: &str,
    probe: &Probe,
    ttl: Duration,
) -> RunningScheduler {
    on(Arc::clone(store) as Arc<dyn SchedulerStore>, id)
        .manual("f", probe.workflow(), TestState::Start)
        .with_lease_ttl(ttl)
        .start()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_outage_longer_than_the_ttl_cancels_the_run_and_the_next_owner_counts_it() {
    let store = faulty();
    let ttl = Duration::from_millis(300);
    let probe = Probe::new(Duration::from_secs(5));
    let a = manual_on(&store, "a", &probe, ttl).await;
    a.trigger("f").await.unwrap();
    assert!(wait_until(Duration::from_secs(2), || probe.runs() == 1).await);

    store.down.store(true, Ordering::SeqCst);
    let info = tokio::time::timeout(Duration::from_secs(3), a.wait_idle("f"))
        .await
        .expect("renewals failing for a whole TTL must cancel the run")
        .unwrap();
    assert_eq!(info.status, Status::Idle);
    let rec = record(&store.inner, "f");
    assert_eq!(rec.lease.as_ref().map(|l| l.owner.as_str()), Some("a"));
    assert_eq!(
        rec.failure_streak, 0,
        "nothing is written while the store is down"
    );

    store.down.store(false, Ordering::SeqCst);
    let next = Probe::new(Duration::from_millis(500));
    let b = manual_on(&store, "b", &next, ttl).await;
    b.trigger("f").await.expect("the expired lease is free");
    let rec = record(&store.inner, "f");
    assert_eq!(rec.lease.as_ref().map(|l| l.owner.as_str()), Some("b"));
    assert_eq!(rec.failure_streak, 1, "the lost run counts as a failure");
    assert!(
        rec.last_error
            .as_deref()
            .is_some_and(|e| e.contains("lease expired") && e.contains("'a'")),
        "{:?}",
        rec.last_error
    );
    assert_eq!(b.wait_idle("f").await.unwrap().status, Status::Completed);
    assert_eq!(probe.runs(), 1);
    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hung_store_cannot_keep_a_flow_running_or_a_trigger_waiting() {
    let store = faulty();
    let ttl = Duration::from_millis(300);
    let probe = Probe::new(Duration::from_secs(5));
    let a = manual_on(&store, "a", &probe, ttl).await;
    a.trigger("f").await.unwrap();
    assert!(wait_until(Duration::from_secs(2), || probe.runs() == 1).await);

    store.stall.store(true, Ordering::SeqCst);
    let info = tokio::time::timeout(Duration::from_secs(3), a.wait_idle("f"))
        .await
        .expect("a store that never answers must not hold the run past its lease")
        .unwrap();
    assert_eq!(info.status, Status::Idle);

    let err = tokio::time::timeout(Duration::from_secs(3), a.trigger("f"))
        .await
        .expect("a claim against a hung store must end")
        .expect_err("the claim cannot succeed");
    assert_eq!(err.category(), "scheduler_store", "{err}");
    assert_ne!(a.status("f").unwrap().status, Status::Running);

    store.stall.store(false, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(3), a.stop())
        .await
        .expect("stop must not wait on the hung calls")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_retries_store_errors_and_writes_the_outcome() {
    let store = faulty();
    // Long enough that no renewal runs during the 100 ms run.
    let ttl = Duration::from_secs(10);
    let probe = Probe::new(Duration::from_millis(100));
    let a = manual_on(&store, "a", &probe, ttl).await;
    a.trigger("f").await.unwrap();
    store.fail_next.store(2, Ordering::SeqCst);

    let info = tokio::time::timeout(Duration::from_secs(3), a.wait_idle("f"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.status, Status::Completed);
    assert_eq!(store.fail_next.load(Ordering::SeqCst), 0);
    let rec = record(&store.inner, "f");
    assert_eq!(rec.state, FlowState::Completed);
    assert!(rec.lease.is_none());
    assert_eq!(rec.run_count, 1);
    a.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_that_gives_up_keeps_others_out_until_its_owner_claims_again() {
    let store = faulty();
    let ttl = Duration::from_secs(10);
    let probe = Probe::new(Duration::from_millis(50));
    let a = manual_on(&store, "a", &probe, ttl).await;
    let b = manual_on(&store, "b", &probe, ttl).await;
    a.trigger("f").await.unwrap();
    // The first attempt and every retry of the release fail.
    store.fail_next.store(4, Ordering::SeqCst);

    let info = tokio::time::timeout(Duration::from_secs(3), a.wait_idle("f"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.status, Status::Idle);
    assert_eq!(store.fail_next.load(Ordering::SeqCst), 0);
    let rec = record(&store.inner, "f");
    assert_eq!(
        rec.lease.as_ref().map(|l| l.owner.as_str()),
        Some("a"),
        "an unreleased lease stays until it expires"
    );

    let err = b.trigger("f").await.expect_err("a still holds the lease");
    assert!(err.to_string().contains("'a'"), "{err}");

    a.trigger("f")
        .await
        .expect("the owner takes its own stale lease back");
    let rec = record(&store.inner, "f");
    assert_eq!(rec.run_count, 2);
    assert_eq!(
        rec.failure_streak, 0,
        "a takeover by the owner is not a failure"
    );
    assert_eq!(a.wait_idle("f").await.unwrap().status, Status::Completed);
    assert_eq!(probe.runs(), 2);
    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reset_on_another_instance_does_not_cost_the_running_instance_its_lease() {
    let store = faulty();
    let now = Utc::now();
    let backoff = FlowRecord {
        failure_streak: 2,
        state: FlowState::Backoff,
        next_eligible: Some(now + chrono::Duration::seconds(60)),
        last_error: Some("earlier".into()),
        written_by: "seed".into(),
        written_at: now,
        ..FlowRecord::default()
    };
    let bytes = serde_json::to_vec(&backoff).unwrap();
    store
        .inner
        .compare_and_swap("f", None, bytes)
        .await
        .unwrap();

    // Renewals every 100 ms, so several of them meet the reset's new version.
    let ttl = Duration::from_millis(300);
    let probe = Probe::new(Duration::from_millis(800));
    let a = manual_on(&store, "a", &probe, ttl).await;
    let b = manual_on(&store, "b", &probe, ttl).await;
    a.trigger("f").await.expect("a trigger bypasses backoff");
    sleep(Duration::from_millis(50)).await;
    b.reset_flow("f").await.unwrap();

    let rec = record(&store.inner, "f");
    assert_eq!(rec.failure_streak, 0);
    assert_eq!(rec.lease.as_ref().map(|l| l.owner.as_str()), Some("a"));

    let info = tokio::time::timeout(Duration::from_secs(3), a.wait_idle("f"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        info.status,
        Status::Completed,
        "the run must finish, not be cancelled as a lost lease"
    );
    let rec = record(&store.inner, "f");
    assert_eq!(rec.state, FlowState::Completed);
    assert!(rec.lease.is_none());
    assert_eq!(probe.runs(), 1);
    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_triggers_on_several_instances_start_one_run() {
    let store = faulty();
    let probe = Probe::new(Duration::from_millis(300));
    let mut instances = Vec::new();
    for i in 0..5 {
        instances.push(manual_on(&store, &format!("i{i}"), &probe, Duration::from_secs(10)).await);
    }

    let results = futures_util::future::join_all(instances.iter().map(|s| s.trigger("f"))).await;
    let won: Vec<usize> = (0..results.len()).filter(|&i| results[i].is_ok()).collect();
    assert_eq!(won.len(), 1, "{results:?}");
    for error in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            error.to_string().contains("already running on instance"),
            "{error}"
        );
    }

    let winner = &instances[won[0]];
    assert_eq!(
        winner.wait_idle("f").await.unwrap().status,
        Status::Completed
    );
    assert_eq!(probe.runs(), 1);
    assert!(!probe.overlapped.load(Ordering::SeqCst));
    assert_eq!(record(&store.inner, "f").run_count, 1);
    for s in &instances {
        s.stop().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_store_fails_the_trigger_without_stranding_the_flow() {
    let store = faulty();
    let probe = Probe::new(Duration::from_millis(10));
    let a = manual_on(&store, "a", &probe, Duration::from_secs(10)).await;

    store.panic.store(true, Ordering::SeqCst);
    let err = a.trigger("f").await.expect_err("the claim panicked");
    assert_eq!(err.category(), "scheduler_store");
    assert!(err.to_string().contains("panicked"), "{err}");
    assert_eq!(a.status("f").unwrap().status, Status::Idle);

    store.panic.store(false, Ordering::SeqCst);
    a.trigger("f").await.expect("the flow is still usable");
    assert_eq!(a.wait_idle("f").await.unwrap().status, Status::Completed);
    a.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_undecodable_record_is_reported_and_never_overwritten() {
    let store = faulty();
    store
        .inner
        .compare_and_swap("f", None, b"not a record".to_vec())
        .await
        .unwrap();
    let probe = Probe::new(Duration::from_millis(10));
    let a = manual_on(&store, "a", &probe, Duration::from_secs(10)).await;

    let err = a.trigger("f").await.expect_err("the record is unreadable");
    assert_eq!(err.category(), "scheduler_store");
    assert!(err.to_string().contains("undecodable"), "{err}");
    let err = a
        .reset_flow("f")
        .await
        .expect_err("the record is unreadable");
    assert_eq!(err.category(), "scheduler_store");
    assert_eq!(probe.runs(), 0);
    assert_eq!(store.inner.record("f").unwrap().value, b"not a record");
    a.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn endless_conflicts_end_the_claim_with_an_error() {
    let store = faulty();
    store.conflict.store(true, Ordering::SeqCst);
    let probe = Probe::new(Duration::from_millis(10));
    let a = manual_on(&store, "a", &probe, Duration::from_secs(10)).await;

    let err = a.trigger("f").await.expect_err("every swap conflicts");
    assert_eq!(err.category(), "scheduler_store");
    assert!(err.to_string().contains("compare-and-swap races"), "{err}");
    assert_eq!(a.status("f").unwrap().status, Status::Idle);
    assert_eq!(probe.runs(), 0);
    a.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_during_a_claim_hands_the_lease_back() {
    let store = faulty();
    store.swap_delay_ms.store(300, Ordering::SeqCst);
    let probe = Probe::new(Duration::from_millis(10));
    let a = manual_on(&store, "a", &probe, Duration::from_secs(10)).await;

    let trigger = tokio::spawn({
        let a = a.clone();
        async move { a.trigger("f").await }
    });
    sleep(Duration::from_millis(100)).await;
    a.stop().await.unwrap();

    let err = trigger
        .await
        .unwrap()
        .expect_err("the scheduler stopped mid-claim");
    assert!(err.to_string().contains("stopping or stopped"), "{err}");
    assert_eq!(probe.runs(), 0);
    let rec = record(&store.inner, "f");
    assert!(rec.lease.is_none(), "the claimed lease is released");
    assert_eq!(rec.state, FlowState::Idle);
    assert_eq!(rec.failure_streak, 0, "an abandoned claim is not a failure");
    assert_eq!(a.status("f").unwrap().status, Status::Idle);
}

#[test]
fn a_backoff_window_holds_scheduled_ticks_but_not_a_trigger() {
    let now = Utc::now();
    let hour_ago = now - chrono::Duration::hours(1);
    let rec = FlowRecord {
        last_run: Some(hour_ago),
        last_tick: Some(hour_ago),
        next_eligible: Some(now + chrono::Duration::seconds(60)),
        ..FlowRecord::default()
    };
    assert!(!Due::Every(Duration::from_secs(1)).is_due(&rec, now));
    assert!(!Due::Cron(now).is_due(&rec, now));
    assert!(Due::Trigger.is_due(&rec, now));

    let after = now + chrono::Duration::seconds(61);
    assert!(Due::Every(Duration::from_secs(1)).is_due(&rec, after));
    assert!(Due::Cron(after).is_due(&rec, after));
}

#[test]
fn a_cron_firing_is_due_once_and_never_after_a_later_one() {
    let tick = Utc::now();
    let rec = FlowRecord {
        last_tick: Some(tick),
        ..FlowRecord::default()
    };
    let second = chrono::Duration::seconds(1);
    assert!(!Due::Cron(tick).is_due(&rec, tick), "the same firing twice");
    assert!(
        !Due::Cron(tick - second).is_due(&rec, tick),
        "an instance with a lagging clock must not run an older firing"
    );
    assert!(Due::Cron(tick + second).is_due(&rec, tick + second));
}

#[test]
fn records_written_by_other_versions_decode() {
    let rec = FlowRecord::decode(
        "f",
        br#"{"run_count":3,"state":"tripped","failure_streak":4,"added_later":{"x":1}}"#,
    )
    .expect("missing fields default and unknown fields are ignored");
    assert_eq!(rec.run_count, 3);
    assert_eq!(rec.state, FlowState::Tripped);
    assert_eq!(rec.failure_streak, 4);
    assert!(rec.lease.is_none());
}
