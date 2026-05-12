//! # Resources — Advanced Usage
//!
//! Run with: `cargo run --example resources_advanced`
//!
//! Three advanced `Resources` patterns:
//!
//! ## (a) Enum resource keys
//!
//! `Resources::<Key>::new()` parameterises the dictionary with a user-defined enum instead
//! of `&str`. Typos in key names become compile errors rather than runtime `ResourceNotFound`
//! panics.
//!
//! ## (b) `try_insert` duplicate detection
//!
//! [`Resources::insert`] panics on a duplicate key (intentional — duplicates are always a
//! programmer error in normal use). [`Resources::try_insert`] instead returns
//! [`CanoError::ResourceDuplicateKey`], which is useful when building a `Resources` from
//! dynamic / user-supplied configuration where duplicates are a plausible runtime condition.
//!
//! ## (c) Partial-LIFO rollback on `setup` failure
//!
//! [`Resources::setup_all`] calls `setup()` on resources in insertion order. If resource *N*
//! fails, it calls `teardown()` on resources *0..N−1* in **reverse** order (LIFO) before
//! returning the error. Resource *N* itself receives no teardown — it never finished setup.
//!
//! This example calls `setup_all()` directly (not via a workflow) so the rollback is easy
//! to inspect. A real workflow would call it internally via `orchestrate`.

use cano::prelude::*;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Part (a) — enum key type
// ---------------------------------------------------------------------------

/// Compile-time-checked resource keys.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
enum Key {
    Store,
    Cache,
    Config,
}

/// A minimal config resource with no lifecycle.
#[derive(Resource)]
struct AppConfig {
    max_connections: u32,
}

/// A trivial in-memory cache resource with no lifecycle.
#[derive(Resource)]
struct Cache {
    label: String,
}

// ---------------------------------------------------------------------------
// Part (b) — try_insert
// ---------------------------------------------------------------------------

/// A plain resource used to demonstrate duplicate-key detection.
#[derive(Resource)]
struct Counter {
    #[allow(dead_code)]
    value: u32,
}

// ---------------------------------------------------------------------------
// Part (c) — partial LIFO rollback
// ---------------------------------------------------------------------------

/// A resource that appends lifecycle events to a shared log so we can observe
/// the setup / teardown order from outside.
struct TrackedResource {
    name: String,
    log: Arc<Mutex<Vec<String>>>,
    fail_on_setup: bool,
}

impl TrackedResource {
    fn new(name: &str, log: Arc<Mutex<Vec<String>>>, fail_on_setup: bool) -> Self {
        Self {
            name: name.to_string(),
            log,
            fail_on_setup,
        }
    }
}

#[resource]
impl Resource for TrackedResource {
    async fn setup(&self) -> Result<(), CanoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("setup:{}", self.name));
        if self.fail_on_setup {
            return Err(CanoError::configuration(format!(
                "TrackedResource '{}' failed during setup",
                self.name
            )));
        }
        println!("    setup: {}", self.name);
        Ok(())
    }

    async fn teardown(&self) -> Result<(), CanoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("teardown:{}", self.name));
        println!("    teardown (rollback): {}", self.name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Resources Advanced Demo ===\n");

    // -----------------------------------------------------------------------
    // (a) Enum keys — compile-time safety
    // -----------------------------------------------------------------------
    println!("-- (a) Enum resource keys --");
    {
        let resources = Resources::<Key>::new()
            .insert(Key::Store, MemoryStore::new())
            .insert(Key::Cache, Cache { label: "L1".into() })
            .insert(
                Key::Config,
                AppConfig {
                    max_connections: 32,
                },
            );

        // `get` requires the right enum variant — a wrong variant is a compile error.
        let store = resources.get::<MemoryStore, _>(&Key::Store)?;
        let cache = resources.get::<Cache, _>(&Key::Cache)?;
        let config = resources.get::<AppConfig, _>(&Key::Config)?;

        store.put("ping", "pong".to_string())?;
        let pong: String = store.get("ping")?;

        println!("  Key::Store  -> MemoryStore, ping='{pong}'");
        println!("  Key::Cache  -> Cache(label='{}')", cache.label);
        println!(
            "  Key::Config -> AppConfig(max_connections={})\n",
            config.max_connections
        );
    }

    // -----------------------------------------------------------------------
    // (b) try_insert — ResourceDuplicateKey on repeated key
    // -----------------------------------------------------------------------
    println!("-- (b) try_insert duplicate detection --");
    {
        // First insertion succeeds.
        let resources: Resources = Resources::new().try_insert("counter", Counter { value: 1 })?;

        println!("  try_insert 'counter' (first)   -> Ok");

        // Second insertion with the same key returns Err.
        match resources.try_insert("counter", Counter { value: 2 }) {
            Ok(_) => println!("  try_insert 'counter' (second)  -> Ok  (unexpected!)"),
            Err(CanoError::ResourceDuplicateKey(msg)) => {
                println!("  try_insert 'counter' (second)  -> Err(ResourceDuplicateKey): {msg}");
            }
            Err(other) => {
                println!("  try_insert 'counter' (second)  -> Err (unexpected variant): {other}")
            }
        }
        println!();
    }

    // -----------------------------------------------------------------------
    // (c) Partial LIFO rollback when a resource setup fails
    //
    // Three resources: "alpha" (ok), "beta" (ok), "gamma" (fails on setup).
    // After setup_all returns Err:
    //   - "gamma" never called teardown (it never finished setup).
    //   - "beta"  teardown called (LIFO: last ok resource torn down first).
    //   - "alpha" teardown called (LIFO: first inserted, last torn down).
    // -----------------------------------------------------------------------
    println!("-- (c) setup_all partial LIFO rollback --");
    {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let resources: Resources = Resources::new()
            .insert(
                "alpha",
                TrackedResource::new("alpha", Arc::clone(&log), false),
            )
            .insert(
                "beta",
                TrackedResource::new("beta", Arc::clone(&log), false),
            )
            .insert(
                "gamma",
                TrackedResource::new("gamma", Arc::clone(&log), true),
            );

        println!("  calling setup_all() ...");
        match resources.setup_all().await {
            Ok(()) => println!("  setup_all -> Ok  (unexpected!)"),
            Err(e) => println!("  setup_all -> Err: {e}"),
        }

        let events = log.lock().unwrap().clone();
        println!("\n  lifecycle event log (in order):");
        for (i, ev) in events.iter().enumerate() {
            println!("    [{i}] {ev}");
        }

        // Verify the expected ordering: setup alpha, setup beta, setup gamma (fail),
        // teardown beta (LIFO), teardown alpha (LIFO). gamma gets no teardown.
        assert_eq!(
            events,
            vec![
                "setup:alpha",
                "setup:beta",
                "setup:gamma",   // gamma's setup runs, then fails
                "teardown:beta", // beta torn down first (LIFO)
                "teardown:alpha",
            ],
            "LIFO rollback order is wrong: {events:?}"
        );
        println!("\n  LIFO rollback confirmed:");
        println!("    - 'gamma' failed during setup -> no teardown for gamma");
        println!("    - 'beta'  torn down first (LIFO: last ok)");
        println!("    - 'alpha' torn down second (LIFO: first inserted)");
    }

    println!("\n=== Done ===");
    Ok(())
}
