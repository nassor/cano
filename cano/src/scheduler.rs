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
//! cano = { version = "0.10", features = ["scheduler"] }
//! ```

use crate::error::{CanoError, CanoResult};
use crate::workflow::Workflow;
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::str::FromStr;
use std::sync::Arc;
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

/// Simple workflow status
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Running,
    Completed,
    Failed(String),
}

/// Workflow information
#[derive(Debug, Clone)]
pub struct FlowInfo {
    pub id: String,
    pub status: Status,
    pub run_count: u64,
    pub last_run: Option<DateTime<Utc>>,
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

/// Type alias for the workflow data stored in the scheduler
type FlowData<TState, TResourceKey> = (
    Arc<Workflow<TState, TResourceKey>>,
    TState, // Initial state
    ParsedSchedule,
    Arc<RwLock<FlowInfo>>,
);

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
            stop_notify: Arc::new(RwLock::new(None)),
            scheduler_tasks: Arc::new(RwLock::new(Vec::new())),
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
        }));

        self.workflows.insert(
            id.to_string(),
            (Arc::new(workflow), initial_state, schedule, info),
        );
        self.flow_order.push(id.to_string());

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
            for (workflow, initial_state, _, _) in ordered.iter() {
                if let Err(e) = workflow
                    .validate()
                    .and_then(|_| workflow.validate_initial_state(initial_state))
                {
                    *self.command_tx.write().await = None;
                    return Err(e);
                }
            }
            for (idx, (workflow, _, _, _)) in ordered.iter().enumerate() {
                if let Err(e) = workflow.resources.setup_all().await {
                    for (prior_wf, _, _, _) in ordered[..idx].iter().rev() {
                        let len = prior_wf.resources.lifecycle_len();
                        prior_wf.resources.teardown_range(0..len).await;
                    }
                    *self.command_tx.write().await = None;
                    return Err(e);
                }
            }
        }

        // Allocate a fresh stop notifier for this run. Loop tasks clone the Arc and
        // race the timer against `notified()` so `stop()` interrupts long sleeps
        // immediately instead of waiting for the cron / interval to fire.
        let notify = Arc::new(Notify::new());
        *self.stop_notify.write().await = Some(Arc::clone(&notify));

        // Setup succeeded — flip the flag and spawn loop tasks.
        *self.running.write().await = true;

        let running = Arc::clone(&self.running);
        let scheduler_tasks = Arc::clone(&self.scheduler_tasks);

        // Spawn scheduler task loops for each workflow
        for (_id, (workflow, initial_state, schedule, info)) in workflows {
            let running_clone = Arc::clone(&running);
            let notify_clone = Arc::clone(&notify);

            match schedule {
                ParsedSchedule::Every(interval) => {
                    let handle = tokio::spawn(async move {
                        // Check running flag before first execution
                        if !*running_clone.read().await {
                            return;
                        }

                        // Check if previous run is still active before executing
                        let status = info.read().await.status.clone();
                        if !matches!(status, Status::Running) {
                            execute_flow(
                                Arc::clone(&workflow),
                                initial_state.clone(),
                                Arc::clone(&info),
                            )
                            .await;
                        }

                        loop {
                            // Race the timer against the stop notifier so a long
                            // interval does not pin the loop on `sleep`.
                            tokio::select! {
                                _ = sleep(interval) => {}
                                _ = notify_clone.notified() => break,
                            }

                            // Check running flag immediately after sleep, before executing
                            if !*running_clone.read().await {
                                break;
                            }

                            // Skip this run if the previous execution is still in progress
                            let status = info.read().await.status.clone();
                            if matches!(status, Status::Running) {
                                continue;
                            }

                            execute_flow(
                                Arc::clone(&workflow),
                                initial_state.clone(),
                                Arc::clone(&info),
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

                                // Skip this run if the previous execution is still in progress
                                let status = info.read().await.status.clone();
                                if matches!(status, Status::Running) {
                                    continue;
                                }

                                execute_flow(
                                    Arc::clone(&workflow),
                                    initial_state.clone(),
                                    Arc::clone(&info),
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
                    let outcome = if let Some((workflow, initial_state, _schedule, info)) =
                        self.workflows.get(&id)
                    {
                        if reserve_flow(Arc::clone(info)).await {
                            let workflow = Arc::clone(workflow);
                            let initial_state = initial_state.clone();
                            let info = Arc::clone(info);
                            let handle = tokio::spawn(async move {
                                execute_reserved_flow(workflow, initial_state, info).await;
                            });
                            let mut tasks = self.scheduler_tasks.write().await;
                            tasks.retain(|h| !h.is_finished());
                            tasks.push(handle);
                            Ok(())
                        } else {
                            Err(CanoError::Workflow(format!(
                                "Flow '{id}' is already running"
                            )))
                        }
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
        for (workflow, _, _, _) in flows_for_teardown.iter().rev() {
            let len = workflow.resources.lifecycle_len();
            workflow.resources.teardown_range(0..len).await;
        }

        // Clear the command sender so subsequent stop()/trigger() calls report
        // the documented "Scheduler not running" error instead of a channel-closed
        // surprise, and allow clean restarts via a fresh start() call.
        *self.command_tx.write().await = None;

        result
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
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running (i.e. [`Scheduler::start`]
    ///   has not been called or has already returned), `id` is unknown, the workflow
    ///   is already running, or the command queue is full
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
        if let Some((_, _, _, info)) = self.workflows.get(id) {
            Some(info.read().await.clone())
        } else {
            None
        }
    }

    /// List all workflows
    pub async fn list(&self) -> Vec<FlowInfo> {
        let mut results = Vec::with_capacity(self.flow_order.len());
        for id in &self.flow_order {
            if let Some((_, _, _, info)) = self.workflows.get(id) {
                results.push(info.read().await.clone());
            }
        }
        results
    }

    /// Check if scheduler has running flows
    pub async fn has_running_flows(&self) -> bool {
        for (_, _, _, info) in self.workflows.values() {
            if info.read().await.status == Status::Running {
                return true;
            }
        }
        false
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

async fn execute_flow<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    if !reserve_flow(Arc::clone(&info)).await {
        return;
    }

    execute_reserved_flow(workflow, initial_state, info).await;
}

async fn reserve_flow(info: Arc<RwLock<FlowInfo>>) -> bool {
    let mut info_guard = info.write().await;
    if matches!(info_guard.status, Status::Running) {
        return false;
    }

    info_guard.status = Status::Running;
    info_guard.last_run = Some(Utc::now());
    info_guard.run_count += 1;
    true
}

async fn execute_reserved_flow<TState, TResourceKey>(
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
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

    // Update status based on result
    let mut info_guard = info.write().await;
    info_guard.status = match result {
        Ok(_) => Status::Completed,
        Err(e) => Status::Failed(e.to_string()),
    };
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
}
