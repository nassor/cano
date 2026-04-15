//! # Node API - Structured Workflow Processing
//!
//! This module provides the core [`Node`] trait, which defines the interface for structured workflow processing.
//! Nodes are ideal for complex operations that benefit from a clear separation of concerns into three phases: `prep`, `exec`, and `post`.
//!
//! ## Task vs Node - Choose the Right Tool
//!
//! - **Use [`Task`]** for simple processing with a single `run()` method.
//! - **Use [`Node`]** for structured processing with a three-phase lifecycle.
//!
//! Both `Task` and `Node` support retry strategies.
//!
//! **Every [`Node`] automatically implements [`Task`]**, so you can mix and match in the same workflow.
//!
//! ## Unified API Benefits
//!
//! - **Simpler API**: One trait to learn, not a hierarchy of traits
//! - **Type Safety**: Return enum values instead of strings for workflow control
//! - **Performance**: No string conversion overhead
//! - **IDE Support**: Autocomplete for enum variants
//! - **Compile-Time Safety**: Impossible to have invalid state transitions
//!
//! ## Quick Start
//!
//! Implement the `Node` trait for your custom processing logic. The `#[cano::node]`
//! attribute infers `type PrepResult` and `type ExecResult` from the return types of
//! your `prep` and `exec` methods, and supplies a default `fn config()` if absent —
//! you only write the business logic:
//!
//! ```ignore
//! use cano::prelude::*;
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! enum Step { Process, Done }
//!
//! struct MyNode;
//!
//! #[node]
//! impl Node<Step> for MyNode {
//!     async fn prep(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
//!         Ok((1..=10).collect())
//!     }
//!     async fn exec(&self, prep: Vec<u32>) -> u32 {
//!         prep.iter().sum()
//!     }
//!     async fn post(&self, _res: &Resources, exec: u32)
//!         -> Result<Step, CanoError>
//!     {
//!         println!("sum = {exec}");
//!         Ok(Step::Done)
//!     }
//! }
//! ```
//!
//! The macro fills in `type PrepResult = Vec<u32>;`, `type ExecResult = u32;`, and
//! `fn config(&self) -> TaskConfig { TaskConfig::default() }`. Pair with
//! [`#[derive(FromResources)]`](crate::FromResources) to declaratively pull the
//! resources each phase needs out of the shared map.
//!
//! ## Retry Behavior
//!
//! When a node is configured with retries (via [`TaskConfig`]), the **entire three-phase
//! pipeline** (`prep` → `exec` → `post`) is re-run from scratch on any failure:
//!
//! - If `post` fails, both `prep` and `exec` run again on the next attempt.
//! - If `prep` fails, the whole pipeline retries from `prep`.
//!
//! **Implementors must ensure `prep` and `exec` are idempotent** — safe to call multiple
//! times with the same observable effect — because any phase failure causes the entire
//! pipeline to restart. Side effects in `exec` (e.g. writing to an external system) will
//! be repeated on every retry attempt.
//!
//! ## Performance Tips
//!
//! - Nodes execute with minimal overhead for maximum throughput
//! - Use async operations for I/O bound work
//! - Implement retry logic in TaskConfig for resilience
//!
//! ## Retry semantics: direct use vs. workflow use
//!
//! | Call site | Retry behaviour |
//! |-----------|----------------|
//! | `node.run(res)` directly | Retries run **inside** `Node::run` via `run_with_retries` |
//! | Node registered in a `Workflow` | Workflow dispatcher drives retries; `Task::run` (blanket impl) executes one `prep`→`exec`→`post` pass per attempt |
//!
//! Both paths honour the same [`TaskConfig`] from [`Node::config`]; retry count and delays are
//! identical. The difference is **where** the retry loop lives. Do not call `Node::run` inside
//! a hand-written `Task::run` override — that would execute the retry loop twice.

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::TaskConfig;
use cano_macros::node;
use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;

#[cfg(feature = "tracing")]
use tracing::Instrument;

/// Standard result type for node execution phases
///
/// This type represents the result of a node's execution phase. It uses `Box<dyn Any>`
/// to allow nodes to return any type while maintaining type erasure for dynamic workflows.
pub type DefaultNodeResult = Result<Box<dyn std::any::Any + Send + Sync>, CanoError>;

