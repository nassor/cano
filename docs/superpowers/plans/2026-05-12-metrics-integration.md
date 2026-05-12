# `metrics` Feature Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional `metrics` feature (using the [`metrics`](https://github.com/metrics-rs/metrics) facade crate, mirroring how the `tracing` feature + `TracingObserver` are wired) that gives the user full visibility into the engine: a `MetricsObserver` that re-emits the `WorkflowObserver` hooks as metrics (state enters, task runs, retries, circuit-open events, checkpoints, resumes — opt-in, zero engine changes, exactly like `TracingObserver`), PLUS always-on `#[cfg(feature = "metrics")]` direct instrumentation for everything observers don't see: workflow run outcome/duration + active gauge, circuit-breaker internals, split-branch results, scheduler flow runs/durations/backoff/trips + active-flows gauge, the poll/batch/stepped processing loops, recovery checkpoint store operations, and saga compensation drains. Low-cardinality labels by design.

**Architecture:** Two complementary mechanisms, mirroring the `tracing` precedent:
1. **`MetricsObserver`** (in `src/observer.rs`, `#[cfg(feature = "metrics")]`, re-exported in the prelude) — implements `WorkflowObserver`; the user attaches it with `Workflow::with_observer(Arc::new(MetricsObserver::new()))`. Emits the 6-ish counters the 8 observer hooks naturally support.
2. **Direct instrumentation** (`#[cfg(feature = "metrics")]` blocks calling thin helpers in a new `src/metrics.rs` module) — at the spots the observer doesn't reach: `workflow.rs`/`workflow/execution.rs` (run outcome/duration, active-workflows gauge, per-state task duration, split-branch results), `circuit.rs` (transitions/acquires/outcomes), `scheduler/loops.rs` (flow runs/durations, backoff/trip counters, active-flows gauge), `task/poll.rs`/`task/batch.rs`/`task/stepped.rs` + `workflow/execution.rs` (poll iterations, batch item outcomes, step iterations), `task/retry.rs` (per-attempt outcome, circuit-rejection), `recovery.rs` integration points (checkpoint append/clear outcomes), `workflow/compensation.rs` (compensation-run outcomes, drain outcomes).

`src/metrics.rs` is the single source of truth for metric names and the (deliberately minimal) label vocabularies. Histograms record f64 seconds. RAII guards manage the in-flight gauges so early returns can't desync them. `metrics` calls compile to cheap no-ops when no recorder is installed, but the feature adds a small per-state-transition cost (formatting the state label) — so it stays opt-in. Behaviour is byte-for-byte unchanged when the feature is off.

**Tech Stack:** Rust 2024 (MSRV 1.89), `metrics` crate (facade, resolves to 0.24.x), `metrics-util` (dev-dependency: `DebuggingRecorder` for assertions + the bundled example, resolves to 0.20.x), Tokio, the `cano` workspace (members `cano`, `cano-macros`, `cano-e2e`).

**Note on a prior attempt:** there is a branch `metrics-stale-base` carrying an earlier metrics implementation that was built against an older `main` (before observer hooks / recovery / saga / the new processing models). It's a useful reference — `git show metrics-stale-base:cano/src/metrics.rs` is roughly the starting point for the `metrics.rs` module here — but the codebase has moved significantly, so this plan is a rewrite, not a refresh. Do **not** merge/rebase that branch.

---

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `cano/Cargo.toml` | modify | Add `metrics` optional dep, `metrics-util` dev-dep, `metrics` feature, extend `all`, register `metrics_demo` example |
| `cano/src/metrics.rs` | **create** | All metric names, `describe()`, the thin emission helpers, `WorkflowActiveGuard` / `SchedulerFlowActiveGuard`, `state_label()`, and `#[cfg(test)]` test-support (recorder harness + snapshot assertions) |
| `cano/src/lib.rs` | modify | `#[cfg(feature = "metrics")] pub mod metrics;` + module-overview doc line + re-export `MetricsObserver` in the prelude (gated) |
| `cano/src/observer.rs` | modify | Add `MetricsObserver` (a `WorkflowObserver` impl behind `#[cfg(feature = "metrics")]`, mirroring `TracingObserver`) |
| `cano/src/workflow.rs` | modify | Instrument the orchestrate/run path: active-workflows gauge + run outcome counter + run duration histogram |
| `cano/src/workflow/execution.rs` | modify | Per-state task-dispatch duration histogram (single/router/split/compensatable/stepped); split-branch result counter; step-iteration counter (engine-owned stepped loop) |
| `cano/src/workflow/compensation.rs` | modify | Compensation-run outcome counter + compensation-drain outcome counter |
| `cano/src/task/retry.rs` | modify | Per-attempt outcome counter; circuit-rejection counter |
| `cano/src/task/poll.rs` | modify | Poll-iteration counter (ready/pending) in `run_poll_loop` |
| `cano/src/task/batch.rs` | modify | Batch-run counter + per-item outcome counter in `run_batch` |
| `cano/src/task/stepped.rs` | modify | Step-iteration counter in `run_stepped` (the in-memory loop used outside `register_stepped`) |
| `cano/src/circuit.rs` | modify | Circuit transitions / acquires / outcomes counters |
| `cano/src/scheduler/loops.rs` | modify | Scheduler flow-run outcome counter + flow-run duration histogram + backoff/trip counters + active-flows gauge |
| `cano/src/recovery.rs` | modify (light) | A tiny `pub(crate)` helper or direct counter call wired from the engine's `append`/`clear` sites — see Task 11 |
| `cano/examples/metrics_demo.rs` | **create** | Runnable example: attach a `MetricsObserver`, install a `DebuggingRecorder`, run a workflow + a scheduler, print every metric Cano emitted |
| `README.md` | modify | Mention the `metrics` feature in the Observability bullet |

`src/metrics.rs` is the single point of truth for metric names and label vocabularies; every instrumentation site (including `MetricsObserver`) calls one of its helpers rather than touching the `metrics` macros directly.

**General rule for every "modify" task below:** grep/read the target file to find the function named in the task (the exact line numbers in this plan are approximate). Weave in the `#[cfg(feature = "metrics")]` lines shown verbatim; preserve everything else. **When the `metrics` feature is OFF, the modified function's behaviour must be byte-for-byte what it was.** If a restructure is needed (e.g. to capture an outcome before `?` consumes it), make it behaviour-preserving and note it.

---

### Task 1: Add the `metrics` dependency, feature flag, and example registration

**Files:** Modify `cano/Cargo.toml`

- [ ] **Step 1: Add the dependency, feature, and dev-dependency**

From the repo root:

```bash
cargo add metrics --package cano --optional
cargo add metrics-util --package cano --dev --features debugging
```

Then confirm `cano/Cargo.toml` looks like:

In `[dependencies]`, alongside the other optional deps:
```toml
metrics = { version = "0.24", optional = true }
```

In `[features]` (note `all` currently is `["scheduler", "tracing", "recovery"]` — keep `recovery`):
```toml
[features]
scheduler = ["dep:chrono", "dep:cron", "tokio/signal"]
tracing = ["dep:tracing"]
recovery = ["dep:redb", "dep:postcard"]
metrics = ["dep:metrics"]
all = ["scheduler", "tracing", "recovery", "metrics"]
```

In `[dev-dependencies]`:
```toml
metrics-util = { version = "0.20", features = ["debugging"] }
```

(`cargo add` will pick exact patch versions — leave whatever it resolves; as of writing that's `metrics 0.24.5` / `metrics-util 0.20.3`. The API this plan uses — `metrics::with_local_recorder`, `DebuggingRecorder`, `Snapshot::into_vec`, `DebugValue` — is stable across `metrics ≥ 0.23` / `metrics-util ≥ 0.18`. If `cargo add` writes a `[dependencies.metrics]` table form, that's fine — just keep `optional = true` / `features = ["debugging"]`.)

- [ ] **Step 2: Register the `metrics_demo` example**

