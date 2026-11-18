//! The async task bodies that drive a running scheduler: the per-flow `Every` /
//! `cron` loops, and the reserve / claim / run / record-outcome helpers shared by the
//! loops and [`RunningScheduler::trigger`](super::RunningScheduler::trigger).

use std::any::Any;
use std::sync::Arc;

use chrono::Utc;
use cron::Schedule as CronSchedule;
use futures_util::FutureExt;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::error::{CanoError, CanoResult};

use super::lease::{Claim, Coordinator, Due, HeldLease, Refusal, Release, plus};
use super::{FlowRuntime, Status};

/// The shortest sleep between two dispatch attempts of a store-backed `Every` loop.
const MIN_POLL: Duration = Duration::from_millis(10);

/// Why a flow could not be taken for a run.
pub(super) enum Refused {
    /// A run is in flight on this instance.
    Running,
    /// The flow is tripped, or another instance holds the lease, or it is not due.
    Refusal(Refusal),
    /// The shared store failed.
    Store(CanoError),
}

impl Refused {
    /// The error `trigger` returns for flow `id`.
    pub(super) fn into_error(self, id: &str) -> CanoError {
        match self {
            Refused::Running => CanoError::workflow(format!("Flow '{id}' is already running")),
            Refused::Refusal(Refusal::Tripped) => CanoError::workflow(format!(
                "Flow '{id}' is tripped — call reset_flow before triggering"
            )),
            Refused::Refusal(Refusal::Elsewhere {
                owner,
                lease_expires,
            }) => CanoError::workflow(format!(
                "Flow '{id}' is already running on instance '{owner}' (lease expires {lease_expires})"
            )),
            Refused::Refusal(Refusal::NotDue) => {
                CanoError::workflow(format!("Flow '{id}' is not due"))
            }
            Refused::Store(error) => error,
        }
    }
}

/// A flow taken for one run: the local run token, and the lease when the scheduler
/// shares a store.
pub(super) struct Reserved {
    token: CancellationToken,
    held: Option<HeldLease>,
}

/// Take an idle flow for this instance and publish a fresh run token (a child of
/// `cancel_runs`) in the same write.
///
/// Without a store the write flips the flow to `Running`, stamps `last_run`, bumps
/// `run_count` and refuses a tripped flow. With a store it only marks a claim in flight
/// (`claiming`) and leaves the status alone: the flow shows `Running` once the claim is
/// won, so a lost or failed claim never shows as a run. The claim stamps the counters
/// from the shared record instead.
///
/// Publishing the token in the same write means `cancel_flow` can never observe a
/// running flow without its token, and a run reserved after `cancel_runs` fired is born
/// cancelled.
fn reserve(
    rt: &FlowRuntime,
    cancel_runs: &CancellationToken,
) -> Result<CancellationToken, Refused> {
    let standalone = rt.coordinator.is_none();
    let mut outcome = Err(Refused::Running);
    rt.cell.send_if_modified(|cell| {
        if cell.claiming || cell.info.status == Status::Running {
            return false;
        }
        if standalone && matches!(cell.info.status, Status::Tripped { .. }) {
            outcome = Err(Refused::Refusal(Refusal::Tripped));
            return false;
        }
        let token = cancel_runs.child_token();
        if standalone {
            cell.info.status = Status::Running;
            cell.info.last_run = Some(Utc::now());
            cell.info.run_count += 1;
        } else {
            cell.claiming = true;
        }
        cell.run_token = Some(token.clone());
        outcome = Ok(token);
        true
    });
    outcome
}

/// Reserve a flow of a scheduler without a store: [`acquire`] minus the claim, so
/// `trigger` can do it synchronously under its task lock.
pub(super) fn reserve_local(
    rt: &FlowRuntime,
    cancel_runs: &CancellationToken,
) -> Result<Reserved, Refused> {
    debug_assert!(rt.coordinator.is_none());
    reserve(rt, cancel_runs).map(|token| Reserved { token, held: None })
}

