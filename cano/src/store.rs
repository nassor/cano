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
//! These store helpers are optional conveniences. Your pipeline tasks can use:
//! - The provided `MemoryStore` for simple in-memory sharing
//! - Any custom resource type that exposes the storage API your workflow needs
//! - Any other storage solution that fits your architecture
//!
//! ### Type Safety with Flexibility
//!
//! The system uses `Box<dyn Any>` internally while providing type-safe access
//! through generic methods. Store any type and retrieve it safely - the system
//! handles type mismatches gracefully.
//!
//! ## 🏗️ Integration with Tasks
//!
//! In a `Task::run`, use the store to read inputs left by earlier states and
//! write results for downstream states.
//!
//! ## 🔧 Available Store Options
//!
//! ### MemoryStore (Included)
//!
//! A thread-safe, in-memory HashMap-based store ready for immediate use.
//! Ideal for pipelines where data fits in memory and doesn't need persistence.
//!
//! ### Custom Storage Resources
//!
//! Register specialized storage backends like databases, file systems, or
//! distributed caches directly in [`Resources`](crate::Resources). Tasks
//! retrieve them by concrete type and call the API exposed by that resource.
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

pub use error::StoreError;
pub use memory::MemoryStore;