In `cano/Cargo.toml`, next to the `tracing_demo` `[[example]]` stanza:
```toml
[[example]]
name = "metrics_demo"
path = "examples/metrics_demo.rs"
required-features = ["metrics", "scheduler"]
```
(The file `cano/examples/metrics_demo.rs` is created in Task 12 — an `[[example]]` pointing at a not-yet-existing file does not break `cargo check`/`build` of the library or other examples. Do not create it now.)

- [ ] **Step 3: Verify**

Run: `cargo check --package cano --features metrics` → PASS.
Run: `cargo check --package cano --no-default-features` → PASS.

- [ ] **Step 4: Commit**

```bash
git add cano/Cargo.toml Cargo.lock
git commit -m "build: add optional metrics feature and metrics-util dev-dep"
```
(Append a trailer line: `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`)

---

### Task 2: Create the `src/metrics.rs` module — names, `describe()`, helpers, RAII guards, test harness

**Files:** Create `cano/src/metrics.rs`; modify `cano/src/lib.rs`; test inline in `cano/src/metrics.rs`.

- [ ] **Step 1: Write the module**

(A useful reference is `git show metrics-stale-base:cano/src/metrics.rs` — this is an extended version of it.) Create `cano/src/metrics.rs` with exactly this content:

```rust
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
    counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram, Unit,
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
    describe_counter!(STATE_ENTERS_TOTAL, Unit::Count, "FSM state entries, by state (emitted by MetricsObserver)");
    describe_counter!(OBSERVED_TASK_RUNS_TOTAL, Unit::Count, "Task dispatches, by task and outcome (completed|failed) (emitted by MetricsObserver)");
    describe_counter!(TASK_RETRIES_TOTAL, Unit::Count, "Retry attempts, by task (emitted by MetricsObserver)");
    describe_counter!(CIRCUIT_OPEN_EVENTS_TOTAL, Unit::Count, "Times a task was short-circuited by an open breaker, by task (emitted by MetricsObserver)");
    describe_counter!(CHECKPOINTS_OBSERVED_TOTAL, Unit::Count, "Checkpoint rows durably appended (emitted by MetricsObserver)");
    describe_counter!(RESUMES_TOTAL, Unit::Count, "Workflow resumes from a checkpoint store (emitted by MetricsObserver)");

    describe_counter!(WORKFLOW_RUNS_TOTAL, Unit::Count, "Workflow runs (via Workflow::orchestrate/resume_from), by terminal outcome (completed|failed|timeout)");
    describe_histogram!(WORKFLOW_DURATION_SECONDS, Unit::Seconds, "Wall-clock duration of a workflow run, by terminal outcome");
    describe_gauge!(WORKFLOW_ACTIVE, Unit::Count, "Workflows currently executing");
    describe_histogram!(TASK_DURATION_SECONDS, Unit::Seconds, "Duration of a state-handler dispatch including retries, by state and kind (single|router|split|compensatable|stepped)");
    describe_counter!(TASK_ATTEMPTS_TOTAL, Unit::Count, "Individual task attempts inside the retry loop, by outcome (completed|failed)");
    describe_counter!(CIRCUIT_REJECTIONS_TOTAL, Unit::Count, "Task attempts short-circuited by an open circuit breaker in the retry loop");
    describe_counter!(SPLIT_BRANCH_RESULTS_TOTAL, Unit::Count, "Per-branch outcomes of split/join states, by result (success|failure|cancelled)");
    describe_counter!(CIRCUIT_TRANSITIONS_TOTAL, Unit::Count, "Circuit-breaker state transitions, by transition (closed_to_open|open_to_halfopen|halfopen_to_closed|halfopen_to_open)");
    describe_counter!(CIRCUIT_ACQUIRES_TOTAL, Unit::Count, "Circuit-breaker permit acquisitions, by result (acquired|rejected)");
    describe_counter!(CIRCUIT_OUTCOMES_TOTAL, Unit::Count, "Outcomes recorded against a circuit breaker, by outcome (success|failure)");
    describe_counter!(POLL_ITERATIONS_TOTAL, Unit::Count, "PollTask poll() calls, by outcome (ready|pending)");
    describe_counter!(BATCH_RUNS_TOTAL, Unit::Count, "BatchTask load→process→finish dispatches, by outcome (completed|failed)");
    describe_counter!(BATCH_ITEMS_TOTAL, Unit::Count, "BatchTask items processed, by result (ok|err)");
    describe_counter!(STEP_ITERATIONS_TOTAL, Unit::Count, "SteppedTask step() calls, by outcome (more|done)");
    describe_counter!(CHECKPOINT_APPENDS_TOTAL, Unit::Count, "CheckpointStore::append calls from the engine, by result (ok|err)");
    describe_counter!(CHECKPOINT_CLEARS_TOTAL, Unit::Count, "CheckpointStore::clear calls from the engine (best-effort on success), by result (ok|err)");
    describe_counter!(COMPENSATIONS_RUN_TOTAL, Unit::Count, "compensate() invocations during a rollback drain, by result (ok|err)");
    describe_counter!(COMPENSATION_DRAINS_TOTAL, Unit::Count, "Compensation-stack drains, by outcome (clean|partial)");

    #[cfg(feature = "scheduler")]
    {
        describe_counter!(SCHEDULER_FLOW_RUNS_TOTAL, Unit::Count, "Scheduled-flow runs, by flow and outcome (completed|failed)");
        describe_histogram!(SCHEDULER_FLOW_DURATION_SECONDS, Unit::Seconds, "Duration of a scheduled-flow run, by flow");
        describe_counter!(SCHEDULER_FLOW_BACKOFF_TOTAL, Unit::Count, "Times a scheduled flow entered a backoff window, by flow");
        describe_counter!(SCHEDULER_FLOW_TRIPPED_TOTAL, Unit::Count, "Times a scheduled flow tripped (hit its streak limit), by flow");
        describe_gauge!(SCHEDULER_ACTIVE_FLOWS, Unit::Count, "Scheduled flows currently executing");
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
    histogram!(TASK_DURATION_SECONDS, "state" => state.to_owned(), "kind" => kind).record(dur.as_secs_f64());
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
    counter!(TASK_ATTEMPTS_TOTAL, "outcome" => if ok { "completed" } else { "failed" }).increment(1);
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
    counter!(POLL_ITERATIONS_TOTAL, "outcome" => if ready { "ready" } else { "pending" }).increment(1);
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
    counter!(COMPENSATION_DRAINS_TOTAL, "outcome" => if clean { "clean" } else { "partial" }).increment(1);
}

// ----- scheduler -----

#[cfg(feature = "scheduler")]
pub(crate) fn scheduler_flow_run(flow: &str, ok: bool, dur: Duration) {
    counter!(SCHEDULER_FLOW_RUNS_TOTAL, "flow" => flow.to_owned(), "outcome" => if ok { "completed" } else { "failed" }).increment(1);
    histogram!(SCHEDULER_FLOW_DURATION_SECONDS, "flow" => flow.to_owned()).record(dur.as_secs_f64());
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

    fn find<'a>(rows: &'a [SnapshotRow], name: &str, labels: &[(&str, &str)]) -> Option<&'a DebugValue> {
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
    pub(crate) fn counter_opt(rows: &[SnapshotRow], name: &str, labels: &[(&str, &str)]) -> Option<u64> {
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
    pub(crate) fn histogram_count(rows: &[SnapshotRow], name: &str, labels: &[(&str, &str)]) -> usize {
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
```

> **Version note:** `cargo` resolved `metrics-util 0.20.x`. If its `debugging` module API differs from what `test_support` uses (`DebuggingRecorder::new`, `.snapshotter()`, `Snapshot::into_vec()`, `DebugValue::{Counter,Gauge,Histogram}`, `metrics_util::CompositeKey`, `CompositeKey::key()`, `Key::{name,labels}`, `Label::{key,value}`, `OrderedFloat::into_inner()`), check `https://docs.rs/metrics-util/<version>/metrics_util/debugging/` and adjust ONLY the `test_support` module. Also confirm `metrics::with_local_recorder`'s exact signature against the resolved `metrics` version (it's `with_local_recorder(&dyn Recorder, FnOnce) -> T` since 0.23 — `&recorder` coerces) and that `counter!(NAME, "k" => string_value)` accepts a `String` label value (it does — `String: Into<SharedString>`). Iterate on compiler errors.

