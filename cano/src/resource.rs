//! # Resources — Unified Lifecycle-Managed Dependencies
//!
//! This module provides the [`Resource`] trait and the [`Resources`] container. All external
//! dependencies a workflow needs — database pools, HTTP clients, configuration objects,
//! caches — are registered once into a [`Resources`] map and then injected into every task at
//! dispatch time.
//!
//! ## Keyed by type
//!
//! A resource is inserted with [`insert`](Resources::insert) and read back by its type with
//! [`get`](Resources::get) — no key strings to keep in sync:
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Resource)]
//! struct Settings { batch: usize }
//!
//! let resources = Resources::new()
//!     .insert(Settings { batch: 3 })
//!     .insert(Blackboard::new());
//!
//! let settings: &Settings = resources.get::<Settings>().unwrap();
//! assert_eq!(settings.batch, 3);
//! resources.get::<Blackboard>().unwrap().put("n", settings.batch);
//! ```
//!
//! A second instance of a type already present needs a name: insert it with
//! [`insert_named`](Resources::insert_named) and read it with
//! [`get_named`](Resources::get_named):
//!
//! ```rust
//! use cano::prelude::*;
//!
//! let resources = Resources::new()
//!     .insert(Blackboard::new())
//!     .insert_named("audit", Blackboard::new());
//!
//! let _primary = resources.get::<Blackboard>().unwrap();
//! let _audit = resources.get_named::<Blackboard>("audit").unwrap();
//! ```
//!
//! `get` / `get_named` borrow (`&R`); [`get_arc`](Resources::get_arc) /
//! [`get_named_arc`](Resources::get_named_arc) hand out an owned `Arc<R>` for code that must
//! outlive the borrow (a spawned task, for instance).
//!
//! For stateless resources (no `setup` / `teardown` logic), `#[derive(Resource)]` skips the
//! empty impl block. To pull several resources out at once, decorate a deps struct with
//! `#[derive(FromResources)]`; every field is an `Arc<T>` looked up by type, or by name with
//! `#[res("name")]`:
//!
//! ```ignore
//! use cano::prelude::*;
//! use std::sync::Arc;
//!
//! #[derive(FromResources)]
//! struct PrepDeps {
//!     settings: Arc<Settings>,
//!     #[res("audit")]
//!     audit: Arc<Blackboard>,
//! }
//!
//! // ... inside a Task::run:
//! let PrepDeps { settings, audit } = PrepDeps::from_resources(res)?;
//! ```
//!
//! ## Lifecycle Guarantees
//!
//! 1. `setup` is called in **insertion order** (FIFO). Dependencies that must start before
//!    others should be inserted first.
//! 2. `teardown` is called in **reverse insertion order** (LIFO).
//! 3. Both `setup` and `teardown` calls are **sequential**, not concurrent.
//! 4. If `setup` fails at position *N*, `teardown` is run LIFO from position *N−1* down to
//!    position 0. Resources at positions ≥ *N* never had their `setup` called and are therefore
//!    never torn down.
//! 5. `teardown` errors are **logged but never abort** the teardown sequence.
//!
//! ## Concurrency Model
//!
//! Every resource is stored once behind an `Arc` and shared by every task, including split
//! tasks running in parallel. Resources that need mutable internal state must use interior
//! mutability (`Mutex`, `RwLock`, atomics, …).
//!
//! `Resources` is **not** `Clone`; share one between several owners by wrapping it in an `Arc`.

use crate::error::CanoError;
use cano_macros::resource;
use std::any::{Any, TypeId};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Health status
// ---------------------------------------------------------------------------

