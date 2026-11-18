//! `RunningScheduler` — the live, cloneable handle returned by [`Scheduler::start`](crate::scheduler::Scheduler::start).
//!
//! **Availability:** `scheduler` feature (`since 0.6.0`) · **not wasm-supported** (native only)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::{CanoError, CanoResult};

use super::lease::{Coordinator, Due};
use super::loops::{
    abandon, acquire, activate, cron_loop, every_loop, reserve_local, run_reserved,
};
use super::{FlowInfo, FlowRuntime, ParsedSchedule, Status};

/// Live handle to a started [`Scheduler`](crate::scheduler::Scheduler).
///
/// **Availability:** `scheduler` feature (`since 0.6.0`) · **not wasm-supported** (native only)
///
/// Cheap to clone; every clone controls the same scheduler. Dropping the last
/// clone without calling [`stop`](Self::stop) cancels every in-flight run and
/// aborts the scheduler's tasks; resources are **not** torn down on that path —
/// call `stop().await` for an orderly shutdown.
#[derive(Clone)]
pub struct RunningScheduler {
    inner: Arc<Inner>,
}

struct Inner {
    flows: HashMap<Arc<str>, Arc<FlowRuntime>>,
    /// Registration order: `list` order, reverse teardown order.
    order: Vec<Arc<str>>,
    /// "Stop dispatching": loops exit at their next check, `trigger` is rejected.
    shutdown: CancellationToken,
    /// Parent of every run token; fired by the cancel phase of `stop`.
    cancel_runs: CancellationToken,
    /// Every loop task and every triggered run.
    tasks: parking_lot::Mutex<JoinSet<()>>,
    /// The shutdown result, computed once by the first `stop` that completes.
    result: tokio::sync::OnceCell<CanoResult<()>>,
    drain_timeout: Duration,
    cancel_timeout: Duration,
    instance_id: Arc<str>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Abort tasks before firing `cancel_runs` so dropping the last handle without
        // `stop()` never races a cooperative cancel-release on another worker thread.
        self.tasks.get_mut().abort_all();
        self.shutdown.cancel();
        self.cancel_runs.cancel();
    }
}

impl RunningScheduler {
    /// Build the handle and spawn one loop per `Every` / `Cron` flow.
    pub(super) fn launch(
        flows: Vec<(Arc<str>, FlowRuntime)>,
        instance_id: Arc<str>,
        coordinator: Option<Arc<Coordinator>>,
        drain_timeout: Duration,
        cancel_timeout: Duration,
    ) -> Self {
        let shutdown = CancellationToken::new();
        let cancel_runs = CancellationToken::new();
        let mut tasks = JoinSet::new();
        let mut map = HashMap::with_capacity(flows.len());
        let mut order = Vec::with_capacity(flows.len());
        for (id, mut rt) in flows {
            rt.coordinator.clone_from(&coordinator);
            let rt = Arc::new(rt);
            match &rt.schedule {
                ParsedSchedule::Every(interval) => {
                    tasks.spawn(every_loop(
                        Arc::clone(&rt),
                        *interval,
                        shutdown.clone(),
                        cancel_runs.clone(),
                    ));
                }
                ParsedSchedule::Cron(schedule) => {
                    tasks.spawn(cron_loop(
                        Arc::clone(&rt),
                        schedule.clone(),
                        shutdown.clone(),
                        cancel_runs.clone(),
                    ));
                }
                ParsedSchedule::Manual => {}
            }
            map.insert(Arc::clone(&id), rt);
            order.push(id);
        }
        Self {
            inner: Arc::new(Inner {
                flows: map,
                order,
                shutdown,
                cancel_runs,
                tasks: parking_lot::Mutex::new(tasks),
                result: tokio::sync::OnceCell::new(),
                drain_timeout,
                cancel_timeout,
                instance_id,
            }),
        }
    }

    /// Stop the scheduler and tear its resources down.
    ///
    /// 1. **Drain** — stop dispatching and wait up to the drain timeout
    ///    ([`Scheduler::with_drain_timeout`](crate::scheduler::Scheduler::with_drain_timeout),
    ///    default 20 s) for in-flight runs to finish on their own.
    /// 2. **Cancel** — fire every in-flight run's
    ///    [`CancellationToken`](crate::CancellationToken) and wait up to the cancel timeout
    ///    ([`Scheduler::with_cancel_timeout`](crate::scheduler::Scheduler::with_cancel_timeout),
    ///    default 5 s). Cancelled runs drain their saga compensation stacks.
    /// 3. **Abort** — drop whatever is still running, return those flows to
    ///    [`Status::Idle`], and report them in the error. With a
    ///    [store](crate::scheduler::Scheduler::with_store) an aborted run cannot release its
    ///    lease: the lease expires on its own, and another instance then counts the run
    ///    as a failure. Runs that end in the drain or cancel phase release theirs.
    /// 4. **Teardown** — every flow's resources, in reverse registration order.
    ///
    /// All flows go through each phase together, so the worst case is the two
    /// timeouts plus teardown regardless of the flow count.
    ///
    /// Idempotent: later calls (from any clone) return the first result. To put a
    /// hard cap on the whole sequence, wrap it: `tokio::time::timeout(d, running.stop())`.
    /// If that future is dropped mid-sequence, the remaining tasks are aborted; a later
    /// `stop()` runs the sequence again on the now-empty task set and still tears the
    /// resources down.
    ///
    /// # Errors
    ///
    /// [`CanoError::Timeout`] naming the flows the abort phase had to drop.
    pub async fn stop(&self) -> CanoResult<()> {
        self.inner
            .result
            .get_or_init(|| shutdown_sequence(&self.inner))
            .await
            .clone()
    }

    /// Wait until some clone calls [`stop`](Self::stop), then return its result.
    ///
    /// Blocks forever if nothing ever calls `stop`.
    pub async fn wait(&self) -> CanoResult<()> {
        self.inner.shutdown.cancelled().await;
        self.stop().await
    }

    /// This instance's id: the one set with
    /// [`Scheduler::with_instance_id`](crate::scheduler::Scheduler::with_instance_id), or the
    /// random default. With a store it is the owner name in leases and in
    /// [`Status::RunningElsewhere`].
    pub fn instance_id(&self) -> &str {
        &self.inner.instance_id
    }

    /// Wait until flow `id` is not [`Status::Running`] and return its info, or `None`
    /// for an unknown id. Returns immediately when the flow is not running.
    pub async fn wait_idle(&self, id: &str) -> Option<FlowInfo> {
        let rt = self.inner.flows.get(id)?;
        let mut rx = rt.cell.subscribe();
        rx.wait_for(|cell| cell.info.status != Status::Running)
            .await
            .ok()?;
        Some(flow_info(rt))
    }

