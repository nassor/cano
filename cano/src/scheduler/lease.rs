//! Multi-instance coordination for a [`Scheduler`](super::Scheduler) built with
//! [`with_store`](super::Scheduler::with_store): the stored flow record, the run
//! lease, and the claim, renew and release protocol over a [`SchedulerStore`].
//!
//! The store only keeps opaque bytes per flow id. Every decision (is the flow due,
//! is it tripped, who holds the lease) is made here from the decoded [`FlowRecord`],
//! and every change is one compare-and-swap from the version that was read.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::{CanoError, CanoResult};

use super::{BackoffPolicy, CasOutcome, FlowCell, FlowRuntime, SchedulerStore, Status};

/// Swaps attempted per claim, release or reset while other instances keep winning.
const CAS_ATTEMPTS: usize = 5;
/// Store errors tolerated, after the first, while releasing a lease.
const RELEASE_RETRIES: u32 = 3;
/// Retry `n` of a release waits `n` times this.
const RELEASE_BACKOFF: Duration = Duration::from_millis(100);

/// `t + d`, saturating at the far future instead of panicking.
pub(super) fn plus(t: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    chrono::Duration::from_std(d)
        .ok()
        .and_then(|d| t.checked_add_signed(d))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// What one more failure does to a flow whose streak is `streak`: the new streak and,
/// unless `policy` trips the flow, the end of the backoff window.
pub(super) fn after_failure(
    policy: &BackoffPolicy,
    streak: u32,
    now: DateTime<Utc>,
) -> (u32, Option<DateTime<Utc>>) {
    let streak = streak.saturating_add(1);
    if policy.is_tripped(streak) {
        return (streak, None);
    }
    (streak, Some(plus(now, policy.compute_delay(streak))))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum FlowState {
    #[default]
    Idle,
    Completed,
    Backoff,
    Tripped,
}

/// The right of one instance to run a flow until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Lease {
    pub(super) owner: String,
    /// Per-process `Coordinator` session token so a crashed and fast-restarted process
    /// sharing a static `instance_id` does not treat its pre-crash lease as its own.
    #[serde(default)]
    pub(super) session: u64,
    /// Monotonic per-claim lease id so a thawed zombie or dropped claim cleanup can never
    /// renew or release a newer run's lease on the same instance.
    #[serde(default)]
    pub(super) id: u64,
    pub(super) expires_at: DateTime<Utc>,
}

/// Everything the cluster knows about one flow; the bytes behind a store key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct FlowRecord {
    pub(super) run_count: u64,
    /// Start of the last claimed run.
    pub(super) last_run: Option<DateTime<Utc>>,
    /// The cron firing the last claimed run belongs to.
    pub(super) last_tick: Option<DateTime<Utc>>,
    pub(super) failure_streak: u32,
    pub(super) next_eligible: Option<DateTime<Utc>>,
    pub(super) last_error: Option<String>,
    pub(super) state: FlowState,
    pub(super) lease: Option<Lease>,
    /// Writer, write time and per-coordinator counter: two writes never have equal
    /// bytes, even within one clock tick, so a store that retries an ambiguous swap can
    /// tell its own earlier write from a rival's.
    pub(super) written_by: String,
    pub(super) written_at: DateTime<Utc>,
    pub(super) write_seq: u64,
}

impl FlowRecord {
    fn decode(flow_id: &str, bytes: &[u8]) -> Result<Self, CanoError> {
        serde_json::from_slice(bytes).map_err(|e| {
            CanoError::scheduler_store(format!("flow '{flow_id}': undecodable record: {e}"))
        })
    }

    /// The status instance `(me, session)` shows for this record at `now`.
    fn status(&self, me: &str, session: u64, now: DateTime<Utc>) -> Status {
        if let Some(lease) = &self.lease
            && (lease.owner != me || lease.session != session)
            && lease.expires_at > now
        {
            return Status::RunningElsewhere {
                owner: Arc::from(lease.owner.as_str()),
                lease_expires: lease.expires_at,
            };
        }
        let last_error = || Arc::<str>::from(self.last_error.as_deref().unwrap_or_default());
        match self.state {
            FlowState::Idle => Status::Idle,
            FlowState::Completed => Status::Completed,
            FlowState::Backoff => Status::Backoff {
                until: self.next_eligible.unwrap_or(self.written_at),
                streak: self.failure_streak,
                last_error: last_error(),
            },
            FlowState::Tripped => Status::Tripped {
                streak: self.failure_streak,
                last_error: last_error(),
            },
        }
    }