/// Node trait for workflow processing
///
/// This trait defines the core interface that all workflow nodes must implement.
/// It provides type flexibility while maintaining performance and type safety.
///
/// # Generic Types
///
/// - **`TState`**: The return type from the post method (typically an enum for workflow control)
/// - **`TResourceKey`**: The key type used to look up resources (defaults to
///   [`Cow<'static, str>`](std::borrow::Cow) — accepts `&'static str` literals
///   without allocating, plus owned `String` keys for runtime-built names)
/// - **`PrepResult`**: The result type from the `prep` phase, passed to `exec`.
/// - **`ExecResult`**: The result type from the `exec` phase, passed to `post`.
///
/// # Node Lifecycle
///
/// Each node follows a three-phase execution lifecycle:
///
/// 1. **[`prep`](Node::prep)**: Preparation phase - setup and data loading
/// 2. **[`exec`](Node::exec)**: Execution phase - main processing logic
/// 3. **[`post`](Node::post)**: Post-processing phase - cleanup and result handling
///
/// The [`run`](Node::run) method orchestrates these phases automatically.
///
/// # Benefits over String-based Approaches
///
/// - **Type Safety**: Return enum values instead of strings
/// - **Performance**: No string conversion overhead
/// - **IDE Support**: Autocomplete for enum variants
/// - **Compile-Time Safety**: Impossible to have invalid state transitions
///
/// # Example
///
/// ```rust,ignore
/// use cano::prelude::*;
///
/// // A params struct that carries node configuration as a resource.
/// struct NodeParams {
///     batch_size: usize,
/// }
/// impl Resource for NodeParams {}
///
/// struct MyNode;
///
/// #[node]
/// impl Node<String> for MyNode {
///     type PrepResult = Vec<u32>;
///     type ExecResult = u32;
///
///     fn config(&self) -> TaskConfig {
///         TaskConfig::minimal()  // Use minimal retries for fast execution
///     }
///
///     async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
///         // Read params and previously stored data from resources.
///         let params = res.get::<NodeParams>("params")?;
///         let store = res.get::<MemoryStore>("store")?;
///         let data: Vec<u32> = store.get("input")?;
///         // Take only up to batch_size items.
///         Ok(data.into_iter().take(params.batch_size).collect())
///     }
///
///     async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
///         // Pure computation: no resource access, easy to test in isolation.
///         prep_res.iter().sum()
///     }
///
///     async fn post(&self, res: &Resources, exec_res: Self::ExecResult)
///         -> Result<String, CanoError> {
///         // Write result back to the store so downstream nodes can read it.
///         let store = res.get::<MemoryStore>("store")?;
///         store.put("sum", exec_res)?;
///         Ok("done".to_string())
///     }
/// }
/// ```
#[node]
pub trait Node<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Result type from the prep phase
    type PrepResult: Send + Sync;
    /// Result type from the exec phase
    type ExecResult: Send + Sync;

    /// Get the node configuration that controls execution behavior
    ///
    /// Returns the TaskConfig that determines how this node should be executed.
    /// The default implementation returns `TaskConfig::default()` which configures
    /// the node with standard retry logic.
    ///
    /// Override this method to customize execution behavior:
    /// - Use `TaskConfig::minimal()` for fast-failing nodes with minimal retries
    /// - Use `TaskConfig::new().with_fixed_retry(n, duration)` for custom retry behavior
    /// - Return a custom configuration with specific retry/parameter settings
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
    }

    /// Preparation phase - load data and setup resources
    ///
    /// This is the first phase of node execution. Use it to:
    /// - Load data from resources that was left by previous nodes
    /// - Validate inputs and parameters
    /// - Setup resources needed for execution
    /// - Prepare any data structures
    ///
    /// The result of this phase is passed to the [`exec`](Node::exec) method.
    async fn prep(&self, res: &Resources<TResourceKey>) -> Result<Self::PrepResult, CanoError>;

    /// Execution phase - main processing logic
    ///
    /// This is the core processing phase where the main business logic runs.
    /// This phase doesn't have access to resources - it only receives the result
    /// from the [`prep`](Node::prep) phase and produces a result for the [`post`](Node::post) phase.
    ///
    /// Benefits of this design:
    /// - Clear separation of concerns
    /// - Easier testing (pure function)
    /// - Better performance (no resource access during processing)
    ///
    /// # Retry Note
    ///
    /// On any phase failure, the **entire** `prep` → `exec` → `post` pipeline restarts.
    /// This method must be idempotent: if it has side effects (e.g. sending a network
    /// request or writing to an external system), those side effects will be repeated
    /// on every retry attempt.
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult;

    /// Post-processing phase - cleanup and result handling
    ///
    /// This is the final phase of node execution. Use it to:
    /// - Store results for the next node to use
    /// - Clean up resources
    /// - Determine the next action/node to run
    /// - Handle errors from the exec phase
    ///
    /// This method returns a typed value that determines what happens next in the workflow.
    async fn post(
        &self,
        res: &Resources<TResourceKey>,
        exec_res: Self::ExecResult,
    ) -> Result<TState, CanoError>;

    /// Run the complete node lifecycle with configuration-driven execution.
    ///
    /// Orchestrates `prep` → `exec` → `post` with the retry policy from [`Node::config`].
    /// Only `prep` and `post` failures are retried; `exec` is infallible by design (returns
    /// `Self::ExecResult` directly). You can override this method for completely custom
    /// orchestration.
    ///
    /// # Workflow integration
    ///
    /// When a `Node` is registered with a [`crate::workflow::Workflow`], the workflow engine
    /// uses the blanket [`crate::task::Task`] impl rather than calling this method directly.
    /// That blanket impl runs a **single** `prep` → `exec` → `post` pass per attempt and
    /// delegates retries to the outer `run_with_retries` call in the workflow dispatcher.
    ///
    /// If you call `Node::run` directly (outside a workflow), retries run **here**, which is
    /// correct for standalone use. Do not call `Node::run` inside a custom `Task::run`
    /// implementation — that would double-retry the node.
    ///
    /// # Errors
    ///
    /// - When retries are disabled (`max_attempts <= 1`), the original error from `prep`
    ///   or `post` is propagated unchanged (typically [`CanoError::Preparation`] or
    ///   [`CanoError::NodeExecution`]).
    /// - When retries are enabled and the attempt limit is reached, the failure is
    ///   wrapped in [`CanoError::RetryExhausted`] with the underlying message inlined.
    async fn run(&self, res: &Resources<TResourceKey>) -> Result<TState, CanoError> {
        let config = self.config();
        self.run_with_retries(res, &config).await
    }

    /// Internal method to run the node lifecycle with retry logic
    ///
    /// Executes the three phases (`prep` → `exec` → `post`) in sequence, retrying the
    /// **entire pipeline** from `prep` whenever any phase returns an error.
    ///
    /// # Full-Pipeline Retry Semantics
    ///
    /// Unlike retry strategies that only re-run the failing step, this method restarts
    /// from the very beginning on each attempt:
    ///
    /// - If `prep` fails → the whole pipeline retries from `prep`.
    /// - If `post` fails → `prep` and `exec` both re-run before `post` is tried again.
    ///
    /// This means **all three phases must be idempotent** when retries are enabled.
    /// Any side effects (network calls, writes to external systems, etc.) in `prep` or
    /// `exec` will be repeated on every retry attempt.
    ///
    /// The number of attempts and delay between them are controlled by the
    /// [`TaskConfig`] returned from [`Node::config`].
    async fn run_with_retries(
        &self,
        res: &Resources<TResourceKey>,
        config: &TaskConfig,
    ) -> Result<TState, CanoError> {
        #[cfg(feature = "tracing")]
        let node_span = tracing::info_span!("node_execution");

        #[cfg(feature = "tracing")]
        let _enter = node_span.enter();

        let max_attempts = config.retry_mode.max_attempts();
        let mut attempt = 0;

        #[cfg(feature = "tracing")]
        tracing::debug!(
            max_attempts = max_attempts,
            "Starting node execution with retry logic"
        );

        loop {
            #[cfg(feature = "tracing")]
            tracing::debug!(attempt = attempt, "Starting node execution attempt");

            // Execute the prep phase
            #[cfg(feature = "tracing")]
            let prep_result = {
                let prep_span = tracing::debug_span!("node_prep", attempt = attempt);
                self.prep(res).instrument(prep_span).await
            };

            #[cfg(not(feature = "tracing"))]
            let prep_result = self.prep(res).await;

            match prep_result {
                Ok(prep_res) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(attempt = attempt, "Prep phase completed successfully");

                    // Execute the exec phase
                    #[cfg(feature = "tracing")]
                    let exec_res = {
                        let exec_span = tracing::debug_span!("node_exec", attempt = attempt);
                        async { self.exec(prep_res).await }
                            .instrument(exec_span)
                            .await
                    };

                    #[cfg(not(feature = "tracing"))]
                    let exec_res = self.exec(prep_res).await;

                    #[cfg(feature = "tracing")]
                    tracing::debug!(attempt = attempt, "Exec phase completed");

                    // Execute the post phase
                    #[cfg(feature = "tracing")]
                    let post_result = {
                        let post_span = tracing::debug_span!("node_post", attempt = attempt);
                        self.post(res, exec_res).instrument(post_span).await
                    };

                    #[cfg(not(feature = "tracing"))]
                    let post_result = self.post(res, exec_res).await;

                    match post_result {
                        Ok(result) => {
                            #[cfg(feature = "tracing")]
                            tracing::info!(attempt = attempt, final_result = ?result, "Node execution completed successfully");
                            return Ok(result);
                        }
                        Err(e) => {
                            attempt += 1;

                            #[cfg(feature = "tracing")]
                            tracing::warn!(attempt = attempt, error = ?e, max_attempts = max_attempts, "Post phase failed");

                            if attempt >= max_attempts {
                                #[cfg(feature = "tracing")]
                                tracing::error!(attempt = attempt, error = ?e, "Node execution failed after maximum attempts");
                                if max_attempts <= 1 {
                                    return Err(e);
                                }
                                return Err(CanoError::retry_exhausted(format!(
                                    "Node post phase failed after {} attempt(s): {}",
                                    attempt, e
                                )));
                            } else if let Some(delay) =
                                config.retry_mode.delay_for_attempt(attempt - 1)
                            {
                                #[cfg(feature = "tracing")]
                                tracing::debug!(
                                    attempt = attempt,
                                    delay_ms = delay.as_millis(),
                                    "Retrying after delay"
                                );
                                tokio::time::sleep(delay).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    attempt += 1;

                    #[cfg(feature = "tracing")]
                    tracing::warn!(attempt = attempt, error = ?e, max_attempts = max_attempts, "Prep phase failed");

                    if attempt >= max_attempts {
                        #[cfg(feature = "tracing")]
                        tracing::error!(attempt = attempt, error = ?e, "Node execution failed after maximum attempts");
                        if max_attempts <= 1 {
                            return Err(e);
                        }
                        return Err(CanoError::retry_exhausted(format!(
                            "Node prep phase failed after {} attempt(s): {}",
                            attempt, e
                        )));
                    } else if let Some(delay) = config.retry_mode.delay_for_attempt(attempt - 1) {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            attempt = attempt,
                            delay_ms = delay.as_millis(),
                            "Retrying after delay"
                        );
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
    }
}

/// Type alias for a dynamic node trait object.
///
/// Use this when you need to store different node types in the same collection.
/// `TResourceKey` defaults to [`Cow<'static, str>`](std::borrow::Cow) to match
/// [`Resources`](crate::resource::Resources); pass an enum key type for typed
/// resource lookups.
pub type DynNode<TState, TResourceKey = Cow<'static, str>> = dyn Node<TState, TResourceKey, PrepResult = DefaultNodeResult, ExecResult = DefaultNodeResult>
    + Send
    + Sync;

/// Type alias for an `Arc`-wrapped dynamic node trait object.
///
/// This alias simplifies working with dynamic node collections in workflows.
pub type NodeObject<TState, TResourceKey = Cow<'static, str>> =
    std::sync::Arc<DynNode<TState, TResourceKey>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::task::RetryMode;
    use cano_macros::node;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tokio;

    // Test enum for node return values
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum TestAction {
        #[allow(dead_code)]
        Continue,
        Complete,
        Error,
        #[allow(dead_code)]
        Retry,
    }

    // Simple test node that always succeeds
    struct SimpleSuccessNode {
        execution_count: Arc<AtomicU32>,
    }

    impl SimpleSuccessNode {
        fn new() -> Self {
            Self {
                execution_count: Arc::new(AtomicU32::new(0)),
            }
        }

        fn execution_count(&self) -> u32 {
            self.execution_count.load(Ordering::SeqCst)
        }
    }

    #[node]
    impl Node<TestAction> for SimpleSuccessNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
            Ok("prepared".to_string())
        }

        async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            prep_res == "prepared"
        }

        async fn post(
            &self,
            _res: &Resources,
            exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            if exec_res {
                Ok(TestAction::Complete)
            } else {
                Ok(TestAction::Error)
            }
        }
    }

    // Node that fails in prep phase
    struct PrepFailureNode {
        error_message: String,
    }

    impl PrepFailureNode {
        fn new(error_message: &str) -> Self {
            Self {
                error_message: error_message.to_string(),
            }
        }
    }

    #[node]
    impl Node<TestAction> for PrepFailureNode {
        type PrepResult = String;
        type ExecResult = bool;

        async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
            Err(CanoError::preparation(&self.error_message))
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            true
        }

        async fn post(
            &self,
            _res: &Resources,
            _exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            Ok(TestAction::Complete)
        }
    }

    // Node that fails in post phase
    struct PostFailureNode;

    #[node]
    impl Node<TestAction> for PostFailureNode {
        type PrepResult = ();
        type ExecResult = ();

        async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
            Ok(())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

        async fn post(
            &self,
            _res: &Resources,
            _exec_res: Self::ExecResult,
        ) -> Result<TestAction, CanoError> {
            Err(CanoError::node_execution("Post phase failure"))
        }
    }

    #[tokio::test]
    async fn test_simple_node_execution() {
        let node = SimpleSuccessNode::new();
        let res = Resources::new();

        let result = node.run(&res).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(node.execution_count(), 1);
    }

    #[tokio::test]
    async fn test_node_lifecycle_phases() {
        let node = SimpleSuccessNode::new();
        let res = Resources::new();

        // Test prep phase
        let prep_result = node.prep(&res).await.unwrap();
        assert_eq!(prep_result, "prepared");

        // Test exec phase
        let exec_result = node.exec(prep_result).await;
        assert!(exec_result);

        // Test post phase
        let post_result = node.post(&res, exec_result).await.unwrap();
        assert_eq!(post_result, TestAction::Complete);
    }

    #[tokio::test]
    async fn test_prep_phase_failure() {
        let node = PrepFailureNode::new("Test prep failure");
        let res = Resources::new();

        let result = node.run(&res).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Test prep failure"));
    }

    #[tokio::test]
    async fn test_post_phase_failure() {
        let node = PostFailureNode;
        let res = Resources::new();

        let result = node.run(&res).await;
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("Post phase failure"));
    }

    #[test]
    fn test_node_config_creation() {
        let config = TaskConfig::new();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_node_config_default() {
        let config = TaskConfig::default();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_node_config_minimal() {
        let config = TaskConfig::minimal();
        assert_eq!(config.retry_mode.max_attempts(), 1);
    }

    #[test]
    fn test_node_config_with_fixed_retry() {
        let config = TaskConfig::new().with_fixed_retry(5, Duration::from_millis(100));

        assert_eq!(config.retry_mode.max_attempts(), 6);
    }

    #[test]
    fn test_node_config_builder_pattern() {
        let config = TaskConfig::new().with_fixed_retry(10, Duration::from_secs(1));

        assert_eq!(config.retry_mode.max_attempts(), 11);
    }

    #[tokio::test]
    async fn test_multiple_node_executions() {
        let node = SimpleSuccessNode::new();
        let res = Resources::new();

        // Run the node multiple times
        for i in 1..=5 {
            let result = node.run(&res).await.unwrap();
            assert_eq!(result, TestAction::Complete);
            assert_eq!(node.execution_count(), i);
        }
    }

    #[tokio::test]
    async fn test_error_propagation() {
        let res = Resources::new();

        // Test prep phase error propagation
        let prep_fail_node = PrepFailureNode::new("Prep failed");
        let result = prep_fail_node.run(&res).await;
        assert!(result.is_err());

        // Test post phase error propagation
        let post_fail_node = PostFailureNode;
        let result = post_fail_node.run(&res).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_concurrent_node_execution() {
        use tokio::task;

        let node = Arc::new(SimpleSuccessNode::new());
        let res = Arc::new(Resources::new());

        let mut handles = vec![];

        // Spawn multiple concurrent executions
        for _ in 0..10 {
            let node_clone = Arc::clone(&node);
            let res_clone = Arc::clone(&res);

            let handle = task::spawn(async move { node_clone.run(&*res_clone).await });
            handles.push(handle);
        }

        // Wait for all executions to complete
        let mut success_count = 0;
        for handle in handles {
            let result = handle.await.unwrap();
            if result.is_ok() && result.unwrap() == TestAction::Complete {
                success_count += 1;
            }
        }

        assert_eq!(success_count, 10);
        assert_eq!(node.execution_count(), 10);
    }

    #[tokio::test]
    async fn test_node_config_retry_behavior() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Node that fails first few times then succeeds
        struct RetryNode {
            attempt_count: Arc<AtomicUsize>,
            max_retries: usize,
        }

        impl RetryNode {
            fn new(max_retries: usize) -> Self {
                Self {
                    attempt_count: Arc::new(AtomicUsize::new(0)),
                    max_retries,
                }
            }

            fn attempt_count(&self) -> usize {
                self.attempt_count.load(Ordering::SeqCst)
            }
        }

        #[node]
        impl Node<TestAction> for RetryNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::new().with_fixed_retry(self.max_retries, Duration::from_millis(1))
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                let attempt = self.attempt_count.fetch_add(1, Ordering::SeqCst) + 1;

                // Fail first 2 attempts, succeed on 3rd
                if attempt < 3 {
                    Err(CanoError::preparation("Simulated failure"))
                } else {
                    Ok(())
                }
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let res = Resources::new();

        // Test with sufficient retries
        let node_success = RetryNode::new(5);
        let result = node_success.run(&res).await.unwrap();
        assert_eq!(result, TestAction::Complete);
        assert_eq!(node_success.attempt_count(), 3); // Failed twice, succeeded on third attempt

        // Test with insufficient retries
        let node_failure = RetryNode::new(1);
        let result = node_failure.run(&res).await;
        assert!(result.is_err());
        assert_eq!(node_failure.attempt_count(), 2); // Initial attempt + 1 retry
    }

    #[tokio::test]
    async fn test_node_config_variants() {
        let res = Resources::new();

        // Test minimal config
        struct MinimalNode;

        #[node]
        impl Node<TestAction> for MinimalNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                Ok(())
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let minimal_node = MinimalNode;
        let result = minimal_node.run(&res).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        let config = minimal_node.config();
        assert_eq!(config.retry_mode.max_attempts(), 1);
    }

    #[test]
    fn test_retry_mode_none() {
        let retry_mode = RetryMode::None;

        assert_eq!(retry_mode.max_attempts(), 1);
        assert_eq!(retry_mode.delay_for_attempt(0), None);
        assert_eq!(retry_mode.delay_for_attempt(1), None);
    }

    #[test]
    fn test_retry_mode_fixed() {
        let retry_mode = RetryMode::fixed(3, Duration::from_millis(100));

        assert_eq!(retry_mode.max_attempts(), 4); // 1 initial + 3 retries

        // Test delay calculations
        assert_eq!(
            retry_mode.delay_for_attempt(0),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(1),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(2),
            Some(Duration::from_millis(100))
        );
        assert_eq!(retry_mode.delay_for_attempt(3), None); // No more retries
        assert_eq!(retry_mode.delay_for_attempt(4), None);
    }

    #[test]
    fn test_retry_mode_exponential_basic() {
        let retry_mode = RetryMode::exponential(3);

        assert_eq!(retry_mode.max_attempts(), 4); // 1 initial + 3 retries

        // Test that delays increase (exact values may vary due to jitter)
        let delay0 = retry_mode.delay_for_attempt(0).unwrap();
        let delay1 = retry_mode.delay_for_attempt(1).unwrap();
        let delay2 = retry_mode.delay_for_attempt(2).unwrap();

        // With exponential backoff, each delay should generally be larger
        // (allowing for some jitter variance)
        assert!(delay1.as_millis() >= delay0.as_millis() / 2); // Account for negative jitter
        assert!(delay2.as_millis() >= delay1.as_millis() / 2);

        // No delay for attempts beyond max_retries
        assert_eq!(retry_mode.delay_for_attempt(3), None);
        assert_eq!(retry_mode.delay_for_attempt(4), None);
    }

    #[test]
    fn test_retry_mode_exponential_custom() {
        let retry_mode = RetryMode::exponential_custom(
            2,                         // max_retries
            Duration::from_millis(50), // base_delay
            3.0,                       // multiplier
            Duration::from_secs(5),    // max_delay
            0.0,                       // no jitter
        );

        assert_eq!(retry_mode.max_attempts(), 3);

        // With no jitter, delays should be predictable
        // attempt 0: 50ms * 3^0 = 50ms
        // attempt 1: 50ms * 3^1 = 150ms
        // attempt 2: None (beyond max_retries)
        assert_eq!(
            retry_mode.delay_for_attempt(0),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(1),
            Some(Duration::from_millis(150))
        );
        assert_eq!(retry_mode.delay_for_attempt(2), None);
    }

    #[test]
    fn test_retry_mode_exponential_max_delay_cap() {
        let retry_mode = RetryMode::exponential_custom(
            5,
            Duration::from_millis(100), // base_delay
            10.0,                       // high multiplier
            Duration::from_millis(500), // low max_delay cap
            0.0,                        // no jitter
        );

        // All delays should be capped at max_delay
        let delay0 = retry_mode.delay_for_attempt(0).unwrap();
        let delay1 = retry_mode.delay_for_attempt(1).unwrap();
        let delay2 = retry_mode.delay_for_attempt(2).unwrap();

        assert_eq!(delay0, Duration::from_millis(100)); // 100 * 10^0 = 100
        assert_eq!(delay1, Duration::from_millis(500)); // 100 * 10^1 = 1000, capped to 500
        assert_eq!(delay2, Duration::from_millis(500)); // Capped to 500
    }

    #[test]
    fn test_retry_mode_exponential_jitter_bounds() {
        let retry_mode = RetryMode::exponential_custom(
            3,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            0.5, // 50% jitter
        );

        // Run multiple times to test jitter variability
        let mut delays = Vec::new();
        for _ in 0..20 {
            if let Some(delay) = retry_mode.delay_for_attempt(0) {
                delays.push(delay.as_millis());
            }
        }

        // With 50% jitter, delays should vary between 50ms and 150ms (100ms ± 50%)
        let min_delay = delays.iter().min().unwrap();
        let max_delay = delays.iter().max().unwrap();

        assert!(*min_delay >= 50); // 100ms - 50% = 50ms minimum
        assert!(*max_delay <= 150); // 100ms + 50% = 150ms maximum
    }

    #[test]
    fn test_retry_mode_jitter_clamping() {
        let retry_mode1 = RetryMode::exponential_custom(
            1,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            -0.5, // Should be clamped to 0.0
        );

        let retry_mode2 = RetryMode::exponential_custom(
            1,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            1.5, // Should be clamped to 1.0
        );

        assert!(retry_mode1.delay_for_attempt(0).is_some());
        assert!(retry_mode2.delay_for_attempt(0).is_some());
    }

    #[test]
    fn test_retry_mode_default() {
        let retry_mode = RetryMode::default();

        // Default should be exponential backoff with 3 retries
        assert_eq!(retry_mode.max_attempts(), 4);

        // Should have delays for first 3 attempts
        assert!(retry_mode.delay_for_attempt(0).is_some());
        assert!(retry_mode.delay_for_attempt(1).is_some());
        assert!(retry_mode.delay_for_attempt(2).is_some());
        assert!(retry_mode.delay_for_attempt(3).is_none());
    }

    #[test]
    fn test_retry_mode_builder_methods() {
        // Test the convenience constructor methods
        let fixed = RetryMode::fixed(2, Duration::from_millis(200));
        assert_eq!(fixed.max_attempts(), 3);

        let exponential = RetryMode::exponential(5);
        assert_eq!(exponential.max_attempts(), 6);

        // Test that exponential uses sensible defaults
        if let RetryMode::ExponentialBackoff {
            base_delay,
            multiplier,
            max_delay,
            jitter,
            ..
        } = exponential
        {
            assert_eq!(base_delay, Duration::from_millis(100));
            assert_eq!(multiplier, 2.0);
            assert_eq!(max_delay, Duration::from_secs(30));
            assert_eq!(jitter, 0.1);
        } else {
            panic!("Expected ExponentialBackoff variant");
        }
    }

    #[tokio::test]
    async fn test_retry_mode_in_node_execution() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Node that fails exactly N times before succeeding
        struct FailNTimesNode {
            fail_count: usize,
            attempt_counter: Arc<AtomicUsize>,
        }

        impl FailNTimesNode {
            fn new(fail_count: usize) -> Self {
                Self {
                    fail_count,
                    attempt_counter: Arc::new(AtomicUsize::new(0)),
                }
            }

            fn attempt_count(&self) -> usize {
                self.attempt_counter.load(Ordering::SeqCst)
            }
        }

        #[node]
        impl Node<TestAction> for FailNTimesNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::new().with_fixed_retry(5, Duration::from_millis(1))
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                let attempt = self.attempt_counter.fetch_add(1, Ordering::SeqCst);

                if attempt < self.fail_count {
                    Err(CanoError::preparation("Simulated failure"))
                } else {
                    Ok(())
                }
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let res = Resources::new();

        // Test successful retry after 2 failures
        let node1 = FailNTimesNode::new(2);
        let result1 = node1.run(&res).await.unwrap();
        assert_eq!(result1, TestAction::Complete);
        assert_eq!(node1.attempt_count(), 3); // Failed twice, succeeded on third

        // Test exhausting all retries
        let node2 = FailNTimesNode::new(10); // Fail more times than retries available
        let result2 = node2.run(&res).await;
        assert!(result2.is_err());
        assert_eq!(node2.attempt_count(), 6); // 1 initial + 5 retries
    }

    #[tokio::test]
    async fn test_retry_mode_timing() {
        use std::time::Instant;

        struct AlwaysFailNode;

        #[node]
        impl Node<TestAction> for AlwaysFailNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::new().with_fixed_retry(2, Duration::from_millis(50))
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                Err(CanoError::preparation("Always fails"))
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let res = Resources::new();
        let node = AlwaysFailNode;

        let start = Instant::now();
        let result = node.run(&res).await;
        let elapsed = start.elapsed();

        assert!(result.is_err());
        // Should take at least 100ms (2 retries * 50ms delay)
        // Allow some tolerance for test timing
        assert!(elapsed >= Duration::from_millis(90));
    }

    #[tokio::test]
    async fn test_retry_reruns_all_phases() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountedNode {
            prep_counter: Arc<AtomicUsize>,
            exec_counter: Arc<AtomicUsize>,
            post_counter: Arc<AtomicUsize>,
        }

        #[node]
        impl Node<TestAction> for CountedNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1))
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                self.prep_counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                self.exec_counter.fetch_add(1, Ordering::SeqCst);
            }

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                let count = self.post_counter.fetch_add(1, Ordering::SeqCst) + 1;
                if count < 2 {
                    Err(CanoError::node_execution("post fails first time"))
                } else {
                    Ok(TestAction::Complete)
                }
            }
        }

        let prep_counter = Arc::new(AtomicUsize::new(0));
        let exec_counter = Arc::new(AtomicUsize::new(0));
        let post_counter = Arc::new(AtomicUsize::new(0));

        let node = CountedNode {
            prep_counter: Arc::clone(&prep_counter),
            exec_counter: Arc::clone(&exec_counter),
            post_counter: Arc::clone(&post_counter),
        };
        let res = Resources::new();

        let result = node.run(&res).await.unwrap();
        assert_eq!(result, TestAction::Complete);

        assert_eq!(prep_counter.load(Ordering::SeqCst), 2, "prep ran twice");
        assert_eq!(exec_counter.load(Ordering::SeqCst), 2, "exec ran twice");
        assert_eq!(post_counter.load(Ordering::SeqCst), 2, "post ran twice");
    }

    #[tokio::test]
    async fn test_node_retry_exhausted_error_type() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct AlwaysFailNode {
            attempt_counter: Arc<AtomicUsize>,
        }

        #[node]
        impl Node<TestAction> for AlwaysFailNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1))
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                self.attempt_counter.fetch_add(1, Ordering::SeqCst);
                Err(CanoError::preparation("always fails"))
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let node = AlwaysFailNode {
            attempt_counter: Arc::new(AtomicUsize::new(0)),
        };
        let res = Resources::new();

        let result = node.run(&res).await;
        assert!(result.is_err());
        assert_eq!(node.attempt_counter.load(Ordering::SeqCst), 3);

        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::RetryExhausted(_)),
            "expected RetryExhausted after retry exhaustion, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_node_no_retry_preserves_error_variant() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct PrepFailNode {
            attempt_counter: Arc<AtomicUsize>,
        }

        #[node]
        impl Node<TestAction> for PrepFailNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                self.attempt_counter.fetch_add(1, Ordering::SeqCst);
                Err(CanoError::preparation("prep boom"))
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Ok(TestAction::Complete)
            }
        }

        let node = PrepFailNode {
            attempt_counter: Arc::new(AtomicUsize::new(0)),
        };
        let res = Resources::new();
        let err = node.run(&res).await.unwrap_err();

        assert_eq!(node.attempt_counter.load(Ordering::SeqCst), 1);
        assert!(
            matches!(err, CanoError::Preparation(_)),
            "expected original Preparation variant when retries disabled, got: {err}"
        );
        assert!(err.to_string().contains("prep boom"));

        struct PostFailNode;

        #[node]
        impl Node<TestAction> for PostFailNode {
            type PrepResult = ();
            type ExecResult = ();

            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }

            async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
                Ok(())
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {}

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> Result<TestAction, CanoError> {
                Err(CanoError::node_execution("post boom"))
            }
        }

        let err = PostFailNode.run(&res).await.unwrap_err();
        assert!(
            matches!(err, CanoError::NodeExecution(_)),
            "expected original NodeExecution variant when retries disabled, got: {err}"
        );
        assert!(err.to_string().contains("post boom"));
    }
}
