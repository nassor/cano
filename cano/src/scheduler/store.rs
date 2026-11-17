//! [`SchedulerStore`]: the shared record that lets several scheduler instances run one
//! set of flows, each tick on exactly one instance.
//!
//! The trait is a versioned compare-and-swap register keyed by flow id. The scheduler
//! keeps everything else (due checks, the run lease, backoff and trip state) in the
//! record's bytes, so a backend only stores and swaps opaque values. The built-in
//! stores in [`crate::store`] implement it; write your own with
//! `#[cano::scheduler_store] impl SchedulerStore for MyStore { … }` and check it with
//! `cano::testing::assert_scheduler_store_contract` (feature `testing`).

use crate::error::CanoError;
use cano_macros::scheduler_store;

/// A stored value and the version it was written at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedRecord {
    /// Opaque, store-assigned. Changes on every successful write; only equality matters.
    pub version: u64,
    /// The bytes last written with [`SchedulerStore::compare_and_swap`].
    pub value: Vec<u8>,
}

/// The result of [`SchedulerStore::compare_and_swap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasOutcome {
    /// The value was written; carries the record's new version.
    Written(u64),
    /// The record's current version is not the expected one; nothing was written.
    Conflict,
}

/// Versioned compare-and-swap storage for scheduler flow records.
///
/// Instances that share one store (one table, bucket or file) coordinate their flows
/// through it: a flow id names the same flow on every instance.
///
/// # Contract
///
/// - [`load`](Self::load) returns `None` for an id never written (or removed).
/// - [`compare_and_swap`](Self::compare_and_swap) with `expected = None` writes only when
///   no record exists; with `Some(v)` only when the current version is `v`. Otherwise it
///   returns [`CasOutcome::Conflict`] and leaves the record untouched. Of concurrent swaps
///   from one version, exactly one is written.
/// - A written record gets a version different from every earlier version of that id.
/// - A store that retries a swap whose acknowledgement was lost must report
///   [`CasOutcome::Written`] when the stored value equals the value it sent: the
///   scheduler's values carry the writer's identity and a timestamp, so equal bytes mean
///   the earlier attempt landed.
/// - [`remove`](Self::remove) deletes one id; removing an unknown id is `Ok`.
/// - Ids are arbitrary UTF-8 (spaces, `.`, `/`, `*`, non-ASCII, 200+ characters) and stay
///   isolated from each other.
///
/// Backends must be `Send + Sync + 'static`; every method may be called concurrently.
/// The scheduler fails any call that takes longer than its lease TTL, dropping the
/// future, so a backend must tolerate a swap cancelled mid-flight: it either landed or
/// it did not, and the scheduler re-reads the record either way.
#[scheduler_store]
pub trait SchedulerStore: Send + Sync + 'static {
    /// The current record for `flow_id`, or `None` when there is none.
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError>;

    /// Write `value` for `flow_id` if its current version is `expected`
    /// (`None`: the record must not exist).
    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError>;

    /// Delete the record for `flow_id`. A no-op for an unknown id.
    async fn remove(&self, flow_id: &str) -> Result<(), CanoError>;
}
