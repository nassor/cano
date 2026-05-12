//! Crash-recovery hardening against a *real* [`RedbCheckpointStore`] file: the redb
//! write lock and on-disk durability are exercised for real here (the unit tests cover
//! the same shapes against an in-memory store).
//!
//! Requires the `recovery` feature.
#![cfg(feature = "recovery")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cano::RedbCheckpointStore;
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum St {
    Start,
    Work,
    Done,
}

/// A file the tasks append to, so a later process / assertion can see what ran.
struct SideEffects {
    path: PathBuf,
}
impl Resource for SideEffects {}
impl SideEffects {
    fn record(&self, what: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .expect("open side-effects file");
        writeln!(f, "{what}").expect("append");
    }
}
fn side_effects(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[derive(Clone)]
struct StartTask;
#[derive(Clone)]
struct WorkTask;

#[task(state = St)]
impl StartTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<St>, CanoError> {
        res.get::<SideEffects, _>("sidefx")?.record("Start");
        Ok(TaskResult::Single(St::Work))
    }
}
#[task(state = St)]
impl WorkTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<St>, CanoError> {
        res.get::<SideEffects, _>("sidefx")?.record("Work");
        Ok(TaskResult::Single(St::Done))
    }
}

fn build(store: Arc<dyn CheckpointStore>, id: &str, sidefx: &Path) -> Workflow<St> {
    Workflow::new(Resources::new().insert(
        "sidefx",
        SideEffects {
            path: sidefx.to_path_buf(),
        },
    ))
    .register(St::Start, StartTask)
    .register(St::Work, WorkTask)
    .add_exit_state(St::Done)
    .with_checkpoint_store(store)
    .with_workflow_id(id)
}

/// A `CheckpointStore` that errors after the first `ok_appends` appends — a stand-in
/// for a process dying mid-run with its earlier checkpoints already durable on disk.
struct KillAfter {
    inner: RedbCheckpointStore,
    ok_appends: AtomicUsize,
}
#[checkpoint_store]
impl CheckpointStore for KillAfter {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        if self.ok_appends.fetch_sub(1, Ordering::SeqCst) == 0 {
            return Err(CanoError::checkpoint_store("simulated crash"));
        }
        self.inner.append(workflow_id, row).await
    }
    async fn load_run(&self, id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        self.inner.load_run(id).await
    }
    async fn clear(&self, id: &str) -> Result<(), CanoError> {
        self.inner.clear(id).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_workflows_one_redb_file_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ckpt.redb");
    let store: Arc<dyn CheckpointStore> = Arc::new(RedbCheckpointStore::new(&db).unwrap());

    const RUNS: usize = 12;
    let mut handles = Vec::new();
    for i in 0..RUNS {
        let store = Arc::clone(&store);
        let sidefx = dir.path().join(format!("sfx-{i}.log"));
        handles.push(tokio::spawn(async move {
            build(store, &format!("run-{i}"), &sidefx)
                .orchestrate(St::Start)
                .await
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap().unwrap(), St::Done);
    }

    for i in 0..RUNS {
        assert!(
            store
                .load_run(&format!("run-{i}"))
                .await
                .unwrap()
                .is_empty(),
            "run-{i}: a successful run leaves no recovery log behind"
        );
        assert_eq!(
            side_effects(&dir.path().join(format!("sfx-{i}.log"))),
            ["Start", "Work"],
            "run-{i}: both tasks ran exactly once"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn racing_runs_of_one_id_on_a_real_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ckpt.redb");
    let store: Arc<dyn CheckpointStore> = Arc::new(RedbCheckpointStore::new(&db).unwrap());

    let mut handles = Vec::new();
    for i in 0..2 {
        let store = Arc::clone(&store);
        let sidefx = dir.path().join(format!("sfx-{i}.log"));
        handles.push(tokio::spawn(async move {
            build(store, "dup", &sidefx).orchestrate(St::Start).await
        }));
    }
    let mut completed = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(St::Done) => completed += 1,
            Ok(other) => panic!("unexpected success state {other:?}"),
            Err(e) => assert_eq!(e.category(), "checkpoint_store", "got {e}"),
        }
    }
    assert!(
        completed >= 1,
        "at least one run wins sequence 0 and completes"
    );
}

#[tokio::test]
async fn crash_mid_work_then_resume_does_not_re_run_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ckpt.redb");
    let sidefx = dir.path().join("sfx.log");
    let wf_id = "gen";

    // Generation 1: a store that survives 2 appends then "crashes". The appends are
    // Start's entry row (1), Work's entry row (2) — written before Work runs — then Done's
    // entry row (3) fails. So Work ran exactly once and its checkpoint is durable.
    {
        let store = Arc::new(KillAfter {
            inner: RedbCheckpointStore::new(&db).unwrap(),
            ok_appends: AtomicUsize::new(2),
        });
        let err = build(store, wf_id, &sidefx)
            .orchestrate(St::Start)
            .await
            .expect_err("generation 1 must crash before reaching Done");
        assert_eq!(err.category(), "checkpoint_store");
    }
    assert_eq!(
        side_effects(&sidefx),
        ["Start", "Work"],
        "gen 1 ran Start then Work before crashing"
    );

    // Generation 2: resume on the same file — must pick up at Work, re-run it, finish at
    // Done, and NOT re-run Start (which is before the resume point).
    {
        let store: Arc<dyn CheckpointStore> = Arc::new(RedbCheckpointStore::new(&db).unwrap());
        let wf = build(store, wf_id, &sidefx);
        assert_eq!(wf.resume_from(wf_id).await.unwrap(), St::Done);
    }
    assert_eq!(
        side_effects(&sidefx),
        ["Start", "Work", "Work"],
        "resume re-ran Work (idempotency contract) but not Start"
    );

    // The completed run cleared its recovery log.
    let reopened = RedbCheckpointStore::new(&db).unwrap();
    assert!(reopened.load_run(wf_id).await.unwrap().is_empty());
}
