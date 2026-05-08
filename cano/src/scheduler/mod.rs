//! # Scheduler API
//!
//! Time-driven dispatch for [`Workflow`](crate::workflow::Workflow) instances —
//! intervals, cron expressions, and manual triggers, plus optional per-flow
//! backoff and trip semantics.
//!
//! Two-stage lifecycle:
//!
//! - [`Scheduler`] is the **builder**. Register workflows with [`every`](Scheduler::every) /
//!   [`cron`](Scheduler::cron) / [`manual`](Scheduler::manual), attach optional
//!   [`BackoffPolicy`] via [`set_backoff`](Scheduler::set_backoff), then call
//!   [`start`](Scheduler::start) to consume the builder. `Scheduler` is **not** `Clone`.
//! - [`RunningScheduler`] is the **live handle** returned by `start`. It owns the
//!   spawned driver and per-flow loop tasks. It is cheap to clone — call
//!   [`trigger`](RunningScheduler::trigger), [`status`](RunningScheduler::status),
//!   [`stop`](RunningScheduler::stop), etc. from any task.
//!
//! Because `start` consumes `self`, double-starting the same builder is rejected
//! at the type level.
//!
//! > **Note:** The scheduler is an optional feature. Enable the `"scheduler"`
//! > feature in your `Cargo.toml`.
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
use tokio::sync::{Notify, RwLock, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
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

/// Scheduler builder for registering workflows before launch.
///
/// `Scheduler` is the **builder** half of a two-stage API. Register every
/// workflow via [`every`](Self::every) / [`cron`](Self::cron) /
/// [`manual`](Self::manual), attach optional [`BackoffPolicy`] via
/// [`set_backoff`](Self::set_backoff), then call [`start`](Self::start) to
/// consume the builder and obtain a [`RunningScheduler`] handle. All control
/// methods (`stop`, `trigger`, `status`, …) live on the running half.
///
/// `Scheduler` deliberately does **not** implement `Clone`. The control plane
/// state (command channel, lifecycle flags, spawned task handles) lives on
/// `RunningScheduler`, which is `Clone`-able and can be shared across tasks
/// for status reads, manual triggers, and shutdown. Splitting these two
/// roles makes a double-`start` impossible at the type level — `start`
/// consumes `self`.
///
/// All workflows registered with a single `Scheduler` instance must share the
/// same `TState` and `TResourceKey` types. The resource key type defaults to
/// [`Cow<'static, str>`](std::borrow::Cow), which accepts both `&'static str`
/// literals (allocation-free) and owned `String` keys. If your application
/// requires workflows with different state enums, create a separate
/// `Scheduler` instance for each state type.
///
/// Requires the `scheduler` feature.
pub struct Scheduler<TState, TResourceKey = Cow<'static, str>>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    workflows: HashMap<String, FlowData<TState, TResourceKey>>,
    /// Registration order of workflow ids. Setup runs FIFO, teardown LIFO.
    /// Required because `HashMap` iteration order is undefined.
    flow_order: Vec<String>,
}