/// Ends an in-flight claim unless disarmed: a claim future dropped mid-await (a
/// cancelled `trigger`) or a claim that hit a store error must not leave the flow
/// reserved forever.
struct Unreserve<'a> {
    rt: &'a FlowRuntime,
    armed: bool,
}

impl Drop for Unreserve<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.rt.cell.send_modify(|cell| {
                cell.claiming = false;
                cell.run_token = None;
            });
        }
    }
}

/// Take flow `rt` for one run: reserve it locally, then (with a store) win the claim.
/// A refused claim leaves the cell showing what the shared record says. A won store
/// claim stays in `claiming` until [`activate`] commits to running it (or [`abandon`]
/// hands it back), so a run cancelled before launch never flashes [`Status::Running`].
pub(super) async fn acquire(
    rt: &FlowRuntime,
    cancel_runs: &CancellationToken,
    due: Due,
    shutdown: Option<&CancellationToken>,
) -> Result<Option<Reserved>, Refused> {
    let token = reserve(rt, cancel_runs)?;
    let Some(coord) = &rt.coordinator else {
        return Ok(Some(Reserved { token, held: None }));
    };
    let mut guard = Unreserve { rt, armed: true };
    let flow_id = rt.cell.borrow().info.id.clone();
    match coord.claim(&flow_id, &rt.policy, due, shutdown).await {
        Ok(Some(Claim::Won(held))) => {
            guard.armed = false;
            Ok(Some(Reserved {
                token,
                held: Some(held),
            }))
        }
        Ok(Some(Claim::Refused { reason, record })) => {
            guard.armed = false;
            rt.cell
                .send_modify(|cell| coord.settle(cell, Some(&record)));
            Err(Refused::Refusal(reason))
        }
        Ok(None) => Ok(None),
        Err(error) => Err(Refused::Store(error)),
    }
}

/// Flip a store-claimed reservation from `claiming` to [`Status::Running`] and mirror
/// the claimed record right before spawning or running the flow.
pub(super) fn activate(rt: &FlowRuntime, reserved: &Reserved) {
    if let (Some(coord), Some(held)) = (&rt.coordinator, &reserved.held) {
        rt.cell.send_modify(|cell| {
            cell.claiming = false;
            cell.info.status = Status::Running;
            coord.mirror(cell, held.record());
        });
    }
}

/// Give a reserved run back without running it (the scheduler began stopping while
/// claiming): clear the local reservation first so a dropped caller cannot strand it,
/// then release the lease as a deliberate cancel.
pub(super) async fn abandon(rt: &FlowRuntime, reserved: Reserved) {
    rt.cell.send_modify(|cell| {
        cell.claiming = false;
        cell.run_token = None;
        cell.info.status = Status::Idle;
    });
    let Reserved { held, .. } = reserved;
    if let (Some(coord), Some(_)) = (&rt.coordinator, held) {
        let flow_id = rt.cell.borrow().info.id.clone();
        let record = match coord
            .release(&flow_id, &rt.policy, &Err(CanoError::cancelled()))
            .await
        {
            Release::Written(record) => Some(record),
            Release::NotHeld(record) => record,
            Release::GaveUp => None,
        };
        if let Some(record) = record {
            rt.cell.send_modify(|cell| coord.mirror(cell, &record));
        }
    }
}

type Unwound = Result<CanoResult<()>, Box<dyn Any + Send>>;

enum RunLease {
    Held,
    Lost(Option<super::lease::FlowRecord>),
}

