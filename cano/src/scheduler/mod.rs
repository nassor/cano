//! # Simplified Scheduler API
//!
//! A simplified scheduler that works with the new Workflow API with split/join support.
//!
//! ## 🚀 Quick Start
//!
//! > **Note:** The scheduler is an optional feature. To use it, you must enable
//! > the `"scheduler"` feature in your `Cargo.toml`.
//!
//! ```toml
//! [dependencies]
//! cano = { version = "0.11", features = ["scheduler"] }
//! ```

mod backoff;

pub use backoff::BackoffPolicy;

use crate::error::{CanoError, CanoResult};
use crate::workflow::Workflow;
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Notify, RwLock, mpsc, oneshot};
use tokio::time::{Duration, sleep};

#[cfg(feature = "tracing")]
use tracing::Instrument;

/// Commands sent over the internal scheduler control channel.
///
/// Using a typed enum instead of magic strings eliminates a class of runtime
/// errors and makes the protocol self-documenting.
enum SchedulerCommand {
    Stop,
    Trigger {
        id: String,
        response: oneshot::Sender<CanoResult<()>>,
    },
    Reset {
        id: String,
        response: oneshot::Sender<CanoResult<()>>,
    },
}

/// Simplified scheduling options
#[derive(Debug, Clone)]
pub enum Schedule {
    /// Run every Duration interval
    Every(Duration),
    /// Cron expression (for advanced users)
    Cron(String),
    /// Manual trigger only
    Manual,
}

/// Simple workflow status.
///
/// `#[non_exhaustive]` because future scheduler features may add variants.
/// External `match` arms must include a wildcard.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Status {
    Idle,
    Running,
    Completed,
    /// Terminal failure for a flow without a [`BackoffPolicy`]. Carries the
    /// last error message for observability.
    ///
    /// Invariant: only reachable when the flow has no policy attached.
    /// `apply_outcome` upholds this; a debug-build assertion guards it.
    /// Flows with a policy use [`Status::Backoff`] or [`Status::Tripped`]
    /// instead, which is what avoids the legacy `Failed` flicker.
    Failed(String),
    /// Flow failed and is waiting until `until` before its next dispatch
    /// (set only when a [`BackoffPolicy`] is attached).
    Backoff {
        until: DateTime<Utc>,
        streak: u32,
        last_error: String,
    },
    /// Flow has reached its [`BackoffPolicy::streak_limit`] and will not
    /// dispatch again until [`Scheduler::reset_flow`] is called.
    Tripped {
        streak: u32,
        last_error: String,
    },
}

/// Workflow information.
///
/// Adding fields here is an additive change; downstream code that constructs
/// `FlowInfo` literally (rare — usually only this crate constructs it) needs
/// `..` rest patterns or to use the public-fields-with-default convention.
#[derive(Debug, Clone)]
pub struct FlowInfo {
    pub id: String,
    pub status: Status,
    pub run_count: u64,
    pub last_run: Option<DateTime<Utc>>,
    /// Number of consecutive failures since the last success. Always 0 when
    /// the flow has no [`BackoffPolicy`] attached.
    pub failure_streak: u32,
    /// When set, the next dispatch must wait until this time. Cleared on
    /// success and on `reset_flow`.
    pub next_eligible: Option<DateTime<Utc>>,
}

/// Internal schedule representation. The public [`Schedule`] keeps the cron
/// expression as a string for ergonomics; we parse it once at registration
/// time and cache the result so the spawned scheduler loop never re-parses.
#[derive(Clone)]
enum ParsedSchedule {
    Every(Duration),
    /// Boxed because `CronSchedule` is ~248 bytes and would dominate the enum size.
    Cron(Box<CronSchedule>),
    Manual,
}

/// Per-flow data stored in the scheduler. Carries the workflow, its initial
/// state, the parsed schedule, the live `FlowInfo` shared with status readers,
/// and an optional [`BackoffPolicy`].
struct FlowData<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    schedule: ParsedSchedule,
    info: Arc<RwLock<FlowInfo>>,
    /// `None` keeps the legacy "fail-and-retry-on-base-schedule" behavior.
    /// `Some(policy)` activates the backoff/trip code path.
    policy: Option<Arc<BackoffPolicy>>,
}

impl<TState, TResourceKey> Clone for FlowData<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            workflow: Arc::clone(&self.workflow),
            initial_state: self.initial_state.clone(),
            schedule: self.schedule.clone(),
            info: Arc::clone(&self.info),
            policy: self.policy.clone(),
        }
    }
}

/// Scheduler system for managing workflows.
///
/// All workflows registered with a single `Scheduler` instance must share the same
/// `TState` and `TResourceKey` types. The resource key type defaults to
/// [`Cow<'static, str>`](std::borrow::Cow), which accepts both `&'static str`
/// literals (allocation-free) and owned `String` keys.
/// If your application requires workflows with different state enums,
/// create a separate `Scheduler` instance for each state type.
///
/// Requires the `scheduler` feature.
#[derive(Clone)]
pub struct Scheduler<TState, TResourceKey = Cow<'static, str>>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    workflows: HashMap<String, FlowData<TState, TResourceKey>>,
    /// Registration order of workflow ids. Setup runs FIFO, teardown LIFO.
    /// Required because `HashMap` iteration order is undefined.
    flow_order: Vec<String>,
    command_tx: Arc<RwLock<Option<mpsc::Sender<SchedulerCommand>>>>,
    running: Arc<RwLock<bool>>,
    /// Set once `start()` has flipped the runtime live (after setup succeeds)
    /// and cleared by the Stop command handler. Read synchronously by the
    /// registration methods (`every` / `cron` / `manual` / `set_backoff`) so
    /// they can refuse mutations after the scheduler is live — the spawned
    /// loops snapshot policy and registry at start time, so post-start
    /// mutations would otherwise be silently dropped.
    started: Arc<AtomicBool>,
    /// Wake the spawned Every/Cron loops so `stop()` interrupts long sleeps
    /// instead of waiting for the timer to fire. Allocated per `start()` call
    /// and taken (and signalled) by the Stop branch.
    stop_notify: Arc<RwLock<Option<Arc<Notify>>>>,
    scheduler_tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
}

