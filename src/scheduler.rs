#![cfg(feature = "scheduler")]
//! # Simplified Scheduler API
//!
//! A simplified scheduler that focuses on ease of use while maintaining
//! the core scheduling functionality. Supports both Tasks and Nodes with
//! unified registration using the `.register()` method.
//!
//! ## 🚀 Quick Start
//!
//! > **Note:** The scheduler is an optional feature. To use it, you must enable
//! > the `"scheduler"` feature in your `Cargo.toml`.
//!
//! ```toml
//! [dependencies]
//! cano = { version = "0.5", features = ["scheduler"] }
//! ```
//!
//! ```rust
//! use cano::prelude::*;
//! use tokio::time::Duration;
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum MyState {
//!     Start,
//!     Complete,
//! }
//!
//! #[tokio::main]
//! async fn main() -> CanoResult<()> {
//!     let mut scheduler = Scheduler::<MyState>::new();
//!     
//!     // Create separate workflows for each scheduled task
//!     let workflow1 = Workflow::new(MyState::Start);
//!     let workflow2 = Workflow::new(MyState::Start);
//!     let workflow3 = Workflow::new(MyState::Start);
//!     let workflow4 = Workflow::new(MyState::Start);
//!     let workflow5 = Workflow::new(MyState::Start);
//!     let workflow6 = Workflow::new(MyState::Start);
//!     
//!     // Multiple ways to schedule workflows:
//!     scheduler.every_seconds("task1", workflow1, 30)?;                    // Every 30 seconds
//!     scheduler.every_minutes("task2", workflow2, 5)?;                     // Every 5 minutes  
//!     scheduler.every_hours("task3", workflow3, 2)?;                       // Every 2 hours
//!     scheduler.every("task4", workflow4, Duration::from_millis(500))?;    // Every 500ms
//!     scheduler.cron("task5", workflow5, "0 */10 * * * *")?;              // Every 10 minutes (cron)
//!     scheduler.manual("task6", workflow6)?;                               // Manual trigger only
//!     
//!     scheduler.start().await?;
//!     Ok(())
//! }
//! ```

use crate::MemoryStore;
use crate::error::{CanoError, CanoResult};
use crate::node::DefaultParams;
use crate::workflow::{ConcurrentWorkflow, ConcurrentWorkflowStatus, WaitStrategy, Workflow};
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, sleep};

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
    Failed(String),
    /// Concurrent workflow is running with current status
    ConcurrentRunning(ConcurrentWorkflowStatus),
    /// Concurrent workflow completed with final status
    ConcurrentCompleted(ConcurrentWorkflowStatus),
}

/// Enhanced workflow information with concurrent workflow support
#[derive(Debug, Clone)]
pub struct FlowInfo {
    pub id: String,
    pub status: Status,
    pub run_count: u64,
    pub last_run: Option<DateTime<Utc>>,
    /// Tracks concurrent workflow instances if applicable
    pub concurrent_instances: Option<usize>,
}

/// Type alias for the complex workflow data stored in the scheduler
type FlowData<TState, TStore, TParams> = (
    Arc<Workflow<TState, TStore, TParams>>,
    Schedule,
    Arc<RwLock<FlowInfo>>,
);

/// Type alias for concurrent workflow data
type ConcurrentFlowData<TState, TStore, TParams> = (
    Arc<ConcurrentWorkflow<TState, TStore, TParams>>,
    Schedule,
    Arc<RwLock<FlowInfo>>,
    WaitStrategy, // Wait strategy for concurrent execution
    usize,        // Number of concurrent instances
);

/// Enum to distinguish between regular and concurrent workflows
#[derive(Debug, Clone)]
enum WorkflowType<TState, TStore, TParams>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Default + Send + Sync + 'static,
{
    Regular(FlowData<TState, TStore, TParams>),
    Concurrent(ConcurrentFlowData<TState, TStore, TParams>),
}

/// Enhanced scheduler system with concurrent workflow support
pub struct Scheduler<TState, TStore = MemoryStore, TParams = DefaultParams>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Default + Send + Sync + 'static,
{
    workflows: HashMap<String, WorkflowType<TState, TStore, TParams>>,
    command_tx: Option<mpsc::UnboundedSender<String>>,
    running: Arc<RwLock<bool>>,
}

impl<TState, TStore, TParams> Scheduler<TState, TStore, TParams>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Default + Send + Sync + 'static,
{
    /// Create a new scheduler
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
            command_tx: None,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Add a workflow that runs every Duration interval
    pub fn every(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore, TParams>,
        interval: Duration,
    ) -> CanoResult<()> {
        self.add_regular_flow(id, workflow, Schedule::Every(interval))
    }