/// Run one reserved flow to completion and record its outcome.
///
/// The run is wrapped in `catch_unwind`: a panic on a path the FSM does not guard
/// (an observer, a custom checkpoint store) would otherwise end this task without
/// recording the outcome, leaving the flow `Running` forever.
///
/// With a store, the lease is renewed alongside the run. A lease that is lost (another
/// instance took it over, or the store was unreachable for a whole TTL) cancels the
/// run, and its outcome is not written: the new owner's record stands.
pub(super) async fn run_reserved(rt: Arc<FlowRuntime>, reserved: Reserved) {
    let Reserved { token, held } = reserved;

    let flow_id = rt.cell.borrow().info.id.clone();
    #[cfg(feature = "metrics")]
    let _active = crate::metrics::SchedulerFlowActiveGuard::new();
    #[cfg(feature = "metrics")]
    let started = std::time::Instant::now();

    let run = rt.flow.run(&token);
    #[cfg(feature = "tracing")]
    let run = tracing::Instrument::instrument(run, tracing::info_span!("execute_flow"));
    let run = std::panic::AssertUnwindSafe(run).catch_unwind();

    let (unwound, lease_state) = match (&rt.coordinator, held) {
        (Some(coord), Some(held)) => run_with_lease(coord, &flow_id, held, &token, run).await,
        _ => (run.await, RunLease::Held),
    };
    let result = match unwound {
        Ok(inner) => inner,
        Err(payload) => {
            let msg = crate::workflow::panic_payload_message(&*payload);
            #[cfg(feature = "tracing")]
            tracing::error!(panic = %msg, "scheduled flow panicked");
            Err(CanoError::task_execution(format!(
                "scheduled flow panicked: {msg}"
            )))
        }
    };

    #[cfg(feature = "metrics")]
    crate::metrics::scheduler_flow_run(&flow_id, result.is_ok(), started.elapsed());

    let Some(coord) = &rt.coordinator else {
        apply_outcome(&rt, result);
        return;
    };
    let record = match lease_state {
        RunLease::Lost(record) => record,
        RunLease::Held => match coord.release(&flow_id, &rt.policy, &result).await {
            Release::Written(record) => Some(record),
            Release::NotHeld(record) => {
                #[cfg(feature = "tracing")]
                tracing::warn!(%flow_id, "scheduler lease was lost before the run ended; outcome not written");
                record
            }
            Release::GaveUp => None,
        },
    };
    rt.cell
        .send_modify(|cell| coord.settle(cell, record.as_ref()));
}

/// Drive `run` while renewing `held`. Returns the run's outcome and whether the
/// lease was held or lost (the run was then cancelled through `token` and drained).
async fn run_with_lease(
    coord: &Coordinator,
    flow_id: &str,
    mut held: HeldLease,
    token: &CancellationToken,
    run: impl Future<Output = Unwound>,
) -> (Unwound, RunLease) {
    tokio::pin!(run);
    // `run` is never dropped here: a lost lease cancels it and waits for it to drain
    // (saga compensation must complete), it does not abandon it.
    let first = tokio::select! {
        outcome = &mut run => Ok(outcome),
        record = coord.keep_alive(flow_id, &mut held) => Err(record),
    };
    match first {
        Ok(outcome) => (outcome, RunLease::Held),
        Err(record) => {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                flow_id,
                instance = coord.instance_id(),
                "scheduler lease lost; cancelling the run"
            );
            token.cancel();
            (run.await, RunLease::Lost(record))
        }
    }
}

/// Record a finished run on a scheduler without a store: `Completed` on success,
/// `Idle` on a deliberate cancel, otherwise `Backoff` / `Tripped` per the flow's
/// policy. Always clears the run token.
fn apply_outcome(rt: &FlowRuntime, result: CanoResult<()>) {
    rt.cell.send_modify(|cell| {
        cell.run_token = None;
        let info = &mut cell.info;
        match result {
            Ok(()) => {
                info.status = Status::Completed;
                info.failure_streak = 0;
                info.next_eligible = None;
            }
            // A deliberate cancellation (`cancel_flow` or shutdown) is not a fault:
            // back to `Idle` without touching the streak. A dirty rollback is
            // `CompensationFailed`, not cancelled, and takes the backoff arm.
            Err(e) if e.is_cancelled() => info.status = Status::Idle,
            Err(e) => {
                let last_error: Arc<str> = Arc::from(e.to_string());
                let (streak, until) =
                    super::lease::after_failure(&rt.policy, info.failure_streak, Utc::now());
                info.failure_streak = streak;
                info.next_eligible = until;
                if let Some(until) = until {
                    info.status = Status::Backoff {
                        until,
                        streak,
                        last_error,
                    };
                    #[cfg(feature = "metrics")]
                    crate::metrics::scheduler_flow_backoff(&info.id);
                } else {
                    info.status = Status::Tripped { streak, last_error };
                    #[cfg(feature = "metrics")]
                    crate::metrics::scheduler_flow_tripped(&info.id);
                }
            }
        }
    });
}