impl<TState, TResourceKey> Scheduler<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Create a new scheduler builder.
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
            flow_order: Vec::new(),
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

    /// Number of registered flows.
    pub fn len(&self) -> usize {
        self.flow_order.len()
    }

    /// True if no flows have been registered yet.
    pub fn is_empty(&self) -> bool {
        self.flow_order.is_empty()
    }

    /// True if a flow with this id has been registered.
    pub fn contains(&self, id: &str) -> bool {
        self.workflows.contains_key(id)
    }

    /// Attach a [`BackoffPolicy`] to an existing flow.
    ///
    /// After failure the scheduler will sleep `policy.compute_delay(streak)`
    /// before the next dispatch and trip the flow when `streak_limit` is hit.
    /// Without this call a flow keeps the legacy "fail and try again on the
    /// base schedule" behavior.
    ///
    /// `set_backoff` is a builder method — it can only be called before
    /// [`Scheduler::start`] consumes `self`, which is enforced at the type
    /// level.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Configuration`] — `id` is not registered.
    pub fn set_backoff(&mut self, id: &str, policy: BackoffPolicy) -> CanoResult<()> {
        let flow = self.workflows.get_mut(id).ok_or_else(|| {
            CanoError::Configuration(format!("Flow '{id}' not found — cannot set backoff"))
        })?;
        flow.policy = Some(Arc::new(policy));
        Ok(())
    }

    /// Consume the builder and start the scheduler.
    ///
    /// Validates every registered workflow, runs `setup_all` for each in
    /// registration order, then spawns the per-flow scheduling loops and the
    /// command-driver task. Returns a [`RunningScheduler`] handle that owns
    /// the live control plane: clone it freely to call `status` / `trigger` /
    /// `stop` from multiple tasks, or call [`RunningScheduler::wait`] to
    /// block until shutdown completes.
    ///
    /// On setup failure, already-setup workflows are torn down in reverse
    /// (LIFO) order before the error propagates. No tasks are spawned in
    /// that case — `start` is the only path that produces a
    /// `RunningScheduler`, so a failed start cannot leave runtime state
    /// behind.
    ///
    /// `start` consumes `self`, so it can only be called once per builder.
    /// This makes a double-start impossible at the type level. To run
    /// another scheduler with the same workflow set, build a fresh
    /// `Scheduler`.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Configuration`] — a registered workflow failed
    ///   `validate()` or `validate_initial_state()`.
    /// - Any resource setup error from a registered workflow.
    pub async fn start(self) -> CanoResult<RunningScheduler<TState, TResourceKey>> {
        let Self {
            workflows,
            flow_order,
        } = self;

        // Validate every registered workflow upfront so misconfigurations
        // surface before any resource setup runs (matches
        // `Workflow::orchestrate`'s contract). Iterating `flow_order`
        // guarantees deterministic FIFO setup / LIFO teardown.
        let ordered: Vec<&FlowData<TState, TResourceKey>> = flow_order
            .iter()
            .filter_map(|id| workflows.get(id))
            .collect();
        for flow in ordered.iter() {
            flow.workflow
                .validate()
                .and_then(|_| flow.workflow.validate_initial_state(&flow.initial_state))?;
        }
        // Setup runs in registration order; on failure, the previously-setup
        // workflows are torn down in reverse registration order.
        for (idx, flow) in ordered.iter().enumerate() {
            if let Err(e) = flow.workflow.resources.setup_all().await {
                for prior in ordered[..idx].iter().rev() {
                    let len = prior.workflow.resources.lifecycle_len();
                    prior.workflow.resources.teardown_range(0..len).await;
                }
                return Err(e);
            }
        }
        drop(ordered);

        // Capacity of 64: Stop is sent once, each Trigger / Reset enqueues one
        // item. try_send in trigger() / reset_flow() returns an error if this
        // fills up rather than growing without bound.
        let (command_tx, command_rx) = mpsc::channel::<SchedulerCommand>(64);

        // Loop tasks clone the Notify and race the timer against `notified()`
        // so `stop()` interrupts long sleeps immediately instead of waiting
        // for the cron / interval to fire.
        let stop_notify = Arc::new(Notify::new());
        let running = Arc::new(RwLock::new(true));
        let scheduler_tasks: Arc<RwLock<Vec<JoinHandle<()>>>> = Arc::new(RwLock::new(Vec::new()));

        // Build a read-only flow-info registry for status / list / has_running_flows.
        let mut flows_view: HashMap<String, Arc<RwLock<FlowInfo>>> =
            HashMap::with_capacity(workflows.len());
        for (id, fd) in &workflows {
            flows_view.insert(id.clone(), Arc::clone(&fd.info));
        }
        let flows_view = Arc::new(flows_view);
        let flow_order_view = Arc::new(flow_order.clone());

        // Spawn per-flow scheduling loops. Manual flows have no loop — they
        // run only on Trigger.
        {
            let mut tasks = scheduler_tasks.write().await;
            for id in flow_order.iter() {
                let Some(fd) = workflows.get(id) else {
                    continue;
                };
                let workflow = Arc::clone(&fd.workflow);
                let initial_state = fd.initial_state.clone();
                let info = Arc::clone(&fd.info);
                let policy = fd.policy.clone();
                let running_clone = Arc::clone(&running);
                let notify_clone = Arc::clone(&stop_notify);

                match &fd.schedule {
                    ParsedSchedule::Every(interval) => {
                        let interval = *interval;
                        let handle = tokio::spawn(spawn_every_loop(
                            workflow,
                            initial_state,
                            info,
                            policy,
                            running_clone,
                            notify_clone,
                            interval,
                        ));
                        tasks.push(handle);
                    }
                    ParsedSchedule::Cron(cron_schedule) => {
                        let cron_schedule = cron_schedule.clone();
                        let handle = tokio::spawn(spawn_cron_loop(
                            workflow,
                            initial_state,
                            info,
                            policy,
                            running_clone,
                            notify_clone,
                            cron_schedule,
                        ));
                        tasks.push(handle);
                    }
                    ParsedSchedule::Manual => {
                        // Manual workflows don't run on their own.
                    }
                }
            }
        }

        // Spawn the driver task. It owns the workflows HashMap (for Trigger /
        // Reset lookups and teardown), drains scheduler_tasks on Stop, and
        // emits the final result on the watch channel.
        let (result_tx, result_rx) = watch::channel::<Option<CanoResult<()>>>(None);
        let driver_handle = tokio::spawn(driver_task(
            command_rx,
            workflows,
            flow_order,
            Arc::clone(&running),
            Arc::clone(&stop_notify),
            Arc::clone(&scheduler_tasks),
            result_tx,
        ));

        Ok(RunningScheduler {
            command_tx,
            flows: flows_view,
            flow_order: flow_order_view,
            result_rx,
            scheduler_tasks,
            driver_handle: Arc::new(driver_handle),
            liveness: Arc::new(()),
            _marker: std::marker::PhantomData,
        })
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

/// Live handle to a started [`Scheduler`].
///
/// Returned by [`Scheduler::start`]. Cheap to clone — every clone shares the
/// same control plane (command channel, flow info registry, shutdown result
/// watch). Use clones across tasks to read status, manually trigger flows,
/// and signal shutdown from anywhere.
///
/// `stop` and `wait` are cooperative: `stop` enqueues a Stop command then
/// awaits the driver task; `wait` only awaits. After the driver task
/// terminates, both calls return the final shutdown result for every clone
/// (idempotent — calling `stop` after a successful shutdown returns the
/// cached result).
///
/// Dropping the last `RunningScheduler` clone without calling `stop` aborts
/// the driver and per-flow loop tasks as a fallback so spawned tasks are not
/// leaked. For predictable graceful shutdown, always call `stop().await`.
#[derive(Clone)]
pub struct RunningScheduler<TState, TResourceKey = Cow<'static, str>>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    command_tx: mpsc::Sender<SchedulerCommand>,
    /// Read-only flow info registry. Cheap clones (Arc) for `status` / `list`
    /// / `has_running_flows`. Built once at start time; never mutated.
    flows: Arc<HashMap<String, Arc<RwLock<FlowInfo>>>>,
    flow_order: Arc<Vec<String>>,
    /// Final shutdown result published by the driver task. Receivers loop
    /// `changed().await` until the value transitions to `Some(_)`.
    result_rx: watch::Receiver<Option<CanoResult<()>>>,
    /// Shared per-flow loop handles so `Drop` (last-clone fallback) and the
    /// driver task can reach them.
    scheduler_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
    /// JoinHandle of the driver task. Wrapped in Arc so multiple clones can
    /// observe `is_finished()` and the last-clone Drop can `abort()`.
    driver_handle: Arc<JoinHandle<()>>,
    /// Strong-count sentinel for the user-visible clone count. Ignores the
    /// Arc references held by the spawned driver task so the last user-clone
    /// drop reliably triggers fallback abort.
    ///
    /// Field is named (not `_liveness`) so clippy does not warn about an
    /// unused field; it is read via `Arc::strong_count` in `Drop`.
    liveness: Arc<()>,
    /// Generics are anchored on the driver task (which owns the workflows
    /// HashMap); the handle itself only sees `FlowInfo`. PhantomData keeps
    /// `TState` / `TResourceKey` in the type signature so a `RunningScheduler`
    /// for one workflow set isn't accidentally interchangeable with another.
    _marker: std::marker::PhantomData<fn() -> (TState, TResourceKey)>,
}

