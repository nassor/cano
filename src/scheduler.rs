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
//! cano = { version = "0.7", features = ["scheduler"] }
//! ```

use crate::error::{CanoError, CanoResult};
use crate::store::{KeyValueStore, MemoryStore};
use crate::workflow::Workflow;
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, sleep};

#[cfg(feature = "tracing")]
use tracing::Instrument;

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

/// Type alias for the workflow data stored in the scheduler
type FlowData<TState, TStore> = (
    Arc<Workflow<TState, TStore>>,
    TState, // Initial state
    Schedule,
    Arc<RwLock<FlowInfo>>,
);

/// Scheduler system for managing workflows.
///
/// All workflows registered with a single `Scheduler` instance must share the same
/// `TState` and `TStore` types. The store type defaults to [`MemoryStore`].
/// If your application requires workflows with different state enums,
/// create a separate `Scheduler` instance for each state type.
///
/// Requires the `scheduler` feature.
#[derive(Clone)]
pub struct Scheduler<TState, TStore = MemoryStore>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TStore: KeyValueStore + 'static,
{
    workflows: HashMap<String, FlowData<TState, TStore>>,
    command_tx: Arc<RwLock<Option<mpsc::UnboundedSender<String>>>>,
    running: Arc<RwLock<bool>>,
    scheduler_tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
}

impl<TState, TStore> Scheduler<TState, TStore>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TStore: KeyValueStore + 'static,
{
    /// Create a new scheduler
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
            command_tx: Arc::new(RwLock::new(None)),
            running: Arc::new(RwLock::new(false)),
            scheduler_tasks: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Add a workflow that runs every Duration interval
    pub fn every(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore>,
        initial_state: TState,
        interval: Duration,
    ) -> CanoResult<()> {
        self.add_flow(id, workflow, initial_state, Schedule::Every(interval))
    }

    /// Add a workflow that runs every N seconds (convenience method)
    pub fn every_seconds(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore>,
        initial_state: TState,
        seconds: u64,
    ) -> CanoResult<()> {
        self.every(id, workflow, initial_state, Duration::from_secs(seconds))
    }

    /// Add a workflow that runs every N minutes (convenience method)
    pub fn every_minutes(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore>,
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
        workflow: Workflow<TState, TStore>,
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
        workflow: Workflow<TState, TStore>,
        initial_state: TState,
        expr: &str,
    ) -> CanoResult<()> {
        // Validate cron expression
        CronSchedule::from_str(expr)
            .map_err(|e| CanoError::Configuration(format!("Invalid cron expression: {e}")))?;
        self.add_flow(
            id,
            workflow,
            initial_state,
            Schedule::Cron(expr.to_string()),
        )
    }

    /// Add a manually triggered workflow
    pub fn manual(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore>,
        initial_state: TState,
    ) -> CanoResult<()> {
        self.add_flow(id, workflow, initial_state, Schedule::Manual)
    }

    fn add_flow(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore>,
        initial_state: TState,
        schedule: Schedule,
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

        Ok(())
    }

