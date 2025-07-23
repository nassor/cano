//! # Store Error Types
//!
//! This module defines error types specific to store operations in Cano workflows.
//! It provides clear, actionable error messages for common store failures.

use std::fmt;

/// Comprehensive error type for store operations
///
/// This enum covers all the different ways store operations can fail.
/// Each variant provides specific information about what went wrong
/// and helps users understand how to fix the issue.
///
/// ## Error Categories
///
/// - `KeyNotFound`: The requested key doesn't exist in store
/// - `TypeMismatch`: The stored value can't be cast to the requested type
/// - `LockError`: Failed to acquire read/write lock on store
/// - `AppendTypeMismatch`: Tried to append to a value that isn't a `Vec<T>`
/// - `Generic`: General store errors with custom messages
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The requested key was not found in store
    ///
    /// This error occurs when trying to access a key that doesn't exist.
    /// Common causes: typos in key names, accessing data before it's stored,
    /// or data being removed by another operation.
    KeyNotFound(String),

    /// Type mismatch when retrieving stored value
    ///
    /// This error occurs when the stored value can't be downcast to the
    /// requested type. Common causes: storing one type and requesting another,
    /// or inconsistent type usage across nodes.
    TypeMismatch(String),

    /// Failed to acquire lock on store
    ///
    /// This error occurs when the store lock is poisoned or unavailable.
    /// Common causes: panic in another thread while holding the lock,
    /// or deadlock situations.
    LockError(String),

    /// Attempted to append to a value that isn't a `Vec<T>`
    ///
    /// This error occurs when trying to append to an existing key that
    /// contains a value of a different type than `Vec<T>`.
    AppendTypeMismatch(String),

    /// General store error with custom message
    ///
    /// Use this for store errors that don't fit other categories.
    /// Provide a descriptive message about what went wrong.
    Generic(String),
}

impl StoreError {
    /// Create a new key not found error
    pub fn key_not_found<S: Into<String>>(key: S) -> Self {
        StoreError::KeyNotFound(format!("Key '{}' not found in store", key.into()))
    }

    /// Create a new type mismatch error
    pub fn type_mismatch<S: Into<String>>(msg: S) -> Self {
        StoreError::TypeMismatch(msg.into())
    }

    /// Create a new lock error
    pub fn lock_error<S: Into<String>>(msg: S) -> Self {
        StoreError::LockError(msg.into())
    }

    /// Create a new append type mismatch error
    pub fn append_type_mismatch<S: Into<String>>(key: S) -> Self {
        StoreError::AppendTypeMismatch(format!(
            "Cannot append to key '{}': existing value is not a Vec<T>",
            key.into()
        ))
    }

    /// Create a new generic store error
    pub fn generic<S: Into<String>>(msg: S) -> Self {
        StoreError::Generic(msg.into())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::KeyNotFound(msg) => write!(f, "Key not found: {msg}"),
            StoreError::TypeMismatch(msg) => write!(f, "Type mismatch: {msg}"),
            StoreError::LockError(msg) => write!(f, "Lock error: {msg}"),
            StoreError::AppendTypeMismatch(msg) => write!(f, "Append type mismatch: {msg}"),
            StoreError::Generic(msg) => write!(f, "store error: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {}
