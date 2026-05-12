//! # RouterTask — Side-Effect-Free Branching
//!
//! A [`RouterTask`] inspects the workflow [`Resources`] and returns the next
//! [`TaskResult`] without performing any side effects. It is the canonical way
//! to implement branching logic in a Cano workflow.
//!
//! ## When to use `RouterTask`
//!
//! - **Pure routing**: redirect the FSM to a different state based on a flag, a
//!   counter in the store, or a configuration value in resources.
//! - **Fan-out / split**: return [`TaskResult::Split`] to launch parallel branches.
//! - **Zero side effects**: because a router only *reads* state (no writes), it is
//!   safe to retry or re-execute without idempotency concerns.
//!
//! Every [`RouterTask`] automatically implements [`Task`](crate::task::Task) via a
//! per-impl-site companion `impl Task<S> for T` emitted by the `#[task::router]` macro
//! (a blanket impl would conflict with the analogous blanket impls for the other
//! specialized task traits). This means you can register a `RouterTask` with
//! [`Workflow::register`](crate::workflow::Workflow::register) using the same call as for
//! any other `Task`.
//!
//! ## Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Route, PathA, PathB, Done }
//!
//! struct MyRouter;
//!
//! #[task::router(state = Step)]
//! impl MyRouter {
//!     async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         // Pure decision: no writes to the store.
//!         Ok(TaskResult::Single(Step::PathA))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! struct DoPathA;
//!
//! #[task]
//! impl Task<Step> for DoPathA {
//!     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! let workflow = Workflow::bare()
//!     .register(Step::Route, MyRouter)
//!     .register(Step::PathA, DoPathA)
//!     .add_exit_state(Step::Done);
//!
//! let result = workflow.orchestrate(Step::Route).await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```
//!
//! ## The trait-impl form
//!
//! You can also write the full `impl RouterTask<S> for T` header explicitly:
//!
//! ```rust
//! use cano::prelude::*;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Route, Done }
//!
//! struct SimpleRouter;
//!
//! #[task::router]
//! impl RouterTask<Step> for SimpleRouter {
//!     async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let workflow = Workflow::bare()
//!     .register(Step::Route, SimpleRouter)
//!     .add_exit_state(Step::Done);
//!
//! let result = workflow.orchestrate(Step::Route).await?;
//! assert_eq!(result, Step::Done);
//! # Ok(())
//! # }
//! ```

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{TaskConfig, TaskResult};
use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;

/// RouterTask trait for side-effect-free workflow branching
///
/// A [`RouterTask`] inspects the workflow [`Resources`] and returns the next
/// [`TaskResult`] *without* mutating any state. It is the lightweight building block
/// for conditional routing and fan-out in a Cano workflow.
///
/// # Generic Types
///
/// - **`TState`**: The state enum used by the workflow (must be `Clone + Debug + Send + Sync`).
/// - **`TResourceKey`**: The key type used to look up resources (defaults to
///   [`Cow<'static, str>`] — accepts `&'static str` literals without allocating, plus
///   owned `String` keys for runtime-built names).
///
/// # Implementing RouterTask
///
/// Prefer the inherent `#[task::router(state = S)]` form — it builds the trait header for
/// you and only requires the `route` method:
///
/// ```rust
/// use cano::prelude::*;
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// enum Step { Decide, Left, Right, Done }
///
/// struct ThresholdRouter { threshold: u32 }
///
/// #[task::router(state = Step)]
/// impl ThresholdRouter {
///     async fn route(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
///         let store = res.get::<MemoryStore, _>("store")?;
///         let value: u32 = store.get("value").unwrap_or(0);
///         if value >= self.threshold {
///             Ok(TaskResult::Single(Step::Right))
///         } else {
///             Ok(TaskResult::Single(Step::Left))
///         }
///     }
/// }
/// ```
///
/// # Fan-out with Split
///
/// A router may return [`TaskResult::Split`] to launch parallel branches:
///
/// ```rust
/// use cano::prelude::*;
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// enum Step { Dispatch, WorkerA, WorkerB, Done }
///
/// struct FanOutRouter;
///
/// #[task::router(state = Step)]
/// impl FanOutRouter {
///     async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
///         Ok(TaskResult::Split(vec![Step::WorkerA, Step::WorkerB]))
///     }
/// }
/// ```
#[crate::task::router]
pub trait RouterTask<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Get the task configuration that controls execution behaviour.
    ///
    /// Returns the `TaskConfig` that determines how this router should be executed
    /// (retry policy, circuit-breaker, etc.). The default returns `TaskConfig::default()`.
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
    }

    /// Human-readable identifier for this router, reported to [`WorkflowObserver`] hooks.
    ///
    /// The default returns [`std::any::type_name`] of the implementing type.
    /// Override it to give a router a stable, friendlier name.
    ///
    /// [`WorkflowObserver`]: crate::observer::WorkflowObserver
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Inspect resources and return the next workflow state.
    ///
    /// This method should be **side-effect-free**: it may read from resources but
    /// must not write to them. For tasks that need to write state before routing, use
    /// the full [`Task`](crate::task::Task) trait instead.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError`] if the routing logic encounters an error (e.g. a missing
    /// required resource).
    async fn route(&self, res: &Resources<TResourceKey>) -> Result<TaskResult<TState>, CanoError>;
}

/// Type alias for a dynamic [`RouterTask`] trait object.
///
/// Use this when you need to store different router types in the same collection.
/// `TResourceKey` defaults to [`Cow<'static, str>`] to match [`Resources`].
pub type DynRouterTask<TState, TResourceKey = Cow<'static, str>> =
    dyn RouterTask<TState, TResourceKey> + Send + Sync;

