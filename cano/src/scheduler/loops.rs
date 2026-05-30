//! The async task bodies that drive a running scheduler: per-flow `Every` /
//! `cron` loops, the central `driver_task`, and the reserve/execute helpers
//! that run a single workflow tick and apply its outcome (success, backoff,
//! trip).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use chrono::Utc;
use cron::Schedule as CronSchedule;
use tokio::sync::{Notify, RwLock, mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{Duration, sleep};

use crate::error::{CanoError, CanoResult};
use crate::workflow::Workflow;

#[cfg(feature = "tracing")]
use tracing::Instrument;

use super::{BackoffPolicy, FlowData, FlowInfo, SchedulerCommand, Status};

/// Maximum chunk size used by [`sleep_unless_stopped`]. Bounds the delay
/// between a `running = false` flip and the loop observing it, regardless of
/// the schedule's interval.
const SHUTDOWN_POLL_CHUNK: Duration = Duration::from_millis(250);

/// Sleep for up to `wait`, returning early if `running` flips to `false` or if
/// `stop_notify.notified()` fires. Returns `true` when the full duration
/// elapsed (continue the loop), `false` when shutdown was observed.
///
/// `Notify::notify_waiters()` only wakes waiters that are already parked on
/// `notified()` at the instant of the call — a loop sleeping when the driver
/// fires the signal would otherwise miss it and stall for up to `wait`. By
/// breaking the wait into `SHUTDOWN_POLL_CHUNK`-sized pieces and re-checking
/// `running` between each chunk, the worst-case delay between a stop signal
/// and shutdown observation is bounded by that chunk size.
async fn sleep_unless_stopped(
    wait: Duration,
    running: &Arc<RwLock<bool>>,
    stop_notify: &Arc<Notify>,
) -> bool {
    // Always honor a shutdown signal — even when `wait == 0` (cron tick whose
    // `next` already elapsed, backoff window that ended exactly now). Without
    // this up-front read the zero-wait path short-circuits the loop and
    // returns `true`, letting the caller dispatch one extra workflow after
    // `running` was flipped to `false`.
    if !*running.read().await {
        return false;
    }
    let mut remaining = wait;
    while !remaining.is_zero() {
        // Re-check `running` BEFORE subscribing to the next `notified()`.
        // `Notify::notify_waiters()` only wakes waiters currently parked, so a
        // signal that fires between two select! iterations is silently lost —
        // without this pre-check the loop would sleep another full chunk before
        // observing the flip. With the pre-check the worst-case shutdown
        // latency is bounded by one `SHUTDOWN_POLL_CHUNK`, not two.
        if !*running.read().await {
            return false;
        }
        let chunk = remaining.min(SHUTDOWN_POLL_CHUNK);
        tokio::select! {
            _ = sleep(chunk) => {}
            _ = stop_notify.notified() => return false,
        }
        remaining = remaining.saturating_sub(chunk);
    }
    true
}

/// Per-flow `Every`-schedule loop body. Lives outside `start` so the driver
/// task and the loops are decoupled — the driver owns the workflows
/// HashMap, the loops just see the data they need.
pub(super) async fn spawn_every_loop<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: Arc<BackoffPolicy>,
    running: Arc<RwLock<bool>>,
    stop_notify: Arc<Notify>,
    interval: Duration,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    if !*running.read().await {
        return;
    }

    // Check if previous run is still active before executing. Tripped flows
    // skip dispatch entirely; backoff flows fall through to the loop where
    // the sleep math accounts for `next_eligible`.
    if dispatchable_now(&info).await {
        execute_flow(
            Arc::clone(&workflow),
            initial_state.clone(),
            Arc::clone(&info),
            &policy,
        )
        .await;
    }

    loop {
        // Check `running` at the top of the iteration, mirroring
        // `spawn_cron_loop`. The driver's `notify_waiters()` only wakes loops
        // parked on `notified()` at the instant of the call — a loop that was
        // inside `execute_flow` (or between `wait_until_eligible` and the
        // `select!`) misses it and would otherwise sleep a full `interval`
        // before the post-sleep check fires, stalling shutdown by up to one
        // schedule period. The sticky `running` flag closes that gap here.
        if !*running.read().await {
            break;
        }

        // Sleep at least `interval`, but if a backoff window pushes us further
        // out, sleep until that instant. The helper bounds the worst-case
        // shutdown delay to `SHUTDOWN_POLL_CHUNK` (250ms) by chunking the
        // sleep and re-checking `running` between chunks — `Notify` alone
        // would silently drop a signal that fired while sleep was already
        // running, leaving the loop blocked for the full schedule interval.
        let wait = wait_until_eligible(&info, interval).await;
        if !sleep_unless_stopped(wait, &running, &stop_notify).await {
            break;
        }

        if !dispatchable_now(&info).await {
            continue;
        }

        execute_flow(
            Arc::clone(&workflow),
            initial_state.clone(),
            Arc::clone(&info),
            &policy,
        )
        .await;
    }
}