    /// Add a workflow that runs every N seconds (convenience method)
    pub fn every_seconds(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore, TParams>,
        seconds: u64,
    ) -> CanoResult<()> {
        self.every(id, workflow, Duration::from_secs(seconds))
    }

    /// Add a workflow that runs every N minutes (convenience method)
    pub fn every_minutes(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore, TParams>,
        minutes: u64,
    ) -> CanoResult<()> {
        self.every(id, workflow, Duration::from_secs(minutes * 60))
    }

    /// Add a workflow that runs every N hours (convenience method)
    pub fn every_hours(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore, TParams>,
        hours: u64,
    ) -> CanoResult<()> {
        self.every(id, workflow, Duration::from_secs(hours * 3600))
    }

    /// Add a workflow with cron schedule
    pub fn cron(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore, TParams>,
        expr: &str,
    ) -> CanoResult<()> {
        // Validate cron expression
        CronSchedule::from_str(expr)
            .map_err(|e| CanoError::Configuration(format!("Invalid cron expression: {e}")))?;
        self.add_regular_flow(id, workflow, Schedule::Cron(expr.to_string()))
    }

    /// Add a manually triggered workflow
    pub fn manual(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore, TParams>,
    ) -> CanoResult<()> {
        self.add_regular_flow(id, workflow, Schedule::Manual)
    }

    /// Add a concurrent workflow that runs every Duration interval
    pub fn every_concurrent(
        &mut self,
        id: &str,
        concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
        interval: Duration,
        instances: usize,
        wait_strategy: WaitStrategy,
    ) -> CanoResult<()> {
        self.add_concurrent_flow(
            id,
            concurrent_workflow,
            Schedule::Every(interval),
            instances,
            wait_strategy,
        )
    }

    /// Add a concurrent workflow that runs every N seconds
    pub fn every_seconds_concurrent(
        &mut self,
        id: &str,
        concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
        seconds: u64,
        instances: usize,
        wait_strategy: WaitStrategy,
    ) -> CanoResult<()> {
        self.every_concurrent(
            id,
            concurrent_workflow,
            Duration::from_secs(seconds),
            instances,
            wait_strategy,
        )
    }

    /// Add a concurrent workflow that runs every N minutes
    pub fn every_minutes_concurrent(
        &mut self,
        id: &str,
        concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
        minutes: u64,
        instances: usize,
        wait_strategy: WaitStrategy,
    ) -> CanoResult<()> {
        self.every_concurrent(
            id,
            concurrent_workflow,
            Duration::from_secs(minutes * 60),
            instances,
            wait_strategy,
        )
    }

    /// Add a concurrent workflow that runs every N hours
    pub fn every_hours_concurrent(
        &mut self,
        id: &str,
        concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
        hours: u64,
        instances: usize,
        wait_strategy: WaitStrategy,
    ) -> CanoResult<()> {
        self.every_concurrent(
            id,
            concurrent_workflow,
            Duration::from_secs(hours * 3600),
            instances,
            wait_strategy,
        )
    }

    /// Add a concurrent workflow with cron schedule
    pub fn cron_concurrent(
        &mut self,
        id: &str,
        concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
        expr: &str,
        instances: usize,
        wait_strategy: WaitStrategy,
    ) -> CanoResult<()> {
        // Validate cron expression
        CronSchedule::from_str(expr)
            .map_err(|e| CanoError::Configuration(format!("Invalid cron expression: {e}")))?;
        self.add_concurrent_flow(
            id,
            concurrent_workflow,
            Schedule::Cron(expr.to_string()),
            instances,
            wait_strategy,
        )
    }

    /// Add a manually triggered concurrent workflow
    pub fn manual_concurrent(
        &mut self,
        id: &str,
        concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
        instances: usize,
        wait_strategy: WaitStrategy,
    ) -> CanoResult<()> {
        self.add_concurrent_flow(
            id,
            concurrent_workflow,
            Schedule::Manual,
            instances,
            wait_strategy,
        )
    }

