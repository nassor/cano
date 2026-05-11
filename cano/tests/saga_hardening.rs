//! Saga / compensation hardening: many compensatable workflows concurrently against a
//! shared ledger with failures injected at every possible step, plus crash-and-resume of
//! a compensatable run against a real redb file (the resume point must not compensate
//! itself twice). The `recovery` parts are gated behind that feature.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cano::prelude::*;

// -- shared, clone-shares-its-data ledger (the `MemoryStore` pattern) --

#[derive(Clone, Default)]
struct Ledger(Arc<Mutex<HashMap<String, i64>>>);
impl Resource for Ledger {}
impl Ledger {
    fn apply(&self, account: &str, delta: i64) {
        *self
            .0
            .lock()
            .unwrap()
            .entry(account.to_string())
            .or_insert(0) += delta;
    }
    fn balance(&self, account: &str) -> i64 {
        *self.0.lock().unwrap().get(account).unwrap_or(&0)
    }
    fn min_balance(&self) -> i64 {
        self.0.lock().unwrap().values().copied().min().unwrap_or(0)
    }
}

/// A compensatable "charge $amount to $account" step. `run` fails *before* touching the
/// ledger when this step is the injected failure point, so a failed step never leaks a
/// charge; `compensate` refunds exactly what `run` charged.
#[derive(Clone)]
struct Charge {
    idx: u32,
    fail_at: u32,
    account: String,
    amount: i64,
    next: u32,
}
#[task(state = u32, compensatable)]
impl Charge {
    type Output = (String, i64);
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    fn name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(format!("charge-{}", self.idx))
    }
    async fn run(&self, res: &Resources) -> Result<(TaskResult<u32>, (String, i64)), CanoError> {
        if self.idx == self.fail_at {
            return Err(CanoError::task_execution(format!(
                "step {} failed",
                self.idx
            )));
        }
        let ledger = res.get::<Ledger, _>("ledger")?;
        ledger.apply(&self.account, self.amount);
        Ok((
            TaskResult::Single(self.next),
            (self.account.clone(), self.amount),
        ))
    }
    async fn compensate(&self, res: &Resources, output: (String, i64)) -> Result<(), CanoError> {
        let (account, amount) = output;
        res.get::<Ledger, _>("ledger")?.apply(&account, -amount);
        Ok(())
    }
}

const STEPS: u32 = 3;
const PER_STEP: i64 = 100;

