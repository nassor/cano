//! Reliability contracts of [`SuspendTask`] composed with sagas, stepped loops,
//! concurrent `resume_with` races, and `with_total_timeout`.

mod support;

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use cano::prelude::*;
use parking_lot::Mutex;
use support::MemStore;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LoopState {
    Crunch,
    Review,
    Done,
}

struct CrunchSteps {
    observed_initial_cursors: Arc<Mutex<Vec<Option<u64>>>>,
    fail_once_on_second_pass: Arc<AtomicBool>,
    pass_entries: Arc<AtomicUsize>,
}

#[task::stepped(state = LoopState)]
impl CrunchSteps {
    async fn step(&self, cursor: Option<u64>) -> Result<StepOutcome<u64, LoopState>, CanoError> {
        if cursor.is_none() {
            self.pass_entries.fetch_add(1, Ordering::SeqCst);
        }
        self.observed_initial_cursors.lock().push(cursor);
        let next = cursor.unwrap_or(0) + 1;
        if next == 2
            && self.pass_entries.load(Ordering::SeqCst) == 2
            && self.fail_once_on_second_pass.swap(false, Ordering::SeqCst)
        {
            return Err(CanoError::task_execution("transient crash on pass 2"));
        }
        if next < 3 {
            Ok(StepOutcome::More(next))
        } else {
            Ok(StepOutcome::Done(LoopState::Review))
        }
    }
}

struct ReviewerGate;

#[task::suspend(state = LoopState)]
impl ReviewerGate {
    async fn suspend(&self) -> Result<(), CanoError> {
        Ok(())
    }

    async fn resume(&self, approved: bool) -> Result<LoopState, CanoError> {
        if approved {
            Ok(LoopState::Done)
        } else {
            Ok(LoopState::Crunch)
        }
    }
}

#[tokio::test]
async fn stepped_cursor_does_not_leak_across_suspend_loop_reentry_or_crash() {
    let store = Arc::new(MemStore::default());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let fail_second_pass = Arc::new(AtomicBool::new(true));
    let pass_entries = Arc::new(AtomicUsize::new(0));

    let wf = Workflow::bare()
        .with_checkpoint_store(store.clone())
        .with_workflow_id("loop-1")
        .register(
            LoopState::Crunch,
            CrunchSteps {
                observed_initial_cursors: Arc::clone(&observed),
                fail_once_on_second_pass: Arc::clone(&fail_second_pass),
                pass_entries: Arc::clone(&pass_entries),
            },
        )
        .register(LoopState::Review, ReviewerGate)
        .add_exit_state(LoopState::Done);

    // Pass 1: Crunch runs steps None -> Some(1) -> Some(2) -> Review (parks).
    assert_eq!(
        wf.orchestrate(LoopState::Crunch).await.unwrap(),
        LoopState::Review
    );
    assert_eq!(*observed.lock(), vec![None, Some(1), Some(2)]);

    // Reviewer rejects (`false`): transitions Review -> Crunch (Pass 2).
    // Pass 2 MUST start from `None` (not Pass 1's `Some(2)`), writes `StepCursor(1)`,
    // then crashes on step 2.
    observed.lock().clear();
    let err = wf
        .resume_with("loop-1", &false)
        .await
        .expect_err("simulated crash on pass 2");
    assert!(
        err.to_string().contains("transient crash on pass 2"),
        "{err}"
    );
    assert_eq!(*observed.lock(), vec![None, Some(1)]);

    // Crash recovery via `resume_from` picks up Pass 2's cursor `Some(1)` (still NOT
    // wiped, because Crunch was the active crashed state) and parks at Review again!
    observed.lock().clear();
    assert_eq!(wf.resume_from("loop-1").await.unwrap(), LoopState::Review);
    assert_eq!(*observed.lock(), vec![Some(1), Some(2)]);

    // Reviewer approves (`true`): transitions Review -> Done and clears checkpoint log.
    assert_eq!(
        wf.resume_with("loop-1", &true).await.unwrap(),
        LoopState::Done
    );
    assert!(store.rows("loop-1").is_empty());
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OrderState {
    Reserve,
    Approve,
    Ship,
    Done,
}

struct ReserveStock {
    events: Arc<Mutex<Vec<String>>>,
}

#[saga::task(state = OrderState)]
impl ReserveStock {
    type Output = String;

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("reserve_stock")
    }

    async fn run(&self, _res: &Resources) -> Result<(OrderState, String), CanoError> {
        self.events.lock().push("reserve:run".into());
        Ok((OrderState::Approve, "hold-42".into()))
    }

    async fn compensate(&self, _res: &Resources, hold_id: String) -> Result<(), CanoError> {
        self.events.lock().push(format!("reserve:undo:{hold_id}"));
        Ok(())
    }
}

struct ApprovalSuspend {
    hang_on_suspend: bool,
    hang_on_resume: bool,
}

