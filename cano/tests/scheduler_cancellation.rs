#![cfg(feature = "scheduler")]
//! Scheduler cancellation across processing models and schedule types.
//!
//! The scheduler fires the engine's `CancellationToken` uniformly for every flow,
//! so `cancel_flow` / cancel-on-shutdown must work for *every* processing model
//! (base Task, saga, split, stepped, timer, poll, batch) and *every* schedule
//! type (manual, every, cron). These tests exercise that scheduler-specific wiring
//! (token publish/clear in `execute_reserved_flow`, the `Cancel` command, the
//! shutdown-cancel sweep, and `apply_outcome`'s cancel→Idle mapping) — the
//! orchestrate-level per-model cancellation lives in `tests/cancellation.rs`.

use cano::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Charge,
    Ship,
    Work,
    Done,
}

fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

/// Spin until `flag` is true (a flow's task has started). Panics after 15s so a
/// wiring slip or a cancellation regression fails fast instead of hanging (some
/// of these flows — poll/stepped — never terminate on their own).
async fn await_flag(flag: &Arc<AtomicBool>) {
    let t_start = Instant::now();
    while !flag.load(SeqCst) {
        assert!(
            t_start.elapsed() < Duration::from_secs(15),
            "flow task never started (wiring bug, or the flow errored before this state)"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Wait until the flow leaves `Status::Running` (its cancelled run settled).
/// Panics after 15s so a cancellation regression fails fast instead of hanging.
async fn await_not_running(running: &RunningScheduler, id: &str) {
    tokio::time::timeout(Duration::from_secs(15), running.wait_idle(id))
        .await
        .unwrap_or_else(|_| {
            panic!("flow '{id}' never left Running after cancel — cancellation regression")
        })
        .expect("flow is registered");
}

// ---- base long-running task (the cancellable building block) ----
#[derive(Clone)]
struct Long {
    started: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    next: Step,
}
#[task(state = Step)]
impl Long {
    async fn run(&self) -> Result<Step, CanoError> {
        self.started.store(true, SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        self.completed.store(true, SeqCst);
        Ok(self.next.clone())
    }
}

// ---- saga steps (distinct types ⇒ distinct compensator keys) ----
#[derive(Clone)]
struct Reserve {
    log: Arc<Mutex<Vec<String>>>,
    fail_comp: bool,
}
#[saga::task(state = Step)]
impl Reserve {
    type Output = ();
    async fn run(&self, _res: &Resources) -> Result<(Step, ()), CanoError> {
        self.log.lock().unwrap().push("run:reserve".into());
        Ok((Step::Charge, ()))
    }
    async fn compensate(&self, _res: &Resources, _o: ()) -> Result<(), CanoError> {
        self.log.lock().unwrap().push("rollback:reserve".into());
        if self.fail_comp {
            return Err(CanoError::task_execution("reserve rollback boom"));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Charge {
    log: Arc<Mutex<Vec<String>>>,
}
#[saga::task(state = Step)]
impl Charge {
    type Output = ();
    async fn run(&self, _res: &Resources) -> Result<(Step, ()), CanoError> {
        self.log.lock().unwrap().push("run:charge".into());
        Ok((Step::Ship, ()))
    }
    async fn compensate(&self, _res: &Resources, _o: ()) -> Result<(), CanoError> {
        self.log.lock().unwrap().push("rollback:charge".into());
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_drains_scheduled_saga_and_returns_to_idle() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let ship_started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            Reserve {
                log: log.clone(),
                fail_comp: false,
            },
        )
        .register(Step::Charge, Charge { log: log.clone() })
        .register(
            Step::Ship,
            Long {
                started: ship_started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("order", wf, Step::Reserve);
    let running = scheduler.start().await.unwrap();

    running.trigger("order").await.unwrap();
    await_flag(&ship_started).await;
    running.cancel_flow("order").unwrap();
    await_not_running(&running, "order").await;

    let info = running.status("order").unwrap();
    assert_eq!(info.status, Status::Idle, "clean cancel → Idle");
    assert_eq!(info.failure_streak, 0, "cancel is not a backoff failure");
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "run:reserve".to_string(),
            "run:charge".to_string(),
            "rollback:charge".to_string(),
            "rollback:reserve".to_string(),
        ],
        "saga must roll back in reverse on a scheduled cancel"
    );
    running.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_dirty_rollback_saga_parks_in_backoff() {
    // A cancel whose compensator FAILS surfaces as `compensation_failed`, which is
    // a genuine fault: the flow lands in Backoff (default policy never trips), NOT Idle.
    let log = Arc::new(Mutex::new(Vec::new()));
    let ship_started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            Reserve {
                log: log.clone(),
                fail_comp: true, // Reserve's rollback fails ⇒ dirty rollback
            },
        )
        // Reserve.run transitions to Charge, so Charge must be registered for the
        // chain to reach the long Ship step where the cancel lands.
        .register(Step::Charge, Charge { log: log.clone() })
        .register(
            Step::Ship,
            Long {
                started: ship_started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("order", wf, Step::Reserve);
    let running = scheduler.start().await.unwrap();

    running.trigger("order").await.unwrap();
    await_flag(&ship_started).await;
    running.cancel_flow("order").unwrap();
    await_not_running(&running, "order").await;

    let info = running.status("order").unwrap();
    assert!(
        matches!(info.status, Status::Backoff { .. }),
        "a dirty rollback is a failure → Backoff, got {:?}",
        info.status
    );
    assert_eq!(info.failure_streak, 1);
    running.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_stop_rolls_back_in_flight_saga() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let ship_started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            Reserve {
                log: log.clone(),
                fail_comp: false,
            },
        )
        .register(Step::Charge, Charge { log: log.clone() })
        .register(
            Step::Ship,
            Long {
                started: ship_started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    // A short drain: the 30s task outlives it, so the cancel phase must fire and
    // roll the saga back.
    let running = Scheduler::new()
        .manual("order", wf, Step::Reserve)
        .with_drain_timeout(Duration::from_millis(100))
        .start()
        .await
        .unwrap();

    running.trigger("order").await.unwrap();
    await_flag(&ship_started).await;

    let t_start = Instant::now();
    running.stop().await.expect("graceful stop succeeds");
    assert!(
        t_start.elapsed() < Duration::from_secs(5),
        "shutdown must cancel + drain after the drain timeout, not wait 30s"
    );
    let events = log.lock().unwrap().clone();
    assert!(events.contains(&"rollback:charge".to_string()));
    assert!(events.contains(&"rollback:reserve".to_string()));
}

// ---- split ----
#[derive(Clone)]
struct SplitChild {
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
}
#[task(state = Step)]
impl SplitChild {
    async fn run(&self) -> Result<Step, CanoError> {
        self.started.fetch_add(1, SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        self.completed.fetch_add(1, SeqCst);
        Ok(Step::Done)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_cancels_in_flight_split_flow() {
    let started = counter();
    let completed = counter();
    let children: Vec<SplitChild> = (0..3)
        .map(|_| SplitChild {
            started: started.clone(),
            completed: completed.clone(),
        })
        .collect();
    let wf = Workflow::bare()
        .register_split(
            Step::Work,
            children,
            JoinConfig::new(JoinStrategy::All, Step::Done),
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("split", wf, Step::Work);
    let running = scheduler.start().await.unwrap();

    running.trigger("split").await.unwrap();
    let t_start = Instant::now();
    while started.load(SeqCst) < 3 {
        assert!(
            t_start.elapsed() < Duration::from_secs(15),
            "split children never all started"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    running.cancel_flow("split").unwrap();
    await_not_running(&running, "split").await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        completed.load(SeqCst),
        0,
        "split children must be aborted, not completed"
    );
    assert_eq!(running.status("split").unwrap().status, Status::Idle);
    running.stop().await.unwrap();
}

// ---- stepped ----
#[derive(Clone)]
struct SlowStepper {
    started: Arc<AtomicBool>,
}
#[task::stepped(state = Step)]
impl SlowStepper {
    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, Step>, CanoError> {
        self.started.store(true, SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let n = cursor.unwrap_or(0);
        if n >= 100_000 {
            Ok(StepOutcome::Done(Step::Done))
        } else {
            Ok(StepOutcome::More(n + 1))
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_cancels_in_flight_stepped_flow() {
    let started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Work,
            SlowStepper {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("stepped", wf, Step::Work);
    let running = scheduler.start().await.unwrap();

    running.trigger("stepped").await.unwrap();
    await_flag(&started).await;
    let t_start = Instant::now();
    running.cancel_flow("stepped").unwrap();
    await_not_running(&running, "stepped").await;

    assert!(
        t_start.elapsed() < Duration::from_secs(5),
        "stepped loop aborts promptly"
    );
    assert_eq!(running.status("stepped").unwrap().status, Status::Idle);
    running.stop().await.unwrap();
}

// ---- timer ----
#[derive(Clone)]
struct SlowTimer {
    started: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
}
#[task::timer(state = Step)]
impl SlowTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        self.started.store(true, SeqCst);
        Ok(TimerOutcome::Duration(Duration::from_secs(30)))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<Step, CanoError> {
        self.fired.store(true, SeqCst);
        Ok(Step::Done)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_cancels_in_flight_timer_flow() {
    let started = flag();
    let fired = flag();
    let wf = Workflow::bare()
        .register(
            Step::Work,
            SlowTimer {
                started: started.clone(),
                fired: fired.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("timer", wf, Step::Work);
    let running = scheduler.start().await.unwrap();

    running.trigger("timer").await.unwrap();
    await_flag(&started).await;
    running.cancel_flow("timer").unwrap();
    await_not_running(&running, "timer").await;

    assert!(
        !fired.load(SeqCst),
        "after_wait must not run when the timer is cancelled"
    );
    assert_eq!(running.status("timer").unwrap().status, Status::Idle);
    running.stop().await.unwrap();
}

// ---- poll ----
#[derive(Clone)]
struct ForeverPoll {
    started: Arc<AtomicBool>,
}
#[task::poll(state = Step)]
impl ForeverPoll {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        self.started.store(true, SeqCst);
        Ok(PollOutcome::Pending { delay_ms: 50 })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_cancels_in_flight_poll_flow() {
    let started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Work,
            ForeverPoll {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("poll", wf, Step::Work);
    let running = scheduler.start().await.unwrap();

    running.trigger("poll").await.unwrap();
    await_flag(&started).await;
    let t_start = Instant::now();
    running.cancel_flow("poll").unwrap();
    await_not_running(&running, "poll").await;

    assert!(
        t_start.elapsed() < Duration::from_secs(5),
        "poll loop aborts promptly"
    );
    assert_eq!(running.status("poll").unwrap().status, Status::Idle);
    running.stop().await.unwrap();
}

// ---- batch ----
#[derive(Clone)]
struct SlowBatch {
    started: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}
#[task::batch(state = Step)]
impl SlowBatch {
    type Item = u32;
    type ItemOutput = ();
    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok(vec![0, 1, 2])
    }
    async fn process_item(&self, _item: &u32) -> Result<(), CanoError> {
        self.started.store(true, SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    }
    async fn finish(
        &self,
        _res: &Resources,
        _outputs: Vec<Result<(), CanoError>>,
    ) -> Result<Step, CanoError> {
        self.finished.store(true, SeqCst);
        Ok(Step::Done)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_cancels_in_flight_batch_flow() {
    let started = flag();
    let finished = flag();
    let wf = Workflow::bare()
        .register(
            Step::Work,
            SlowBatch {
                started: started.clone(),
                finished: finished.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("batch", wf, Step::Work);
    let running = scheduler.start().await.unwrap();

    running.trigger("batch").await.unwrap();
    await_flag(&started).await;
    let t_start = Instant::now();
    running.cancel_flow("batch").unwrap();
    await_not_running(&running, "batch").await;

    assert!(
        t_start.elapsed() < Duration::from_secs(5),
        "batch aborts promptly"
    );
    assert!(
        !finished.load(SeqCst),
        "finish must not run when the batch is cancelled"
    );
    assert_eq!(running.status("batch").unwrap().status, Status::Idle);
    running.stop().await.unwrap();
}

// ---- schedule types: every / cron ----

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_on_every_flow_cancels_current_run_and_keeps_scheduling() {
    // An interval flow: cancelling the in-flight run returns it to Idle and the
    // loop keeps firing — a deliberate cancel must not stop future scheduled runs.
    let started = counter();
    let wf = Workflow::bare()
        .register(
            Step::Work,
            CountingLong {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.every("ticker", wf, Step::Work, Duration::from_millis(80));
    let running = scheduler.start().await.unwrap();

    // First run is in flight.
    while started.load(SeqCst) < 1 {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    running.cancel_flow("ticker").unwrap();

    // The loop must dispatch a *second* run after the cancel returns it to Idle.
    let t_start = Instant::now();
    while started.load(SeqCst) < 2 {
        assert!(
            t_start.elapsed() < Duration::from_secs(5),
            "interval flow must keep scheduling after a cancel"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !matches!(
            running.status("ticker").unwrap().status,
            Status::Tripped { .. }
        ),
        "cancel must not trip an interval flow"
    );
    running.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_cancels_in_flight_cron_flow() {
    // Cover the cron loop path (`spawn_cron_loop`): a per-second cron flow whose
    // run is cancelled mid-flight returns to Idle.
    let started = counter();
    let wf = Workflow::bare()
        .register(
            Step::Work,
            CountingLong {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.cron("cronflow", wf, Step::Work, "* * * * * *");
    let running = scheduler.start().await.unwrap();

    // First per-second tick starts a run within ~1s.
    let t_start = Instant::now();
    while started.load(SeqCst) < 1 {
        assert!(
            t_start.elapsed() < Duration::from_secs(3),
            "cron tick should fire"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    running.cancel_flow("cronflow").unwrap();
    await_not_running(&running, "cronflow").await;
    assert_eq!(running.status("cronflow").unwrap().status, Status::Idle);
    running.stop().await.unwrap();
}

// A long task that counts how many runs have started — for interval/cron tests.
#[derive(Clone)]
struct CountingLong {
    started: Arc<AtomicUsize>,
}
#[task(state = Step)]
impl CountingLong {
    async fn run(&self) -> Result<Step, CanoError> {
        self.started.fetch_add(1, SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(Step::Done)
    }
}

// ---- stepped + checkpoint store (recovery/testing) ----
#[cfg(feature = "testing")]
#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_cancels_checkpointed_stepped_flow() {
    use cano::testing::InMemoryCheckpointStore;

    let started = flag();
    let store = Arc::new(InMemoryCheckpointStore::new());
    let wf = Workflow::bare()
        .with_checkpoint_store(store.clone())
        .with_workflow_id("stepped-ckpt")
        .register(
            Step::Work,
            SlowStepper {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let scheduler = Scheduler::new();
    let scheduler = scheduler.manual("stepped", wf, Step::Work);
    let running = scheduler.start().await.unwrap();

    running.trigger("stepped").await.unwrap();
    await_flag(&started).await;
    running.cancel_flow("stepped").unwrap();
    await_not_running(&running, "stepped").await;

    assert_eq!(running.status("stepped").unwrap().status, Status::Idle);
    running.stop().await.unwrap();
}

#[cfg(feature = "testing")]
#[tokio::test(flavor = "multi_thread")]
async fn cancel_flow_checkpointed_stepped_flow_resumes_from_cursor_and_clears_log() {
    use cano::testing::InMemoryCheckpointStore;

    #[derive(Clone)]
    struct ResumableStepper {
        seen: Arc<Mutex<Vec<Option<u32>>>>,
        park_on_step1: Arc<AtomicBool>,
        step1_parked: Arc<AtomicBool>,
    }

    #[task::stepped(state = Step)]
    impl ResumableStepper {
        async fn step(
            &self,
            _res: &Resources,
            cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, Step>, CanoError> {
            self.seen.lock().unwrap().push(cursor);
            if cursor == Some(0) && self.park_on_step1.load(SeqCst) {
                self.step1_parked.store(true, SeqCst);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            match cursor {
                None => Ok(StepOutcome::More(0)),
                Some(0) => Ok(StepOutcome::More(1)),
                Some(_) => Ok(StepOutcome::Done(Step::Done)),
            }
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let park_on_step1 = Arc::new(AtomicBool::new(true));
    let step1_parked = flag();
    let store = Arc::new(InMemoryCheckpointStore::new());
    let wf = Workflow::bare()
        .with_checkpoint_store(store.clone())
        .with_workflow_id("stepped-resume-ckpt")
        .register(
            Step::Work,
            ResumableStepper {
                seen: seen.clone(),
                park_on_step1: park_on_step1.clone(),
                step1_parked: step1_parked.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let running = Scheduler::new()
        .manual("stepped", wf, Step::Work)
        .start()
        .await
        .unwrap();

    // Run 1: persists cursor 0, parks inside cursor Some(0), then gets cancelled.
    running.trigger("stepped").await.unwrap();
    await_flag(&step1_parked).await;
    running.cancel_flow("stepped").unwrap();
    await_not_running(&running, "stepped").await;
    assert_eq!(running.status("stepped").unwrap().status, Status::Idle);
    assert!(
        !store.rows("stepped-resume-ckpt").is_empty(),
        "cancelled stepped run retains checkpoint log for resume"
    );

    // Run 2: re-trigger auto-resumes from persisted cursor 0 (does not re-run None) and clears log.
    park_on_step1.store(false, SeqCst);
    running.trigger("stepped").await.unwrap();
    await_not_running(&running, "stepped").await;
    assert_eq!(running.status("stepped").unwrap().status, Status::Completed);
    assert!(
        store.rows("stepped-resume-ckpt").is_empty(),
        "completed resumed run clears checkpoint log"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![None, Some(0), Some(0), Some(1)],
        "second run must resume from cursor 0 instead of restarting at None"
    );

    // Run 3: starts fresh from cursor None.
    running.trigger("stepped").await.unwrap();
    await_not_running(&running, "stepped").await;
    assert_eq!(running.status("stepped").unwrap().status, Status::Completed);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![None, Some(0), Some(0), Some(1), None, Some(0), Some(1)]
    );

    running.stop().await.unwrap();
}