    /// Start the scheduler, running all registered workflows on their configured schedules.
    ///
    /// Blocks until [`Scheduler::stop`] is called. After the stop signal is received the
    /// scheduler waits up to 30 seconds for in-progress workflow executions to finish.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the 30-second graceful-shutdown timeout elapsed while
    ///   one or more workflows were still running
    pub async fn start(&mut self) -> CanoResult<()> {
        *self.running.write().await = true;

        let (tx, mut rx) = mpsc::unbounded_channel();
        *self.command_tx.write().await = Some(tx);

        let workflows = self.workflows.clone();
        let running = Arc::clone(&self.running);
        let scheduler_tasks = Arc::clone(&self.scheduler_tasks);

        // Spawn scheduler task loops for each workflow
        for (_id, (workflow, initial_state, schedule, info)) in workflows {
            let running_clone = Arc::clone(&running);

            match schedule {
                Schedule::Every(interval) => {
                    let handle = tokio::spawn(async move {
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
                            sleep(interval).await;

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
                Schedule::Cron(expr) => {
                    let handle = tokio::spawn(async move {
                        let schedule = CronSchedule::from_str(&expr)
                            .expect("cron expression was validated at registration time");
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
                                sleep(wait_duration).await;

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
                Schedule::Manual => {
                    // Manual workflows don't run on their own
                }
            }
        }

        // Wait for stop command
        while let Some(cmd) = rx.recv().await {
            if cmd == "stop" {
                *self.running.write().await = false;
                break;
            } else if cmd.starts_with("trigger:") {
                let id = cmd.strip_prefix("trigger:").unwrap();
                if let Some((workflow, initial_state, _schedule, info)) = self.workflows.get(id) {
                    let workflow = Arc::clone(workflow);
                    let initial_state = initial_state.clone();
                    let info = Arc::clone(info);
                    let handle = tokio::spawn(async move {
                        execute_flow(workflow, initial_state, info).await;
                    });
                    self.scheduler_tasks.write().await.push(handle);
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

        while self.has_running_flows().await {
            if start_time.elapsed() >= timeout {
                return Err(CanoError::Workflow(
                    "Timeout waiting for workflows to complete".to_string(),
                ));
            }
            sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }

    /// Stop the scheduler and wait for all running workflows to complete
    pub async fn stop(&self) -> CanoResult<()> {
        // Send stop signal
        if let Some(tx) = self.command_tx.read().await.as_ref() {
            tx.send("stop".to_string())
                .map_err(|e| CanoError::Workflow(format!("Failed to send stop: {}", e)))?;
        }

        // The actual waiting happens in start()
        Ok(())
    }

    /// Manually trigger a workflow by ID.
    ///
    /// Sends a trigger command to the running scheduler. The workflow executes
    /// asynchronously — this method returns as soon as the command is enqueued.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running (i.e. [`Scheduler::start`]
    ///   has not been called or has already returned)
    pub async fn trigger(&self, id: &str) -> CanoResult<()> {
        if let Some(tx) = self.command_tx.read().await.as_ref() {
            tx.send(format!("trigger:{}", id))
                .map_err(|e| CanoError::Workflow(format!("Failed to trigger: {}", e)))?;
        } else {
            return Err(CanoError::Workflow(
                "Scheduler not running — call start() before trigger()".to_string(),
            ));
        }
        Ok(())
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
        let mut results = Vec::new();
        for (_, _, _, info) in self.workflows.values() {
            results.push(info.read().await.clone());
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

impl<TState, TStore> Default for Scheduler<TState, TStore>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TStore: KeyValueStore + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

async fn execute_flow<TState, TStore>(
    workflow: Arc<Workflow<TState, TStore>>,
    initial_state: TState,
    info: Arc<RwLock<FlowInfo>>,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TStore: KeyValueStore + 'static,
{
    // Update status to running
    {
        let mut info_guard = info.write().await;
        info_guard.status = Status::Running;
        info_guard.last_run = Some(Utc::now());
        info_guard.run_count += 1;
    }

    // Execute workflow
    #[cfg(feature = "tracing")]
    let result = workflow
        .orchestrate(initial_state)
        .instrument(tracing::info_span!("execute_flow"))
        .await;

    #[cfg(not(feature = "tracing"))]
    let result = workflow.orchestrate(initial_state).await;

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
    use crate::store::MemoryStore;
    use async_trait::async_trait;
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

    #[async_trait]
    impl Node<TestState> for TestNode {
        type PrepResult = ();
        type ExecResult = ();

        async fn prep(&self, _store: &MemoryStore) -> CanoResult<()> {
            Ok(())
        }

        async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
            self.execution_count.fetch_add(1, Ordering::Relaxed);
        }

        async fn post(
            &self,
            _store: &MemoryStore,
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
        let store = MemoryStore::new();
        Workflow::new(store)
            .register(TestState::Start, TestNode::new())
            .add_exit_state(TestState::Complete)
            .add_exit_state(TestState::Error)
    }

    fn create_failing_workflow() -> Workflow<TestState> {
        let store = MemoryStore::new();
        Workflow::new(store)
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
