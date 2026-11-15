//! # Custom Store Backend and `get_shared`
//!
//! Run with: `cargo run --example store_custom_backend`
//!
//! Two related store demonstrations in one example:
//!
//! ## Part 1 — `NamespacedStore`
//!
//! A custom resource that wraps a [`MemoryStore`] and automatically prefixes every
//! key with a namespace string. Two tasks share the same underlying `MemoryStore`
//! but each sees only its own namespace, avoiding accidental key collisions without
//! requiring per-task key-naming conventions.
//!
//! ## Part 2 — `MemoryStore::get_shared` and `Arc::ptr_eq`
//!
//! [`MemoryStore::get_shared::<T>`] returns an `Arc<T>` pointing at the same
//! allocation that was stored with `put`. Cloning the `Arc` is O(1) — no copying
//! of the underlying value. Two `get_shared` calls on the same key return arcs that
//! are `ptr_eq` to each other, proving they reference the same heap allocation.

use cano::prelude::*;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Part 1: NamespacedStore resource
// ---------------------------------------------------------------------------

/// Wraps a [`MemoryStore`] and prefixes every key with `"{namespace}/"`.
///
/// Multiple `NamespacedStore` instances can share one `MemoryStore` without
/// key collisions as long as their namespaces differ.
struct NamespacedStore {
    namespace: String,
    inner: MemoryStore,
}

impl NamespacedStore {
    fn new(namespace: impl Into<String>, inner: MemoryStore) -> Self {
        Self {
            namespace: namespace.into(),
            inner,
        }
    }

    /// Store `value` under `"{namespace}/{key}"`.
    fn put<T: Send + Sync + 'static>(&self, key: &str, value: T) -> Result<(), CanoError> {
        let namespaced = format!("{}/{key}", self.namespace);
        self.inner
            .put(&namespaced, value)
            .map_err(|e| CanoError::store(format!("{e}")))
    }

    /// Retrieve a value stored under `"{namespace}/{key}"`.
    fn get<T: Clone + 'static>(&self, key: &str) -> Result<T, CanoError> {
        let namespaced = format!("{}/{key}", self.namespace);
        self.inner
            .get(&namespaced)
            .map_err(|e| CanoError::store(format!("{e}")))
    }
}

#[resource]
impl Resource for NamespacedStore {}

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    WriteA,
    WriteB,
    ReadBack,
    LoadLarge,
    SharedArc,
    Done,
}

// ---------------------------------------------------------------------------
// Part 1 tasks — use NamespacedStore
// ---------------------------------------------------------------------------

struct WriterA;

#[task(state = Step)]
impl WriterA {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<NamespacedStore, _>("ns_a")?;
        store.put("result", 100u32)?;
        store.put("label", "from-writer-a".to_string())?;
        println!("  WriterA: wrote result=100, label='from-writer-a' in namespace 'a'");
        Ok(TaskResult::Single(Step::WriteB))
    }
}

struct WriterB;

#[task(state = Step)]
impl WriterB {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<NamespacedStore, _>("ns_b")?;
        // Same key names as WriterA, but a different namespace — no collision.
        store.put("result", 200u32)?;
        store.put("label", "from-writer-b".to_string())?;
        println!("  WriterB: wrote result=200, label='from-writer-b' in namespace 'b'");
        Ok(TaskResult::Single(Step::ReadBack))
    }
}

struct ReadBack;

#[task(state = Step)]
impl ReadBack {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let ns_a = res.get::<NamespacedStore, _>("ns_a")?;
        let ns_b = res.get::<NamespacedStore, _>("ns_b")?;

        let a_result: u32 = ns_a.get("result")?;
        let a_label: String = ns_a.get("label")?;
        let b_result: u32 = ns_b.get("result")?;
        let b_label: String = ns_b.get("label")?;

        println!("  ReadBack: namespace 'a' -> result={a_result}, label='{a_label}'");
        println!("  ReadBack: namespace 'b' -> result={b_result}, label='{b_label}'");

        assert_eq!(a_result, 100, "namespace 'a' must be independent of 'b'");
        assert_eq!(b_result, 200, "namespace 'b' must be independent of 'a'");
        assert_ne!(a_result, b_result, "no cross-namespace collision");
        println!("  ReadBack: namespaces are isolated — no key collisions");

        Ok(TaskResult::Single(Step::LoadLarge))
    }
}

// ---------------------------------------------------------------------------
// Part 2: large value stored once, Arc-shared between tasks
// ---------------------------------------------------------------------------

/// A large payload — in a real system this might be a parsed config, a model
/// blob, or a pre-computed lookup table. We store it once and share it cheaply.
struct LargePayload {
    data: Vec<u8>,
}

impl LargePayload {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0xAB; size],
        }
    }
}

/// Stores a `LargePayload` in the `MemoryStore` via `put`.
struct LoadLarge;

#[task(state = Step)]
impl LoadLarge {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, _>("mem")?;
        let payload = LargePayload::new(1_000_000); // 1 MiB
        store.put("payload", payload)?;
        println!("  LoadLarge: stored 1 MiB payload");
        Ok(TaskResult::Single(Step::SharedArc))
    }
}

/// Retrieves the payload twice with `get_shared` and proves both calls return
/// arcs pointing at the same allocation — no data was copied.
struct SharedArc;

#[task(state = Step)]
impl SharedArc {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let store = res.get::<MemoryStore, _>("mem")?;

        // First Arc — points at the stored allocation.
        let arc1: Arc<LargePayload> = store.get_shared("payload")?;
        // Second Arc — must be ptr_eq to arc1: no copy, same heap address.
        let arc2: Arc<LargePayload> = store.get_shared("payload")?;

        println!("  SharedArc: arc1 data length = {} bytes", arc1.data.len());
        println!("  SharedArc: arc2 data length = {} bytes", arc2.data.len());
        assert!(
            Arc::ptr_eq(&arc1, &arc2),
            "get_shared must return arcs pointing at the same allocation"
        );
        println!("  SharedArc: Arc::ptr_eq(&arc1, &arc2) == true — zero-copy sharing confirmed");

        // The `Arc` clone is O(1): only the reference count changes, not the data.
        let arc3 = Arc::clone(&arc1);
        assert!(Arc::ptr_eq(&arc1, &arc3));
        println!("  SharedArc: Arc::clone is also O(1) and ptr_eq");

        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("=== Custom Store Backend and get_shared Demo ===\n");

    // Shared underlying store — both namespaced views point at it.
    let shared_mem = MemoryStore::new();

    let resources = Resources::new()
        .insert("mem", shared_mem.clone())
        .insert("ns_a", NamespacedStore::new("a", shared_mem.clone()))
        .insert("ns_b", NamespacedStore::new("b", shared_mem.clone()));

    println!("-- Part 1: NamespacedStore (key isolation) --");
    let workflow = Workflow::new(resources)
        .register(Step::WriteA, WriterA)
        .register(Step::WriteB, WriterB)
        .register(Step::ReadBack, ReadBack)
        .register(Step::LoadLarge, LoadLarge)
        .register(Step::SharedArc, SharedArc)
        .add_exit_state(Step::Done);

    println!("\n-- Part 2: MemoryStore::get_shared (Arc zero-copy sharing) --");
    let result = workflow.orchestrate(Step::WriteA).await?;
    println!("\ncompleted at {result:?}");

    println!("\n=== Done ===");
    Ok(())
}
