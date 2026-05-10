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
        };
        let bytes = postcard::to_stdvec(&payload)
            .map_err(|e| CanoError::CheckpointStore(format!("encode checkpoint row: {e}")))?;

        let tx = self.db.begin_write().map_err(redb_err)?;
        {
            let mut table = tx.open_table(CHECKPOINTS).map_err(redb_err)?;
            table
                .insert((workflow_id, sequence), bytes.as_slice())
                .map_err(redb_err)?;
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
            let payload = postcard::from_bytes::<StoredRow>(value.value())
                .map_err(|e| CanoError::CheckpointStore(format!("decode checkpoint row: {e}")))?;
            rows.push(CheckpointRow {
                sequence: key.value().1,
                state: payload.state,
                task_id: payload.task_id,
                output_blob: payload.output_blob,
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
}
