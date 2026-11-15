//! # Workflow Resources Example
//!
//! Demonstrates the `Resources` abstraction in Cano workflows with three
//! distinct resource styles:
//!
//! 1. **`MemoryStore`** — Cano's built-in in-process scratch space for
//!    passing values between tasks within one workflow run.
//! 2. **`CounterResource`** — a custom resource with non-trivial
//!    `setup` / `teardown` lifecycle and interior mutability.
//! 3. **`RedbResource`** — a real persistent embedded database (redb)
//!    opened in `setup` and torn down (and removed) in `teardown`.
//! 4. **`WorkflowParams`** — plain configuration data carried as a resource
//!    with no-op lifecycle.
//!
//! The workflow runs four states:
//!
//! ```text
//! Init  -> Process -> Persist -> Verify -> Done
//! ```
//!
//! - `Init`: read params, write a derived value into `MemoryStore`
//! - `Process`: combine the stored value with the counter's setup count
//! - `Persist`: write the final value into redb (ACID-committed transaction)
//! - `Verify`: open a redb read transaction and confirm the value round-trips
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_resources
//! ```
use cano::prelude::*;
use redb::{Database, ReadableDatabase, TableDefinition};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ============================================================================
// Resource key type
// ============================================================================

/// Typed key enum used to identify resources stored in `Resources<Key>`.
///
/// Using an enum instead of string literals gives compile-time safety —
/// a typo in a key is a compile error, not a runtime panic.
#[derive(Debug, Hash, Eq, PartialEq)]
enum Key {
    Store,
    Counter,
    Params,
    Db,
}

// ============================================================================
// Custom resource: CounterResource
// ============================================================================

/// A resource that tracks how many times its `setup` lifecycle hook has been
/// called. Uses interior mutability so it can be shared across clones.
struct CounterResource {
    setup_count: Arc<Mutex<u32>>,
}

impl CounterResource {
    fn new() -> Self {
        Self {
            setup_count: Arc::new(Mutex::new(0)),
        }
    }

    fn get_setup_count(&self) -> u32 {
        *self.setup_count.lock().unwrap()
    }
}

#[resource]
impl Resource for CounterResource {
    async fn setup(&self) -> Result<(), CanoError> {
        *self.setup_count.lock().unwrap() += 1;
        println!("CounterResource: setup called");
        Ok(())
    }

    async fn teardown(&self) -> Result<(), CanoError> {
        println!("CounterResource: teardown called");
        Ok(())
    }
}

// ============================================================================
// Custom resource: RedbResource (embedded ACID database)
// ============================================================================

/// Single redb table holding `result_name -> u32` rows.
const RESULTS_TABLE: TableDefinition<&str, u32> = TableDefinition::new("results");

/// A `Resource` that owns an open `redb::Database`.
///
/// `setup` opens (or creates) the database file and creates the `results`
/// table inside a write transaction so subsequent tasks can rely on the
/// table existing. `teardown` drops the database handle and removes the
/// file so the example leaves the temp dir clean.
struct RedbResource {
    path: PathBuf,
    db: Mutex<Option<Arc<Database>>>,
}

impl RedbResource {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            db: Mutex::new(None),
        }
    }

    /// Borrow the underlying database after `setup_all()` has run.
    fn db(&self) -> Result<Arc<Database>, CanoError> {
        self.db
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or_else(|| CanoError::Generic("redb database not initialized".into()))
    }
}

#[resource]
impl Resource for RedbResource {
    async fn setup(&self) -> Result<(), CanoError> {
        let db = Database::create(&self.path)
            .map_err(|e| CanoError::Generic(format!("redb create: {e}")))?;

        // Pre-create the table so write/read tasks don't have to.
        let write_tx = db
            .begin_write()
            .map_err(|e| CanoError::Generic(format!("redb begin_write: {e}")))?;
        {
            let _ = write_tx
                .open_table(RESULTS_TABLE)
                .map_err(|e| CanoError::Generic(format!("redb open_table: {e}")))?;
        }
        write_tx
            .commit()
            .map_err(|e| CanoError::Generic(format!("redb commit: {e}")))?;

        *self.db.lock().unwrap() = Some(Arc::new(db));
        println!("RedbResource: opened {}", self.path.display());
        Ok(())
    }

    async fn teardown(&self) -> Result<(), CanoError> {
        // Drop the database first so the file handle is released.
        *self.db.lock().unwrap() = None;
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
        println!("RedbResource: closed and removed {}", self.path.display());
        Ok(())
    }
}

// ============================================================================
// Workflow parameters resource
// ============================================================================

/// Plain workflow parameters passed as a resource. The default no-op
/// `Resource` impl is generated by `#[derive(Resource)]`.
#[derive(Resource)]
struct WorkflowParams {
    multiplier: u32,
}

// ============================================================================
// Workflow states
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Init,
    Process,
    Persist,
    Verify,
    Done,
}

// ============================================================================
// Tasks
// ============================================================================

/// Reads `WorkflowParams` and writes an initial value to the in-memory store.
struct InitTask;