/// Health of a single [`Resource`], as reported by [`Resource::health`].
///
/// Returned per-resource by [`Resources::check_all_health`] and folded into a
/// single worst-case value by [`Resources::aggregate_health`]. Health checks
/// are *opt-in observability* — the engine never calls [`Resource::health`]
/// during normal workflow execution.
///
/// This enum is `#[non_exhaustive]`; match it with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HealthStatus {
    /// The resource is fully operational.
    Healthy,
    /// The resource is still usable but impaired (e.g. a degraded replica,
    /// partial connectivity, elevated latency). The string explains why.
    Degraded(String),
    /// The resource is not usable. The string explains why.
    Unhealthy(String),
}

impl HealthStatus {
    /// Returns `true` only for [`HealthStatus::Healthy`].
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }

    /// Severity rank used to fold many statuses into the worst one:
    /// `Healthy` (0) < `Degraded` (1) < `Unhealthy` (2).
    fn severity(&self) -> u8 {
        match self {
            HealthStatus::Healthy => 0,
            HealthStatus::Degraded(_) => 1,
            HealthStatus::Unhealthy(_) => 2,
        }
    }
}

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

    /// Report the current health of this resource.
    ///
    /// Invoked only by [`Resources::check_all_health`] and
    /// [`Resources::aggregate_health`] — never automatically during workflow
    /// execution. The default implementation returns [`HealthStatus::Healthy`].
    async fn health(&self) -> HealthStatus {
        HealthStatus::Healthy
    }
}

// ---------------------------------------------------------------------------
// Resources container
// ---------------------------------------------------------------------------

/// `TypeId` is already a uniformly distributed hash, so the type map skips SipHash.
#[derive(Default)]
struct TypeIdHasher(u64);

impl Hasher for TypeIdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Not reached for `TypeId` today (it writes integers); fold the bytes anyway so the
        // map stays correct if that ever changes.
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 ^= n;
    }

    fn write_u128(&mut self, n: u128) {
        self.0 ^= (n as u64) ^ ((n >> 64) as u64);
    }
}

type TypeMap = HashMap<TypeId, Box<dyn Any + Send + Sync>, BuildHasherDefault<TypeIdHasher>>;

/// A map of lifecycle-managed resources, keyed by type (and optionally by name).
///
/// See the [module documentation](self) for lookups, lifecycle guarantees and
/// concurrency notes.
#[derive(Default)]
pub struct Resources {
    /// One resource per type; each box holds an `Arc<R>`.
    by_type: TypeMap,
    /// Named resources; each box holds an `Arc<R>`.
    by_name: HashMap<Cow<'static, str>, Box<dyn Any + Send + Sync>>,
    /// Insertion order for FIFO setup / LIFO teardown, labelled with the name (or the type
    /// name for unnamed resources) for health reports.
    lifecycle: Vec<(Cow<'static, str>, Arc<dyn Resource>)>,
}

impl Resources {
    /// Create a new, empty `Resources` map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `resource` as the instance of its type, returning `self` for chaining.
    ///
    /// # Panics
    ///
    /// Panics if a resource of the same type was already inserted — wiring is a one-shot
    /// startup step, so this is a programmer error. Use [`insert_named`](Self::insert_named)
    /// for a second instance, or [`try_insert`](Self::try_insert) to get an error instead.
    #[must_use]
    pub fn insert<R: Resource>(mut self, resource: R) -> Self {
        if let Err(e) = self.put(None, resource) {
            panic!("{}", e.message());
        }
        self
    }

    /// Insert `resource` under `name`, returning `self` for chaining. Read it back with
    /// [`get_named`](Self::get_named).
    ///
    /// # Panics
    ///
    /// Panics if `name` is already taken. Use
    /// [`try_insert_named`](Self::try_insert_named) to get an error instead.
    #[must_use]
    pub fn insert_named<R: Resource>(
        mut self,
        name: impl Into<Cow<'static, str>>,
        resource: R,
    ) -> Self {
        if let Err(e) = self.put(Some(name.into()), resource) {
            panic!("{}", e.message());
        }
        self
    }

