//! # Resources — Advanced Usage
//!
//! Run with: `cargo run --example resources_advanced`
//!
//! Three advanced `Resources` patterns:
//!
//! ## (a) Type-keyed lookups, named second instances
//!
//! [`Resources::get`] finds a resource by its type, so there is no key string to mistype.
//! A second instance of a type already present goes in with [`Resources::insert_named`]
//! and comes back out with [`Resources::get_named`].
//!
//! ## (b) `try_insert` duplicate detection
//!
//! [`Resources::insert`] panics on a duplicate type (intentional — duplicates are always a
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
// Part (a) — type-keyed and named lookups
// ---------------------------------------------------------------------------

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
    // (a) Type-keyed lookups, plus a named second instance
    // -----------------------------------------------------------------------
    println!("-- (a) Type-keyed and named lookups --");
    {
        let resources = Resources::new()
            .insert(MemoryStore::new())
            .insert(Cache { label: "L1".into() })
            .insert(AppConfig {
                max_connections: 32,
            })
            .insert_named("l2", Cache { label: "L2".into() });

        // `get` looks a resource up by its type; no key string to mistype.
        let store = resources.get::<MemoryStore>()?;
        let cache = resources.get::<Cache>()?;
        let config = resources.get::<AppConfig>()?;
        // A second `Cache` needs a name.
        let l2 = resources.get_named::<Cache>("l2")?;

        store.put("ping", "pong".to_string());
        let pong: String = store.get("ping")?;

        println!("  MemoryStore        -> ping='{pong}'");
        println!("  Cache              -> label='{}'", cache.label);
        println!("  Cache named \"l2\"   -> label='{}'", l2.label);
        println!(
            "  AppConfig          -> max_connections={}\n",
            config.max_connections
        );
    }

    // -----------------------------------------------------------------------
    // (b) try_insert — ResourceDuplicateKey on a repeated type
    // -----------------------------------------------------------------------
    println!("-- (b) try_insert duplicate detection --");
    {
        // First insertion succeeds.
        let resources: Resources = Resources::new().try_insert(Counter { value: 1 })?;

        println!("  try_insert Counter (first)   -> Ok");

        // A second `Counter` without a name returns Err.
        match resources.try_insert(Counter { value: 2 }) {
            Ok(_) => println!("  try_insert Counter (second)  -> Ok  (unexpected!)"),
            Err(CanoError::ResourceDuplicateKey(msg)) => {
                println!("  try_insert Counter (second)  -> Err(ResourceDuplicateKey): {msg}");
            }
            Err(other) => {
                println!("  try_insert Counter (second)  -> Err (unexpected variant): {other}")
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

        // Three resources of one type: each needs its own name.
        let resources: Resources = Resources::new()
            .insert_named(
                "alpha",
                TrackedResource::new("alpha", Arc::clone(&log), false),
            )
            .insert_named(
                "beta",
                TrackedResource::new("beta", Arc::clone(&log), false),
            )
            .insert_named(
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
