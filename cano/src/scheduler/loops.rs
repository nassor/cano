//! The async task bodies that drive a running scheduler: the per-flow `Every` /
//! `cron` loops, and the reserve / run / apply-outcome helpers shared by the loops
//! and [`RunningScheduler::trigger`](super::RunningScheduler::trigger).

use std::sync::Arc;

use chrono::Utc;
use cron::Schedule as CronSchedule;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::error::{CanoError, CanoResult};

use super::{FlowRuntime, Status};

/// Why a flow could not be reserved for a run.
pub(super) enum ReserveOutcome {
    AlreadyRunning,
    Tripped,
}

/// Atomically flip an idle flow to `Running`, stamp `last_run`, bump `run_count`,
/// and publish a fresh run token (a child of `cancel_runs`) in the same write.
///
/// Publishing the token together with the status flip means `cancel_flow` can never
/// observe a running flow without its token, and a run reserved after `cancel_runs`
/// fired is born cancelled.
pub(super) fn reserve(
    rt: &FlowRuntime,
    cancel_runs: &CancellationToken,
) -> Result<CancellationToken, ReserveOutcome> {
    let mut outcome = Err(ReserveOutcome::AlreadyRunning);
    rt.cell.send_if_modified(|cell| match cell.info.status {
        Status::Running => false,
        Status::Tripped { .. } => {
            outcome = Err(ReserveOutcome::Tripped);
            false
        }
        _ => {
            let token = cancel_runs.child_token();
            cell.info.status = Status::Running;
            cell.info.last_run = Some(Utc::now());
            cell.info.run_count += 1;
            cell.run_token = Some(token.clone());
            outcome = Ok(token);
            true
        }
    });
    outcome
}

/// Run one reserved flow to completion and record its outcome.
///
/// The run is wrapped in `catch_unwind`: a panic on a path the FSM does not guard
/// (an observer, a custom checkpoint store) would otherwise end this task without
/// `apply_outcome`, leaving the flow `Running` forever.
pub(super) async fn run_reserved(rt: Arc<FlowRuntime>, token: CancellationToken) {
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;

    #[cfg(feature = "metrics")]
    let _active = crate::metrics::SchedulerFlowActiveGuard::new();
    #[cfg(feature = "metrics")]
    let flow_id = rt.cell.borrow().info.id.clone();
    #[cfg(feature = "metrics")]
    let started = std::time::Instant::now();

    let run = rt.flow.run(&token);
    #[cfg(feature = "tracing")]
    let run = tracing::Instrument::instrument(run, tracing::info_span!("execute_flow"));

    let result = match AssertUnwindSafe(run).catch_unwind().await {
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

    apply_outcome(&rt, result);
}

/// Record a finished run: `Completed` on success, `Idle` on a deliberate cancel,
/// otherwise `Backoff` / `Tripped` per the flow's policy. Always clears the run token.
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
                let streak = info.failure_streak.saturating_add(1);
                info.failure_streak = streak;
                if rt.policy.is_tripped(streak) {
                    info.next_eligible = None;
                    info.status = Status::Tripped { streak, last_error };
                    #[cfg(feature = "metrics")]
                    crate::metrics::scheduler_flow_tripped(&info.id);
                } else {
                    let delay = rt.policy.compute_delay(streak);
                    let until = Utc::now()
                        + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::zero());
                    info.next_eligible = Some(until);
                    info.status = Status::Backoff {
                        until,
                        streak,
                        last_error,
                    };
                    #[cfg(feature = "metrics")]
                    crate::metrics::scheduler_flow_backoff(&info.id);
                }
            }
        }
    });
}

/// Reserve and run the flow inline; a reserve failure skips this tick.
async fn dispatch(rt: &Arc<FlowRuntime>, cancel_runs: &CancellationToken) {
    if let Ok(token) = reserve(rt, cancel_runs) {
        run_reserved(Arc::clone(rt), token).await;
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
pub(super) async fn every_loop(
    rt: Arc<FlowRuntime>,
    interval: Duration,
    shutdown: CancellationToken,
    cancel_runs: CancellationToken,
) {
    if shutdown.is_cancelled() {
        return;
    }
    dispatch(&rt, &cancel_runs).await;
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let wait = wait_until_eligible(&rt, interval);
        if !sleep_or_shutdown(wait, &shutdown).await {
            break;
        }
        dispatch(&rt, &cancel_runs).await;
    }
}

/// Cron loop: sleep to the next firing (re-checking the wall clock after waking,
/// for backwards clock jumps), skip ticks inside a backoff window, then dispatch.
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

        dispatch(&rt, &cancel_runs).await;
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