- [ ] **Step 2: Wire into `lib.rs`**

In `cano/src/lib.rs`, add the module declaration. The module list is roughly `pub mod circuit; pub mod error; pub mod observer; pub mod recovery; pub mod resource; pub mod saga; pub mod store; pub mod task; pub mod workflow;` then `#[cfg(feature = "scheduler")] pub mod scheduler;`. Insert after `pub mod workflow;`:
```rust
#[cfg(feature = "metrics")]
pub mod metrics;
```
And in the `//! ## Module Overview` doc block, add a bullet after the `observer` line (or wherever fits):
```rust
//! - `metrics` (requires `metrics` feature): a `MetricsObserver` plus low-cardinality
//!   counters / histograms / gauges for workflow, task, retry, split/join, circuit-breaker,
//!   scheduler, processing-loop, recovery and saga internals — call `cano::metrics::describe()`
//!   for the full list with help text and units
```
(Don't add an intra-doc `[link]` to `crate::metrics::*` here — it would be a broken link when the feature is off, just like the existing `scheduler` bullet uses plain text.)

- [ ] **Step 3: Verify**

`cargo check --package cano` → PASS. `cargo check --package cano --features metrics` → PASS. `cargo test --package cano --lib --features metrics metrics::` → 2 tests PASS. Then `cargo fmt --all` and re-verify.

- [ ] **Step 4: Commit**
```bash
git add cano/src/metrics.rs cano/src/lib.rs
git commit -m "feat(metrics): add metrics module with names, describe(), helpers, test harness"
```
(Trailer as in Task 1.)

---

### Task 3: Add `MetricsObserver` (mirrors `TracingObserver`)

**Files:** Modify `cano/src/observer.rs`; modify `cano/src/lib.rs` (prelude + crate re-export); test inline in `cano/src/observer.rs`.

Follow TDD.

- [ ] **Step 1: Write the failing test**

Read `cano/src/observer.rs` first — it has a `WorkflowObserver` trait (hooks `on_state_enter(&str)`, `on_task_start(&str)`, `on_task_success(&str)`, `on_task_failure(&str, &CanoError)`, `on_retry(&str, u32)`, `on_circuit_open(&str)`, `on_checkpoint(&str, u64)`, `on_resume(&str, u64)`, all defaulted no-op), a `#[cfg(feature = "tracing")] TracingObserver` impl, and a `#[cfg(test)] mod tests` with a `RecordingObserver`. Append a metrics test module after the existing tests:

```rust
#[cfg(all(test, feature = "metrics"))]
mod metrics_observer_tests {
    use super::*;
    use crate::metrics::test_support::*;
    use crate::prelude::*;
    use std::sync::Arc;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        Start,
        Mid,
        Done,
    }

    struct GoTo(S);
    #[crate::task]
    impl Task<S> for GoTo {
        fn name(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed("GoTo")
        }
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(self.0.clone()))
        }
    }

    #[test]
    fn metrics_observer_emits_state_enters_and_task_runs() {
        let (res, rows) = run_with_recorder(|| async {
            Workflow::bare()
                .with_observer(Arc::new(MetricsObserver::new()))
                .register(S::Start, GoTo(S::Mid))
                .register(S::Mid, GoTo(S::Done))
                .add_exit_state(S::Done)
                .orchestrate(S::Start)
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(counter(&rows, "cano_state_enters_total", &[("state", "Start")]), 1);
        assert_eq!(counter(&rows, "cano_state_enters_total", &[("state", "Mid")]), 1);
        assert_eq!(counter_opt(&rows, "cano_state_enters_total", &[("state", "Done")]), None);
        assert_eq!(counter(&rows, "cano_observed_task_runs_total", &[("task", "GoTo"), ("outcome", "completed")]), 2);
    }

    #[test]
    fn metrics_observer_emits_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = Arc::clone(&n);
        struct Flaky(Arc<AtomicUsize>);
        #[crate::task]
        impl Task<S> for Flaky {
            fn name(&self) -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed("Flaky")
            }
            fn config(&self) -> TaskConfig {
                TaskConfig::new().with_fixed_retry(3, std::time::Duration::from_millis(1))
            }
            async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
                if self.0.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(CanoError::task_execution("transient"))
                } else {
                    Ok(TaskResult::Single(S::Done))
                }
            }
        }
        let (res, rows) = run_with_recorder(move || async move {
            Workflow::bare()
                .with_observer(Arc::new(MetricsObserver::new()))
                .register(S::Start, Flaky(n2))
                .add_exit_state(S::Done)
                .orchestrate(S::Start)
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(counter(&rows, "cano_task_retries_total", &[("task", "Flaky")]), 2);
        assert_eq!(counter(&rows, "cano_observed_task_runs_total", &[("task", "Flaky"), ("outcome", "completed")]), 1);
    }
}
```

NOTES: `Workflow::bare()` / `with_observer` / `register` / `add_exit_state` / `orchestrate` are existing APIs (grep `workflow.rs` to confirm `bare()` exists; if not, use `Workflow::new(Resources::new())`). `Task::name()` exists (defaults to `type_name`); overriding it gives stable label values for the test. `#[crate::task]` is the in-crate path to the `task` attr macro — if it doesn't resolve, `use cano_macros::task;` and use `#[task]`. The `on_state_enter` argument is the `Debug` label of the state enum, so `"Start"` etc. The exit state is not "entered". `on_task_success` fires after a successful dispatch; `on_retry` fires once per retry.

- [ ] **Step 2: Run, confirm it fails** — `cargo test --package cano --lib --features metrics observer::metrics_observer_tests` → FAIL (`MetricsObserver` doesn't exist / `cano_state_enters_total` not found).

- [ ] **Step 3: Add `MetricsObserver`**

In `cano/src/observer.rs`, right after the `TracingObserver` block (look for `#[cfg(feature = "tracing")]` ... `impl WorkflowObserver for TracingObserver`), add an analogous metrics observer:

```rust
/// A [`WorkflowObserver`] that re-emits each hook as a metric (counter), under the
/// `cano_*` namespace. Attach with `Workflow::with_observer(Arc::new(MetricsObserver::new()))`
/// — the metrics it emits then require this observer to be attached, exactly as `tracing`
/// events from [`TracingObserver`] require attaching that observer.
///
/// Requires the `metrics` feature. Complements (does not replace) the always-on
/// `#[cfg(feature = "metrics")]` direct instrumentation elsewhere in the engine (workflow run
/// duration, circuit internals, scheduler flows, …) — see [`crate::metrics`].
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Default)]
pub struct MetricsObserver;