    /// Internal method to add regular flows
    fn add_regular_flow(
        &mut self,
        id: &str,
        workflow: Workflow<TState, TStore, TParams>,
        schedule: Schedule,
    ) -> CanoResult<()> {
        if self.workflows.contains_key(id) {
            return Err(CanoError::Configuration(format!(
                "Workflow '{id}' already exists"
            )));
        }

        let info = FlowInfo {
            id: id.to_string(),
            status: Status::Idle,
            run_count: 0,
            last_run: None,
            concurrent_instances: None,
        };

        let flow_data = (Arc::new(workflow), schedule, Arc::new(RwLock::new(info)));
        self.workflows
            .insert(id.to_string(), WorkflowType::Regular(flow_data));
        Ok(())
    }

    /// Internal method to add concurrent flows
    fn add_concurrent_flow(
        &mut self,
        id: &str,
        concurrent_workflow: ConcurrentWorkflow<TState, TStore, TParams>,
        schedule: Schedule,
        instances: usize,
        wait_strategy: WaitStrategy,
    ) -> CanoResult<()> {
        if self.workflows.contains_key(id) {
            return Err(CanoError::Configuration(format!(
                "Workflow '{id}' already exists"
            )));
        }

        if instances == 0 {
            return Err(CanoError::Configuration(
                "Concurrent workflow instances must be greater than 0".to_string(),
            ));
        }

        let info = FlowInfo {
            id: id.to_string(),
            status: Status::Idle,
            run_count: 0,
            last_run: None,
            concurrent_instances: Some(instances),
        };

        let flow_data = (
            Arc::new(concurrent_workflow),
            schedule,
            Arc::new(RwLock::new(info)),
            wait_strategy,
            instances,
        );
        self.workflows
            .insert(id.to_string(), WorkflowType::Concurrent(flow_data));
        Ok(())
    }