impl<TState, TResourceKey> RunningScheduler<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Enqueue a Stop command and wait for the driver task to finish
    /// graceful shutdown (drain loops, wait up to 30s for in-flight
    /// workflows, run resource teardown LIFO).
    ///
    /// Idempotent: a second call after successful shutdown returns the same
    /// cached result via [`wait`](Self::wait). Does not error if the driver
    /// has already exited — the existing result is returned.
    ///
    /// # Errors
    ///
    /// Whatever the driver task returned: typically
    /// [`CanoError::Workflow`] if the 30-second graceful-shutdown window
    /// elapsed with workflows still running.
    pub async fn stop(&self) -> CanoResult<()> {
        // Best-effort send. If the driver task has already exited, the
        // receiver is dropped and `send` errors — fall through to `wait`,
        // which returns the cached result.
        let _ = self.command_tx.send(SchedulerCommand::Stop).await;
        self.wait().await
    }

    /// Wait for the driver task to finish without sending Stop.
    ///
    /// Useful when shutdown is signalled from one task (e.g. a Ctrl+C
    /// handler calling `stop`) and another task wants to block on
    /// completion. Calling `wait` without ever calling `stop` blocks
    /// indefinitely — the scheduler only terminates on Stop or on last-clone
    /// Drop.
    ///
    /// # Errors
    ///
    /// Whatever the driver task returned. If the driver task panicked or was
    /// aborted before publishing a result, `wait` returns
    /// [`CanoError::Workflow`] noting unexpected termination.
    pub async fn wait(&self) -> CanoResult<()> {
        let mut rx = self.result_rx.clone();
        loop {
            if let Some(result) = rx.borrow().clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                // Sender dropped without publishing — driver task panicked
                // or was aborted. Surface this so silent panic doesn't read
                // as Ok.
                return Err(CanoError::Workflow(
                    "Scheduler driver task terminated unexpectedly without publishing a result"
                        .to_string(),
                ));
            }
        }
    }

    /// Manually trigger a workflow by ID.
    ///
    /// Sends a trigger command to the driver task; the workflow then
    /// executes asynchronously. This method returns once the driver
    /// accepts or rejects the trigger.
    ///
    /// Manual triggers **bypass** an active [`Status::Backoff`] window — the
    /// operator is presumed to know what they're doing. They are
    /// **rejected** when the flow is in [`Status::Tripped`]; call
    /// [`reset_flow`](Self::reset_flow) first to clear the trip.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running (the driver
    ///   task has already exited), `id` is unknown, the workflow is already
    ///   running, the flow is tripped, or the command queue is full.
    pub async fn trigger(&self, id: &str) -> CanoResult<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .try_send(SchedulerCommand::Trigger {
                id: id.to_string(),
                response: response_tx,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Closed(_) => CanoError::Workflow(
                    "Scheduler not running — call start() before trigger()".to_string(),
                ),
                mpsc::error::TrySendError::Full(_) => {
                    CanoError::Workflow("Scheduler command queue full".to_string())
                }
            })?;

        response_rx.await.map_err(|_| {
            CanoError::Workflow("Scheduler stopped before trigger was processed".to_string())
        })?
    }

    /// Reset a flow that hit its [`BackoffPolicy::streak_limit`] (or that
    /// you otherwise want to clear). Sends a Reset command to the driver
    /// task so the authoritative registry is updated.
    ///
    /// Clears `failure_streak`, `next_eligible`, and (when the flow is not
    /// currently running) sets `status = Idle`.
    ///
    /// # Race with in-flight executions
    ///
    /// `reset_flow` serializes against `trigger` via the scheduler's command
    /// channel, but the post-execution status write (`apply_outcome`) runs
    /// in the spawned execution task and is **not** routed through that
    /// channel. If a flow is mid-execution when `reset_flow` is called and
    /// the run subsequently fails, `apply_outcome` will increment the
    /// freshly-cleared streak back to `1` and (depending on the policy)
    /// re-park the flow in [`Status::Backoff`]. The reset is not lost — the
    /// streak is `1` rather than `streak_limit` — but operators should
    /// prefer to call `reset_flow` only after observing the flow in
    /// [`Status::Tripped`] or [`Status::Idle`] via
    /// [`status`](Self::status) to avoid the surprise.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running, `id` is
    ///   unknown, or the command queue is full.
    pub async fn reset_flow(&self, id: &str) -> CanoResult<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .try_send(SchedulerCommand::Reset {
                id: id.to_string(),
                response: response_tx,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Closed(_) => CanoError::Workflow(
                    "Scheduler not running — call start() before reset_flow()".to_string(),
                ),
                mpsc::error::TrySendError::Full(_) => {
                    CanoError::Workflow("Scheduler command queue full".to_string())
                }
            })?;

        response_rx.await.map_err(|_| {
            CanoError::Workflow("Scheduler stopped before reset was processed".to_string())
        })?
    }

    /// Get a snapshot of the workflow status.
    pub async fn status(&self, id: &str) -> Option<FlowInfo> {
        let info = self.flows.get(id)?;
        Some(info.read().await.clone())
    }

    /// List all workflows in registration order.
    pub async fn list(&self) -> Vec<FlowInfo> {
        let mut results = Vec::with_capacity(self.flow_order.len());
        for id in self.flow_order.iter() {
            if let Some(info) = self.flows.get(id) {
                results.push(info.read().await.clone());
            }
        }
        results
    }

    /// Returns true if any workflow is currently in [`Status::Running`].
    pub async fn has_running_flows(&self) -> bool {
        for info in self.flows.values() {
            if info.read().await.status == Status::Running {
                return true;
            }
        }
        false
    }
}

