//! The async task bodies that drive a running scheduler: per-flow `Every` /
//! `cron` loops, the central `driver_task`, and the reserve/execute helpers
//! that run a single workflow tick and apply its outcome (success, backoff,
//! trip).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use chrono::Utc;
use cron::Schedule as CronSchedule;
use tokio::sync::{RwLock, mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{Duration, sleep};

use crate::cancel::{CancellationHandle, CancellationToken};
use crate::error::{CanoError, CanoResult};
use crate::workflow::Workflow;

#[cfg(feature = "tracing")]
use tracing::Instrument;

use super::{BackoffPolicy, FlowData, FlowInfo, SchedulerCommand, Status};

/// Sleep for up to `wait`, returning early when the scheduler driver signals
/// shutdown by sending `false` on the `running` watch channel. Returns `true`
/// when the full duration elapsed (continue the loop), `false` when shutdown
/// was observed.
///
/// `watch` is used instead of `Notify::notified()` because a `Notify` signal
/// only wakes waiters already parked at the instant of the call — a loop that
/// was between `select!` iterations would miss it and stall for up to `wait`.
/// `watch::Receiver::changed()` returns immediately whenever an unseen version
/// exists (the receiver remembers the last value it observed), so a shutdown
/// send is never lost, and the loop parks exactly once per wait instead of
/// re-checking a boolean every 250ms.
async fn sleep_unless_stopped(wait: Duration, running: &mut watch::Receiver<bool>) -> bool {
    // Always honor a shutdown signal — even when `wait == 0` (cron tick whose
    // `next` already elapsed, backoff window that ended exactly now). Without
    // this up-front read the zero-wait path short-circuits the loop and
    // returns `true`, letting the caller dispatch one extra workflow after
    // `running` was flipped to `false`.
    if !*running.borrow() {
        return false;
    }
    tokio::select! {
        _ = sleep(wait) => true,
        res = running.changed() => {
            // `Ok(())` — the driver sent a new value (shutdown flip, or any
            // other update); `Err(_)` — the driver's sender was dropped (the
            // scheduler is gone). Either way, stop.
            let _ = res;
            false
        }
    }
}

/// Per-flow `Every`-schedule loop body. Lives outside `start` so the driver
/// task and the loops are decoupled — the driver owns the workflows
/// HashMap, the loops just see the data they need.
#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_every_loop<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: Arc<BackoffPolicy>,
    cancel: Arc<RwLock<Option<CancellationHandle>>>,
    mut running: watch::Receiver<bool>,
    interval: Duration,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    if !*running.borrow() {
        return;
    }

    // Check if previous run is still active before executing. Tripped flows
    // skip dispatch entirely; backoff flows fall through to the loop where
    // the sleep math accounts for `next_eligible`.
    dispatch_flow_if_eligible(&workflow, &initial_state, &info, &policy, &cancel).await;

    loop {
        // Check `running` at the top of the iteration, mirroring
        // `spawn_cron_loop`. The driver's `send(false)` bumps the watch
        // version, and `borrow()` reads the latest value — a loop that was
        // inside `execute_flow` (or between `wait_until_eligible` and the
        // `select!`) still sees the flip here, so it can't sleep a full
        // `interval` after shutdown. The versioned `changed()` in
        // `sleep_unless_stopped` closes the same gap for a loop already
        // parked.
        if !*running.borrow() {
            break;
        }

        // Sleep at least `interval`, but if a backoff window pushes us further
        // out, sleep until that instant. The helper parks exactly once on the
        // watch channel; a shutdown `send(false)` wakes it immediately (no
        // polling).
        let wait = wait_until_eligible(&info, interval).await;
        if !sleep_unless_stopped(wait, &mut running).await {
            break;
        }

        if !dispatch_flow_if_eligible(&workflow, &initial_state, &info, &policy, &cancel).await {
            continue;
        }
    }
}