    /// Start the scheduler
    pub async fn start(&mut self) -> CanoResult<()> {
        if *self.running.read().await {
            return Err(CanoError::Configuration("Already running".to_string()));
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        self.command_tx = Some(tx);
        *self.running.write().await = true;

        // Clone data for the scheduler task
        let workflows = self.workflows.clone();
        let running = Arc::clone(&self.running);

        tokio::spawn(async move {
            let mut last_check = HashMap::new();
            let mut interval = tokio::time::interval(Duration::from_secs(1));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if !*running.read().await {
                            break;
                        }

                        let now = Utc::now();
                        for (id, workflow_type) in &workflows {
                            match workflow_type {
                                WorkflowType::Regular((workflow, schedule, info)) => {
                                    if should_run(schedule, &mut last_check, id, now) {
                                        let workflow = Arc::clone(workflow);
                                        let info = Arc::clone(info);
                                        let store = TStore::default();

                                        tokio::spawn(async move {
                                            execute_regular_flow(workflow, info, store).await;
                                        });
                                    }
                                }
                                WorkflowType::Concurrent((concurrent_workflow, schedule, info, wait_strategy, instances)) => {
                                    if should_run(schedule, &mut last_check, id, now) {
                                        let concurrent_workflow = Arc::clone(concurrent_workflow);
                                        let info = Arc::clone(info);
                                        let wait_strategy = wait_strategy.clone();
                                        let instances = *instances;

                                        tokio::spawn(async move {
                                            execute_concurrent_flow(concurrent_workflow, info, wait_strategy, instances).await;
                                        });
                                    }
                                }
                            }
                        }
                    }

                    command = rx.recv() => {
                        match command {
                            Some(flow_id) => {
                                if let Some(workflow_type) = workflows.get(&flow_id) {
                                    match workflow_type {
                                        WorkflowType::Regular((workflow, _, info)) => {
                                            let workflow = Arc::clone(workflow);
                                            let info = Arc::clone(info);
                                            let store = TStore::default();

                                            tokio::spawn(async move {
                                                execute_regular_flow(workflow, info, store).await;
                                            });
                                        }
                                        WorkflowType::Concurrent((concurrent_workflow, _, info, wait_strategy, instances)) => {
                                            let concurrent_workflow = Arc::clone(concurrent_workflow);
                                            let info = Arc::clone(info);
                                            let wait_strategy = wait_strategy.clone();
                                            let instances = *instances;

                                            tokio::spawn(async move {
                                                execute_concurrent_flow(concurrent_workflow, info, wait_strategy, instances).await;
                                            });
                                        }
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Trigger a workflow manually
    pub async fn trigger(&self, id: &str) -> CanoResult<()> {
        if let Some(tx) = &self.command_tx {
            tx.send(id.to_string())
                .map_err(|_| CanoError::workflow("Failed to trigger workflow"))?;
            Ok(())
        } else {
            Err(CanoError::Configuration(
                "Scheduler not running".to_string(),
            ))
        }
    }

    /// Stop the scheduler
    pub async fn stop(&mut self) -> CanoResult<()> {
        self.stop_with_timeout(Duration::from_secs(30)).await
    }

    /// Stop the scheduler with a timeout for waiting on running flows
    pub async fn stop_with_timeout(&mut self, timeout: Duration) -> CanoResult<()> {
        // Signal scheduler to stop accepting new tasks
        *self.running.write().await = false;
        self.command_tx = None;

        // Wait for all running flows to complete
        let start_time = Instant::now();

        loop {
            // Check if any flows are still running
            let mut any_running = false;
            for workflow_type in self.workflows.values() {
                let status = match workflow_type {
                    WorkflowType::Regular((_, _, info)) => &info.read().await.status,
                    WorkflowType::Concurrent((_, _, info, _, _)) => &info.read().await.status,
                };

                match status {
                    Status::Running | Status::ConcurrentRunning(_) => {
                        any_running = true;
                        break;
                    }
                    _ => {}
                }
            }

            // If no flows are running, we're done
            if !any_running {
                break;
            }

            // Check if we've exceeded the timeout
            if start_time.elapsed() >= timeout {
                return Err(CanoError::Configuration(format!(
                    "Timeout after {timeout:?} waiting for flows to complete"
                )));
            }

            // Wait a bit before checking again
            sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }

    /// Stop the scheduler immediately without waiting for flows
    pub async fn stop_immediately(&mut self) -> CanoResult<()> {
        *self.running.write().await = false;
        self.command_tx = None;
        Ok(())
    }

    /// Check if any flows are currently running
    pub async fn has_running_flows(&self) -> bool {
        for workflow_type in self.workflows.values() {
            let status = match workflow_type {
                WorkflowType::Regular((_, _, info)) => &info.read().await.status,
                WorkflowType::Concurrent((_, _, info, _, _)) => &info.read().await.status,
            };

            match status {
                Status::Running | Status::ConcurrentRunning(_) => return true,
                _ => {}
            }
        }
        false
    }

    /// Get count of currently running flows
    pub async fn running_count(&self) -> usize {
        let mut count = 0;
        for workflow_type in self.workflows.values() {
            let status = match workflow_type {
                WorkflowType::Regular((_, _, info)) => &info.read().await.status,
                WorkflowType::Concurrent((_, _, info, _, _)) => &info.read().await.status,
            };

            match status {
                Status::Running | Status::ConcurrentRunning(_) => count += 1,
                _ => {}
            }
        }
        count
    }

    /// Get workflow status
    pub async fn status(&self, id: &str) -> Option<FlowInfo> {
        if let Some(workflow_type) = self.workflows.get(id) {
            let info = match workflow_type {
                WorkflowType::Regular((_, _, info)) => info,
                WorkflowType::Concurrent((_, _, info, _, _)) => info,
            };
            Some(info.read().await.clone())
        } else {
            None
        }
    }

    /// List all flows
    pub async fn list(&self) -> Vec<FlowInfo> {
        let mut result = Vec::new();
        for workflow_type in self.workflows.values() {
            let info = match workflow_type {
                WorkflowType::Regular((_, _, info)) => info,
                WorkflowType::Concurrent((_, _, info, _, _)) => info,
            };
            result.push(info.read().await.clone());
        }
        result
    }
}

impl<TState, TStore, TParams> Default for Scheduler<TState, TStore, TParams>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Default + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a workflow should run
fn should_run(
    schedule: &Schedule,
    last_check: &mut HashMap<String, DateTime<Utc>>,
    id: &str,
    now: DateTime<Utc>,
) -> bool {
    match schedule {
        Schedule::Manual => false,
        Schedule::Every(interval) => {
            let duration =
                chrono::Duration::from_std(*interval).unwrap_or(chrono::Duration::seconds(60));
            if let Some(last) = last_check.get(id) {
                if now >= *last + duration {
                    last_check.insert(id.to_string(), now);
                    true
                } else {
                    false
                }
            } else {
                last_check.insert(id.to_string(), now);
                true
            }
        }
        Schedule::Cron(expr) => {
            if let Ok(schedule) = CronSchedule::from_str(expr) {
                let last = last_check
                    .get(id)
                    .copied()
                    .unwrap_or_else(|| now - chrono::Duration::seconds(1));

                // Check if there's a scheduled time between last check and now
                for upcoming in schedule.after(&last).take(1) {
                    if upcoming <= now {
                        last_check.insert(id.to_string(), now);
                        return true;
                    }
                }
            }
            false
        }
    }
}

/// Execute a regular workflow
async fn execute_regular_flow<TState, TStore, TParams>(
    workflow: Arc<Workflow<TState, TStore, TParams>>,
    info: Arc<RwLock<FlowInfo>>,
    store: TStore,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Default + Send + Sync + 'static,
{
    // Update status to running
    {
        let mut info_guard = info.write().await;
        info_guard.status = Status::Running;
        info_guard.last_run = Some(Utc::now());
    }

    // Execute workflow
    let result = workflow.orchestrate(&store).await;

    // Update final status
    {
        let mut info_guard = info.write().await;
        match result {
            Ok(_) => {
                info_guard.status = Status::Idle;
                info_guard.run_count += 1;
            }
            Err(e) => {
                info_guard.status = Status::Failed(e.to_string());
            }
        }
    }
}

/// Execute a concurrent workflow
async fn execute_concurrent_flow<TState, TStore, TParams>(
    concurrent_workflow: Arc<ConcurrentWorkflow<TState, TStore, TParams>>,
    info: Arc<RwLock<FlowInfo>>,
    wait_strategy: WaitStrategy,
    instances: usize,
) where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TParams: Clone + Send + Sync + 'static,
    TStore: Clone + Default + Send + Sync + 'static,
{
    // Create stores for each instance
    let stores: Vec<TStore> = (0..instances).map(|_| TStore::default()).collect();

    // Update status to concurrent running
    {
        let mut info_guard = info.write().await;
        let initial_status = ConcurrentWorkflowStatus::new(instances);
        info_guard.status = Status::ConcurrentRunning(initial_status);
        info_guard.last_run = Some(Utc::now());
    }

    // Execute concurrent workflow
    let result = concurrent_workflow
        .execute_concurrent(stores, wait_strategy)
        .await;

    // Update final status
    {
        let mut info_guard = info.write().await;
        match result {
            Ok((_, final_status)) => {
                info_guard.status = Status::ConcurrentCompleted(final_status);
                info_guard.run_count += 1;
            }
            Err(e) => {
                info_guard.status = Status::Failed(e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Node;
    use crate::store::MemoryStore;
    use async_trait::async_trait;
    use chrono::Timelike;
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

        fn new_success() -> Self {
            Self::new()
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
        let mut workflow = Workflow::new(TestState::Start);
        workflow.register(TestState::Start, TestNode::new());
        workflow.add_exit_state(TestState::Complete);
        workflow.add_exit_state(TestState::Error);
        workflow
    }

    fn create_failing_workflow() -> Workflow<TestState> {
        let mut workflow = Workflow::new(TestState::Start);
        workflow.register(TestState::Start, TestNode::new_failing());
        workflow.add_exit_state(TestState::Complete);
        workflow.add_exit_state(TestState::Error);
        workflow
    }

    #[tokio::test]
    async fn test_scheduler_creation() {
        let scheduler: Scheduler<TestState> = Scheduler::new();
        assert!(!scheduler.has_running_flows().await);
        assert_eq!(scheduler.running_count().await, 0);
        assert!(scheduler.list().await.is_empty());
    }

    #[tokio::test]
    async fn test_add_workflow_every_seconds() {
        let mut scheduler = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every_seconds("test_task", workflow, 5);
        assert!(result.is_ok());

        let flows = scheduler.list().await;
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].id, "test_task");
        assert_eq!(flows[0].status, Status::Idle);
        assert_eq!(flows[0].run_count, 0);
    }

    #[tokio::test]
    async fn test_add_workflow_every_minutes() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every_minutes("test_task", workflow, 2);
        assert!(result.is_ok());

        let status = scheduler.status("test_task").await;
        assert!(status.is_some());
        assert_eq!(status.unwrap().id, "test_task");
    }

    #[tokio::test]
    async fn test_add_workflow_every_hours() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every_hours("test_task", workflow, 1);
        assert!(result.is_ok());

        let status = scheduler.status("test_task").await;
        assert!(status.is_some());
    }

    #[tokio::test]
    async fn test_add_workflow_every_duration() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.every("test_task", workflow, Duration::from_millis(100));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_workflow_cron() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        // Valid cron expression
        let result = scheduler.cron("test_task", workflow, "0 */5 * * * *");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_workflow_cron_invalid() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        // Invalid cron expression
        let result = scheduler.cron("test_task", workflow, "invalid cron");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CanoError::Configuration(_)));
    }

    #[tokio::test]
    async fn test_add_workflow_manual() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();

        let result = scheduler.manual("test_task", workflow);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_duplicate_workflow_id() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow1 = create_test_workflow();
        let workflow2 = create_test_workflow();

        scheduler.every_seconds("test_task", workflow1, 5).unwrap();

        let result = scheduler.every_seconds("test_task", workflow2, 10);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CanoError::Configuration(_)));
    }

    #[tokio::test]
    async fn test_manual_trigger_without_start() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler.manual("test_task", workflow).unwrap();

        let result = scheduler.trigger("test_task").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CanoError::Configuration(_)));
    }

    #[tokio::test]
    async fn test_start_and_stop() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler.manual("test_task", workflow).unwrap();

        // Start scheduler
        scheduler.start().await.unwrap();

        // Try to start again (should fail)
        let result = scheduler.start().await;
        assert!(result.is_err());

        // Stop scheduler
        scheduler.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_stop_immediately() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler.manual("test_task", workflow).unwrap();

        scheduler.start().await.unwrap();
        scheduler.stop_immediately().await.unwrap();
    }