/// Per-flow `Cron`-schedule loop body. See [`spawn_every_loop`] for the
/// rationale on splitting the loop bodies out of `start`.
pub(super) async fn spawn_cron_loop<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: Arc<BackoffPolicy>,
    running: Arc<RwLock<bool>>,
    stop_notify: Arc<Notify>,
    schedule: Box<CronSchedule>,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    loop {
        if !*running.read().await {
            break;
        }

        let now = Utc::now();
        let Some(next) = schedule.after(&now).next() else {
            // No future cron firing — exit the loop cleanly.
            break;
        };
        let wait_duration = (next - now).to_std().unwrap_or(Duration::from_secs(0));
        // Chunked sleep — same rationale as in spawn_every_loop: ensures
        // shutdown is observed within `SHUTDOWN_POLL_CHUNK` even if `Notify`
        // dropped the signal because the loop wasn't parked at the instant
        // of the call. Also: re-validate that we're actually past `next`
        // after waking (handles wall-clock jumps backwards from NTP / suspend).
        if !sleep_unless_stopped(wait_duration, &running, &stop_notify).await {
            break;
        }
        // Wall-clock jumped back? Sleep again until we're past `next`.
        // Single re-check is sufficient — a runaway clock would just loop here.
        let now2 = Utc::now();
        if now2 < next {
            let extra = (next - now2).to_std().unwrap_or(Duration::from_secs(0));
            if !sleep_unless_stopped(extra, &running, &stop_notify).await {
                break;
            }
        }

        // If a backoff window pushes us past this tick, skip dispatch and
        // let the next iteration pick up the following cron firing.
        let info_snapshot = info.read().await;
        if let Some(eligible) = info_snapshot.next_eligible
            && Utc::now() < eligible
        {
            #[cfg(feature = "tracing")]
            tracing::debug!(
                flow_id = %info_snapshot.id,
                next_eligible = %eligible,
                "cron tick suppressed by backoff window"
            );
            drop(info_snapshot);
            continue;
        }
        drop(info_snapshot);

        if !dispatchable_now(&info).await {
            continue;
        }

        execute_flow(
            Arc::clone(&workflow),
            initial_state.clone(),
            Arc::clone(&info),
            &policy,
        )
        .await;
    }
}