    /// [`insert`](Self::insert) that reports a duplicate type as an error.
    ///
    /// # Errors
    ///
    /// [`CanoError::ResourceDuplicateKey`] if a resource of type `R` is already present.
    pub fn try_insert<R: Resource>(mut self, resource: R) -> Result<Self, CanoError> {
        self.put(None, resource)?;
        Ok(self)
    }

    /// [`insert_named`](Self::insert_named) that reports a duplicate name as an error.
    ///
    /// # Errors
    ///
    /// [`CanoError::ResourceDuplicateKey`] if `name` is already taken.
    pub fn try_insert_named<R: Resource>(
        mut self,
        name: impl Into<Cow<'static, str>>,
        resource: R,
    ) -> Result<Self, CanoError> {
        self.put(Some(name.into()), resource)?;
        Ok(self)
    }

    /// Borrow the resource of type `R`.
    ///
    /// # Errors
    ///
    /// [`CanoError::ResourceNotFound`] when no resource of type `R` was inserted (named
    /// resources are not considered).
    pub fn get<R: Resource>(&self) -> Result<&R, CanoError> {
        self.slot::<R>(None).map(|arc| &**arc)
    }

    /// Borrow the resource named `name`.
    ///
    /// # Errors
    ///
    /// - [`CanoError::ResourceNotFound`] — nothing is named `name`.
    /// - [`CanoError::ResourceTypeMismatch`] — `name` holds a different type.
    pub fn get_named<R: Resource>(&self, name: &str) -> Result<&R, CanoError> {
        self.slot::<R>(Some(name)).map(|arc| &**arc)
    }

    /// [`get`](Self::get), returning an owned `Arc<R>`.
    ///
    /// # Errors
    ///
    /// As [`get`](Self::get).
    pub fn get_arc<R: Resource>(&self) -> Result<Arc<R>, CanoError> {
        self.slot::<R>(None).map(Arc::clone)
    }

    /// [`get_named`](Self::get_named), returning an owned `Arc<R>`.
    ///
    /// # Errors
    ///
    /// As [`get_named`](Self::get_named).
    pub fn get_named_arc<R: Resource>(&self, name: &str) -> Result<Arc<R>, CanoError> {
        self.slot::<R>(Some(name)).map(Arc::clone)
    }

    fn put<R: Resource>(
        &mut self,
        name: Option<Cow<'static, str>>,
        resource: R,
    ) -> Result<(), CanoError> {
        let arc = Arc::new(resource);
        let label = match name {
            Some(name) => {
                if self.by_name.contains_key(&name) {
                    return Err(CanoError::resource_duplicate_key(format!(
                        "duplicate resource name {name:?}"
                    )));
                }
                self.by_name
                    .insert(name.clone(), Box::new(Arc::clone(&arc)));
                name
            }
            None => {
                let type_name = std::any::type_name::<R>();
                if self.by_type.contains_key(&TypeId::of::<R>()) {
                    return Err(CanoError::resource_duplicate_key(format!(
                        "duplicate resource type {type_name}; use insert_named for a second instance"
                    )));
                }
                self.by_type
                    .insert(TypeId::of::<R>(), Box::new(Arc::clone(&arc)));
                Cow::Borrowed(type_name)
            }
        };
        self.lifecycle.push((label, arc));
        Ok(())
    }

    fn slot<R: Resource>(&self, name: Option<&str>) -> Result<&Arc<R>, CanoError> {
        let type_name = std::any::type_name::<R>;
        match name {
            None => self
                .by_type
                .get(&TypeId::of::<R>())
                .and_then(|boxed| boxed.downcast_ref::<Arc<R>>())
                .ok_or_else(|| {
                    CanoError::ResourceNotFound(format!("no resource of type {}", type_name()))
                }),
            Some(name) => {
                let boxed = self.by_name.get(name).ok_or_else(|| {
                    CanoError::ResourceNotFound(format!(
                        "no resource named {name:?} (requested type: {})",
                        type_name()
                    ))
                })?;
                boxed.downcast_ref::<Arc<R>>().ok_or_else(|| {
                    CanoError::ResourceTypeMismatch(format!(
                        "resource {name:?} is not a {}",
                        type_name()
                    ))
                })
            }
        }
    }

