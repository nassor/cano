//! # store API - Share Data Between Workflow Nodes
//!
//! This module provides simple, thread-safe store for sharing data between nodes in workflows.
//! Think of it as a shared key-value store that any node can read from and write to.
//!
//! ## 🎯 Why store?
//!
//! In workflows, nodes often need to pass data to each other:
//! - A data loader puts results for a processor to use
//! - A validator stores validation results for an auditor
//! - A processor saves intermediate results for a reporter
//!
//! store makes this communication simple and type-safe.
//!
//! ## 🚀 Quick Start
//!
//! Use `MemoryStore` to store different types of data using key-value pairs.
//! Store data with `put()` and retrieve it later with `get()` - the store
//! handles type safety automatically through generic methods.
//!
//! ## 🔧 How It Works
//!
//! ### Stack and Heap Values
//!
//! The store system can accept both stack-based and heap-based values.
//! It's the user's responsibility to decide whether to use Copy-on-Write (Cow)
//! or direct value store based on their performance requirements.
//!
//! ### Type Safety with Any
//!
//! store uses `Box<dyn Any>` internally but provides type-safe access.
//! When you request data with the correct type, it returns `Some(value)`.
//! When the type doesn't match, it safely returns `None`.
//!
//! ## 🏗️ Using store in Nodes
//!
//! In your custom nodes, use store to read input data in the `prep()` phase,
//! process it in `exec()`, and store results in the `post()` phase for the next node.
//!
//! ## 🔧 Available store Types
//!
//! ### MemoryStore (Default)
//!
//! A thread-safe, in-memory HashMap-based store that's ready to use out of the box.
//! Perfect for most workflows where data fits in memory.
//!
//! ### Custom store
//!
//! Implement the [`Store`] trait for custom backends like databases,
//! file systems, or network store. The trait requires implementing
//! `get`, `put`, `remove`, `append`, `delete`, `keys`, `len`, and `clear` methods.
//!
//! ## 💡 Best Practices
//!
//! ### Key Naming
//!
//! Use consistent, descriptive key names throughout your workflow.
//! Good keys are clear about what data they contain (e.g., "user_profile",
//! "validation_errors", "processing_status").
//!
//! ### Memory Management
//!
//! - Choose between stack and heap allocation based on data size and lifetime
//! - Use Copy-on-Write (Cow) when you need memory efficiency
//! - Clear store when workflows complete to free memory
//!
//! ### Error Handling
//!
//! Always handle missing keys gracefully by providing sensible defaults.
//! Use the `unwrap_or()` pattern to provide fallback values when data
//! might not be present in store.

pub mod error;
pub mod memory;

/// Type alias for store operation results
pub type StoreResult<TState> = Result<TState, error::StoreError>;

/// Core store trait for workflow data sharing
///
/// This trait defines the interface for store backends used in Cano workflows.
/// It provides a simple key-value store that any node can read from and write to.
///
/// ## 🎯 Purpose
///
/// The `Store` enables different store backends while keeping the same API:
/// - [`MemoryStore`]: Fast in-memory store (default)
/// - File-based store: For persistent workflows
/// - Database store: For enterprise workflows
/// - Network store: For distributed workflows
///
/// ## 🔧 Type Safety
///
/// The store system uses type erasure with `Box<dyn Any>` internally but provides
/// type-safe access through generic methods. Store data with automatic type inference
/// and retrieve with explicit type annotation for safety.
///
/// ## 🔒 Thread Safety Requirements
///
/// All `Store` implementations are always thread-safe (`Send + Sync`) to support
/// concurrent access from async tasks. This typically means using interior mutability
/// patterns like `Arc<Mutex<_>>` or `Arc<RwLock<_>>`.
///
/// ## 🚀 Implementation Example
///
/// A simple thread-safe store implementation using `Arc<RwLock<HashMap>>`.
/// Custom store backends can implement the Store to provide
/// persistence, distribution, or other specialized functionality.
///
/// ## 📋 Method Overview
///
/// | Method | Purpose | Example |
/// |--------|---------|---------|
/// | `get` | Retrieve a value | `let val: Result<TState, StoreError> = store.get("key")` |
/// | `put` | Store a value | `store.put("key", value)?` |
/// | `remove` | Delete a key | `store.remove("key")?` |
/// | `append` | Append to existing value | `store.append("key", item)?` |
/// | `delete` | Alias for remove | `store.delete("key")?` |
/// | `keys` | Get all keys | `for key in store.keys()? { ... }` |
/// | `len` | Get count of items | `let count = store.len()?` |
/// | `clear` | Remove all data | `store.clear()?` |
///
/// ## Type Safety
///
/// The store system uses type erasure with `Box<dyn Any + Send + Sync>` to allow
/// storing arbitrary types. Users must handle type safety through downcasting.
///
/// ## Implementation Requirements
///
/// All methods must be implemented to provide complete store functionality.
/// The trait provides no default implementations to ensure optimal performance
/// for different store backends.
///
/// ## Thread Safety
///
/// All implementations are guaranteed to be thread-safe (`Send + Sync`).
/// For mutable operations, this typically means using interior mutability patterns
/// or external synchronization.
pub trait Store: Send + Sync {
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

    /// Delete a value by key (alias for remove)
    ///
    /// This is an alias for `remove()` to provide a more explicit API.
    /// Removes the value associated with the key and returns Ok(()) if successful.
    /// Returns an error if the operation fails.
    fn delete(&self, key: &str) -> StoreResult<()> {
        self.remove(key)
    }

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