enum Tick {
    Ran,
    Refused(Refused),
    Stopped,
}

/// Take the flow and run it inline; a refusal skips this tick. `shutdown` interrupts a
/// claim before its swap begins; once a swap is in flight it finishes and drains during
/// `drain_timeout`, or `cancel_runs` aborts it when `drain_timeout` expires on a hung swap.
async fn dispatch(
    rt: &Arc<FlowRuntime>,
    shutdown: &CancellationToken,
    cancel_runs: &CancellationToken,
    due: Due,
) -> Tick {
    let acquired = tokio::select! {
        biased;
        _ = cancel_runs.cancelled() => return Tick::Stopped,
        res = acquire(rt, cancel_runs, due, Some(shutdown)) => res,
    };
    match acquired {
        Ok(Some(reserved)) => {
            activate(rt, &reserved);
            run_reserved(Arc::clone(rt), reserved).await;
            Tick::Ran
        }
        Ok(None) => Tick::Stopped,
        Err(Refused::Store(error)) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                flow_id = %rt.cell.borrow().info.id,
                error = %error,
                "scheduler store: claim failed; skipping this tick"
            );
            let _ = &error;
            Tick::Refused(Refused::Store(error))
        }
        Err(other) => Tick::Refused(other),
    }
}

/// Sleep for `wait`; `false` as soon as `shutdown` fires (immediately if it
/// already has, so a zero wait cannot dispatch after shutdown).
async fn sleep_or_shutdown(wait: Duration, shutdown: &CancellationToken) -> bool {
    if shutdown.is_cancelled() {
        return false;
    }
    tokio::select! {
        _ = tokio::time::sleep(wait) => true,
        _ = shutdown.cancelled() => false,
    }
}

/// `Every` loop: run immediately, then every `max(interval, backoff)`. A run in
/// flight when `shutdown` fires completes (that is the drain); the loop exits at
/// its next check.
///
/// With a store the shared record decides whether each attempt is due, so the first
/// attempt does not re-run a flow that another instance ran within the interval.
pub(super) async fn every_loop(
    rt: Arc<FlowRuntime>,
    interval: Duration,
    shutdown: CancellationToken,
    cancel_runs: CancellationToken,
) {
    if shutdown.is_cancelled() {
        return;
    }
    let mut last_tick = dispatch(&rt, &shutdown, &cancel_runs, Due::Every(interval)).await;
    loop {
        if shutdown.is_cancelled() || matches!(last_tick, Tick::Stopped) {
            break;
        }
        let wait = if rt.coordinator.is_some() {
            shared_wait(&rt, interval, &last_tick)
        } else {
            wait_until_eligible(&rt, interval)
        };
        if !sleep_or_shutdown(wait, &shutdown).await {
            break;
        }
        last_tick = dispatch(&rt, &shutdown, &cancel_runs, Due::Every(interval)).await;
    }
}

