//! UI tests for `#[derive(FromResources)]`.
//!
//! Covers single and multiple string-literal keys, enum-path keys, and
//! the `#[from_resources(key = MyType)]` override.

use cano::{MemoryStore, Resource, Resources};
use cano_macros::{FromResources, Resource as ResourceDerive};
use std::sync::Arc;

// ── Helper resource types ────────────────────────────────────────────────────

/// A second simple resource type for multi-field tests.
#[derive(ResourceDerive)]
struct Counter {
    #[allow(dead_code)]
    n: u32,
}

// ── Test 1: single string-literal-key field ─────────────────────────────────

#[derive(FromResources)]
struct SingleDep {
    #[res("store")]
    store: Arc<MemoryStore>,
}

#[tokio::test]
async fn single_string_key_resolves() {
    let res: Resources = Resources::new().insert("store", MemoryStore::new());
    let deps = SingleDep::from_resources(&res).expect("should resolve");
    // Confirm we got the right Arc by calling a method on it.
    deps.store.put("k", 1u32).expect("put should work");
}

// ── Test 2: multiple string-literal-key fields ───────────────────────────────

#[derive(FromResources)]
struct MultiDep {
    #[res("store")]
    store: Arc<MemoryStore>,
    #[res("counter")]
    counter: Arc<Counter>,
}

#[tokio::test]
async fn multiple_string_keys_resolve() {
    let res: Resources = Resources::new()
        .insert("store", MemoryStore::new())
        .insert("counter", Counter { n: 7 });
    let deps = MultiDep::from_resources(&res).expect("should resolve");
    deps.store.put("x", 42u32).expect("put should work");
    let _ = &deps.counter; // just access it
}

#[tokio::test]
async fn missing_key_returns_error() {
    // Use the default Resources (Cow<'static, str> key) to avoid ambiguity.
    let res: Resources = Resources::new(); // no "store" inserted
    let result = SingleDep::from_resources(&res);
    assert!(result.is_err(), "should fail when key is missing");
}

// ── Test 3: enum-path keys ───────────────────────────────────────────────────

#[derive(Debug, Hash, Eq, PartialEq)]
enum Key {
    Store,
    Counter,
}

#[derive(FromResources)]
struct EnumDep {
    #[res(Key::Store)]
    store: Arc<MemoryStore>,
    #[res(Key::Counter)]
    counter: Arc<Counter>,
}

#[tokio::test]
async fn enum_path_keys_resolve() {
    let res = Resources::<Key>::new()
        .insert(Key::Store, MemoryStore::new())
        .insert(Key::Counter, Counter { n: 3 });
    let deps = EnumDep::from_resources(&res).expect("should resolve");
    deps.store.put("y", 99u32).expect("put should work");
    let _ = &deps.counter;
}

// ── Test 4: key-type override with string keys ───────────────────────────────

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct MyKey(String);

impl std::borrow::Borrow<str> for MyKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl Resource for MyKey {}

#[derive(FromResources)]
#[from_resources(key = MyKey)]
struct OverrideDep {
    #[res("store")]
    store: Arc<MemoryStore>,
}

#[tokio::test]
async fn key_override_with_string_keys_resolves() {
    let res = Resources::<MyKey>::new().insert(MyKey("store".to_string()), MemoryStore::new());
    let deps = OverrideDep::from_resources(&res).expect("should resolve");
    deps.store.put("z", 5u32).expect("put should work");
}
