//! UI test: `#[cano::checkpoint_store]` on an inherent `impl T { ... }` block builds
//! the `impl CheckpointStore for T` header (and rewrites the `async fn`s).

use cano::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct InMemory(Mutex<HashMap<String, Vec<CheckpointRow>>>);

#[checkpoint_store]
impl InMemory {
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
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(workflow_id)
            .cloned()
            .unwrap_or_default())
    }
    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.0.lock().unwrap().remove(workflow_id);
        Ok(())
    }
}

#[tokio::test]
async fn inherent_checkpoint_store_impl_roundtrips() {
    let store = InMemory::default();
    store
        .append("run", CheckpointRow::new(0, "Start", "t"))
        .await
        .unwrap();
    store
        .append("run", CheckpointRow::new(1, "Done", ""))
        .await
        .unwrap();

    let rows = store.load_run("run").await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].state, "Start");
    assert_eq!(rows[1].task_id, "");

    store.clear("run").await.unwrap();
    assert!(store.load_run("run").await.unwrap().is_empty());
}
