//! # Error Handling - Clear, Actionable Error Messages
//!
//! This module provides comprehensive error handling for the Cano workflow engine.
//! It includes categorized error types that make it easy to understand what went wrong
//! and how to fix it.
//!
//! ## 🎯 Design Philosophy
//!
//! Cano's error handling is designed to be:
//! - **Clear**: Error messages explain what happened
//! - **Actionable**: Errors suggest how to fix the problem  
//! - **Categorized**: Different error types for different scenarios
//! - **Context-Rich**: Errors include relevant context information
//!
//! ## 🚀 Quick Start
//!
//! Use `CanoResult<T>` for functions that might fail and return different
//! `CanoError` variants to indicate specific failure types. Handle errors
//! with pattern matching to provide appropriate responses.
//!
//! ## Error Categories
//!
//! | Error Type | When It Occurs | How to Fix |
//! |------------|----------------|------------|
//! | `TaskExecution` | `Task::run` fails | Check your `run` method logic |
//! | `Store` | Store operations fail | Check store state and keys |
//! | `Workflow` | Workflow orchestration fails | Verify task registration and routing |
//! | `Configuration` | Invalid task/workflow config | Check parameters and settings |
//! | `RetryExhausted` | All retries exhausted | Increase retries or fix root cause |
//! | `Timeout` | Per-attempt timeout reached | Increase `attempt_timeout` or speed up the task |
//! | `CircuitOpen` | Circuit breaker rejected the call | Wait for the breaker's `reset_timeout` or fix the upstream dependency |
//! | `CheckpointStore` | Checkpoint persistence failed | Check the recovery store backend (disk, permissions, encoding) |
//! | `CompensationFailed` | A `compensate` run failed during rollback | Inspect the aggregated errors; the original failure is `errors[0]` |
//! | `Generic` | General errors | Check the specific error message |
//!
//! ## Using Errors in Your Tasks
//!
//! In your custom tasks, return specific error types for different failures.
//! Use `CanoError::TaskExecution` for errors from a `Task::run` implementation,
//! and `CanoError::Store` for store-access failures.
//!
//! ## Store Error Integration
//!
//! `CanoError` automatically converts `StoreError` instances via the `From` impl.
//! Store operations propagate seamlessly into the broader error system:
//!
//! ```rust
//! use cano::{CanoResult, MemoryStore};
//!
//! fn example_function() -> CanoResult<String> {
//!     let store = MemoryStore::new();
//!     // StoreError is automatically converted to CanoError::Store via From
//!     let value: String = store.get("key")?;
//!     Ok(value)
//! }
//! ```

/// Comprehensive error type for Cano workflows
///
/// This enum covers all the different ways things can go wrong in Cano workflows.
/// Each variant is designed to give you clear information about what happened
/// and how to fix it.
///
/// ## 🎯 Error Categories
///
/// ### TaskExecution
/// Something went wrong during a [`Task::run`](crate::task::Task::run) implementation.
///
/// **Common causes:**
/// - Failed store write
/// - Business-logic validation rejected an input
/// - External API call failed
///
/// **How to fix:** Check your task's `run` method logic and store writes.
///
/// ### Store
/// Store operations failed (get, put, remove, etc.).
///
/// **Common causes:**
/// - Type mismatches when retrieving data
/// - Store backend issues
/// - Concurrent access problems
///
/// **How to fix:** Check store keys and ensure type consistency.
///
/// ### Workflow
/// Workflow orchestration problems.
///
/// **Common causes:**
/// - Unregistered task references
/// - Invalid action routing
/// - Circular dependencies
///
/// **How to fix:** Verify task registration and action string routing.
///
/// ### Configuration
/// Invalid task or workflow configuration.
///
/// **Common causes:**
/// - Invalid concurrency settings
/// - Negative retry counts
/// - Conflicting settings
///
/// **How to fix:** Review task builder parameters and workflow setup.
///
/// ### RetryExhausted
/// All retry attempts have been exhausted.
///
/// **Common causes:**
/// - Persistent external failures
/// - Insufficient retry configuration
/// - Systemic issues
///
/// **How to fix:** Increase retry count, fix root cause, or add exponential backoff.
///
/// ## 💡 Usage Examples
///
/// Create specific error types with helpful messages. Use the appropriate
/// error variant (TaskExecution, Configuration, Store, etc.) and provide
/// clear, actionable error messages that help users understand what went wrong.
///
/// ## 🔄 Converting from Other Errors
///
/// Cano errors can be created from various sources including standard library
/// errors, string slices, and owned strings. Use the appropriate constructor
/// method or the `Into` trait for convenient conversion.
#[derive(Debug, Clone)]
pub enum CanoError {
    /// Error during task execution
    ///
    /// Use this when your task's `run` method encounters an error during
    /// execution.
    TaskExecution(String),