#[cfg(feature = "metrics")]
impl MetricsObserver {
    /// Construct a `MetricsObserver`.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "metrics")]
impl WorkflowObserver for MetricsObserver {
    fn on_state_enter(&self, state: &str) {
        crate::metrics::observed_state_enter(state);
    }
    fn on_task_success(&self, task_id: &str) {
        crate::metrics::observed_task_run(task_id, true);
    }
    fn on_task_failure(&self, task_id: &str, _err: &CanoError) {
        crate::metrics::observed_task_run(task_id, false);
    }
    fn on_retry(&self, task_id: &str, _attempt: u32) {
        crate::metrics::observed_task_retry(task_id);
    }
    fn on_circuit_open(&self, task_id: &str) {
        crate::metrics::observed_circuit_open(task_id);
    }
    fn on_checkpoint(&self, _workflow_id: &str, _sequence: u64) {
        crate::metrics::observed_checkpoint();
    }
    fn on_resume(&self, _workflow_id: &str, _sequence: u64) {
        crate::metrics::observed_resume();
    }
    // on_task_start: intentionally not emitted — on_task_success/on_task_failure already
    // count each dispatch by outcome; a separate "start" counter would just be their sum.
}
```

(Match the surrounding style — if `TracingObserver` derives `Debug, Default, Clone` or has a `new()`, mirror that. If `WorkflowObserver` is in scope as `self::WorkflowObserver` already in that file, no extra import needed; `CanoError` is presumably already imported.)

- [ ] **Step 4: Re-export `MetricsObserver`**

In `cano/src/lib.rs`: in the crate-level re-exports, find where `observer::WorkflowObserver` is re-exported (and the `#[cfg(feature = "tracing")] pub use ...::TracingObserver;` if it's at crate level — check). Add:
```rust
#[cfg(feature = "metrics")]
pub use observer::MetricsObserver;
```
right next to it. And in the `prelude` module, next to `#[cfg(feature = "tracing")] pub use crate::TracingObserver;`, add:
```rust
#[cfg(feature = "metrics")]
pub use crate::MetricsObserver;
```

- [ ] **Step 5: Run, confirm pass** — `cargo test --package cano --lib --features metrics observer::metrics_observer_tests` → PASS. `cargo test --package cano --lib` → PASS (no regressions). `cargo check --package cano` → PASS. Then `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 6: Commit**
```bash
git add cano/src/observer.rs cano/src/lib.rs
git commit -m "feat(metrics): add MetricsObserver re-emitting WorkflowObserver hooks as metrics"
```
(Trailer.)

---

### Task 4: Instrument workflow runs — active gauge, run outcome counter, duration histogram

**Files:** Modify `cano/src/workflow.rs` (the orchestrate→execute path); test inline in `cano/src/workflow.rs`.

Follow TDD.

- [ ] **Step 1: Failing test** — add a `#[cfg(all(test, feature = "metrics"))] mod metrics_tests` near the bottom of `cano/src/workflow.rs`:
```rust
#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use crate::metrics::test_support::*;
    use crate::prelude::*;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S { Start, Mid, Done }
    struct GoTo(S);
    #[crate::task]
    impl Task<S> for GoTo {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> { Ok(TaskResult::Single(self.0.clone())) }
    }
    struct Boom;
    #[crate::task]
    impl Task<S> for Boom {
        fn config(&self) -> TaskConfig { TaskConfig::minimal() }
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> { Err(CanoError::task_execution("boom")) }
    }
    fn ok_workflow() -> Workflow<S> {
        Workflow::bare().register(S::Start, GoTo(S::Mid)).register(S::Mid, GoTo(S::Done)).add_exit_state(S::Done)
    }

    #[test]
    fn successful_run_records_outcome_duration_and_clears_active_gauge() {
        let (res, rows) = run_with_recorder(|| async { ok_workflow().orchestrate(S::Start).await });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(counter(&rows, "cano_workflow_runs_total", &[("outcome", "completed")]), 1);
        assert_eq!(histogram_count(&rows, "cano_workflow_duration_seconds", &[("outcome", "completed")]), 1);
        assert_eq!(gauge(&rows, "cano_workflow_active", &[]), 0.0);
    }
    #[test]
    fn failed_run_records_failed_outcome() {
        let (res, rows) = run_with_recorder(|| async {
            Workflow::bare().register(S::Start, Boom).add_exit_state(S::Done).orchestrate(S::Start).await
        });
        assert!(res.is_err());
        assert_eq!(counter(&rows, "cano_workflow_runs_total", &[("outcome", "failed")]), 1);
        assert_eq!(gauge(&rows, "cano_workflow_active", &[]), 0.0);
    }
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test --package cano --lib --features metrics workflow::metrics_tests` → FAIL.

- [ ] **Step 3: Instrument**

In `cano/src/workflow.rs`, find the function that wraps the FSM execution with the optional `workflow_timeout` and runs lifecycle setup/teardown (likely `run_workflow` and/or `orchestrate`; also note `resume_from` lives in `workflow/compensation.rs` and reaches the same `execute_workflow_from` — for this task, instrument the *common* run wrapper if there's one, otherwise instrument `orchestrate`). The goal: at the start of a run, create a `WorkflowActiveGuard` and record a start `Instant`; on the way out, call `crate::metrics::workflow_run(outcome, elapsed)` where `outcome` is `"completed"` on `Ok`, `"timeout"` on the timeout-error path, `"failed"` otherwise. Concretely, for a `run_workflow`-shaped function:

```rust
    async fn run_workflow(&self, initial_state: TState) -> Result<TState, CanoError> {
        #[cfg(feature = "metrics")]
        let _active = crate::metrics::WorkflowActiveGuard::new();
        #[cfg(feature = "metrics")]
        let _started = std::time::Instant::now();

        let workflow_future = self.execute_workflow(initial_state);
        let result = if let Some(timeout_duration) = self.workflow_timeout {
            match tokio::time::timeout(timeout_duration, workflow_future).await {
                Ok(inner) => inner,
                Err(_) => {
                    #[cfg(feature = "metrics")]
                    crate::metrics::workflow_run("timeout", _started.elapsed());
                    return Err(CanoError::workflow("Workflow timeout exceeded"));
                }
            }
        } else {
            workflow_future.await
        };
        #[cfg(feature = "metrics")]
        crate::metrics::workflow_run(if result.is_ok() { "completed" } else { "failed" }, _started.elapsed());
        result
    }
```

If the real function differs (different name, the timeout handling elsewhere, lifecycle wrapping in `orchestrate`), preserve behaviour and just (a) put a `WorkflowActiveGuard` + `Instant` at the top of the function that represents "a workflow run is executing", (b) emit `workflow_run` on every exit path with the right `outcome`. Make sure `resume_from` (in `workflow/compensation.rs`) also counts as a workflow run — if it doesn't share the `run_workflow` wrapper, add the same `_active`/`_started`/`workflow_run` instrumentation there too (its successful outcome is `"completed"`). Without `metrics`, behaviour is byte-for-byte unchanged.

- [ ] **Step 4: Run, confirm pass** — `cargo test --package cano --lib --features metrics workflow::metrics_tests` → PASS. `cargo test --package cano --lib` → PASS. `cargo check --package cano` → PASS. Then `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 5: Commit** — `git add` the changed files; `git commit -m "feat(metrics): record workflow run outcome, duration, and active gauge"` (trailer).

---

### Task 5: Instrument per-state task durations + split-branch results

**Files:** Modify `cano/src/workflow/execution.rs`; test in `cano/src/workflow.rs`'s `metrics_tests` module (append).

Follow TDD.

- [ ] **Step 1: Append failing tests** to `mod metrics_tests` in `cano/src/workflow.rs` (reuses `S`, `GoTo`, `Boom`, `ok_workflow`, `#[task]` import already there):
```rust
    #[test]
    fn per_state_task_durations_are_recorded_single_and_split() {
        let (res, rows) = run_with_recorder(|| async { ok_workflow().orchestrate(S::Start).await });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(histogram_count(&rows, "cano_task_duration_seconds", &[("state", "Start"), ("kind", "single")]), 1);
        assert_eq!(histogram_count(&rows, "cano_task_duration_seconds", &[("state", "Mid"), ("kind", "single")]), 1);
    }

    #[derive(Clone)]
    struct Branch { fail: bool }
    #[crate::task]
    impl Task<S> for Branch {
        fn config(&self) -> TaskConfig { TaskConfig::minimal() }
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            if self.fail { Err(CanoError::task_execution("nope")) } else { Ok(TaskResult::Single(S::Done)) }
        }
    }
    #[test]
    fn split_records_branch_results_and_a_split_kind_duration() {
        let (res, rows) = run_with_recorder(|| async {
            Workflow::bare()
                .register_split(S::Start, vec![Branch{fail:false}, Branch{fail:true}, Branch{fail:false}],
                                JoinConfig::new(JoinStrategy::PartialResults(2), S::Done))
                .add_exit_state(S::Done)
                .orchestrate(S::Start)
                .await
        });
        assert_eq!(res.unwrap(), S::Done);
        assert_eq!(counter(&rows, "cano_split_branch_results_total", &[("result", "success")]), 2);
        assert_eq!(counter(&rows, "cano_split_branch_results_total", &[("result", "failure")]), 1);
        assert_eq!(counter_opt(&rows, "cano_split_branch_results_total", &[("result", "cancelled")]), None);
        assert_eq!(histogram_count(&rows, "cano_task_duration_seconds", &[("state", "Start"), ("kind", "split")]), 1);
    }
```
(`register_split` is generic over a single `T: Task`, so the `Vec` must be homogeneous — hence the data-driven `Branch`. Confirm `JoinConfig::new` / `JoinStrategy::PartialResults` shapes by grepping `workflow/join.rs`; adjust to the real API — the assertion targets don't change. If `register_split` / `JoinStrategy` aren't available without a feature, they should be in base; verify.)

- [ ] **Step 2: Run, confirm fail.**

- [ ] **Step 3: Instrument `workflow/execution.rs`**

Read `cano/src/workflow/execution.rs`. In `execute_workflow_from` (the FSM loop), each iteration looks up the `StateEntry` for `current_state` and dispatches. The loop already fires `on_state_enter` (observer) — that handles the state-enter count. For *durations*: around the per-state dispatch (the `match entry.as_ref() { StateEntry::Single {..} => ..., StateEntry::Router {..} => ..., StateEntry::Split {..} => ..., StateEntry::CompensatableSingle {..} => ..., StateEntry::Stepped {..} => ... }` that produces the next state), capture the state label and a start `Instant` *before*, and after the dispatch call `crate::metrics::task_dispatch_duration(&state_label, kind, started.elapsed())` where `kind` is `"single"|"router"|"split"|"compensatable"|"stepped"` matching the arm. Concretely, wrap the dispatch:

```rust
            #[cfg(feature = "metrics")]
            let _state_label = crate::metrics::state_label(&current_state);
            #[cfg(feature = "metrics")]
            let _dispatch_started = std::time::Instant::now();

            // ... existing dispatch: `let next_state = match entry.as_ref() { ... }?;`
            // — capture it into a Result first if `?` would otherwise consume it before
            //   the metrics call, then re-`?` after; OR if the existing code already binds
            //   `next_state` without `?` (handling Err separately), just add the metrics call
            //   after the dispatch on both paths.

            #[cfg(feature = "metrics")]
            crate::metrics::task_dispatch_duration(
                &_state_label,
                match entry.as_ref() {
                    StateEntry::Single { .. } => "single",
                    StateEntry::Router { .. } => "router",
                    StateEntry::Split { .. } => "split",
                    StateEntry::CompensatableSingle { .. } => "compensatable",
                    StateEntry::Stepped { .. } => "stepped",
                },
                _dispatch_started.elapsed(),
            );
```
(`entry` is a borrow from `self.states.get(...)` that's valid for the iteration; re-matching it for the `kind` string is fine since the dispatch arms `.clone()`/`Arc::clone` rather than moving out of `entry`. Match the actual `StateEntry` variant names by reading the file. Without `metrics`, behaviour unchanged — if you had to restructure `... = match {...}?` into `let r = match {...}; ...; r?`, that's behaviour-preserving.)

For **split-branch results**: in the split-handling code (in `execution.rs`, the `StateEntry::Split` arm calls something like `collect_results` and gets a `SplitResult` with `.successes` / `.errors` / `.cancelled` `Vec`s). Right after that result is available, add:
```rust
            #[cfg(feature = "metrics")]
            crate::metrics::split_branch_results(split_result.successes.len(), split_result.errors.len(), split_result.cancelled.len());
```
(Match the actual field names — grep `SplitResult` in `workflow/join.rs` or `workflow/execution.rs`. If the names are e.g. `.failures` / `.cancelled_count()`, adapt — the helper just wants the three counts.)

- [ ] **Step 4: Run, confirm pass** — the workflow `metrics_tests` all green; `cargo test --package cano --lib` green; `cargo check --package cano` green. `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 5: Commit** — `git add cano/src/workflow/execution.rs cano/src/workflow.rs`; `git commit -m "feat(metrics): record per-state task durations and split-branch results"` (trailer).

---

### Task 6: Instrument the retry loop — per-attempt outcome + circuit rejections

**Files:** Modify `cano/src/task/retry.rs` (`run_with_retries`); test inline in `cano/src/task/retry.rs`.

Follow TDD.

- [ ] **Step 1: Failing test** — add a `#[cfg(all(test, feature = "metrics"))] mod metrics_tests` near the bottom of `cano/src/task/retry.rs`:
```rust
#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use super::*;
    use crate::metrics::test_support::*;
    use crate::circuit::{CircuitBreaker, CircuitPolicy};
    use crate::error::CanoError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn retry_loop_counts_attempts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let config = TaskConfig::new().with_fixed_retry(3, Duration::from_millis(1));
        let (res, rows) = run_with_recorder(move || async move {
            run_with_retries::<u8, _, _>(&config, move || {
                let n = calls2.fetch_add(1, Ordering::SeqCst);
                async move { if n < 2 { Err(CanoError::task_execution("transient")) } else { Ok(7u8) } }
            }).await
        });
        assert_eq!(res.unwrap(), 7);
        assert_eq!(counter(&rows, "cano_task_attempts_total", &[("outcome", "failed")]), 2);
        assert_eq!(counter(&rows, "cano_task_attempts_total", &[("outcome", "completed")]), 1);
        assert_eq!(counter_opt(&rows, "cano_circuit_rejections_total", &[]), None);
    }

    #[test]
    fn open_breaker_records_a_circuit_rejection() {
        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy { failure_threshold: 1, reset_timeout: Duration::from_secs(60), half_open_max_calls: 1 }));
        let p = breaker.try_acquire().unwrap();
        breaker.record_failure(p);
        let config = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));
        let (res, rows) = run_with_recorder(move || async move {
            run_with_retries::<u8, _, _>(&config, || async { Ok(1u8) }).await
        });
        assert!(matches!(res, Err(CanoError::CircuitOpen(_))));
        assert_eq!(counter(&rows, "cano_circuit_rejections_total", &[]), 1);
        assert_eq!(counter_opt(&rows, "cano_task_attempts_total", &[("outcome", "completed")]), None);
    }
}
```
(`run_with_retries<TState, F, Fut>(config: &TaskConfig, run_fn: F)` where `F: Fn() -> Fut`, `Fut: Future<Output = Result<TState, CanoError>>` — confirm in the file. `CircuitBreaker`/`CircuitPolicy`/`try_acquire`/`record_failure`/`TaskConfig::{minimal,with_circuit_breaker,with_fixed_retry}` are existing APIs. `#[test]` plain sync — `run_with_recorder` builds its own runtime.)

- [ ] **Step 2: Run, confirm fail.**

- [ ] **Step 3: Instrument `run_with_retries`** in `cano/src/task/retry.rs`. Two additions:

(a) On the breaker `try_acquire()` `Err` arm — where it currently does `notify_observers(config, |o, name| o.on_circuit_open(name)); return Err(e);` — add a `circuit_rejection()` call:
```rust
                Err(e) => {
                    notify_observers(config, |o, name| o.on_circuit_open(name));
                    #[cfg(feature = "metrics")]
                    crate::metrics::circuit_rejection();
                    return Err(e);
                }
```
(Match the actual code around there — preserve the observer notify; just add the `#[cfg(feature="metrics")]` line.)

(b) Right after the attempt's outcome is computed and the breaker permit recorded (the `if let (Some(b), Some(p)) = (breaker, permit) { match &attempt_outcome { Ok(_) => b.record_success(p), Err(_) => b.record_failure(p) } }` block), before the `match attempt_outcome { ... }` that decides retry-or-return — add:
```rust
        #[cfg(feature = "metrics")]
        crate::metrics::task_attempt(attempt_outcome.is_ok());
```
(Without `metrics`, `run_with_retries` is byte-for-byte unchanged. Note: the per-retry count `cano_task_retries_total` is emitted by `MetricsObserver` via `on_retry` — don't add a direct retries counter here, it'd double-count when the observer is attached.)

- [ ] **Step 4: Run, confirm pass** — the new tests green; `cargo test --package cano --lib --features metrics` green (esp. the existing retry/circuit tests); `cargo test --package cano --lib` green; `cargo check --package cano` green. `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 5: Commit** — `git add cano/src/task/retry.rs`; `git commit -m "feat(metrics): count retry-loop attempts and circuit rejections"` (trailer).

---

### Task 7: Instrument the circuit breaker — transitions, acquires, outcomes

**Files:** Modify `cano/src/circuit.rs`; test inline in `cano/src/circuit.rs`.

Follow TDD.

- [ ] **Step 1: Failing test** — add a `#[cfg(all(test, feature = "metrics"))] mod metrics_tests` near the bottom of `cano/src/circuit.rs`:
```rust
#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use super::*;
    use crate::metrics::test_support::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn closed_to_open_and_outcomes_are_recorded() {
        let ((), rows) = run_with_recorder(|| async {
            let b = Arc::new(CircuitBreaker::new(CircuitPolicy { failure_threshold: 2, reset_timeout: Duration::from_secs(60), half_open_max_calls: 1 }));
            let p = b.try_acquire().unwrap(); b.record_success(p);
            let p = b.try_acquire().unwrap(); b.record_failure(p);
            let p = b.try_acquire().unwrap(); b.record_failure(p);
            assert!(b.try_acquire().is_err());
        });
        assert_eq!(counter(&rows, "cano_circuit_acquires_total", &[("result", "acquired")]), 3);
        assert_eq!(counter(&rows, "cano_circuit_acquires_total", &[("result", "rejected")]), 1);
        assert_eq!(counter(&rows, "cano_circuit_outcomes_total", &[("outcome", "success")]), 1);
        assert_eq!(counter(&rows, "cano_circuit_outcomes_total", &[("outcome", "failure")]), 2);
        assert_eq!(counter(&rows, "cano_circuit_transitions_total", &[("transition", "closed_to_open")]), 1);
    }
    #[test]
    fn dropped_unconsumed_permit_counts_as_failure() {
        let ((), rows) = run_with_recorder(|| async {
            let b = Arc::new(CircuitBreaker::new(CircuitPolicy::default()));
            let _p = b.try_acquire().unwrap();
        });
        assert_eq!(counter(&rows, "cano_circuit_outcomes_total", &[("outcome", "failure")]), 1);
    }
}
```

- [ ] **Step 2: Run, confirm fail.**

- [ ] **Step 3: Instrument `circuit.rs`** — same edits as the prior implementation (read the file; the structure should be similar):
  - `try_acquire`: restructure the final `match inner.state { ... }` so it produces a `Result` value (`HalfOpen` arm's early `return Err(...)` → `else { ... }` yielding the `Err`); then under `#[cfg(feature = "metrics")] { drop(inner); crate::metrics::circuit_acquire(if result.is_ok() {"acquired"} else {"rejected"}); }`; then `result`. Without `metrics`, `inner` is held to function end exactly as before.
  - `record_success`: add `#[cfg(feature = "metrics")] crate::metrics::circuit_outcome("success");` right after `permit.consumed = true;`.
  - `do_record_failure`: add `#[cfg(feature = "metrics")] crate::metrics::circuit_outcome("failure");` as the first statement (this also covers `Permit::drop` of an unconsumed permit, which routes through `do_record_failure`).
  - The 4 `inner.transition(...)` sites (each next to a `#[cfg(feature = "tracing")] info!(...)`): add `#[cfg(feature = "metrics")] crate::metrics::circuit_transition("...")` after each — `"open_to_halfopen"` (Open→HalfOpen lazy transition in `try_acquire`), `"halfopen_to_closed"` (in `record_success`), `"closed_to_open"` (in `do_record_failure`, Closed arm), `"halfopen_to_open"` (in `do_record_failure`, HalfOpen arm).
  Without `metrics`, `circuit.rs` behaviour is byte-for-byte unchanged (the `try_acquire` restructure is behaviour-preserving).

- [ ] **Step 4: Run, confirm pass** — new tests green; existing circuit tests green; `cargo check --package cano` green. `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 5: Commit** — `git add cano/src/circuit.rs`; `git commit -m "feat(metrics): record circuit-breaker transitions, acquires, and outcomes"` (trailer).

---

### Task 8: Instrument the poll/batch/stepped processing loops

**Files:** Modify `cano/src/task/poll.rs` (`run_poll_loop`), `cano/src/task/batch.rs` (`run_batch`), `cano/src/task/stepped.rs` (`run_stepped`), and `cano/src/workflow/execution.rs` (the engine-owned stepped loop, `execute_stepped_task` or similar — so `register_stepped` flows also get step counts). Test inline in each file (or one combined test module per file).

Follow TDD. Implement per-loop; commit once at the end.

- [ ] **Step 1: Failing tests** — write small tests exercising each loop with the recorder. Examples (adapt the trait/loop APIs to reality by reading the files):

In `task/poll.rs`:
```rust
#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use super::*;
    use crate::metrics::test_support::*;
    // Build a PollTask whose poll() returns Pending twice then Ready, register it in a
    // Workflow::bare(), orchestrate, and assert:
    //   cano_poll_iterations_total{outcome=pending} == 2
    //   cano_poll_iterations_total{outcome=ready}   == 1
    // (Use a tiny delay_ms so the test is fast. Confirm PollOutcome::{Ready,Pending} shapes.)
}
```
In `task/batch.rs`: a `BatchTask` that loads 3 items, one of which `process_item` errors on, and `finish` returns `Single(Done)` — assert `cano_batch_runs_total{outcome=completed}==1`, `cano_batch_items_total{result=ok}==2`, `cano_batch_items_total{result=err}==1`. (Confirm the `BatchTask` trait shape — per the crate docs it has assoc types `Item`/`ItemOutput`, methods `load`/`process_item`/`finish`, `concurrency()`, `item_retry()`.)
In `task/stepped.rs` (or `execution.rs` for the engine-owned loop): a `SteppedTask` that does `More` twice then `Done` — assert `cano_step_iterations_total{outcome=more}==2`, `cano_step_iterations_total{outcome=done}==1`. Test BOTH paths: register via `register` (uses `run_stepped`) and via `register_stepped` (engine-owned loop in `execution.rs`) if both exist.

- [ ] **Step 2: Run, confirm fail.**

- [ ] **Step 3: Instrument**

  - `run_poll_loop` (in `task/poll.rs`): inside the loop, after each `poll()` call returns, before acting on the outcome — `#[cfg(feature = "metrics")] crate::metrics::poll_iteration(matches!(outcome, PollOutcome::Ready(_)));` (match the actual outcome variant name; if `poll()` returns `Result<PollOutcome<_>, _>`, only count on `Ok` — or count `Err` as a non-ready iteration, your call; simplest: count `poll_iteration(false)` on `Pending`, `poll_iteration(true)` on `Ready`, and don't count on `Err` since the loop exits). Without `metrics`, unchanged.
  - `run_batch` (in `task/batch.rs`): after `load` + the concurrent `process_item` phase produces the `Vec<Result<ItemOutput, CanoError>>` (call it `outputs`), before `finish`: `#[cfg(feature = "metrics")] crate::metrics::batch_items(outputs.iter().filter(|r| r.is_ok()).count(), outputs.iter().filter(|r| r.is_err()).count());`. And at the end of `run_batch`, when the overall result is known: `#[cfg(feature = "metrics")] crate::metrics::batch_run(result.is_ok());` (restructure to `let result = ...; ...; result` if needed — behaviour-preserving). Without `metrics`, unchanged.
  - `run_stepped` (in `task/stepped.rs`): inside the loop, after each `step()` returns its `StepOutcome`: `#[cfg(feature = "metrics")] crate::metrics::step_iteration(matches!(outcome, StepOutcome::Done(_)));` (match the variant; count only on `Ok`). And the engine-owned stepped loop in `workflow/execution.rs` (the `StateEntry::Stepped` handling — `execute_stepped_task` or wherever it loops `step`): the same `step_iteration(...)` call per step. Without `metrics`, unchanged.

- [ ] **Step 4: Run, confirm pass** — new tests green; `cargo test --package cano --lib` green; `cargo check --package cano` green. `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 5: Commit** — `git add cano/src/task/poll.rs cano/src/task/batch.rs cano/src/task/stepped.rs cano/src/workflow/execution.rs`; `git commit -m "feat(metrics): count poll/batch/step iterations and batch item outcomes"` (trailer).

---

### Task 9: Instrument the scheduler — flow runs, durations, backoff, trips, active gauge

**Files:** Modify `cano/src/scheduler/loops.rs` (and wherever the terminal `Status` is written — `apply_outcome` or similar, possibly in `loops.rs` or `running.rs`); test inline in `cano/src/scheduler/loops.rs` or `cano/src/scheduler.rs`.

Follow TDD. (The scheduler module + everything under it is `#[cfg(feature = "scheduler")]`, so test modules there are `#[cfg(all(test, feature = "metrics"))]` with `scheduler` already implied.)

- [ ] **Step 1: Failing test** — add a `#[cfg(all(test, feature = "metrics"))] mod metrics_tests` near the bottom of `cano/src/scheduler.rs` (or `scheduler/loops.rs`):
```rust
#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use crate::metrics::test_support::*;
    use crate::prelude::*;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum St { Start, Done }
    struct Ok1;
    #[crate::task]
    impl Task<St> for Ok1 {
        async fn run_bare(&self) -> Result<TaskResult<St>, CanoError> { Ok(TaskResult::Single(St::Done)) }
    }

    #[test]
    fn manual_trigger_records_a_completed_flow_run() {
        let ((), rows) = run_with_recorder(|| async {
            let wf = Workflow::bare().register(St::Start, Ok1).add_exit_state(St::Done);
            let running = Scheduler::new().manual("flow_a", wf).start().await.unwrap();
            running.trigger("flow_a").await.unwrap();
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            running.stop().await.unwrap();
        });
        assert_eq!(counter(&rows, "cano_scheduler_flow_runs_total", &[("flow", "flow_a"), ("outcome", "completed")]), 1);
        assert_eq!(histogram_count(&rows, "cano_scheduler_flow_duration_seconds", &[("flow", "flow_a")]), 1);
        assert_eq!(gauge(&rows, "cano_scheduler_active_flows", &[]), 0.0);
        assert_eq!(counter_opt(&rows, "cano_scheduler_flow_backoff_total", &[("flow", "flow_a")]), None);
    }
}
```
(Confirm the builder API by reading `scheduler/builder.rs`: `Scheduler::new()`, `.manual(id, workflow)` — note 0.12 `manual`/`every` may take the *initial state* baked into the workflow or as a separate arg; the crate docs show `manual(id, workflow, initial)` in an older form but the new builder may differ — match reality. `.start() -> CanoResult<RunningScheduler>` (async, consumes self), `running.trigger(id)` (async), `running.stop()` (async). `RunningScheduler` is `Clone`. If awaiting a `manual` trigger from a current-thread runtime is flaky, fall back to `every_seconds("flow_a", wf, 1)` + `sleep(1200ms)` — but `stop()` drains in-flight workflows, so the `manual`+`trigger`+`stop` pattern should be deterministic.)

- [ ] **Step 2: Run, confirm fail.**

- [ ] **Step 3: Instrument `scheduler/loops.rs`** — read it. There's a function that, for a reserved/dispatched flow, runs the workflow (calls `workflow.execute_workflow(...)` or similar) and then writes the terminal status (`apply_outcome` or inline). Around that:
  - Before running the workflow: `#[cfg(feature = "metrics")] let _active = crate::metrics::SchedulerFlowActiveGuard::new();` + capture the flow id (read it from the flow's `FlowInfo`, which has `pub id: String`; or it may already be in scope as a `&str`) into `_flow_id` + `let _started = std::time::Instant::now();`.
  - After the workflow returns its `Result`: `#[cfg(feature = "metrics")] crate::metrics::scheduler_flow_run(&_flow_id, result.is_ok(), _started.elapsed());`.
  - In the terminal-status write (`apply_outcome` or wherever `Status::Backoff { .. }` / `Status::Tripped { .. }` are set): add `#[cfg(feature = "metrics")] crate::metrics::scheduler_flow_backoff(&flow_id);` on the Backoff branch and `crate::metrics::scheduler_flow_tripped(&flow_id);` on the Tripped branch (the flow id is available via the `FlowInfo` being mutated — `&info.id` or similar). Note: every flow has a `BackoffPolicy::default()` in 0.12, so a failed run always parks in `Backoff` (or `Tripped` once the streak limit hits) — there's no `Status::Failed` variant.
  Without `metrics`, `scheduler/loops.rs` behaviour is byte-for-byte unchanged.

- [ ] **Step 4: Run, confirm pass** — new test green; `cargo test --package cano --lib --features scheduler` green (esp. backoff/trip/reset tests); `cargo test --package cano --lib` green; `cargo check --package cano --features scheduler` green. `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 5: Commit** — `git add cano/src/scheduler/loops.rs` (+ any other scheduler file touched); `git commit -m "feat(metrics): record scheduled-flow runs, durations, backoff, trips, and active gauge"` (trailer).

---

### Task 10: Instrument recovery checkpoint operations + saga compensation drains

**Files:** Modify `cano/src/workflow/execution.rs` and/or `cano/src/workflow/compensation.rs` (the engine sites that call `CheckpointStore::append`/`clear` and that run the compensation drain); test inline in `cano/src/workflow/compensation.rs` (these tests need a `CheckpointStore` impl — use `RedbCheckpointStore` behind `#[cfg(feature = "recovery")]`, or a tiny in-test in-memory `CheckpointStore` impl if that's simpler — so the test module is `#[cfg(all(test, feature = "metrics", feature = "recovery"))]` if it uses `RedbCheckpointStore`).

Follow TDD. Implement both surfaces; commit once.

- [ ] **Step 1: Failing tests** — two scenarios:
  - Recovery: a checkpointed workflow (`with_checkpoint_store(Arc::new(RedbCheckpointStore::new(tmp_path)?))` + `with_workflow_id("wf1")`) that completes — assert `cano_checkpoint_appends_total{result=ok} >= 1` (one per state entered) and `cano_checkpoint_clears_total{result=ok} == 1` (the successful-run clear). (Confirm the `with_checkpoint_store`/`with_workflow_id` API and that a successful run calls `clear`. Use a `tempfile`-ish path — check what the existing recovery tests do; there's `cano/src/recovery/redb.rs` tests and `cano/tests/recovery_e2e.rs`.)
  - Saga: a workflow with a `CompensatableTask` that succeeds, then a later plain task that fails — the compensation drains LIFO and runs `compensate` once. Assert `cano_compensations_run_total{result=ok} == 1` and `cano_compensation_drains_total{outcome=clean} == 1`. (Confirm `register_with_compensation` + `CompensatableTask` shape by reading `saga.rs`. If a clean rollback returns the *original* error, the workflow result is `Err` — that's expected.)

- [ ] **Step 2: Run, confirm fail.**

- [ ] **Step 3: Instrument**
  - Wherever the engine calls `store.append(workflow_id, row).await` (in `execution.rs` — near the `on_checkpoint` notify), wrap the result: `#[cfg(feature = "metrics")] crate::metrics::checkpoint_append(<append result>.is_ok());` — then propagate the result with `?` as before (so capture into a `let` first, emit, then `?`). Similarly wherever `store.clear(workflow_id).await` runs (on a successful run, and after a clean saga rollback): `#[cfg(feature = "metrics")] crate::metrics::checkpoint_clear(<clear result>.is_ok());` (it's best-effort, so the result is probably ignored — capture it just for the metric).
  - In `workflow/compensation.rs`'s `run_compensations` (the LIFO drain): for each compensation entry, after the `compensate` call (which may time out / panic / error — all collected): `#[cfg(feature = "metrics")] crate::metrics::compensation_run(<this compensate's outcome>.is_ok());`. At the end of the drain, when it's known whether it was clean (all `compensate` ok) or partial: `#[cfg(feature = "metrics")] crate::metrics::compensation_drain(<clean?>);`. Match the actual variable names / control flow — preserve everything else. Without `metrics`, behaviour byte-for-byte unchanged.

- [ ] **Step 4: Run, confirm pass** — new tests green; `cargo test --package cano --lib --features metrics,recovery` green; `cargo test --package cano --lib` green; `cargo check --package cano` green; `cargo check --package cano --features recovery` green. `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 5: Commit** — `git add` the touched files; `git commit -m "feat(metrics): count checkpoint store operations and saga compensation drains"` (trailer).

---

### Task 11: (placeholder removed — folded into Task 10)

*(Recovery instrumentation is handled in Task 10. This task number intentionally unused — proceed to Task 12.)*

---

### Task 12: Add the `metrics_demo` example

**Files:** Create `cano/examples/metrics_demo.rs`.

- [ ] **Step 1: Write the example** — create `cano/examples/metrics_demo.rs`:
```rust
//! Demonstrates Cano's `metrics` feature: attach a `MetricsObserver`, run a workflow several
//! times and once under the scheduler, then dump every metric Cano emitted.
//!
//! Run with: `cargo run --example metrics_demo --features metrics,scheduler`
//!
//! In a real application you'd install an exporter here instead of the debugging recorder —
//! e.g. `metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder()` — and call
//! `cano::metrics::describe()` once so help text and units reach your backend. Attaching the
//! `MetricsObserver` is what enables the workflow-level counters (state enters, task runs,
//! retries, …); the engine-internal metrics (workflow run duration, circuit internals,
//! scheduler flows, …) are always-on once the `metrics` feature is compiled in.

use cano::prelude::*;
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Fetch, Process, Done }

struct FetchTask;
#[task]
impl Task<Step> for FetchTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(TaskResult::Single(Step::Process))
    }
}
struct ProcessTask;
#[task]
impl Task<Step> for ProcessTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        tokio::time::sleep(Duration::from_millis(3)).await;
        Ok(TaskResult::Single(Step::Done))
    }
}
fn workflow() -> Workflow<Step> {
    Workflow::bare()
        .with_observer(Arc::new(MetricsObserver::new()))
        .register(Step::Fetch, FetchTask)
        .register(Step::Process, ProcessTask)
        .add_exit_state(Step::Done)
}