    /// Start a run of flow `id` now.
    ///
    /// Manual triggers **bypass** an active [`Status::Backoff`] window but are
    /// **rejected** while the flow is [`Status::Tripped`] (call
    /// [`reset_flow`](Self::reset_flow) first). Must be called inside a Tokio runtime:
    /// the run is spawned, and this call returns once it has started.
    ///
    /// With a [store](crate::scheduler::Scheduler::with_store) the trigger first claims the
    /// flow in the shared record, so it is also rejected while another instance runs it
    /// (the error names that instance) and a store failure is returned as the error. The
    /// claim counts as a run on the record.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`]: `id` is unknown, the scheduler is stopping or stopped,
    ///   the flow is already running (here or, with a store, on another instance), or the
    ///   flow is tripped.
    /// - [`CanoError::SchedulerStore`]: the store failed or holds an undecodable record.
    pub async fn trigger(&self, id: &str) -> CanoResult<()> {
        let rt = self.runtime(id)?;
        if rt.coordinator.is_none() {
            // Holding the lock across the shutdown check and the spawn is what lets
            // `stop`'s `mem::take` see every run spawned before shutdown.
            let mut tasks = self.inner.tasks.lock();
            if self.inner.shutdown.is_cancelled() {
                return Err(Self::stopped());
            }
            while tasks.try_join_next().is_some() {}
            let reserved = reserve_local(rt, &self.inner.cancel_runs)
                .map_err(|refused| refused.into_error(id))?;
            tasks.spawn(run_reserved(Arc::clone(rt), reserved));
            return Ok(());
        }

        if self.inner.shutdown.is_cancelled() {
            return Err(Self::stopped());
        }
        let reserved = acquire(rt, &self.inner.cancel_runs, Due::Trigger, None)
            .await
            .map_err(|refused| refused.into_error(id))?
            .expect("trigger does not pass a shutdown token to acquire");
        let late = {
            let mut tasks = self.inner.tasks.lock();
            if self.inner.shutdown.is_cancelled() {
                Some(reserved)
            } else {
                while tasks.try_join_next().is_some() {}
                activate(rt, &reserved);
                tasks.spawn(run_reserved(Arc::clone(rt), reserved));
                None
            }
        };
        if let Some(reserved) = late {
            // `stop` began while the claim was in flight: hand the lease back.
            abandon(rt, reserved).await;
            return Err(Self::stopped());
        }
        Ok(())
    }

    /// Cooperatively cancel flow `id`'s in-flight run **on this instance**.
    ///
    /// The run's [`CancellationToken`](crate::CancellationToken) fires: the workflow
    /// stops at its next await point, drains its saga compensation stack, and the flow
    /// returns to [`Status::Idle`] — a deliberate cancel is not counted against the
    /// [`BackoffPolicy`](crate::scheduler::BackoffPolicy). A no-op when the flow is
    /// idle here. Use [`wait_idle`](Self::wait_idle) to wait for the run to end. Must be
    /// called inside a Tokio runtime.
    ///
    /// With a [store](crate::scheduler::Scheduler::with_store) this does not reach a run
    /// on another instance (the flow shows [`Status::RunningElsewhere`] here): call it on
    /// the instance that runs the flow.
    ///
    /// # Errors
    ///
    /// [`CanoError::Workflow`] — `id` is unknown.
    pub fn cancel_flow(&self, id: &str) -> CanoResult<()> {
        let rt = self.runtime(id)?;
        if let Some(token) = &rt.cell.borrow().run_token {
            token.cancel();
        }
        Ok(())
    }

    /// Clear flow `id`'s failure streak and backoff window; a tripped (or otherwise
    /// non-running) flow returns to [`Status::Idle`]. Must be called inside a Tokio
    /// runtime.
    ///
    /// If the flow is mid-run and that run fails, the streak restarts at `1` — call
    /// this after observing [`Status::Tripped`] or [`Status::Idle`] to avoid the surprise.
    ///
    /// With a [store](crate::scheduler::Scheduler::with_store) the reset is written to the
    /// shared record, so it applies to every instance: a flow tripped on one instance runs
    /// again on all of them. Expired leases are cleared, while a live lease held by an
    /// in-flight run remains intact.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`]: `id` is unknown.
    /// - [`CanoError::SchedulerStore`]: the store failed or holds an undecodable record;
    ///   nothing was reset.
    pub async fn reset_flow(&self, id: &str) -> CanoResult<()> {
        let rt = self.runtime(id)?;
        if let Some(coord) = &rt.coordinator {
            let record = coord.reset(id).await?;
            rt.cell.send_modify(|cell| coord.mirror(cell, &record));
            return Ok(());
        }
        rt.cell.send_modify(|cell| {
            cell.info.failure_streak = 0;
            cell.info.next_eligible = None;
            cell.last_error = None;
            if cell.info.status != Status::Running {
                cell.info.status = Status::Idle;
            }
        });
        Ok(())
    }

    /// Snapshot of flow `id`, or `None` for an unknown id.
    ///
    /// With a [store](crate::scheduler::Scheduler::with_store) the counters and status
    /// reflect the shared record as this instance last read it (see
    /// [`Status::RunningElsewhere`]); once an observed remote lease expires, the status
    /// falls back to the mirrored non-lease state.
    pub fn status(&self, id: &str) -> Option<FlowInfo> {
        self.inner.flows.get(id).map(|rt| flow_info(rt))
    }

    /// Snapshot of every flow, in registration order.
    pub fn list(&self) -> Vec<FlowInfo> {
        self.inner
            .order
            .iter()
            .map(|id| flow_info(&self.inner.flows[id]))
            .collect()
    }

    fn runtime(&self, id: &str) -> CanoResult<&Arc<FlowRuntime>> {
        self.inner
            .flows
            .get(id)
            .ok_or_else(|| CanoError::workflow(format!("No workflow registered with id '{id}'")))
    }

    fn stopped() -> CanoError {
        CanoError::workflow("Scheduler is stopping or stopped")
    }
}

/// Snapshot `rt`'s public `FlowInfo`, normalizing an already-expired
/// `RunningElsewhere` status back to the underlying mirrored record state.
fn flow_info(rt: &FlowRuntime) -> FlowInfo {
    let cell = rt.cell.borrow();
    let mut info = cell.info.clone();
    if let (Some(coord), Status::RunningElsewhere { lease_expires, .. }) =
        (&rt.coordinator, &info.status)
    {
        let now = chrono::Utc::now();
        if *lease_expires <= now {
            info.status = coord.normalized_status(&cell, now);
        }
    }
    info
}