/// Cron loop: sleep to the next firing (re-checking the wall clock after waking,
/// for backwards clock jumps), skip ticks inside a backoff window, then dispatch.
/// With a store the claim carries the firing, so exactly one instance runs each one,
/// and the shared record decides about backoff.
pub(super) async fn cron_loop(
    rt: Arc<FlowRuntime>,
    schedule: Box<CronSchedule>,
    shutdown: CancellationToken,
    cancel_runs: CancellationToken,
) {
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let now = Utc::now();
        let Some(next) = schedule.after(&now).next() else {
            // No future cron firing — exit the loop cleanly.
            break;
        };
        let wait = (next - now).to_std().unwrap_or(Duration::ZERO);
        if !sleep_or_shutdown(wait, &shutdown).await {
            break;
        }
        // Wall clock jumped back? Sleep again until we're past `next`.
        let now2 = Utc::now();
        if now2 < next {
            let extra = (next - now2).to_std().unwrap_or(Duration::ZERO);
            if !sleep_or_shutdown(extra, &shutdown).await {
                break;
            }
        }

        // A backoff window that reaches past this tick suppresses it.
        if rt.coordinator.is_none() {
            let eligible = rt.cell.borrow().info.next_eligible;
            if let Some(eligible) = eligible
                && Utc::now() < eligible
            {
                #[cfg(feature = "tracing")]
                tracing::debug!(
                    flow_id = %rt.cell.borrow().info.id,
                    next_eligible = %eligible,
                    "cron tick suppressed by backoff window"
                );
                continue;
            }
        }

        if matches!(
            dispatch(&rt, &shutdown, &cancel_runs, Due::Cron(next)).await,
            Tick::Stopped
        ) {
            break;
        }
    }
}

/// How long the `Every` loop sleeps before the next dispatch:
/// `max(interval, next_eligible - now)`, or `interval` when no backoff window is
/// pending.
fn wait_until_eligible(rt: &FlowRuntime, interval: Duration) -> Duration {
    let eligible = rt.cell.borrow().info.next_eligible;
    if let Some(eligible) = eligible {
        let now = Utc::now();
        if eligible > now {
            let extra = (eligible - now).to_std().unwrap_or(Duration::ZERO);
            return interval.max(extra);
        }
    }
    interval
}

/// How long a store-backed `Every` loop sleeps, from the mirrored record: until
/// `max(last_run + interval, next_eligible)`, at least [`MIN_POLL`].
///
/// - While another instance holds the lease the flow cannot be due before it ends,
///   so a due time already past becomes "look again in one interval", and the wake
///   never goes beyond the lease expiry: a holder that stopped renewing is noticed
///   promptly.
/// - A tripped flow only changes when someone calls `reset_flow`; look every interval.
/// - When the claim failed on a store error or a local trigger is still running, retry
///   after at least one interval instead of polling at [`MIN_POLL`].
fn shared_wait(rt: &FlowRuntime, interval: Duration, last_tick: &Tick) -> Duration {
    let now = Utc::now();
    let cell = rt.cell.borrow();
    let info = &cell.info;
    let mut wake = match info.last_run {
        Some(last_run) => plus(last_run, interval),
        None => plus(now, interval),
    };
    if let Some(eligible) = info.next_eligible {
        wake = wake.max(eligible);
    }
    match &info.status {
        Status::Tripped { .. } | Status::Running => wake = wake.max(plus(now, interval)),
        Status::RunningElsewhere { lease_expires, .. } => {
            if wake <= now {
                wake = plus(now, interval);
            }
            wake = wake.min(*lease_expires);
        }
        _ => {}
    }
    if matches!(
        last_tick,
        Tick::Refused(Refused::Store(_) | Refused::Running)
    ) {
        wake = wake.max(plus(now, interval));
    }
    (wake - now)
        .to_std()
        .unwrap_or(Duration::ZERO)
        .max(MIN_POLL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn sleep_or_shutdown_returns_early_on_shutdown() {
        let shutdown = CancellationToken::new();
        let start = tokio::time::Instant::now();
        let token = shutdown.clone();
        let task =
            tokio::spawn(async move { sleep_or_shutdown(Duration::from_secs(10), &token).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.cancel();
        assert!(!task.await.unwrap(), "shutdown must interrupt the sleep");
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn sleep_or_shutdown_zero_wait_after_shutdown_is_false() {
        // A zero wait (an already-elapsed cron tick) must still observe shutdown
        // instead of dispatching one more run.
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        assert!(!sleep_or_shutdown(Duration::ZERO, &shutdown).await);
    }
}