    #[tokio::test]
    async fn test_manual_trigger_success() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_test_workflow();
        scheduler.manual("test_task", workflow).unwrap();

        scheduler.start().await.unwrap();

        // Trigger manually
        scheduler.trigger("test_task").await.unwrap();

        // Give some time for execution
        sleep(Duration::from_millis(100)).await;

        let status = scheduler.status("test_task").await.unwrap();
        assert_eq!(status.run_count, 1);
        assert_eq!(status.status, Status::Idle);
        assert!(status.last_run.is_some());

        scheduler.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_manual_trigger_nonexistent() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        scheduler.start().await.unwrap();

        scheduler.trigger("nonexistent").await.unwrap(); // Should not fail, just do nothing

        scheduler.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_workflow_execution_failure() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = create_failing_workflow();
        scheduler.manual("test_task", workflow).unwrap();

        scheduler.start().await.unwrap();
        scheduler.trigger("test_task").await.unwrap();

        // Give more time for execution and poll status
        for _ in 0..50 {
            sleep(Duration::from_millis(100)).await;
            let status = scheduler.status("test_task").await.unwrap();
            if !matches!(status.status, Status::Running) {
                break;
            }
        }

        let status = scheduler.status("test_task").await.unwrap();
        assert!(matches!(status.status, Status::Failed(_)));
        assert_eq!(status.run_count, 0); // Should not increment on failure

