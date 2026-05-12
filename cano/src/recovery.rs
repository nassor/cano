//! # Recovery — Append-Only Checkpoint Storage
//!
//! This module defines the [`CheckpointStore`] trait: a pluggable, append-only
//! log of FSM transitions that lets a workflow be resumed after a crash. Each
//! transition is recorded as one [`CheckpointRow`] — a monotonically increasing
//! sequence number, the state that was entered, the task that produced it, an
//! optional output blob for compensatable tasks, and a [`RowKind`] discriminant
//! that identifies why the row was written.
//!
//! ## Design
//!
//! - **Append-only.** A run is a sequence of rows; the store never mutates a row
//!   in place. Reconstructing a run is "load every row for this workflow id, in
//!   sequence order".
//! - **Pluggable.** Implement [`CheckpointStore`] over any backend (an embedded
//!   KV store, Postgres, an HTTP service). The trait is intentionally tiny:
//!   [`append`](CheckpointStore::append), [`load_run`](CheckpointStore::load_run),
//!   [`clear`](CheckpointStore::clear).
//! - **Optional default.** With the `recovery` feature enabled, [`RedbCheckpointStore`]
//!   provides an embedded, ACID, daemon-free implementation backed by
//!   [`redb`](https://docs.rs/redb). The default build pulls in no extra dependencies.
//!
//! ## Example
//!
//! ```rust
//! use cano::recovery::{CheckpointRow, CheckpointStore};
//! use cano::CanoError;
//! use std::collections::HashMap;
//! use std::sync::Mutex;
//!
//! /// A trivial in-memory store, useful for tests.
//! #[derive(Default)]
//! struct InMemoryStore(Mutex<HashMap<String, Vec<CheckpointRow>>>);
//!
//! // `#[cano::checkpoint_store]` on an inherent `impl` builds the
//! // `impl CheckpointStore for InMemoryStore` header for you. (Or write that header
//! // yourself: `#[cano::checkpoint_store] impl CheckpointStore for InMemoryStore { … }`.)
//! #[cano::checkpoint_store]
//! impl InMemoryStore {
//!     async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
//!         let mut runs = self.0.lock().unwrap();
//!         let rows = runs.entry(workflow_id.to_string()).or_default();
//!         if rows.iter().any(|r| r.sequence == row.sequence) {
//!             return Err(CanoError::checkpoint_store(format!(
//!                 "checkpoint conflict: {workflow_id:?} already has sequence {}", row.sequence
//!             )));
//!         }
//!         rows.push(row);
//!         Ok(())
//!     }
//!     async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
//!         let mut rows = self.0.lock().unwrap().get(workflow_id).cloned().unwrap_or_default();
//!         rows.sort_by_key(|r| r.sequence); // contract: ascending by `sequence`
//!         Ok(rows)
//!     }
//!     async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
//!         self.0.lock().unwrap().remove(workflow_id);
//!         Ok(())
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let store = InMemoryStore::default();
//! store.append("run-1", CheckpointRow::new(0, "Start", "fetch")).await?;
//! store.append("run-1", CheckpointRow::new(1, "Done", "process")).await?;
//! assert_eq!(store.load_run("run-1").await?.len(), 2);
//! # Ok(())
//! # }
//! ```

use crate::error::CanoError;
use cano_macros::checkpoint_store;

#[cfg(feature = "recovery")]
mod redb;

#[cfg(feature = "recovery")]
pub use redb::RedbCheckpointStore;

/// Why a [`CheckpointRow`] was written — distinguishes ordinary state-entry rows
/// from saga compensation-completion rows and `SteppedTask` cursor rows so
/// `resume_from` can route each correctly.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RowKind {
    /// An ordinary "the FSM entered this state" row (the common case).
    #[default]
    StateEntry,
    /// A saga compensatable task's completion row, carrying its serialized output in `output_blob`.
    CompensationCompletion,
    /// A `SteppedTask` iteration row, carrying the serialized cursor in `output_blob`.
    StepCursor,
}

