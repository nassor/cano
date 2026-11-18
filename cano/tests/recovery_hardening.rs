//! Crash-recovery hardening against a *real* [`RedbStore`] file: the redb
//! write lock and on-disk durability are exercised for real here (the unit tests cover
//! the same shapes against an in-memory store).
#![cfg(feature = "store-redb")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cano::RedbStore;
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
    async fn run(&self, res: &Resources) -> Result<St, CanoError> {
        res.get::<SideEffects>()?.record("Start");
        Ok(St::Work)
    }
}
#[task(state = St)]
impl WorkTask {
    async fn run(&self, res: &Resources) -> Result<St, CanoError> {
        res.get::<SideEffects>()?.record("Work");
        Ok(St::Done)
    }
}

fn build(store: Arc<dyn CheckpointStore>, id: &str, sidefx: &Path) -> Workflow<St> {
    Workflow::new(Resources::new().insert(SideEffects {
        path: sidefx.to_path_buf(),
    }))
    .register(St::Start, StartTask)
    .register(St::Work, WorkTask)
    .add_exit_state(St::Done)
    .with_checkpoint_store(store)
    .with_workflow_id(id)
}

/// A `CheckpointStore` that errors after the first `ok_appends` appends — a stand-in
/// for a process dying mid-run with its earlier checkpoints already durable on disk.
struct KillAfter {
    inner: RedbStore,
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
    let store: Arc<dyn CheckpointStore> = Arc::new(RedbStore::new(&db).unwrap());

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
    let store: Arc<dyn CheckpointStore> = Arc::new(RedbStore::new(&db).unwrap());

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
            Err(e) => {
                assert_eq!(e.inner().category(), "checkpoint_store", "got {e}");
            }
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
            inner: RedbStore::new(&db).unwrap(),
            ok_appends: AtomicUsize::new(2),
        });
        let err = build(store, wf_id, &sidefx)
            .orchestrate(St::Start)
            .await
            .expect_err("generation 1 must crash before reaching Done");
        assert_eq!(err.inner().category(), "checkpoint_store");
    }
    assert_eq!(
        side_effects(&sidefx),
        ["Start", "Work"],
        "gen 1 ran Start then Work before crashing"
    );

    // Generation 2: resume on the same file — must pick up at Work, re-run it, finish at
    // Done, and NOT re-run Start (which is before the resume point).
    {
        let store: Arc<dyn CheckpointStore> = Arc::new(RedbStore::new(&db).unwrap());
        let wf = build(store, wf_id, &sidefx);
        assert_eq!(wf.resume_from(wf_id).await.unwrap(), St::Done);
    }
    assert_eq!(
        side_effects(&sidefx),
        ["Start", "Work", "Work"],
        "resume re-ran Work (idempotency contract) but not Start"
    );

    // The completed run cleared its recovery log.
    let reopened = RedbStore::new(&db).unwrap();
    assert!(reopened.load_run(wf_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn codec_v2_write_id_rows_decode_with_distinct_payloads() {
    #[derive(serde::Serialize)]
    struct StoredRowMirror {
        state: String,
        task_id: String,
        output_blob: Option<Vec<u8>>,
        kind: RowKind,
        workflow_version: u32,
    }

    const CHECKPOINTS: redb::TableDefinition<(&str, u64), &[u8]> =
        redb::TableDefinition::new("cano_checkpoints");

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("v2.redb");

    let encode_v2 = |write_id: u128| {
        let mut out = Vec::from(*b"CW");
        out.extend_from_slice(&write_id.to_le_bytes());
        out.extend_from_slice(
            &postcard::to_stdvec(&StoredRowMirror {
                state: "Start".into(),
                task_id: "t0".into(),
                output_blob: Some(vec![1, 2, 3]),
                kind: RowKind::CompensationCompletion,
                workflow_version: 5,
            })
            .unwrap(),
        );
        out
    };

    let bytes_a = encode_v2(1);
    let bytes_b = encode_v2(2);
    assert_ne!(bytes_a, bytes_b, "distinct write_ids must differ on disk");

    {
        let db = redb::Database::create(&db_path).unwrap();
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(CHECKPOINTS).unwrap();
            table.insert(("run-a", 0u64), bytes_a.as_slice()).unwrap();
            table.insert(("run-b", 0u64), bytes_b.as_slice()).unwrap();
            table
                .insert(("run-bad", 0u64), b"CWshort".as_slice())
                .unwrap();
        }
        tx.commit().unwrap();
    }

    let store = RedbStore::new(&db_path).unwrap();
    let expected = vec![
        CheckpointRow::new(0, "Start", "t0")
            .with_output(vec![1, 2, 3])
            .with_workflow_version(5),
    ];
    assert_eq!(store.load_run("run-a").await.unwrap(), expected);
    assert_eq!(store.load_run("run-b").await.unwrap(), expected);

    let err = store.load_run("run-bad").await.unwrap_err();
    assert_eq!(err.category(), "checkpoint_store");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redb_dropped_write_futures_do_not_commit_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(RedbStore::new(dir.path().join("drop-orphan.redb")).unwrap());
    let keep = CheckpointRow::new(0, "Keep", "t");
    store.append("keep-run", keep.clone()).await.unwrap();

    // Keep the redb write lock busy with concurrent large appends while we poll and
    // drop orphan write futures.
    let blob = vec![0xABu8; 512 * 1024];
    let mut blockers = Vec::new();
    for i in 0..8u64 {
        let s = Arc::clone(&store);
        let b = blob.clone();
        blockers.push(tokio::spawn(async move {
            s.append(
                &format!("blocker-{i}"),
                CheckpointRow::new(0, "B", "t").with_output(b),
            )
            .await
            .unwrap();
        }));
    }
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let waker = futures_util::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);

    let mut append_fut = Box::pin(store.append("orphan-run", CheckpointRow::new(0, "Orphan", "t")));
    let append_done = append_fut.as_mut().poll(&mut cx).is_ready();
    drop(append_fut);

    let mut clear_fut = Box::pin(store.clear("keep-run"));
    let clear_done = clear_fut.as_mut().poll(&mut cx).is_ready();
    drop(clear_fut);

    #[cfg(feature = "scheduler")]
    let swap_done = {
        use cano::SchedulerStore;
        let mut swap_fut =
            Box::pin(store.compare_and_swap("orphan-flow", None, b"orphan".to_vec()));
        let done = swap_fut.as_mut().poll(&mut cx).is_ready();
        drop(swap_fut);
        done
    };

    for h in blockers {
        h.await.unwrap();
    }
    // Ensure any aborted blocking tasks have finished before asserting.
    store
        .append("post-drop", CheckpointRow::new(0, "Ok", "t"))
        .await
        .unwrap();

    if !append_done {
        assert!(store.load_run("orphan-run").await.unwrap().is_empty());
    }
    if !clear_done {
        assert_eq!(store.load_run("keep-run").await.unwrap(), vec![keep]);
    }
    #[cfg(feature = "scheduler")]
    if !swap_done {
        use cano::SchedulerStore;
        assert!(store.load("orphan-flow").await.unwrap().is_none());
    }
}