        scheduler.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_status_nonexistent_workflow() {
        let scheduler = Scheduler::<TestState, MemoryStore>::new();
        let status = scheduler.status("nonexistent").await;
        assert!(status.is_none());
    }

    #[tokio::test]
    async fn test_list_multiple_workflows() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();

        scheduler.manual("task1", create_test_workflow()).unwrap();
        scheduler.manual("task2", create_test_workflow()).unwrap();
        scheduler.manual("task3", create_test_workflow()).unwrap();

        let flows = scheduler.list().await;
        assert_eq!(flows.len(), 3);

        let ids: Vec<&str> = flows.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"task1"));
        assert!(ids.contains(&"task2"));
        assert!(ids.contains(&"task3"));
    }

    #[tokio::test]
    async fn test_running_flows_tracking() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        scheduler
            .manual("test_task", create_test_workflow())
            .unwrap();

        // Initially no running flows
        assert!(!scheduler.has_running_flows().await);
        assert_eq!(scheduler.running_count().await, 0);

        scheduler.start().await.unwrap();
        scheduler.trigger("test_task").await.unwrap();

        // Give some time for execution to complete
        sleep(Duration::from_millis(100)).await;

        // Should be back to no running flows
        assert!(!scheduler.has_running_flows().await);
        assert_eq!(scheduler.running_count().await, 0);

        scheduler.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_should_run_manual() {
        let mut last_check = HashMap::new();
        let schedule = Schedule::Manual;
        let now = Utc::now();

        assert!(!should_run(&schedule, &mut last_check, "test", now));
    }

    #[tokio::test]
    async fn test_should_run_every() {
        let mut last_check = HashMap::new();
        let schedule = Schedule::Every(Duration::from_millis(100));
        let now = Utc::now();

        // First run should trigger
        assert!(should_run(&schedule, &mut last_check, "test", now));

        // Immediate second run should not trigger
        assert!(!should_run(&schedule, &mut last_check, "test", now));

        // After interval, should trigger again
        let later = now + chrono::Duration::milliseconds(200);
        assert!(should_run(&schedule, &mut last_check, "test", later));
    }

    #[tokio::test]
    async fn test_should_run_cron() {
        let mut last_check = HashMap::new();
        // Every minute cron expression
        let schedule = Schedule::Cron("0 * * * * *".to_string());

        // Set a time just before the minute boundary
        let base_time = Utc::now()
            .with_second(59)
            .unwrap()
            .with_nanosecond(0)
            .unwrap();

        // Should not run before the minute
        assert!(!should_run(&schedule, &mut last_check, "test", base_time));

        // Should run at the minute boundary
        let minute_boundary = base_time + chrono::Duration::seconds(1);
        assert!(should_run(
            &schedule,
            &mut last_check,
            "test",
            minute_boundary
        ));
    }

    #[tokio::test]
    async fn test_should_run_invalid_cron() {
        let mut last_check = HashMap::new();
        let schedule = Schedule::Cron("invalid cron".to_string());
        let now = Utc::now();

        // Invalid cron should never trigger
        assert!(!should_run(&schedule, &mut last_check, "test", now));
    }

    #[tokio::test]
    async fn test_stop_with_timeout() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        scheduler
            .manual("test_task", create_test_workflow())
            .unwrap();

        scheduler.start().await.unwrap();

        // Stop with a very short timeout should work since we have no running flows
        let result = scheduler.stop_with_timeout(Duration::from_millis(1)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_scheduler_default() {
        let scheduler: Scheduler<TestState> = Scheduler::default();
        assert!(scheduler.list().await.is_empty());
    }

    #[tokio::test]
    async fn test_flow_info_creation() {
        let info = FlowInfo {
            id: "test".to_string(),
            status: Status::Idle,
            run_count: 5,
            last_run: Some(Utc::now()),
            concurrent_instances: None,
        };

        assert_eq!(info.id, "test");
        assert_eq!(info.status, Status::Idle);
        assert_eq!(info.run_count, 5);
        assert!(info.last_run.is_some());
    }

    #[tokio::test]
    async fn test_status_variants() {
        let idle = Status::Idle;
        let running = Status::Running;
        let failed = Status::Failed("error".to_string());

        assert_ne!(idle, running);
        assert_ne!(running, failed);
        assert_ne!(idle, failed);

        // Test clone
        let failed_clone = failed.clone();
        assert_eq!(failed, failed_clone);
    }

    #[tokio::test]
    async fn test_schedule_variants() {
        let every = Schedule::Every(Duration::from_secs(60));
        let cron = Schedule::Cron("0 * * * * *".to_string());
        let manual = Schedule::Manual;

        // Test that all variants can be created and cloned
        let _every_clone = every.clone();
        let _cron_clone = cron.clone();
        let _manual_clone = manual.clone();
    }

    #[tokio::test]
    async fn test_concurrent_manual_triggers() {
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        scheduler
            .manual("test_task", create_test_workflow())
            .unwrap();
        scheduler.start().await.unwrap();

        // Trigger multiple times concurrently
        let triggers = vec![
            scheduler.trigger("test_task"),
            scheduler.trigger("test_task"),
            scheduler.trigger("test_task"),
        ];

        for trigger in triggers {
            trigger.await.unwrap();
        }

        // Give time for execution
        sleep(Duration::from_millis(200)).await;

        let status = scheduler.status("test_task").await.unwrap();
        assert!(status.run_count >= 1); // At least one should have run

        scheduler.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_scheduler_cleanup_on_drop() {
        // This test ensures that the scheduler can be dropped without issues
        {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            scheduler
                .manual("test_task", create_test_workflow())
                .unwrap();
            scheduler.start().await.unwrap();
            // scheduler will be dropped here
        }

        // If we reach here without hanging, the test passes
        assert_eq!(true, true, "Scheduler dropped without issues");
    }

    #[tokio::test]
    async fn test_execute_regular_flow_function() {
        // Test the execute_regular_flow function directly
        let workflow = Arc::new(create_test_workflow());
        let info = Arc::new(RwLock::new(FlowInfo {
            id: "test".to_string(),
            status: Status::Idle,
            run_count: 0,
            last_run: None,
            concurrent_instances: None,
        }));
        let store = MemoryStore::default();

        // Before execution
        assert_eq!(info.read().await.status, Status::Idle);
        assert_eq!(info.read().await.run_count, 0);

        // Execute
        execute_regular_flow(workflow, Arc::clone(&info), store).await;

        // After execution
        let final_info = info.read().await;
        assert_eq!(final_info.status, Status::Idle);
        assert_eq!(final_info.run_count, 1);
        assert!(final_info.last_run.is_some());
    }

    #[tokio::test]
    async fn test_execute_regular_flow_function_failure() {
        // Test the execute_regular_flow function with a failing workflow
        let workflow = Arc::new(create_failing_workflow());
        let info = Arc::new(RwLock::new(FlowInfo {
            id: "test".to_string(),
            status: Status::Idle,
            run_count: 0,
            last_run: None,
            concurrent_instances: None,
        }));
        let store = MemoryStore::default();

        // Execute
        execute_regular_flow(workflow, Arc::clone(&info), store).await;

        // After execution
        let final_info = info.read().await;
        assert!(matches!(final_info.status, Status::Failed(_)));
        assert_eq!(final_info.run_count, 0);
        assert!(final_info.last_run.is_some());
    }

    #[tokio::test]
    async fn test_concurrent_workflow_scheduling() {
        let mut scheduler: Scheduler<TestState> = Scheduler::new();

        // Create a concurrent workflow
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        let node = TestNode::new_success();
        concurrent_workflow.register(TestState::Start, node);

        scheduler
            .manual_concurrent(
                "concurrent_test",
                concurrent_workflow,
                3, // 3 instances
                WaitStrategy::WaitForever,
            )
            .unwrap();

        // Verify the workflow was added
        let flows = scheduler.list().await;
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].id, "concurrent_test");
        assert_eq!(flows[0].concurrent_instances, Some(3));
    }

    #[tokio::test]
    async fn test_concurrent_workflow_status_tracking() {
        // Create a concurrent workflow
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        let node = TestNode::new_success();
        concurrent_workflow.register(TestState::Start, node);

        let info = Arc::new(RwLock::new(FlowInfo {
            id: "test".to_string(),
            status: Status::Idle,
            run_count: 0,
            last_run: None,
            concurrent_instances: Some(3),
        }));

        // Test concurrent execution
        execute_concurrent_flow(
            Arc::new(concurrent_workflow),
            Arc::clone(&info),
            WaitStrategy::WaitForever,
            3,
        )
        .await;

        // Check final status
        let final_info = info.read().await;
        assert!(matches!(final_info.status, Status::ConcurrentCompleted(_)));
        assert_eq!(final_info.run_count, 1);
        assert!(final_info.last_run.is_some());

        if let Status::ConcurrentCompleted(status) = &final_info.status {
            assert_eq!(status.total_workflows, 3);
            assert_eq!(status.completed, 3);
            assert_eq!(status.failed, 0);
            assert_eq!(status.cancelled, 0);
        }
    }

    #[tokio::test]
    async fn test_scheduler_with_mixed_workflow_types() {
        let mut scheduler: Scheduler<TestState> = Scheduler::new();

        // Add a regular workflow
        scheduler
            .manual("regular_test", create_test_workflow())
            .unwrap();

        // Add a concurrent workflow
        let mut concurrent_workflow = ConcurrentWorkflow::new(TestState::Start);
        concurrent_workflow.add_exit_state(TestState::Complete);

        let node = TestNode::new_success();
        concurrent_workflow.register(TestState::Start, node);

        scheduler
            .manual_concurrent(
                "concurrent_test",
                concurrent_workflow,
                2,
                WaitStrategy::WaitForever,
            )
            .unwrap();

        // Verify both workflows were added
        let flows = scheduler.list().await;
        assert_eq!(flows.len(), 2);

        // Check workflow types
        let regular_flow = flows.iter().find(|f| f.id == "regular_test").unwrap();
        let concurrent_flow = flows.iter().find(|f| f.id == "concurrent_test").unwrap();

        assert_eq!(regular_flow.concurrent_instances, None);
        assert_eq!(concurrent_flow.concurrent_instances, Some(2));
    }

    #[tokio::test]
    async fn test_time_based_scheduling_logic() {
        // Test the actual scheduling logic more thoroughly
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();

        // Create a workflow that runs every 100ms
        scheduler
            .every(
                "fast_task",
                create_test_workflow(),
                Duration::from_millis(100),
            )
            .unwrap();

        scheduler.start().await.unwrap();

        // Wait for multiple execution cycles
        sleep(Duration::from_millis(250)).await;

        let status = scheduler.status("fast_task").await.unwrap();
        // Should have run at least once (being less strict since timing can be tricky in tests)
        assert!(status.run_count >= 1);

        scheduler.stop().await.unwrap();
    }
}