/// Drain → cancel → abort → teardown. See [`RunningScheduler::stop`].
async fn shutdown_sequence(inner: &Inner) -> CanoResult<()> {
    async fn join_all(tasks: &mut JoinSet<()>) {
        while tasks.join_next().await.is_some() {}
    }

    inner.shutdown.cancel();
    let mut tasks = std::mem::take(&mut *inner.tasks.lock());

    let drained = tokio::time::timeout(inner.drain_timeout, join_all(&mut tasks))
        .await
        .is_ok();
    if !drained {
        inner.cancel_runs.cancel();
        let cancelled = tokio::time::timeout(inner.cancel_timeout, join_all(&mut tasks))
            .await
            .is_ok();
        if !cancelled {
            tasks.shutdown().await;
        }
    }

    let mut aborted: Vec<&str> = Vec::new();
    for id in &inner.order {
        let mut was_aborted = false;
        inner.flows[id].cell.send_if_modified(|cell| {
            let running = cell.info.status == Status::Running;
            was_aborted = running || cell.aborted;
            if !running && !cell.claiming {
                return false;
            }
            cell.claiming = false;
            cell.info.status = Status::Idle;
            cell.run_token = None;
            cell.aborted = true;
            true
        });
        if was_aborted {
            aborted.push(id);
        }
    }
    let result = if aborted.is_empty() {
        Ok(())
    } else {
        Err(CanoError::timeout(format!(
            "scheduler shutdown aborted {} in-flight flow(s): {}",
            aborted.len(),
            aborted.join(", ")
        )))
    };

    for id in inner.order.iter().rev() {
        let rt = &inner.flows[id];
        if !rt.torn_down.swap(true, std::sync::atomic::Ordering::AcqRel) {
            rt.flow.teardown().await;
        }
    }
    result
}

