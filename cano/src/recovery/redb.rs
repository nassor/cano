//! [`RedbCheckpointStore`] — the default [`CheckpointStore`] backed by [`redb`].
//!
//! [`redb`](https://docs.rs/redb) is an embedded, ACID, pure-Rust key-value store
//! with no background daemon — a good fit for a crash-recovery log that ships in
//! the same process as the workflow engine. This module is compiled only when the
//! `recovery` feature is enabled.
//!
//! ## Storage layout
//!
//! A single table, `cano_checkpoints`, maps `(workflow_id, sequence)` to a
//! [`postcard`]-encoded [`StoredRow`] (the [`CheckpointRow`] fields *other than*
//! `sequence` — which is already the second key component, so there's no need to
//! store it twice). redb orders composite keys element by element, so within one
//! `workflow_id` the rows are stored — and range-scanned — in ascending
//! `sequence` order, which is exactly what [`CheckpointStore::load_run`] returns.

use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::Arc;

use redb::{Database, ReadableDatabase, TableDefinition};

use super::{CheckpointRow, CheckpointStore};
use crate::error::CanoError;
use cano_macros::checkpoint_store;

/// `(workflow_id, sequence) -> postcard(StoredRow)`.
const CHECKPOINTS: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("cano_checkpoints");

/// The payload half of a [`CheckpointRow`]: everything except `sequence`, which
/// is carried by the redb key. Kept private — callers only ever see `CheckpointRow`.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredRow {
    state: String,
    task_id: String,
    output_blob: Option<Vec<u8>>,
    kind: super::RowKind,
}

/// The key range covering every row for `workflow_id`, in ascending `sequence` order.
fn workflow_range(workflow_id: &str) -> RangeInclusive<(&str, u64)> {
    (workflow_id, u64::MIN)..=(workflow_id, u64::MAX)
}

/// Wrap any `redb` error (they all implement [`Display`](std::fmt::Display)) as a
/// [`CanoError::CheckpointStore`].
fn redb_err(e: impl std::fmt::Display) -> CanoError {
    CanoError::CheckpointStore(format!("redb: {e}"))
}

/// An embedded, ACID [`CheckpointStore`] backed by a single `redb` database file.
///
/// Cheap to clone — the database handle is held behind an `Arc`, so every clone
/// shares the same file and write lock. Construct one with [`new`](Self::new);
/// the constructor creates the file (if absent) and the checkpoint table so that
/// later reads never trip over a missing table.
#[derive(Clone)]
pub struct RedbCheckpointStore {
    db: Arc<Database>,
}

impl RedbCheckpointStore {
    /// Open (creating if necessary) the `redb` database at `path` and ensure the
    /// checkpoint table exists.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, CanoError> {
        let db = Database::create(path).map_err(redb_err)?;
        // Materialize the table up front so `begin_read` + `open_table` in
        // `load_run` cannot fail on a freshly created database.
        let tx = db.begin_write().map_err(redb_err)?;
        {
            let _ = tx.open_table(CHECKPOINTS).map_err(redb_err)?;
        }
        tx.commit().map_err(redb_err)?;
        Ok(Self { db: Arc::new(db) })
    }
}