#[task(state = Step, key = Key)]
impl InitTask {
    async fn run(&self, res: &Resources<Key>) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        let params = res.get::<WorkflowParams, _>(&Key::Params)?;

        let value = 10u32 * params.multiplier;
        store.put("value", value)?;
        println!("InitTask: stored value = {value}");

        Ok(TaskResult::Single(Step::Process))
    }
}

/// Reads the value written by `InitTask`, adds the counter's setup count,
/// and stores the combined result back into the in-memory store.
struct ProcessTask;

#[task(state = Step, key = Key)]
impl ProcessTask {
    async fn run(&self, res: &Resources<Key>) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        let counter = res.get::<CounterResource, _>(&Key::Counter)?;

        let value: u32 = store.get("value")?;
        let result = value + counter.get_setup_count();
        store.put("result", result)?;
        println!("ProcessTask: result = {result}");

        Ok(TaskResult::Single(Step::Persist))
    }
}

/// Persists the final result to redb inside an ACID write transaction.
struct PersistTask;

#[task(state = Step, key = Key)]
impl PersistTask {
    async fn run(&self, res: &Resources<Key>) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        let redb = res.get::<RedbResource, _>(&Key::Db)?;

        let result: u32 = store.get("result")?;
        let db = redb.db()?;

        let write_tx = db
            .begin_write()
            .map_err(|e| CanoError::Generic(format!("redb begin_write: {e}")))?;
        {
            let mut table = write_tx
                .open_table(RESULTS_TABLE)
                .map_err(|e| CanoError::Generic(format!("redb open_table: {e}")))?;
            table
                .insert("final_result", &result)
                .map_err(|e| CanoError::Generic(format!("redb insert: {e}")))?;
        }
        write_tx
            .commit()
            .map_err(|e| CanoError::Generic(format!("redb commit: {e}")))?;

        println!("PersistTask: committed final_result = {result} to redb");
        Ok(TaskResult::Single(Step::Verify))
    }
}

/// Reads the result back from redb in a read transaction and prints it.
/// Demonstrates that the previous write is durable across transactions.
struct VerifyTask;

#[task(state = Step, key = Key)]
impl VerifyTask {
    async fn run(&self, res: &Resources<Key>) -> Result<TaskResult<Step>, CanoError> {
        let redb = res.get::<RedbResource, _>(&Key::Db)?;
        let db = redb.db()?;

        let read_tx = db
            .begin_read()
            .map_err(|e| CanoError::Generic(format!("redb begin_read: {e}")))?;
        let table = read_tx
            .open_table(RESULTS_TABLE)
            .map_err(|e| CanoError::Generic(format!("redb open_table: {e}")))?;
        let value = table
            .get("final_result")
            .map_err(|e| CanoError::Generic(format!("redb get: {e}")))?
            .ok_or_else(|| CanoError::Generic("final_result missing from redb".into()))?
            .value();

        println!("VerifyTask: read back {value} from redb");
        Ok(TaskResult::Single(Step::Done))
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("Workflow Resources Example");
    println!("==========================");
    println!();

    let store = MemoryStore::new();
    let counter = CounterResource::new();

    // Keep a second handle to the same Arc so we can inspect it after the
    // workflow takes ownership of the primary `CounterResource`.
    let counter_clone = CounterResource {
        setup_count: Arc::clone(&counter.setup_count),
    };

    // Place the redb file in the OS temp dir so the example doesn't pollute
    // the working directory. Teardown removes the file.
    let db_path = std::env::temp_dir().join("cano_workflow_resources_example.redb");
    // If a previous run was interrupted, start clean.
    if db_path.exists() {
        let _ = std::fs::remove_file(&db_path);
    }

    let resources = Resources::<Key>::new()
        .insert(Key::Store, store.clone())
        .insert(Key::Counter, counter)
        .insert(Key::Params, WorkflowParams { multiplier: 3 })
        .insert(Key::Db, RedbResource::new(db_path));

    let workflow = Workflow::new(resources)
        .register(Step::Init, InitTask)
        .register(Step::Process, ProcessTask)
        .register(Step::Persist, PersistTask)
        .register(Step::Verify, VerifyTask)
        .add_exit_state(Step::Done);

    println!("Running workflow...");
    let final_state = workflow.orchestrate(Step::Init).await?;
    assert_eq!(final_state, Step::Done);

    let result: u32 = store.get("result")?;
    println!("In-memory result: {result}");

    // The orchestrator called setup() once before running tasks.
    assert_eq!(counter_clone.get_setup_count(), 1);
    println!(
        "CounterResource setup was called {} time(s)",
        counter_clone.get_setup_count()
    );

    println!();
    println!("Demonstrated:");
    println!("  - Enum keys for compile-time resource safety");
    println!("  - Custom Resource with setup/teardown lifecycle (CounterResource)");
    println!("  - Persistent ACID resource (RedbResource) with file lifecycle");
    println!("  - Plain config resource (WorkflowParams) with no-op lifecycle");
    println!("  - In-memory hand-off store (MemoryStore) shared across tasks");
    println!("  - Arc-based sharing for post-workflow inspection");

    Ok(())
}