#[tokio::main]
async fn main() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::set_global_recorder(recorder).expect("install metrics recorder");
    cano::metrics::describe();

    for _ in 0..3 {
        workflow().orchestrate(Step::Fetch).await.expect("workflow run");
    }

    let running = Scheduler::new()
        .every_seconds("demo_flow", workflow(), 1)
        .start()
        .await
        .expect("start scheduler");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    running.stop().await.expect("stop scheduler");

    println!("\n=== Cano metrics ===");
    let mut rows = snapshotter.snapshot().into_vec();
    rows.sort_by(|a, b| a.0.key().name().cmp(b.0.key().name()));
    for (ck, _unit, _desc, value) in rows {
        let key = ck.key();
        let labels: Vec<String> = key.labels().map(|l| format!("{}={}", l.key(), l.value())).collect();
        let label_str = if labels.is_empty() { String::new() } else { format!("{{{}}}", labels.join(",")) };
        match value {
            DebugValue::Counter(v) => println!("  {}{label_str} = {v}", key.name()),
            DebugValue::Gauge(v) => println!("  {}{label_str} = {}", key.name(), v.into_inner()),
            DebugValue::Histogram(s) => {
                let n = s.len();
                let sum: f64 = s.iter().map(|x| x.into_inner()).sum();
                println!("  {}{label_str} = {{count={n}, sum={sum:.6}s}}", key.name());
            }
        }
    }
}
```
ADAPT: confirm `Scheduler::new().every_seconds(id, workflow, seconds).start()` matches the 0.12 builder (it may be `every_seconds(id, workflow, initial, seconds)` — match reality; also `every_seconds` may or may not be a thing — `every(id, wf, Duration)` definitely is). Confirm `metrics::set_global_recorder` exists in the resolved `metrics` version (it does — returns `Result`). Confirm the `metrics_util::debugging` accessors (same as Task 2's `test_support`). `use metrics;` works in the example because `metrics` is an optional dep and `required-features` ensures it's present (precedent: `tracing_demo` uses `tracing`). `Workflow::with_observer` takes `Arc<dyn WorkflowObserver>` — `Arc::new(MetricsObserver::new())` coerces. Iterate on compiler errors.

- [ ] **Step 2: Verify** — `cargo check --package cano --example metrics_demo --features metrics,scheduler` → PASS; `cargo build ...` → PASS; `cargo run --package cano --example metrics_demo --features metrics,scheduler` → prints a `=== Cano metrics ===` block (with `cano_workflow_runs_total{outcome=completed}=3`, `cano_state_enters_total{state=Fetch}`, `cano_observed_task_runs_total{...}`, `cano_scheduler_flow_runs_total{flow=demo_flow,...}`, `cano_workflow_active=0`, `cano_scheduler_active_flows=0`, etc.) and exits 0 — capture the output. Also `cargo check --package cano --no-default-features` and `cargo check --package cano --all-features` → PASS. `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, re-verify.