/// One recorded FSM transition.
///
/// Rows are append-only and ordered within a run by [`sequence`](Self::sequence).
/// `output_blob` carries opaque bytes whose purpose is discriminated by [`kind`](Self::kind):
/// `None` for plain state-entry rows, `Some` for saga completion rows
/// ([`RowKind::CompensationCompletion`]) and `SteppedTask` cursor rows ([`RowKind::StepCursor`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointRow {
    /// Monotonically increasing position of this transition within its run.
    pub sequence: u64,
    /// The state that was entered at this transition.
    pub state: String,
    /// Identifier of the task that produced this transition (see `Task::name`).
    pub task_id: String,
    /// Optional opaque bytes payload; see [`kind`](Self::kind) for semantics.
    pub output_blob: Option<Vec<u8>>,
    /// Why this row was written; drives how [`resume_from`](crate::workflow::Workflow::resume_from)
    /// routes the row during replay.
    pub kind: RowKind,
}

impl CheckpointRow {
    /// Build a plain state-entry row: `kind = `[`RowKind::StateEntry`]`, `output_blob = None`.
    pub fn new(sequence: u64, state: impl Into<String>, task_id: impl Into<String>) -> Self {
        Self {
            sequence,
            state: state.into(),
            task_id: task_id.into(),
            output_blob: None,
            kind: RowKind::StateEntry,
        }
    }

    /// Attach a saga compensation output blob and mark the row as
    /// [`RowKind::CompensationCompletion`].
    ///
    /// Used for compensatable tasks whose output must be retained for rollback so that
    /// [`resume_from`](crate::workflow::Workflow::resume_from) can rehydrate the
    /// compensation stack. Builder-style: `CheckpointRow::new(..).with_output(bytes)`.
    pub fn with_output(mut self, output_blob: Vec<u8>) -> Self {
        self.output_blob = Some(output_blob);
        self.kind = RowKind::CompensationCompletion;
        self
    }

    /// Attach a `SteppedTask` cursor blob and mark the row as [`RowKind::StepCursor`].
    ///
    /// Mark this row as a `SteppedTask` cursor checkpoint carrying the serialized cursor
    /// bytes. Builder-style: `CheckpointRow::new(..).with_cursor(bytes)`.
    pub fn with_cursor(mut self, cursor_blob: Vec<u8>) -> Self {
        self.output_blob = Some(cursor_blob);
        self.kind = RowKind::StepCursor;
        self
    }
}

