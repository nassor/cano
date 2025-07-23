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
//! ## 📊 Error Categories
//!
//! | Error Type | When It Occurs | How to Fix |
//! |------------|----------------|------------|
//! | `NodeExecution` | Node processing fails | Check your business logic |
//! | `Preparation` | Node prep phase fails | Verify input data and store |
//! | `store` | store operations fail | Check store state and keys |
//! | `Flow` | Workflow orchestration fails | Verify node registration and routing |
//! | `Configuration` | Invalid node/flow config | Check parameters and settings |
//! | `RetryExhausted` | All retries failed | Increase retries or fix root cause |
//! | `Generic` | General errors | Check the specific error message |
//!
//! ## 🔧 Using Errors in Your Nodes
//!
//! In your custom nodes, return specific error types for different failures.
//! Use `CanoError::Preparation` for prep phase issues, `CanoError::NodeExecution`
//! for exec phase problems, and handle errors appropriately in the post phase.
//!
//! ## 🔗 store Error Integration
//!
//! `CanoError` automatically understands and converts `StoreError` instances.
//! This means store operations can seamlessly flow into the broader error system:
//!
//! ```rust
//! use cano::{CanoResult, store::MemoryStore, store::Store};
//!
//! fn example_function() -> CanoResult<String> {
//!     let store = MemoryStore::new();
//!     // StoreError is automatically converted to CanoError::store
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
/// ### NodeExecution
/// Something went wrong in your node's business logic during the `exec` phase.
///
/// **Common causes:**
/// - Invalid input data
/// - External API failures  
/// - Business rule violations
/// - Resource unavailability
///
/// **How to fix:** Check your node's `exec` method logic and input validation.
///
/// ### Preparation  
/// Something went wrong while preparing data in the `prep` phase.
///
/// **Common causes:**
/// - Missing data in store
/// - Invalid data format
/// - Resource initialization failures
///
/// **How to fix:** Verify that previous nodes stored the expected data.
///
/// ### store
/// store operations failed (get, put, remove, etc.).
///
/// **Common causes:**
/// - Type mismatches when retrieving data
/// - store backend issues
/// - Concurrent access problems
///
/// **How to fix:** Check store keys and ensure type consistency.
///
/// ### Flow
/// Workflow orchestration problems.
///
/// **Common causes:**
/// - Unregistered node references
/// - Invalid action routing
/// - Circular dependencies
///
/// **How to fix:** Verify node registration and action string routing.
///
/// ### Configuration
/// Invalid node or flow configuration.
///
/// **Common causes:**
/// - Invalid concurrency settings
/// - Negative retry counts
/// - Conflicting settings
///
/// **How to fix:** Review node builder parameters and flow setup.
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
/// error variant (NodeExecution, Configuration, store, etc.) and provide
/// clear, actionable error messages that help users understand what went wrong.
///
/// ## 🔄 Converting from Other Errors
///
/// Cano errors can be created from various sources including standard library
/// errors, string slices, and owned strings. Use the appropriate constructor
/// method or the `Into` trait for convenient conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanoError {
    /// Error during node execution phase (your business logic)
    ///
    /// Use this when your node's `exec` method encounters an error in its
    /// core processing logic. Include specific details about what went wrong.
    NodeExecution(String),

    /// Error during node preparation phase (data loading/setup)
    ///
    /// Use this when your node's `prep` method fails to load or prepare data.
    /// Often indicates missing or invalid data in store.
    Preparation(String),

    /// Error in store operations (get/put/remove)
    ///
    /// Use this for store-related failures like missing keys, type mismatches,
    /// or store backend issues.
    Store(String),

    /// Error in flow orchestration (routing/registration)
    ///
    /// Use this for workflow-level problems like unregistered nodes,
    /// invalid action routing, or flow configuration issues.
    Flow(String),

    /// Error in node or flow configuration (invalid settings)
    ///
    /// Use this for configuration problems like invalid parameters,
    /// conflicting settings, or constraint violations.
    Configuration(String),

    /// All retry attempts have been exhausted
    ///
    /// This error is automatically generated when a node fails and
    /// all configured retry attempts have been used up.
    RetryExhausted(String),

    /// General-purpose error for other scenarios
    ///
    /// Use this for errors that don't fit the other categories.
    /// Try to be specific in the error message.
    Generic(String),
}

impl CanoError {
    /// Create a new node execution error
    pub fn node_execution<S: Into<String>>(msg: S) -> Self {
        CanoError::NodeExecution(msg.into())
    }