/// Per-flow `Cron`-schedule loop body. See [`spawn_every_loop`] for the
/// rationale on splitting the loop bodies out of `start`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_cron_loop<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: Arc<BackoffPolicy>,
    cancel: Arc<RwLock<Option<CancellationHandle>>>,
    mut running: watch::Receiver<bool>,
    schedule: Box<CronSchedule>,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    loop {
        if !*running.borrow() {
            break;
        }

        let now = Utc::now();
        let Some(next) = schedule.after(&now).next() else {
            // No future cron firing — exit the loop cleanly.
            break;
        };
        let wait_duration = (next - now).to_std().unwrap_or(Duration::from_secs(0));
        // Single-park sleep — same rationale as in spawn_every_loop: the watch
        // channel's versioned `changed()` can't lose a shutdown signal sent
        // while the loop wasn't parked, so no chunked polling is needed. Also:
        // re-validate that we're actually past `next` after waking (handles
        // wall-clock jumps backwards from NTP / suspend).
        if !sleep_unless_stopped(wait_duration, &mut running).await {
            break;
        }
        // Wall-clock jumped back? Sleep again until we're past `next`.
        // Single re-check is sufficient — a runaway clock would just loop here.
        let now2 = Utc::now();
        if now2 < next {
            let extra = (next - now2).to_std().unwrap_or(Duration::from_secs(0));
            if !sleep_unless_stopped(extra, &mut running).await {
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

        if !dispatch_flow_if_eligible(&workflow, &initial_state, &info, &policy, &cancel).await {
            continue;
        }
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
    running: watch::Sender<bool>,
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
                            let cancel = Arc::clone(&flow.cancel);
                            let handle = tokio::spawn(async move {
                                execute_reserved_flow(
                                    workflow,
                                    initial_state,
                                    info,
                                    &policy,
                                    cancel,
                                )
                                .await;
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
            SchedulerCommand::Cancel { id, response } => {
                let outcome = if let Some(flow) = workflows.get(&id) {
                    // Fire the in-flight run's cancellation handle, if any. The
                    // run observes `Cancelled` at its next await, drains its saga,
                    // and `apply_outcome` returns the flow to `Idle`. A flow that
                    // isn't currently running has no handle — an idempotent no-op.
                    if let Some(h) = flow.cancel.read().await.as_ref() {
                        h.cancel();
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
    // through the same graceful drain. Sending `false` on the watch channel
    // wakes every Every/Cron loop — whether it is currently parked in
    // `sleep_unless_stopped` (via `changed()`) or about to park (via
    // `borrow()`/`changed()`'s version check) — so shutdown latency is bounded
    // by how long an in-flight workflow takes, not by the schedule interval
    // and with no polling.
    let _ = running.send(false);

    // Cooperatively cancel every in-flight run so shutdown latency is bounded by
    // the time to the next await + the saga drain, not by how long the workflow
    // would naturally take. Each cancelled run drains its compensation stack and
    // returns `Cancelled` (recorded as Idle, not a failure, by `apply_outcome`).
    // The bounded wait below still caps the total drain time.
    for flow in workflows.values() {
        if let Some(h) = flow.cancel.read().await.as_ref() {
            h.cancel();
        }
    }

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
    //
    // Each `handle.await` is bounded by `DRAIN_TIMEOUT` so a single
    // non-cooperating flow (e.g. a tight CPU loop or a blocking call that
    // never reaches a cancellation point) can't make `stop()`/`wait()` hang
    // forever — cancellation is cooperative and `total_timeout` defaults to
    // `None`. On timeout the task is aborted as a last resort; the same 30s
    // budget the post-drain `'wait:` poll loop applies to in-flight
    // workflows.
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
    loop {
        let handle = {
            let mut tasks = scheduler_tasks.write().await;
            tasks.pop()
        };
        match handle {
            Some(h) => {
                let abort = h.abort_handle();
                *in_flight_drain.write().await = Some(abort.clone());
                if tokio::time::timeout(DRAIN_TIMEOUT, h).await.is_err() {
                    // The task ignored cooperative cancellation for the whole
                    // budget — force-stop it so shutdown can proceed.
                    abort.abort();
                }
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
    cancel: Arc<RwLock<Option<CancellationHandle>>>,
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

    execute_reserved_flow(workflow, initial_state, info, policy, cancel).await;
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
    cancel: Arc<RwLock<Option<CancellationHandle>>>,
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

    // Publish a fresh cancellation handle for this run so `cancel_flow` and
    // graceful shutdown can cooperatively stop it (and drain its saga). A fresh
    // token per run means cancelling one run never poisons a later one. Cleared
    // below once the run finishes, so a `cancel_flow` on an idle flow is a no-op.
    let (handle, token) = CancellationToken::new();
    *cancel.write().await = Some(handle);

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
        .execute_workflow(initial_state, total_budget, token)
        .instrument(tracing::info_span!("execute_flow"));
    #[cfg(not(feature = "tracing"))]
    let workflow_fut = workflow.execute_workflow(initial_state, total_budget, token);

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

    // The run is over: drop the handle so a later `cancel_flow` on this now-idle
    // flow is a clean no-op rather than firing a stale token.
    *cancel.write().await = None;

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
        // A deliberate cancellation (via `cancel_flow` or graceful shutdown) is
        // not a fault: return the flow to `Idle` without touching the failure
        // streak or backoff window, so its next scheduled run fires normally. A
        // *dirty* cancel whose rollback itself failed surfaces as
        // `compensation_failed`, which falls through to the backoff arm below.
        Err(ref e) if e.category() == "cancelled" => {
            info_guard.status = Status::Idle;
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

/// Execute one flow dispatch if the flow is currently dispatchable.
///
/// Shared by `spawn_every_loop` and `spawn_cron_loop`, which both gate
/// `execute_flow` behind `dispatchable_now` — the same block in three places.
/// Returns whether a dispatch actually happened so callers can `continue` the
/// loop when it didn't.
async fn dispatch_flow_if_eligible<TState, TResourceKey>(
    workflow: &Arc<Workflow<TState, TResourceKey>>,
    initial_state: &TState,
    info: &Arc<RwLock<FlowInfo>>,
    policy: &Arc<BackoffPolicy>,
    cancel: &Arc<RwLock<Option<CancellationHandle>>>,
) -> bool
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    if !dispatchable_now(info).await {
        return false;
    }
    execute_flow(
        Arc::clone(workflow),
        initial_state.clone(),
        Arc::clone(info),
        policy,
        Arc::clone(cancel),
    )
    .await;
    true
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
        // A shutdown send observed via the watch channel must interrupt the
        // sleep immediately — even if the loop wasn't parked at the instant of
        // the send (the receiver remembers the last seen version, so
        // `changed()` resolves on the next poll). This is the property the old
        // chunked-polling design existed to provide.
        let (tx, rx) = watch::channel(true);
        let mut rx = rx.clone();
        let start = tokio::time::Instant::now();
        let task =
            tokio::spawn(
                async move { sleep_unless_stopped(Duration::from_secs(10), &mut rx).await },
            );

        // Yield briefly so the helper has parked in its select, then flip
        // `running` via the watch sender.
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(false).unwrap();

        let returned_full = task.await.unwrap();
        let elapsed = start.elapsed();
        assert!(!returned_full, "helper must report early-exit");
        // The flip happens ~50ms after the helper parks; the watch `changed()`
        // wakes it within a scheduler tick — far under 1s.
        assert!(
            elapsed < Duration::from_secs(1),
            "helper must observe `running=false` promptly, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn sleep_unless_stopped_returns_false_on_zero_when_already_stopped() {
        // Regression: a cron loop with `wait_duration == 0` (cron tick already
        // elapsed, NTP forward jump, very tight cron) used to skip the running
        // check entirely because the `while !remaining.is_zero()` body never
        // ran — the helper returned `true` and the caller dispatched one extra
        // workflow after shutdown was requested.
        let (tx, rx) = watch::channel(false);
        let _tx = tx; // keep the sender alive so `changed()` stays well-formed
        let returned_full = sleep_unless_stopped(Duration::ZERO, &mut rx.clone()).await;
        assert!(
            !returned_full,
            "zero-duration sleep must surface running=false instead of short-circuiting to true"
        );
    }

    #[tokio::test]
    async fn sleep_unless_stopped_observes_shutdown_send() {
        // Sanity: the helper still responds to a shutdown send while parked.
        let (tx, rx) = watch::channel(true);
        let mut rx = rx.clone();
        let task =
            tokio::spawn(
                async move { sleep_unless_stopped(Duration::from_secs(10), &mut rx).await },
            );
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(false).unwrap();
        let returned_full = task.await.unwrap();
        assert!(!returned_full, "shutdown send must trigger early-exit");
    }
}
