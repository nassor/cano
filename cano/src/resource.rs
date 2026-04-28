//! # Resources — Unified Lifecycle-Managed Dependencies
//!
//! This module provides the [`Resource`] trait and the [`Resources`] container, which together
//! replace the `TStore` + `TParams` dual-generic pattern used in earlier versions of Cano. All
//! external dependencies a workflow needs — database pools, HTTP clients, configuration objects,
//! caches — are registered once into a [`Resources`] map and then injected into every task at
//! dispatch time.
//!
//! ## Design Rationale
//!
//! The old `TStore`/`TParams` split forced callers to carry two separate generics and to manually
//! thread shared state between tasks. `Resources` unifies that into a single typed map:
//!
//! - **One place to register** — call `.insert()` once at startup.
//! - **Type-safe retrieval** — [`Resources::get`] downcasts to the concrete type you request; a
//!   wrong type is a compile-time or runtime error, never silent corruption.
//! - **Lifecycle management** — every `Resource` has optional `setup` / `teardown` hooks that the
//!   engine calls automatically.
//!
//! ## Lifecycle Guarantees
//!
//! 1. `setup` is called in **insertion order** (FIFO). Dependencies that must start before
//!    others should be inserted first.
//! 2. `teardown` is called in **reverse insertion order** (LIFO). Resources are torn down from
//!    most-recently-inserted to least-recently-inserted, which is the natural reverse of
//!    initialization.
//! 3. Both `setup` and `teardown` calls are **sequential**, not concurrent. This avoids
//!    ordering races between resources that depend on each other.
//! 4. If `setup` fails at position *N*, `teardown` is run LIFO from position *N−1* down to
//!    position 0. Resources at positions ≥ *N* never had their `setup` called and are therefore
//!    never torn down.
//! 5. `teardown` is called **only** on resources whose `setup` returned `Ok`. A resource whose
//!    `setup` returned `Err` is considered uninitialized and receives no teardown.
//! 6. `teardown` errors are **logged but never abort** the teardown sequence. All remaining
//!    resources continue to be torn down even if one fails.
//!
//! ## Concurrency Model
//!
//! [`Resources::get`] returns an `Arc<R>`. Split tasks that run in parallel each receive their own
//! `Arc` clone, which is cheap (atomic reference count increment). Resources that need mutable
//! internal state must use interior mutability (`Mutex`, `RwLock`, `DashMap`, etc.). A coarse
//! `Mutex<T>` inside a resource serializes concurrent split tasks on that resource; a fine-grained
//! `RwLock` allows concurrent reads with exclusive writes.
//!
//! ## Key Types
//!
//! By default `TResourceKey = Cow<'static, str>`, which lets you insert `&'static str` keys
//! without allocating (they wrap as `Cow::Borrowed`) while still accepting owned `String`
//! keys (`Cow::Owned`) for runtime-built names. Lookups use `&str` via the `Borrow<str>`
//! blanket impl, so the hot path stays allocation-free:
//!
//! ```rust
//! use cano::resource::{Resource, Resources};
//!
//! struct MyService;
//!
//! #[cano::resource]
//! impl Resource for MyService {}
//!
//! // "my_service" stays borrowed — no allocation on insert.
//! let resources: Resources = Resources::new().insert("my_service", MyService);
//!
//! // &str lookup works because Cow<'_, str>: Borrow<str>.
//! let _svc: std::sync::Arc<MyService> = resources.get("my_service").unwrap();
//! ```
//!
//! For stateless resources (no `setup` / `teardown` logic needed), use the
//! `#[derive(Resource)]` shortcut to skip the empty impl block entirely:
//!
//! ```rust
//! use cano::Resource;
//!
//! #[derive(Resource)]
//! struct Settings { batch: usize }
//! ```
//!
//! To pull several resources out at once with one call, define a deps struct
//! decorated with `#[derive(FromResources)]`. Each field is tagged with the
//! resource key and must be `Arc<T>` (matching `Resources::get`'s return shape):
//!
//! ```ignore
//! use cano::prelude::*;
//! use std::sync::Arc;
//!
//! #[derive(FromResources)]
//! struct PrepDeps {
//!     #[res("settings")]
//!     settings: Arc<Settings>,
//!     #[res("store")]
//!     store: Arc<MemoryStore>,
//! }
//!
//! // ... inside a Node::prep:
//! let PrepDeps { settings, store } = PrepDeps::from_resources(res)?;
//! ```
//!
//! For larger codebases, enum keys eliminate typos at compile time:
//!
//! ```rust
//! use cano::resource::{Resource, Resources};
//!
//! #[derive(Hash, PartialEq, Eq)]
//! enum Key { Db, Cache }
//!
//! struct DbPool;
//! struct CacheClient;
//!
//! #[cano::resource]
//! impl Resource for DbPool {}
//!
//! #[cano::resource]
//! impl Resource for CacheClient {}
//!
//! let resources: Resources<Key> = Resources::new()
//!     .insert(Key::Db,    DbPool)
//!     .insert(Key::Cache, CacheClient);
//!
//! let _db: std::sync::Arc<DbPool> = resources.get(&Key::Db).unwrap();
//! ```
//!
//! ## Sharing Resources Across Tasks
//!
//! `Resources` is **not** `Clone` — `Box<dyn Any>` is not cloneable. To share a `Resources`
//! instance between concurrent parts of your system (e.g., a scheduler running multiple
//! workflows), wrap it in an `Arc`:
//!
//! ```rust
//! use std::sync::Arc;
//! use cano::resource::{Resource, Resources};
//!
//! struct Noop;
//!
//! #[cano::resource]
//! impl Resource for Noop {}
//!
//! let shared: Arc<Resources> = Arc::new(Resources::new().insert("noop", Noop));
//! let clone = Arc::clone(&shared);
//! let _ = clone.get::<Noop, str>("noop").unwrap();
//! ```

