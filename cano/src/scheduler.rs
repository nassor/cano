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
//! ## Several instances
//!
//! Give every process the same flows and one shared [`SchedulerStore`] with
//! [`with_store`](Scheduler::with_store), and each tick of each flow runs on exactly one
//! of them: an `every` flow runs once per interval across the cluster, a `cron` flow once
//! per firing, and a [`trigger`](RunningScheduler::trigger) is refused while another
//! instance runs the flow ([`Status::RunningElsewhere`]). Backoff and trip state live in
//! the store too, so a restarted process picks up where the cluster left off: a flow that
//! ran within its interval is not run again at startup, and a tripped flow stays tripped
//! until [`reset_flow`](RunningScheduler::reset_flow).
//!
//! ```rust,no_run
//! # use cano::prelude::*;
//! # use std::sync::Arc;
//! # use std::time::Duration;
//! # #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! # enum Step { Start, Done }
//! # fn workflow() -> Workflow<Step> { unimplemented!() }
//! # fn shared_store() -> Arc<dyn SchedulerStore> { unimplemented!() }
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Every instance runs this same code against the same store.
//! let running = Scheduler::new()
//!     .every("fetch", workflow(), Step::Start, Duration::from_secs(60))
//!     .with_store(shared_store())
//!     .with_lease_ttl(Duration::from_secs(30))
//!     .start()
//!     .await?;
//! cano::scheduler::shutdown_signal().await?;
//! running.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The running instance holds a lease on the flow's record and renews it every third of
//! the TTL. When it cannot renew for a whole TTL the run is cancelled and its outcome is
//! not written. An instance that crashes mid-run leaves its lease to expire; the next
//! instance to look counts the lost run as one failure (see
//! [`BackoffPolicy`]) and carries on. A run aborted by the shutdown cancel timeout, or
//! whose release hits a store outage, is counted the same way.
//!
//! Lease expiry compares wall clocks across instances, so keep them synchronised with a
//! skew well below the TTL, and give every instance a distinct
//! [`with_instance_id`](Scheduler::with_instance_id) (the default is random). Instances
//! that share a table, bucket or file form one cluster; the built-in stores name theirs
//! `cano_scheduler` by default.
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
mod lease;
mod loops;
mod running;
mod store;
#[cfg(test)]
mod test_support;

pub use backoff::BackoffPolicy;
pub use builder::Scheduler;
pub use running::RunningScheduler;
pub use store::{CasOutcome, SchedulerStore, VersionedRecord};

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
    /// Another scheduler instance sharing the same [`SchedulerStore`] holds the flow's
    /// lease and is running it (or crashed while running it, until the lease expires).
    ///
    /// Only a scheduler built with [`Scheduler::with_store`] reports it. The value is
    /// the shared record as of this instance's last read: it is refreshed at every
    /// dispatch attempt of an `every` or `cron` flow, on `trigger`, on `reset_flow` and
    /// at `start`, so it can lag the other instance's run by up to one interval.
    RunningElsewhere {
        /// The instance id of the lease holder.
        owner: Arc<str>,
        /// When the lease runs out unless its holder renews it.
        lease_expires: DateTime<Utc>,
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

struct WorkflowFlow<TState>
where
    TState: Clone + std::fmt::Debug + Hash + Eq + Send + Sync + 'static,
{
    workflow: Workflow<TState>,
    initial_state: TState,
}

impl<TState> Flow for WorkflowFlow<TState>
where
    TState: Clone + std::fmt::Debug + Hash + Eq + Send + Sync + 'static,
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
    /// A store claim is in flight on this instance. It blocks a second local dispatch
    /// without showing the flow as `Running` before the claim is won.
    claiming: bool,
}

/// Everything a running scheduler keeps per flow.
struct FlowRuntime {
    flow: Arc<dyn Flow>,
    schedule: ParsedSchedule,
    policy: BackoffPolicy,
    cell: tokio::sync::watch::Sender<FlowCell>,
    /// Present when the scheduler shares its flows through a [`SchedulerStore`].
    coordinator: Option<Arc<lease::Coordinator>>,
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
        async fn run(&self) -> Result<St, CanoError> {
            Ok(St::Done)
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
            running.trigger("flow_a").await.unwrap();
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