fn saga(ledger: Ledger, account: &str, fail_at: u32) -> Workflow<u32> {
    let mut wf = Workflow::new(Resources::new().insert("ledger", ledger)).add_exit_state(STEPS);
    for i in 0..STEPS {
        wf = wf.register_with_compensation(
            i,
            Charge {
                idx: i,
                fail_at,
                account: account.to_string(),
                amount: PER_STEP,
                next: i + 1,
            },
        );
    }
    wf
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_compensatable_workflows_concurrent_with_injected_failures() {
    let ledger = Ledger::default();
    const RUNS: u32 = 40;

    let mut handles = Vec::new();
    for i in 0..RUNS {
        let ledger = ledger.clone();
        let account = format!("acct-{i}");
        // Sweep the failure point across all steps (3 of every 4 runs roll back).
        let fail_at = i % (STEPS + 1);
        handles.push(tokio::spawn(async move {
            let result = saga(ledger.clone(), &account, fail_at).orchestrate(0).await;
            (account, fail_at, result)
        }));
    }

    let mut completed = 0;
    for h in handles {
        let (account, fail_at, result) = h.await.unwrap();
        if fail_at == STEPS {
            assert_eq!(
                result.unwrap(),
                STEPS,
                "{account}: no failure ⇒ run completes"
            );
            assert_eq!(
                ledger.balance(&account),
                PER_STEP * STEPS as i64,
                "{account}: a completed saga leaves all its charges in place"
            );
            completed += 1;
        } else {
            assert!(result.is_err(), "{account}: failure at step {fail_at}");
            assert_eq!(
                ledger.balance(&account),
                0,
                "{account}: a rolled-back saga nets to zero"
            );
        }
    }

    assert!(
        completed > 0 && (completed as u32) < RUNS,
        "the sweep exercises both outcomes"
    );
    assert!(ledger.min_balance() >= 0, "no account ever goes negative");
}

// -- crash / resume with no double compensation (real redb file) --

#[cfg(feature = "recovery")]
mod recovery {
    use super::*;
    use cano::RedbCheckpointStore;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum St {
        Start,
        Process,
        Split,
        Done,
    }

    type CompLog = Arc<Mutex<Vec<(String, u32)>>>;
    #[derive(Clone)]
    struct LogRes(CompLog);
    impl Resource for LogRes {}

    /// Forward `run` succeeds (→ `next`, output `value`); `compensate` records `(name, value)`.
    /// `fail_forward` makes the forward run fail (no output ⇒ never compensated).
    #[derive(Clone)]
    struct Step {
        name: &'static str,
        value: u32,
        next: St,
        fail_forward: bool,
    }
    #[task(state = St, compensatable)]
    impl Step {
        type Output = u32;
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        fn name(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed(self.name)
        }
        async fn run(&self, _res: &Resources) -> Result<(TaskResult<St>, u32), CanoError> {
            if self.fail_forward {
                return Err(CanoError::task_execution(format!("{} failed", self.name)));
            }
            Ok((TaskResult::Single(self.next.clone()), self.value))
        }
        async fn compensate(&self, res: &Resources, output: u32) -> Result<(), CanoError> {
            res.get::<LogRes, _>("log")?
                .0
                .lock()
                .unwrap()
                .push((self.name.to_string(), output));
            Ok(())
        }
    }

    #[tokio::test]
    async fn resume_after_completion_row_does_not_double_compensate() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("saga.redb");

        // Seed a log that crashed *after* B's completion row but *before* the next state's
        // entry row: A entry@0, A completion@1 (out 1), B entry@2, B completion@3 (out 2).
        {
            let seed = RedbCheckpointStore::new(&db).unwrap();
            seed.append("g", CheckpointRow::new(0, "Start", "A"))
                .await
                .unwrap();
            seed.append(
                "g",
                CheckpointRow::new(1, "Start", "A").with_output(serde_json::to_vec(&1u32).unwrap()),
            )
            .await
            .unwrap();
            seed.append("g", CheckpointRow::new(2, "Process", "B"))
                .await
                .unwrap();
            seed.append(
                "g",
                CheckpointRow::new(3, "Process", "B")
                    .with_output(serde_json::to_vec(&2u32).unwrap()),
            )
            .await
            .unwrap();
        }

        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        // Keep our own handle to the same store so we can inspect it after the run without
        // opening a *second* `Database` on the file (which redb would reject).
        let store = Arc::new(RedbCheckpointStore::new(&db).unwrap());
        let wf = Workflow::new(Resources::new().insert("log", LogRes(log.clone())))
            .register_with_compensation(
                St::Start,
                Step {
                    name: "A",
                    value: 1,
                    next: St::Process,
                    fail_forward: false,
                },
            )
            .register_with_compensation(
                St::Process,
                Step {
                    name: "B",
                    value: 2,
                    next: St::Split,
                    fail_forward: false,
                },
            )
            .register_with_compensation(
                St::Split,
                Step {
                    name: "C",
                    value: 3,
                    next: St::Done,
                    fail_forward: true,
                },
            )
            .add_exit_state(St::Done)
            .with_checkpoint_store(store.clone());

        let err = wf.resume_from("g").await.unwrap_err();
        assert_eq!(err.message(), "C failed");
        // B re-ran on resume and re-pushed its own entry; its persisted completion row at the
        // resume point must NOT be replayed too, or B would compensate twice.
        assert_eq!(
            *log.lock().unwrap(),
            vec![("B".to_string(), 2), ("A".to_string(), 1)],
            "B compensated exactly once, then A"
        );
        // Clean rollback ⇒ recovery log cleared.
        assert!(store.load_run("g").await.unwrap().is_empty());
    }
}