#[checkpoint_store]
impl CheckpointStore for RedbCheckpointStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let sequence = row.sequence;
        let payload = StoredRow {
            state: row.state,
            task_id: row.task_id,
            output_blob: row.output_blob,
            kind: row.kind,
        };
        let bytes = postcard::to_stdvec(&payload)
            .map_err(|e| CanoError::CheckpointStore(format!("encode checkpoint row: {e}")))?;

        let tx = self.db.begin_write().map_err(redb_err)?;
        {
            let mut table = tx.open_table(CHECKPOINTS).map_err(redb_err)?;
            // Reject a duplicate `(workflow_id, sequence)`: `insert` would silently
            // overwrite the existing row. That only happens when two runs share a
            // `workflow_id` (a misuse — `resume_from` the existing run, or `clear` it
            // first), so surface it instead of corrupting the log. The uncommitted write
            // is rolled back when `tx` drops on the early return. (`resume_from` always
            // appends *new* sequences, so a legitimate resume never trips this.)
            if table
                .insert((workflow_id, sequence), bytes.as_slice())
                .map_err(redb_err)?
                .is_some()
            {
                return Err(CanoError::CheckpointStore(format!(
                    "checkpoint conflict: workflow {workflow_id:?} already has a row at \
                     sequence {sequence}; resume the existing run or clear it before starting \
                     a new one"
                )));
            }
        }
        tx.commit().map_err(redb_err)?;
        Ok(())
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let tx = self.db.begin_read().map_err(redb_err)?;
        let table = tx.open_table(CHECKPOINTS).map_err(redb_err)?;

        let mut rows = Vec::new();
        for entry in table.range(workflow_range(workflow_id)).map_err(redb_err)? {
            let (key, value) = entry.map_err(redb_err)?;
            let sequence = key.value().1;
            let payload: StoredRow = postcard::from_bytes(value.value())
                .map_err(|e| CanoError::CheckpointStore(format!("decode checkpoint row: {e}")))?;

            rows.push(CheckpointRow {
                sequence,
                state: payload.state,
                task_id: payload.task_id,
                output_blob: payload.output_blob,
                kind: payload.kind,
            });
        }
        Ok(rows)
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        let tx = self.db.begin_write().map_err(redb_err)?;
        {
            let mut table = tx.open_table(CHECKPOINTS).map_err(redb_err)?;
            table
                .retain_in(workflow_range(workflow_id), |_, _| false)
                .map_err(redb_err)?;
        }
        tx.commit().map_err(redb_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn append_load_clear_roundtrip() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();

        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        store
            .append("run", CheckpointRow::new(1, "B", "t1"))
            .await
            .unwrap();
        store
            .append(
                "run",
                CheckpointRow::new(2, "C", "t2").with_output(vec![7, 8, 9]),
            )
            .await
            .unwrap();

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(rows[0].state, "A");
        assert_eq!(rows[2].task_id, "t2");
        assert_eq!(rows[2].output_blob.as_deref(), Some(&[7u8, 8, 9][..]));

        store.clear("run").await.unwrap();
        assert!(store.load_run("run").await.unwrap().is_empty());
        // Clearing an unknown id is a no-op.
        store.clear("missing").await.unwrap();
    }

    #[tokio::test]
    async fn rows_ordered_by_sequence_regardless_of_insert_order() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();

        for seq in [5u64, 1, 9, 0, 3] {
            store
                .append("run", CheckpointRow::new(seq, format!("S{seq}"), "t"))
                .await
                .unwrap();
        }

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1, 3, 5, 9]
        );
    }

    #[tokio::test]
    async fn rows_survive_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ckpt.redb");
        {
            let store = RedbCheckpointStore::new(&path).unwrap();
            store
                .append("run", CheckpointRow::new(0, "A", "t0"))
                .await
                .unwrap();
            store
                .append("run", CheckpointRow::new(1, "B", "t1"))
                .await
                .unwrap();
        }
        // New instance over the same file — committed rows must still be there.
        let reopened = RedbCheckpointStore::new(&path).unwrap();
        let rows = reopened.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(rows[1].state, "B");
    }

    #[tokio::test]
    async fn clear_isolates_workflows() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();

        store
            .append("a", CheckpointRow::new(0, "A0", "t"))
            .await
            .unwrap();
        store
            .append("a", CheckpointRow::new(1, "A1", "t"))
            .await
            .unwrap();
        store
            .append("b", CheckpointRow::new(0, "B0", "t"))
            .await
            .unwrap();

        store.clear("a").await.unwrap();

        assert!(store.load_run("a").await.unwrap().is_empty());
        let b = store.load_run("b").await.unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].state, "B0");
    }

    #[tokio::test]
    async fn load_run_unknown_id_is_empty() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();
        assert!(store.load_run("nope").await.unwrap().is_empty());
    }

    // -- hardening ---------------------------------------------------------

    #[tokio::test]
    async fn append_rejects_duplicate_sequence() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();

        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        let err = store
            .append("run", CheckpointRow::new(0, "A-again", "t0"))
            .await
            .expect_err("duplicate (workflow_id, sequence) must be rejected");
        assert_eq!(err.category(), "checkpoint_store");
        assert!(
            err.message().contains("conflict"),
            "unexpected message: {err}"
        );

        // The original row survives the rejected write; new sequences still append.
        store
            .append("run", CheckpointRow::new(1, "B", "t1"))
            .await
            .unwrap();
        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.sequence, r.state.clone()))
                .collect::<Vec<_>>(),
            vec![(0, "A".to_string()), (1, "B".to_string())]
        );
        // A different workflow id can reuse sequence 0 freely.
        store
            .append("other", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_distinct_ids_stay_isolated_and_monotonic() {
        let dir = tempdir().unwrap();
        let store = Arc::new(RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap());

        const RUNS: u64 = 16;
        const ROWS_PER_RUN: u64 = 12;

        let mut handles = Vec::new();
        for r in 0..RUNS {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let id = format!("run-{r}");
                for s in 0..ROWS_PER_RUN {
                    store
                        .append(&id, CheckpointRow::new(s, format!("S{r}-{s}"), "t"))
                        .await
                        .unwrap();
                    // Yield so the redb write lock changes hands between runs.
                    tokio::task::yield_now().await;
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        for r in 0..RUNS {
            let rows = store.load_run(&format!("run-{r}")).await.unwrap();
            assert_eq!(rows.len() as u64, ROWS_PER_RUN, "run {r} row count");
            assert_eq!(
                rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
                (0..ROWS_PER_RUN).collect::<Vec<_>>(),
                "run {r} sequences"
            );
            assert!(
                rows.iter()
                    .enumerate()
                    .all(|(i, row)| row.state == format!("S{r}-{i}")),
                "run {r} rows belong only to that run"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_same_id_distinct_sequences_all_land() {
        let dir = tempdir().unwrap();
        let store = Arc::new(RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap());

        const N: u64 = 32;
        let mut handles = Vec::new();
        for s in 0..N {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                store
                    .append("run", CheckpointRow::new(s, format!("S{s}"), "t"))
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            (0..N).collect::<Vec<_>>(),
            "every distinct sequence landed exactly once, in order"
        );
    }

    #[tokio::test]
    async fn large_output_blob_roundtrips() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();
        let blob: Vec<u8> = (0..5 * 1024 * 1024usize).map(|i| (i % 251) as u8).collect();
        store
            .append(
                "run",
                CheckpointRow::new(0, "Big", "t").with_output(blob.clone()),
            )
            .await
            .unwrap();
        let rows = store.load_run("run").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].output_blob.as_deref(), Some(blob.as_slice()));
    }

    #[tokio::test]
    async fn corrupted_stored_row_is_a_decode_error_not_a_panic() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();
        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        // Overwrite sequence 1's slot with bytes that aren't a valid `StoredRow`.
        // (We reach past the public API on purpose to simulate an on-disk corruption.)
        {
            let tx = store.db.begin_write().unwrap();
            {
                let mut table = tx.open_table(CHECKPOINTS).unwrap();
                table
                    .insert(("run", 1u64), [0xFFu8, 0xFF, 0xFF, 0xFF].as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }
        let err = store
            .load_run("run")
            .await
            .expect_err("a corrupted row must surface as an error, not panic");
        assert_eq!(err.category(), "checkpoint_store");
        assert!(
            err.message().contains("decode"),
            "unexpected message: {err}"
        );
    }

    // -- RowKind round-trip ---------------------------------------------------

    /// All three `RowKind` variants must survive an append → load_run cycle with
    /// their kind and output_blob intact.
    #[tokio::test]
    async fn all_row_kinds_roundtrip() {
        let dir = tempdir().unwrap();
        let store = RedbCheckpointStore::new(dir.path().join("ckpt.redb")).unwrap();

        // StateEntry: kind=StateEntry, output_blob=None
        store
            .append("run", CheckpointRow::new(0, "A", "task-a"))
            .await
            .unwrap();

        // CompensationCompletion: kind=CompensationCompletion, output_blob=Some
        store
            .append(
                "run",
                CheckpointRow::new(1, "B", "task-b").with_output(vec![1, 2, 3]),
            )
            .await
            .unwrap();

        // StepCursor: kind=StepCursor, output_blob=Some
        store
            .append(
                "run",
                CheckpointRow::new(2, "C", "task-c").with_cursor(vec![4, 5]),
            )
            .await
            .unwrap();

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(rows.len(), 3);

        assert_eq!(rows[0].sequence, 0);
        assert_eq!(rows[0].state, "A");
        assert_eq!(rows[0].kind, super::super::RowKind::StateEntry);
        assert_eq!(rows[0].output_blob, None);

        assert_eq!(rows[1].sequence, 1);
        assert_eq!(rows[1].state, "B");
        assert_eq!(rows[1].kind, super::super::RowKind::CompensationCompletion);
        assert_eq!(rows[1].output_blob.as_deref(), Some(&[1u8, 2, 3][..]));

        assert_eq!(rows[2].sequence, 2);
        assert_eq!(rows[2].state, "C");
        assert_eq!(rows[2].kind, super::super::RowKind::StepCursor);
        assert_eq!(rows[2].output_blob.as_deref(), Some(&[4u8, 5][..]));
    }
}
