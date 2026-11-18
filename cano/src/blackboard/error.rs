//! # Blackboard Error Types
//!
//! This module defines error types specific to blackboard operations in Cano workflows.
//! It provides clear, actionable error messages for common blackboard failures.

use std::fmt;

/// Comprehensive error type for blackboard operations
///
/// This enum covers all the different ways blackboard operations can fail.
/// Each variant provides specific information about what went wrong
/// and helps users understand how to fix the issue.
///
/// ## Error Categories
///
/// - `KeyNotFound`: The requested key doesn't exist in the blackboard
/// - `TypeMismatch`: The stored value can't be cast to the requested type
/// - `AppendTypeMismatch`: Tried to append to a value that isn't a `Vec<TState>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlackboardError {
    /// The requested key was not found in the blackboard
    ///
    /// This error occurs when trying to access a key that doesn't exist.
    /// Common causes: typos in key names, accessing data before it's stored,
    /// or data being removed by another operation.
    KeyNotFound(String),

    /// Type mismatch when retrieving stored value
    ///
    /// This error occurs when the stored value can't be downcast to the
    /// requested type. Common causes: storing one type and requesting another,
    /// or inconsistent type usage across tasks.
    TypeMismatch(String),

    /// Attempted to append to a value that isn't a `Vec<TState>`
    ///
    /// This error occurs when trying to append to an existing key that
    /// contains a value of a different type than `Vec<TState>`.
    AppendTypeMismatch(String),
}

impl BlackboardError {
    /// Create a new key not found error
    pub fn key_not_found<S: Into<String>>(key: S) -> Self {
        BlackboardError::KeyNotFound(format!("Key '{}' not found in blackboard", key.into()))
    }

    /// Create a new type mismatch error
    pub fn type_mismatch<S: Into<String>>(msg: S) -> Self {
        BlackboardError::TypeMismatch(msg.into())
    }
    /// Create a new append type mismatch error
    pub fn append_type_mismatch<S: Into<String>>(key: S) -> Self {
        BlackboardError::AppendTypeMismatch(format!(
            "Cannot append to key '{}': existing value is not a Vec<TState>",
            key.into()
        ))
    }
}

impl fmt::Display for BlackboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlackboardError::KeyNotFound(msg) => write!(f, "Key not found: {msg}"),
            BlackboardError::TypeMismatch(msg) => write!(f, "Type mismatch: {msg}"),
            BlackboardError::AppendTypeMismatch(msg) => write!(f, "Append type mismatch: {msg}"),
        }
    }
}

impl std::error::Error for BlackboardError {}
