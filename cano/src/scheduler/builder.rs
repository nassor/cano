//! The `Scheduler` builder — register workflows, then `start()` to obtain a
//! [`RunningScheduler`].

use std::str::FromStr;
use std::sync::Arc;

use cron::Schedule as CronSchedule;
use tokio::time::Duration;

use crate::error::{CanoError, CanoResult};
use crate::workflow::Workflow;

use super::{
    BackoffPolicy, FlowCell, FlowInfo, FlowRuntime, ParsedSchedule, RunningScheduler, Status,
    WorkflowFlow,
};

/// Scheduler builder: register workflows, then [`start`](Self::start).
///
/// Every method consumes and returns the builder, so registration is one chain.
/// One scheduler can host workflows with different state (and resource) types.
/// Registration problems (a zero interval, a bad cron expression, a duplicate id,
/// a backoff override for an unknown id) are remembered and returned by `start`
/// before anything is validated or set up; the first one wins.
///
/// ```rust
/// # use cano::prelude::*;
/// # use std::time::Duration;
/// # #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// # enum Step { Start, Done }
/// # struct Noop;
/// # #[task]
/// # impl Task<Step> for Noop {
/// #     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
/// #         Ok(TaskResult::Single(Step::Done))
/// #     }
/// # }
/// # #[tokio::main]
/// # async fn main() -> CanoResult<()> {
/// let workflow = Workflow::bare().register(Step::Start, Noop).add_exit_state(Step::Done);
/// let running = Scheduler::new()
///     .every("tick", workflow.clone(), Step::Start, Duration::from_secs(60))
///     .manual("on_demand", workflow, Step::Start)
///     .with_drain_timeout(Duration::from_secs(20))
///     .start()
///     .await?;
/// running.trigger("on_demand")?;
/// running.stop().await?;
/// # Ok(())
/// # }
/// ```
///
/// Requires the `scheduler` feature.
#[must_use]
pub struct Scheduler {
    flows: Vec<(Arc<str>, FlowRuntime)>,
    error: Option<CanoError>,
    drain_timeout: Duration,
    cancel_timeout: Duration,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// An empty scheduler with a 20 s drain timeout and a 5 s cancel timeout
    /// (see [`RunningScheduler::stop`]).
    pub fn new() -> Self {
        Self {
            flows: Vec::new(),
            error: None,
            drain_timeout: Duration::from_secs(20),
            cancel_timeout: Duration::from_secs(5),
        }
    }

    /// Run `workflow` from `initial_state` immediately after `start`, then every
    /// `interval` (stretched by a pending backoff window). A zero interval is a
    /// configuration error.
    pub fn every<TState>(
        self,
        id: &str,
        workflow: Workflow<TState>,
        initial_state: TState,
        interval: Duration,
    ) -> Self
    where
        TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    {
        if interval.is_zero() {
            return self.fail(CanoError::configuration(format!(
                "Flow '{id}': every() interval must be non-zero"
            )));
        }
        self.add(id, workflow, initial_state, ParsedSchedule::Every(interval))
    }

    /// Run `workflow` from `initial_state` on a cron schedule (seconds-resolution
    /// expression, as parsed by the `cron` crate).
    pub fn cron<TState>(
        self,
        id: &str,
        workflow: Workflow<TState>,
        initial_state: TState,
        expr: &str,
    ) -> Self
    where
        TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    {
        match CronSchedule::from_str(expr) {
            Ok(parsed) => self.add(
                id,
                workflow,
                initial_state,
                ParsedSchedule::Cron(Box::new(parsed)),
            ),
            Err(e) => self.fail(CanoError::configuration(format!(
                "Invalid cron expression: {e}"
            ))),
        }
    }

    /// Register `workflow` to run only on [`RunningScheduler::trigger`].
    pub fn manual<TState>(self, id: &str, workflow: Workflow<TState>, initial_state: TState) -> Self
    where
        TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    {
        self.add(id, workflow, initial_state, ParsedSchedule::Manual)
    }

    /// Replace the [`BackoffPolicy`] of the already-registered flow `id`
    /// (every flow starts with [`BackoffPolicy::default`]).
    pub fn with_backoff(mut self, id: &str, policy: BackoffPolicy) -> Self {
        match self.flows.iter_mut().find(|(flow_id, _)| &**flow_id == id) {
            Some((_, flow)) => {
                flow.policy = policy;
                self
            }
            None => self.fail(CanoError::configuration(format!(
                "Flow '{id}' not found — cannot set backoff"
            ))),
        }
    }

