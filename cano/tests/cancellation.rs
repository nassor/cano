//! Integration tests for cooperative cancellation (QoL-2).
//!
//! Covers: mid-task cancel, cancel-before-start, saga drain on cancel, dirty rollback,
//! saga-safety (compensatable tasks run to completion), split child abort, exactly-once
//! `on_cancelled`, idempotency, precedence over `with_total_timeout`, an uncancellable drain,
//! pass-through equivalence with `orchestrate`, and resume cancellation.

mod support;

use cano::CancellationToken;
use cano::prelude::*;
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Charge,
    Ship,
    Park,
    Done,
}

// ---- a recording observer (no `testing` feature dependency) ----
#[derive(Default)]
struct Rec {
    events: Mutex<Vec<String>>,
}
impl Rec {
    fn push(&self, s: String) {
        self.events.lock().unwrap().push(s);
    }
    fn snapshot(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
    fn count_prefix(&self, prefix: &str) -> usize {
        self.snapshot()
            .iter()
            .filter(|e| e.starts_with(prefix))
            .count()
    }
    fn has(&self, s: &str) -> bool {
        self.snapshot().iter().any(|e| e == s)
    }
}
impl WorkflowObserver for Rec {
    fn on_task_start(&self, task_id: &str) {
        self.push(format!("start:{task_id}"));
    }
    fn on_task_failure(&self, task_id: &str, _err: &CanoError) {
        self.push(format!("failure:{task_id}"));
    }
    fn on_cancelled(&self, state: &str) {
        self.push(format!("cancelled:{state}"));
    }
}

// ---- a long-running, non-compensatable task: records start/completion ----
struct LongTask {
    started: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    next: Step,
}
#[task(state = Step)]
impl LongTask {
    async fn run(&self) -> Result<Step, CanoError> {
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(10)).await;
        self.completed.store(true, Ordering::SeqCst);
        Ok(self.next.clone())
    }
}

// ---- a compensatable step with a per-instance name (so the compensator registry keys
// don't collide), optional pre-completion sleep, and an optionally-failing compensator ----
struct CompStep {
    name: &'static str,
    next: Step,
    log: Arc<Mutex<Vec<String>>>,
    started: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    run_sleep_ms: u64,
    comp_sleep_ms: u64,
    fail_comp: bool,
}
#[saga::task(state = Step)]
impl CompStep {
    type Output = ();
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(self.name)
    }
    async fn run(&self, _res: &Resources) -> Result<(Step, ()), CanoError> {
        self.started.store(true, Ordering::SeqCst);
        if self.run_sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.run_sleep_ms)).await;
        }
        self.log.lock().unwrap().push(format!("run:{}", self.name));
        self.completed.store(true, Ordering::SeqCst);
        Ok((self.next.clone(), ()))
    }
    async fn compensate(&self, _res: &Resources, _out: ()) -> Result<(), CanoError> {
        if self.comp_sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.comp_sleep_ms)).await;
        }
        self.log
            .lock()
            .unwrap()
            .push(format!("rollback:{}", self.name));
        if self.fail_comp {
            return Err(CanoError::task_execution(format!(
                "comp {} boom",
                self.name
            )));
        }
        Ok(())
    }
}

fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Cancel via `handle` as soon as `flag` flips true (deterministic: fire while the
/// target task is parked in its sleep).
fn cancel_when(flag: Arc<AtomicBool>, handle: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while !flag.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        handle.cancel();
    })
}

