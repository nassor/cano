//! Migration-compatibility for redb databases written before `workflow_version`
//! existed.
//!
//! These tests construct a *real* redb file by hand and write rows in the legacy
//! byte format (no `workflow_version` field), then drive the full
//! [`Workflow::resume_from`] path through [`RedbCheckpointStore`]:
//!
//! 1. A workflow with the default `workflow_version = 0` resumes such a database
//!    cleanly — the decode fallback in `RedbCheckpointStore` treats the missing
//!    field as `0`, and the version check passes.
//! 2. The same database against a workflow that bumped to `with_workflow_version(1)`
//!    is rejected with [`CanoError::WorkflowVersionMismatch`].
//!
//! The unit-level legacy-decode test in `cano/src/recovery/redb.rs` already proves
//! that the decode path works; these tests are the integration-level safety net
//! around `resume_from`.
#![cfg(feature = "recovery")]

use std::path::Path;
use std::sync::Arc;

use cano::prelude::*;
use cano::recovery::RowKind;
use redb::{Database, TableDefinition};
use tempfile::tempdir;

/// Must mirror the production constant in `cano/src/recovery/redb.rs` byte-for-byte;
/// the integration test is asserting on-disk binary compatibility, so reach in
/// through the redb API rather than the public store wrapper.
const CHECKPOINTS: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("cano_checkpoints");

/// Pre-versioning on-disk shape (a serializable twin of `StoredRowV0`). Field order
/// MUST match `cano/src/recovery/redb.rs`'s `StoredRowV0` exactly so postcard emits
/// the same bytes a real legacy database would contain.
#[derive(serde::Serialize)]
struct LegacyWrite {
    state: String,
    task_id: String,
    output_blob: Option<Vec<u8>>,
    kind: RowKind,
}

/// Write a single legacy-shape `StateEntry` row for `workflow_id` at sequence 0,
/// labelled "Start", straight into the redb table.
fn seed_legacy_row(path: &Path, workflow_id: &str) {
    let db = Database::create(path).expect("create redb file");
    let tx = db.begin_write().expect("begin write txn");
    {
        let mut table = tx.open_table(CHECKPOINTS).expect("open checkpoints table");
        let legacy = LegacyWrite {
            state: "Start".into(),
            task_id: "legacy-task".into(),
            output_blob: None,
            kind: RowKind::StateEntry,
        };
        let bytes = postcard::to_stdvec(&legacy).expect("encode legacy row");
        table
            .insert((workflow_id, 0u64), bytes.as_slice())
            .expect("insert legacy row");
    }
    tx.commit().expect("commit legacy txn");
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum St {
    Start,
    Done,
}

/// Trivial idempotent task: re-running it on resume just transitions to `Done`.
#[derive(Clone)]
struct StartTask;

#[task]
impl Task<St> for StartTask {
    async fn run_bare(&self) -> Result<TaskResult<St>, CanoError> {
        Ok(TaskResult::Single(St::Done))
    }
}

#[tokio::test]
async fn legacy_redb_row_resumes_under_default_workflow_version() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("ckpt.redb");
    let wf_id = "legacy-wf";

    // Pre-populate the redb file with a single legacy-shape row, then drop the
    // direct database handle so `RedbCheckpointStore::new` can take its own.
    seed_legacy_row(&path, wf_id);

    let store: Arc<dyn CheckpointStore> =
        Arc::new(RedbCheckpointStore::new(&path).expect("open redb store over seeded file"));

    // Default workflow_version is 0 — matches the legacy row's defaulted version.
    let workflow = Workflow::bare()
        .register(St::Start, StartTask)
        .add_exit_state(St::Done)
        .with_checkpoint_store(store);

    let final_state = workflow
        .resume_from(wf_id, CancellationToken::disabled())
        .await
        .expect("legacy row must resume cleanly under default workflow_version");
    assert_eq!(final_state, St::Done);
}

#[tokio::test]
async fn legacy_redb_row_rejected_when_workflow_version_bumped() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("ckpt.redb");
    let wf_id = "legacy-wf";

    seed_legacy_row(&path, wf_id);

    let store: Arc<dyn CheckpointStore> =
        Arc::new(RedbCheckpointStore::new(&path).expect("open redb store over seeded file"));

    // Bumping to v1 must surface the stored-vs-expected mismatch as a
    // WorkflowVersionMismatch error (stored = 0 from legacy fallback, expected = 1).
    let workflow = Workflow::bare()
        .register(St::Start, StartTask)
        .add_exit_state(St::Done)
        .with_checkpoint_store(store)
        .with_workflow_version(1);

    let err = workflow
        .resume_from(wf_id, CancellationToken::disabled())
        .await
        .expect_err("bumped workflow_version must reject the legacy row");
    assert_eq!(err, CanoError::workflow_version_mismatch(0, 1));
}