    /// How long [`RunningScheduler::stop`] waits for in-flight runs to finish on
    /// their own before cancelling them. Default 20 s.
    pub fn with_drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = timeout;
        self
    }

    /// How long [`RunningScheduler::stop`] waits for cancelled runs to finish before
    /// aborting them. Default 5 s.
    pub fn with_cancel_timeout(mut self, timeout: Duration) -> Self {
        self.cancel_timeout = timeout;
        self
    }

    /// Validate every workflow and its initial state, set up every workflow's
    /// resources in registration order, and spawn the scheduling loops.
    ///
    /// Must be called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// - The first registration error (see the type docs).
    /// - [`CanoError::Configuration`] — a workflow failed `validate()` or
    ///   `validate_initial_state()`; nothing has been set up.
    /// - A resource setup error; the flows set up before it are torn down in
    ///   reverse order first.
    pub async fn start(self) -> CanoResult<RunningScheduler> {
        if let Some(e) = self.error {
            return Err(e);
        }
        for (_, rt) in &self.flows {
            rt.flow.validate()?;
        }
        for (idx, (_, rt)) in self.flows.iter().enumerate() {
            if let Err(e) = rt.flow.setup().await {
                for (_, prior) in self.flows[..idx].iter().rev() {
                    prior.flow.teardown().await;
                }
                return Err(e);
            }
        }
        Ok(RunningScheduler::launch(
            self.flows,
            self.drain_timeout,
            self.cancel_timeout,
        ))
    }

    fn fail(mut self, error: CanoError) -> Self {
        self.error.get_or_insert(error);
        self
    }

    fn add<TState>(
        mut self,
        id: &str,
        workflow: Workflow<TState>,
        initial_state: TState,
        schedule: ParsedSchedule,
    ) -> Self
    where
        TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    {
        if self.flows.iter().any(|(flow_id, _)| &**flow_id == id) {
            return self.fail(CanoError::configuration(format!(
                "Flow '{id}' already exists"
            )));
        }
        let id: Arc<str> = Arc::from(id);
        let cell = FlowCell {
            info: FlowInfo {
                id: Arc::clone(&id),
                status: Status::Idle,
                run_count: 0,
                last_run: None,
                failure_streak: 0,
                next_eligible: None,
            },
            run_token: None,
        };
        let runtime = FlowRuntime {
            flow: Arc::new(WorkflowFlow {
                workflow,
                initial_state,
            }),
            schedule,
            policy: BackoffPolicy::default(),
            cell: tokio::sync::watch::Sender::new(cell),
        };
        self.flows.push((id, runtime));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::test_support::*;

    async fn start_err(scheduler: Scheduler) -> CanoError {
        scheduler
            .start()
            .await
            .expect_err("start must report the registration error")
    }

    #[tokio::test]
    async fn invalid_cron_is_reported_by_start() {
        let err = start_err(Scheduler::new().cron(
            "f",
            create_test_workflow(),
            TestState::Start,
            "invalid cron",
        ))
        .await;
        assert!(matches!(err, CanoError::Configuration(_)));
        assert!(err.to_string().contains("Invalid cron expression"), "{err}");
    }

    #[tokio::test]
    async fn zero_interval_is_reported_by_start() {
        let err = start_err(Scheduler::new().every(
            "f",
            create_test_workflow(),
            TestState::Start,
            Duration::ZERO,
        ))
        .await;
        assert!(
            err.to_string().contains("interval must be non-zero"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn duplicate_id_is_reported_by_start() {
        let err = start_err(
            Scheduler::new()
                .manual("f", create_test_workflow(), TestState::Start)
                .manual("f", create_test_workflow(), TestState::Start),
        )
        .await;
        assert!(err.to_string().contains("Flow 'f' already exists"), "{err}");
    }

    #[tokio::test]
    async fn backoff_for_unknown_id_is_reported_by_start() {
        let err = start_err(Scheduler::new().with_backoff("ghost", BackoffPolicy::default())).await;
        assert!(err.to_string().contains("Flow 'ghost' not found"), "{err}");
    }

    #[tokio::test]
    async fn first_registration_error_wins() {
        let err = start_err(
            Scheduler::new()
                .cron("a", create_test_workflow(), TestState::Start, "nope")
                .manual("b", create_test_workflow(), TestState::Start)
                .manual("b", create_test_workflow(), TestState::Start),
        )
        .await;
        assert!(err.to_string().contains("Invalid cron expression"), "{err}");
    }
}