    fn in_backoff(&self, now: DateTime<Utc>) -> bool {
        self.next_eligible.is_some_and(|until| until > now)
    }

    /// A failed run: bump the streak, then trip or open a backoff window.
    fn fail(&mut self, policy: &BackoffPolicy, error: String, now: DateTime<Utc>) {
        let (streak, until) = after_failure(policy, self.failure_streak, now);
        self.failure_streak = streak;
        self.next_eligible = until;
        self.state = if until.is_some() {
            FlowState::Backoff
        } else {
            FlowState::Tripped
        };
        self.last_error = Some(error);
    }

    fn succeed(&mut self) {
        self.failure_streak = 0;
        self.next_eligible = None;
        self.last_error = None;
        self.state = FlowState::Completed;
    }

    /// A deliberate cancel is not a fault: leave streak and window untouched, restoring
    /// `Backoff` if an earlier backoff window is still active at `now`.
    fn cancelled(&mut self, now: DateTime<Utc>) {
        if self.failure_streak > 0 && self.in_backoff(now) {
            self.state = FlowState::Backoff;
        } else {
            self.state = FlowState::Idle;
        }
    }
}

#[cfg(feature = "metrics")]
fn count_failure(flow_id: &str, record: &FlowRecord) {
    if record.state == FlowState::Tripped {
        crate::metrics::scheduler_flow_tripped(flow_id);
    } else {
        crate::metrics::scheduler_flow_backoff(flow_id);
    }
}

/// When a dispatch attempt is allowed to claim the flow.
#[derive(Debug, Clone, Copy)]
pub(super) enum Due {
    /// `last_run + interval` has passed and no backoff window is open.
    Every(Duration),
    /// This cron firing has not been claimed yet and no backoff window is open.
    Cron(DateTime<Utc>),
    /// A manual trigger: always due, ignores backoff.
    Trigger,
}

impl Due {
    fn is_due(self, record: &FlowRecord, now: DateTime<Utc>) -> bool {
        match self {
            Due::Trigger => true,
            Due::Every(interval) => {
                !record.in_backoff(now)
                    && record
                        .last_run
                        .is_none_or(|last| plus(last, interval) <= now)
            }
            Due::Cron(tick) => {
                !record.in_backoff(now) && record.last_tick.is_none_or(|last| last < tick)
            }
        }
    }
}

/// Why a claim was not won.
#[derive(Debug, Clone)]
pub(super) enum Refusal {
    /// Another instance holds a live lease.
    Elsewhere {
        owner: Arc<str>,
        lease_expires: DateTime<Utc>,
    },
    Tripped,
    NotDue,
}

pub(super) enum Claim {
    /// This instance owns the run.
    Won(HeldLease),
    /// Not claimed; `record` is the latest state, for mirroring.
    Refused { reason: Refusal, record: FlowRecord },
}

/// A lease this instance holds, with the record version it was last written at.
pub(super) struct HeldLease {
    version: u64,
    id: u64,
    record: FlowRecord,
}

impl HeldLease {
    pub(super) fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn record(&self) -> &FlowRecord {
        &self.record
    }

    /// Time left on the lease as last written, zero once it ran out.
    fn remaining(&self, now: DateTime<Utc>) -> Duration {
        self.record.lease.as_ref().map_or(Duration::ZERO, |lease| {
            (lease.expires_at - now).to_std().unwrap_or(Duration::ZERO)
        })
    }
}

/// The result of recording a finished run.
pub(super) enum Release {
    /// The outcome was written and the lease cleared.
    Written(FlowRecord),
    /// The lease was no longer ours (it expired and was taken over, or the record is
    /// gone); nothing was written.
    NotHeld(Option<FlowRecord>),
    /// The store kept failing; the lease is left to expire.
    GaveUp,
}