    /// Error in store operations (get/put/remove)
    ///
    /// Use this for store-related failures like missing keys, type mismatches,
    /// or store backend issues.
    Store(String),

    /// Error in workflow orchestration (routing/registration)
    ///
    /// Use this for workflow-level problems like unregistered tasks,
    /// invalid action routing, or workflow configuration issues.
    Workflow(String),

    /// Error in task or workflow configuration (invalid settings)
    ///
    /// Use this for configuration problems like invalid parameters,
    /// conflicting settings, or constraint violations.
    Configuration(String),

    /// All retry attempts have been exhausted
    ///
    /// This error is automatically generated when a task fails and
    /// all configured retry attempts have been used up.
    RetryExhausted(String),

    /// A bounded operation exceeded its deadline.
    ///
    /// Emitted by per-attempt task timeouts (see
    /// [`crate::task::TaskConfig::with_attempt_timeout`]). The retry loop treats
    /// this as a recoverable failure: each attempt that times out is retried
    /// (subject to the configured `RetryMode`), and if all attempts time out
    /// the loop wraps the final timeout error in
    /// [`CanoError::RetryExhausted`].
    Timeout(String),

    /// A call was rejected because the circuit breaker is open.
    ///
    /// Emitted by [`crate::circuit::CircuitBreaker::try_acquire`] (and surfaced through the
    /// retry loop in [`crate::task::run_with_retries`]). Unlike most failures this error
    /// short-circuits the retry loop: the breaker is signalling that the underlying
    /// dependency is unhealthy, and immediate retries would only add load.
    CircuitOpen(String),

    /// A checkpoint store operation failed.
    ///
    /// Emitted by [`crate::recovery::CheckpointStore`] implementations (including the
    /// built-in `RedbCheckpointStore`, behind the `recovery` feature) when an append,
    /// load, or clear cannot complete — for example a disk error, a permission
    /// problem, or a row that fails to encode/decode.
    CheckpointStore(String),

    /// A workflow failed and at least one [`compensate`](crate::saga::CompensatableTask::compensate)
    /// run also failed while rolling it back.
    ///
    /// `errors[0]` is the original failure that triggered compensation; `errors[1..]`
    /// are the compensation errors, in the order they occurred (LIFO over the
    /// compensation stack). A clean rollback — every `compensate` succeeded — does *not*
    /// produce this variant: the original error is returned unchanged instead.
    CompensationFailed {
        /// The original failure followed by every compensation error.
        errors: Vec<CanoError>,
    },

    /// General-purpose error for other scenarios
    ///
    /// Use this for errors that don't fit the other categories.
    /// Try to be specific in the error message.
    Generic(String),

    /// A resource key was not found in the Resources map
    ResourceNotFound(String),

    /// A resource was found under the key but its stored type did not match the requested type
    ResourceTypeMismatch(String),

    /// A resource key was inserted twice into the same Resources map
    ResourceDuplicateKey(String),
}

impl CanoError {
    /// Create a new task execution error
    pub fn task_execution<S: Into<String>>(msg: S) -> Self {
        CanoError::TaskExecution(msg.into())
    }

    /// Create a new store error
    pub fn store<S: Into<String>>(msg: S) -> Self {
        CanoError::Store(msg.into())
    }

    /// Create a new workflow error
    pub fn workflow<S: Into<String>>(msg: S) -> Self {
        CanoError::Workflow(msg.into())
    }