/// Driver task: owns the rx side of the command channel plus the workflows
/// HashMap (for Trigger / Reset lookups and final teardown). On Stop (or rx
/// closed) flips the running flag, wakes the per-flow loops, drains
/// `scheduler_tasks`, waits up to 30s for in-flight workflows to finish,
/// runs resource teardown in LIFO order, and publishes the final result on
/// the watch channel.
#[allow(clippy::too_many_arguments)]
pub(super) async fn driver_task<TState, TResourceKey>(
    mut rx: mpsc::Receiver<SchedulerCommand>,
    workflows: HashMap<Arc<str>, FlowData<TState, TResourceKey>>,
    flow_order: Vec<Arc<str>>,
    running: Arc<RwLock<bool>>,
    stop_notify: Arc<Notify>,
    scheduler_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
    in_flight_drain: Arc<RwLock<Option<AbortHandle>>>,
    result_tx: watch::Sender<Option<CanoResult<()>>>,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    while let Some(cmd) = rx.recv().await {
        match cmd {
            SchedulerCommand::Stop => {
                // Explicit Stop signal — exit the rx loop and proceed to
                // teardown. `rx.close()` closes the receiver so any further
                // try_send from a `RunningScheduler` clone errors with
                // `Closed`, surfacing a deterministic "not running" error.
                rx.close();
                break;
            }
            SchedulerCommand::Trigger { id, response } => {
                let outcome = if let Some(flow) = workflows.get(&id) {
                    // `reserve_flow` folds the Tripped and Running checks
                    // under the same write lock as the `Status::Running`
                    // flip, so there is no window where a concurrent
                    // `apply_outcome` can trip the flow between the check
                    // and the dispatch. Backoff windows are deliberately
                    // not enforced on manual triggers — the operator
                    // override is the documented behavior.
                    match reserve_flow(Arc::clone(&flow.info)).await {
                        ReserveOutcome::Reserved => {
                            let workflow = Arc::clone(&flow.workflow);
                            let initial_state = flow.initial_state.clone();
                            let info = Arc::clone(&flow.info);
                            let policy = Arc::clone(&flow.policy);
                            let handle = tokio::spawn(async move {
                                execute_reserved_flow(workflow, initial_state, info, &policy).await;
                            });
                            let mut tasks = scheduler_tasks.write().await;
                            tasks.retain(|h| !h.is_finished());
                            tasks.push(handle);
                            Ok(())
                        }
                        ReserveOutcome::AlreadyRunning => Err(CanoError::Workflow(format!(
                            "Flow '{id}' is already running"
                        ))),
                        ReserveOutcome::Tripped => Err(CanoError::Workflow(format!(
                            "Flow '{id}' is tripped — call reset_flow before triggering"
                        ))),
                    }
                } else {
                    Err(CanoError::Workflow(format!(
                        "No workflow registered with id '{id}'"
                    )))
                };

                let _ = response.send(outcome);
            }
            SchedulerCommand::Reset { id, response } => {
                let outcome = if let Some(flow) = workflows.get(&id) {
                    let mut info_guard = flow.info.write().await;
                    info_guard.failure_streak = 0;
                    info_guard.next_eligible = None;
                    // Don't clobber a `Running` status — a concurrent
                    // execution would set Completed/Backoff/Tripped on its
                    // own write.
                    if !matches!(info_guard.status, Status::Running) {
                        info_guard.status = Status::Idle;
                    }
                    Ok(())
                } else {
                    Err(CanoError::Workflow(format!(
                        "No workflow registered with id '{id}'"
                    )))
                };

                let _ = response.send(outcome);
            }
        }
    }

    // Shutdown phase. Reached either via explicit Stop or via rx-closed (all
    // RunningScheduler clones dropped without stop). Either way we proceed
    // through the same graceful drain.
    *running.write().await = false;
    // Wake any Every/Cron loop currently parked on its sleep so they observe
    // `running == false` and exit immediately, bounding shutdown latency by
    // how long an in-flight workflow takes — not by the schedule interval.
    stop_notify.notify_waiters();

    // Wait for all scheduler loop tasks to finish.
    //
    // Pop with a short-lived write lock per iteration (rather than holding
    // the lock across every `handle.await`) so a concurrent
    // `RunningScheduler::Drop` can `try_write()` the same Vec and abort any
    // stuck handles instead of being skipped. A wedged per-flow task that
    // never returns from `handle.await` would otherwise hold the lock
    // indefinitely, defeating the Drop fallback abort.
    //
    // After popping, publish the handle's `AbortHandle` into `in_flight_drain`
    // so a concurrent `Drop` can still reach the wedged task — a dropped
    // `JoinHandle` only detaches the spawned task, it doesn't abort it. The
    // slot is cleared as soon as the await returns (or is cancelled), so the
    // window where Drop's abort applies is exactly the duration of the await.
    loop {
        let handle = {
            let mut tasks = scheduler_tasks.write().await;
            tasks.pop()
        };
        match handle {
            Some(h) => {
                let abort = h.abort_handle();
                *in_flight_drain.write().await = Some(abort);
                let _ = h.await;
                *in_flight_drain.write().await = None;
            }
            None => break,
        }
    }

    // Wait for any running workflows to complete, bounded by 30s.
    let timeout = Duration::from_secs(30);
    let start_time = tokio::time::Instant::now();
    let mut result: CanoResult<()> = Ok(());
    'wait: loop {
        let mut any_running = false;
        for fd in workflows.values() {
            if fd.info.read().await.status == Status::Running {
                any_running = true;
                break;
            }
        }
        if !any_running {
            break 'wait;
        }
        if start_time.elapsed() >= timeout {
            result = Err(CanoError::Workflow(
                "Timeout waiting for workflows to complete".to_string(),
            ));
            break 'wait;
        }
        sleep(Duration::from_millis(100)).await;
    }

    // Teardown workflow resources in reverse registration order (LIFO).
    // Driven by `flow_order` rather than `HashMap::values()` to keep
    // teardown deterministic across runs.
    for id in flow_order.iter().rev() {
        if let Some(flow) = workflows.get(id) {
            let len = flow.workflow.resources.lifecycle_len();
            flow.workflow.resources.teardown_range(0..len).await;
        }
    }

    // Publish the final result. Receivers (`wait` / `stop`) loop on
    // `changed().await` until they observe the Some(_) transition.
    let _ = result_tx.send(Some(result));
}