enum ReleaseAttempt {
    Written(FlowRecord),
    NotHeld(Option<FlowRecord>),
    Conflict,
}

enum Renew {
    Held,
    Lost(Option<FlowRecord>),
}

/// One scheduler instance's view of the shared store.
pub(super) struct Coordinator {
    store: Arc<dyn SchedulerStore>,
    instance_id: Arc<str>,
    session: u64,
    ttl: Duration,
    writes: AtomicU64,
    leases: AtomicU64,
}

/// Run a store call so that a panic inside a custom store becomes an error instead of
/// ending the dispatch task with the flow stuck in `Running`, and a call that never
/// returns fails after `limit` (one lease TTL) instead of holding the flow forever.
/// A call that outlives a TTL could not have kept any lease it wrote anyway.
async fn guarded<T>(
    limit: Duration,
    call: impl Future<Output = Result<T, CanoError>>,
) -> Result<T, CanoError> {
    match tokio::time::timeout(limit, std::panic::AssertUnwindSafe(call).catch_unwind()).await {
        Ok(Ok(result)) => result,
        Ok(Err(payload)) => Err(CanoError::scheduler_store(format!(
            "scheduler store panicked: {}",
            crate::workflow::panic_payload_message(&*payload)
        ))),
        Err(_) => Err(CanoError::scheduler_store(format!(
            "scheduler store call timed out after {limit:?}"
        ))),
    }
}

#[cfg(feature = "tracing")]
fn log_store_failure(flow_id: &str, what: &str, error: &CanoError) {
    tracing::warn!(flow_id, error = %error, "scheduler store: {what}");
}

#[cfg(not(feature = "tracing"))]
fn log_store_failure(_flow_id: &str, _what: &str, _error: &CanoError) {}

fn contended(flow_id: &str, what: &str) -> CanoError {
    CanoError::scheduler_store(format!(
        "flow '{flow_id}': {what} lost {CAS_ATTEMPTS} compare-and-swap races in a row"
    ))
}

impl Coordinator {
    pub(super) fn new(
        store: Arc<dyn SchedulerStore>,
        instance_id: Arc<str>,
        ttl: Duration,
    ) -> Self {
        Self {
            store,
            instance_id,
            session: rand::random::<u64>().max(1),
            ttl,
            writes: AtomicU64::new(0),
            leases: AtomicU64::new(0),
        }
    }

    pub(super) fn next_lease_id(&self) -> u64 {
        self.leases.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn is_same_session(&self, lease: &Lease) -> bool {
        *lease.owner == *self.instance_id && lease.session == self.session
    }

    fn matches_lease(&self, lease: &Lease, lease_id: u64) -> bool {
        self.is_same_session(lease) && lease.id == lease_id
    }

    #[cfg(feature = "tracing")]
    pub(super) fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// The record and its version; `None` when the flow was never written.
    async fn load(&self, flow_id: &str) -> Result<Option<(u64, FlowRecord)>, CanoError> {
        guarded(self.ttl, self.store.load(flow_id))
            .await?
            .map(|stored| {
                FlowRecord::decode(flow_id, &stored.value).map(|record| (stored.version, record))
            })
            .transpose()
    }

    /// Best-effort read for mirroring; a failure reads as "unknown".
    pub(super) async fn snapshot(&self, flow_id: &str) -> Option<FlowRecord> {
        match self.load(flow_id).await {
            Ok(loaded) => Some(loaded.map_or_else(FlowRecord::default, |(_, record)| record)),
            Err(error) => {
                log_store_failure(flow_id, "reading the record failed", &error);
                None
            }
        }
    }

    /// Read every flow's record once and mirror it into the flow's cell, so the first
    /// `status` call after `start` already shows the cluster's state. A flow whose read
    /// fails keeps its default status.
    pub(super) async fn preload(&self, flows: &[(Arc<str>, FlowRuntime)]) {
        let records =
            futures_util::future::join_all(flows.iter().map(|(id, _)| self.snapshot(id))).await;
        for ((_, rt), record) in flows.iter().zip(records) {
            if let Some(record) = record {
                rt.cell.send_modify(|cell| self.mirror(cell, &record));
            }
        }
    }

    /// Stamp and write `record` if the stored version is still `expected`.
    /// `Some(version)` when written, `None` on a conflict.
    async fn swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        record: &mut FlowRecord,
    ) -> Result<Option<u64>, CanoError> {
        record.written_by = self.instance_id.to_string();
        record.written_at = Utc::now();
        record.write_seq = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        let bytes = serde_json::to_vec(record).map_err(|e| {
            CanoError::scheduler_store(format!("flow '{flow_id}': record encoding failed: {e}"))
        })?;
        Ok(
            match guarded(
                self.ttl,
                self.store.compare_and_swap(flow_id, expected, bytes),
            )
            .await?
            {
                CasOutcome::Written(version) => Some(version),
                CasOutcome::Conflict => None,
            },
        )
    }

