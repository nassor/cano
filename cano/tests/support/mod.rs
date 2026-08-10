//! Shared scaffolding for the processing-model reliability suites
//! (`stream_reliability`, `batch_reliability`, `stepped_reliability`,
//! `poll_reliability`, `pipeline_reliability`).
//!
//! Compiled into each test binary via `mod support;` — a `tests/` subdirectory is
//! not itself a test target. No feature flags required: checkpointing uses the
//! in-memory [`MemStore`].
#![allow(dead_code)] // each test binary uses a different subset of this module

use cano::prelude::*;
use futures_util::{Stream, stream};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::pin::Pin;

/// Two-state flow used by the single-model tests.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Flow {
    Work,
    Done,
}

/// In-memory append-only checkpoint store (the doc-example shape from
/// `cano::recovery`), plus test-side introspection helpers.
#[derive(Default)]
pub struct MemStore(Mutex<HashMap<String, Vec<CheckpointRow>>>);

#[cano::checkpoint_store]
impl MemStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let mut runs = self.0.lock();
        let rows = runs.entry(workflow_id.to_string()).or_default();
        if rows.iter().any(|r| r.sequence == row.sequence) {
            return Err(CanoError::checkpoint_store(format!(
                "duplicate sequence {} for {workflow_id:?}",
                row.sequence
            )));
        }
        rows.push(row);
        Ok(())
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let mut rows = self.0.lock().get(workflow_id).cloned().unwrap_or_default();
        rows.sort_by_key(|r| r.sequence);
        Ok(rows)
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.0.lock().remove(workflow_id);
        Ok(())
    }
}

impl MemStore {
    pub fn rows(&self, workflow_id: &str) -> Vec<CheckpointRow> {
        self.0.lock().get(workflow_id).cloned().unwrap_or_default()
    }

    /// Latest persisted `StepCursor` row for `state`, decoded as the integer the
    /// stream/stepped adapters serialize via `serde_json`.
    pub fn last_cursor(&self, workflow_id: &str, state: &str) -> Option<u64> {
        let mut rows = self.rows(workflow_id);
        rows.sort_by_key(|r| r.sequence);
        rows.iter()
            .rev()
            .find(|r| r.kind == RowKind::StepCursor && r.state == state)
            .and_then(|r| r.output_blob.as_deref())
            .map(|b| serde_json::from_slice(b).expect("cursor blob is a serde_json integer"))
    }
}

/// A finite, always-ready source stream over an inclusive range.
pub fn boxed(range: std::ops::RangeInclusive<u64>) -> Pin<Box<dyn Stream<Item = u64> + Send>> {
    Box::pin(stream::iter(range))
}
