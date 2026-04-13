//! # Key-Value Store Helpers for Processing Pipelines
//!
//! This module provides optional helper utilities for common key-value store needs
//! in processing pipelines. These are convenience tools - you can use any storage
//! backend that meets your pipeline's requirements.
//!
//! ## 🎯 Purpose
//!
//! Processing pipelines often need to share data between stages:
//! - A data loader stores results for downstream processors
//! - Validation stages store results for audit trails
//! - Processing steps cache intermediate results
//!
//! This module provides common patterns for these use cases, but you're free
//! to use any storage solution that fits your needs.
//!
//! ## 🚀 Quick Start
//!
//! The included `MemoryStore` provides a simple in-memory key-value store
//! for basic pipeline data sharing needs. Use `put()` to store data and
//! `get()` to retrieve it with automatic type safety.
//!
//! ## 🔧 Design Philosophy
//!
//! ### Optional Helper, Not a Requirement
//!
//! These store helpers are optional conveniences. Your pipeline nodes can use:
//! - The provided `MemoryStore` for simple in-memory sharing
//! - Custom implementations of `KeyValueStore` for specific needs
//! - Any other storage solution that fits your architecture
//!
//! ### Type Safety with Flexibility
//!
//! The system uses `Box<dyn Any>` internally while providing type-safe access
//! through generic methods. Store any type and retrieve it safely - the system
//! handles type mismatches gracefully.
//!
//! ## 🏗️ Integration with Processing Nodes
//!
//! In pipeline nodes, use the store in different phases:
//! - `prep()`: Read input data from previous stages
//! - `exec()`: Process data (store is available but not required)
//! - `post()`: Store results for downstream stages
//!
//! ## 🔧 Available Store Options
//!
//! ### MemoryStore (Included)
//!
//! A thread-safe, in-memory HashMap-based store ready for immediate use.
//! Ideal for pipelines where data fits in memory and doesn't need persistence.
//!
//! ### Custom Key-Value Stores
//!
//! Implement the [`KeyValueStore`] trait for specialized backends like databases,
//! file systems, or distributed caches. The trait provides a simple interface
//! with `get`, `put`, `remove`, `append`, and utility methods.
//!
//! ## 💡 Best Practices
//!
//! ### Key Naming Conventions
//!
//! Use consistent, descriptive key names across your pipeline stages.
//! Clear keys improve maintainability (e.g., "user_profile", "validation_errors",
//! "processing_status").
//!
//! ### Memory and Performance
//!
//! - Choose appropriate data structures for your use case
//! - Clear temporary data when pipeline stages complete
//! - Consider using Copy-on-Write patterns for large data sets
//!
//! ### Error Handling
//!
//! Handle missing keys gracefully with sensible defaults. Use patterns like
//! `store.get("key").unwrap_or_default()` for robust pipeline behavior.

pub mod error;
pub mod memory;

use std::sync::Arc;

/// Type alias for store operation results
pub type StoreResult<TState> = Result<TState, error::StoreError>;

/// Core key-value store trait for pipeline data sharing
///
/// This trait provides a simple interface for key-value storage backends used in
/// processing pipelines. It's designed as a helper for common storage patterns,
/// not as a comprehensive database solution.
///
/// ## 🎯 Purpose
///
/// The `KeyValueStore` trait enables different storage backends while maintaining
/// a consistent API for pipeline data sharing:
/// - [`MemoryStore`]: Fast in-memory storage (included)
/// - File-based stores: For persistent pipeline data
/// - Database backends: For enterprise processing workflows
/// - Distributed caches: For scaled pipeline architectures
///
/// ## 🔧 Type Safety Approach
///
/// Uses type erasure with `Box<dyn Any>` internally while providing type-safe
/// access through generic methods. Store any type and retrieve it with automatic
/// type checking - mismatches return errors rather than panicking.
///
/// ## 🔒 Thread Safety
///
/// All `KeyValueStore` implementations must be thread-safe (`Send + Sync`) to support
/// concurrent access from async pipeline stages. This typically requires interior
/// mutability patterns like `Arc<Mutex<_>>` or `Arc<RwLock<_>>`.
///
/// ## 🚀 Implementation Guide
///
/// Implement this trait for custom storage backends. The interface is intentionally
/// simple to support a wide range of storage solutions while maintaining performance.
///
/// ## 📋 Method Reference
///
/// | Method | Purpose | Example Usage |
/// |--------|---------|---------------|
/// | `get` | Retrieve typed value | `let val: String = store.get("key")?` |
/// | `get_shared` | Retrieve Arc-wrapped value | `let val: Arc<String> = store.get_shared("key")?` |
/// | `put` | Store typed value | `store.put("key", value)?` |
/// | `remove` | Delete by key | `store.remove("key")?` |
/// | `append` | Add to collection | `store.append("list", item)?` |
/// | `contains_key` | Check key existence | `if store.contains_key("key")? { ... }` |
/// | `keys` | List all keys | `let keys: Vec<String> = store.keys()?` |
/// | `len` | Count stored items | `let count = store.len()?` |
/// | `clear` | Remove all data | `store.clear()?` |
///
/// ## Design Considerations
///
/// This trait prioritizes simplicity and flexibility over advanced features.
/// For complex storage needs, consider using specialized database libraries
/// alongside or instead of this helper interface.
///
/// ## Thread Safety Requirements
///
/// All implementations guarantee thread-safe access (`Send + Sync`).
/// Mutable operations typically require interior mutability patterns or
/// external synchronization mechanisms.
pub trait KeyValueStore: Send + Sync {
    /// Retrieve a typed value by key.
    ///
    /// Returns a clone of the stored value if the key exists and the value can be
    /// downcast to `TState`. The user is responsible for choosing whether to store
    /// values directly or via Copy-on-Write (`Arc<T>`).
    ///
    /// # Errors
    ///
    /// - [`StoreError::KeyNotFound`] — the key does not exist in the store
    /// - [`StoreError::TypeMismatch`] — the stored value cannot be downcast to `TState`
    /// - [`StoreError::LockError`] — the internal lock is poisoned
    fn get<TState: 'static + Clone>(&self, key: &str) -> StoreResult<TState>;