async fn execute_flow<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: &BackoffPolicy,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    if !matches!(
        reserve_flow(Arc::clone(&info)).await,
        ReserveOutcome::Reserved
    ) {
        return;
    }

    execute_reserved_flow(workflow, initial_state, info, policy).await;
}

/// Result of attempting to reserve a flow for dispatch. The Tripped and
/// AlreadyRunning variants both mean "skip this dispatch" but are distinguished
/// so the manual-trigger path can return distinct error messages.
enum ReserveOutcome {
    Reserved,
    AlreadyRunning,
    Tripped,
}

/// Atomically check the gating status and (on success) flip to `Running`,
/// stamp `last_run`, and bump `run_count`. Folding the check and the write
/// under one write-lock acquisition closes the TOCTOU window where a
/// concurrent `apply_outcome` could trip a flow between a separate read
/// and the dispatch.
async fn reserve_flow(info: Arc<RwLock<FlowInfo>>) -> ReserveOutcome {
    let mut info_guard = info.write().await;
    match info_guard.status {
        Status::Running => return ReserveOutcome::AlreadyRunning,
        Status::Tripped { .. } => return ReserveOutcome::Tripped,
        _ => {}
    }

    info_guard.status = Status::Running;
    info_guard.last_run = Some(Utc::now());
    info_guard.run_count += 1;
    ReserveOutcome::Reserved
}

async fn execute_reserved_flow<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: &BackoffPolicy,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;

    #[cfg(feature = "metrics")]
    let _active = crate::metrics::SchedulerFlowActiveGuard::new();
    #[cfg(feature = "metrics")]
    let _flow_id = info.read().await.id.clone();
    #[cfg(feature = "metrics")]
    let _started = std::time::Instant::now();

    // Compute the total-timeout budget for this flow invocation. Mirrors the
    // logic in `Workflow::run_workflow` so scheduler-driven runs honor
    // `with_total_timeout` the same way orchestrate-driven runs do.
    let total_budget = workflow
        .total_timeout
        .map(|d| (std::time::Instant::now(), d));

    // Wrap the workflow future in `catch_unwind`. A panic inside any path
    // that bypasses the FSM's own `catch_unwind` (e.g. an observer that
    // panics, a custom checkpoint store that panics) would otherwise abort
    // this spawned task with `apply_outcome` never running — leaving
    // `Status::Running` set forever and blocking every subsequent `trigger`
    // for this flow with `AlreadyRunning`. Converting the panic to an `Err`
    // restores the status flip and surfaces the failure through the normal
    // `BackoffPolicy`.
    #[cfg(feature = "tracing")]
    let workflow_fut = workflow
        .execute_workflow(initial_state, total_budget)
        .instrument(tracing::info_span!("execute_flow"));
    #[cfg(not(feature = "tracing"))]
    let workflow_fut = workflow.execute_workflow(initial_state, total_budget);

    let result = match AssertUnwindSafe(workflow_fut).catch_unwind().await {
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
    crate::metrics::scheduler_flow_run(&_flow_id, result.is_ok(), _started.elapsed());

    apply_outcome(&info, result.map(|_| ()), policy).await;
}

/// Atomic post-execution status write: a single write under the existing lock
/// decides the terminal status for this run (`Completed` on success, else
/// `Backoff` / `Tripped` per the policy). Streak and `next_eligible` are reset
/// on success.
async fn apply_outcome(
    info: &Arc<RwLock<FlowInfo>>,
    result: Result<(), CanoError>,
    policy: &BackoffPolicy,
) {
    let mut info_guard = info.write().await;
    match result {
        Ok(_) => {
            info_guard.status = Status::Completed;
            info_guard.failure_streak = 0;
            info_guard.next_eligible = None;
        }
        Err(e) => {
            let err_str: Arc<str> = Arc::from(e.to_string());
            let new_streak = info_guard.failure_streak.saturating_add(1);
            info_guard.failure_streak = new_streak;
            if policy.is_tripped(new_streak) {
                info_guard.next_eligible = None;
                info_guard.status = Status::Tripped {
                    streak: new_streak,
                    last_error: err_str,
                };
                #[cfg(feature = "metrics")]
                crate::metrics::scheduler_flow_tripped(&info_guard.id);
            } else {
                let delay = policy.compute_delay(new_streak);
                let until = Utc::now()
                    + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::zero());
                info_guard.next_eligible = Some(until);
                info_guard.status = Status::Backoff {
                    until,
                    streak: new_streak,
                    last_error: err_str,
                };
                #[cfg(feature = "metrics")]
                crate::metrics::scheduler_flow_backoff(&info_guard.id);
            }
        }
    }
}

