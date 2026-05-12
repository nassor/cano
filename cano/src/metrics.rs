//! Internal metrics emission, behind the `metrics` feature.
//!
//! Cano uses the [`metrics`](https://docs.rs/metrics) facade crate the same way it uses
//! `tracing`: instrumentation is compiled in only when the `metrics` feature is enabled,
//! and it does nothing until you install a recorder (e.g. `metrics-exporter-prometheus`).
//!
//! Two complementary mechanisms (mirroring the `tracing` integration):
//!
//! - [`MetricsObserver`](crate::observer::MetricsObserver) — a [`WorkflowObserver`](crate::observer::WorkflowObserver)
//!   you opt into with `Workflow::with_observer(Arc::new(MetricsObserver::new()))`; it emits
//!   counters for the workflow-level events (state enters, task runs by outcome, retries,
//!   circuit-open events, checkpoints, resumes), exactly as `TracingObserver` emits `tracing`
//!   events for them.
//! - Always-on direct instrumentation (the helpers in this module) for engine internals the
//!   observer doesn't reach: workflow run outcome/duration + an active-workflows gauge,
//!   circuit-breaker transitions/acquires/outcomes, per-state task durations, split-branch
//!   results, scheduler flow runs/durations/backoff/trips + an active-flows gauge, the
//!   poll/batch/stepped processing loops, recovery checkpoint-store operations, and saga
//!   compensation drains.
//!
//! ## What gets measured
//!
//! Counters (`_total`), histograms (`_seconds`, recorded as f64 seconds), and gauges, all
//! prefixed `cano_`. See [`describe`] for the full list with help text and units; call
//! [`describe`] once at startup, after installing your recorder.
//!
//! ## Cardinality
//!
//! Labels are deliberately minimal. Workflow/task/split metrics carry a `state` label
//! (`format!("{:?}")` of your FSM state enum, bounded by the small static set of registered
//! states) or a `task` label (`Task::name()`, defaulting to `type_name`, bounded by the
//! registered task types). A few metrics carry bounded enum labels (`outcome`, `kind`,
//! `result`, `transition`). Scheduler metrics carry the `flow` id (bounded by registered
//! flows). The deepest hot-path metrics (per-attempt counters in the retry loop, circuit
//! internals, poll/batch/step iteration counts) carry no per-state label.
//!
//! ## Cost
//!
//! When the `metrics` feature is compiled in, every FSM state transition formats the state
//! label (`format!("{state:?}")`) and performs a few small string clones (the `metrics`
//! macros take owned label values) regardless of whether a recorder is installed. That cost
//! is small and bounded, but if you have a latency-critical build that does not collect
//! metrics, leave the feature off.

#![allow(dead_code)] // helpers are wired up across the following tasks; the final task removes this

use metrics::{
    Unit, counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram,
};
use std::time::Duration;

// ----- metric names -----

// Observer-emitted (require attaching MetricsObserver):
pub const STATE_ENTERS_TOTAL: &str = "cano_state_enters_total";
pub const OBSERVED_TASK_RUNS_TOTAL: &str = "cano_observed_task_runs_total";
pub const TASK_RETRIES_TOTAL: &str = "cano_task_retries_total";
pub const CIRCUIT_OPEN_EVENTS_TOTAL: &str = "cano_circuit_open_events_total";
pub const CHECKPOINTS_OBSERVED_TOTAL: &str = "cano_checkpoints_observed_total";
pub const RESUMES_TOTAL: &str = "cano_resumes_total";

// Always-on direct instrumentation:
pub const WORKFLOW_RUNS_TOTAL: &str = "cano_workflow_runs_total";
pub const WORKFLOW_DURATION_SECONDS: &str = "cano_workflow_duration_seconds";
pub const WORKFLOW_ACTIVE: &str = "cano_workflow_active";
pub const TASK_DURATION_SECONDS: &str = "cano_task_duration_seconds";
pub const TASK_ATTEMPTS_TOTAL: &str = "cano_task_attempts_total";
pub const CIRCUIT_REJECTIONS_TOTAL: &str = "cano_circuit_rejections_total";
pub const SPLIT_BRANCH_RESULTS_TOTAL: &str = "cano_split_branch_results_total";
pub const CIRCUIT_TRANSITIONS_TOTAL: &str = "cano_circuit_transitions_total";
pub const CIRCUIT_ACQUIRES_TOTAL: &str = "cano_circuit_acquires_total";
pub const CIRCUIT_OUTCOMES_TOTAL: &str = "cano_circuit_outcomes_total";
pub const POLL_ITERATIONS_TOTAL: &str = "cano_poll_iterations_total";
pub const BATCH_RUNS_TOTAL: &str = "cano_batch_runs_total";
pub const BATCH_ITEMS_TOTAL: &str = "cano_batch_items_total";
pub const STEP_ITERATIONS_TOTAL: &str = "cano_step_iterations_total";
pub const CHECKPOINT_APPENDS_TOTAL: &str = "cano_checkpoint_appends_total";
pub const CHECKPOINT_CLEARS_TOTAL: &str = "cano_checkpoint_clears_total";
pub const COMPENSATIONS_RUN_TOTAL: &str = "cano_compensations_run_total";
pub const COMPENSATION_DRAINS_TOTAL: &str = "cano_compensation_drains_total";