    /// Create a new preparation error
    pub fn preparation<S: Into<String>>(msg: S) -> Self {
        CanoError::Preparation(msg.into())
    }

    /// Create a new store error
    pub fn store<S: Into<String>>(msg: S) -> Self {
        CanoError::Store(msg.into())
    }

    /// Create a new flow error
    pub fn flow<S: Into<String>>(msg: S) -> Self {
        CanoError::Flow(msg.into())
    }

    /// Create a new configuration error
    pub fn configuration<S: Into<String>>(msg: S) -> Self {
        CanoError::Configuration(msg.into())
    }

    /// Create a new retry exhausted error
    pub fn retry_exhausted<S: Into<String>>(msg: S) -> Self {
        CanoError::RetryExhausted(msg.into())
    }

    /// Create a new generic error
    pub fn generic<S: Into<String>>(msg: S) -> Self {
        CanoError::Generic(msg.into())
    }

    /// Convert a StoreError to a CanoError
    ///
    /// This is a convenience method that explicitly converts store errors
    /// to workflow errors. The automatic `From` implementation also works.
    pub fn from_store_error(err: crate::store::error::StoreError) -> Self {
        CanoError::store(err.to_string())
    }

    /// Get the error message as a string slice
    pub fn message(&self) -> &str {
        match self {
            CanoError::NodeExecution(msg) => msg,
            CanoError::Preparation(msg) => msg,
            CanoError::Store(msg) => msg,
            CanoError::Flow(msg) => msg,
            CanoError::Configuration(msg) => msg,
            CanoError::RetryExhausted(msg) => msg,
            CanoError::Generic(msg) => msg,
        }
    }

    /// Get the error category as a string
    pub fn category(&self) -> &'static str {
        match self {
            CanoError::NodeExecution(_) => "node_execution",
            CanoError::Preparation(_) => "preparation",
            CanoError::Store(_) => "store",
            CanoError::Flow(_) => "flow",
            CanoError::Configuration(_) => "configuration",
            CanoError::RetryExhausted(_) => "retry_exhausted",
            CanoError::Generic(_) => "generic",
        }
    }
}

impl std::fmt::Display for CanoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanoError::NodeExecution(msg) => write!(f, "Node execution error: {msg}"),
            CanoError::Preparation(msg) => write!(f, "Preparation error: {msg}"),
            CanoError::Store(msg) => write!(f, "Store error: {msg}"),
            CanoError::Flow(msg) => write!(f, "Flow error: {msg}"),
            CanoError::Configuration(msg) => write!(f, "Configuration error: {msg}"),
            CanoError::RetryExhausted(msg) => write!(f, "Retry exhausted: {msg}"),
            CanoError::Generic(msg) => write!(f, "Error: {msg}"),
        }
    }
}

impl std::error::Error for CanoError {}

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
        CanoError::store(format!("IO error: {err}"))
    }
}

impl From<crate::store::error::StoreError> for CanoError {
    fn from(err: crate::store::error::StoreError) -> Self {
        CanoError::store(err.to_string())
    }
}

/// Convenient Result type alias for Cano operations
///
/// This type alias wraps the standard `Result<T, E>` with [`CanoError`] as the error type.
/// It's the recommended return type for all Cano-related functions that can fail.
///
/// ## 🎯 Why Use CanoResult?
///
/// Instead of writing `Result<String, CanoError>` everywhere, you can use `CanoResult<String>`.
/// This makes your function signatures cleaner and more consistent.
///
/// ## 📖 Examples
///
/// Use `CanoResult<T>` for functions that process data and might fail.
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
pub type CanoResult<T> = Result<T, CanoError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let error = CanoError::node_execution("Test error");
        assert_eq!(error.message(), "Test error");
        assert_eq!(error.category(), "node_execution");
    }

    #[test]
    fn test_error_display() {
        let error = CanoError::NodeExecution("Test error".to_string());
        assert_eq!(format!("{error}"), "Node execution error: Test error");
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
            CanoError::NodeExecution("".to_string()).category(),
            "node_execution"
        );
        assert_eq!(CanoError::store("".to_string()).category(), "store");
        assert_eq!(CanoError::Flow("".to_string()).category(), "flow");
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

        // Test explicit conversion method
        let store_error = StoreError::lock_error("lock failed");
        let cano_error = CanoError::from_store_error(store_error);
        assert_eq!(cano_error.category(), "store");
        assert!(cano_error.message().contains("lock failed"));
    }
}
