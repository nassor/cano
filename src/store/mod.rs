//! # Storage API - Share Data Between Workflow Nodes
//!
//! This module provides simple, thread-safe storage for sharing data between nodes in workflows.
//! Think of it as a shared key-value store that any node can read from and write to.
//!
//! ## 🎯 Why Storage?
//!
//! In workflows, nodes often need to pass data to each other:
//! - A data loader puts results for a processor to use
//! - A validator stores validation results for an auditor
//! - A processor saves intermediate results for a reporter
//!
//! Storage makes this communication simple and type-safe.
//!
//! ## 🚀 Quick Start
//!
//! Use `MemoryStore` to store different types of data using key-value pairs.
//! Store data with `put()` and retrieve it later with `get()` - the storage
//! handles type safety automatically through generic methods.
//!
//! ## 🔧 How It Works
//!
//! ### Stack and Heap Values
//!
//! The storage system can accept both stack-based and heap-based values.
//! It's the user's responsibility to decide whether to use Copy-on-Write (Cow)
//! or direct value storage based on their performance requirements.
//!
//! ### Type Safety with Any
//!
//! Storage uses `Box<dyn Any>` internally but provides type-safe access.
//! When you request data with the correct type, it returns `Some(value)`.
//! When the type doesn't match, it safely returns `None`.
//!
//! ## 🏗️ Using Storage in Nodes
//!
//! In your custom nodes, use storage to read input data in the `prep()` phase,
//! process it in `exec()`, and store results in the `post()` phase for the next node.
//!
//! ## 🔧 Available Storage Types
//!
//! ### MemoryStore (Default)
//!
//! A thread-safe, in-memory HashMap-based storage that's ready to use out of the box.
//! Perfect for most workflows where data fits in memory.
//!
//! ### Custom Storage
//!
//! Implement the [`StoreTrait`] trait for custom backends like databases,
//! file systems, or network storage. The trait requires implementing
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
//! - Clear storage when workflows complete to free memory
//!
//! ### Error Handling
//!
//! Always handle missing keys gracefully by providing sensible defaults.
//! Use the `unwrap_or()` pattern to provide fallback values when data
//! might not be present in storage.

pub mod error;
pub mod memory;

/// Type alias for storage operation results
pub type StoreResult<T> = Result<T, error::StoreError>;

/// Core storage trait for workflow data sharing
///
/// This trait defines the interface for storage backends used in Cano workflows.
/// It provides a simple key-value store that any node can read from and write to.
///
/// ## 🎯 Purpose
///
/// The `StoreTrait` enables different storage backends while keeping the same API:
/// - [`MemoryStore`]: Fast in-memory storage (default)
/// - File-based storage: For persistent workflows
/// - Database storage: For enterprise workflows
/// - Network storage: For distributed workflows
///
/// ## 🔧 Type Safety
///
/// The storage system uses type erasure with `Box<dyn Any>` internally but provides
/// type-safe access through generic methods. Store data with automatic type inference
/// and retrieve with explicit type annotation for safety.
///
/// ## 🔒 Thread Safety Requirements
///
/// All `StoreTrait` implementations are always thread-safe (`Send + Sync`) to support
/// concurrent access from async tasks. This typically means using interior mutability
/// patterns like `Arc<Mutex<_>>` or `Arc<RwLock<_>>`.
///
/// ## 🚀 Implementation Example
///
/// A simple thread-safe storage implementation using `Arc<RwLock<HashMap>>`.
/// Custom storage backends can implement the StoreTrait to provide
/// persistence, distribution, or other specialized functionality.
///
/// ## 📋 Method Overview
///
/// | Method | Purpose | Example |
/// |--------|---------|---------|
/// | `get` | Retrieve a value | `let val: Result<T, StoreError> = storage.get("key")` |
/// | `put` | Store a value | `storage.put("key", value)?` |
/// | `remove` | Delete a key | `storage.remove("key")?` |
/// | `append` | Append to existing value | `storage.append("key", item)?` |
/// | `delete` | Alias for remove | `storage.delete("key")?` |
/// | `keys` | Get all keys | `for key in storage.keys()? { ... }` |
/// | `len` | Get count of items | `let count = storage.len()?` |
/// | `clear` | Remove all data | `storage.clear()?` |
///
/// ## Type Safety
///
/// The storage system uses type erasure with `Box<dyn Any + Send + Sync>` to allow
/// storing arbitrary types. Users must handle type safety through downcasting.
///
/// ## Implementation Requirements
///
/// All methods must be implemented to provide complete storage functionality.
/// The trait provides no default implementations to ensure optimal performance
/// for different storage backends.
///
/// ## Thread Safety
///
/// All implementations are guaranteed to be thread-safe (`Send + Sync`).
/// For mutable operations, this typically means using interior mutability patterns
/// or external synchronization.
pub trait StoreTrait: Send + Sync {
    /// Get a typed value by key
    ///
    /// Returns the value if the key exists and can be downcast to type T.
    /// Returns an error if the key doesn't exist or type mismatch occurs.
    /// The user is responsible for choosing whether to use
    /// Copy-on-Write or direct value storage.
    fn get<T: 'static + Clone>(&self, key: &str) -> StoreResult<T>;

    /// Store a typed value by key
    ///
    /// Stores the value under the given key. If the key already exists,
    /// the previous value is replaced. The storage accepts both stack and
    /// heap-based values - it's the user's responsibility to choose the
    /// appropriate allocation strategy.
    fn put<T: 'static + Send + Sync + Clone>(&self, key: &str, value: T) -> StoreResult<()>;

    /// Remove a value by key
    ///
    /// Removes the value associated with the key and returns Ok(()) if successful.
    /// Returns an error if the operation fails.
    fn remove(&self, key: &str) -> StoreResult<()>;

    /// Append an item to an existing collection value
    ///
    /// If the key exists and contains a `Vec<T>`, appends the item to it.
    /// If the key doesn't exist, creates a new `Vec<T>` with the single item.
    /// Returns an error if the key exists but doesn't contain a `Vec<T>`.
    fn append<T: 'static + Send + Sync + Clone>(&self, key: &str, item: T) -> StoreResult<()>;

    /// Delete a value by key (alias for remove)
    ///
    /// This is an alias for `remove()` to provide a more explicit API.
    /// Removes the value associated with the key and returns Ok(()) if successful.
    /// Returns an error if the operation fails.
    fn delete(&self, key: &str) -> StoreResult<()> {
        self.remove(key)
    }

    /// Get all keys in the storage
    ///
    /// Returns an iterator over all keys currently stored.
    fn keys(&self) -> StoreResult<Box<dyn Iterator<Item = String> + '_>>;

    /// Get the number of key-value pairs in storage
    ///
    /// Returns the count of currently stored items.
    fn len(&self) -> StoreResult<usize>;

    /// Check if the storage is empty
    ///
    /// Returns `true` if the storage contains no key-value pairs.
    /// This is a default implementation based on `len()`.
    fn is_empty(&self) -> StoreResult<bool> {
        self.len().map(|len| len == 0)
    }

    /// Clear all data from storage
    ///
    /// Removes all key-value pairs from the storage.
    fn clear(&self) -> StoreResult<()>;
}

pub use error::StoreError;
pub use memory::MemoryStore;