impl<TState, TResourceKey> std::fmt::Debug for RunningScheduler<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningScheduler")
            .field("flows", &self.flow_order)
            .field("driver_finished", &self.driver_handle.is_finished())
            .finish_non_exhaustive()
    }
}

/// Best-effort fallback if the user drops every [`RunningScheduler`] clone
/// without calling [`RunningScheduler::stop`]. Only the final clone (i.e.
/// when we are the last holder of the `liveness` Arc) aborts the spawned
/// driver and per-flow loop tasks; earlier clones leave them alive for the
/// remaining holders.
///
/// **Limitation:** the `Arc::strong_count == 1` gate is racy under
/// concurrent drops of the last two clones — both may observe count `> 1`
/// and skip the abort, leaking the spawned tasks. The fallback is identical
/// to having no `Drop` impl at all (the prior behavior), so this is
/// strictly an improvement; for guaranteed teardown call `stop()` explicitly.
impl<TState, TResourceKey> Drop for RunningScheduler<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if Arc::strong_count(&self.liveness) > 1 {
            return;
        }
        // Last clone. If the driver already finished (Stop / wait completed
        // or was aborted), do nothing.
        if self.driver_handle.is_finished() {
            return;
        }
        self.driver_handle.abort();
        if let Ok(handles) = self.scheduler_tasks.try_write() {
            let n = handles.len();
            for h in handles.iter() {
                h.abort();
            }
            #[cfg(feature = "tracing")]
            tracing::warn!(
                aborted = n,
                "RunningScheduler dropped without stop() — aborted spawned tasks"
            );
            #[cfg(not(feature = "tracing"))]
            let _ = n;
        }
    }
}

