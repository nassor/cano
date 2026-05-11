//! The `Scheduler` builder — register workflows, then `start()` to obtain a
//! [`RunningScheduler`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::str::FromStr;
use std::sync::Arc;

use cron::Schedule as CronSchedule;
use tokio::sync::{Notify, RwLock, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::error::{CanoError, CanoResult};
use crate::workflow::Workflow;

use super::loops::{driver_task, spawn_cron_loop, spawn_every_loop};
use super::{
    BackoffPolicy, FlowData, FlowInfo, ParsedSchedule, RunningScheduler, SchedulerCommand, Status,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::test_support::*;

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
}