#[cfg(feature = "scheduler")]
pub const SCHEDULER_FLOW_RUNS_TOTAL: &str = "cano_scheduler_flow_runs_total";
#[cfg(feature = "scheduler")]
pub const SCHEDULER_FLOW_DURATION_SECONDS: &str = "cano_scheduler_flow_duration_seconds";
#[cfg(feature = "scheduler")]
pub const SCHEDULER_FLOW_BACKOFF_TOTAL: &str = "cano_scheduler_flow_backoff_total";
#[cfg(feature = "scheduler")]
pub const SCHEDULER_FLOW_TRIPPED_TOTAL: &str = "cano_scheduler_flow_tripped_total";
#[cfg(feature = "scheduler")]
pub const SCHEDULER_ACTIVE_FLOWS: &str = "cano_scheduler_active_flows";

/// Register descriptions and units for every metric Cano emits.
///
/// Call once at startup, after installing your `metrics` recorder, so exporters surface help
/// text and units. Safe to call more than once. No-op unless a recorder is installed.
pub fn describe() {
    describe_counter!(
        STATE_ENTERS_TOTAL,
        Unit::Count,
        "FSM state entries, by state (emitted by MetricsObserver)"
    );
    describe_counter!(
        OBSERVED_TASK_RUNS_TOTAL,
        Unit::Count,
        "Task dispatches, by task and outcome (completed|failed) (emitted by MetricsObserver)"
    );
    describe_counter!(
        TASK_RETRIES_TOTAL,
        Unit::Count,
        "Retry attempts, by task (emitted by MetricsObserver)"
    );
    describe_counter!(
        CIRCUIT_OPEN_EVENTS_TOTAL,
        Unit::Count,
        "Times a task was short-circuited by an open breaker, by task (emitted by MetricsObserver)"
    );
    describe_counter!(
        CHECKPOINTS_OBSERVED_TOTAL,
        Unit::Count,
        "Checkpoint rows durably appended (emitted by MetricsObserver)"
    );
    describe_counter!(
        RESUMES_TOTAL,
        Unit::Count,
        "Workflow resumes from a checkpoint store (emitted by MetricsObserver)"
    );

    describe_counter!(
        WORKFLOW_RUNS_TOTAL,
        Unit::Count,
        "Workflow runs (via Workflow::orchestrate/resume_from), by terminal outcome (completed|failed|timeout)"
    );
    describe_histogram!(
        WORKFLOW_DURATION_SECONDS,
        Unit::Seconds,
        "Wall-clock duration of a workflow run, by terminal outcome"
    );
    describe_gauge!(
        WORKFLOW_ACTIVE,
        Unit::Count,
        "Workflows currently executing"
    );
    describe_histogram!(
        TASK_DURATION_SECONDS,
        Unit::Seconds,
        "Duration of a state-handler dispatch including retries, by state and kind (single|router|split|compensatable|stepped)"
    );
    describe_counter!(
        TASK_ATTEMPTS_TOTAL,
        Unit::Count,
        "Individual task attempts inside the retry loop, by outcome (completed|failed)"
    );
    describe_counter!(
        CIRCUIT_REJECTIONS_TOTAL,
        Unit::Count,
        "Task attempts short-circuited by an open circuit breaker in the retry loop"
    );
    describe_counter!(
        SPLIT_BRANCH_RESULTS_TOTAL,
        Unit::Count,
        "Per-branch outcomes of split/join states, by result (success|failure|cancelled)"
    );
    describe_counter!(
        CIRCUIT_TRANSITIONS_TOTAL,
        Unit::Count,
        "Circuit-breaker state transitions, by transition (closed_to_open|open_to_halfopen|halfopen_to_closed|halfopen_to_open)"
    );
    describe_counter!(
        CIRCUIT_ACQUIRES_TOTAL,
        Unit::Count,
        "Circuit-breaker permit acquisitions, by result (acquired|rejected)"
    );
    describe_counter!(
        CIRCUIT_OUTCOMES_TOTAL,
        Unit::Count,
        "Outcomes recorded against a circuit breaker, by outcome (success|failure)"
    );
    describe_counter!(
        POLL_ITERATIONS_TOTAL,
        Unit::Count,
        "PollTask poll() calls, by outcome (ready|pending)"
    );
    describe_counter!(
        BATCH_RUNS_TOTAL,
        Unit::Count,
        "BatchTask load→process→finish dispatches, by outcome (completed|failed)"
    );
    describe_counter!(
        BATCH_ITEMS_TOTAL,
        Unit::Count,
        "BatchTask items processed, by result (ok|err)"
    );
    describe_counter!(
        STEP_ITERATIONS_TOTAL,
        Unit::Count,
        "SteppedTask step() calls, by outcome (more|done)"
    );
    describe_counter!(
        CHECKPOINT_APPENDS_TOTAL,
        Unit::Count,
        "CheckpointStore::append calls from the engine, by result (ok|err)"
    );
    describe_counter!(
        CHECKPOINT_CLEARS_TOTAL,
        Unit::Count,
        "CheckpointStore::clear calls from the engine (best-effort on success), by result (ok|err)"
    );
    describe_counter!(
        COMPENSATIONS_RUN_TOTAL,
        Unit::Count,
        "compensate() invocations during a rollback drain, by result (ok|err)"
    );
    describe_counter!(
        COMPENSATION_DRAINS_TOTAL,
        Unit::Count,
        "Compensation-stack drains, by outcome (clean|partial)"
    );

    #[cfg(feature = "scheduler")]
    {
        describe_counter!(
            SCHEDULER_FLOW_RUNS_TOTAL,
            Unit::Count,
            "Scheduled-flow runs, by flow and outcome (completed|failed)"
        );
        describe_histogram!(
            SCHEDULER_FLOW_DURATION_SECONDS,
            Unit::Seconds,
            "Duration of a scheduled-flow run, by flow"
        );
        describe_counter!(
            SCHEDULER_FLOW_BACKOFF_TOTAL,
            Unit::Count,
            "Times a scheduled flow entered a backoff window, by flow"
        );
        describe_counter!(
            SCHEDULER_FLOW_TRIPPED_TOTAL,
            Unit::Count,
            "Times a scheduled flow tripped (hit its streak limit), by flow"
        );
        describe_gauge!(
            SCHEDULER_ACTIVE_FLOWS,
            Unit::Count,
            "Scheduled flows currently executing"
        );
    }
}