/// `false` when the flow's status indicates we should skip this dispatch
/// (already running, or tripped). Backoff windows are honored by the loop's
/// sleep math, not by this gate, so the gate stays cheap.
async fn dispatchable_now(info: &Arc<RwLock<FlowInfo>>) -> bool {
    let guard = info.read().await;
    !matches!(guard.status, Status::Running | Status::Tripped { .. })
}

/// Compute how long the Every loop should sleep before the next dispatch.
/// Returns `max(interval, next_eligible - now)` so a backoff window pushes the
/// next attempt out without affecting flows that are healthy.
///
/// Falls back to `interval` when `next_eligible` is unset, in the past, or
/// negative (the latter via `to_std()` returning `Err`) — i.e. no extra delay
/// is added once the backoff window has elapsed.
async fn wait_until_eligible(info: &Arc<RwLock<FlowInfo>>, interval: Duration) -> Duration {
    let snapshot = info.read().await;
    if let Some(eligible) = snapshot.next_eligible {
        let now = Utc::now();
        if eligible > now {
            let extra = (eligible - now).to_std().unwrap_or(Duration::from_secs(0));
            return interval.max(extra);
        }
    }
    interval
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sleep_unless_stopped_returns_early_when_running_flips() {
        // Even if `Notify::notified()` was never reached (e.g. the signal
        // arrived just before the loop subscribed), the chunked re-check
        // must observe `running == false` within `SHUTDOWN_POLL_CHUNK`.
        let running = Arc::new(RwLock::new(true));
        let stop = Arc::new(Notify::new());

        // Spawn the helper for a 10-second wait — if the chunked re-check
        // were removed it would sleep the full 10s because the notify was
        // never observed.
        let running_clone = Arc::clone(&running);
        let stop_clone = Arc::clone(&stop);
        let start = tokio::time::Instant::now();
        let task = tokio::spawn(async move {
            sleep_unless_stopped(Duration::from_secs(10), &running_clone, &stop_clone).await
        });

        // Yield briefly so the helper has parked in its first chunk sleep,
        // then flip `running` *without* calling notify_waiters() at all.
        // The race scenario in the bug: signal was lost or never sent.
        tokio::time::sleep(Duration::from_millis(50)).await;
        *running.write().await = false;

        let returned_full = task.await.unwrap();
        let elapsed = start.elapsed();
        assert!(!returned_full, "helper must report early-exit");
        // The flip happens ~50ms after the helper enters its first chunk
        // sleep. Worst-case shutdown latency is one SHUTDOWN_POLL_CHUNK
        // (250 ms) — the in-progress sleep finishes, then the pre-select
        // re-check catches the flag and exits. Bound the assertion tightly
        // (under 1× chunk + slack) so a regression that drops the pre-check
        // (worst case → 2× chunk) is caught by this test.
        assert!(
            elapsed < SHUTDOWN_POLL_CHUNK + Duration::from_millis(150),
            "helper must observe `running=false` within ~1 chunk, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn sleep_unless_stopped_returns_false_on_zero_when_already_stopped() {
        // Regression: a cron loop with `wait_duration == 0` (cron tick already
        // elapsed, NTP forward jump, very tight cron) used to skip the running
        // check entirely because the `while !remaining.is_zero()` body never
        // ran — the helper returned `true` and the caller dispatched one extra
        // workflow after shutdown was requested.
        let running = Arc::new(RwLock::new(false));
        let stop = Arc::new(Notify::new());

        let returned_full = sleep_unless_stopped(Duration::ZERO, &running, &stop).await;
        assert!(
            !returned_full,
            "zero-duration sleep must surface running=false instead of short-circuiting to true"
        );
    }

    #[tokio::test]
    async fn sleep_unless_stopped_observes_notify() {
        // Sanity: the helper still responds to notify_waiters when it does
        // arrive while the loop is parked.
        let running = Arc::new(RwLock::new(true));
        let stop = Arc::new(Notify::new());

        let r = Arc::clone(&running);
        let s = Arc::clone(&stop);
        let task =
            tokio::spawn(
                async move { sleep_unless_stopped(Duration::from_secs(10), &r, &s).await },
            );
        tokio::time::sleep(Duration::from_millis(20)).await;
        stop.notify_waiters();
        let returned_full = task.await.unwrap();
        assert!(!returned_full, "notify must trigger early-exit");
    }
}