    /// Create a new configuration error
    pub fn configuration<S: Into<String>>(msg: S) -> Self {
        CanoError::Configuration(msg.into())
    }

    /// Create a new retry exhausted error
    pub fn retry_exhausted<S: Into<String>>(msg: S) -> Self {
        CanoError::RetryExhausted(msg.into())
    }

    /// Create a new timeout error
    pub fn timeout<S: Into<String>>(msg: S) -> Self {
        CanoError::Timeout(msg.into())
    }

    /// Create a new circuit-open error
    pub fn circuit_open<S: Into<String>>(msg: S) -> Self {
        CanoError::CircuitOpen(msg.into())
    }

    /// Create a new checkpoint-store error
    pub fn checkpoint_store<S: Into<String>>(msg: S) -> Self {
        CanoError::CheckpointStore(msg.into())
    }

    /// Create a new compensation-failed error from the original failure plus the
    /// compensation errors (the convention is `errors[0]` = original failure).
    pub fn compensation_failed(errors: Vec<CanoError>) -> Self {
        CanoError::CompensationFailed { errors }
    }

    /// Create a new generic error
    pub fn generic<S: Into<String>>(msg: S) -> Self {
        CanoError::Generic(msg.into())
    }

    /// Create a new resource not found error
    pub fn resource_not_found<S: Into<String>>(msg: S) -> Self {
        CanoError::ResourceNotFound(msg.into())
    }

    /// Create a new resource type mismatch error
    pub fn resource_type_mismatch<S: Into<String>>(msg: S) -> Self {
        CanoError::ResourceTypeMismatch(msg.into())
    }

    /// Create a new resource duplicate key error
    pub fn resource_duplicate_key<S: Into<String>>(msg: S) -> Self {
        CanoError::ResourceDuplicateKey(msg.into())
    }

    /// Get the error message as a string slice
    pub fn message(&self) -> &str {
        match self {
            CanoError::TaskExecution(msg) => msg,
            CanoError::Store(msg) => msg,
            CanoError::Workflow(msg) => msg,
            CanoError::Configuration(msg) => msg,
            CanoError::RetryExhausted(msg) => msg,
            CanoError::Timeout(msg) => msg,
            CanoError::CircuitOpen(msg) => msg,
            CanoError::CheckpointStore(msg) => msg,
            CanoError::CompensationFailed { errors } => errors
                .first()
                .map_or("compensation failed", CanoError::message),
            CanoError::Generic(msg) => msg,
            CanoError::ResourceNotFound(msg) => msg,
            CanoError::ResourceTypeMismatch(msg) => msg,
            CanoError::ResourceDuplicateKey(msg) => msg,
        }
    }

    /// Get the error category as a string
    pub fn category(&self) -> &'static str {
        match self {
            CanoError::TaskExecution(_) => "task_execution",
            CanoError::Store(_) => "store",
            CanoError::Workflow(_) => "workflow",
            CanoError::Configuration(_) => "configuration",
            CanoError::RetryExhausted(_) => "retry_exhausted",
            CanoError::Timeout(_) => "timeout",
            CanoError::CircuitOpen(_) => "circuit_open",
            CanoError::CheckpointStore(_) => "checkpoint_store",
            CanoError::CompensationFailed { .. } => "compensation_failed",
            CanoError::Generic(_) => "generic",
            CanoError::ResourceNotFound(_) => "resource_not_found",
            CanoError::ResourceTypeMismatch(_) => "resource_type_mismatch",
            CanoError::ResourceDuplicateKey(_) => "resource_duplicate_key",
        }
    }
}