/// Per-flow `Every`-schedule loop body. Lives outside `start` so the driver
/// task and the loops are decoupled — the driver owns the workflows
/// HashMap, the loops just see the data they need.
async fn spawn_every_loop<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: Option<Arc<BackoffPolicy>>,
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
            policy.as_ref(),
        )
        .await;
    }

    loop {
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
            policy.as_ref(),
        )
        .await;
    }
}

/// Per-flow `Cron`-schedule loop body. See [`spawn_every_loop`] for the
/// rationale on splitting the loop bodies out of `start`.
async fn spawn_cron_loop<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
    policy: Option<Arc<BackoffPolicy>>,
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
            policy.as_ref(),
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
async fn driver_task<TState, TResourceKey>(
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
        assert!(scheduler.is_empty());
        assert_eq!(scheduler.len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_seconds() {
        let mut scheduler = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        scheduler
            .every_seconds("test_task", workflow, TestState::Start, 5)
            .unwrap();

        assert_eq!(scheduler.len(), 1);
        assert!(scheduler.contains("test_task"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_minutes() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        scheduler
            .every_minutes("test_task", workflow, TestState::Start, 2)
            .unwrap();

        assert!(scheduler.contains("test_task"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_hours() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        scheduler
            .every_hours("test_task", workflow, TestState::Start, 1)
            .unwrap();

        assert!(scheduler.contains("test_task"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_every_duration() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        scheduler
            .every(
                "test_task",
                workflow,
                TestState::Start,
                Duration::from_millis(100),
            )
            .unwrap();
        assert!(scheduler.contains("test_task"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_cron() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        scheduler
            .cron("test_task", workflow, TestState::Start, "0 */5 * * * *")
            .unwrap();
        assert!(scheduler.contains("test_task"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_cron_invalid() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let err = scheduler
            .cron("test_task", workflow, TestState::Start, "invalid cron")
            .unwrap_err();
        assert!(matches!(err, CanoError::Configuration(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_add_workflow_manual() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        scheduler
            .manual("test_task", workflow, TestState::Start)
            .unwrap();
        assert!(scheduler.contains("test_task"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_duplicate_workflow_id() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow1 = create_test_workflow();
        let workflow2 = create_test_workflow();

        scheduler
            .every_seconds("test_task", workflow1, TestState::Start, 5)
            .unwrap();

        let err = scheduler
            .every_seconds("test_task", workflow2, TestState::Start, 10)
            .unwrap_err();
        assert!(matches!(err, CanoError::Configuration(_)));
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

        let running = scheduler.start().await.expect("start should succeed");

        // Give the spawned tasks time to enter their sleep.
        sleep(Duration::from_millis(100)).await;

        let stop_started = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(5), running.stop())
            .await
            .expect("stop should return shortly")
            .expect("stop should not error");
        let elapsed = stop_started.elapsed();

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

        let running = scheduler.start().await.expect("start should succeed");
        sleep(Duration::from_millis(100)).await;

        let stop_started = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(5), running.stop())
            .await
            .expect("stop should return shortly")
            .expect("stop should not error");
        let elapsed = stop_started.elapsed();

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

            let running = scheduler.start().await.unwrap();

            // Trigger the workflow and wait for execution.
            running.trigger("test_task").await.unwrap();
            sleep(Duration::from_millis(100)).await;

            let status = running.status("test_task").await;
            assert!(status.is_some());

            running.stop().await.unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_handle_clones_share_state() {
        // RunningScheduler is cheap-to-clone — every clone reaches the same
        // command channel and flow registry. Verify that triggering on one
        // clone is observable via status() on another.
        let timeout = Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("shared", create_test_workflow(), TestState::Start)
                .unwrap();

            let running = scheduler.start().await.unwrap();
            let other = running.clone();

            other.trigger("shared").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            assert_eq!(running.status("shared").await.unwrap().run_count, 1);

            running.stop().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_status_check_post_start() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler
            .manual("test_task", workflow, TestState::Start)
            .unwrap();

        let running = scheduler.start().await.unwrap();
        let status = running.status("test_task").await.expect("must exist");

        assert_eq!(status.id, "test_task");
        assert_eq!(status.status, Status::Idle);
        assert_eq!(status.run_count, 0);
        assert!(status.last_run.is_none());

        running.stop().await.unwrap();
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

        assert_eq!(scheduler.len(), 3);
        assert!(scheduler.contains("task1"));
        assert!(scheduler.contains("task2"));
        assert!(scheduler.contains("task3"));

        let running = scheduler.start().await.unwrap();
        let flows = running.list().await;
        assert_eq!(flows.len(), 3);
        let ids: Vec<String> = flows.iter().map(|f| f.id.clone()).collect();
        assert!(ids.contains(&"task1".to_string()));
        assert!(ids.contains(&"task2".to_string()));
        assert!(ids.contains(&"task3".to_string()));

        running.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_nonexistent_status() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        scheduler
            .manual("known", create_test_workflow(), TestState::Start)
            .unwrap();
        let running = scheduler.start().await.unwrap();
        assert!(running.status("nonexistent").await.is_none());
        running.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trigger_unknown_workflow_errors() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("known", create_test_workflow(), TestState::Start)
                .unwrap();

            let running = scheduler.start().await.unwrap();

            let err = running
                .trigger("missing")
                .await
                .expect_err("unknown workflow id must error");
            assert!(
                err.to_string().contains("No workflow registered"),
                "expected unknown workflow error, got: {err}"
            );

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            running.trigger("slow").await.unwrap();
            let err = running
                .trigger("slow")
                .await
                .expect_err("overlapping manual trigger must be rejected");
            assert!(
                err.to_string().contains("already running"),
                "expected overlap error, got: {err}"
            );

            let status = running.status("slow").await.unwrap();
            assert_eq!(status.run_count, 1);

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            // Manual schedules push no loop handles.
            assert_eq!(running.scheduler_tasks.read().await.len(), 0);

            // Fire many triggers sequentially, letting each complete before the next.
            // Without reaping, scheduler_tasks would grow to 20 entries.
            for _ in 0..20 {
                running.trigger("manual_task").await.unwrap();
                sleep(Duration::from_millis(30)).await;
            }

            // After reaping, only the most recent (finished) handle can still sit in the vec.
            let in_flight = running.scheduler_tasks.read().await.len();
            assert!(
                in_flight <= 1,
                "expected reaping to bound in-flight handles to <=1, got {in_flight}"
            );

            // Sanity: workflow actually ran all 20 times.
            let status = running.status("manual_task").await.unwrap();
            assert_eq!(status.run_count, 20);

            running.stop().await.unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_post_shutdown_calls_report_not_running() {
        // After stop() returns, a second stop() is idempotent (returns the cached
        // result) and trigger()/reset_flow() report "Scheduler not running".
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("manual_task", create_test_workflow(), TestState::Start)
                .unwrap();

            let running = scheduler.start().await.unwrap();
            running.stop().await.unwrap();

            // Idempotent stop().
            running.stop().await.unwrap();

            // Trigger / reset must surface "Scheduler not running".
            let err = running.trigger("manual_task").await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running error after shutdown, got: {err}"
            );
            let err = running.reset_flow("manual_task").await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running error after shutdown, got: {err}"
            );
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trigger_during_graceful_shutdown_window_reports_not_running() {
        // While the driver task is parked waiting for a slow in-flight workflow
        // to finish, a concurrent trigger() must surface "not running" instead
        // of enqueueing into the closed command channel.
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

            let running = scheduler.start().await.unwrap();
            let probe = running.clone();

            // Kick off the slow workflow and wait until it is actually Running.
            probe.trigger("slow_task").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            assert!(
                probe.has_running_flows().await,
                "slow workflow should be Running before stop()"
            );

            // Spawn stop() so we can probe the shutdown window concurrently.
            let stop_handle = tokio::spawn(async move { running.stop().await });

            // Let the driver dequeue Stop and close the command channel. The
            // slow workflow is still running (~400ms total), so the driver is
            // parked inside has_running_flows() — the shutdown window we want
            // to probe.
            sleep(Duration::from_millis(50)).await;
            assert!(
                !stop_handle.is_finished(),
                "stop() must still be parked while the slow workflow is in flight"
            );

            // During the window, trigger() must report not-running.
            let err = probe.trigger("slow_task").await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "expected not-running during shutdown window, got: {err}"
            );

            // stop() eventually returns Ok (teardown finishes).
            let stop_result = stop_handle.await.expect("stop task should not panic");
            stop_result.expect("stop should succeed once slow workflow finishes");
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_failed_workflow_registration() {
        // Registering a "failing" workflow (one whose post() returns Err) is a
        // build-time concern; it is registered exactly like any other.
        let timeout = Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = create_failing_workflow();
            scheduler
                .manual("failing_task", workflow, TestState::Start)
                .unwrap();
            assert!(scheduler.contains("failing_task"));
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

            let running = scheduler.start().await.unwrap();

            // Wait long enough for: first immediate run (~0), then
            // wait max(50, 150) = 150ms for second run, then 300ms, then 600ms (capped).
            // Budget ~1.3s for >=4 runs.
            sleep(Duration::from_millis(1300)).await;
            let runs_observed = running.status("flaky").await.unwrap().run_count;
            running.stop().await.unwrap();

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

            let running = scheduler.start().await.unwrap();

            // ~3 failures then trip. Budget includes the small backoff windows.
            sleep(Duration::from_millis(500)).await;

            let snap_before = running.status("trippy").await.unwrap();
            assert!(
                matches!(snap_before.status, Status::Tripped { streak: 3, .. }),
                "expected Tripped(streak=3), got: {:?}",
                snap_before.status
            );
            assert_eq!(snap_before.run_count, 3);

            // Wait long enough for several base intervals — run_count must stay 3.
            sleep(Duration::from_millis(300)).await;
            let snap_after = running.status("trippy").await.unwrap();
            assert_eq!(
                snap_after.run_count, 3,
                "tripped flow must stop dispatching"
            );

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            // Wait for trip.
            sleep(Duration::from_millis(300)).await;
            let snap = running.status("reset_me").await.unwrap();
            assert!(
                matches!(snap.status, Status::Tripped { .. }),
                "expected trip, got: {:?}",
                snap.status
            );
            assert_eq!(snap.run_count, 2);

            // Reset — flow resumes, hits another failure, trips again, then we
            // reset and let it succeed.
            running.reset_flow("reset_me").await.unwrap();
            let snap_after_reset = running.status("reset_me").await.unwrap();
            assert_eq!(snap_after_reset.failure_streak, 0);
            assert!(snap_after_reset.next_eligible.is_none());

            // Loop trips again (failures 3,4 → trip). Reset once more.
            sleep(Duration::from_millis(200)).await;
            running.reset_flow("reset_me").await.unwrap();

            // Attempt 5 succeeds → status Completed, streak 0.
            sleep(Duration::from_millis(200)).await;
            let snap_done = running.status("reset_me").await.unwrap();
            assert_eq!(snap_done.status, Status::Completed);
            assert_eq!(snap_done.failure_streak, 0);
            assert!(snap_done.next_eligible.is_none());
            assert!(snap_done.run_count >= 5);

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            sleep(Duration::from_millis(400)).await;
            let snap = running.status("recover").await.unwrap();
            assert_eq!(
                snap.failure_streak, 0,
                "streak must reset on success, got {snap:?}"
            );
            assert_eq!(snap.status, Status::Completed);

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            // Two failed manual runs trip the flow.
            running.trigger("manual_flaky").await.unwrap();
            sleep(Duration::from_millis(40)).await;
            running.trigger("manual_flaky").await.unwrap();
            sleep(Duration::from_millis(40)).await;

            let snap = running.status("manual_flaky").await.unwrap();
            assert!(
                matches!(snap.status, Status::Tripped { .. }),
                "expected trip, got: {:?}",
                snap.status
            );

            // Subsequent trigger must be rejected with the documented error.
            let err = running
                .trigger("manual_flaky")
                .await
                .expect_err("trigger on tripped flow must fail");
            assert!(
                err.to_string().contains("tripped"),
                "expected tripped error, got: {err}"
            );

            // Reset and trigger again — accepted.
            running.reset_flow("manual_flaky").await.unwrap();
            running
                .trigger("manual_flaky")
                .await
                .expect("trigger after reset must succeed");

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            let err = running.reset_flow("missing").await.unwrap_err();
            assert!(
                err.to_string().contains("No workflow registered"),
                "expected unknown error, got: {err}"
            );

            running.stop().await.unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
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

            let running = scheduler.start().await.unwrap();
            // Wait for first run + status update so the loop is parked on
            // the backoff window.
            sleep(Duration::from_millis(150)).await;
            assert!(matches!(
                running.status("long_backoff").await.unwrap().status,
                Status::Backoff { .. }
            ));

            let stop_started = tokio::time::Instant::now();
            running.stop().await.unwrap();
            let elapsed = stop_started.elapsed();

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

            let running = scheduler.start().await.unwrap();

            // Poll until we observe a non-Running status with run_count ≥ 3.
            // The loop alternates Running ↔ Failed quickly, so a single sleep
            // can land on either side. Polling makes the test robust without
            // hiding bugs (a stuck status never escapes the inner timeout).
            let snap = loop {
                sleep(Duration::from_millis(50)).await;
                let s = running.status("legacy").await.unwrap();
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

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            // Trigger #1 fails, parks the flow in Backoff.
            running.trigger("flow").await.unwrap();
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = running.status("flow").await.unwrap();
                if matches!(s.status, Status::Backoff { .. }) {
                    break s;
                }
            };
            assert_eq!(snap.run_count, 1);

            // Trigger #2 within the backoff window — must NOT be rejected.
            running.trigger("flow").await.unwrap();

            // Wait for the override run to land and clear the window.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = running.status("flow").await.unwrap();
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

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            // Wait for the first trip.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = running.status("flow").await.unwrap();
                if matches!(s.status, Status::Tripped { .. }) {
                    break s;
                }
            };
            let runs_at_first_trip = snap.run_count;
            assert_eq!(snap.failure_streak, 2, "tripped at streak limit");

            // Reset and confirm streak is cleared.
            running.reset_flow("flow").await.unwrap();
            let snap = running.status("flow").await.unwrap();
            assert_eq!(snap.failure_streak, 0);
            assert!(matches!(
                snap.status,
                Status::Idle | Status::Backoff { .. } | Status::Running
            ));

            // Wait for the second trip.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = running.status("flow").await.unwrap();
                if matches!(s.status, Status::Tripped { .. }) && s.run_count > runs_at_first_trip {
                    break s;
                }
            };
            assert_eq!(snap.failure_streak, 2, "second trip also at streak limit");
            assert!(snap.run_count >= runs_at_first_trip + 2);

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            // Wait for the first run + backoff status.
            let snap = loop {
                sleep(Duration::from_millis(50)).await;
                let s = running.status("flow").await.unwrap();
                if matches!(s.status, Status::Backoff { .. }) {
                    break s;
                }
            };
            assert_eq!(snap.run_count, 1);

            // Sit through ~3 cron ticks (3s) and verify run_count holds steady.
            sleep(Duration::from_millis(3200)).await;
            let snap = running.status("flow").await.unwrap();
            assert_eq!(
                snap.run_count, 1,
                "cron ticks must not dispatch inside backoff window"
            );
            assert!(matches!(snap.status, Status::Backoff { .. }));

            running.stop().await.unwrap();
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

            let running = scheduler.start().await.unwrap();

            // Drive streak up to 3, then reset and trigger one more failure.
            for _ in 0..3 {
                running.trigger("flow").await.unwrap();
                loop {
                    sleep(Duration::from_millis(15)).await;
                    let s = running.status("flow").await.unwrap();
                    if matches!(s.status, Status::Backoff { .. }) {
                        break;
                    }
                }
            }
            assert_eq!(running.status("flow").await.unwrap().failure_streak, 3);

            running.reset_flow("flow").await.unwrap();
            assert_eq!(running.status("flow").await.unwrap().failure_streak, 0);

            // One more failure after the reset puts streak at 1, not 4.
            running.trigger("flow").await.unwrap();
            let snap = loop {
                sleep(Duration::from_millis(15)).await;
                let s = running.status("flow").await.unwrap();
                if matches!(s.status, Status::Backoff { .. }) {
                    break s;
                }
            };
            assert_eq!(
                snap.failure_streak, 1,
                "reset clears streak — next failure starts a fresh streak"
            );

            running.stop().await.unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_drop_aborts_loops_when_no_stop() {
        // Drop on the last RunningScheduler clone should abort the spawned
        // driver and per-flow loop tasks instead of leaking them. We assert
        // via the node's own execution_count — once we drop the handle, the
        // count must freeze.
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

            let running = scheduler.start().await.unwrap();

            // Let the loop tick a few times.
            sleep(Duration::from_millis(120)).await;
            let observed = count.load(Ordering::SeqCst);
            assert!(
                observed >= 2,
                "loop should have ticked at least twice, got {observed}"
            );

            // Drop the handle without calling stop(). Drop must abort the
            // driver + per-flow loops; subsequent samples must freeze.
            drop(running);

            // Brief pause so any in-flight execution can finish writing
            // counter increments before we sample.
            sleep(Duration::from_millis(50)).await;
            let final_count = count.load(Ordering::SeqCst);
            sleep(Duration::from_millis(120)).await;
            let later_count = count.load(Ordering::SeqCst);
            assert_eq!(
                final_count, later_count,
                "spawned loops must stop after RunningScheduler is dropped (was {final_count}, became {later_count})"
            );
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }
}