#[cfg(feature = "store-postgres")]
#[tokio::test]
async fn store_retry_applies_jitter_to_transient_failures() {
    use cano::PostgresStore;
    use std::time::{Duration, Instant};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let accepts_srv = Arc::clone(&accepts);
    let srv = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            accepts_srv.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });

    let retries = 6;
    let delay = Duration::from_millis(60);
    let store = PostgresStore::builder()
        .with_create_table(false)
        .with_operation_timeout(Duration::from_millis(200))
        .with_retry(RetryMode::fixed(retries, delay))
        .connect(&format!("postgres://u:p@127.0.0.1:{}/db", addr.port()))
        .await
        .unwrap();

    let started = Instant::now();
    let err = store.load_run("jitter").await.unwrap_err();
    let elapsed = started.elapsed();

    srv.abort();
    assert_eq!(err.category(), "checkpoint_store");
    let observed_accepts = accepts.load(Ordering::SeqCst);
    if observed_accepts > 0 {
        assert_eq!(observed_accepts, retries + 1);
    } else {
        // Sandboxed environments block outbound TCP connect() before the kernel reaches accept().
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("error connecting to server"),
            "unexpected postgres error when no sockets were accepted: {msg}"
        );
    }
    // Each retry applies 50%..=100% jitter, so 6 * 60ms sleeps >= ~180ms minus timer granularity.
    assert!(
        elapsed >= Duration::from_millis(retries as u64 * 25),
        "expected >= ~50% of fixed retry budget ({elapsed:?})"
    );
}