impl std::fmt::Display for CanoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanoError::TaskExecution(msg) => write!(f, "Task execution error: {msg}"),
            CanoError::Store(msg) => write!(f, "Store error: {msg}"),
            CanoError::Workflow(msg) => write!(f, "Workflow error: {msg}"),
            CanoError::Configuration(msg) => write!(f, "Configuration error: {msg}"),
            CanoError::RetryExhausted(msg) => write!(f, "Retry exhausted: {msg}"),
            CanoError::Timeout(msg) => write!(f, "Timeout error: {msg}"),
            CanoError::CircuitOpen(msg) => write!(f, "Circuit open: {msg}"),
            CanoError::CheckpointStore(msg) => write!(f, "Checkpoint store error: {msg}"),
            CanoError::CompensationFailed { errors } => {
                write!(f, "Compensation failed ({} error(s))", errors.len())?;
                for (i, e) in errors.iter().enumerate() {
                    if i == 0 {
                        write!(f, "; original: {e}")?;
                    } else {
                        write!(f, "; compensation #{i}: {e}")?;
                    }
                }
                Ok(())
            }
            CanoError::Generic(msg) => write!(f, "Error: {msg}"),
            CanoError::ResourceNotFound(msg) => write!(f, "Resource not found: {msg}"),
            CanoError::ResourceTypeMismatch(msg) => write!(f, "Resource type mismatch: {msg}"),
            CanoError::ResourceDuplicateKey(msg) => write!(f, "Resource duplicate key: {msg}"),
        }
    }
}

impl std::error::Error for CanoError {}

