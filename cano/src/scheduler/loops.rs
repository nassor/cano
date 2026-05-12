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
use tokio::task::JoinHandle;
use tokio::time::{Duration, sleep};

use crate::error::{CanoError, CanoResult};
use crate::workflow::Workflow;

#[cfg(feature = "tracing")]
use tracing::Instrument;

use super::{BackoffPolicy, FlowData, FlowInfo, SchedulerCommand, Status};

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
        // out, sleep until that instant. `notify` still wins in the select!
        // below.
        let wait = wait_until_eligible(&info, interval).await;
        tokio::select! {
            _ = sleep(wait) => {}
            _ = stop_notify.notified() => break,
        }

        // The notify_waiters → `notified()` race can be lost if sleep wins:
        // notify_waiters only signals waiters blocked at the time of the
        // call, so a follow-up `notified()` would not see it. The running
        // flag serves as a sticky shutdown signal.
        if !*running.read().await {
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
        tokio::select! {
            _ = sleep(wait_duration) => {}
            _ = stop_notify.notified() => break,
        }

        if !*running.read().await {
            break;
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
pub(super) async fn driver_task<TState, TResourceKey>(
    mut rx: mpsc::Receiver<SchedulerCommand>,
    workflows: HashMap<String, FlowData<TState, TResourceKey>>,
    flow_order: Vec<String>,
    running: Arc<RwLock<bool>>,
    stop_notify: Arc<Notify>,
    scheduler_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
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
    {
        let mut tasks = scheduler_tasks.write().await;
        while let Some(handle) = tasks.pop() {
            let _ = handle.await;
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
    // Execute workflow — skip lifecycle (setup/teardown handled by start/stop)
    #[cfg(feature = "tracing")]
    let result = workflow
        .execute_workflow(initial_state)
        .instrument(tracing::info_span!("execute_flow"))
        .await;

    #[cfg(not(feature = "tracing"))]
    let result = workflow.execute_workflow(initial_state).await;

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
            let err_str = e.to_string();
            let new_streak = info_guard.failure_streak.saturating_add(1);
            info_guard.failure_streak = new_streak;
            if policy.is_tripped(new_streak) {
                info_guard.next_eligible = None;
                info_guard.status = Status::Tripped {
                    streak: new_streak,
                    last_error: err_str,
                };
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
