//! UI tests for `#[derive(Resource)]`.
//!
//! Verifies that the derive macro correctly generates an empty `Resource` impl
//! for zero-sized structs, structs with fields, and generic structs.
//!
//! The `cano_macros::Resource` derive is used directly here to avoid the import
//! ambiguity that arises when `cano::Resource` (re-exporting the derive) and
//! `cano::Resource` (the trait) collide in the macro namespace.

// Import the trait for bounds and method calls.
use cano::Resource;

// ── ZST struct ──────────────────────────────────────────────────────────────

#[derive(cano_macros::Resource)]
struct Empty;

// ── Struct with fields ───────────────────────────────────────────────────────

#[derive(cano_macros::Resource)]
struct Cfg {
    #[allow(dead_code)]
    x: u32,
    #[allow(dead_code)]
    name: String,
}

// ── Generic struct ───────────────────────────────────────────────────────────

#[derive(cano_macros::Resource)]
struct Cell<T: Send + Sync + 'static> {
    #[allow(dead_code)]
    v: T,
}

// ── Verify trait is implemented ──────────────────────────────────────────────

fn assert_resource<R: Resource>() {}

#[test]
fn empty_implements_resource() {
    assert_resource::<Empty>();
}

#[test]
fn cfg_implements_resource() {
    assert_resource::<Cfg>();
}

#[test]
fn cell_implements_resource() {
    assert_resource::<Cell<u32>>();
    assert_resource::<Cell<String>>();
}

// ── Verify the generated impl is usable at runtime ───────────────────────────

#[tokio::test]
async fn resource_setup_teardown_are_noop() {
    let e = Empty;
    e.setup().await.expect("Empty setup should succeed");
    e.teardown().await.expect("Empty teardown should succeed");
}

#[tokio::test]
async fn cfg_resource_lifecycle() {
    let cfg = Cfg {
        x: 42,
        name: "test".to_string(),
    };
    cfg.setup().await.expect("Cfg setup should succeed");
    cfg.teardown().await.expect("Cfg teardown should succeed");
}
