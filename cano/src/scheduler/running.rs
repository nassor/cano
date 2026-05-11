//! `RunningScheduler` — the live, cloneable handle returned by [`Scheduler::start`](crate::scheduler::Scheduler::start).

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

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
    pub(super) flows: Arc<HashMap<String, Arc<RwLock<FlowInfo>>>>,
    pub(super) flow_order: Arc<Vec<String>>,
    /// Final shutdown result published by the driver task. Receivers loop
    /// `changed().await` until the value transitions to `Some(_)`.
    pub(super) result_rx: watch::Receiver<Option<CanoResult<()>>>,
    /// Shared per-flow loop handles so `Drop` (last-clone fallback) and the
    /// driver task can reach them.
    pub(super) scheduler_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::scheduler::test_support::*;
    use crate::scheduler::{BackoffPolicy, Scheduler};
    use crate::task;
    use crate::task::node::Node;
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

        #[task::node]
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

        #[task::node]
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

    #[task::node]
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