/// `format!("{:?}")` of an FSM state, used as the low-cardinality `state` label value.
#[inline]
pub(crate) fn state_label<S: std::fmt::Debug>(state: &S) -> String {
    format!("{state:?}")
}

// ----- RAII gauge guards -----

/// Increments [`WORKFLOW_ACTIVE`] on construction, decrements on drop — survives every
/// early-return / `?` / caught-panic path of the function it guards.
pub(crate) struct WorkflowActiveGuard;
impl WorkflowActiveGuard {
    pub(crate) fn new() -> Self {
        gauge!(WORKFLOW_ACTIVE).increment(1.0);
        Self
    }
}
impl Drop for WorkflowActiveGuard {
    fn drop(&mut self) {
        gauge!(WORKFLOW_ACTIVE).decrement(1.0);
    }
}

#[cfg(feature = "scheduler")]
pub(crate) struct SchedulerFlowActiveGuard;
#[cfg(feature = "scheduler")]
impl SchedulerFlowActiveGuard {
    pub(crate) fn new() -> Self {
        gauge!(SCHEDULER_ACTIVE_FLOWS).increment(1.0);
        Self
    }
}
#[cfg(feature = "scheduler")]
impl Drop for SchedulerFlowActiveGuard {
    fn drop(&mut self) {
        gauge!(SCHEDULER_ACTIVE_FLOWS).decrement(1.0);
    }
}

// ----- observer-emitted helpers (called from MetricsObserver) -----

pub(crate) fn observed_state_enter(state: &str) {
    counter!(STATE_ENTERS_TOTAL, "state" => state.to_owned()).increment(1);
}
pub(crate) fn observed_task_run(task: &str, ok: bool) {
    counter!(OBSERVED_TASK_RUNS_TOTAL, "task" => task.to_owned(), "outcome" => if ok { "completed" } else { "failed" }).increment(1);
}
pub(crate) fn observed_task_retry(task: &str) {
    counter!(TASK_RETRIES_TOTAL, "task" => task.to_owned()).increment(1);
}
pub(crate) fn observed_circuit_open(task: &str) {
    counter!(CIRCUIT_OPEN_EVENTS_TOTAL, "task" => task.to_owned()).increment(1);
}
pub(crate) fn observed_checkpoint() {
    counter!(CHECKPOINTS_OBSERVED_TOTAL).increment(1);
}
pub(crate) fn observed_resume() {
    counter!(RESUMES_TOTAL).increment(1);
}