impl<TState, TResourceKey> Scheduler<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Create a new scheduler
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
            flow_order: Vec::new(),
            command_tx: Arc::new(RwLock::new(None)),
            running: Arc::new(RwLock::new(false)),
            started: Arc::new(AtomicBool::new(false)),
            stop_notify: Arc::new(RwLock::new(None)),
            scheduler_tasks: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Refuse a mutation when the scheduler has already been started — the
    /// spawned loops captured the registry and policies at start time, so any
    /// post-start change would be silently dropped. Returns
    /// [`CanoError::Configuration`] when not allowed.
    fn ensure_not_started(&self, op: &str) -> CanoResult<()> {
        if self.started.load(Ordering::Acquire) {
            Err(CanoError::Configuration(format!(
                "Scheduler is already started — {op} must be called before start()"
            )))
        } else {
            Ok(())
        }
    }

    /// Add a workflow that runs every Duration interval
    pub fn every(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TResourceKey>,
        initial_state: TState,
        interval: Duration,
    ) -> CanoResult<()> {
        self.add_flow_internal(id, workflow, initial_state, ParsedSchedule::Every(interval))
    }

    /// Add a workflow that runs every N seconds (convenience method)
    pub fn every_seconds(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TResourceKey>,
        initial_state: TState,
        seconds: u64,
    ) -> CanoResult<()> {
        self.every(id, workflow, initial_state, Duration::from_secs(seconds))
    }

    /// Add a workflow that runs every N minutes (convenience method)
    pub fn every_minutes(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TResourceKey>,
        initial_state: TState,
        minutes: u64,
    ) -> CanoResult<()> {
        self.every(
            id,
            workflow,
            initial_state,
            Duration::from_secs(minutes * 60),
        )
    }

    /// Add a workflow that runs every N hours (convenience method)
    pub fn every_hours(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TResourceKey>,
        initial_state: TState,
        hours: u64,
    ) -> CanoResult<()> {
        self.every(
            id,
            workflow,
            initial_state,
            Duration::from_secs(hours * 3600),
        )
    }

    /// Add a workflow with cron schedule
    pub fn cron(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TResourceKey>,
        initial_state: TState,
        expr: &str,
    ) -> CanoResult<()> {
        // Parse once at registration time so the scheduler loop can reuse the
        // result without re-parsing on every wake-up (and without an `expect`).
        let parsed = CronSchedule::from_str(expr)
            .map_err(|e| CanoError::Configuration(format!("Invalid cron expression: {e}")))?;
        self.add_flow_internal(
            id,
            workflow,
            initial_state,
            ParsedSchedule::Cron(Box::new(parsed)),
        )
    }

    /// Add a manually triggered workflow
    pub fn manual(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TResourceKey>,
        initial_state: TState,
    ) -> CanoResult<()> {
        self.add_flow_internal(id, workflow, initial_state, ParsedSchedule::Manual)
    }

    fn add_flow_internal(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TResourceKey>,
        initial_state: TState,
        schedule: ParsedSchedule,
    ) -> CanoResult<()> {
        self.ensure_not_started("registering a workflow")?;
        if self.workflows.contains_key(id) {
            return Err(CanoError::Configuration(format!(
                "Flow '{}' already exists",
                id
            )));
        }

        let info = Arc::new(RwLock::new(FlowInfo {
            id: id.to_string(),
            status: Status::Idle,
            run_count: 0,
            last_run: None,
            failure_streak: 0,
            next_eligible: None,
        }));

        self.workflows.insert(
            id.to_string(),
            FlowData {
                workflow: Arc::new(workflow),
                initial_state,
                schedule,
                info,
                policy: None,
            },
        );
        self.flow_order.push(id.to_string());

        Ok(())
    }

    /// Attach a [`BackoffPolicy`] to an existing flow.
    ///
    /// After failure the scheduler will sleep `policy.compute_delay(streak)`
    /// before the next dispatch and trip the flow when `streak_limit` is hit.
    /// Without this call a flow keeps the legacy "fail and try again on the
    /// base schedule" behavior.
    ///
    /// Must be called **before** [`Scheduler::start`]. The spawned loop tasks
    /// snapshot the policy at start time, so a post-start call returns an
    /// error rather than silently being dropped.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Configuration`] — `id` is not registered, or the
    ///   scheduler has already been started.
    pub fn set_backoff(&mut self, id: &str, policy: BackoffPolicy) -> CanoResult<()> {
        self.ensure_not_started("set_backoff")?;
        let flow = self.workflows.get_mut(id).ok_or_else(|| {
            CanoError::Configuration(format!("Flow '{id}' not found — cannot set backoff"))
        })?;
        flow.policy = Some(Arc::new(policy));
        Ok(())
    }

    /// Start the scheduler, running all registered workflows on their configured schedules.
    ///
    /// Sets up all workflow resources before starting. If setup fails for a workflow,
    /// already-setup workflows are torn down in reverse order and the error is returned.
    ///
    /// Blocks until [`Scheduler::stop`] is called. After the stop signal is received the
    /// scheduler waits up to 30 seconds for in-progress workflow executions to finish,
    /// then tears down all workflow resources in reverse registration order.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the 30-second graceful-shutdown timeout elapsed while
    ///   one or more workflows were still running
    /// - Any resource setup error from a registered workflow
    pub async fn start(&mut self) -> CanoResult<()> {
        // Capacity of 64: Stop is sent once, each Trigger enqueues one item.
        // try_send in stop()/trigger() returns an error if this fills up rather
        // than growing without bound.
        let (tx, mut rx) = mpsc::channel::<SchedulerCommand>(64);
        *self.command_tx.write().await = Some(tx);

        // Lock the registry against post-start mutations *before* we snapshot
        // it. Setting `started` later (e.g. after `running = true`) leaves a
        // window where another `Scheduler` clone could call `set_backoff`,
        // pass `ensure_not_started`, and have its mutation silently dropped
        // because the snapshot was already taken. Failure paths below reset
        // the flag so a failed start does not permanently lock the scheduler.
        self.started.store(true, Ordering::Release);

        let workflows = self.workflows.clone();

        // Validate every registered workflow upfront so misconfigurations surface
        // before any resource setup runs (matches `Workflow::orchestrate`'s contract).
        // Setup then runs in registration order; on failure, the previously-setup
        // workflows are torn down in reverse registration order. Iterating `flow_order`
        // guarantees deterministic FIFO setup / LIFO teardown — `HashMap::values()`
        // would not. The `running` flag is intentionally not flipped to `true` until
        // after both validate and setup succeed, so observers (and the spawned loop
        // tasks below) never see "running" for a scheduler that failed to start.
        {
            let ordered: Vec<&FlowData<TState, TResourceKey>> = self
                .flow_order
                .iter()
                .filter_map(|id| workflows.get(id))
                .collect();
            for flow in ordered.iter() {
                if let Err(e) = flow
                    .workflow
                    .validate()
                    .and_then(|_| flow.workflow.validate_initial_state(&flow.initial_state))
                {
                    self.rollback_start().await;
                    return Err(e);
                }
            }
            for (idx, flow) in ordered.iter().enumerate() {
                if let Err(e) = flow.workflow.resources.setup_all().await {
                    for prior in ordered[..idx].iter().rev() {
                        let len = prior.workflow.resources.lifecycle_len();
                        prior.workflow.resources.teardown_range(0..len).await;
                    }
                    self.rollback_start().await;
                    return Err(e);
                }
            }
        }

        // Allocate a fresh stop notifier for this run. Loop tasks clone the Arc and
        // race the timer against `notified()` so `stop()` interrupts long sleeps
        // immediately instead of waiting for the cron / interval to fire.
        let notify = Arc::new(Notify::new());
        *self.stop_notify.write().await = Some(Arc::clone(&notify));

        // Setup succeeded — flip the runtime-live flag and spawn loop tasks.
        // `started` was already set at the top of this function so that
        // post-start registration mutations are rejected even during the
        // setup window; it stays true until the Stop handler resets it below.
        *self.running.write().await = true;

        let running = Arc::clone(&self.running);
        let scheduler_tasks = Arc::clone(&self.scheduler_tasks);

        // Spawn scheduler task loops for each workflow
        for (_id, flow) in workflows {
            let running_clone = Arc::clone(&running);
            let notify_clone = Arc::clone(&notify);
            let FlowData {
                workflow,
                initial_state,
                schedule,
                info,
                policy,
            } = flow;

            match schedule {
                ParsedSchedule::Every(interval) => {
                    let handle = tokio::spawn(async move {
                        // Check running flag before first execution
                        if !*running_clone.read().await {
                            return;
                        }

                        // Check if previous run is still active before executing.
                        // Tripped flows skip dispatch entirely; backoff flows fall
                        // through to the loop where the sleep math accounts for
                        // `next_eligible`.
                        if dispatchable_now(&info).await {
                            execute_flow(
                                Arc::clone(&workflow),
                                initial_state.clone(),
                                Arc::clone(&info),
                                policy.as_ref(),
                            )
                            .await;
                        }

                        loop {
                            // Sleep at least `interval`, but if a backoff window
                            // pushes us further out, sleep until that instant.
                            // `notify` still wins in the select! below.
                            let wait = wait_until_eligible(&info, interval).await;
                            tokio::select! {
                                _ = sleep(wait) => {}
                                _ = notify_clone.notified() => break,
                            }

                            // Check running flag immediately after sleep, before executing
                            if !*running_clone.read().await {
                                break;
                            }

                            if !dispatchable_now(&info).await {
                                continue;
                            }

                            execute_flow(
                                Arc::clone(&workflow),
                                initial_state.clone(),
                                Arc::clone(&info),
                                policy.as_ref(),
                            )
                            .await;
                        }
                    });
                    scheduler_tasks.write().await.push(handle);
                }
                ParsedSchedule::Cron(schedule) => {
                    let handle = tokio::spawn(async move {
                        loop {
                            // Check running flag BEFORE calculating next execution
                            if !*running_clone.read().await {
                                break;
                            }

                            let now = Utc::now();
                            if let Some(next) = schedule.after(&now).next() {
                                let wait_duration = match (next - now).to_std() {
                                    Ok(d) => d,
                                    Err(_) => {
                                        // We're past the scheduled time; execute immediately
                                        Duration::from_secs(0)
                                    }
                                };
                                tokio::select! {
                                    _ = sleep(wait_duration) => {}
                                    _ = notify_clone.notified() => break,
                                }

                                // Check again after sleep before executing
                                if !*running_clone.read().await {
                                    break;
                                }

                                // If a backoff window pushes us past this tick,
                                // skip dispatch and let the next iteration pick
                                // up the following cron firing.
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
                                    policy.as_ref(),
                                )
                                .await;
                            }
                        }
                    });
                    scheduler_tasks.write().await.push(handle);
                }
                ParsedSchedule::Manual => {
                    // Manual workflows don't run on their own
                }
            }
        }

        // Wait for stop command
        while let Some(cmd) = rx.recv().await {
            match cmd {
                SchedulerCommand::Stop => {
                    *self.running.write().await = false;
                    self.started.store(false, Ordering::Release);
                    // Wake any Every/Cron loop currently parked on its sleep so
                    // they observe `running == false` and exit immediately,
                    // bounding shutdown latency by how long an in-flight workflow
                    // takes — not by the schedule interval.
                    if let Some(n) = self.stop_notify.write().await.take() {
                        n.notify_waiters();
                    }
                    // Clear the sender immediately so callers racing with the
                    // graceful-shutdown window (draining loop tasks + waiting on
                    // in-flight workflows) get a deterministic "not running" error
                    // instead of enqueueing commands into a queue nobody is draining.
                    *self.command_tx.write().await = None;
                    break;
                }
                SchedulerCommand::Trigger { id, response } => {
                    let outcome = if let Some(flow) = self.workflows.get(&id) {
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
                                let policy = flow.policy.clone();
                                let handle = tokio::spawn(async move {
                                    execute_reserved_flow(
                                        workflow,
                                        initial_state,
                                        info,
                                        policy.as_ref(),
                                    )
                                    .await;
                                });
                                let mut tasks = self.scheduler_tasks.write().await;
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
                    let outcome = if let Some(flow) = self.workflows.get(&id) {
                        let mut info_guard = flow.info.write().await;
                        info_guard.failure_streak = 0;
                        info_guard.next_eligible = None;
                        // Don't clobber a `Running` status — a concurrent execution
                        // would set Completed/Backoff/Tripped on its own write.
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

        // Wait for all scheduler loop tasks to finish
        let mut tasks = self.scheduler_tasks.write().await;
        while let Some(handle) = tasks.pop() {
            let _ = handle.await;
        }
        drop(tasks);

        // Wait for any running workflows to complete
        let timeout = Duration::from_secs(30);
        let start_time = tokio::time::Instant::now();

        let mut result = Ok(());
        while self.has_running_flows().await {
            if start_time.elapsed() >= timeout {
                result = Err(CanoError::Workflow(
                    "Timeout waiting for workflows to complete".to_string(),
                ));
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }

        // Teardown workflow resources in reverse registration order (LIFO).
        // Driven by `flow_order` rather than `HashMap::values()` to keep teardown
        // deterministic across runs.
        let flows_for_teardown: Vec<&FlowData<TState, TResourceKey>> = self
            .flow_order
            .iter()
            .filter_map(|id| self.workflows.get(id))
            .collect();
        for flow in flows_for_teardown.iter().rev() {
            let len = flow.workflow.resources.lifecycle_len();
            flow.workflow.resources.teardown_range(0..len).await;
        }

        // Clear the command sender so subsequent stop()/trigger() calls report
        // the documented "Scheduler not running" error instead of a channel-closed
        // surprise, and allow clean restarts via a fresh start() call.
        *self.command_tx.write().await = None;

        result
    }

    /// Clear the command channel and release the registry lock when validation
    /// or setup at the top of `start()` fails, so the scheduler can be retried.
    async fn rollback_start(&self) {
        *self.command_tx.write().await = None;
        self.started.store(false, Ordering::Release);
    }

    /// Stop the scheduler and wait for all running workflows to complete
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running
    pub async fn stop(&self) -> CanoResult<()> {
        // Clone the sender out of the lock and drop the guard before awaiting
        // send(): holding an async RwLock guard across .await would block a
        // concurrent writer (e.g. start() clearing command_tx at shutdown).
        let tx = self.command_tx.read().await.clone().ok_or_else(|| {
            CanoError::Workflow("Scheduler not running — call start() before stop()".to_string())
        })?;

        // Use send().await to ensure delivery for this critical control-plane
        // command. Safe because stop is rare and waiting for queue space is OK.
        tx.send(SchedulerCommand::Stop)
            .await
            .map_err(|e| CanoError::Workflow(format!("Failed to send stop: {}", e)))?;

        // The actual waiting happens in start()
        Ok(())
    }

    /// Manually trigger a workflow by ID.
    ///
    /// Sends a trigger command to the running scheduler. The workflow executes
    /// asynchronously — this method returns once the scheduler accepts or rejects
    /// the trigger.
    ///
    /// Manual triggers **bypass** an active [`Status::Backoff`] window — the
    /// operator is presumed to know what they're doing. They are **rejected**
    /// when the flow is in [`Status::Tripped`]; call
    /// [`Scheduler::reset_flow`] first to clear the trip.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running (i.e. [`Scheduler::start`]
    ///   has not been called or has already returned), `id` is unknown, the workflow
    ///   is already running, the flow is tripped (call [`Scheduler::reset_flow`]
    ///   first), or the command queue is full.
    pub async fn trigger(&self, id: &str) -> CanoResult<()> {
        let tx = self.command_tx.read().await.clone().ok_or_else(|| {
            CanoError::Workflow("Scheduler not running — call start() before trigger()".to_string())
        })?;

        // No local `workflows.contains_key` pre-check: the caller may be a
        // stale `Scheduler` clone that snapshotted the registry before the
        // running instance registered `id`. The scheduler loop holds the
        // authoritative registry and returns the same "No workflow registered"
        // error on lookup miss — let the loop be the single source of truth.
        let (response_tx, response_rx) = oneshot::channel();
        tx.try_send(SchedulerCommand::Trigger {
            id: id.to_string(),
            response: response_tx,
        })
        .map_err(|e| CanoError::Workflow(format!("Failed to trigger: {}", e)))?;

        response_rx.await.map_err(|_| {
            CanoError::Workflow("Scheduler stopped before trigger was processed".to_string())
        })?
    }

    /// Get status of a specific workflow
    pub async fn status(&self, id: &str) -> Option<FlowInfo> {
        if let Some(flow) = self.workflows.get(id) {
            Some(flow.info.read().await.clone())
        } else {
            None
        }
    }

    /// List all workflows
    pub async fn list(&self) -> Vec<FlowInfo> {
        let mut results = Vec::with_capacity(self.flow_order.len());
        for id in &self.flow_order {
            if let Some(flow) = self.workflows.get(id) {
                results.push(flow.info.read().await.clone());
            }
        }
        results
    }

    /// Check if scheduler has running flows
    pub async fn has_running_flows(&self) -> bool {
        for flow in self.workflows.values() {
            if flow.info.read().await.status == Status::Running {
                return true;
            }
        }
        false
    }

    /// Reset a flow that hit its [`BackoffPolicy::streak_limit`] (or that you
    /// otherwise want to clear). Sends a [`SchedulerCommand::Reset`] to the
    /// running scheduler so the authoritative registry is updated.
    ///
    /// Clears `failure_streak`, `next_eligible`, and (when the flow is not
    /// currently running) sets `status = Idle`.
    ///
    /// # Race with in-flight executions
    ///
    /// `reset_flow` serializes against `trigger` via the scheduler's command
    /// channel, but the post-execution status write (`apply_outcome`) runs in
    /// the spawned execution task and is **not** routed through that channel.
    /// If a flow is mid-execution when `reset_flow` is called and the run
    /// subsequently fails, `apply_outcome` will increment the freshly-cleared
    /// streak back to `1` and (depending on the policy) re-park the flow in
    /// [`Status::Backoff`]. The reset is not lost — the streak is `1` rather
    /// than `streak_limit` — but operators should prefer to call `reset_flow`
    /// only after observing the flow in [`Status::Tripped`] or
    /// [`Status::Idle`] via [`Scheduler::status`] to avoid the surprise.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running, `id` is unknown,
    ///   or the command queue is full.
    pub async fn reset_flow(&self, id: &str) -> CanoResult<()> {
        let tx = self.command_tx.read().await.clone().ok_or_else(|| {
            CanoError::Workflow(
                "Scheduler not running — call start() before reset_flow()".to_string(),
            )
        })?;

        let (response_tx, response_rx) = oneshot::channel();
        tx.try_send(SchedulerCommand::Reset {
            id: id.to_string(),
            response: response_tx,
        })
        .map_err(|e| CanoError::Workflow(format!("Failed to reset: {}", e)))?;

        response_rx.await.map_err(|_| {
            CanoError::Workflow("Scheduler stopped before reset was processed".to_string())
        })?
    }
}

impl<TState, TResourceKey> Default for Scheduler<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort cleanup if the user drops a started scheduler without calling
/// [`Scheduler::stop`]. Only the final clone (i.e. when we are the last
/// holder of the shared `scheduler_tasks` Arc) aborts the spawned loops —
/// earlier clones leave them alive for the remaining holders. If `stop` was
/// already issued (`started == false`) we exit without touching anything.
///
/// **Limitation:** the `Arc::strong_count > 1` gate is racy under concurrent
/// drops of the last two clones — both may observe count `> 1` and skip the
/// abort, leaking the spawned loops. The fallback is identical to having no
/// `Drop` impl at all (the prior behavior), so this is strictly an
/// improvement; for guaranteed teardown call [`Scheduler::stop`] explicitly.
impl<TState, TResourceKey> Drop for Scheduler<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if Arc::strong_count(&self.scheduler_tasks) > 1 {
            return;
        }
        if !self.started.load(Ordering::Acquire) {
            return;
        }
        if let Ok(handles) = self.scheduler_tasks.try_write() {
            let n = handles.len();
            for h in handles.iter() {
                h.abort();
            }
            #[cfg(feature = "tracing")]
            tracing::warn!(
                aborted = n,
                "Scheduler dropped without stop() — aborted spawned loop tasks"
            );
            #[cfg(not(feature = "tracing"))]
            let _ = n;
        }
    }
}

async fn execute_flow<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: Option<&Arc<BackoffPolicy>>,
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
    policy: Option<&Arc<BackoffPolicy>>,
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

    apply_outcome(&info, result.map(|_| ()), policy.map(|p| p.as_ref())).await;
}

/// Atomic post-execution status write. With a policy attached the loop never
/// flickers through `Status::Failed` on the way to `Backoff`/`Tripped` — a
/// single write under the existing lock decides the terminal status for this
/// run. Streak and `next_eligible` are reset on success.
async fn apply_outcome(
    info: &Arc<RwLock<FlowInfo>>,
    result: Result<(), CanoError>,
    policy: Option<&BackoffPolicy>,
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
            match policy {
                None => {
                    debug_assert!(
                        info_guard.failure_streak == 0,
                        "Status::Failed path should never see a non-zero failure_streak (no policy is attached)"
                    );
                    info_guard.status = Status::Failed(err_str);
                    // No policy: don't track streak / next_eligible.
                }
                Some(p) => {
                    let new_streak = info_guard.failure_streak.saturating_add(1);
                    info_guard.failure_streak = new_streak;
                    if p.is_tripped(new_streak) {
                        info_guard.next_eligible = None;
                        info_guard.status = Status::Tripped {
                            streak: new_streak,
                            last_error: err_str,
                        };
                    } else {
                        let delay = p.compute_delay(new_streak);
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
    use crate::node::Node;
    use crate::resource::Resources;
    use cano_macros::node;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::time::sleep;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum TestState {
        Start,
        Complete,
        Error,
    }

    #[derive(Clone)]
    struct TestNode {
        execution_count: Arc<AtomicU32>,
        should_fail: bool,
    }

    impl TestNode {
        fn new() -> Self {
            Self {
                execution_count: Arc::new(AtomicU32::new(0)),
                should_fail: false,
            }
        }

        fn new_failing() -> Self {
            Self {
                execution_count: Arc::new(AtomicU32::new(0)),
                should_fail: true,
            }
        }
    }

    #[node]
    impl Node<TestState> for TestNode {
        type PrepResult = ();
        type ExecResult = ();

        async fn prep(&self, _res: &Resources) -> CanoResult<()> {
            Ok(())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            self.execution_count.fetch_add(1, Ordering::Relaxed);
        }

        async fn post(
            &self,
            _res: &Resources,
            _exec_res: Self::ExecResult,
        ) -> CanoResult<TestState> {
            if self.should_fail {
                Err(CanoError::NodeExecution("Test failure".to_string()))
            } else {
                Ok(TestState::Complete)
            }
        }
    }

    fn create_test_workflow() -> Workflow<TestState> {
        Workflow::bare()
            .register(TestState::Start, TestNode::new())
            .add_exit_state(TestState::Complete)
            .add_exit_state(TestState::Error)
    }

    fn create_failing_workflow() -> Workflow<TestState> {
        Workflow::bare()
            .register(TestState::Start, TestNode::new_failing())
            .add_exit_state(TestState::Complete)
            .add_exit_state(TestState::Error)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_scheduler_creation() {
        let scheduler: Scheduler<TestState> = Scheduler::new();
        assert!(!scheduler.has_running_flows().await);
        assert!(scheduler.list().await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_seconds() {
        let mut scheduler = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every_seconds("test_task", workflow, TestState::Start, 5);
        assert!(result.is_ok());

        let flows = scheduler.list().await;
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].id, "test_task");
        assert_eq!(flows[0].status, Status::Idle);
        assert_eq!(flows[0].run_count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_minutes() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every_minutes("test_task", workflow, TestState::Start, 2);
        assert!(result.is_ok());

        let status = scheduler.status("test_task").await;
        assert!(status.is_some());
        assert_eq!(status.unwrap().id, "test_task");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_hours() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every_hours("test_task", workflow, TestState::Start, 1);
        assert!(result.is_ok());

        let status = scheduler.status("test_task").await;
        assert!(status.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_duration() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every(
            "test_task",
            workflow,
            TestState::Start,
            Duration::from_millis(100),
        );
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_cron() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        // Valid cron expression
        let result = scheduler.cron("test_task", workflow, TestState::Start, "0 */5 * * * *");
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_cron_invalid() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        // Invalid cron expression
        let result = scheduler.cron("test_task", workflow, TestState::Start, "invalid cron");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CanoError::Configuration(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_manual() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.manual("test_task", workflow, TestState::Start);
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_duplicate_workflow_id() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow1 = create_test_workflow();
        let workflow2 = create_test_workflow();

        scheduler
            .every_seconds("test_task", workflow1, TestState::Start, 5)
            .unwrap();

        let result = scheduler.every_seconds("test_task", workflow2, TestState::Start, 10);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CanoError::Configuration(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_rejects_misconfigured_workflow() {
        // A workflow with no exit states fails `validate()`. The scheduler must surface
        // that error from `start()` rather than running setup and failing later at the
        // first scheduled execution.
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let bad_workflow: Workflow<TestState> =
            Workflow::bare().register(TestState::Start, TestNode::new());

        scheduler
            .every_seconds("bad", bad_workflow, TestState::Start, 60)
            .unwrap();

        let err = scheduler.start().await.expect_err("start should reject");
        assert!(matches!(err, CanoError::Configuration(_)));
        assert!(!scheduler.has_running_flows().await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_rejects_unregistered_initial_state() {
        // Initial state isn't registered or an exit state — `validate_initial_state`
        // must catch this at start, before any resource setup runs.
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow: Workflow<TestState> = Workflow::bare()
            .register(TestState::Start, TestNode::new())
            .add_exit_state(TestState::Complete);

        scheduler
            .every_seconds("bad_init", workflow, TestState::Error, 60)
            .unwrap();

        let err = scheduler.start().await.expect_err("start should reject");
        assert!(matches!(err, CanoError::Configuration(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_failed_start_leaves_scheduler_idle() {
        // After a failed start (validation error), the running flag must stay off
        // and the command channel cleared, so a follow-up stop() reports the
        // documented "Scheduler not running" error rather than waiting forever.
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let bad_workflow: Workflow<TestState> =
            Workflow::bare().register(TestState::Start, TestNode::new());
        scheduler
            .every_seconds("bad", bad_workflow, TestState::Start, 60)
            .unwrap();

        scheduler.start().await.expect_err("start should reject");

        let stop_err = scheduler
            .stop()
            .await
            .expect_err("stop should fail when start did not succeed");
        assert!(matches!(stop_err, CanoError::Workflow(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stop_interrupts_long_interval_sleep() {
        // Register an Every-loop workflow with a 1-hour interval. After start(),
        // the loop parks on `sleep(1 hour)`. Stop must wake the sleep via the
        // notify so the scheduler returns within seconds, not hours.
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler
            .every(
                "long",
                workflow,
                TestState::Start,
                Duration::from_secs(3600),
            )
            .unwrap();

        let mut start_handle = scheduler.clone();
        let start_task = tokio::spawn(async move { start_handle.start().await });

        // Give the spawned tasks time to enter their sleep.
        sleep(Duration::from_millis(100)).await;

        let stop_started = tokio::time::Instant::now();
        scheduler.stop().await.expect("stop should succeed");
        let elapsed = stop_started.elapsed();

        // start() returns once stop completes; bound the whole flow so a
        // regression to uncancellable sleep would fail the test fast.
        let result = tokio::time::timeout(Duration::from_secs(5), start_task)
            .await
            .expect("start should return shortly after stop")
            .expect("start task should not panic");
        assert!(result.is_ok());
        assert!(
            elapsed < Duration::from_secs(2),
            "stop took too long: {:?}",
            elapsed
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stop_interrupts_cron_sleep() {
        // Cron expression that fires once a day. Without notify-driven cancellation
        // the loop would sleep up to 24 hours; stop() must wake it immediately.
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler
            .cron("daily", workflow, TestState::Start, "0 0 0 * * *")
            .unwrap();

        let mut start_handle = scheduler.clone();
        let start_task = tokio::spawn(async move { start_handle.start().await });

        sleep(Duration::from_millis(100)).await;

        let stop_started = tokio::time::Instant::now();
        scheduler.stop().await.expect("stop should succeed");
        let elapsed = stop_started.elapsed();

        let result = tokio::time::timeout(Duration::from_secs(5), start_task)
            .await
            .expect("start should return shortly after stop")
            .expect("start task should not panic");
        assert!(result.is_ok());
        assert!(
            elapsed < Duration::from_secs(2),
            "stop took too long: {:?}",
            elapsed
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_manual_trigger() {
        let timeout = Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = create_test_workflow();
            scheduler
                .manual("test_task", workflow, TestState::Start)
                .unwrap();

            // Clone the scheduler — both clones share the same internal state
            let mut scheduler_for_start = scheduler.clone();

            // Start scheduler in background using the clone
            let scheduler_handle = tokio::spawn(async move { scheduler_for_start.start().await });

            // Give time for scheduler to start
            sleep(Duration::from_millis(50)).await;

            // Trigger the workflow on the original scheduler (shares command_tx)
            scheduler.trigger("test_task").await.unwrap();

            // Wait a bit for execution
            sleep(Duration::from_millis(100)).await;

            // Check status
            let status = scheduler.status("test_task").await;
            assert!(status.is_some());

            // Stop scheduler
            scheduler.stop().await.unwrap();

            // Wait for scheduler to stop
            let _ = tokio::time::timeout(Duration::from_millis(100), scheduler_handle).await;
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_status_check() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler
            .manual("test_task", workflow, TestState::Start)
            .unwrap();

        let status = scheduler.status("test_task").await;
        assert!(status.is_some());

        let status = status.unwrap();
        assert_eq!(status.id, "test_task");
        assert_eq!(status.status, Status::Idle);
        assert_eq!(status.run_count, 0);
        assert!(status.last_run.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_list_workflows() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();

        scheduler
            .manual("task1", create_test_workflow(), TestState::Start)
            .unwrap();
        scheduler
            .manual("task2", create_test_workflow(), TestState::Start)
            .unwrap();
        scheduler
            .manual("task3", create_test_workflow(), TestState::Start)
            .unwrap();

        let flows = scheduler.list().await;
        assert_eq!(flows.len(), 3);

        let ids: Vec<String> = flows.iter().map(|f| f.id.clone()).collect();
        assert!(ids.contains(&"task1".to_string()));
        assert!(ids.contains(&"task2".to_string()));
        assert!(ids.contains(&"task3".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_nonexistent_status() {
        let scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let status = scheduler.status("nonexistent").await;
        assert!(status.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trigger_unknown_workflow_errors() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("known", create_test_workflow(), TestState::Start)
                .unwrap();

            let mut scheduler_for_start = scheduler.clone();
            let scheduler_handle = tokio::spawn(async move { scheduler_for_start.start().await });
            sleep(Duration::from_millis(50)).await;

            let err = scheduler
                .trigger("missing")
                .await
                .expect_err("unknown workflow id must error");
            assert!(
                err.to_string().contains("No workflow registered"),
                "expected unknown workflow error, got: {err}"
            );

            scheduler.stop().await.unwrap();
            scheduler_handle.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trigger_via_stale_clone_reaches_running_registry() {
        // Regression: a clone of `Scheduler` snapshots the workflows HashMap,
        // so a workflow registered after cloning is invisible to the clone.
        // The trigger path must still succeed because the running scheduler
        // (started via `start()`) holds the authoritative registry and the
        // command channel is shared through the inner `Arc<RwLock<...>>`.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut running_scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();

            // Take the trigger handle BEFORE registering the workflow — its
            // local `workflows` HashMap will not contain "late_task".
            let stale_handle = running_scheduler.clone();

            running_scheduler
                .manual("late_task", create_test_workflow(), TestState::Start)
                .unwrap();
            assert!(!stale_handle.workflows.contains_key("late_task"));

            let mut scheduler_for_start = running_scheduler.clone();
            let scheduler_handle = tokio::spawn(async move { scheduler_for_start.start().await });
            sleep(Duration::from_millis(50)).await;

            // Trigger via the stale clone. The running scheduler's loop sees
            // "late_task" in its registry and accepts the trigger.
            stale_handle
                .trigger("late_task")
                .await
                .expect("stale clone trigger must reach the running registry");
            sleep(Duration::from_millis(50)).await;

            let status = running_scheduler.status("late_task").await.unwrap();
            assert_eq!(status.run_count, 1);

            running_scheduler.stop().await.unwrap();
            scheduler_handle.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_manual_trigger_rejects_overlap() {
        #[derive(Clone)]
        struct SlowNode;

        #[node]
        impl Node<TestState> for SlowNode {
            type PrepResult = ();
            type ExecResult = ();

            async fn prep(&self, _res: &Resources) -> CanoResult<()> {
                Ok(())
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                sleep(Duration::from_millis(300)).await;
            }

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> CanoResult<TestState> {
                Ok(TestState::Complete)
            }
        }

        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = Workflow::bare()
                .register(TestState::Start, SlowNode)
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);
            scheduler
                .manual("slow", workflow, TestState::Start)
                .unwrap();

            let mut scheduler_for_start = scheduler.clone();
            let scheduler_handle = tokio::spawn(async move { scheduler_for_start.start().await });
            sleep(Duration::from_millis(50)).await;

            scheduler.trigger("slow").await.unwrap();
            let err = scheduler
                .trigger("slow")
                .await
                .expect_err("overlapping manual trigger must be rejected");
            assert!(
                err.to_string().contains("already running"),
                "expected overlap error, got: {err}"
            );

            let status = scheduler.status("slow").await.unwrap();
            assert_eq!(status.run_count, 1);

            scheduler.stop().await.unwrap();
            scheduler_handle.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trigger_reaps_finished_handles() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("manual_task", create_test_workflow(), TestState::Start)
                .unwrap();

            let mut scheduler_for_start = scheduler.clone();
            let scheduler_handle = tokio::spawn(async move { scheduler_for_start.start().await });

            // Let scheduler boot. Manual schedules push no loop handles.
            sleep(Duration::from_millis(50)).await;
            assert_eq!(scheduler.scheduler_tasks.read().await.len(), 0);

            // Fire many triggers sequentially, letting each complete before the next.
            // Without reaping, scheduler_tasks would grow to 20 entries.
            for _ in 0..20 {
                scheduler.trigger("manual_task").await.unwrap();
                sleep(Duration::from_millis(30)).await;
            }

            // After reaping, only the most recent (finished) handle can still sit in the vec.
            let in_flight = scheduler.scheduler_tasks.read().await.len();
            assert!(
                in_flight <= 1,
                "expected reaping to bound in-flight handles to <=1, got {in_flight}"
            );

            // Sanity: workflow actually ran all 20 times.
            let status = scheduler.status("manual_task").await.unwrap();
            assert_eq!(status.run_count, 20);

            scheduler.stop().await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(1), scheduler_handle).await;
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stop_and_trigger_after_shutdown_report_not_running() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("manual_task", create_test_workflow(), TestState::Start)
                .unwrap();

            // Before start(), both calls must report "not running".
            let err = scheduler.stop().await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running error before start, got: {err}"
            );
            let err = scheduler.trigger("manual_task").await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running error before start, got: {err}"
            );

            // Start, then stop, then wait for start() to actually return.
            let mut scheduler_for_start = scheduler.clone();
            let scheduler_handle = tokio::spawn(async move { scheduler_for_start.start().await });
            sleep(Duration::from_millis(50)).await;
            scheduler.stop().await.unwrap();
            scheduler_handle.await.unwrap().unwrap();

            // After start() returned, the API contract must hold again:
            // subsequent stop()/trigger() must report "not running" (not a
            // channel-closed error leak).
            let err = scheduler.stop().await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running error after shutdown, got: {err}"
            );
            let err = scheduler.trigger("manual_task").await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running error after shutdown, got: {err}"
            );

            // And a clean restart must work.
            let mut scheduler_for_restart = scheduler.clone();
            let restart_handle = tokio::spawn(async move { scheduler_for_restart.start().await });
            sleep(Duration::from_millis(50)).await;
            scheduler.trigger("manual_task").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            scheduler.stop().await.unwrap();
            restart_handle.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stop_clears_sender_during_graceful_shutdown_window() {
        // Regression: while start() drains the command loop and waits on
        // in-flight workflows, a concurrent stop()/trigger() must report
        // "not running" instead of enqueueing into a queue nobody drains.
        #[derive(Clone)]
        struct SlowNode;

        #[node]
        impl Node<TestState> for SlowNode {
            type PrepResult = ();
            type ExecResult = ();

            async fn prep(&self, _res: &Resources) -> CanoResult<()> {
                Ok(())
            }

            async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
                // Hold Status::Running long enough to span the shutdown window.
                sleep(Duration::from_millis(400)).await;
            }

            async fn post(
                &self,
                _res: &Resources,
                _exec_res: Self::ExecResult,
            ) -> CanoResult<TestState> {
                Ok(TestState::Complete)
            }
        }

        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let slow_workflow = Workflow::bare()
                .register(TestState::Start, SlowNode)
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);
            scheduler
                .manual("slow_task", slow_workflow, TestState::Start)
                .unwrap();

            let mut scheduler_for_start = scheduler.clone();
            let scheduler_handle = tokio::spawn(async move { scheduler_for_start.start().await });

            // Let start() spin up.
            sleep(Duration::from_millis(50)).await;

            // Kick off the slow workflow and wait until it is actually Running
            // so has_running_flows() will keep start() parked after Stop.
            scheduler.trigger("slow_task").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            assert!(
                scheduler.has_running_flows().await,
                "slow workflow should be in Running state before we call stop()"
            );

            // First stop(): succeeds, Stop lands in the channel.
            scheduler.stop().await.unwrap();

            // Let start() dequeue Stop and clear command_tx. The slow workflow
            // is still in exec (~400ms total), so start() is parked inside
            // has_running_flows() — the shutdown window we want to probe.
            sleep(Duration::from_millis(50)).await;
            assert!(
                !scheduler_handle.is_finished(),
                "start() must still be parked waiting for the slow workflow"
            );
            assert!(
                scheduler.has_running_flows().await,
                "slow workflow must still be running during the shutdown window"
            );

            // During the window, stop()/trigger() must report not-running
            // instead of filling a queue that will never be drained.
            let err = scheduler.stop().await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running during shutdown window, got: {err}"
            );
            let err = scheduler.trigger("slow_task").await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running during shutdown window, got: {err}"
            );

            // Finally, start() completes cleanly once the slow workflow finishes.
            scheduler_handle.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_failed_workflow() {
        let timeout = Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            // Create a simple test that validates failed workflow handling
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = create_failing_workflow();
            scheduler
                .manual("failing_task", workflow, TestState::Start)
                .unwrap();

            // Verify the workflow was added
            let status = scheduler.status("failing_task").await;
            assert!(status.is_some());

            let status = status.unwrap();
            assert_eq!(status.id, "failing_task");
            assert_eq!(status.status, Status::Idle);
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    // ----- Backoff / trip / reset tests -----

    /// Node that fails until its internal counter reaches `succeed_after`.
    /// Used to drive deterministic streak behavior in the tests below.
    #[derive(Clone)]
    struct FlakyNode {
        attempts: Arc<AtomicU32>,
        succeed_after: u32,
    }

    impl FlakyNode {
        fn always_failing() -> Self {
            Self {
                attempts: Arc::new(AtomicU32::new(0)),
                succeed_after: u32::MAX,
            }
        }

        fn succeed_on_attempt(n: u32) -> Self {
            Self {
                attempts: Arc::new(AtomicU32::new(0)),
                succeed_after: n,
            }
        }
    }

    #[node]
    impl Node<TestState> for FlakyNode {
        type PrepResult = ();
        type ExecResult = bool;

        // Disable retries so each scheduler dispatch is a single attempt.
        // The scheduler's BackoffPolicy is the only retry layer under test.
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::minimal()
        }

        async fn prep(&self, _res: &Resources) -> CanoResult<()> {
            Ok(())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            attempt >= self.succeed_after
        }

        async fn post(&self, _res: &Resources, ok: Self::ExecResult) -> CanoResult<TestState> {
            if ok {
                Ok(TestState::Complete)
            } else {
                Err(CanoError::NodeExecution("flaky".to_string()))
            }
        }
    }

    fn flaky_workflow(node: FlakyNode) -> Workflow<TestState> {
        Workflow::bare()
            .register(TestState::Start, node)
            .add_exit_state(TestState::Complete)
            .add_exit_state(TestState::Error)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_backoff_progression_extends_interval() {
        // Base interval (50ms) is small; backoff should grow above it. With
        // streak_limit None the loop keeps re-firing through the policy.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .every(
                    "flaky",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(50),
                )
                .unwrap();
            scheduler
                .set_backoff(
                    "flaky",
                    BackoffPolicy {
                        initial: Duration::from_millis(150),
                        multiplier: 2.0,
                        max_delay: Duration::from_millis(600),
                        jitter: 0.0,
                        streak_limit: None,
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            // Wait long enough for: first immediate run (~0), then
            // wait max(50, 150) = 150ms for second run, then 300ms, then 600ms (capped).
            // Budget ~1.3s for >=4 runs.
            sleep(Duration::from_millis(1300)).await;
            let runs_observed = scheduler.status("flaky").await.unwrap().run_count;
            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();

            // Without backoff a 50ms interval would yield ~25 runs in 1.3s.
            // With the policy above the gaps are 0,150,300,600 → ~4-5 runs.
            assert!(
                (3..=8).contains(&runs_observed),
                "expected backoff to throttle to ~3-8 runs in 1.3s, got {runs_observed}"
            );
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trip_on_streak_limit_stops_dispatch() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .every(
                    "trippy",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(40),
                )
                .unwrap();
            scheduler
                .set_backoff(
                    "trippy",
                    BackoffPolicy {
                        initial: Duration::from_millis(20),
                        multiplier: 1.0,
                        max_delay: Duration::from_millis(40),
                        jitter: 0.0,
                        streak_limit: Some(3),
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            // ~3 failures then trip. Budget includes the small backoff windows.
            sleep(Duration::from_millis(500)).await;

            let snap_before = scheduler.status("trippy").await.unwrap();
            assert!(
                matches!(snap_before.status, Status::Tripped { streak: 3, .. }),
                "expected Tripped(streak=3), got: {:?}",
                snap_before.status
            );
            assert_eq!(snap_before.run_count, 3);

            // Wait long enough for several base intervals — run_count must stay 3.
            sleep(Duration::from_millis(300)).await;
            let snap_after = scheduler.status("trippy").await.unwrap();
            assert_eq!(
                snap_after.run_count, 3,
                "tripped flow must stop dispatching"
            );

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reset_flow_clears_trip_and_resumes() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            // Succeeds on attempt 5, after 4 failures. With streak_limit=2 it
            // trips on the 2nd failure; a reset clears state and the 3rd run
            // (the "first attempt after reset") is again a failure, but with
            // streak_limit=10 (we'll bump the policy via re-registration trick
            // — instead we just verify the reset clears the trip and advances).
            let workflow = flaky_workflow(FlakyNode::succeed_on_attempt(5));
            scheduler
                .every(
                    "reset_me",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(40),
                )
                .unwrap();
            scheduler
                .set_backoff(
                    "reset_me",
                    BackoffPolicy {
                        initial: Duration::from_millis(20),
                        multiplier: 1.0,
                        max_delay: Duration::from_millis(20),
                        jitter: 0.0,
                        streak_limit: Some(2),
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            // Wait for trip.
            sleep(Duration::from_millis(300)).await;
            let snap = scheduler.status("reset_me").await.unwrap();
            assert!(
                matches!(snap.status, Status::Tripped { .. }),
                "expected trip, got: {:?}",
                snap.status
            );
            assert_eq!(snap.run_count, 2);

            // Reset — flow resumes, hits another failure, trips again, then we
            // reset and let it succeed.
            scheduler.reset_flow("reset_me").await.unwrap();
            let snap_after_reset = scheduler.status("reset_me").await.unwrap();
            assert_eq!(snap_after_reset.failure_streak, 0);
            assert!(snap_after_reset.next_eligible.is_none());

            // Loop trips again (failures 3,4 → trip). Reset once more.
            sleep(Duration::from_millis(200)).await;
            scheduler.reset_flow("reset_me").await.unwrap();

            // Attempt 5 succeeds → status Completed, streak 0.
            sleep(Duration::from_millis(200)).await;
            let snap_done = scheduler.status("reset_me").await.unwrap();
            assert_eq!(snap_done.status, Status::Completed);
            assert_eq!(snap_done.failure_streak, 0);
            assert!(snap_done.next_eligible.is_none());
            assert!(snap_done.run_count >= 5);

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_streak_resets_on_success() {
        // Fail twice, succeed once: streak should reset to 0 mid-run.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::succeed_on_attempt(3));
            scheduler
                .every(
                    "recover",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(30),
                )
                .unwrap();
            scheduler
                .set_backoff(
                    "recover",
                    BackoffPolicy {
                        initial: Duration::from_millis(30),
                        multiplier: 1.0,
                        max_delay: Duration::from_millis(30),
                        jitter: 0.0,
                        streak_limit: None,
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            sleep(Duration::from_millis(400)).await;
            let snap = scheduler.status("recover").await.unwrap();
            assert_eq!(
                snap.failure_streak, 0,
                "streak must reset on success, got {snap:?}"
            );
            assert_eq!(snap.status, Status::Completed);

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_manual_trigger_blocked_when_tripped() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .manual("manual_flaky", workflow, TestState::Start)
                .unwrap();
            scheduler
                .set_backoff(
                    "manual_flaky",
                    BackoffPolicy {
                        initial: Duration::from_millis(10),
                        multiplier: 1.0,
                        max_delay: Duration::from_millis(10),
                        jitter: 0.0,
                        streak_limit: Some(2),
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });
            sleep(Duration::from_millis(50)).await;

            // Two failed manual runs trip the flow.
            scheduler.trigger("manual_flaky").await.unwrap();
            sleep(Duration::from_millis(40)).await;
            scheduler.trigger("manual_flaky").await.unwrap();
            sleep(Duration::from_millis(40)).await;

            let snap = scheduler.status("manual_flaky").await.unwrap();
            assert!(
                matches!(snap.status, Status::Tripped { .. }),
                "expected trip, got: {:?}",
                snap.status
            );

            // Subsequent trigger must be rejected with the documented error.
            let err = scheduler
                .trigger("manual_flaky")
                .await
                .expect_err("trigger on tripped flow must fail");
            assert!(
                err.to_string().contains("tripped"),
                "expected tripped error, got: {err}"
            );

            // Reset and trigger again — accepted.
            scheduler.reset_flow("manual_flaky").await.unwrap();
            scheduler
                .trigger("manual_flaky")
                .await
                .expect("trigger after reset must succeed");

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reset_flow_unknown_id_errors() {
        let timeout = Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("known", create_test_workflow(), TestState::Start)
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });
            sleep(Duration::from_millis(50)).await;

            let err = scheduler.reset_flow("missing").await.unwrap_err();
            assert!(
                err.to_string().contains("No workflow registered"),
                "expected unknown error, got: {err}"
            );

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reset_flow_before_start_errors() {
        let scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let err = scheduler.reset_flow("anything").await.unwrap_err();
        assert!(
            err.to_string().contains("Scheduler not running"),
            "expected not-running error before start, got: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_set_backoff_unknown_id_errors() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let err = scheduler
            .set_backoff("missing", BackoffPolicy::default())
            .unwrap_err();
        assert!(
            matches!(err, CanoError::Configuration(_)),
            "expected Configuration error, got: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stop_interrupts_backoff_sleep() {
        // Failing flow with a long backoff window; stop() must wake the sleep
        // immediately, not wait for the backoff to elapse.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .every(
                    "long_backoff",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(20),
                )
                .unwrap();
            scheduler
                .set_backoff(
                    "long_backoff",
                    BackoffPolicy {
                        // First failure → 60s backoff. Without notify wakeup,
                        // stop() would block for the duration.
                        initial: Duration::from_secs(60),
                        multiplier: 1.0,
                        max_delay: Duration::from_secs(60),
                        jitter: 0.0,
                        streak_limit: None,
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });
            // Wait for first run + status update so the loop is parked on
            // the backoff window.
            sleep(Duration::from_millis(150)).await;
            assert!(matches!(
                scheduler.status("long_backoff").await.unwrap().status,
                Status::Backoff { .. }
            ));

            let stop_started = tokio::time::Instant::now();
            scheduler.stop().await.unwrap();
            let elapsed = stop_started.elapsed();
            task.await.unwrap().unwrap();

            assert!(
                elapsed < Duration::from_secs(2),
                "stop took too long during backoff: {:?}",
                elapsed
            );
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_no_policy_preserves_failed_status() {
        // A flow registered without `set_backoff` keeps the legacy semantics:
        // failures land as Status::Failed and re-fire on the base interval.
        let timeout = Duration::from_secs(3);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .every(
                    "legacy",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(40),
                )
                .unwrap();
            // Intentionally no set_backoff call.

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            // Poll until we observe a non-Running status with run_count ≥ 3.
            // The loop alternates Running ↔ Failed quickly, so a single sleep
            // can land on either side. Polling makes the test robust without
            // hiding bugs (a stuck status never escapes the inner timeout).
            let snap = loop {
                sleep(Duration::from_millis(50)).await;
                let s = scheduler.status("legacy").await.unwrap();
                if !matches!(s.status, Status::Running) && s.run_count >= 3 {
                    break s;
                }
            };

            assert!(
                matches!(snap.status, Status::Failed(_)),
                "no-policy flow must report Failed, got: {:?}",
                snap.status
            );
            assert_eq!(snap.failure_streak, 0, "no streak tracking without policy");
            assert!(snap.next_eligible.is_none());

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_set_backoff_after_start_errors() {
        // Documented contract: registry mutations after start() are silently
        // dropped because the spawned loops snapshot policy at start time.
        // The `started` guard turns that into an explicit Configuration error.
        let timeout = Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("flow", create_test_workflow(), TestState::Start)
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });
            sleep(Duration::from_millis(50)).await;

            let err = scheduler
                .clone()
                .set_backoff("flow", BackoffPolicy::with_trip(3))
                .unwrap_err();
            assert!(
                matches!(err, CanoError::Configuration(ref s) if s.contains("already started")),
                "expected Configuration error after start, got: {err}"
            );

            // Same guard applies to registration.
            let err = scheduler
                .clone()
                .manual("late", create_test_workflow(), TestState::Start)
                .unwrap_err();
            assert!(
                matches!(err, CanoError::Configuration(ref s) if s.contains("already started")),
                "expected Configuration error registering after start, got: {err}"
            );

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();

            // After stop() the flag flips back; new registrations are allowed.
            let mut scheduler = scheduler;
            assert!(
                scheduler
                    .set_backoff("flow", BackoffPolicy::default())
                    .is_ok()
            );
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trigger_overrides_backoff_window() {
        // A manual trigger bypasses an active Backoff window — that is the
        // documented operator-override behavior. (Tripped is the only state
        // that blocks trigger; that case is covered by
        // test_manual_trigger_blocked_when_tripped.)
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            // Flaky node that fails first then succeeds, so the manual
            // override can be observed advancing run_count past the failure.
            let workflow = flaky_workflow(FlakyNode::succeed_on_attempt(2));
            scheduler
                .manual("flow", workflow, TestState::Start)
                .unwrap();
            scheduler
                .set_backoff(
                    "flow",
                    BackoffPolicy {
                        // 60s park window — well outside this test's runtime.
                        initial: Duration::from_secs(60),
                        multiplier: 1.0,
                        max_delay: Duration::from_secs(60),
                        jitter: 0.0,
                        streak_limit: None,
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });
            sleep(Duration::from_millis(50)).await;

            // Trigger #1 fails, parks the flow in Backoff.
            scheduler.trigger("flow").await.unwrap();
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = scheduler.status("flow").await.unwrap();
                if matches!(s.status, Status::Backoff { .. }) {
                    break s;
                }
            };
            assert_eq!(snap.run_count, 1);

            // Trigger #2 within the backoff window — must NOT be rejected.
            scheduler.trigger("flow").await.unwrap();

            // Wait for the override run to land and clear the window.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = scheduler.status("flow").await.unwrap();
                if s.run_count == 2 && !matches!(s.status, Status::Running) {
                    break s;
                }
            };
            assert!(
                matches!(snap.status, Status::Completed),
                "override run should succeed and clear backoff, got: {:?}",
                snap.status
            );
            assert_eq!(snap.failure_streak, 0);
            assert!(snap.next_eligible.is_none());

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trip_reset_trip_roundtrip_in_one_session() {
        // Trip → reset → trip again, all within a single start() session.
        // Documents that reset_flow is idempotent on the lifecycle and the
        // streak counter resumes from 0 after a successful reset.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .every(
                    "flow",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(20),
                )
                .unwrap();
            scheduler
                .set_backoff(
                    "flow",
                    BackoffPolicy {
                        initial: Duration::from_millis(20),
                        multiplier: 1.0,
                        max_delay: Duration::from_millis(20),
                        jitter: 0.0,
                        streak_limit: Some(2),
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            // Wait for the first trip.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = scheduler.status("flow").await.unwrap();
                if matches!(s.status, Status::Tripped { .. }) {
                    break s;
                }
            };
            let runs_at_first_trip = snap.run_count;
            assert_eq!(snap.failure_streak, 2, "tripped at streak limit");

            // Reset and confirm streak is cleared.
            scheduler.reset_flow("flow").await.unwrap();
            let snap = scheduler.status("flow").await.unwrap();
            assert_eq!(snap.failure_streak, 0);
            assert!(matches!(
                snap.status,
                Status::Idle | Status::Backoff { .. } | Status::Running
            ));

            // Wait for the second trip.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = scheduler.status("flow").await.unwrap();
                if matches!(s.status, Status::Tripped { .. }) && s.run_count > runs_at_first_trip {
                    break s;
                }
            };
            assert_eq!(snap.failure_streak, 2, "second trip also at streak limit");
            assert!(snap.run_count >= runs_at_first_trip + 2);

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cron_tick_suppressed_in_backoff_window() {
        // A failing cron flow that ticks every second. After the first failure
        // the backoff window pushes next_eligible 60s into the future; cron
        // ticks during that window must be suppressed so run_count does not
        // advance on the base schedule.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .cron("flow", workflow, TestState::Start, "* * * * * *")
                .unwrap();
            scheduler
                .set_backoff(
                    "flow",
                    BackoffPolicy {
                        initial: Duration::from_secs(60),
                        multiplier: 1.0,
                        max_delay: Duration::from_secs(60),
                        jitter: 0.0,
                        streak_limit: None,
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            // Wait for the first run + backoff status.
            let snap = loop {
                sleep(Duration::from_millis(50)).await;
                let s = scheduler.status("flow").await.unwrap();
                if matches!(s.status, Status::Backoff { .. }) {
                    break s;
                }
            };
            assert_eq!(snap.run_count, 1);

            // Sit through ~3 cron ticks (3s) and verify run_count holds steady.
            sleep(Duration::from_millis(3200)).await;
            let snap = scheduler.status("flow").await.unwrap();
            assert_eq!(
                snap.run_count, 1,
                "cron ticks must not dispatch inside backoff window"
            );
            assert!(matches!(snap.status, Status::Backoff { .. }));

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reset_flow_during_inflight_run_documents_race() {
        // Documents the reset_flow race: if reset is called while a run is
        // in flight and that run subsequently fails, apply_outcome will
        // increment the freshly-cleared streak back to 1 rather than 0.
        // The reset is *not* lost (streak < streak_limit), but it is partly
        // overwritten — operators should observe Tripped/Idle before resetting.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyNode::always_failing());
            scheduler
                .manual("flow", workflow, TestState::Start)
                .unwrap();
            scheduler
                .set_backoff(
                    "flow",
                    BackoffPolicy {
                        initial: Duration::from_millis(20),
                        multiplier: 1.0,
                        max_delay: Duration::from_millis(20),
                        jitter: 0.0,
                        streak_limit: None,
                    },
                )
                .unwrap();

            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });
            sleep(Duration::from_millis(50)).await;

            // Drive streak up to 3, then reset and trigger one more failure.
            for _ in 0..3 {
                scheduler.trigger("flow").await.unwrap();
                loop {
                    sleep(Duration::from_millis(15)).await;
                    let s = scheduler.status("flow").await.unwrap();
                    if matches!(s.status, Status::Backoff { .. }) {
                        break;
                    }
                }
            }
            assert_eq!(scheduler.status("flow").await.unwrap().failure_streak, 3);

            scheduler.reset_flow("flow").await.unwrap();
            assert_eq!(scheduler.status("flow").await.unwrap().failure_streak, 0);

            // One more failure after the reset puts streak at 1, not 4.
            scheduler.trigger("flow").await.unwrap();
            let snap = loop {
                sleep(Duration::from_millis(15)).await;
                let s = scheduler.status("flow").await.unwrap();
                if matches!(s.status, Status::Backoff { .. }) {
                    break s;
                }
            };
            assert_eq!(
                snap.failure_streak, 1,
                "reset clears streak — next failure starts a fresh streak"
            );

            scheduler.stop().await.unwrap();
            task.await.unwrap().unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_drop_aborts_loops_when_no_stop() {
        // Drop on the last clone of a started scheduler should abort the
        // spawned loop tasks instead of leaking them. We assert via the
        // node's own execution_count — once we drop the scheduler, it must
        // stop advancing.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let node = TestNode::new();
            let count = Arc::clone(&node.execution_count);
            let workflow = Workflow::bare()
                .register(TestState::Start, node)
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);

            let mut scheduler: Scheduler<TestState> = Scheduler::new();
            scheduler
                .every(
                    "ticker",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(20),
                )
                .unwrap();

            // Spawn start() owning a clone; the test holds the original.
            let mut start_handle = scheduler.clone();
            let task = tokio::spawn(async move { start_handle.start().await });

            // Let the loop tick a few times.
            sleep(Duration::from_millis(120)).await;
            let observed = count.load(Ordering::SeqCst);
            assert!(observed >= 2, "loop should have ticked at least twice, got {observed}");

            // Drop the user-facing scheduler clone, then drop the start_handle
            // by aborting the spawned start() task. After Drop, count must
            // freeze — no more ticks. Sample once before and once after a
            // short wait; any post-drop tick would diverge the two reads.
            drop(scheduler);
            task.abort();
            let _ = task.await;

            let final_count = count.load(Ordering::SeqCst);
            sleep(Duration::from_millis(120)).await;
            let later_count = count.load(Ordering::SeqCst);
            assert_eq!(
                final_count, later_count,
                "spawned loops must stop after Scheduler is dropped (was {final_count}, became {later_count})"
            );
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }
}
