//! # Scheduler API
//!
//! Time-driven dispatch for [`Workflow`] instances — intervals, cron
//! expressions, and manual triggers, plus per-flow backoff and trip semantics.
//!
//! - [`Scheduler`] is the **builder**: chain [`every`](Scheduler::every) /
//!   [`cron`](Scheduler::cron) / [`manual`](Scheduler::manual) (workflows may use
//!   different state types), optional [`with_backoff`](Scheduler::with_backoff) and
//!   shutdown timeouts, then [`start`](Scheduler::start).
//! - [`RunningScheduler`] is the **live handle** `start` returns. Clone it freely;
//!   [`trigger`](RunningScheduler::trigger), [`status`](RunningScheduler::status),
//!   [`cancel_flow`](RunningScheduler::cancel_flow) and friends work from any task,
//!   and [`stop`](RunningScheduler::stop) shuts down in bounded time: drain, then
//!   cancel, then abort.
//!
//! ```rust,no_run
//! # use cano::prelude::*;
//! # use std::time::Duration;
//! # #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! # enum Step { Start, Done }
//! # fn workflow() -> Workflow<Step> { unimplemented!() }
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let running = Scheduler::new()
//!     .every("fetch", workflow(), Step::Start, Duration::from_secs(60))
//!     .start()
//!     .await?;
//! cano::scheduler::shutdown_signal().await?; // Ctrl-C, or SIGTERM on Unix
//! running.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! > **Note:** The scheduler is an optional feature. Enable the `"scheduler"`
//! > feature in your `Cargo.toml`.
//!
//! ```toml
//! [dependencies]
//! cano = { version = "0.15", features = ["scheduler"] }
//! ```

mod backoff;
mod builder;
mod loops;
mod running;
#[cfg(test)]
mod test_support;

pub use backoff::BackoffPolicy;
pub use builder::Scheduler;
pub use running::RunningScheduler;

use std::hash::Hash;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use futures_util::future::BoxFuture;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::error::CanoResult;
use crate::workflow::Workflow;

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
    /// Flow failed and is waiting until `until` before its next dispatch,
    /// per the flow's [`BackoffPolicy`].
    Backoff {
        until: DateTime<Utc>,
        streak: u32,
        last_error: Arc<str>,
    },
    /// Flow has reached its [`BackoffPolicy::streak_limit`] and will not
    /// dispatch again until [`RunningScheduler::reset_flow`] is called.
    Tripped {
        streak: u32,
        last_error: Arc<str>,
    },
}

/// Workflow information.
#[derive(Debug, Clone)]
pub struct FlowInfo {
    pub id: Arc<str>,
    pub status: Status,
    pub run_count: u64,
    pub last_run: Option<DateTime<Utc>>,
    /// Number of consecutive failures since the last success.
    pub failure_streak: u32,
    /// When set, the next dispatch must wait until this time. Cleared on
    /// success and on `reset_flow`.
    pub next_eligible: Option<DateTime<Utc>>,
}

/// Resolves on Ctrl-C, or on SIGTERM on Unix — the usual "shut down now" signals
/// for a scheduler process.
///
/// # Errors
///
/// The I/O error from installing a signal handler.
pub async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            res = tokio::signal::ctrl_c() => res,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Parsed schedule; the cron expression is parsed once at registration.
#[derive(Clone)]
enum ParsedSchedule {
    Every(Duration),
    /// Boxed because `CronSchedule` is ~248 bytes and would dominate the enum size.
    Cron(Box<CronSchedule>),
    Manual,
}

/// A registered workflow with its state type erased, so one scheduler can host
/// workflows with different state enums.
trait Flow: Send + Sync + 'static {
    /// `validate()` + `validate_initial_state`.
    fn validate(&self) -> CanoResult<()>;
    fn setup(&self) -> BoxFuture<'_, CanoResult<()>>;
    fn teardown(&self) -> BoxFuture<'_, ()>;
    /// One run from the initial state under `token` (no resource lifecycle).
    fn run<'a>(&'a self, token: &'a CancellationToken) -> BoxFuture<'a, CanoResult<()>>;
}

struct WorkflowFlow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    workflow: Workflow<TState, TResourceKey>,
    initial_state: TState,
}

impl<TState, TResourceKey> Flow for WorkflowFlow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn validate(&self) -> CanoResult<()> {
        self.workflow.validate()?;
        self.workflow.validate_initial_state(&self.initial_state)
    }

    fn setup(&self) -> BoxFuture<'_, CanoResult<()>> {
        Box::pin(self.workflow.resources.setup_all())
    }

    fn teardown(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.workflow.resources.teardown_all())
    }

    fn run<'a>(&'a self, token: &'a CancellationToken) -> BoxFuture<'a, CanoResult<()>> {
        Box::pin(async move {
            self.workflow
                .run_once(self.initial_state.clone(), Some(token))
                .await
                .map(drop)
        })
    }
}

/// The live, watchable part of a flow: its public info plus the token of the run
/// in flight (`None` while idle).
struct FlowCell {
    info: FlowInfo,
    run_token: Option<CancellationToken>,
}

/// Everything a running scheduler keeps per flow.
struct FlowRuntime {
    flow: Arc<dyn Flow>,
    schedule: ParsedSchedule,
    policy: BackoffPolicy,
    cell: tokio::sync::watch::Sender<FlowCell>,
}

#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use crate::metrics::test_support::*;
    use crate::prelude::*;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum St {
        Start,
        Done,
    }
    struct Ok1;
    #[crate::task]
    impl Task<St> for Ok1 {
        async fn run_bare(&self) -> Result<TaskResult<St>, CanoError> {
            Ok(TaskResult::Single(St::Done))
        }
    }

    #[test]
    fn manual_trigger_records_a_completed_flow_run() {
        let ((), rows) = run_with_recorder(|| async {
            let wf = Workflow::bare()
                .register(St::Start, Ok1)
                .add_exit_state(St::Done);
            let running = Scheduler::new()
                .manual("flow_a", wf, St::Start)
                .start()
                .await
                .unwrap();
            running.trigger("flow_a").unwrap();
            running.wait_idle("flow_a").await.unwrap();
            running.stop().await.unwrap();
        });
        assert_eq!(
            counter(
                &rows,
                "cano_scheduler_flow_runs_total",
                &[("flow", "flow_a"), ("outcome", "completed")]
            ),
            1
        );
        assert_eq!(
            histogram_count(
                &rows,
                "cano_scheduler_flow_duration_seconds",
                &[("flow", "flow_a")]
            ),
            1
        );
        assert_eq!(gauge(&rows, "cano_scheduler_active_flows", &[]), 0.0);
        assert_eq!(
            counter_opt(
                &rows,
                "cano_scheduler_flow_backoff_total",
                &[("flow", "flow_a")]
            ),
            None
        );
    }
}