/// Type alias for an `Arc`-wrapped dynamic [`RouterTask`] trait object.
///
/// This alias simplifies working with dynamic router collections.
pub type RouterTaskObject<TState, TResourceKey = Cow<'static, str>> =
    std::sync::Arc<DynRouterTask<TState, TResourceKey>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::task;
    use crate::task::Task;
    use std::sync::Arc;

    // Note: all `#[task::router]` usages inside the `cano` crate itself use the
    // trait-impl form (`impl RouterTask<S> for T`), because the inherent form emits
    // `::cano::RouterTask<...>` paths that don't resolve inside this crate. The
    // inherent form is tested in `cano-macros/tests/router_task_impl.rs` where
    // `::cano` resolves correctly.

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum Step {
        Decide,
        PathA,
        PathB,
        Done,
    }

    // ---------------------------------------------------------------------------
    // (a) RouterTask returning TaskResult::Single — trait-impl form
    // ---------------------------------------------------------------------------

    struct SingleRouter;

    #[task::router]
    impl RouterTask<Step> for SingleRouter {
        async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::PathA))
        }
    }

    #[tokio::test]
    async fn test_router_task_single_via_route() {
        let router = SingleRouter;
        let res = Resources::new();
        let result = RouterTask::route(&router, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::PathA));
    }

    #[tokio::test]
    async fn test_router_task_single_via_task_run() {
        // Verify the companion Task impl works: call Task::run explicitly.
        let router = SingleRouter;
        let res = Resources::new();
        let result = Task::run(&router, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::PathA));
    }

    // ---------------------------------------------------------------------------
    // (b) RouterTask returning TaskResult::Split
    // ---------------------------------------------------------------------------

    struct SplitRouter;

    #[task::router]
    impl RouterTask<Step> for SplitRouter {
        async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Split(vec![Step::PathA, Step::PathB]))
        }
    }

    #[tokio::test]
    async fn test_router_task_split() {
        let router = SplitRouter;
        let res = Resources::new();
        let result = Task::run(&router, &res).await.unwrap();
        assert_eq!(result, TaskResult::Split(vec![Step::PathA, Step::PathB]));
    }

    // ---------------------------------------------------------------------------
    // (c) RouterTask through the companion Task impl — dynamic dispatch
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_router_task_as_dyn_task() {
        // Cast to Arc<dyn Task<Step>> to confirm the companion Task impl compiles
        // and executes correctly.
        let router: Arc<dyn Task<Step>> = Arc::new(SingleRouter);
        let res = Resources::new();
        let result = Task::run(router.as_ref(), &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::PathA));
    }

    // ---------------------------------------------------------------------------
    // (d) config + name overrides in the trait-impl form
    // ---------------------------------------------------------------------------

    struct CustomRouter;

    #[task::router]
    impl RouterTask<Step> for CustomRouter {
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        fn name(&self) -> Cow<'static, str> {
            Cow::Borrowed("my-custom-router")
        }
        async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[test]
    fn test_router_task_config_override() {
        let router = CustomRouter;
        // Disambiguate: both RouterTask and the companion Task impl expose config().
        assert_eq!(RouterTask::config(&router).retry_mode.max_attempts(), 1);
    }

    #[test]
    fn test_router_task_name_override() {
        let router = CustomRouter;
        // Disambiguate: both RouterTask and the companion Task impl expose name().
        assert_eq!(RouterTask::<Step>::name(&router), "my-custom-router");
    }

    // Task config/name are forwarded from RouterTask through the companion impl.
    #[test]
    fn test_companion_task_forwards_config_and_name() {
        let router = CustomRouter;
        // Via Task trait methods (companion impl delegates to RouterTask via UFCS)
        assert_eq!(Task::config(&router).retry_mode.max_attempts(), 1);
        assert_eq!(Task::name(&router), "my-custom-router");
    }

    // ---------------------------------------------------------------------------
    // (e) Workflow integration via Workflow::register (plain Task path)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_router_task_in_workflow() {
        use crate::workflow::Workflow;

        struct PathATask;

        #[task]
        impl Task<Step> for PathATask {
            async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
                Ok(TaskResult::Single(Step::Done))
            }
        }

        let workflow = Workflow::bare()
            .register(Step::Decide, SingleRouter)
            .register(Step::PathA, PathATask)
            .add_exit_state(Step::Done);

        let result = workflow.orchestrate(Step::Decide).await.unwrap();
        assert_eq!(result, Step::Done);
    }

    // ---------------------------------------------------------------------------
    // (f) RouterTask default config and name
    // ---------------------------------------------------------------------------

    struct DefaultConfigRouter;

    #[task::router]
    impl RouterTask<Step> for DefaultConfigRouter {
        async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[test]
    fn test_router_task_default_config() {
        let router = DefaultConfigRouter;
        // Disambiguate: both RouterTask and companion Task impl expose config().
        assert_eq!(
            RouterTask::<Step>::config(&router)
                .retry_mode
                .max_attempts(),
            4
        );
    }

    #[test]
    fn test_router_task_default_name_contains_type_name() {
        let router = DefaultConfigRouter;
        // Disambiguate: both RouterTask and companion Task impl expose name().
        let name = RouterTask::<Step>::name(&router);
        assert!(
            name.contains("DefaultConfigRouter"),
            "default name should contain the type name, got: {name}",
        );
    }
}