- [ ] **Step 3: Commit** — `git add cano/examples/metrics_demo.rs`; `git commit -m "docs(metrics): add metrics_demo example"` (trailer).

---

### Task 13: Documentation, cleanup, and full verification

**Files:** Modify `cano/src/metrics.rs` (remove the temporary `#![allow(dead_code)]`); modify `README.md`.

- [ ] **Step 1: Remove the `#![allow(dead_code)]`** from the top of `cano/src/metrics.rs`. Every helper is now wired into a call site (the `MetricsObserver` helpers from Task 3, the rest from Tasks 4-10), so `clippy --all-features -D warnings` should stay clean. If it reports `dead_code` on a `metrics` helper, that helper's instrumentation site is missing — report DONE_WITH_CONCERNS naming it rather than re-adding the allow.

- [ ] **Step 2: Update the README** — find the Observability bullet in `## Features`:
```markdown
- **Observability**: Integrated `tracing` support for deep insights into workflow execution.
```
Replace with:
```markdown
- **Observability**: Optional `tracing` (spans + events, plus `TracingObserver`) and `metrics` (a `MetricsObserver` plus low-cardinality counters / histograms / gauges via the [`metrics`](https://docs.rs/metrics) facade) features for deep insight into workflow, task, retry, split/join, circuit-breaker, scheduler, processing-loop, recovery and saga internals.
```
(Match the actual text; expand it in the same spirit.)

