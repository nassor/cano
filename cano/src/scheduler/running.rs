//! `RunningScheduler` — the live, cloneable handle returned by [`Scheduler::start`](crate::scheduler::Scheduler::start).

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};

use crate::error::{CanoError, CanoResult};

use super::{FlowInfo, SchedulerCommand, Status};

/// Live handle to a started [`Scheduler`](crate::scheduler::Scheduler).
///
/// Returned by [`Scheduler::start`](crate::scheduler::Scheduler::start). Cheap to clone — every clone shares the
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
    pub(super) command_tx: mpsc::Sender<SchedulerCommand>,
    /// Read-only flow info registry. Cheap clones (Arc) for `status` / `list`
    /// / `has_running_flows`. Built once at start time; never mutated.
    pub(super) flows: Arc<HashMap<Arc<str>, Arc<RwLock<FlowInfo>>>>,
    pub(super) flow_order: Arc<Vec<Arc<str>>>,
    /// Final shutdown result published by the driver task. Receivers loop
    /// `changed().await` until the value transitions to `Some(_)`.
    pub(super) result_rx: watch::Receiver<Option<CanoResult<()>>>,
    /// Shared per-flow loop handles so `Drop` (last-clone fallback) and the
    /// driver task can reach them.
    pub(super) scheduler_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
    /// `AbortHandle` for the per-flow task the driver is currently awaiting
    /// in its drain phase. `None` whenever the driver isn't between pop+await
    /// of a `scheduler_tasks` handle. `Drop` reads this to abort a wedged
    /// task that the driver already popped out of `scheduler_tasks` — without
    /// it the popped handle is local to the driver, so the iteration in
    /// `Drop` cannot reach it and aborting `driver_handle` only detaches the
    /// underlying spawned task instead of aborting it.
    pub(super) in_flight_drain: Arc<RwLock<Option<AbortHandle>>>,
    /// JoinHandle of the driver task. Wrapped in Arc so multiple clones can
    /// observe `is_finished()` and the last-clone Drop can `abort()`.
    pub(super) driver_handle: Arc<JoinHandle<()>>,
    /// Strong-count sentinel for the user-visible clone count. Ignores the
    /// Arc references held by the spawned driver task so the last user-clone
    /// drop reliably triggers fallback abort.
    ///
    /// Field is named (not `_liveness`) so clippy does not warn about an
    /// unused field; it is read via `Arc::strong_count` in `Drop`.
    pub(super) liveness: Arc<()>,
    /// Generics are anchored on the driver task (which owns the workflows
    /// HashMap); the handle itself only sees `FlowInfo`. PhantomData keeps
    /// `TState` / `TResourceKey` in the type signature so a `RunningScheduler`
    /// for one workflow set isn't accidentally interchangeable with another.
    pub(super) _marker: std::marker::PhantomData<fn() -> (TState, TResourceKey)>,
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
                id: Arc::from(id),
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
                id: Arc::from(id),
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

    /// Request cooperative cancellation of a flow's in-flight run.
    ///
    /// Fires the run's [`CancellationToken`](crate::cancel::CancellationToken):
    /// the in-flight workflow aborts at its next await point, drains its saga
    /// compensation stack, and returns [`CanoError::Cancelled`]. The flow then
    /// returns to [`Status::Idle`](crate::scheduler::Status::Idle) — a deliberate
    /// cancel is **not** counted as a failure against the [`BackoffPolicy`](crate::scheduler::BackoffPolicy),
    /// so the next scheduled run fires normally.
    ///
    /// A **no-op** (returns `Ok`) when the flow exists but isn't currently
    /// running. Graceful [`stop`](Self::stop) cancels every in-flight flow this
    /// same way before draining.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Workflow`] — the scheduler is not running, `id` is unknown,
    ///   or the command queue is full.
    pub async fn cancel_flow(&self, id: &str) -> CanoResult<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .try_send(SchedulerCommand::Cancel {
                id: Arc::from(id),
                response: response_tx,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Closed(_) => CanoError::Workflow(
                    "Scheduler not running — call start() before cancel_flow()".to_string(),
                ),
                mpsc::error::TrySendError::Full(_) => {
                    CanoError::Workflow("Scheduler command queue full".to_string())
                }
            })?;

        response_rx.await.map_err(|_| {
            CanoError::Workflow("Scheduler stopped before cancel was processed".to_string())
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
        // Reach the popped-and-being-awaited handle the driver removed from
        // `scheduler_tasks` before starting its `.await`. Without this, the
        // popped JoinHandle is dropped (detached) when the driver future is
        // cancelled by the abort above, and the underlying spawned task
        // leaks.
        if let Ok(slot) = self.in_flight_drain.try_write()
            && let Some(abort) = slot.as_ref()
        {
            abort.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::test_support::*;
    use crate::scheduler::{BackoffPolicy, Scheduler};
    use crate::task;
    use crate::task::{Task, TaskResult};
    use crate::workflow::Workflow;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::time::{Duration, sleep};

    #[tokio::test(flavor = "multi_thread")]
    async fn test_start_rejects_misconfigured_workflow() {
        // A workflow with no exit states fails `validate()`. The scheduler must surface
        // that error from `start()` rather than running setup and failing later at the
        // first scheduled execution.
        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let bad_workflow: Workflow<TestState> =
            Workflow::bare().register(TestState::Start, TestTask::new());

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
            .register(TestState::Start, TestTask::new())
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

        let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
        let workflow = Workflow::bare()
            .register(TestState::Start, TestTask::new())
            .add_exit_state(TestState::Complete)
            .with_observer(Arc::new(PanickyObserver));
        scheduler
            .manual("panicky", workflow, TestState::Start)
            .unwrap();
        let running = scheduler.start().await.unwrap();

        // First trigger — the observer panics.
        let _ = running.trigger("panicky").await;
        // Wait for the spawned task to finish and apply_outcome to run.
        sleep(Duration::from_millis(200)).await;

        let st = running.status("panicky").await.unwrap();
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

        assert_eq!(&*status.id, "test_task");
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
        let ids: Vec<&str> = flows.iter().map(|f| f.id.as_ref()).collect();
        assert!(ids.contains(&"task1"));
        assert!(ids.contains(&"task2"));
        assert!(ids.contains(&"task3"));

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
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                sleep(Duration::from_millis(300)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = Workflow::bare()
                .register(TestState::Start, SlowTask)
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
            crate::task::TaskConfig::minimal()
        }

        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt >= self.succeed_after {
                Ok(TaskResult::Single(TestState::Complete))
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_backoff_progression_extends_interval() {
        // Base interval (50ms) is small; backoff should grow above it. With
        // streak_limit None the loop keeps re-firing through the policy.
        let timeout = Duration::from_secs(5);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
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
            let workflow = flaky_workflow(FlakyTask::always_failing());
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
            let workflow = flaky_workflow(FlakyTask::succeed_on_attempt(5));
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
            let workflow = flaky_workflow(FlakyTask::succeed_on_attempt(3));
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
            let workflow = flaky_workflow(FlakyTask::always_failing());
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
            let workflow = flaky_workflow(FlakyTask::always_failing());
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
    async fn test_default_policy_parks_failed_flow_in_backoff() {
        // A flow registered without `set_backoff` still gets BackoffPolicy::default(),
        // so a failure parks it in Backoff (never the removed Failed status) and
        // tracks the streak.
        let timeout = Duration::from_secs(3);
        let result = tokio::time::timeout(timeout, async {
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            let workflow = flaky_workflow(FlakyTask::always_failing());
            scheduler
                .every(
                    "defaulted",
                    workflow,
                    TestState::Start,
                    Duration::from_millis(40),
                )
                .unwrap();
            // Intentionally no set_backoff call — the default policy applies.

            let running = scheduler.start().await.unwrap();

            // Poll until we observe a non-Running status with at least one run.
            let snap = loop {
                sleep(Duration::from_millis(50)).await;
                let s = running.status("defaulted").await.unwrap();
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
            let mut scheduler: Scheduler<TestState> = Scheduler::<TestState>::new();
            // Flaky task that fails first then succeeds, so the manual
            // override can be observed advancing run_count past the failure.
            let workflow = flaky_workflow(FlakyTask::succeed_on_attempt(2));
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
            let workflow = flaky_workflow(FlakyTask::always_failing());
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
            let workflow = flaky_workflow(FlakyTask::always_failing());
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
            let workflow = flaky_workflow(FlakyTask::always_failing());
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
            crate::task::TaskConfig::minimal()
        }
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_secs(30)).await;
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(TaskResult::Single(TestState::Complete))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn graceful_stop_cancels_in_flight_flow() {
        // Graceful shutdown cooperatively cancels a running flow instead of
        // blocking until it finishes: `stop()` returns promptly (not after the
        // task's 30s sleep) and the task never reaches completion.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let started = std::sync::Arc::new(AtomicU32::new(0));
            let completed = std::sync::Arc::new(AtomicU32::new(0));
            let mut scheduler: Scheduler<TestState> = Scheduler::new();
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
            scheduler.manual("slow", wf, TestState::Start).unwrap();

            let running = scheduler.start().await.unwrap();
            running.trigger("slow").await.unwrap();
            // Wait until the flow is actually in flight.
            while started.load(Ordering::SeqCst) == 0 {
                sleep(Duration::from_millis(5)).await;
            }
            assert!(running.has_running_flows().await);

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
    async fn trigger_after_graceful_stop_reports_not_running() {
        // Once graceful shutdown has run, the command channel is closed, so a
        // subsequent trigger() reports "not running" rather than enqueueing.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let started = std::sync::Arc::new(AtomicU32::new(0));
            let completed = std::sync::Arc::new(AtomicU32::new(0));
            let mut scheduler: Scheduler<TestState> = Scheduler::new();
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
            scheduler.manual("slow", wf, TestState::Start).unwrap();

            let running = scheduler.start().await.unwrap();
            running.trigger("slow").await.unwrap();
            while started.load(Ordering::SeqCst) == 0 {
                sleep(Duration::from_millis(5)).await;
            }
            running.stop().await.expect("graceful stop should succeed");

            let err = running.trigger("slow").await.unwrap_err();
            assert!(
                err.to_string().contains("Scheduler not running"),
                "trigger after shutdown must report not-running, got: {err}"
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
            let mut scheduler: Scheduler<TestState> = Scheduler::new();
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
            scheduler.manual("slow", wf, TestState::Start).unwrap();

            let running = scheduler.start().await.unwrap();
            running.trigger("slow").await.unwrap();
            while started.load(Ordering::SeqCst) == 0 {
                sleep(Duration::from_millis(5)).await;
            }

            running
                .cancel_flow("slow")
                .await
                .expect("cancel_flow should succeed");

            // Wait for the cancelled run's apply_outcome to settle the status.
            loop {
                let st = running.status("slow").await.unwrap().status;
                if st != crate::scheduler::Status::Running {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
            let info = running.status("slow").await.unwrap();
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
            let mut scheduler: Scheduler<TestState> = Scheduler::new();
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
            scheduler.manual("idle", wf, TestState::Start).unwrap();

            let running = scheduler.start().await.unwrap();
            // Never triggered → no in-flight run → cancel is a no-op Ok.
            running
                .cancel_flow("idle")
                .await
                .expect("cancel on idle flow is a no-op");
            assert_eq!(
                running.status("idle").await.unwrap().status,
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
            let mut scheduler: Scheduler<TestState> = Scheduler::new();
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
            scheduler.manual("known", wf, TestState::Start).unwrap();

            let running = scheduler.start().await.unwrap();
            let err = running.cancel_flow("nope").await.unwrap_err();
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
