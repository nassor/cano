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
/// | `put` | Store typed value | `store.put("key", value)?` |
/// | `remove` | Delete by key | `store.remove("key")?` |
/// | `append` | Add to collection | `store.append("list", item)?` |
/// | `keys` | List all keys | `for key in store.keys()? { ... }` |
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
    /// Get a typed value by key
    ///
    /// Returns the value if the key exists and can be downcast to type T.
    /// Returns an error if the key doesn't exist or type mismatch occurs.
    /// The user is responsible for choosing whether to use
    /// Copy-on-Write or direct value store.
    fn get<TState: 'static + Clone>(&self, key: &str) -> StoreResult<TState>;

    /// Store a typed value by key
    ///
    /// Stores the value under the given key. If the key already exists,
    /// the previous value is replaced. The store accepts both stack and
    /// heap-based values - it's the user's responsibility to choose the
    /// appropriate allocation strategy.
    fn put<TState: 'static + Send + Sync + Clone>(
        &self,
        key: &str,
        value: TState,
    ) -> StoreResult<()>;

    /// Remove a value by key
    ///
    /// Removes the value associated with the key and returns Ok(()) if successful.
    /// Returns an error if the operation fails.
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

    /// Get all keys in the store
    ///
    /// Returns an iterator over all keys currently stored.
    fn keys(&self) -> StoreResult<Box<dyn Iterator<Item = String> + '_>>;

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