    /// Call `setup` on every resource in insertion order.
    ///
    /// If any `setup` call fails, `teardown` is called LIFO on all previously
    /// successful resources before the error is returned. Resources at position ≥ N
    /// (where N is the failing index) are never touched.
    ///
    /// # Errors
    ///
    /// The first `setup` error.
    pub async fn setup_all(&self) -> Result<(), CanoError> {
        use futures_util::FutureExt;
        use std::panic::AssertUnwindSafe;

        for (idx, (label, resource)) in self.lifecycle.iter().enumerate() {
            let outcome = AssertUnwindSafe(async { resource.setup().await })
                .catch_unwind()
                .await;
            let err = match outcome {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => e,
                Err(payload) => {
                    let msg = crate::workflow::panic_payload_message(&*payload);
                    #[cfg(feature = "tracing")]
                    tracing::error!(resource = %label, panic = %msg, "resource setup panicked");
                    let _ = label;
                    CanoError::generic(format!("resource setup panicked: {msg}"))
                }
            };
            self.teardown_range(0..idx).await;
            return Err(err);
        }
        Ok(())
    }

    /// Call `teardown` on every resource in **reverse** (LIFO) order.
    ///
    /// Errors from individual teardowns are logged (or printed to stderr when the
    /// `tracing` feature is disabled) but never abort the sequence.
    pub async fn teardown_all(&self) {
        self.teardown_range(0..self.lifecycle.len()).await;
    }

    /// [`teardown_all`](Self::teardown_all), returning the first teardown error.
    pub(crate) async fn teardown_all_result(&self) -> Result<(), CanoError> {
        self.teardown_range_result(0..self.lifecycle.len()).await
    }

    async fn teardown_range(&self, range: Range<usize>) {
        let _ = self.teardown_range_result(range).await;
    }

    /// Tear down `lifecycle[range]` LIFO; every resource is still torn down when one fails,
    /// each error is logged, and the first one is returned.
    async fn teardown_range_result(&self, range: Range<usize>) -> Result<(), CanoError> {
        use futures_util::FutureExt;
        use std::panic::AssertUnwindSafe;

        let mut first_error: Option<CanoError> = None;
        for (label, resource) in self.lifecycle[range].iter().rev() {
            let outcome = AssertUnwindSafe(async { resource.teardown().await })
                .catch_unwind()
                .await;
            let maybe_err = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(payload) => {
                    let msg = crate::workflow::panic_payload_message(&*payload);
                    let _ = label;
                    Some(CanoError::generic(format!(
                        "resource teardown panicked: {msg}"
                    )))
                }
            };
            if let Some(e) = maybe_err {
                #[cfg(feature = "tracing")]
                tracing::error!(resource = %label, "resource teardown failed: {e}");
                #[cfg(not(feature = "tracing"))]
                eprintln!("cano: resource teardown error: {e}");
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Call [`Resource::health`] on every resource, in insertion order. Each status is
    /// labelled with the resource's name, or its type name when it was inserted unnamed.
    ///
    /// Checks run sequentially, so a slow `health` implementation delays the rest.
    pub async fn check_all_health(&self) -> Vec<(Cow<'static, str>, HealthStatus)> {
        let mut out = Vec::with_capacity(self.lifecycle.len());
        for (label, resource) in &self.lifecycle {
            out.push((label.clone(), resource.health().await));
        }
        out
    }

    /// Call [`Resource::health`] on every resource and fold the results into the single
    /// worst status (`Healthy` < `Degraded` < `Unhealthy`).
    ///
    /// Checks run sequentially in insertion order. Returns [`HealthStatus::Healthy`] when no
    /// resources are registered.
    pub async fn aggregate_health(&self) -> HealthStatus {
        let mut worst = HealthStatus::Healthy;
        for (_, resource) in &self.lifecycle {
            let status = resource.health().await;
            if status.severity() > worst.severity() {
                worst = status;
            }
        }
        worst
    }
}

impl fmt::Debug for Resources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.lifecycle.iter().map(|(label, _)| label))
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