#[task::suspend(state = OrderState)]
impl ApprovalSuspend {
    async fn suspend(&self) -> Result<(), CanoError> {
        if self.hang_on_suspend {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        Ok(())
    }

    async fn resume(&self, decision: String) -> Result<OrderState, CanoError> {
        if self.hang_on_resume {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        if decision == "reject" {
            return Err(CanoError::task_execution("order rejected by approver"));
        }
        Ok(OrderState::Ship)
    }
}

struct ShipOrder {
    winners: Arc<Mutex<Vec<usize>>>,
}

#[task(state = OrderState)]
impl ShipOrder {
    async fn run(&self) -> Result<OrderState, CanoError> {
        self.winners.lock().push(1);
        Ok(OrderState::Done)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_resume_with_allows_one_winner_without_false_saga_rollback() {
    let store = Arc::new(MemStore::default());
    let events = Arc::new(Mutex::new(Vec::new()));
    let winners = Arc::new(Mutex::new(Vec::new()));

    let wf = Arc::new(
        Workflow::bare()
            .with_checkpoint_store(store.clone())
            .with_workflow_id("race-order")
            .register(
                OrderState::Reserve,
                ReserveStock {
                    events: Arc::clone(&events),
                },
            )
            .register(
                OrderState::Approve,
                ApprovalSuspend {
                    hang_on_suspend: false,
                    hang_on_resume: false,
                },
            )
            .register(
                OrderState::Ship,
                ShipOrder {
                    winners: Arc::clone(&winners),
                },
            )
            .add_exit_state(OrderState::Done),
    );

    assert_eq!(
        wf.orchestrate(OrderState::Reserve).await.unwrap(),
        OrderState::Approve
    );

    let mut handles = Vec::new();
    for i in 0..8usize {
        let wf = Arc::clone(&wf);
        handles.push(tokio::spawn(async move {
            wf.resume_with("race-order", &format!("approve-{i}")).await
        }));
    }

    let mut ok_count = 0;
    let mut err_count = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(state) => {
                assert_eq!(state, OrderState::Done);
                ok_count += 1;
            }
            Err(e) => {
                // Losers fail either on duplicate ResumeInput sequence or because the
                // winner already advanced / cleared the log.
                assert!(
                    matches!(e.category(), "checkpoint_store" | "workflow"),
                    "unexpected loser error: {e:?}"
                );
                err_count += 1;
            }
        }
    }

    assert_eq!(ok_count, 1, "exactly one concurrent resume_with must win");
    assert_eq!(err_count, 7);
    assert_eq!(winners.lock().len(), 1, "Ship must execute once");
    assert_eq!(
        *events.lock(),
        vec!["reserve:run".to_string()],
        "losing resume_with racers must NEVER compensate the winner's saga"
    );
}

#[tokio::test]
async fn suspend_resume_error_compensates_pre_park_saga_and_clears_log() {
    let store = Arc::new(MemStore::default());
    let events = Arc::new(Mutex::new(Vec::new()));

    let wf = Workflow::bare()
        .with_checkpoint_store(store.clone())
        .with_workflow_id("reject-order")
        .register(
            OrderState::Reserve,
            ReserveStock {
                events: Arc::clone(&events),
            },
        )
        .register(
            OrderState::Approve,
            ApprovalSuspend {
                hang_on_suspend: false,
                hang_on_resume: false,
            },
        )
        .register(
            OrderState::Ship,
            ShipOrder {
                winners: Arc::new(Mutex::new(Vec::new())),
            },
        )
        .add_exit_state(OrderState::Done);

    assert_eq!(
        wf.orchestrate(OrderState::Reserve).await.unwrap(),
        OrderState::Approve
    );
    assert_eq!(*events.lock(), vec!["reserve:run".to_string()]);

    let err = wf
        .resume_with("reject-order", &"reject")
        .await
        .expect_err("rejected approval must fail");
    assert!(
        err.to_string().contains("order rejected by approver"),
        "{err}"
    );
    assert_eq!(
        *events.lock(),
        vec![
            "reserve:run".to_string(),
            "reserve:undo:hold-42".to_string()
        ]
    );
    assert!(store.rows("reject-order").is_empty());
}

#[tokio::test]
async fn suspend_and_resume_honor_with_total_timeout_and_drain_sagas() {
    let store = Arc::new(MemStore::default());
    let events = Arc::new(Mutex::new(Vec::new()));

    // 1. Timeout during `suspend()` rolls back pre-park saga.
    let wf_hang_suspend = Workflow::bare()
        .with_total_timeout(Duration::from_millis(25))
        .with_checkpoint_store(store.clone())
        .with_workflow_id("timeout-suspend")
        .register(
            OrderState::Reserve,
            ReserveStock {
                events: Arc::clone(&events),
            },
        )
        .register(
            OrderState::Approve,
            ApprovalSuspend {
                hang_on_suspend: true,
                hang_on_resume: false,
            },
        )
        .add_exit_state(OrderState::Done);

    let err = wf_hang_suspend
        .orchestrate(OrderState::Reserve)
        .await
        .unwrap_err();
    assert!(
        matches!(err.inner(), CanoError::WorkflowTimeout { .. }),
        "{err:?}"
    );
    assert_eq!(
        *events.lock(),
        vec![
            "reserve:run".to_string(),
            "reserve:undo:hold-42".to_string()
        ]
    );

    // 2. Timeout during `resume()` rolls back pre-park saga.
    events.lock().clear();
    let wf_hang_resume = Workflow::bare()
        .with_total_timeout(Duration::from_millis(40))
        .with_checkpoint_store(store.clone())
        .with_workflow_id("timeout-resume")
        .register(
            OrderState::Reserve,
            ReserveStock {
                events: Arc::clone(&events),
            },
        )
        .register(
            OrderState::Approve,
            ApprovalSuspend {
                hang_on_suspend: false,
                hang_on_resume: true,
            },
        )
        .add_exit_state(OrderState::Done);

    assert_eq!(
        wf_hang_resume
            .orchestrate(OrderState::Reserve)
            .await
            .unwrap(),
        OrderState::Approve
    );
    let err = wf_hang_resume
        .resume_with("timeout-resume", &"ok")
        .await
        .unwrap_err();
    assert!(
        matches!(err.inner(), CanoError::WorkflowTimeout { .. }),
        "{err:?}"
    );
    assert_eq!(
        *events.lock(),
        vec![
            "reserve:run".to_string(),
            "reserve:undo:hold-42".to_string()
        ]
    );
}