/// Append-only checkpoint log keyed by workflow id.
///
/// Implementations record one [`CheckpointRow`] per FSM transition and can
/// replay them in sequence order to resume a crashed run. The contract:
///
/// - [`append`](Self::append) durably persists `row` for `workflow_id`. It **must
///   reject a duplicate `(workflow_id, row.sequence)`** with an `Err` rather than
///   overwriting the existing row — the engine assigns sequences densely from `0`, so a
///   collision means two runs are sharing a `workflow_id` (a misuse: resume the existing
///   run, or [`clear`](Self::clear) it first). A legitimate [`resume_from`] only ever
///   appends sequences past the last persisted one, so it never collides.
/// - [`load_run`](Self::load_run) returns every row ever appended for
///   `workflow_id`, **sorted ascending by `sequence`**, or an empty `Vec` if the
///   id is unknown.
/// - [`clear`](Self::clear) removes all rows for `workflow_id` and must not
///   affect any other id. Clearing an unknown id is a no-op (`Ok`).
///
/// Backends must be `Send + Sync + 'static` so a single store can be shared
/// (typically as `Arc<dyn CheckpointStore>`) across concurrent workflows; `append`,
/// `load_run` and `clear` may be called concurrently for the same or different ids.
///
/// [`resume_from`]: crate::workflow::Workflow::resume_from
#[checkpoint_store]
pub trait CheckpointStore: Send + Sync + 'static {
    /// Durably append `row` to the log for `workflow_id`. Returns an error if a row
    /// already exists at `(workflow_id, row.sequence)` (see the trait-level contract).
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError>;

    /// Load every row for `workflow_id`, sorted ascending by `sequence`.
    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError>;

    /// Remove all rows for `workflow_id`. No-op if the id is unknown.
    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Minimal in-memory `CheckpointStore` for exercising the trait contract.
    #[derive(Default)]
    struct InMemoryStore(Mutex<HashMap<String, Vec<CheckpointRow>>>);

    #[checkpoint_store]
    impl CheckpointStore for InMemoryStore {
        async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
            let mut runs = self.0.lock().unwrap();
            let rows = runs.entry(workflow_id.to_string()).or_default();
            if rows.iter().any(|r| r.sequence == row.sequence) {
                return Err(CanoError::checkpoint_store(format!(
                    "checkpoint conflict: {workflow_id:?} already has sequence {}",
                    row.sequence
                )));
            }
            rows.push(row);
            Ok(())
        }

        async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
            let mut rows = self
                .0
                .lock()
                .unwrap()
                .get(workflow_id)
                .cloned()
                .unwrap_or_default();
            rows.sort_by_key(|r| r.sequence);
            Ok(rows)
        }

        async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
            self.0.lock().unwrap().remove(workflow_id);
            Ok(())
        }
    }

    #[test]
    fn checkpoint_store_is_dyn_compatible() {
        // `CheckpointStore` must stay object-safe so a single store can be shared
        // as `Arc<dyn CheckpointStore>` across concurrent workflows.
        let _erased: std::sync::Arc<dyn CheckpointStore> =
            std::sync::Arc::new(InMemoryStore::default());
    }

    #[test]
    fn checkpoint_row_constructors() {
        let bare = CheckpointRow::new(3, "Process", "worker");
        assert_eq!(bare.sequence, 3);
        assert_eq!(bare.state, "Process");
        assert_eq!(bare.task_id, "worker");
        assert_eq!(bare.output_blob, None);
        assert_eq!(bare.kind, RowKind::StateEntry);

        let carried = CheckpointRow::new(4, "Done", "worker").with_output(vec![1, 2, 3]);
        assert_eq!(carried.sequence, 4);
        assert_eq!(carried.output_blob.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(carried.kind, RowKind::CompensationCompletion);

        let cursor = CheckpointRow::new(5, "Step", "stepper").with_cursor(vec![9, 8, 7]);
        assert_eq!(cursor.sequence, 5);
        assert_eq!(cursor.output_blob.as_deref(), Some(&[9u8, 8, 7][..]));
        assert_eq!(cursor.kind, RowKind::StepCursor);
    }

    #[tokio::test]
    async fn trait_roundtrip_append_load_clear() {
        let store = InMemoryStore::default();

        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        store
            .append("run", CheckpointRow::new(1, "B", "t1"))
            .await
            .unwrap();
        store
            .append("run", CheckpointRow::new(2, "C", "t2").with_output(vec![9]))
            .await
            .unwrap();

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(rows[2].output_blob.as_deref(), Some(&[9u8][..]));

        store.clear("run").await.unwrap();
        assert!(store.load_run("run").await.unwrap().is_empty());

        // Clearing an unknown id is a no-op.
        store.clear("never-existed").await.unwrap();
    }

    #[tokio::test]
    async fn load_run_unknown_id_is_empty() {
        let store = InMemoryStore::default();
        assert!(store.load_run("nope").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn append_rejects_duplicate_sequence() {
        let store = InMemoryStore::default();
        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        // Same `(workflow_id, sequence)` again — must be rejected, not overwrite.
        let err = store
            .append("run", CheckpointRow::new(0, "A-again", "t0"))
            .await
            .expect_err("duplicate sequence must be rejected");
        assert_eq!(err.category(), "checkpoint_store");
        // The original row is untouched and a *different* sequence still appends fine.
        store
            .append("run", CheckpointRow::new(1, "B", "t1"))
            .await
            .unwrap();
        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.sequence, r.state.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "A"), (1, "B")]
        );
        // Distinct ids never collide.
        store
            .append("other", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
    }
}