// ----- workflow run -----

pub(crate) fn workflow_run(outcome: &'static str, dur: Duration) {
    counter!(WORKFLOW_RUNS_TOTAL, "outcome" => outcome).increment(1);
    histogram!(WORKFLOW_DURATION_SECONDS, "outcome" => outcome).record(dur.as_secs_f64());
}

pub(crate) fn task_dispatch_duration(state: &str, kind: &'static str, dur: Duration) {
    histogram!(TASK_DURATION_SECONDS, "state" => state.to_owned(), "kind" => kind)
        .record(dur.as_secs_f64());
}

pub(crate) fn split_branch_results(successes: usize, failures: usize, cancelled: usize) {
    if successes > 0 {
        counter!(SPLIT_BRANCH_RESULTS_TOTAL, "result" => "success").increment(successes as u64);
    }
    if failures > 0 {
        counter!(SPLIT_BRANCH_RESULTS_TOTAL, "result" => "failure").increment(failures as u64);
    }
    if cancelled > 0 {
        counter!(SPLIT_BRANCH_RESULTS_TOTAL, "result" => "cancelled").increment(cancelled as u64);
    }
}

// ----- retry loop -----

pub(crate) fn task_attempt(ok: bool) {
    counter!(TASK_ATTEMPTS_TOTAL, "outcome" => if ok { "completed" } else { "failed" })
        .increment(1);
}
pub(crate) fn circuit_rejection() {
    counter!(CIRCUIT_REJECTIONS_TOTAL).increment(1);
}

// ----- circuit breaker -----

pub(crate) fn circuit_transition(transition: &'static str) {
    counter!(CIRCUIT_TRANSITIONS_TOTAL, "transition" => transition).increment(1);
}
pub(crate) fn circuit_acquire(result: &'static str) {
    counter!(CIRCUIT_ACQUIRES_TOTAL, "result" => result).increment(1);
}
pub(crate) fn circuit_outcome(outcome: &'static str) {
    counter!(CIRCUIT_OUTCOMES_TOTAL, "outcome" => outcome).increment(1);
}

// ----- processing loops -----

pub(crate) fn poll_iteration(ready: bool) {
    counter!(POLL_ITERATIONS_TOTAL, "outcome" => if ready { "ready" } else { "pending" })
        .increment(1);
}
pub(crate) fn batch_run(ok: bool) {
    counter!(BATCH_RUNS_TOTAL, "outcome" => if ok { "completed" } else { "failed" }).increment(1);
}
pub(crate) fn batch_items(ok: usize, err: usize) {
    if ok > 0 {
        counter!(BATCH_ITEMS_TOTAL, "result" => "ok").increment(ok as u64);
    }
    if err > 0 {
        counter!(BATCH_ITEMS_TOTAL, "result" => "err").increment(err as u64);
    }
}
pub(crate) fn step_iteration(done: bool) {
    counter!(STEP_ITERATIONS_TOTAL, "outcome" => if done { "done" } else { "more" }).increment(1);
}

// ----- recovery / saga -----

pub(crate) fn checkpoint_append(ok: bool) {
    counter!(CHECKPOINT_APPENDS_TOTAL, "result" => if ok { "ok" } else { "err" }).increment(1);
}
pub(crate) fn checkpoint_clear(ok: bool) {
    counter!(CHECKPOINT_CLEARS_TOTAL, "result" => if ok { "ok" } else { "err" }).increment(1);
}
pub(crate) fn compensation_run(ok: bool) {
    counter!(COMPENSATIONS_RUN_TOTAL, "result" => if ok { "ok" } else { "err" }).increment(1);
}
pub(crate) fn compensation_drain(clean: bool) {
    counter!(COMPENSATION_DRAINS_TOTAL, "outcome" => if clean { "clean" } else { "partial" })
        .increment(1);
}

// ----- scheduler -----

