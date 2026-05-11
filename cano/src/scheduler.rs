//! # Scheduler API
//!
//! Time-driven dispatch for [`Workflow`](crate::workflow::Workflow) instances —
//! intervals, cron expressions, and manual triggers, plus optional per-flow
//! backoff and trip semantics.
//!
//! Two-stage lifecycle:
//!
//! - [`Scheduler`] is the **builder**. Register workflows with [`every`](Scheduler::every) /
//!   [`cron`](Scheduler::cron) / [`manual`](Scheduler::manual), attach optional
//!   [`BackoffPolicy`] via [`set_backoff`](Scheduler::set_backoff), then call
//!   [`start`](Scheduler::start) to consume the builder. `Scheduler` is **not** `Clone`.
//! - [`RunningScheduler`] is the **live handle** returned by `start`. It owns the
//!   spawned driver and per-flow loop tasks. It is cheap to clone — call
//!   [`trigger`](RunningScheduler::trigger), [`status`](RunningScheduler::status),
//!   [`stop`](RunningScheduler::stop), etc. from any task.
//!
//! Because `start` consumes `self`, double-starting the same builder is rejected
//! at the type level.
//!
//! > **Note:** The scheduler is an optional feature. Enable the `"scheduler"`
//! > feature in your `Cargo.toml`.
//!
//! ```toml
//! [dependencies]
//! cano = { version = "0.11", features = ["scheduler"] }
//! ```

mod backoff;

pub use backoff::BackoffPolicy;

use crate::error::CanoResult;
use crate::workflow::Workflow;
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::{RwLock, oneshot};
use tokio::time::Duration;

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
    Reset {
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
    /// Terminal failure for a flow without a [`BackoffPolicy`]. Carries the
    /// last error message for observability.
    ///
    /// Invariant: only reachable when the flow has no policy attached.
    /// `apply_outcome` upholds this; a debug-build assertion guards it.
    /// Flows with a policy use [`Status::Backoff`] or [`Status::Tripped`]
    /// instead, which is what avoids the legacy `Failed` flicker.
    Failed(String),
    /// Flow failed and is waiting until `until` before its next dispatch
    /// (set only when a [`BackoffPolicy`] is attached).
    Backoff {
        until: DateTime<Utc>,
        streak: u32,
        last_error: String,
    },
    /// Flow has reached its [`BackoffPolicy::streak_limit`] and will not
    /// dispatch again until [`Scheduler::reset_flow`] is called.
    Tripped {
        streak: u32,
        last_error: String,
    },
}

/// Workflow information.
///
/// Adding fields here is an additive change; downstream code that constructs
/// `FlowInfo` literally (rare — usually only this crate constructs it) needs
/// `..` rest patterns or to use the public-fields-with-default convention.
#[derive(Debug, Clone)]
pub struct FlowInfo {
    pub id: String,
    pub status: Status,
    pub run_count: u64,
    pub last_run: Option<DateTime<Utc>>,
    /// Number of consecutive failures since the last success. Always 0 when
    /// the flow has no [`BackoffPolicy`] attached.
    pub failure_streak: u32,
    /// When set, the next dispatch must wait until this time. Cleared on
    /// success and on `reset_flow`.
    pub next_eligible: Option<DateTime<Utc>>,
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

/// Per-flow data stored in the scheduler. Carries the workflow, its initial
/// state, the parsed schedule, the live `FlowInfo` shared with status readers,
/// and an optional [`BackoffPolicy`].
struct FlowData<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    workflow: Arc<Workflow<TState, TResourceKey>>,
    initial_state: TState,
    schedule: ParsedSchedule,
    info: Arc<RwLock<FlowInfo>>,
    /// `None` keeps the legacy "fail-and-retry-on-base-schedule" behavior.
    /// `Some(policy)` activates the backoff/trip code path.
    policy: Option<Arc<BackoffPolicy>>,
}

impl<TState, TResourceKey> Clone for FlowData<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            workflow: Arc::clone(&self.workflow),
            initial_state: self.initial_state.clone(),
            schedule: self.schedule.clone(),
            info: Arc::clone(&self.info),
            policy: self.policy.clone(),
        }
    }
}

mod builder;
mod loops;
mod running;
#[cfg(test)]
mod test_support;

pub use builder::Scheduler;
pub use running::RunningScheduler;