use crate::error::CanoError;
use cano_macros::resource;
use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::ops::Range;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Resource trait
// ---------------------------------------------------------------------------

/// A lifecycle-managed dependency that can be set up and torn down by the engine.
///
/// Implement this trait for any external resource your workflow depends on —
/// database connection pools, HTTP clients, message-queue consumers, etc.
///
/// Both methods have default implementations that do nothing, so you only need
/// to override the phases that matter for your resource.
///
/// # Examples
///
/// ```rust
/// use cano::resource::Resource;
/// use cano::error::CanoError;
///
/// struct HttpClient {
///     base_url: String,
/// }
///
/// #[cano::resource]
/// impl Resource for HttpClient {
///     async fn setup(&self) -> Result<(), CanoError> {
///         // verify connectivity, warm connection pool, etc.
///         Ok(())
///     }
///
///     async fn teardown(&self) -> Result<(), CanoError> {
///         // drain pending requests, close sockets, etc.
///         Ok(())
///     }
/// }
/// ```
#[resource]
pub trait Resource: Send + Sync + 'static {
    /// Called once before the workflow starts. Use it to open connections, validate
    /// configuration, or perform any other one-time initialization.
    ///
    /// The default implementation does nothing and returns `Ok(())`.
    async fn setup(&self) -> Result<(), CanoError> {
        Ok(())
    }

    /// Called once after the workflow finishes (or after a failed setup rollback).
    /// Use it to close connections, flush buffers, or release OS resources.
    ///
    /// The default implementation does nothing and returns `Ok(())`.
    async fn teardown(&self) -> Result<(), CanoError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Resources container
// ---------------------------------------------------------------------------

/// A typed map of lifecycle-managed resources.
///
/// Resources are inserted with [`insert`](Resources::insert) and retrieved with
/// [`get`](Resources::get). The engine calls [`setup_all`](Resources::setup_all)
/// before executing the workflow and handles teardown on completion or failure.
///
/// See the [module documentation](self) for lifecycle guarantees, concurrency notes,
/// and examples of enum vs. string keys.
pub struct Resources<TResourceKey = Cow<'static, str>>
where
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Type-erased resource values, keyed for fast lookup.
    data: HashMap<TResourceKey, Box<dyn Any + Send + Sync>>,
    /// Ordered list of resources for FIFO setup / LIFO teardown.
    lifecycle: Vec<Arc<dyn Resource>>,
}