#[cfg(feature = "scheduler")]
pub(crate) fn scheduler_flow_run(flow: &str, ok: bool, dur: Duration) {
    counter!(SCHEDULER_FLOW_RUNS_TOTAL, "flow" => flow.to_owned(), "outcome" => if ok { "completed" } else { "failed" }).increment(1);
    histogram!(SCHEDULER_FLOW_DURATION_SECONDS, "flow" => flow.to_owned())
        .record(dur.as_secs_f64());
}
#[cfg(feature = "scheduler")]
pub(crate) fn scheduler_flow_backoff(flow: &str) {
    counter!(SCHEDULER_FLOW_BACKOFF_TOTAL, "flow" => flow.to_owned()).increment(1);
}
#[cfg(feature = "scheduler")]
pub(crate) fn scheduler_flow_tripped(flow: &str) {
    counter!(SCHEDULER_FLOW_TRIPPED_TOTAL, "flow" => flow.to_owned()).increment(1);
}

#[cfg(all(test, feature = "metrics"))]
pub(crate) mod test_support {
    //! Test harness: run async code with a thread-local `DebuggingRecorder` installed and
    //! assert over the captured snapshot. Reused by every metrics test in the crate.

    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use std::future::Future;

    /// One row of a debugging snapshot: (key, unit, description, value).
    pub(crate) type SnapshotRow = (
        metrics_util::CompositeKey,
        Option<metrics::Unit>,
        Option<metrics::SharedString>,
        DebugValue,
    );

    /// Run `f` (an async closure) on a fresh **current-thread** Tokio runtime with a
    /// thread-local `DebuggingRecorder` installed, then return the future's output plus the
    /// captured metrics. A current-thread runtime keeps every poll — including
    /// `tokio::spawn`ed tasks — on the calling thread, so the thread-local recorder stays
    /// in scope across `.await` points (which it would not on a multi-thread runtime).
    pub(crate) fn run_with_recorder<F, Fut, R>(f: F) -> (R, Vec<SnapshotRow>)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = R>,
    {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");
        let out = metrics::with_local_recorder(&recorder, || rt.block_on(f()));
        (out, snapshotter.snapshot().into_vec())
    }

    fn find<'a>(
        rows: &'a [SnapshotRow],
        name: &str,
        labels: &[(&str, &str)],
    ) -> Option<&'a DebugValue> {
        rows.iter()
            .find(|(ck, _, _, _)| {
                let key = ck.key();
                key.name() == name
                    && labels
                        .iter()
                        .all(|(k, v)| key.labels().any(|l| l.key() == *k && l.value() == *v))
            })
            .map(|(_, _, _, v)| v)
    }

    /// Counter value for `name` with (a superset of) `labels`; panics if absent or not a counter.
    pub(crate) fn counter(rows: &[SnapshotRow], name: &str, labels: &[(&str, &str)]) -> u64 {
        match find(rows, name, labels) {
            Some(DebugValue::Counter(v)) => *v,
            other => panic!("expected counter `{name}` {labels:?}, found {other:?}"),
        }
    }
    /// Like [`counter`] but returns `None` when the metric is absent.
    pub(crate) fn counter_opt(
        rows: &[SnapshotRow],
        name: &str,
        labels: &[(&str, &str)],
    ) -> Option<u64> {
        match find(rows, name, labels) {
            Some(DebugValue::Counter(v)) => Some(*v),
            None => None,
            other => panic!("expected counter `{name}` {labels:?}, found {other:?}"),
        }
    }
    /// Gauge value for `name` with (a superset of) `labels`; panics if absent or not a gauge.
    pub(crate) fn gauge(rows: &[SnapshotRow], name: &str, labels: &[(&str, &str)]) -> f64 {
        match find(rows, name, labels) {
            Some(DebugValue::Gauge(v)) => v.into_inner(),
            other => panic!("expected gauge `{name}` {labels:?}, found {other:?}"),
        }
    }
    /// Number of samples recorded into the histogram `name` with (a superset of) `labels`.
    pub(crate) fn histogram_count(
        rows: &[SnapshotRow],
        name: &str,
        labels: &[(&str, &str)],
    ) -> usize {
        match find(rows, name, labels) {
            Some(DebugValue::Histogram(v)) => v.len(),
            other => panic!("expected histogram `{name}` {labels:?}, found {other:?}"),
        }
    }
}

#[cfg(all(test, feature = "metrics"))]
mod tests {
    use super::test_support::*;

    #[test]
    fn describe_does_not_panic_without_a_recorder() {
        super::describe();
    }

    #[test]
    fn recorder_captures_metrics_emitted_across_a_spawn() {
        let ((), rows) = run_with_recorder(|| async {
            let h = tokio::spawn(async {
                metrics::counter!("cano_test_probe", "k" => "v").increment(2);
                metrics::gauge!("cano_test_gauge").set(7.0);
            });
            h.await.unwrap();
        });
        assert_eq!(counter(&rows, "cano_test_probe", &[("k", "v")]), 2);
        assert_eq!(gauge(&rows, "cano_test_gauge", &[]), 7.0);
    }
}