impl PartialEq for CanoError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (CanoError::TaskExecution(a), CanoError::TaskExecution(b)) => a == b,
            (CanoError::Store(a), CanoError::Store(b)) => a == b,
            (CanoError::Workflow(a), CanoError::Workflow(b)) => a == b,
            (CanoError::Configuration(a), CanoError::Configuration(b)) => a == b,
            (CanoError::RetryExhausted(a), CanoError::RetryExhausted(b)) => a == b,
            (CanoError::Timeout(a), CanoError::Timeout(b)) => a == b,
            (CanoError::CircuitOpen(a), CanoError::CircuitOpen(b)) => a == b,
            (CanoError::CheckpointStore(a), CanoError::CheckpointStore(b)) => a == b,
            (
                CanoError::CompensationFailed { errors: a },
                CanoError::CompensationFailed { errors: b },
            ) => a == b,
            (CanoError::Generic(a), CanoError::Generic(b)) => a == b,
            (CanoError::ResourceNotFound(a), CanoError::ResourceNotFound(b)) => a == b,
            (CanoError::ResourceTypeMismatch(a), CanoError::ResourceTypeMismatch(b)) => a == b,
            (CanoError::ResourceDuplicateKey(a), CanoError::ResourceDuplicateKey(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for CanoError {}

// Conversion traits for ergonomic error handling

impl From<Box<dyn std::error::Error + Send + Sync>> for CanoError {
    fn from(err: Box<dyn std::error::Error + Send + Sync>) -> Self {
        CanoError::Generic(err.to_string())
    }
}

impl From<&str> for CanoError {
    fn from(err: &str) -> Self {
        CanoError::Generic(err.to_string())
    }
}

impl From<String> for CanoError {
    fn from(err: String) -> Self {
        CanoError::Generic(err)
    }
}

impl From<std::io::Error> for CanoError {
    fn from(err: std::io::Error) -> Self {
        CanoError::Generic(format!("IO error: {err}"))
    }
}

impl From<crate::store::error::StoreError> for CanoError {
    fn from(err: crate::store::error::StoreError) -> Self {
        CanoError::store(err.to_string())
    }
}

/// Convenient Result type alias for Cano operations
///
/// This type alias wraps the standard `Result<TState, E>` with [`CanoError`] as the error type.
/// It's the recommended return type for all Cano-related functions that can fail.
///
/// ## 🎯 Why Use CanoResult?
///
/// Instead of writing `Result<String, CanoError>` everywhere, you can use `CanoResult<String>`.
/// This makes your function signatures cleaner and more consistent.
///
/// ## 📖 Examples
///
/// Use `CanoResult<TState>` for functions that process data and might fail.
/// Return appropriate `CanoError` variants to indicate specific failure types.
/// Use pattern matching to handle results, or use the `?` operator to
/// propagate errors automatically in functions that return `CanoResult`.
///
/// ## 🔧 Common Patterns
///
/// ### Converting Other Errors
///
/// Convert from standard library errors using `map_err` and provide
/// descriptive error messages that help with debugging.
///
/// ### Chaining Operations
///
/// Use the `?` operator to chain operations and short-circuit on the first error.
/// This allows you to write clean, readable error-handling code that stops
/// execution as soon as any step fails.
pub type CanoResult<TState> = Result<TState, CanoError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let error = CanoError::task_execution("Test error");
        assert_eq!(error.message(), "Test error");
        assert_eq!(error.category(), "task_execution");
    }

    #[test]
    fn test_error_display() {
        let error = CanoError::TaskExecution("Test error".to_string());
        assert_eq!(format!("{error}"), "Task execution error: Test error");
    }

    #[test]
    fn test_error_conversions() {
        let error1: CanoError = "Test error".into();
        let error2: CanoError = "Test error".to_string().into();

        match (&error1, &error2) {
            (CanoError::Generic(msg1), CanoError::Generic(msg2)) => {
                assert_eq!(msg1, msg2);
            }
            _ => panic!("Expected Generic errors"),
        }
    }

    #[test]
    fn test_error_categories() {
        assert_eq!(
            CanoError::TaskExecution("".to_string()).category(),
            "task_execution"
        );
        assert_eq!(CanoError::store("".to_string()).category(), "store");
        assert_eq!(CanoError::Workflow("".to_string()).category(), "workflow");
    }

    #[test]
    fn test_store_error_conversion() {
        use crate::store::error::StoreError;

        // Test conversion from StoreError to CanoError
        let store_error = StoreError::key_not_found("test_key");
        let cano_error: CanoError = store_error.into();

        // Should be converted to store variant
        match cano_error {
            CanoError::Store(msg) => {
                assert!(msg.contains("test_key"));
                assert!(msg.contains("not found"));
            }
            _ => panic!("Expected store error variant"),
        }

        // Test that the category is correct
        let store_error = StoreError::type_mismatch("type error");
        let cano_error: CanoError = store_error.into();
        assert_eq!(cano_error.category(), "store");

        // Test conversion via From trait
        let store_error = StoreError::lock_error("lock failed");
        let cano_error: CanoError = store_error.into();
        assert_eq!(cano_error.category(), "store");
        assert!(cano_error.message().contains("lock failed"));
    }

    #[test]
    fn test_io_error_maps_to_generic() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let cano_err: CanoError = io_err.into();
        assert_eq!(cano_err.category(), "generic");
        assert!(cano_err.message().contains("IO error"));
        assert!(cano_err.message().contains("file missing"));
    }

    #[test]
    fn test_partial_eq_same_variant_same_message() {
        let a = CanoError::TaskExecution("oops".to_string());
        let b = CanoError::TaskExecution("oops".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn test_partial_eq_same_variant_different_message() {
        let a = CanoError::TaskExecution("a".to_string());
        let b = CanoError::TaskExecution("b".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn test_partial_eq_different_variants() {
        let a = CanoError::TaskExecution("msg".to_string());
        let b = CanoError::Workflow("msg".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn test_resource_not_found_constructor_and_category() {
        let err = CanoError::resource_not_found("missing key");
        assert_eq!(err.message(), "missing key");
        assert_eq!(err.category(), "resource_not_found");
        assert_eq!(format!("{err}"), "Resource not found: missing key");
    }

    #[test]
    fn test_resource_type_mismatch_constructor_and_category() {
        let err = CanoError::resource_type_mismatch("wrong type");
        assert_eq!(err.message(), "wrong type");
        assert_eq!(err.category(), "resource_type_mismatch");
        assert_eq!(format!("{err}"), "Resource type mismatch: wrong type");
    }

    #[test]
    fn test_resource_variants_partial_eq() {
        let a = CanoError::ResourceNotFound("k".to_string());
        let b = CanoError::ResourceNotFound("k".to_string());
        assert_eq!(a, b);

        let c = CanoError::ResourceNotFound("k".to_string());
        let d = CanoError::ResourceTypeMismatch("k".to_string());
        assert_ne!(c, d);

        let e = CanoError::ResourceTypeMismatch("t".to_string());
        let f = CanoError::ResourceTypeMismatch("t".to_string());
        assert_eq!(e, f);
    }

    #[test]
    fn test_timeout_constructor_and_category() {
        let err = CanoError::timeout("attempt deadline reached");
        assert_eq!(err.message(), "attempt deadline reached");
        assert_eq!(err.category(), "timeout");
        assert_eq!(format!("{err}"), "Timeout error: attempt deadline reached");
    }

    #[test]
    fn test_timeout_partial_eq() {
        let a = CanoError::timeout("d");
        let b = CanoError::timeout("d");
        assert_eq!(a, b);

        let c = CanoError::timeout("x");
        let d = CanoError::timeout("y");
        assert_ne!(c, d);

        // Distinct from same-message variants
        let timeout = CanoError::timeout("k");
        let workflow = CanoError::workflow("k");
        assert_ne!(timeout, workflow);
    }

    #[test]
    fn test_circuit_open_constructor_and_category() {
        let err = CanoError::circuit_open("breaker tripped");
        assert_eq!(err.message(), "breaker tripped");
        assert_eq!(err.category(), "circuit_open");
        assert_eq!(format!("{err}"), "Circuit open: breaker tripped");
    }

    #[test]
    fn test_circuit_open_partial_eq() {
        let a = CanoError::circuit_open("x");
        let b = CanoError::circuit_open("x");
        assert_eq!(a, b);

        let c = CanoError::circuit_open("x");
        let d = CanoError::circuit_open("y");
        assert_ne!(c, d);

        // Different variant with same message must not compare equal.
        let timeout = CanoError::timeout("x");
        let circuit = CanoError::circuit_open("x");
        assert_ne!(timeout, circuit);
    }

    #[test]
    fn test_checkpoint_store_constructor_and_category() {
        let err = CanoError::checkpoint_store("disk full");
        assert_eq!(err.message(), "disk full");
        assert_eq!(err.category(), "checkpoint_store");
        assert_eq!(format!("{err}"), "Checkpoint store error: disk full");
    }

    #[test]
    fn test_checkpoint_store_partial_eq() {
        let a = CanoError::checkpoint_store("x");
        let b = CanoError::checkpoint_store("x");
        assert_eq!(a, b);

        let c = CanoError::checkpoint_store("x");
        let d = CanoError::checkpoint_store("y");
        assert_ne!(c, d);

        // Different variant with the same message must not compare equal.
        let circuit = CanoError::circuit_open("x");
        let checkpoint = CanoError::checkpoint_store("x");
        assert_ne!(circuit, checkpoint);
    }

    #[test]
    fn test_compensation_failed_constructor_category_and_message() {
        let err = CanoError::compensation_failed(vec![
            CanoError::task_execution("charge declined"),
            CanoError::generic("refund timed out"),
        ]);
        assert_eq!(err.category(), "compensation_failed");
        // message() surfaces the original failure (errors[0]).
        assert_eq!(err.message(), "charge declined");
        let shown = format!("{err}");
        assert!(shown.contains("Compensation failed (2 error(s))"));
        assert!(shown.contains("original: Task execution error: charge declined"));
        assert!(shown.contains("compensation #1: Error: refund timed out"));

        // Empty-vec edge case: message() falls back to a static string.
        assert_eq!(
            CanoError::compensation_failed(vec![]).message(),
            "compensation failed"
        );
    }

    #[test]
    fn test_compensation_failed_partial_eq() {
        let a = CanoError::compensation_failed(vec![CanoError::generic("x")]);
        let b = CanoError::compensation_failed(vec![CanoError::generic("x")]);
        assert_eq!(a, b);

        let c = CanoError::compensation_failed(vec![CanoError::generic("x")]);
        let d =
            CanoError::compensation_failed(vec![CanoError::generic("x"), CanoError::generic("y")]);
        assert_ne!(c, d);

        // Distinct from a same-message string variant.
        assert_ne!(
            CanoError::compensation_failed(vec![CanoError::generic("x")]),
            CanoError::generic("x")
        );
    }

    #[test]
    fn test_resource_not_found_distinct_from_resource_type_mismatch() {
        let not_found = CanoError::resource_not_found("key");
        let mismatch = CanoError::resource_type_mismatch("key");
        // Same message, different variants — must not be equal
        assert_ne!(not_found, mismatch);
    }
}
