//! # Stores: the persistence backends
//!
//! **Availability:** since `0.17.0` · **not wasm-supported** (native only; requires `runtime-tokio`)
//!
//! A store is one storage backend. Each store implements the persistence traits cano
//! defines, [`CheckpointStore`](crate::checkpoint::CheckpointStore) and, with the
//! `scheduler` feature, `SchedulerStore` (the shared record behind several scheduler
//! instances), and each is behind its own feature:
//!
//! | Store | Feature | Since | Wasm | Backend |
//! |---|---|---|---|---|
//! | `RedbStore` | `store-redb`, on by default | `0.17.0` | **not wasm-supported** | an embedded, ACID, daemon-free [`redb`](https://docs.rs/redb) file, `cano.redb` by default; tables `cano_checkpoints`, `cano_scheduler` |
//! | `PostgresStore` (module `postgres`) | `store-postgres` | `0.17.0` | **not wasm-supported** | PostgreSQL; checkpoints in one table, `cano_checkpoints` by default, scheduler records in `cano_scheduler` |
//! | `NatsStore` (module `nats`) | `store-nats` | `0.17.0` | **not wasm-supported** | NATS JetStream Key/Value; checkpoints in one bucket, `cano_checkpoints` by default, scheduler records in `cano_scheduler` |
//!
//! To use another backend, implement the trait yourself: see
//! [`crate::checkpoint`].

#[cfg(any(feature = "store-redb", feature = "store-nats"))]
mod codec;
#[cfg(feature = "store-nats")]
pub mod nats;
#[cfg(feature = "store-postgres")]
pub mod postgres;
#[cfg(feature = "store-redb")]
mod redb;
#[cfg(any(feature = "store-postgres", feature = "store-nats"))]
mod retry;

#[cfg(feature = "store-nats")]
pub use nats::NatsStore;
#[cfg(feature = "store-postgres")]
pub use postgres::PostgresStore;
#[cfg(feature = "store-redb")]
pub use redb::RedbStore;

/// The error every built-in store returns when `(workflow_id, sequence)` already holds a row.
#[cfg(any(
    feature = "store-redb",
    feature = "store-postgres",
    feature = "store-nats"
))]
pub(crate) fn conflict_error(workflow_id: &str, sequence: u64) -> crate::CanoError {
    crate::CanoError::CheckpointStore(format!(
        "checkpoint conflict: workflow {workflow_id:?} already has a row at sequence {sequence}; \
         resume the existing run or clear it before starting a new one"
    ))
}
