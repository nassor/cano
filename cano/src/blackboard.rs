//! # Blackboard for Processing Pipelines
//!
//! This module provides an optional shared key-value blackboard for common needs
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
//! The included `Blackboard` provides a simple in-memory typed key-value map
//! for basic pipeline data sharing needs. Use `put()` to write data and
//! `get()` to retrieve it with automatic type safety.
//!
//! ## 🔧 Design Philosophy
//!
//! ### Optional Helper, Not a Requirement
//!
//! The blackboard is an optional convenience. Your pipeline tasks can use:
//! - The provided `Blackboard` for simple in-memory sharing
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
//! In a `Task::run`, use the blackboard to read inputs left by earlier states and
//! write results for downstream states.
//!
//! ## 🔧 Available Options
//!
//! ### Blackboard (Included)
//!
//! A thread-safe, in-memory HashMap-based blackboard ready for immediate use.
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
//! `board.get("key").unwrap_or_default()` for robust pipeline behavior.

pub mod error;
pub mod memory;

/// Type alias for blackboard operation results
pub type BlackboardResult<TState> = Result<TState, error::BlackboardError>;

pub use error::BlackboardError;
pub use memory::Blackboard;