    /// Try to take the flow for one run. See the module docs of
    /// [`Scheduler`](super::Scheduler) for the rules; each attempt is one compare-and-swap
    /// from the loaded version, re-loaded when another instance wins it.
    ///
    /// When `shutdown` is given (a scheduled loop), firing it before a claim swap starts
    /// aborts the attempt with `Ok(None)` so `stop` never waits on a hung read or starts
    /// a new swap after shutdown.
    pub(super) async fn claim(
        &self,
        flow_id: &str,
        lease_id: u64,
        policy: &BackoffPolicy,
        due: Due,
        shutdown: Option<&CancellationToken>,
        sent_lease: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<Claim>, CanoError> {
        for _ in 0..CAS_ATTEMPTS {
            let loaded = match shutdown {
                Some(stop) => tokio::select! {
                    biased;
                    _ = stop.cancelled() => return Ok(None),
                    res = self.load(flow_id) => res?,
                },
                None => self.load(flow_id).await?,
            };
            if shutdown.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let (version, mut record) = match loaded {
                Some((version, record)) => (Some(version), record),
                None => (None, FlowRecord::default()),
            };
            let now = Utc::now();

            let mut abandoned = false;
            if let Some(lease) = record.lease.clone() {
                if lease.expires_at > now {
                    if !self.is_same_session(&lease) {
                        return Ok(Some(Claim::Refused {
                            reason: Refusal::Elsewhere {
                                owner: Arc::from(lease.owner.as_str()),
                                lease_expires: lease.expires_at,
                            },
                            record,
                        }));
                    }
                    // Our own live lease from this same Coordinator session with no run
                    // in flight: a release that gave up. Take it over without counting a
                    // failure.
                } else {
                    // The owner stopped renewing: count the lost run as a failure,
                    // and write that even when the claim below is refused.
                    record.fail(
                        policy,
                        format!("lease expired: instance '{}' stopped renewing", lease.owner),
                        now,
                    );
                    record.lease = None;
                    abandoned = true;
                }
            }

            let refusal = if record.state == FlowState::Tripped {
                Some(Refusal::Tripped)
            } else if !due.is_due(&record, now) {
                Some(Refusal::NotDue)
            } else {
                None
            };

            if let Some(reason) = refusal {
                if !abandoned {
                    return Ok(Some(Claim::Refused { reason, record }));
                }
                if self.swap(flow_id, version, &mut record).await?.is_some() {
                    #[cfg(feature = "metrics")]
                    count_failure(flow_id, &record);
                    return Ok(Some(Claim::Refused { reason, record }));
                }
                continue;
            }

            record.lease = Some(Lease {
                owner: self.instance_id.to_string(),
                session: self.session,
                id: lease_id,
                expires_at: plus(now, self.ttl),
            });
            record.run_count = record.run_count.saturating_add(1);
            record.last_run = Some(now);
            if let Due::Cron(tick) = due {
                record.last_tick = Some(tick);
            }
            sent_lease.store(true, Ordering::Relaxed);
            if let Some(version) = self.swap(flow_id, version, &mut record).await? {
                #[cfg(feature = "metrics")]
                if abandoned {
                    count_failure(flow_id, &record);
                }
                return Ok(Some(Claim::Won(HeldLease {
                    version,
                    id: lease_id,
                    record,
                })));
            }
        }
        Err(contended(flow_id, "claim"))
    }

    /// Renew `held` every `ttl / 3` for as long as it can; returns once the lease is
    /// lost, carrying the replacing record when another owner took it over (`None` when
    /// the record was removed or the lease ran out without an answer from the store). It
    /// runs alongside the flow and is dropped when the flow ends, so every await point
    /// keeps `held` consistent with what the store acknowledged or will be re-synchronised
    /// by the next conflict.
    pub(super) async fn keep_alive(
        &self,
        flow_id: &str,
        held: &mut HeldLease,
    ) -> Option<FlowRecord> {
        let period = (self.ttl / 3).max(Duration::from_millis(1));
        loop {
            // Never sleep past the expiry: a lease that ran out is reported at once.
            tokio::time::sleep(period.min(held.remaining(Utc::now()))).await;
            if let Renew::Lost(record) = self.renew(flow_id, held).await {
                return record;
            }
        }
    }

    /// One renewal round: `Lost` when the lease ran out or belongs to another owner (or
    /// no record is stored); `Held` when renewed, or on a store problem to retry next round.
    async fn renew(&self, flow_id: &str, held: &mut HeldLease) -> Renew {
        for _ in 0..CAS_ATTEMPTS {
            let now = Utc::now();
            let remaining = held.remaining(now);
            if remaining.is_zero() {
                return Renew::Lost(None);
            }
            // No store call may outlive the lease it is renewing.
            let deadline = crate::workflow::deadline_after(remaining);

            let mut next = held.record.clone();
            next.lease = Some(Lease {
                owner: self.instance_id.to_string(),
                session: self.session,
                id: held.id,
                expires_at: plus(now, self.ttl),
            });
            match tokio::time::timeout_at(
                deadline,
                self.swap(flow_id, Some(held.version), &mut next),
            )
            .await
            {
                Ok(Ok(Some(version))) => {
                    held.version = version;
                    held.record = next;
                    return Renew::Held;
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    log_store_failure(flow_id, "lease renewal failed", &error);
                    return Renew::Held;
                }
                Err(_) => return Renew::Held,
            }

            // Someone changed the record (`reset_flow` on another instance): follow it
            // while this exact lease (`held.id`) is still ours.
            match tokio::time::timeout_at(deadline, self.load(flow_id)).await {
                Ok(Ok(Some((version, record))))
                    if record
                        .lease
                        .as_ref()
                        .is_some_and(|lease| self.matches_lease(lease, held.id)) =>
                {
                    held.version = version;
                    held.record = record;
                }
                Ok(Ok(other)) => return Renew::Lost(other.map(|(_, record)| record)),
                Ok(Err(error)) => {
                    log_store_failure(flow_id, "lease renewal failed", &error);
                    return Renew::Held;
                }
                Err(_) => return Renew::Held,
            }
        }
        Renew::Held
    }

    /// Record a finished run and clear the lease. Retries conflicts only while the
    /// lease is still ours, and store errors a few times within one lease TTL before
    /// giving up (past a TTL the lease has already expired).
    pub(super) async fn release(
        &self,
        flow_id: &str,
        lease_id: u64,
        policy: &BackoffPolicy,
        result: &CanoResult<()>,
    ) -> Release {
        let deadline = crate::workflow::deadline_after(self.ttl);
        let mut conflicts = 0;
        let mut errors = 0;
        loop {
            let attempt = match tokio::time::timeout_at(
                deadline,
                self.try_release(flow_id, lease_id, policy, result),
            )
            .await
            {
                Ok(attempt) => attempt,
                Err(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        flow_id,
                        ttl = ?self.ttl,
                        "scheduler store: releasing the lease timed out; it will expire"
                    );
                    return Release::GaveUp;
                }
            };
            match attempt {
                Ok(ReleaseAttempt::Written(record)) => {
                    #[cfg(feature = "metrics")]
                    if matches!(result, Err(e) if !e.is_cancelled()) {
                        count_failure(flow_id, &record);
                    }
                    return Release::Written(record);
                }
                Ok(ReleaseAttempt::NotHeld(record)) => return Release::NotHeld(record),
                Ok(ReleaseAttempt::Conflict) => {
                    conflicts += 1;
                    if conflicts >= CAS_ATTEMPTS {
                        log_store_failure(
                            flow_id,
                            "releasing the lease lost every swap; it will expire",
                            &contended(flow_id, "release"),
                        );
                        return Release::GaveUp;
                    }
                }
                Err(error) => {
                    errors += 1;
                    if errors > RELEASE_RETRIES {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            flow_id,
                            error = %error,
                            "scheduler store: releasing the lease failed; it will expire"
                        );
                        return Release::GaveUp;
                    }
                    log_store_failure(flow_id, "releasing the lease failed; retrying", &error);
                    if tokio::time::timeout_at(
                        deadline,
                        tokio::time::sleep(RELEASE_BACKOFF * errors),
                    )
                    .await
                    .is_err()
                    {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            flow_id,
                            error = %error,
                            "scheduler store: releasing the lease failed before expiry; it will expire"
                        );
                        return Release::GaveUp;
                    }
                }
            }
        }
    }

    async fn try_release(
        &self,
        flow_id: &str,
        lease_id: u64,
        policy: &BackoffPolicy,
        result: &CanoResult<()>,
    ) -> Result<ReleaseAttempt, CanoError> {
        let Some((version, mut record)) = self.load(flow_id).await? else {
            return Ok(ReleaseAttempt::NotHeld(None));
        };
        if !record
            .lease
            .as_ref()
            .is_some_and(|lease| self.matches_lease(lease, lease_id))
        {
            return Ok(ReleaseAttempt::NotHeld(Some(record)));
        }
        let now = Utc::now();
        match result {
            Ok(()) => record.succeed(),
            Err(e) if e.is_cancelled() => record.cancelled(now),
            Err(e) => record.fail(policy, e.to_string(), now),
        }
        record.lease = None;
        Ok(
            match self.swap(flow_id, Some(version), &mut record).await? {
                Some(_) => ReleaseAttempt::Written(record),
                None => ReleaseAttempt::Conflict,
            },
        )
    }

    /// Clear the failure streak and backoff window in the shared record. The lease,
    /// if any, is left alone. A flow with no record is already clean.
    pub(super) async fn reset(&self, flow_id: &str) -> Result<FlowRecord, CanoError> {
        for _ in 0..CAS_ATTEMPTS {
            let Some((version, mut record)) = self.load(flow_id).await? else {
                return Ok(FlowRecord::default());
            };
            record.failure_streak = 0;
            record.next_eligible = None;
            record.last_error = None;
            record.state = FlowState::Idle;
            if self
                .swap(flow_id, Some(version), &mut record)
                .await?
                .is_some()
            {
                return Ok(record);
            }
        }
        Err(contended(flow_id, "reset"))
    }

    /// Copy `record` into the local cell. The status is left alone while a local run
    /// is in flight: `Running` always wins over what the store says.
    pub(super) fn mirror(&self, cell: &mut FlowCell, record: &FlowRecord) {
        let info = &mut cell.info;
        info.run_count = record.run_count;
        info.last_run = record.last_run;
        info.failure_streak = record.failure_streak;
        info.next_eligible = record.next_eligible;
        if info.status != Status::Running {
            info.status = record.status(&self.instance_id, self.session, Utc::now());
        }
    }

    /// End the local run: drop its token, leave `Running`, then mirror `record` when
    /// there is one (otherwise the flow is `Idle`).
    pub(super) fn settle(&self, cell: &mut FlowCell, record: Option<&FlowRecord>) {
        cell.claiming = false;
        cell.run_token = None;
        cell.info.status = Status::Idle;
        if let Some(record) = record {
            self.mirror(cell, record);
        }
    }
}

#[cfg(test)]
mod tests;
