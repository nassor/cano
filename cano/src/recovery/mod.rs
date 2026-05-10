//! # Recovery — Append-Only Checkpoint Storage
//!
//! This module defines the [`CheckpointStore`] trait: a pluggable, append-only
//! log of FSM transitions that lets a workflow be resumed after a crash. Each
//! transition is recorded as one [`CheckpointRow`] — a monotonically increasing
//! sequence number, the state that was entered, the task that produced it, and
//! an optional output blob for compensatable tasks.
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
//!         self.0.lock().unwrap().entry(workflow_id.to_string()).or_default().push(row);
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

/// One recorded FSM transition.
///
/// Rows are append-only and ordered within a run by [`sequence`](Self::sequence).
/// `output_blob` is `Some` only for tasks whose output must be retained for
/// compensation/rollback; it is opaque bytes to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "recovery", derive(bincode::Encode, bincode::Decode))]
pub struct CheckpointRow {
    /// Monotonically increasing position of this transition within its run.
    pub sequence: u64,
    /// The state that was entered at this transition.
    pub state: String,
    /// Identifier of the task that produced this transition (see `Task::name`).
    pub task_id: String,
    /// Optional opaque output blob, retained for compensatable tasks.
    pub output_blob: Option<Vec<u8>>,
}

impl CheckpointRow {
    /// Build a row with no output blob.
    pub fn new(sequence: u64, state: impl Into<String>, task_id: impl Into<String>) -> Self {
        Self {
            sequence,
            state: state.into(),
            task_id: task_id.into(),
            output_blob: None,
        }
    }

    /// Attach an output blob (for compensatable tasks whose output must be
    /// retained for rollback). Builder-style: `CheckpointRow::new(..).with_output(bytes)`.
    pub fn with_output(mut self, output_blob: Vec<u8>) -> Self {
        self.output_blob = Some(output_blob);
        self
    }
}

/// Append-only checkpoint log keyed by workflow id.
///
/// Implementations record one [`CheckpointRow`] per FSM transition and can
/// replay them in sequence order to resume a crashed run. The contract:
///
/// - [`append`](Self::append) durably persists `row` for `workflow_id`.
/// - [`load_run`](Self::load_run) returns every row ever appended for
///   `workflow_id`, **sorted ascending by `sequence`**, or an empty `Vec` if the
///   id is unknown.
/// - [`clear`](Self::clear) removes all rows for `workflow_id` and must not
///   affect any other id. Clearing an unknown id is a no-op (`Ok`).
///
/// Backends must be `Send + Sync + 'static` so a single store can be shared
/// (typically as `Arc<dyn CheckpointStore>`) across concurrent workflows.
#[checkpoint_store]
pub trait CheckpointStore: Send + Sync + 'static {
    /// Durably append `row` to the log for `workflow_id`.
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
            self.0
                .lock()
                .unwrap()
                .entry(workflow_id.to_string())
                .or_default()
                .push(row);
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

        let carried = CheckpointRow::new(4, "Done", "worker").with_output(vec![1, 2, 3]);
        assert_eq!(carried.sequence, 4);
        assert_eq!(carried.output_blob.as_deref(), Some(&[1u8, 2, 3][..]));
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
}