impl std::fmt::Debug for RunningScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningScheduler")
            .field("flows", &self.inner.order)
            .field("stopping", &self.inner.shutdown.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::test_support::*;
    use crate::scheduler::{BackoffPolicy, Scheduler};
    use crate::task;
    use crate::task::Task;
    use crate::workflow::Workflow;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::time::{Duration, sleep};

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_rejects_misconfigured_workflow() {
        // A workflow with no exit states fails `validate()`. The scheduler must surface
        // that error from `start()` rather than running setup and failing later at the
        // first scheduled execution.
        let scheduler = Scheduler::new();
        let bad_workflow: Workflow<TestState> =
            Workflow::bare().register(TestState::Start, TestTask::new());

        let scheduler = scheduler.every(
            "bad",
            bad_workflow,
            TestState::Start,
            Duration::from_secs(60),
        );

        let err = scheduler.start().await.expect_err("start should reject");
        assert!(matches!(err, CanoError::Configuration(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_rejects_unregistered_initial_state() {
        // Initial state isn't registered or an exit state — `validate_initial_state`
        // must catch this at start, before any resource setup runs.
        let scheduler = Scheduler::new();
        let workflow: Workflow<TestState> = Workflow::bare()
            .register(TestState::Start, TestTask::new())
            .add_exit_state(TestState::Complete);

        let scheduler = scheduler.every(
            "bad_init",
            workflow,
            TestState::Error,
            Duration::from_secs(60),
        );

        let err = scheduler.start().await.expect_err("start should reject");
        assert!(matches!(err, CanoError::Configuration(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stop_interrupts_long_interval_sleep() {
        // Register an Every-loop workflow with a 1-hour interval. After start(),
        // the loop parks on `sleep(1 hour)`. Stop must wake the sleep via the
        // notify so the scheduler returns within seconds, not hours.
        let scheduler = Scheduler::new();
        let workflow = create_test_workflow();
        let scheduler = scheduler.every(
            "long",
            workflow,
            TestState::Start,
            Duration::from_secs(3600),
        );

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
        let scheduler = Scheduler::new();
        let workflow = create_test_workflow();
        let scheduler = scheduler.cron("daily", workflow, TestState::Start, "0 0 0 * * *");

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
    async fn panicking_workflow_does_not_strand_status_running() {
        // A panic in a path the FSM doesn't catch (e.g. a user observer that
        // panics) must not unwind out of the spawned task — that would skip
        // `apply_outcome` and leave `Status::Running` set forever, blocking
        // every subsequent `trigger` with `AlreadyRunning`. The catch_unwind
        // inside `execute_reserved_flow` converts the panic into an `Err` so
        // the status flips to a recoverable state and the BackoffPolicy
        // applies.
        use crate::observer::WorkflowObserver;
        use std::sync::Arc;

        struct PanickyObserver;
        impl WorkflowObserver for PanickyObserver {
            fn on_state_enter(&self, _state: &str) {
                panic!("observer panic — must not strand Status::Running");
            }
        }

        let scheduler = Scheduler::new();
        let workflow = Workflow::bare()
            .register(TestState::Start, TestTask::new())
            .add_exit_state(TestState::Complete)
            .with_observer(Arc::new(PanickyObserver));
        let scheduler = scheduler.manual("panicky", workflow, TestState::Start);
        let running = scheduler.start().await.unwrap();

        // First trigger — the observer panics.
        let _ = running.trigger("panicky").await;
        // Wait for the spawned task to finish and apply_outcome to run.
        sleep(Duration::from_millis(200)).await;

        let st = running.status("panicky").unwrap();
        assert!(
            !matches!(st.status, Status::Running),
            "Status::Running must not be left stranded after a panic — got {:?}",
            st.status
        );

        // A second trigger must NOT be rejected with `AlreadyRunning`.
        let second = running.trigger("panicky").await;
        let err_msg = second
            .err()
            .map(|e| e.message().to_string())
            .unwrap_or_default();
        assert!(
            !err_msg.contains("already running"),
            "second trigger should not be blocked by stranded Running: {err_msg}"
        );

        running.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_manual_trigger() {
        let timeout = Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            let scheduler = Scheduler::new();
            let workflow = create_test_workflow();
            let scheduler = scheduler.manual("test_task", workflow, TestState::Start);

            let running = scheduler.start().await.unwrap();

            // Trigger the workflow and wait for execution.
            running.trigger("test_task").await.unwrap();
            sleep(Duration::from_millis(100)).await;

            let status = running.status("test_task");
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
            let scheduler = Scheduler::new();
            let scheduler = scheduler.manual("shared", create_test_workflow(), TestState::Start);

            let running = scheduler.start().await.unwrap();
            let other = running.clone();

            other.trigger("shared").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            assert_eq!(running.status("shared").unwrap().run_count, 1);

            running.stop().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_status_check_post_start() {
        let scheduler = Scheduler::new();
        let workflow = create_test_workflow();
        let scheduler = scheduler.manual("test_task", workflow, TestState::Start);

        let running = scheduler.start().await.unwrap();
        let status = running.status("test_task").expect("must exist");

        assert_eq!(&*status.id, "test_task");
        assert_eq!(status.status, Status::Idle);
        assert_eq!(status.run_count, 0);
        assert!(status.last_run.is_none());

        running.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_list_workflows() {
        let scheduler = Scheduler::new();

        let scheduler = scheduler.manual("task1", create_test_workflow(), TestState::Start);
        let scheduler = scheduler.manual("task2", create_test_workflow(), TestState::Start);
        let scheduler = scheduler.manual("task3", create_test_workflow(), TestState::Start);
        let running = scheduler.start().await.unwrap();
        let ids: Vec<String> = running.list().iter().map(|f| f.id.to_string()).collect();
        assert_eq!(ids, ["task1", "task2", "task3"], "registration order");

        running.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_nonexistent_status() {
        let scheduler = Scheduler::new();
        let scheduler = scheduler.manual("known", create_test_workflow(), TestState::Start);
        let running = scheduler.start().await.unwrap();
        assert!(running.status("nonexistent").is_none());
        running.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_trigger_unknown_workflow_errors() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let scheduler = Scheduler::new();
            let scheduler = scheduler.manual("known", create_test_workflow(), TestState::Start);

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
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run(&self) -> Result<TestState, CanoError> {
                sleep(Duration::from_millis(300)).await;
                Ok(TestState::Complete)
            }
        }

        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let scheduler = Scheduler::new();
            let workflow = Workflow::bare()
                .register(TestState::Start, SlowTask)
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);
            let scheduler = scheduler.manual("slow", workflow, TestState::Start);

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

            let status = running.status("slow").unwrap();
            assert_eq!(status.run_count, 1);

            running.stop().await.unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_repeated_triggers_all_run() {
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let running = Scheduler::new()
                .manual("manual_task", create_test_workflow(), TestState::Start)
                .start()
                .await
                .unwrap();
            for _ in 0..20 {
                running.trigger("manual_task").await.unwrap();
                running.wait_idle("manual_task").await.unwrap();
            }
            assert_eq!(running.status("manual_task").unwrap().run_count, 20);
            running.stop().await.unwrap();
        })
        .await;

        assert!(result.is_ok(), "Test timed out");
    }

    // ----- Backoff / trip / reset tests -----

    /// Task that fails until its internal counter reaches `succeed_after`.
    /// Used to drive deterministic streak behavior in the tests below.
    #[derive(Clone)]
    struct FlakyTask {
        attempts: Arc<AtomicU32>,
        succeed_after: u32,
    }

    impl FlakyTask {
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

    #[task]
    impl Task<TestState> for FlakyTask {
        // Disable retries so each scheduler dispatch is a single attempt.
        // The scheduler's BackoffPolicy is the only retry layer under test.
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::new()
        }

        async fn run(&self) -> Result<TestState, CanoError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt >= self.succeed_after {
                Ok(TestState::Complete)
            } else {
                Err(CanoError::TaskExecution("flaky".to_string()))
            }
        }
    }

    fn flaky_workflow(task: FlakyTask) -> Workflow<TestState> {
        Workflow::bare()
            .register(TestState::Start, task)
            .add_exit_state(TestState::Complete)
            .add_exit_state(TestState::Error)
    }

    /// Poll `running.status(id)` until `pred(&snapshot)` holds or `budget` elapses,
    /// returning the last observed snapshot either way.
    ///
    /// Fixed `sleep`-then-assert races the scheduler's dispatch loop: a duration that's
    /// "surely long enough" on an idle machine can be too short once hundreds of other
    /// tests share the runtime's thread pool. Polling waits for the real condition and
    /// still fails loudly — the caller's outer `tokio::time::timeout` bounds the total
    /// test runtime regardless of what `budget` is passed here.
    async fn wait_for_status(
        running: &RunningScheduler,
        id: &str,
        budget: Duration,
        mut pred: impl FnMut(&FlowInfo) -> bool,
    ) -> FlowInfo {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let snap = running.status(id).unwrap();
            if pred(&snap) || tokio::time::Instant::now() >= deadline {
                return snap;
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_backoff_progression_extends_interval() {
        // Base interval (50ms) is small; backoff should grow above it. With
        // streak_limit None the loop keeps re-firing through the policy.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.every(
                "flaky",
                workflow,
                TestState::Start,
                Duration::from_millis(50),
            );
            let scheduler = scheduler.with_backoff(
                "flaky",
                BackoffPolicy {
                    initial: Duration::from_millis(150),
                    multiplier: 2.0,
                    max_delay: Duration::from_millis(600),
                    jitter: 0.0,
                    streak_limit: None,
                },
            );

            let running = scheduler.start().await.unwrap();

            // Wait long enough for: first immediate run (~0), then
            // wait max(50, 150) = 150ms for second run, then 300ms, then 600ms (capped).
            // Budget ~1.3s for >=4 runs.
            sleep(Duration::from_millis(1300)).await;
            let runs_observed = running.status("flaky").unwrap().run_count;
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
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.every(
                "trippy",
                workflow,
                TestState::Start,
                Duration::from_millis(40),
            );
            let scheduler = scheduler.with_backoff(
                "trippy",
                BackoffPolicy {
                    initial: Duration::from_millis(20),
                    multiplier: 1.0,
                    max_delay: Duration::from_millis(40),
                    jitter: 0.0,
                    streak_limit: Some(3),
                },
            );

            let running = scheduler.start().await.unwrap();

            // ~3 failures then trip. Budget includes the small backoff windows.
            sleep(Duration::from_millis(500)).await;

            let snap_before = running.status("trippy").unwrap();
            assert!(
                matches!(snap_before.status, Status::Tripped { streak: 3, .. }),
                "expected Tripped(streak=3), got: {:?}",
                snap_before.status
            );
            assert_eq!(snap_before.run_count, 3);

            // Wait long enough for several base intervals — run_count must stay 3.
            sleep(Duration::from_millis(300)).await;
            let snap_after = running.status("trippy").unwrap();
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
        let timeout = Duration::from_secs(10);
        let result = tokio::time::timeout(timeout, async {
            let scheduler = Scheduler::new();
            // Succeeds on attempt 5, after 4 failures. With streak_limit=2 it
            // trips on the 2nd failure; a reset clears state and the 3rd run
            // (the "first attempt after reset") is again a failure, but with
            // streak_limit=10 (we'll bump the policy via re-registration trick
            // — instead we just verify the reset clears the trip and advances).
            let workflow = flaky_workflow(FlakyTask::succeed_on_attempt(5));
            let scheduler = scheduler.every(
                "reset_me",
                workflow,
                TestState::Start,
                Duration::from_millis(40),
            );
            let scheduler = scheduler.with_backoff(
                "reset_me",
                BackoffPolicy {
                    initial: Duration::from_millis(20),
                    multiplier: 1.0,
                    max_delay: Duration::from_millis(20),
                    jitter: 0.0,
                    streak_limit: Some(2),
                },
            );

            let running = scheduler.start().await.unwrap();

            // Wait for trip.
            let snap = wait_for_status(&running, "reset_me", Duration::from_secs(3), |s| {
                matches!(s.status, Status::Tripped { .. })
            })
            .await;
            assert!(
                matches!(snap.status, Status::Tripped { .. }),
                "expected trip, got: {:?}",
                snap.status
            );
            assert_eq!(snap.run_count, 2);

            // Reset — flow resumes, hits another failure, trips again, then we
            // reset and let it succeed.
            running.reset_flow("reset_me").await.unwrap();
            let snap_after_reset = running.status("reset_me").unwrap();
            assert_eq!(snap_after_reset.failure_streak, 0);
            assert!(snap_after_reset.next_eligible.is_none());

            // Loop trips again (failures 3,4 → trip). Reset once more.
            wait_for_status(&running, "reset_me", Duration::from_secs(3), |s| {
                matches!(s.status, Status::Tripped { .. })
            })
            .await;
            running.reset_flow("reset_me").await.unwrap();

            // Attempt 5 succeeds → status Completed, streak 0.
            let snap_done = wait_for_status(&running, "reset_me", Duration::from_secs(3), |s| {
                s.status == Status::Completed
            })
            .await;
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
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::succeed_on_attempt(3));
            let scheduler = scheduler.every(
                "recover",
                workflow,
                TestState::Start,
                Duration::from_millis(30),
            );
            let scheduler = scheduler.with_backoff(
                "recover",
                BackoffPolicy {
                    initial: Duration::from_millis(30),
                    multiplier: 1.0,
                    max_delay: Duration::from_millis(30),
                    jitter: 0.0,
                    streak_limit: None,
                },
            );

            let running = scheduler.start().await.unwrap();

            let snap = wait_for_status(&running, "recover", Duration::from_secs(4), |s| {
                s.status == Status::Completed
            })
            .await;
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
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.manual("manual_flaky", workflow, TestState::Start);
            let scheduler = scheduler.with_backoff(
                "manual_flaky",
                BackoffPolicy {
                    initial: Duration::from_millis(10),
                    multiplier: 1.0,
                    max_delay: Duration::from_millis(10),
                    jitter: 0.0,
                    streak_limit: Some(2),
                },
            );

            let running = scheduler.start().await.unwrap();

            // Two failed manual runs trip the flow.
            running.trigger("manual_flaky").await.unwrap();
            running.wait_idle("manual_flaky").await.unwrap();
            running.trigger("manual_flaky").await.unwrap();
            running.wait_idle("manual_flaky").await.unwrap();

            let snap = running.status("manual_flaky").unwrap();
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
            let scheduler = Scheduler::new();
            let scheduler = scheduler.manual("known", create_test_workflow(), TestState::Start);

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
    async fn test_stop_interrupts_backoff_sleep() {
        // Failing flow with a long backoff window; stop() must wake the sleep
        // immediately, not wait for the backoff to elapse.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.every(
                "long_backoff",
                workflow,
                TestState::Start,
                Duration::from_millis(20),
            );
            let scheduler = scheduler.with_backoff(
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
            );

            let running = scheduler.start().await.unwrap();
            // Wait for first run + status update so the loop is parked on
            // the backoff window.
            sleep(Duration::from_millis(150)).await;
            assert!(matches!(
                running.status("long_backoff").unwrap().status,
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
    async fn test_default_policy_parks_failed_flow_in_backoff() {
        // A flow registered without `with_backoff` still gets BackoffPolicy::default(),
        // so a failure parks it in Backoff (never the removed Failed status) and
        // tracks the streak.
        let timeout = Duration::from_secs(3);
        let result = tokio::time::timeout(timeout, async {
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.every(
                "defaulted",
                workflow,
                TestState::Start,
                Duration::from_millis(40),
            );
            // Intentionally no with_backoff call — the default policy applies.

            let running = scheduler.start().await.unwrap();

            // Poll until we observe a non-Running status with at least one run.
            let snap = loop {
                sleep(Duration::from_millis(50)).await;
                let s = running.status("defaulted").unwrap();
                if !matches!(s.status, Status::Running) && s.run_count >= 1 {
                    break s;
                }
            };

            assert!(
                matches!(snap.status, Status::Backoff { .. }),
                "default-policy flow must park in Backoff, got: {:?}",
                snap.status
            );
            assert!(snap.failure_streak >= 1, "streak must be tracked");
            assert!(snap.next_eligible.is_some());

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
            let scheduler = Scheduler::new();
            // Flaky task that fails first then succeeds, so the manual
            // override can be observed advancing run_count past the failure.
            let workflow = flaky_workflow(FlakyTask::succeed_on_attempt(2));
            let scheduler = scheduler.manual("flow", workflow, TestState::Start);
            let scheduler = scheduler.with_backoff(
                "flow",
                BackoffPolicy {
                    // 60s park window — well outside this test's runtime.
                    initial: Duration::from_secs(60),
                    multiplier: 1.0,
                    max_delay: Duration::from_secs(60),
                    jitter: 0.0,
                    streak_limit: None,
                },
            );

            let running = scheduler.start().await.unwrap();

            // Trigger #1 fails, parks the flow in Backoff.
            running.trigger("flow").await.unwrap();
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = running.status("flow").unwrap();
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
                let s = running.status("flow").unwrap();
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
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.every(
                "flow",
                workflow,
                TestState::Start,
                Duration::from_millis(20),
            );
            let scheduler = scheduler.with_backoff(
                "flow",
                BackoffPolicy {
                    initial: Duration::from_millis(20),
                    multiplier: 1.0,
                    max_delay: Duration::from_millis(20),
                    jitter: 0.0,
                    streak_limit: Some(2),
                },
            );

            let running = scheduler.start().await.unwrap();

            // Wait for the first trip.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = running.status("flow").unwrap();
                if matches!(s.status, Status::Tripped { .. }) {
                    break s;
                }
            };
            let runs_at_first_trip = snap.run_count;
            assert_eq!(snap.failure_streak, 2, "tripped at streak limit");

            // Reset and confirm streak is cleared.
            running.reset_flow("flow").await.unwrap();
            let snap = running.status("flow").unwrap();
            assert_eq!(snap.failure_streak, 0);
            assert!(matches!(
                snap.status,
                Status::Idle | Status::Backoff { .. } | Status::Running
            ));

            // Wait for the second trip.
            let snap = loop {
                sleep(Duration::from_millis(20)).await;
                let s = running.status("flow").unwrap();
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
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.cron("flow", workflow, TestState::Start, "* * * * * *");
            let scheduler = scheduler.with_backoff(
                "flow",
                BackoffPolicy {
                    initial: Duration::from_secs(60),
                    multiplier: 1.0,
                    max_delay: Duration::from_secs(60),
                    jitter: 0.0,
                    streak_limit: None,
                },
            );

            let running = scheduler.start().await.unwrap();

            // Wait for the first run + backoff status.
            let snap = loop {
                sleep(Duration::from_millis(50)).await;
                let s = running.status("flow").unwrap();
                if matches!(s.status, Status::Backoff { .. }) {
                    break s;
                }
            };
            assert_eq!(snap.run_count, 1);

            // Sit through ~3 cron ticks (3s) and verify run_count holds steady.
            sleep(Duration::from_millis(3200)).await;
            let snap = running.status("flow").unwrap();
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
            let scheduler = Scheduler::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            let scheduler = scheduler.manual("flow", workflow, TestState::Start);
            let scheduler = scheduler.with_backoff(
                "flow",
                BackoffPolicy {
                    initial: Duration::from_millis(20),
                    multiplier: 1.0,
                    max_delay: Duration::from_millis(20),
                    jitter: 0.0,
                    streak_limit: None,
                },
            );

            let running = scheduler.start().await.unwrap();

            // Drive streak up to 3, then reset and trigger one more failure.
            for _ in 0..3 {
                running.trigger("flow").await.unwrap();
                loop {
                    sleep(Duration::from_millis(15)).await;
                    let s = running.status("flow").unwrap();
                    if matches!(s.status, Status::Backoff { .. }) {
                        break;
                    }
                }
            }
            assert_eq!(running.status("flow").unwrap().failure_streak, 3);

            running.reset_flow("flow").await.unwrap();
            assert_eq!(running.status("flow").unwrap().failure_streak, 0);

            // One more failure after the reset puts streak at 1, not 4.
            running.trigger("flow").await.unwrap();
            let snap = loop {
                sleep(Duration::from_millis(15)).await;
                let s = running.status("flow").unwrap();
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

    // A long-running, cancellable flow task that records when it starts and (if
    // never cancelled) when it completes — used to verify graceful shutdown
    // cooperatively cancels in-flight flows.
    #[derive(Clone)]
    struct CancellableSlow {
        started: std::sync::Arc<AtomicU32>,
        completed: std::sync::Arc<AtomicU32>,
    }
    #[task]
    impl Task<TestState> for CancellableSlow {
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::new()
        }
        async fn run(&self) -> Result<TestState, CanoError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_secs(30)).await;
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(TestState::Complete)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn zero_drain_stop_cancels_in_flight_flow() {
        // With no drain budget, `stop()` cancels a running flow instead of
        // waiting for it: it returns promptly (not after the task's 30s sleep)
        // and the task never reaches completion.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let started = std::sync::Arc::new(AtomicU32::new(0));
            let completed = std::sync::Arc::new(AtomicU32::new(0));
            let scheduler = Scheduler::new().with_drain_timeout(Duration::ZERO);
            let wf = Workflow::bare()
                .register(
                    TestState::Start,
                    CancellableSlow {
                        started: started.clone(),
                        completed: completed.clone(),
                    },
                )
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);
            let scheduler = scheduler.manual("slow", wf, TestState::Start);

            let running = scheduler.start().await.unwrap();
            running.trigger("slow").await.unwrap();
            // Wait until the flow is actually in flight.
            while started.load(Ordering::SeqCst) == 0 {
                sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(running.status("slow").unwrap().status, Status::Running);

            let t0 = std::time::Instant::now();
            running.stop().await.expect("graceful stop should succeed");
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "stop() must cancel the in-flight flow, not wait for its 30s sleep"
            );
            assert_eq!(
                completed.load(Ordering::SeqCst),
                0,
                "the in-flight flow must be cancelled, not run to completion"
            );
        })
        .await;
        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_flow_cancels_in_flight_run_and_returns_to_idle() {
        // `cancel_flow` cooperatively cancels the in-flight run; the flow returns
        // to Idle (a deliberate cancel is NOT a failure, so the streak stays 0 and
        // the flow does not trip) and the task never completes.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let started = std::sync::Arc::new(AtomicU32::new(0));
            let completed = std::sync::Arc::new(AtomicU32::new(0));
            let scheduler = Scheduler::new();
            let wf = Workflow::bare()
                .register(
                    TestState::Start,
                    CancellableSlow {
                        started: started.clone(),
                        completed: completed.clone(),
                    },
                )
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);
            let scheduler = scheduler.manual("slow", wf, TestState::Start);

            let running = scheduler.start().await.unwrap();
            running.trigger("slow").await.unwrap();
            while started.load(Ordering::SeqCst) == 0 {
                sleep(Duration::from_millis(5)).await;
            }

            running
                .cancel_flow("slow")
                .expect("cancel_flow should succeed");
            let info = running.wait_idle("slow").await.unwrap();
            assert_eq!(
                info.status,
                crate::scheduler::Status::Idle,
                "a cancelled run returns to Idle"
            );
            assert_eq!(info.failure_streak, 0, "cancel must not count as a failure");
            assert_eq!(
                completed.load(Ordering::SeqCst),
                0,
                "task was cancelled, not completed"
            );
            running.stop().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_flow_on_idle_flow_is_noop() {
        // Cancelling a registered flow that isn't running is an idempotent no-op.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let started = std::sync::Arc::new(AtomicU32::new(0));
            let completed = std::sync::Arc::new(AtomicU32::new(0));
            let scheduler = Scheduler::new();
            let wf = Workflow::bare()
                .register(
                    TestState::Start,
                    CancellableSlow {
                        started: started.clone(),
                        completed: completed.clone(),
                    },
                )
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);
            let scheduler = scheduler.manual("idle", wf, TestState::Start);

            let running = scheduler.start().await.unwrap();
            // Never triggered → no in-flight run → cancel is a no-op Ok.
            running
                .cancel_flow("idle")
                .expect("cancel on idle flow is a no-op");
            assert_eq!(
                running.status("idle").unwrap().status,
                crate::scheduler::Status::Idle
            );
            running.stop().await.unwrap();
        })
        .await;
        assert!(result.is_ok(), "Test timed out");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_flow_unknown_flow_errors() {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let started = std::sync::Arc::new(AtomicU32::new(0));
            let completed = std::sync::Arc::new(AtomicU32::new(0));
            let scheduler = Scheduler::new();
            let wf = Workflow::bare()
                .register(
                    TestState::Start,
                    CancellableSlow {
                        started: started.clone(),
                        completed: completed.clone(),
                    },
                )
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);
            let scheduler = scheduler.manual("known", wf, TestState::Start);

            let running = scheduler.start().await.unwrap();
            let err = running.cancel_flow("nope").unwrap_err();
            assert!(
                err.to_string().contains("No workflow registered"),
                "unknown flow must error, got: {err}"
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
        // via the task's own execution_count — once we drop the handle, the
        // count must freeze.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let task = TestTask::new();
            let count = Arc::clone(&task.execution_count);
            let workflow = Workflow::bare()
                .register(TestState::Start, task)
                .add_exit_state(TestState::Complete)
                .add_exit_state(TestState::Error);

            let scheduler = Scheduler::new();
            let scheduler = scheduler.every(
                    "ticker",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(20),
                );

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

    // ----- Bounded three-phase shutdown -----

    /// Sleeps `dur`, then sets `done` and completes. No retries.
    struct Sleeper {
        dur: Duration,
        done: Arc<std::sync::atomic::AtomicBool>,
    }

    #[task]
    impl Task<TestState> for Sleeper {
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::new()
        }
        async fn run(&self) -> Result<TestState, CanoError> {
            sleep(self.dur).await;
            self.done.store(true, Ordering::SeqCst);
            Ok(TestState::Complete)
        }
    }

    fn sleeper(dur: Duration) -> (Workflow<TestState>, Arc<std::sync::atomic::AtomicBool>) {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wf = Workflow::bare()
            .register(
                TestState::Start,
                Sleeper {
                    dur,
                    done: Arc::clone(&done),
                },
            )
            .add_exit_state(TestState::Complete);
        (wf, done)
    }

    /// A compensatable task whose forward run sleeps for a minute: the engine never
    /// interrupts it with a token, so only the abort phase can end it.
    struct Stubborn;

    #[crate::saga::task]
    impl crate::saga::CompensatableTask<TestState> for Stubborn {
        type Output = ();
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::new()
        }
        async fn run(
            &self,
            _res: &crate::resource::Resources,
        ) -> Result<(TestState, ()), CanoError> {
            sleep(Duration::from_secs(60)).await;
            Ok((TestState::Complete, ()))
        }
        async fn compensate(
            &self,
            _res: &crate::resource::Resources,
            _output: (),
        ) -> Result<(), CanoError> {
            Ok(())
        }
    }

    fn stubborn() -> Workflow<TestState> {
        Workflow::bare()
            .register(TestState::Start, Stubborn)
            .add_exit_state(TestState::Complete)
    }

    #[tokio::test(start_paused = true)]
    async fn stop_lets_in_flight_run_finish() {
        let (wf, done) = sleeper(Duration::from_millis(300));
        let running = Scheduler::new()
            .manual("f", wf, TestState::Start)
            .with_drain_timeout(Duration::from_secs(2))
            .start()
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        running.trigger("f").await.unwrap();
        running.stop().await.unwrap();
        assert!(
            done.load(Ordering::SeqCst),
            "the drain must let the run finish"
        );
        assert_eq!(running.status("f").unwrap().status, Status::Completed);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn stop_cancels_after_drain_timeout() {
        let (wf, done) = sleeper(Duration::from_secs(60));
        let running = Scheduler::new()
            .manual("f", wf, TestState::Start)
            .with_drain_timeout(Duration::from_millis(100))
            .with_cancel_timeout(Duration::from_secs(2))
            .start()
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        running.trigger("f").await.unwrap();
        running.stop().await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            !done.load(Ordering::SeqCst),
            "the run must have been cancelled"
        );
        assert_eq!(running.status("f").unwrap().status, Status::Idle);
        assert!(elapsed >= Duration::from_millis(100), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn stop_aborts_after_cancel_timeout() {
        let running = Scheduler::new()
            .manual("stubborn", stubborn(), TestState::Start)
            .with_drain_timeout(Duration::from_millis(100))
            .with_cancel_timeout(Duration::from_millis(200))
            .start()
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        running.trigger("stubborn").await.unwrap();
        let err = running.stop().await.expect_err("abort must be reported");
        assert_eq!(err.category(), "timeout");
        assert!(err.to_string().contains("stubborn"), "{err}");
        assert_eq!(running.status("stubborn").unwrap().status, Status::Idle);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn stop_budget_is_not_multiplied_by_flow_count() {
        let running = Scheduler::new()
            .manual("a", stubborn(), TestState::Start)
            .manual("b", stubborn(), TestState::Start)
            .manual("c", stubborn(), TestState::Start)
            .with_drain_timeout(Duration::from_millis(100))
            .with_cancel_timeout(Duration::from_millis(200))
            .start()
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        for id in ["a", "b", "c"] {
            running.trigger(id).await.unwrap();
        }
        let err = running.stop().await.expect_err("abort must be reported");
        assert!(err.to_string().contains("3 in-flight"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn trigger_after_stop_is_rejected() {
        let running = Scheduler::new()
            .manual("f", create_test_workflow(), TestState::Start)
            .start()
            .await
            .unwrap();
        running.stop().await.unwrap();
        let err = running.trigger("f").await.unwrap_err();
        assert!(
            err.to_string().contains("Scheduler is stopping or stopped"),
            "{err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_flow_then_wait_idle() {
        let (wf, done) = sleeper(Duration::from_secs(60));
        let running = Scheduler::new()
            .manual("f", wf, TestState::Start)
            .start()
            .await
            .unwrap();
        running.trigger("f").await.unwrap();
        running.cancel_flow("f").unwrap();
        let info = running.wait_idle("f").await.unwrap();
        assert_eq!(info.status, Status::Idle);
        assert!(!done.load(Ordering::SeqCst));
        running.stop().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn hosts_two_state_enums() {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        enum Other {
            Go,
            End,
        }
        struct OtherTask;
        #[task]
        impl Task<Other> for OtherTask {
            async fn run(&self) -> Result<Other, CanoError> {
                Ok(Other::End)
            }
        }
        let other = Workflow::bare()
            .register(Other::Go, OtherTask)
            .add_exit_state(Other::End);

        let running = Scheduler::new()
            .manual("test", create_test_workflow(), TestState::Start)
            .manual("other", other, Other::Go)
            .start()
            .await
            .unwrap();
        running.trigger("test").await.unwrap();
        running.trigger("other").await.unwrap();
        for id in ["test", "other"] {
            let info = running.wait_idle(id).await.unwrap();
            assert_eq!(info.status, Status::Completed, "{id}");
        }
        running.stop().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn teardown_runs_once_after_stop() {
        struct Counted(Arc<AtomicU32>);
        #[crate::resource]
        impl crate::resource::Resource for Counted {
            async fn teardown(&self) -> Result<(), CanoError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
        let teardowns = Arc::new(AtomicU32::new(0));
        let wf = Workflow::new(
            crate::resource::Resources::new().insert(Counted(Arc::clone(&teardowns))),
        )
        .register(TestState::Start, TestTask::new())
        .add_exit_state(TestState::Complete);

        let running = Scheduler::new()
            .manual("f", wf, TestState::Start)
            .start()
            .await
            .unwrap();
        running.stop().await.unwrap();
        running.stop().await.unwrap();
        assert_eq!(teardowns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_every_handle_aborts_runs() {
        let (wf, done) = sleeper(Duration::from_secs(1));
        let running = Scheduler::new()
            .manual("f", wf, TestState::Start)
            .start()
            .await
            .unwrap();
        running.trigger("f").await.unwrap();
        drop(running);
        sleep(Duration::from_secs(5)).await;
        assert!(
            !done.load(Ordering::SeqCst),
            "dropping the handle must abort the run"
        );
    }

    #[tokio::test]
    async fn start_rejects_workflow_with_suspend_task() {
        struct WaitHuman;
        #[crate::task::suspend(state = TestState)]
        impl WaitHuman {
            async fn suspend(&self) -> Result<(), CanoError> {
                Ok(())
            }
            async fn resume(&self, _input: String) -> Result<TestState, CanoError> {
                Ok(TestState::Complete)
            }
        }

        let store = Arc::new(crate::testing::InMemoryCheckpointStore::new());
        let wf = Workflow::bare()
            .with_checkpoint_store(store)
            .with_workflow_id("parked")
            .register(TestState::Start, WaitHuman)
            .add_exit_state(TestState::Complete);
        assert!(wf.validate().is_ok());

        let err = Scheduler::new()
            .manual("suspend-flow", wf, TestState::Start)
            .start()
            .await
            .expect_err("scheduler must reject workflows with suspend states");
        assert_eq!(err.category(), "configuration");
        assert!(err.to_string().contains("suspend"), "{err}");
    }

    #[tokio::test]
    async fn dropped_stop_mid_drain_resets_running_status_and_retry_stop_tears_down_once() {
        struct UncancellableTask {
            started: Arc<tokio::sync::Notify>,
        }
        #[task]
        impl Task<TestState> for UncancellableTask {
            async fn run(&self) -> Result<TestState, CanoError> {
                self.started.notify_one();
                std::future::pending::<()>().await;
                Ok(TestState::Complete)
            }
        }

        struct CountedTeardown(Arc<AtomicU32>);
        #[crate::resource]
        impl crate::resource::Resource for CountedTeardown {
            async fn teardown(&self) -> Result<(), CanoError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let started = Arc::new(tokio::sync::Notify::new());
        let teardowns = Arc::new(AtomicU32::new(0));
        let wf = Workflow::new(
            crate::resource::Resources::new().insert(CountedTeardown(Arc::clone(&teardowns))),
        )
        .register(
            TestState::Start,
            UncancellableTask {
                started: Arc::clone(&started),
            },
        )
        .add_exit_state(TestState::Complete);

        let running = Scheduler::new()
            .manual("stuck", wf, TestState::Start)
            .with_drain_timeout(Duration::from_secs(30))
            .with_cancel_timeout(Duration::from_secs(30))
            .start()
            .await
            .unwrap();
        running.trigger("stuck").await.unwrap();
        started.notified().await;

        // First `stop()` is wrapped in a short timeout and dropped mid-drain.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), running.stop())
                .await
                .is_err()
        );

        // `RunningGuard` must flip the aborted flow out of `Status::Running` so `wait_idle`
        // resolves immediately instead of hanging forever.
        let info = tokio::time::timeout(Duration::from_secs(1), running.wait_idle("stuck"))
            .await
            .expect("wait_idle must not hang after stop() was dropped")
            .unwrap();
        assert_eq!(info.status, Status::Idle);

        // Retrying `stop()` must report the aborted flow and tear down resources exactly once.
        let err = running
            .stop()
            .await
            .expect_err("second stop() must report the flow aborted when the first stop() dropped");
        assert!(err.to_string().contains("stuck"), "{err}");
        assert_eq!(teardowns.load(Ordering::SeqCst), 1);

        // Idempotent thereafter: no second teardown.
        let _ = running.stop().await;
        assert_eq!(teardowns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancelling_manual_trigger_during_backoff_preserves_backoff_status() {
        struct FailOrHang {
            hang: Arc<std::sync::atomic::AtomicBool>,
            started: Arc<tokio::sync::Notify>,
        }
        #[task]
        impl Task<TestState> for FailOrHang {
            async fn run(&self) -> Result<TestState, CanoError> {
                if self.hang.load(Ordering::SeqCst) {
                    self.started.notify_one();
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok(TestState::Complete)
                } else {
                    Err(CanoError::task_execution("initial failure"))
                }
            }
        }

        let hang = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = Arc::new(tokio::sync::Notify::new());
        let wf = Workflow::bare()
            .register(
                TestState::Start,
                FailOrHang {
                    hang: Arc::clone(&hang),
                    started: Arc::clone(&started),
                },
            )
            .add_exit_state(TestState::Complete);

        let running = Scheduler::new()
            .manual("f", wf, TestState::Start)
            .with_backoff(
                "f",
                BackoffPolicy {
                    initial: Duration::from_secs(120),
                    jitter: 0.0,
                    ..BackoffPolicy::default()
                },
            )
            .start()
            .await
            .unwrap();

        running.trigger("f").await.unwrap();
        let after_fail = running.wait_idle("f").await.unwrap();
        assert!(matches!(
            after_fail.status,
            Status::Backoff { streak: 1, .. }
        ));

        // Trigger manually while in backoff, then cancel that manual run.
        hang.store(true, Ordering::SeqCst);
        running.trigger("f").await.unwrap();
        started.notified().await;
        running.cancel_flow("f").unwrap();
        let after_cancel = running.wait_idle("f").await.unwrap();
        assert!(
            matches!(after_cancel.status, Status::Backoff { streak: 1, ref last_error, .. } if last_error.contains("initial failure")),
            "cancelling a manual trigger while next_eligible is still in the future must keep Status::Backoff, got {:?}",
            after_cancel.status
        );
        running.stop().await.unwrap();
    }
}