impl<TResourceKey: Hash + Eq + Send + Sync + 'static> Resources<TResourceKey> {
    /// Create a new, empty `Resources` map.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use cano::resource::Resources;
    ///
    /// // Default key type is `Cow<'static, str>` — accepts both literals and owned strings.
    /// let resources: Resources = Resources::new();
    /// # let _ = resources;
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
            lifecycle: Vec::new(),
        }
    }

    /// Return an empty resource map.
    ///
    /// Equivalent to [`new`](Self::new). Use this when you want to be explicit
    /// that the workflow intentionally has no external dependencies.
    ///
    /// # Example
    ///
    /// ```rust
    /// use cano::prelude::*;
    /// # #[derive(Clone, Debug, PartialEq, Eq, Hash)] enum S { Done }
    /// let wf: Workflow<S> = Workflow::new(Resources::empty()).add_exit_state(S::Done);
    /// ```
    #[must_use]
    pub fn empty() -> Self {
        Self::new()
    }

    /// Create a new `Resources` map with preallocated capacity for `n` entries.
    ///
    /// Equivalent to [`new`](Self::new) but avoids reallocations when you know
    /// the number of resources up front.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use cano::resource::Resources;
    ///
    /// let resources: Resources = Resources::with_capacity(8);
    /// # let _ = resources;
    /// ```
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            data: HashMap::with_capacity(n),
            lifecycle: Vec::with_capacity(n),
        }
    }

    /// Insert a resource into the map, returning `self` for builder-style chaining.
    ///
    /// The resource is stored under `key` and added to the lifecycle list in insertion order.
    ///
    /// # Panics
    ///
    /// Panics if `key` is already present. Builder-style insertion is a one-shot wiring
    /// step; a duplicate key is a programmer error caught at startup, before any task runs.
    /// If you need to handle duplicates as data, use [`try_insert`](Self::try_insert), which
    /// returns a [`CanoError::ResourceDuplicateKey`] instead of panicking.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use cano::resource::{Resource, Resources};
    ///
    /// struct Counter;
    ///
    /// #[cano::resource]
    /// impl Resource for Counter {}
    ///
    /// let resources: Resources = Resources::new().insert("counter", Counter);
    /// # let _ = resources;
    /// ```
    #[must_use]
    pub fn insert<R: Resource + 'static>(
        mut self,
        key: impl Into<TResourceKey>,
        resource: R,
    ) -> Self {
        let arc = Arc::new(resource);
        let key = key.into();
        assert!(
            !self.data.contains_key(&key),
            "duplicate resource key inserted into Resources; use try_insert to handle duplicates as Result"
        );
        self.data.insert(key, Box::new(Arc::clone(&arc)));
        self.lifecycle.push(arc as Arc<dyn Resource>);
        self
    }

    /// Insert a resource into the map, returning the updated `Resources` or an error.
    ///
    /// Same as [`insert`](Self::insert) but returns a [`CanoError::ResourceDuplicateKey`]
    /// instead of panicking when `key` is already present. Use this when duplicates are a
    /// possible runtime condition you want to handle (e.g. building `Resources` from
    /// dynamic input) rather than a programmer error.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError::ResourceDuplicateKey`] if `key` is already present.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use cano::resource::{Resource, Resources};
    ///
    /// struct Counter;
    ///
    /// #[cano::resource]
    /// impl Resource for Counter {}
    ///
    /// # fn main() -> Result<(), cano::error::CanoError> {
    /// let resources: Resources = Resources::new().try_insert("counter", Counter)?;
    /// # let _ = resources;
    /// # Ok(()) }
    /// ```
    pub fn try_insert<R: Resource + 'static>(
        mut self,
        key: impl Into<TResourceKey>,
        resource: R,
    ) -> Result<Self, CanoError> {
        let arc = Arc::new(resource);
        let key = key.into();
        if self.data.contains_key(&key) {
            return Err(CanoError::resource_duplicate_key(
                "duplicate resource key inserted into Resources",
            ));
        }
        self.data.insert(key, Box::new(Arc::clone(&arc)));
        self.lifecycle.push(arc as Arc<dyn Resource>);
        Ok(self)
    }

    /// Retrieve a resource by key, returning an `Arc<R>`.
    ///
    /// # Errors
    ///
    /// - [`CanoError::ResourceNotFound`] — no entry exists for `key`.
    /// - [`CanoError::ResourceTypeMismatch`] — an entry exists but was stored as a different type.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use cano::resource::{Resource, Resources};
    ///
    /// struct Pool;
    ///
    /// #[cano::resource]
    /// impl Resource for Pool {}
    ///
    /// let resources: Resources = Resources::new().insert("pool", Pool);
    /// let pool: std::sync::Arc<Pool> = resources.get("pool").unwrap();
    /// ```
    pub fn get<R, Q>(&self, key: &Q) -> Result<Arc<R>, CanoError>
    where
        R: Resource + 'static,
        TResourceKey: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let boxed = self.data.get(key).ok_or_else(|| {
            CanoError::ResourceNotFound(format!(
                "no resource found for the given key (requested type: {})",
                std::any::type_name::<R>(),
            ))
        })?;
        boxed
            .downcast_ref::<Arc<R>>()
            .map(Arc::clone)
            .ok_or_else(|| {
                CanoError::ResourceTypeMismatch(format!(
                    "resource found but the requested type {} does not match the stored type",
                    std::any::type_name::<R>(),
                ))
            })
    }

    /// Call `setup` on every resource in insertion order.
    ///
    /// If any `setup` call fails, `teardown` is called LIFO on all previously
    /// successful resources before the error is returned. Resources at position ≥ N
    /// (where N is the failing index) are never touched.
    pub async fn setup_all(&self) -> Result<(), CanoError> {
        if self.lifecycle.is_empty() {
            return Ok(());
        }
        for (idx, resource) in self.lifecycle.iter().enumerate() {
            if let Err(e) = resource.setup().await {
                self.teardown_range(0..idx).await;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Call `teardown` on `lifecycle[range]` in **reverse** (LIFO) order.
    ///
    /// Errors from individual teardowns are logged (or printed to stderr when the
    /// `tracing` feature is disabled) but never abort the sequence.
    pub async fn teardown_all(&self) {
        self.teardown_range(0..self.lifecycle.len()).await;
    }

    pub(crate) async fn teardown_range(&self, range: Range<usize>) {
        for resource in self.lifecycle[range].iter().rev() {
            if let Err(e) = resource.teardown().await {
                #[cfg(feature = "tracing")]
                tracing::error!("resource teardown failed: {e}");
                #[cfg(not(feature = "tracing"))]
                eprintln!("cano: resource teardown error: {e}");
            }
        }
    }

    /// Return the number of resources in the lifecycle list.
    ///
    /// Internal helper used by the workflow and scheduler engines together with
    /// [`teardown_range`](Self::teardown_range) when partial-rollback teardown is
    /// needed. Public callers should use [`teardown_all`](Self::teardown_all).
    pub(crate) fn lifecycle_len(&self) -> usize {
        self.lifecycle.len()
    }
}

impl<TResourceKey: Hash + Eq + Send + Sync + 'static> Default for Resources<TResourceKey> {
    fn default() -> Self {
        Self::new()
    }
}

impl<TResourceKey: fmt::Debug + Hash + Eq + Send + Sync + 'static> fmt::Debug
    for Resources<TResourceKey>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resources")
            .field("count", &self.lifecycle.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ------------------------------------------------------------------
    // Simple zero-size types for type-mismatch tests
    // ------------------------------------------------------------------

    #[derive(Debug)]
    struct TypeA;
    #[resource]
    impl Resource for TypeA {}

    #[derive(Debug)]
    struct TypeB;
    #[resource]
    impl Resource for TypeB {}

    // ------------------------------------------------------------------
    // Tracking resource for lifecycle-order tests
    // ------------------------------------------------------------------

    struct TrackingResource {
        log: Arc<Mutex<Vec<String>>>,
        name: String,
    }

    impl TrackingResource {
        fn new(name: impl Into<String>, log: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                log,
                name: name.into(),
            }
        }
    }

    #[resource]
    impl Resource for TrackingResource {
        async fn setup(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("setup:{}", self.name));
            Ok(())
        }

        async fn teardown(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("teardown:{}", self.name));
            Ok(())
        }
    }

    // Tracking resource that fails on setup
    struct FailingResource {
        log: Arc<Mutex<Vec<String>>>,
        name: String,
    }

    impl FailingResource {
        fn new(name: impl Into<String>, log: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                log,
                name: name.into(),
            }
        }
    }

    #[resource]
    impl Resource for FailingResource {
        async fn setup(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("setup:{}", self.name));
            Err(CanoError::generic(format!(
                "setup failed for {}",
                self.name
            )))
        }

        async fn teardown(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("teardown:{}", self.name));
            Ok(())
        }
    }

    // Tracking resource that fails on teardown
    struct FailingTeardownResource {
        log: Arc<Mutex<Vec<String>>>,
        name: String,
    }

    impl FailingTeardownResource {
        fn new(name: impl Into<String>, log: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                log,
                name: name.into(),
            }
        }
    }

    #[resource]
    impl Resource for FailingTeardownResource {
        async fn setup(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("setup:{}", self.name));
            Ok(())
        }

        async fn teardown(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("teardown:{}", self.name));
            Err(CanoError::generic(format!(
                "teardown failed for {}",
                self.name
            )))
        }
    }

    // ------------------------------------------------------------------
    // 0. EMPTY constant is an empty map
    // ------------------------------------------------------------------

    #[test]
    fn test_empty_is_empty() {
        let r = Resources::<String>::empty();
        assert_eq!(
            r.lifecycle.len(),
            0,
            "Resources::empty() should have no lifecycle entries"
        );
        assert_eq!(
            r.data.len(),
            0,
            "Resources::empty() should have no data entries"
        );
    }

    // ------------------------------------------------------------------
    // 1. insert + get round-trip with string key
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_insert_get_roundtrip_string_key() {
        let resources: Resources<String> = Resources::new().insert("a".to_string(), TypeA);
        let result = resources.get::<TypeA, str>("a");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    // ------------------------------------------------------------------
    // 2. get missing key returns ResourceNotFound
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_missing_key() {
        let resources: Resources<String> = Resources::new();
        let result = resources.get::<TypeA, str>("absent");

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.category(), "resource_not_found");
    }

    // ------------------------------------------------------------------
    // 3. get with wrong type returns ResourceTypeMismatch, distinct from missing
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_wrong_type() {
        let resources: Resources<String> = Resources::new().insert("a".to_string(), TypeA);

        // Correct type — should succeed
        assert!(resources.get::<TypeA, str>("a").is_ok());

        // Wrong type — should be ResourceTypeMismatch, not ResourceNotFound
        let err = resources.get::<TypeB, str>("a").unwrap_err();
        assert_eq!(err.category(), "resource_type_mismatch");

        // Missing key — should be ResourceNotFound, confirming the two are distinct
        let missing_err = resources.get::<TypeA, str>("missing").unwrap_err();
        assert_eq!(missing_err.category(), "resource_not_found");

        assert_ne!(err.category(), missing_err.category());
    }

    // ------------------------------------------------------------------
    // 4. two get() calls on the same key return ptr_eq Arcs
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_arc_identity() {
        let resources: Resources<String> = Resources::new().insert("a".to_string(), TypeA);

        let arc1 = resources.get::<TypeA, str>("a").unwrap();
        let arc2 = resources.get::<TypeA, str>("a").unwrap();

        assert!(Arc::ptr_eq(&arc1, &arc2), "expected same underlying Arc");
    }

    // ------------------------------------------------------------------
    // 5. default() produces the same empty state as new()
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_default_equals_new() {
        let by_default: Resources<String> = Resources::default();
        let by_new: Resources<String> = Resources::new();

        assert_eq!(by_default.data.len(), by_new.data.len());
        assert_eq!(by_default.lifecycle.len(), by_new.lifecycle.len());
    }

    // ------------------------------------------------------------------
    // 6. with_capacity does not panic
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_with_capacity() {
        let resources: Resources<String> = Resources::with_capacity(64);
        assert_eq!(resources.lifecycle.len(), 0);
        assert_eq!(resources.data.len(), 0);
    }

    // ------------------------------------------------------------------
    // 7. setup is called in insertion (FIFO) order
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_lifecycle_setup_insertion_order() {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let resources: Resources<String> = Resources::new()
            .insert(
                "a".to_string(),
                TrackingResource::new("A", Arc::clone(&log)),
            )
            .insert(
                "b".to_string(),
                TrackingResource::new("B", Arc::clone(&log)),
            )
            .insert(
                "c".to_string(),
                TrackingResource::new("C", Arc::clone(&log)),
            );

        resources.setup_all().await.unwrap();

        let events = log.lock().unwrap().clone();
        assert_eq!(events, ["setup:A", "setup:B", "setup:C"]);
    }

    // ------------------------------------------------------------------
    // 8. teardown is called in reverse insertion (LIFO) order
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_lifecycle_teardown_lifo_order() {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let resources: Resources<String> = Resources::new()
            .insert(
                "a".to_string(),
                TrackingResource::new("A", Arc::clone(&log)),
            )
            .insert(
                "b".to_string(),
                TrackingResource::new("B", Arc::clone(&log)),
            )
            .insert(
                "c".to_string(),
                TrackingResource::new("C", Arc::clone(&log)),
            );

        // Setup all first (FIFO), then teardown all (LIFO)
        resources.setup_all().await.unwrap();

        // Clear log so we only observe teardown order
        log.lock().unwrap().clear();

        resources.teardown_range(0..resources.lifecycle.len()).await;

        let events = log.lock().unwrap().clone();
        assert_eq!(events, ["teardown:C", "teardown:B", "teardown:A"]);
    }

    // ------------------------------------------------------------------
    // 9. partial rollback: failure at position 2 of 4 triggers teardown of 0..1 LIFO
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_setup_failure_partial_rollback() {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // Position 0: ok, Position 1: ok, Position 2: fails, Position 3: never reached
        let resources: Resources<String> = Resources::new()
            .insert(
                "r0".to_string(),
                TrackingResource::new("R0", Arc::clone(&log)),
            )
            .insert(
                "r1".to_string(),
                TrackingResource::new("R1", Arc::clone(&log)),
            )
            .insert(
                "r2".to_string(),
                FailingResource::new("R2", Arc::clone(&log)),
            )
            .insert(
                "r3".to_string(),
                TrackingResource::new("R3", Arc::clone(&log)),
            );

        let result = resources.setup_all().await;
        assert!(result.is_err(), "expected setup_all to fail");

        let events = log.lock().unwrap().clone();

        // setup:R0, setup:R1, setup:R2 (fails), teardown:R1, teardown:R0
        // R3 must never appear; teardown order is LIFO of successfully-setup resources
        assert_eq!(
            events,
            [
                "setup:R0",
                "setup:R1",
                "setup:R2",
                "teardown:R1",
                "teardown:R0"
            ],
            "unexpected lifecycle events: {events:?}"
        );
    }

    // ------------------------------------------------------------------
    // 10. teardown continues past individual failures — all N resources get called
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_teardown_continues_past_failures() {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // All three resources fail their teardown
        let resources: Resources<String> = Resources::new()
            .insert(
                "r0".to_string(),
                FailingTeardownResource::new("R0", Arc::clone(&log)),
            )
            .insert(
                "r1".to_string(),
                FailingTeardownResource::new("R1", Arc::clone(&log)),
            )
            .insert(
                "r2".to_string(),
                FailingTeardownResource::new("R2", Arc::clone(&log)),
            );

        // teardown_range should not return an error or panic even when all fail
        resources.teardown_range(0..resources.lifecycle.len()).await;

        let events = log.lock().unwrap().clone();
        // All three must have been called despite failures; LIFO order
        assert_eq!(events, ["teardown:R2", "teardown:R1", "teardown:R0"]);
    }

    // ------------------------------------------------------------------
    // 11. insert panics on duplicate key (programmer error)
    // ------------------------------------------------------------------

    #[test]
    #[should_panic(expected = "duplicate resource key inserted into Resources")]
    fn test_insert_panics_on_duplicate() {
        let _resources: Resources<String> = Resources::new()
            .insert("dup".to_string(), TypeA)
            .insert("dup".to_string(), TypeA);
    }

    // ------------------------------------------------------------------
    // 12. try_insert returns ResourceDuplicateKey on duplicate (no panic)
    // ------------------------------------------------------------------

    #[test]
    fn test_try_insert_returns_duplicate_key_error() {
        let first = Resources::<String>::new().insert("dup".to_string(), TypeA);
        let err = first
            .try_insert("dup".to_string(), TypeA)
            .expect_err("try_insert must reject duplicates");
        assert_eq!(err.category(), "resource_duplicate_key");
        assert!(err.message().contains("duplicate"));
    }

    // ------------------------------------------------------------------
    // 13. try_insert succeeds for fresh keys and chains with insert
    // ------------------------------------------------------------------

    #[test]
    fn test_try_insert_succeeds_for_fresh_key() {
        let resources = Resources::<String>::new()
            .insert("a".to_string(), TypeA)
            .try_insert("b".to_string(), TypeB)
            .expect("fresh key must insert");
        assert!(resources.get::<TypeA, str>("a").is_ok());
        assert!(resources.get::<TypeB, str>("b").is_ok());
    }
}