- [ ] **Step 3: Format, lint, full test matrix** — run each, all must PASS (report each):
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --package cano --lib
cargo test --package cano --lib --all-features
cargo test --package cano --doc --all-features
cargo check --package cano --no-default-features
cargo check --package cano --features metrics
cargo check --package cano --features metrics,scheduler
cargo check --package cano --features metrics,recovery
cargo check --package cano --examples --all-features
```
(If `cargo fmt --all -- --check` reports a diff, `cargo fmt --all` and re-check; include the change in the commit. If `clippy` reports a `dead_code` on a metrics helper, STOP and report DONE_WITH_CONCERNS. If any test fails, STOP and report BLOCKED with output. Don't touch `CLAUDE.md`.)

- [ ] **Step 4: Commit** — `git add cano/src/metrics.rs README.md` (+ anything fmt touched); `git commit -m "docs(metrics): document the metrics feature; drop temporary dead_code allow"` (trailer).

---

## Out of Scope / Follow-up

- **`CLAUDE.md` is not updated by this plan.** It already describes the 0.12 architecture; adding a `metrics` section there is a small follow-up.
- **No per-state label inside `run_with_retries` / the poll/batch/step loops** — those are the deepest hot paths and don't have state context; threading it through their (public) signatures is an API change. The `MetricsObserver` provides per-task retry counts via `on_retry`, which is the practical substitute.
- **`MetricsObserver` does not emit durations** — `WorkflowObserver` gives start/success/failure events but tracking per-task-id durations across concurrent dispatches would be racy in the observer; per-state task durations are instead emitted directly from `workflow/execution.rs` (Task 5).
- **No histogram bucket configuration** — Cano records raw f64-seconds samples; bucketing/quantiles are the exporter's job.
- **The `metrics_demo` example uses the debugging recorder, not a real exporter** — to keep the dev-dependency footprint to one crate (`metrics-util`, which the test harness needs anyway). The doc comment points users at `metrics-exporter-prometheus`.