    #[derive(Debug)]
    struct TypeA;
    #[resource]
    impl Resource for TypeA {}

    #[derive(Debug)]
    struct TypeB;
    #[resource]
    impl Resource for TypeB {}

    /// Logs `setup:{name}` / `teardown:{name}`; optionally fails either phase.
    struct Tracking {
        log: Arc<Mutex<Vec<String>>>,
        name: &'static str,
        fail_setup: bool,
        fail_teardown: bool,
    }

    impl Tracking {
        fn new(name: &'static str, log: &Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                log: Arc::clone(log),
                name,
                fail_setup: false,
                fail_teardown: false,
            }
        }
    }

    #[resource]
    impl Resource for Tracking {
        async fn setup(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("setup:{}", self.name));
            if self.fail_setup {
                return Err(CanoError::generic(format!(
                    "setup failed for {}",
                    self.name
                )));
            }
            Ok(())
        }

        async fn teardown(&self) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("teardown:{}", self.name));
            if self.fail_teardown {
                return Err(CanoError::generic(format!(
                    "teardown failed for {}",
                    self.name
                )));
            }
            Ok(())
        }
    }

    fn log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[test]
    fn get_by_type_borrows_the_same_instance() {
        let resources = Resources::new().insert(TypeA).insert(TypeB);
        let a1: *const TypeA = resources.get::<TypeA>().unwrap();
        let a2 = resources.get_arc::<TypeA>().unwrap();
        assert!(std::ptr::eq(a1, &*a2));
        assert!(resources.get::<TypeB>().is_ok());
    }

    #[test]
    fn missing_type_names_the_type() {
        let err = Resources::new().get::<TypeA>().unwrap_err();
        assert_eq!(err.category(), "resource_not_found");
        assert!(err.message().contains("TypeA"), "{err}");
    }

    #[test]
    fn named_resources_are_separate_from_typed_ones() {
        let resources = Resources::new().insert(TypeA).insert_named("second", TypeA);
        assert!(resources.get::<TypeA>().is_ok());
        assert!(resources.get_named::<TypeA>("second").is_ok());
        assert!(resources.get_named_arc::<TypeA>("second").is_ok());
        assert!(!std::ptr::eq(
            resources.get::<TypeA>().unwrap(),
            resources.get_named::<TypeA>("second").unwrap()
        ));

        // A named-only type is not found by type.
        let named_only = Resources::new().insert_named("b", TypeB);
        assert_eq!(
            named_only.get::<TypeB>().unwrap_err().category(),
            "resource_not_found"
        );
    }

    #[test]
    fn get_named_reports_missing_and_mismatch_distinctly() {
        let resources = Resources::new().insert_named("a", TypeA);
        let mismatch = resources.get_named::<TypeB>("a").unwrap_err();
        assert_eq!(mismatch.category(), "resource_type_mismatch");
        assert!(mismatch.message().contains("\"a\""), "{mismatch}");

        let missing = resources.get_named::<TypeA>("absent").unwrap_err();
        assert_eq!(missing.category(), "resource_not_found");
        assert!(missing.message().contains("\"absent\""), "{missing}");
    }

    #[test]
    #[should_panic(expected = "use insert_named for a second instance")]
    fn insert_panics_on_duplicate_type() {
        let _ = Resources::new().insert(TypeA).insert(TypeA);
    }

    #[test]
    #[should_panic(expected = "duplicate resource name \"dup\"")]
    fn insert_named_panics_on_duplicate_name() {
        let _ = Resources::new()
            .insert_named("dup", TypeA)
            .insert_named("dup", TypeB);
    }

    #[test]
    fn try_insert_reports_duplicates() {
        let err = Resources::new()
            .insert(TypeA)
            .try_insert(TypeA)
            .expect_err("duplicate type");
        assert_eq!(err.category(), "resource_duplicate_key");

        let err = Resources::new()
            .insert_named("x", TypeA)
            .try_insert_named("x", TypeB)
            .expect_err("duplicate name");
        assert_eq!(err.category(), "resource_duplicate_key");

        let ok = Resources::new()
            .try_insert(TypeA)
            .and_then(|r| r.try_insert_named("x", TypeA))
            .expect("fresh type and name");
        assert!(ok.get_named::<TypeA>("x").is_ok());
    }

    #[tokio::test]
    async fn setup_is_fifo_and_teardown_is_lifo() {
        let log = log();
        let resources = Resources::new()
            .insert_named("a", Tracking::new("A", &log))
            .insert_named("b", Tracking::new("B", &log))
            .insert_named("c", Tracking::new("C", &log));

        resources.setup_all().await.unwrap();
        resources.teardown_all().await;

        let events = log.lock().unwrap().clone();
        assert_eq!(
            events,
            [
                "setup:A",
                "setup:B",
                "setup:C",
                "teardown:C",
                "teardown:B",
                "teardown:A"
            ]
        );
    }

    #[tokio::test]
    async fn setup_failure_rolls_back_only_the_set_up_prefix() {
        let log = log();
        let resources = Resources::new()
            .insert_named("r0", Tracking::new("R0", &log))
            .insert_named("r1", Tracking::new("R1", &log))
            .insert_named(
                "r2",
                Tracking {
                    fail_setup: true,
                    ..Tracking::new("R2", &log)
                },
            )
            .insert_named("r3", Tracking::new("R3", &log));

        assert!(resources.setup_all().await.is_err());
        let events = log.lock().unwrap().clone();
        assert_eq!(
            events,
            [
                "setup:R0",
                "setup:R1",
                "setup:R2",
                "teardown:R1",
                "teardown:R0"
            ]
        );
    }

    #[tokio::test]
    async fn teardown_continues_past_failures_and_reports_the_first() {
        let log = log();
        let failing = |name| Tracking {
            fail_teardown: true,
            ..Tracking::new(name, &log)
        };
        let resources = Resources::new()
            .insert_named("r0", failing("R0"))
            .insert_named("r1", failing("R1"))
            .insert_named("r2", failing("R2"));

        let err = resources.teardown_all_result().await.unwrap_err();
        assert!(
            err.message().contains("R2"),
            "first error is the first torn down: {err}"
        );
        let events = log.lock().unwrap().clone();
        assert_eq!(events, ["teardown:R2", "teardown:R1", "teardown:R0"]);
    }

    struct DegradedResource;
    #[resource]
    impl Resource for DegradedResource {
        async fn health(&self) -> HealthStatus {
            HealthStatus::Degraded("replica lag".to_string())
        }
    }

    struct UnhealthyResource;
    #[resource]
    impl Resource for UnhealthyResource {
        async fn health(&self) -> HealthStatus {
            HealthStatus::Unhealthy("connection refused".to_string())
        }
    }

    #[tokio::test]
    async fn check_all_health_is_labelled_and_ordered() {
        let resources = Resources::new()
            .insert(TypeA)
            .insert_named("degraded", DegradedResource)
            .insert_named("down", UnhealthyResource);

        let health = resources.check_all_health().await;
        assert_eq!(health.len(), 3);
        assert!(health[0].0.ends_with("TypeA"), "{:?}", health[0].0);
        assert_eq!(health[0].1, HealthStatus::Healthy);
        assert_eq!(
            health[1],
            (
                Cow::Borrowed("degraded"),
                HealthStatus::Degraded("replica lag".to_string())
            )
        );
        assert_eq!(
            health[2],
            (
                Cow::Borrowed("down"),
                HealthStatus::Unhealthy("connection refused".to_string())
            )
        );
    }

    #[tokio::test]
    async fn aggregate_health_returns_worst_status() {
        let degraded_only = Resources::new().insert(TypeA).insert(DegradedResource);
        assert_eq!(
            degraded_only.aggregate_health().await,
            HealthStatus::Degraded("replica lag".to_string())
        );

        let with_unhealthy = Resources::new()
            .insert(TypeA)
            .insert(DegradedResource)
            .insert(UnhealthyResource);
        assert_eq!(
            with_unhealthy.aggregate_health().await,
            HealthStatus::Unhealthy("connection refused".to_string())
        );

        let empty = Resources::new();
        assert!(empty.check_all_health().await.is_empty());
        assert_eq!(empty.aggregate_health().await, HealthStatus::Healthy);
    }

    #[test]
    fn test_health_status_is_healthy_helper() {
        assert!(HealthStatus::Healthy.is_healthy());
        assert!(!HealthStatus::Degraded("x".to_string()).is_healthy());
        assert!(!HealthStatus::Unhealthy("x".to_string()).is_healthy());
    }

    #[tokio::test]
    async fn panicking_setup_rolls_back_prior_resources_lifo_and_returns_error() {
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

        struct TrackedRes {
            name: &'static str,
            log: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[crate::resource]
        impl Resource for TrackedRes {
            async fn setup(&self) -> Result<(), crate::CanoError> {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("setup:{}", self.name));
                Ok(())
            }
            async fn teardown(&self) -> Result<(), crate::CanoError> {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("teardown:{}", self.name));
                Ok(())
            }
        }

        struct PanicSetup;
        #[crate::resource]
        impl Resource for PanicSetup {
            async fn setup(&self) -> Result<(), crate::CanoError> {
                panic!("boom during setup");
            }
        }

        let res = Resources::new()
            .insert_named(
                "first",
                TrackedRes {
                    name: "first",
                    log: Arc::clone(&log),
                },
            )
            .insert_named(
                "second",
                TrackedRes {
                    name: "second",
                    log: Arc::clone(&log),
                },
            )
            .insert(PanicSetup);

        let err = res
            .setup_all()
            .await
            .expect_err("panicking setup must fail");
        assert!(err.to_string().contains("boom during setup"), "{err}");
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "setup:first".to_string(),
                "setup:second".to_string(),
                "teardown:second".to_string(),
                "teardown:first".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn panicking_teardown_still_runs_remaining_teardowns_lifo() {
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

        struct TrackedTear {
            name: &'static str,
            panic_on_tear: bool,
            log: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[crate::resource]
        impl Resource for TrackedTear {
            async fn teardown(&self) -> Result<(), crate::CanoError> {
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("teardown:{}", self.name));
                if self.panic_on_tear {
                    panic!("boom during teardown {}", self.name);
                }
                Ok(())
            }
        }

        let res = Resources::new()
            .insert_named(
                "a",
                TrackedTear {
                    name: "a",
                    panic_on_tear: false,
                    log: Arc::clone(&log),
                },
            )
            .insert_named(
                "b",
                TrackedTear {
                    name: "b",
                    panic_on_tear: true,
                    log: Arc::clone(&log),
                },
            )
            .insert_named(
                "c",
                TrackedTear {
                    name: "c",
                    panic_on_tear: false,
                    log: Arc::clone(&log),
                },
            );

        let err = res
            .teardown_all_result()
            .await
            .expect_err("panicking teardown must surface an error");
        assert!(err.to_string().contains("boom during teardown b"), "{err}");
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "teardown:c".to_string(),
                "teardown:b".to_string(),
                "teardown:a".to_string(),
            ]
        );
    }
}