    /// Get a value wrapped in an `Arc` for zero-copy shared access.
    ///
    /// Returns the value wrapped in an `Arc<TState>`, avoiding an extra clone
    /// when sharing data across tasks. Backends that store values as `Arc<TState>`
    /// internally (such as [`MemoryStore`]) return the existing pointer directly.
    ///
    /// Unlike [`get`](Self::get), this method does **not** require `TState: Clone`,
    /// since the underlying value is never cloned — only the `Arc` pointer is.
    /// Backends that cannot provide zero-copy access may store their values inside
    /// an `Arc` internally and hand out clones of that pointer.
    ///
    /// [`MemoryStore`]: memory::MemoryStore
    fn get_shared<TState: 'static + Send + Sync>(&self, key: &str) -> StoreResult<Arc<TState>>;

    /// Store a typed value by key.
    ///
    /// Inserts or replaces the value at `key`. Both stack-allocated and heap-allocated
    /// types are accepted; choose the allocation strategy appropriate for your workload.
    ///
    /// # Errors
    ///
    /// - [`StoreError::LockError`] — the internal write lock is poisoned
    fn put<TState: 'static + Send + Sync + Clone>(
        &self,
        key: &str,
        value: TState,
    ) -> StoreResult<()>;

    /// Remove a value by key
    ///
    /// Removes the value associated with the key. Returns `Ok(())` whether or
    /// not the key existed — callers that need to distinguish the two cases
    /// should call [`contains_key`] first. Returns an error only if the
    /// underlying storage operation itself fails (e.g., a poisoned lock).
    ///
    /// [`contains_key`]: KeyValueStore::contains_key
    fn remove(&self, key: &str) -> StoreResult<()>;

    /// Append an item to an existing collection value
    ///
    /// If the key exists and contains a `Vec<TState>`, appends the item to it.
    /// If the key doesn't exist, creates a new `Vec<TState>` with the single item.
    /// Returns an error if the key exists but doesn't contain a `Vec<TState>`.
    fn append<TState: 'static + Send + Sync + Clone>(
        &self,
        key: &str,
        item: TState,
    ) -> StoreResult<()>;

    /// Check whether the store contains the given key
    ///
    /// Returns `Ok(true)` if the key exists, `Ok(false)` if it does not.
    /// Returns an error only if the storage backend itself fails.
    fn contains_key(&self, key: &str) -> StoreResult<bool>;

    /// Get all keys in the store
    ///
    /// Returns a `Vec<String>` of all keys currently stored.
    fn keys(&self) -> StoreResult<Vec<String>>;

    /// Get the number of key-value pairs in store
    ///
    /// Returns the count of currently stored items.
    fn len(&self) -> StoreResult<usize>;

    /// Check if the store is empty
    ///
    /// Returns `true` if the store contains no key-value pairs.
    /// This is a default implementation based on `len()`.
    fn is_empty(&self) -> StoreResult<bool> {
        self.len().map(|len| len == 0)
    }

    /// Clear all data from store
    ///
    /// Removes all key-value pairs from the store.
    fn clear(&self) -> StoreResult<()>;
}

pub use error::StoreError;
pub use memory::MemoryStore;