#[tokio::test]
async fn cancel_mid_long_running_task_returns_cancelled() {
    let started = flag();
    let completed = flag();
    let wf = Workflow::bare()
        .register(
            Step::Ship,
            LongTask {
                started: started.clone(),
                completed: completed.clone(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);

    let t0 = Instant::now();
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        !completed.load(Ordering::SeqCst),
        "task should not complete"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "should abort promptly"
    );
}

#[tokio::test]
async fn cancel_before_orchestrate_returns_immediately_without_running_any_task() {
    let started = flag();
    let completed = flag();
    let rec = Arc::new(Rec::default());
    let wf = Workflow::bare()
        .with_observer(rec.clone())
        .register(
            Step::Ship,
            LongTask {
                started: started.clone(),
                completed: completed.clone(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    handle.cancel(); // pre-cancel before running

    let result = wf.orchestrate_with_token(Step::Ship, &token).await;

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(!started.load(Ordering::SeqCst), "task must not start");
    assert_eq!(rec.count_prefix("start:"), 0, "no on_task_start fired");
    assert_eq!(rec.count_prefix("cancelled:"), 1, "on_cancelled fired once");
}

#[tokio::test]
async fn cancel_with_compensation_drains_stack_then_returns_cancelled() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let ship_started = flag();
    let ignore = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            CompStep {
                name: "reserve",
                next: Step::Charge,
                log: log.clone(),
                started: flag(),
                completed: flag(),
                run_sleep_ms: 0,
                comp_sleep_ms: 0,
                fail_comp: false,
            },
        )
        .register(
            Step::Charge,
            CompStep {
                name: "charge",
                next: Step::Ship,
                log: log.clone(),
                started: flag(),
                completed: flag(),
                run_sleep_ms: 0,
                comp_sleep_ms: 0,
                fail_comp: false,
            },
        )
        .register(
            Step::Ship,
            LongTask {
                started: ship_started.clone(),
                completed: ignore.clone(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(ship_started.clone(), handle);

    let result = wf.orchestrate_with_token(Step::Reserve, &token).await;
    canceller.await.unwrap();

    let err = result.unwrap_err();
    assert_eq!(err.category(), "cancelled");
    assert!(matches!(err.inner(), CanoError::Cancelled));
    // Both compensatable steps ran, then rolled back in reverse order.
    let events = log.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "run:reserve".to_string(),
            "run:charge".to_string(),
            "rollback:charge".to_string(),
            "rollback:reserve".to_string(),
        ]
    );
}

#[tokio::test]
async fn cancel_with_failing_compensator_surfaces_compensation_failed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let ship_started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            CompStep {
                name: "reserve",
                next: Step::Ship,
                log: log.clone(),
                started: flag(),
                completed: flag(),
                run_sleep_ms: 0,
                comp_sleep_ms: 0,
                fail_comp: true, // its rollback fails
            },
        )
        .register(
            Step::Ship,
            LongTask {
                started: ship_started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(ship_started.clone(), handle);

    let result = wf.orchestrate_with_token(Step::Reserve, &token).await;
    canceller.await.unwrap();

    match result.unwrap_err() {
        CanoError::CompensationFailed { errors } => {
            // errors[0] is the original (wrapped) cancellation.
            assert_eq!(errors[0].category(), "cancelled");
            assert!(errors.len() >= 2, "must also carry the compensator error");
        }
        other => panic!("expected CompensationFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn compensatable_task_not_interrupted_midflight() {
    // Cancel WHILE a CompensatableSingle is running. Saga safety requires it to run to
    // completion (so its rollback entry is recorded), with the cancel honoured at the next
    // boundary — draining that entry. The downstream Park task must never start.
    let log = Arc::new(Mutex::new(Vec::new()));
    let hold_started = flag();
    let hold_completed = flag();
    let park_started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            CompStep {
                name: "hold",
                next: Step::Park,
                log: log.clone(),
                started: hold_started.clone(),
                completed: hold_completed.clone(),
                run_sleep_ms: 150, // long enough to be cancelled mid-run
                comp_sleep_ms: 0,
                fail_comp: false,
            },
        )
        .register(
            Step::Park,
            LongTask {
                started: park_started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(hold_started.clone(), handle); // cancel during Hold's run

    let result = wf.orchestrate_with_token(Step::Reserve, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        hold_completed.load(Ordering::SeqCst),
        "compensatable task must run to completion, not be interrupted"
    );
    assert!(
        !park_started.load(Ordering::SeqCst),
        "downstream task must not start"
    );
    assert_eq!(
        log.lock().unwrap().clone(),
        vec!["run:hold".to_string(), "rollback:hold".to_string()]
    );
}

// A long-running split child.
struct SplitChild {
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
}
#[task(state = Step)]
impl SplitChild {
    async fn run(&self) -> Result<Step, CanoError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(10)).await;
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn cancel_mid_split_aborts_children_and_returns_cancelled() {
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let children: Vec<SplitChild> = (0..3)
        .map(|_| SplitChild {
            started: started.clone(),
            completed: completed.clone(),
        })
        .collect();
    // No `with_total_timeout` here on purpose: cancellation is the *only* abort path,
    // which is exactly the case where the synthetic per-branch `on_task_failure`
    // fan-out must still fire to keep observer gauges balanced.
    let rec = Arc::new(Rec::default());
    let wf = Workflow::bare()
        .with_observer(rec.clone())
        .register_split(
            Step::Ship,
            children,
            JoinConfig::new(JoinStrategy::All, Step::Done),
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    // Cancel only once all three children have started (so all three fired on_task_start).
    let started_probe = started.clone();
    let canceller = tokio::spawn(async move {
        while started_probe.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        handle.cancel();
    });

    let t0 = Instant::now();
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "split should abort promptly"
    );
    // Give any aborted children a moment; none should have completed.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "split children must be aborted, not completed"
    );
    // Observer gauge balance: every on_task_start must be paired with an on_task_failure,
    // even though no total-timeout budget was set (the cancel path is the only abort route).
    assert_eq!(rec.count_prefix("start:"), 3, "all branches started");
    assert_eq!(
        rec.count_prefix("failure:"),
        rec.count_prefix("start:"),
        "each started branch must get a paired on_task_failure on cancel (gauge balance)"
    );
}

#[tokio::test]
async fn on_cancelled_fires_exactly_once() {
    let started = flag();
    let rec = Arc::new(Rec::default());
    let wf = Workflow::bare()
        .with_observer(rec.clone())
        .register(
            Step::Ship,
            LongTask {
                started: started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert_eq!(
        rec.count_prefix("cancelled:"),
        1,
        "on_cancelled exactly once"
    );
    assert!(
        rec.has("cancelled:Ship"),
        "fired with the right state label"
    );
}

#[tokio::test]
async fn on_cancelled_does_not_fire_on_successful_run() {
    let rec = Arc::new(Rec::default());
    let log = Arc::new(Mutex::new(Vec::new()));
    let wf = Workflow::bare()
        .with_observer(rec.clone())
        .register(
            Step::Reserve,
            CompStep {
                name: "reserve",
                next: Step::Done,
                log,
                started: flag(),
                completed: flag(),
                run_sleep_ms: 0,
                comp_sleep_ms: 0,
                fail_comp: false,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new(); // armed but never fired
    let result = wf.orchestrate_with_token(Step::Reserve, &token).await;

    assert_eq!(result.unwrap(), Step::Done);
    assert_eq!(
        rec.count_prefix("cancelled:"),
        0,
        "on_cancelled must not fire on a successful run"
    );
}

#[tokio::test]
async fn uncancelled_token_behaves_like_orchestrate() {
    let build = || {
        Workflow::bare()
            .register(
                Step::Reserve,
                // A quick compensatable task (no sleep) that transitions straight to Done;
                // a successful run never triggers its compensator.
                CompStep {
                    name: "reserve",
                    next: Step::Done,
                    log: Arc::new(Mutex::new(Vec::new())),
                    started: flag(),
                    completed: flag(),
                    run_sleep_ms: 0,
                    comp_sleep_ms: 0,
                    fail_comp: false,
                },
            )
            .add_exit_state(Step::Done)
    };

    let plain = build().orchestrate(Step::Reserve).await;
    let token = CancellationToken::new(); // never cancelled
    let with_cancel = build().orchestrate_with_token(Step::Reserve, &token).await;

    assert_eq!(plain.unwrap(), Step::Done);
    assert_eq!(with_cancel.unwrap(), Step::Done);
}

#[tokio::test]
async fn double_cancel_is_idempotent() {
    let started = flag();
    let rec = Arc::new(Rec::default());
    let wf = Workflow::bare()
        .with_observer(rec.clone())
        .register(
            Step::Ship,
            LongTask {
                started: started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let probe = started.clone();
    let canceller = tokio::spawn(async move {
        while !probe.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        handle.cancel();
        handle.cancel(); // second cancel is a no-op
    });

    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert_eq!(
        rec.count_prefix("cancelled:"),
        1,
        "still exactly one on_cancelled"
    );
}

#[tokio::test]
async fn cancellation_precedence_over_total_timeout() {
    let started = flag();
    let wf = Workflow::bare()
        .with_total_timeout(Duration::from_secs(30)) // would not fire before the cancel
        .register(
            Step::Ship,
            LongTask {
                started: started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    let err = result.unwrap_err();
    assert_eq!(
        err.category(),
        "cancelled",
        "cancellation wins over the budget"
    );
}

#[tokio::test]
async fn compensation_drain_completes_fully_under_cancellation() {
    // The drain is uncancellable: both (slow) compensators run to completion even though the
    // token stays cancelled throughout the rollback.
    let log = Arc::new(Mutex::new(Vec::new()));
    let ship_started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            CompStep {
                name: "reserve",
                next: Step::Charge,
                log: log.clone(),
                started: flag(),
                completed: flag(),
                run_sleep_ms: 0,
                comp_sleep_ms: 40,
                fail_comp: false,
            },
        )
        .register(
            Step::Charge,
            CompStep {
                name: "charge",
                next: Step::Ship,
                log: log.clone(),
                started: flag(),
                completed: flag(),
                run_sleep_ms: 0,
                comp_sleep_ms: 40,
                fail_comp: false,
            },
        )
        .register(
            Step::Ship,
            LongTask {
                started: ship_started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(ship_started.clone(), handle);
    let result = wf.orchestrate_with_token(Step::Reserve, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    let events = log.lock().unwrap().clone();
    assert!(events.contains(&"rollback:charge".to_string()));
    assert!(events.contains(&"rollback:reserve".to_string()));
}

// A resource whose teardown is observable, to prove cleanup runs even on cancel.
struct TeardownProbe {
    tore_down: Arc<AtomicUsize>,
}
#[resource]
impl Resource for TeardownProbe {
    async fn teardown(&self) -> Result<(), CanoError> {
        self.tore_down.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn resources_are_torn_down_on_cancel() {
    let tore_down = Arc::new(AtomicUsize::new(0));
    let started = flag();
    let resources = Resources::new().insert(TeardownProbe {
        tore_down: tore_down.clone(),
    });
    let wf = Workflow::new(resources)
        .register(
            Step::Ship,
            LongTask {
                started: started.clone(),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert_eq!(
        tore_down.load(Ordering::SeqCst),
        1,
        "resource teardown must still run when a run is cancelled"
    );
}

// =====================================================================================
// Cancellation across every processing model. Each model is dispatched through the same
// `dispatch_with_budget` race, so each must be interruptible mid-flight and surface
// `Cancelled` promptly rather than running to completion.
// =====================================================================================

// RouterTask: cancel while a route lookup is in flight.
struct SlowRouter {
    started: Arc<AtomicBool>,
}
#[task::router(state = Step)]
impl SlowRouter {
    async fn route(&self, _res: &Resources) -> Result<Step, CanoError> {
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn cancel_during_router_task_returns_cancelled() {
    let started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Reserve,
            SlowRouter {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let t0 = Instant::now();
    let result = wf.orchestrate_with_token(Step::Reserve, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "router must abort promptly"
    );
}

// PollTask: cancel while the poll loop is parked between Pending polls.
struct ForeverPoll {
    started: Arc<AtomicBool>,
}
#[task::poll(state = Step)]
impl ForeverPoll {
    async fn poll(&self, _res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        self.started.store(true, Ordering::SeqCst);
        Ok(PollOutcome::Pending { delay_ms: 50 })
    }
}

#[tokio::test]
async fn cancel_during_poll_task_returns_cancelled() {
    let started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Ship,
            ForeverPoll {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let t0 = Instant::now();
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "poll loop must abort promptly"
    );
}

// TimerTask: cancel during the scheduled sleep.
struct SlowTimer {
    started: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
}
#[task::timer(state = Step)]
impl SlowTimer {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        self.started.store(true, Ordering::SeqCst);
        Ok(TimerOutcome::Duration(Duration::from_secs(10)))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<Step, CanoError> {
        self.fired.store(true, Ordering::SeqCst);
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn cancel_during_timer_task_returns_cancelled() {
    let started = flag();
    let fired = flag();
    let wf = Workflow::bare()
        .register(
            Step::Ship,
            SlowTimer {
                started: started.clone(),
                fired: fired.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let t0 = Instant::now();
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "timer must abort promptly"
    );
    assert!(
        !fired.load(Ordering::SeqCst),
        "after_wait must not run when the timer is cancelled mid-sleep"
    );
}

// BatchTask: cancel while items are being processed.
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
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(())
    }
    async fn finish(
        &self,
        _res: &Resources,
        _outputs: Vec<Result<(), CanoError>>,
    ) -> Result<Step, CanoError> {
        self.finished.store(true, Ordering::SeqCst);
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn cancel_during_batch_task_returns_cancelled() {
    let started = flag();
    let finished = flag();
    let wf = Workflow::bare()
        .register(
            Step::Ship,
            SlowBatch {
                started: started.clone(),
                finished: finished.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let t0 = Instant::now();
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "batch must abort promptly"
    );
    assert!(
        !finished.load(Ordering::SeqCst),
        "finish must not run when the batch is cancelled mid-processing"
    );
}

// SteppedTask: cancel mid-step (no checkpoint store ⇒ cursor is in-memory only).
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
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let n = cursor.unwrap_or(0);
        if n >= 10_000 {
            Ok(StepOutcome::Done(Step::Done))
        } else {
            Ok(StepOutcome::More(n + 1))
        }
    }
}

#[tokio::test]
async fn cancel_during_stepped_task_returns_cancelled() {
    let started = flag();
    let wf = Workflow::bare()
        .register(
            Step::Ship,
            SlowStepper {
                started: started.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let t0 = Instant::now();
    let result = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();

    assert_eq!(result.unwrap_err().category(), "cancelled");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "stepped loop must abort promptly"
    );
}

#[cfg(feature = "testing")]
#[tokio::test]
async fn resume_from_honors_precancelled_token() {
    use cano::testing::InMemoryCheckpointStore;

    let store = Arc::new(InMemoryCheckpointStore::new());
    let run_count = Arc::new(AtomicUsize::new(0));

    // Run 1: cancel mid-Ship so a checkpoint log is left behind (Ship's StateEntry row was
    // written before the task ran). Ship counts its runs.
    struct CountingLong {
        started: Arc<AtomicBool>,
        runs: Arc<AtomicUsize>,
    }
    #[task(state = Step)]
    impl CountingLong {
        async fn run(&self) -> Result<Step, CanoError> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            self.started.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(Step::Done)
        }
    }

    let started = flag();
    let wf = Workflow::bare()
        .with_checkpoint_store(store.clone())
        .with_workflow_id("run-1")
        .register(
            Step::Ship,
            CountingLong {
                started: started.clone(),
                runs: run_count.clone(),
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let handle = token.clone();
    let canceller = cancel_when(started.clone(), handle);
    let r1 = wf.orchestrate_with_token(Step::Ship, &token).await;
    canceller.await.unwrap();
    assert_eq!(r1.unwrap_err().category(), "cancelled");
    assert_eq!(run_count.load(Ordering::SeqCst), 1);

    // Run 2: resume with a pre-cancelled token — the resumed run must cancel at the Ship
    // boundary WITHOUT re-running the task.
    let token2 = CancellationToken::new();
    let handle2 = token2.clone();
    handle2.cancel();
    let r2 = wf.resume_from_with_token("run-1", &token2).await;
    assert_eq!(r2.unwrap_err().category(), "cancelled");
    assert_eq!(
        run_count.load(Ordering::SeqCst),
        1,
        "resumed task must not re-run when cancelled at the boundary"
    );
}

#[tokio::test]
async fn cancel_mid_suspend_and_mid_resume_balances_observer_hooks() {
    struct SlowSuspend {
        suspend_started: Arc<AtomicBool>,
        resume_started: Arc<AtomicBool>,
        hang_suspend: bool,
    }

    #[task::suspend(state = Step)]
    impl SlowSuspend {
        async fn suspend(&self) -> Result<(), CanoError> {
            self.suspend_started.store(true, Ordering::SeqCst);
            if self.hang_suspend {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Ok(())
        }

        async fn resume(&self, _input: String) -> Result<Step, CanoError> {
            self.resume_started.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(Step::Done)
        }
    }

    // 1. Cancel mid-`suspend()`.
    let store = Arc::new(support::MemStore::default());
    let rec = Arc::new(Rec::default());
    let suspend_started = flag();
    let wf_suspend = Workflow::bare()
        .with_observer(rec.clone())
        .with_checkpoint_store(store.clone())
        .with_workflow_id("cancel-suspend")
        .register(
            Step::Park,
            SlowSuspend {
                suspend_started: Arc::clone(&suspend_started),
                resume_started: flag(),
                hang_suspend: true,
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let canceller = cancel_when(Arc::clone(&suspend_started), token.clone());
    let err = wf_suspend
        .orchestrate_with_token(Step::Park, &token)
        .await
        .unwrap_err();
    canceller.await.unwrap();
    assert!(err.is_cancelled());
    assert_eq!(rec.count_prefix("start:"), 1);
    assert_eq!(rec.count_prefix("failure:"), 1);
    assert_eq!(rec.count_prefix("cancelled:"), 1);

    // 2. Park cleanly, then cancel mid-`resume()` via `resume_with_token`.
    let rec2 = Arc::new(Rec::default());
    let resume_started = flag();
    let wf_resume = Workflow::bare()
        .with_observer(rec2.clone())
        .with_checkpoint_store(store)
        .with_workflow_id("cancel-resume")
        .register(
            Step::Park,
            SlowSuspend {
                suspend_started: flag(),
                resume_started: Arc::clone(&resume_started),
                hang_suspend: false,
            },
        )
        .add_exit_state(Step::Done);

    assert_eq!(wf_resume.orchestrate(Step::Park).await.unwrap(), Step::Park);
    let token2 = CancellationToken::new();
    let canceller2 = cancel_when(Arc::clone(&resume_started), token2.clone());
    let err2 = wf_resume
        .resume_with_token("cancel-resume", &"go", &token2)
        .await
        .unwrap_err();
    canceller2.await.unwrap();
    assert!(err2.is_cancelled());
    // 1 start+success for park + 1 start+failure for cancelled resume => 2 starts, 1 failure.
    assert_eq!(rec2.count_prefix("start:"), 2);
    assert_eq!(rec2.count_prefix("failure:"), 1);
    assert_eq!(rec2.count_prefix("cancelled:"), 1);
}

#[tokio::test]
async fn cancel_mid_stream_task_flushes_window_and_balances_observer_hooks() {
    use futures_util::{StreamExt, stream};
    use std::pin::Pin;

    struct InfiniteStream {
        item_seen: Arc<AtomicBool>,
        flushed_windows: Arc<AtomicUsize>,
    }

    #[task::stream(state = Step)]
    impl InfiniteStream {
        fn window(&self) -> StreamWindow {
            StreamWindow::Count(4)
        }

        async fn open(
            &self,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn futures_util::Stream<Item = u64> + Send>>, CanoError> {
            let seen = Arc::clone(&self.item_seen);
            let s = stream::unfold(0u64, move |n| {
                let seen = Arc::clone(&seen);
                async move {
                    if n >= 1 {
                        seen.store(true, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                    Some((n + 1, n + 1))
                }
            });
            Ok(s.boxed())
        }

        async fn process_item(&self, item: u64) -> Result<(u64, u64), CanoError> {
            Ok((item, item))
        }

        async fn flush_window(&self, _outputs: Vec<u64>) -> Result<WindowSignal<Step>, CanoError> {
            self.flushed_windows.fetch_add(1, Ordering::SeqCst);
            Ok(WindowSignal::Continue)
        }

        async fn on_close(&self, _reason: CloseReason) -> Result<Step, CanoError> {
            Ok(Step::Done)
        }
    }

    let seen = flag();
    let flushed = Arc::new(AtomicUsize::new(0));
    let rec = Arc::new(Rec::default());
    let wf = Workflow::bare()
        .with_observer(rec.clone())
        .register(
            Step::Ship,
            InfiniteStream {
                item_seen: Arc::clone(&seen),
                flushed_windows: Arc::clone(&flushed),
            },
        )
        .add_exit_state(Step::Done);

    let token = CancellationToken::new();
    let canceller = cancel_when(seen, token.clone());
    let err = wf
        .orchestrate_with_token(Step::Ship, &token)
        .await
        .unwrap_err();
    canceller.await.unwrap();

    assert!(err.is_cancelled());
    assert_eq!(
        flushed.load(Ordering::SeqCst),
        1,
        "partial in-flight stream window must flush on cancel"
    );
    assert_eq!(rec.count_prefix("start:"), 1);
    assert_eq!(rec.count_prefix("failure:"), 1);
    assert_eq!(rec.count_prefix("cancelled:"), 1);
}

#[tokio::test]
async fn cancel_during_checkpoint_append_does_not_emit_unpaired_task_failure() {
    struct CancelOnAppendStore {
        inner: support::MemStore,
        token: CancellationToken,
    }

    #[cano::checkpoint_store]
    impl CheckpointStore for CancelOnAppendStore {
        async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
            let res = self.inner.append(workflow_id, row).await;
            // Cancel AFTER the top-of-loop `is_cancelled()` check has passed, right before
            // `dispatch_with_budget` runs!
            self.token.cancel();
            res
        }

        async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
            self.inner.load_run(workflow_id).await
        }

        async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
            self.inner.clear(workflow_id).await
        }
    }

    let token = CancellationToken::new();
    let store = Arc::new(CancelOnAppendStore {
        inner: support::MemStore::default(),
        token: token.clone(),
    });
    let rec = Arc::new(Rec::default());
    let started = flag();

    let wf = Workflow::bare()
        .with_observer(rec.clone())
        .with_checkpoint_store(store)
        .with_workflow_id("append-cancel")
        .register(
            Step::Ship,
            LongTask {
                started: Arc::clone(&started),
                completed: flag(),
                next: Step::Done,
            },
        )
        .add_exit_state(Step::Done);

    let err = wf
        .orchestrate_with_token(Step::Ship, &token)
        .await
        .unwrap_err();
    assert!(err.is_cancelled());
    assert!(!started.load(Ordering::SeqCst));
    assert_eq!(rec.count_prefix("start:"), 0);
    assert_eq!(
        rec.count_prefix("failure:"),
        0,
        "on_task_failure must NOT fire when on_task_start never ran"
    );
    assert_eq!(rec.count_prefix("cancelled:"), 1);
}
