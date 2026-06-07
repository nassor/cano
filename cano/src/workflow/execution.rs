//! The forward FSM execution loop: dispatching state entries, running single
//! and split tasks, and collecting split results against a [`JoinStrategy`].
//!
//! [`StateEntry`] lives here because the dispatch `match` in
//! [`execute_workflow_from`](Workflow::execute_workflow_from) is its primary
//! consumer. Compensation drain / resume live in the sibling
//! [`compensation`](super::compensation) module.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;

use crate::cancel::CancellationToken;
use crate::error::CanoError;
use crate::recovery::CheckpointRow;
use crate::saga::{CompensationEntry, ErasedCompensatable};
use crate::task::stepped::{ErasedStep, ErasedSteppedTask};
use crate::task::{Task, TaskResult, run_with_retries};

use super::compensation::resolve_compensation_deadline;
use super::join::{JoinConfig, JoinStrategy, SplitResult, SplitTaskResult};
use super::{Workflow, notify_observers, panic_payload_message};

#[cfg(feature = "tracing")]
use tracing::{debug, info, info_span, warn};

/// Entry in the workflow state machine
pub enum StateEntry<TState, TResourceKey = Cow<'static, str>>
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Single task execution
    Single {
        task: Arc<dyn Task<TState, TResourceKey> + Send + Sync>,
        /// TaskConfig captured at registration time. Avoids per-dispatch
        /// virtual call through dyn Task to retrieve it.
        config: Arc<crate::task::TaskConfig>,
    },
    /// Side-effect-free routing task (registered via
    /// [`Workflow::register_router`](crate::workflow::Workflow::register_router)).
    ///
    /// A router only reads resources and returns the next state — it never
    /// writes. Router states are dispatched like `Single` but skipped by the
    /// checkpoint writer: no [`CheckpointRow`] is appended and no sequence
    /// number is consumed when a router state is entered. The `on_state_enter`
    /// observer is still fired (the FSM did enter the state).
    ///
    /// Note: if the initial state is a router and the workflow crashes before
    /// any non-router state is entered, `resume_from` will find zero rows and
    /// error. Restarting from the initial state is safe — routers have no side
    /// effects.
    Router {
        task: Arc<dyn Task<TState, TResourceKey> + Send + Sync>,
        /// TaskConfig captured at registration time (same rationale as `Single`).
        config: Arc<crate::task::TaskConfig>,
    },
    /// Split into parallel tasks with join configuration
    Split {
        tasks: Vec<Arc<dyn Task<TState, TResourceKey> + Send + Sync>>,
        /// Per-task configs in the same order as `tasks`. Captured at
        /// registration time (see `Single::config` for rationale).
        configs: Arc<Vec<Arc<crate::task::TaskConfig>>>,
        join_config: Arc<JoinConfig<TState>>,
    },
    /// Single task that records an output and can be compensated (see
    /// [`Workflow::register_with_compensation`]).
    CompensatableSingle {
        /// Type-erased compensatable task — exposes the forward run (returning the next
        /// state plus the serialized output) and the compensation.
        task: Arc<dyn ErasedCompensatable<TState, TResourceKey>>,
        /// Forward-run config, captured at registration time (see `Single::config`).
        config: Arc<crate::task::TaskConfig>,
    },
    /// A [`SteppedTask`](crate::task::stepped::SteppedTask) registered via
    /// [`Workflow::register_stepped`](crate::workflow::Workflow::register_stepped).
    ///
    /// The engine drives the step loop, persisting each cursor as a
    /// [`RowKind::StepCursor`](crate::recovery::RowKind::StepCursor) row after each
    /// `StepOutcome::More`. On
    /// [`resume_from`](crate::workflow::Workflow::resume_from), the latest persisted
    /// cursor for this state is rehydrated and passed to the first `step` call instead
    /// of `None`, so processing continues from where it left off.
    Stepped {
        /// Type-erased stepped task — exposes `name`, `config`, and `step`.
        task: Arc<dyn ErasedSteppedTask<TState, TResourceKey>>,
        /// Task config captured at registration time (same rationale as `Single::config`).
        config: Arc<crate::task::TaskConfig>,
    },
}

impl<TState, TResourceKey> Clone for StateEntry<TState, TResourceKey>
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        match self {
            StateEntry::Single { task, config } => StateEntry::Single {
                task: task.clone(),
                config: Arc::clone(config),
            },
            StateEntry::Router { task, config } => StateEntry::Router {
                task: task.clone(),
                config: Arc::clone(config),
            },
            StateEntry::Split {
                tasks,
                configs,
                join_config,
            } => StateEntry::Split {
                tasks: tasks.clone(),
                configs: Arc::clone(configs),
                join_config: join_config.clone(),
            },
            StateEntry::CompensatableSingle { task, config } => StateEntry::CompensatableSingle {
                task: Arc::clone(task),
                config: Arc::clone(config),
            },
            StateEntry::Stepped { task, config } => StateEntry::Stepped {
                task: Arc::clone(task),
                config: Arc::clone(config),
            },
        }
    }
}

fn split_error_summary<TState>(errors: &[SplitTaskResult<TState>]) -> String {
    const MAX_ERRORS_TO_REPORT: usize = 3;

    let mut parts: Vec<String> = errors
        .iter()
        .take(MAX_ERRORS_TO_REPORT)
        .map(|err| match &err.result {
            Ok(_) => format!("task {}: unexpected success in error list", err.task_index),
            Err(e) => format!("task {}: {}", err.task_index, e),
        })
        .collect();

    if errors.len() > MAX_ERRORS_TO_REPORT {
        parts.push(format!(
            "... and {} more error(s)",
            errors.len() - MAX_ERRORS_TO_REPORT
        ));
    }

    parts.join("; ")
}

impl<TState, TResourceKey> Workflow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    pub(crate) async fn execute_workflow(
        &self,
        initial_state: TState,
        total_budget: Option<(std::time::Instant, std::time::Duration)>,
        token: CancellationToken,
    ) -> Result<TState, CanoError> {
        self.execute_workflow_from(
            initial_state,
            0,
            self.workflow_id.clone(),
            Vec::new(),
            HashMap::new(),
            Vec::new(),
            total_budget,
            token,
        )
        .await
    }

    /// The FSM loop, parameterized by the starting checkpoint sequence, the workflow id
    /// to record under, and the (possibly pre-populated, on resume) compensation stack.
    /// `execute_workflow` enters with `(state, 0, self.workflow_id, [])`;
    /// [`resume_from`](Self::resume_from) enters with `(resumed_state, last_sequence + 1,
    /// Some(id), rehydrated_stack)`.
    ///
    /// When a checkpoint store is attached, each iteration writes one [`CheckpointRow`]
    /// for the state being entered — including the initial/resumed state and any terminal
    /// exit state — *before* that state's task runs. A split is one state ⇒ one row.
    /// A successful [compensatable](crate::saga::CompensatableTask) state writes a second,
    /// *completion* row carrying the serialized output and pushes it onto the compensation
    /// stack. The task at the resumed state re-runs on resume; tasks must be idempotent.
    ///
    /// On a terminal failure (a task error, a `Split` returned from a `Single`, a missing
    /// handler, or a checkpoint-append failure) the compensation stack is drained — see
    /// [`run_compensations`](Self::run_compensations). On success against a checkpoint
    /// store, the run's log is cleared.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_workflow_from(
        &self,
        initial_state: TState,
        start_sequence: u64,
        workflow_id: Option<Arc<str>>,
        mut compensation_stack: Vec<CompensationEntry>,
        // Map of `state_label → latest serialized cursor bytes` for `Stepped` states
        // being resumed. Populated by `resume_from`; empty on a fresh run.
        // Entries are consumed via `remove` so visiting the same state twice (on the
        // resumed run continuing past it) starts fresh from `None`.
        mut resume_cursors: HashMap<String, Vec<u8>>,
        // States visited *before* this call entered the FSM. Empty on a fresh
        // `orchestrate`; on `resume_from` it carries the pre-crash path (every
        // `RowKind::StateEntry` recorded for this run, ascending by sequence) so
        // a failure after the resume point surfaces the full transition history
        // — including states the original run visited before the crash.
        prior_transitions: Vec<TState>,
        // Wall-clock budget for the entire FSM call: `(start_instant, limit)`.
        // When `Some`, each per-iteration step future is wrapped in
        // `tokio::time::timeout_at(start + limit, ...)` and a trip surfaces as
        // `CanoError::WorkflowTimeout`, drained against a bounded compensation
        // deadline. `None` is the zero-cost path — dispatch is awaited directly
        // with no `timeout_at` wrapper.
        total_budget: Option<(std::time::Instant, std::time::Duration)>,
        // Cooperative-cancellation signal. The internal "never" token (used by
        // `orchestrate`/`resume_from`) reports `can_cancel() == false`, so the
        // dispatch path skips the cancellation `select!` entirely — the existing
        // zero-cost behaviour is preserved bit-for-bit.
        token: CancellationToken,
    ) -> Result<TState, CanoError> {
        let mut current_state = initial_state;
        let mut sequence = start_sequence;
        // Path of states entered so far (one push per loop iteration, including
        // router states and the terminal exit state). Pre-populated with the
        // resumed run's pre-crash path so the wrapped error's `path` field is
        // truthful across crashes. Stored as `TState` to avoid per-iteration
        // `format!` allocations on the success path; formatted to `Vec<String>`
        // only when folded into a `WithStateContext` on failure.
        let mut transitions_so_far: Vec<TState> = prior_transitions;

        #[cfg(feature = "tracing")]
        info!(initial_state = ?current_state, "Starting workflow execution");

        // Optional absolute deadline shared by every in-flight task, bundled with
        // the `(start, limit)` context needed to report a timeout. Loop-invariant
        // (it derives only from `total_budget`), so it's resolved once here rather
        // than rebuilt every iteration. When `total_budget` is `None` (the common
        // path) every branch awaits its dispatch directly — same scheduling
        // behavior as before. Carrying all three values together makes the
        // "deadline implies budget" invariant structural so the branches below
        // don't need an `.expect` to recover it.
        //
        // `start.checked_add(limit)` guards against `std::time::Instant + Duration`
        // panicking on overflow (e.g. `with_total_timeout(Duration::MAX)`); on
        // overflow we synthesize a far-future deadline so the `timeout_at` simply
        // never fires, rather than aborting the orchestrate task with a panic.
        // `u32::MAX` seconds (~136 years) is comfortably below the representable
        // range of `tokio::time::Instant` on every supported platform.
        let step_budget: Option<(
            std::time::Instant,
            std::time::Duration,
            tokio::time::Instant,
        )> = total_budget.map(|(start, limit)| {
            let deadline = start
                .checked_add(limit)
                .map(tokio::time::Instant::from_std)
                .unwrap_or_else(|| {
                    tokio::time::Instant::now() + std::time::Duration::from_secs(u32::MAX as u64)
                });
            (start, limit, deadline)
        });

        loop {
            // Cooperative cancellation observed at a state boundary: stop before
            // entering this state, fire `on_cancelled` once, and drain whatever
            // compensatable work has completed so far. `is_cancelled()` is a
            // non-blocking poll and is `false` for the "never" token, so this is
            // free on the no-token path. `current_state` is not yet pushed onto
            // `transitions_so_far`, so the reported path stops *before* the state
            // we declined to run.
            if token.is_cancelled() {
                let label = format!("{current_state:?}");
                notify_observers(&self.observers, |o| o.on_cancelled(&label));
                return self
                    .wrap_and_drain(
                        workflow_id.as_deref(),
                        compensation_stack,
                        &current_state,
                        &transitions_so_far,
                        CanoError::cancelled(),
                        total_budget,
                    )
                    .await;
            }

            // The `Debug` label of the state being entered. Needed for observer
            // `on_state_enter`, checkpoint rows, and the `metrics` feature's
            // `state` label; skipped (no allocation) when none are in play — the
            // common, zero-overhead case. Computed once per iteration and
            // reused on every hot path below.
            let need_state_label = !self.observers.is_empty()
                || self.checkpoint_store.is_some()
                || cfg!(feature = "metrics");
            let state_label: Option<String> = if need_state_label {
                Some(format!("{current_state:?}"))
            } else {
                None
            };

            // Record the state being entered for state-context error wrapping.
            // Unconditional — every state entered (including Router and exit
            // states) appears in `transitions_so_far`.
            transitions_so_far.push(current_state.clone());

            // Notify observers of every state entry, including the initial state
            // and any terminal exit state.
            if let Some(ref label) = state_label {
                notify_observers(&self.observers, |o| o.on_state_enter(label));
            }

            // Router states are pure routing: no side effects, nothing to
            // recover. Skip the checkpoint write and do not consume a sequence
            // number so that sequence numbers remain dense for `resume_from`.
            // `on_state_enter` was already fired above — the FSM did enter the
            // state; only the recovery footprint is suppressed.
            //
            // All other variants (Single, Split, CompensatableSingle, Stepped) are
            // checkpointed.
            let is_router = matches!(
                self.states.get(&current_state).map(|e| e.as_ref()),
                Some(StateEntry::Router { .. }),
            );

            // Checkpoint this state entry before touching its task. One row per
            // state — a split is one state, hence one row regardless of fan-out.
            // Skipped for Router states (see above).
            if !is_router && let Some(ref store) = self.checkpoint_store {
                let wf_id = match workflow_id.as_deref() {
                    Some(id) => id,
                    None => {
                        let err = CanoError::checkpoint_store(
                            "checkpointing requires a workflow id (call with_workflow_id)",
                        );
                        return self
                            .wrap_and_drain(
                                workflow_id.as_deref(),
                                compensation_stack,
                                &current_state,
                                &transitions_so_far,
                                err,
                                total_budget,
                            )
                            .await;
                    }
                };
                // `state_label` is always `Some` here (checkpoint_store ⇒ label computed).
                let label = state_label.as_deref().unwrap_or_default();
                let task_id = match self.states.get(&current_state).map(|e| e.as_ref()) {
                    Some(StateEntry::Single { task, .. }) => task.name().into_owned(),
                    Some(StateEntry::CompensatableSingle { task, .. }) => task.name().into_owned(),
                    Some(StateEntry::Stepped { task, .. }) => task.name().into_owned(),
                    // Router is unreachable here (is_router guard above), Split has no single task_id.
                    _ => String::new(),
                };
                let append_result = store
                    .append(
                        wf_id,
                        CheckpointRow::new(sequence, label, task_id)
                            .with_workflow_version(self.workflow_version),
                    )
                    .await;
                #[cfg(feature = "metrics")]
                crate::metrics::checkpoint_append(append_result.is_ok());
                if let Err(e) = append_result {
                    let err = CanoError::checkpoint_store(format!("append checkpoint: {e}"));
                    return self
                        .wrap_and_drain(
                            workflow_id.as_deref(),
                            compensation_stack,
                            &current_state,
                            &transitions_so_far,
                            err,
                            total_budget,
                        )
                        .await;
                }
                notify_observers(&self.observers, |o| o.on_checkpoint(wf_id, sequence));
                sequence += 1;
            }

            // Reached an exit state — the run succeeded. Clear the log (best-effort) and return.
            if self.exit_states.contains(&current_state) {
                #[cfg(feature = "tracing")]
                info!(final_state = ?current_state, "Workflow completed successfully");
                self.clear_checkpoint_log(workflow_id.as_deref()).await;
                return Ok(current_state);
            }

            // Get the state entry — borrow from the map, clone only Arc handles below.
            let state_entry = match self.states.get(&current_state) {
                Some(e) => e,
                None => {
                    let err = CanoError::workflow(format!(
                        "No task registered for state: {:?}",
                        current_state
                    ));
                    return self
                        .wrap_and_drain(
                            workflow_id.as_deref(),
                            compensation_stack,
                            &current_state,
                            &transitions_so_far,
                            err,
                            total_budget,
                        )
                        .await;
                }
            };

            #[cfg(feature = "tracing")]
            debug!(current_state = ?current_state, "Executing state");

            // Reuse the already-computed label (see `need_state_label` above —
            // `metrics` feature is in that condition so this is always `Some`).
            #[cfg(feature = "metrics")]
            let _state_label = state_label.as_deref().unwrap_or_default();
            #[cfg(feature = "metrics")]
            let _dispatch_started = std::time::Instant::now();

            // Dispatch by entry type. Any `Err(err)` triggers a compensation
            // drain after the error is wrapped with state context. The
            // `step_budget` timeout wrapper and the paired
            // `on_task_failure` / `on_workflow_timeout` observer fan-out
            // live in `Self::dispatch_with_budget`; each branch supplies
            // only the failure-name fan-out specific to its shape.
            let step: Result<TState, CanoError> = match state_entry.as_ref() {
                StateEntry::Single { task, config } => {
                    let task_name = task.name();
                    let fut = self.execute_single_task(task.clone(), Arc::clone(config));
                    Self::dispatch_with_budget(
                        step_budget,
                        &token,
                        state_label.as_deref(),
                        &self.observers,
                        fut,
                        |o, err| {
                            // `execute_single_task` fired `on_task_start` inside the
                            // dropped future; pair it with `on_task_failure` so observer
                            // gauges (`active_tasks` etc.) remain balanced.
                            o.on_task_failure(task_name.as_ref(), err);
                        },
                    )
                    .await
                }
                StateEntry::Router { task, config } => {
                    // Dispatched like Single; checkpoint write is skipped in the
                    // block above (is_router guard).
                    let task_name = task.name();
                    let fut = self.execute_single_task(task.clone(), Arc::clone(config));
                    Self::dispatch_with_budget(
                        step_budget,
                        &token,
                        state_label.as_deref(),
                        &self.observers,
                        fut,
                        |o, err| {
                            o.on_task_failure(task_name.as_ref(), err);
                        },
                    )
                    .await
                }
                StateEntry::Split {
                    tasks,
                    configs,
                    join_config,
                } => {
                    let fut = self.execute_split_join(
                        tasks.clone(),
                        Arc::clone(configs),
                        join_config.clone(),
                    );
                    // Pre-format branch ids once (instead of per observer) — the
                    // helper invokes `task_failure_fan_out` once per observer, so
                    // formatting inside the closure would re-allocate every time.
                    // `execute_split_join` fires `on_task_start` per branch; on
                    // outer cancellation OR a total-timeout trip those branches are
                    // dropped, so we fire a synthetic per-branch `on_task_failure`
                    // to keep observer gauges balanced — needed whenever the
                    // dispatch can be aborted (a budget deadline or a live token).
                    let branch_ids: Vec<String> = if step_budget.is_some() || token.can_cancel() {
                        tasks
                            .iter()
                            .enumerate()
                            .map(|(idx, t)| format!("{}[{}]", t.name(), idx))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Self::dispatch_with_budget(
                        step_budget,
                        &token,
                        state_label.as_deref(),
                        &self.observers,
                        fut,
                        |o, err| {
                            for id in &branch_ids {
                                o.on_task_failure(id, err);
                            }
                        },
                    )
                    .await
                }
                StateEntry::CompensatableSingle { task, config } => {
                    // Saga safety: do NOT wrap `execute_compensatable_task` in `timeout_at`.
                    // The compensation entry is pushed onto the stack *after* the task body
                    // returns its serialized output (see the `Ok((next_state, output_blob))`
                    // branch below). A mid-await cancel would drop the future *after* an
                    // in-task side effect committed, leaving the engine with no entry to
                    // roll back. Bounding via the task's own `attempt_timeout` is the right
                    // level — there the task author controls the commit-vs-yield ordering.
                    // The cost is at most one task duration of total-budget drift; the
                    // alternative — losing track of a committed side effect — is worse.
                    let fut = self.execute_compensatable_task(task.clone(), Arc::clone(config));
                    let comp_result: Result<(TState, Vec<u8>), CanoError> = fut.await;
                    #[cfg(feature = "metrics")]
                    crate::metrics::task_dispatch_duration(
                        _state_label,
                        "compensatable",
                        _dispatch_started.elapsed(),
                    );
                    match comp_result {
                        Ok((next_state, output_blob)) => {
                            let task_name: Arc<str> = Arc::from(task.name());
                            // Persist a completion row carrying the output so a resumed
                            // run rehydrates this compensation entry.
                            if let (Some(store), Some(wf_id)) =
                                (&self.checkpoint_store, workflow_id.as_deref())
                            {
                                let label = state_label.as_deref().unwrap_or_default();
                                let row = CheckpointRow::new(sequence, label, &*task_name)
                                    .with_output(output_blob.clone())
                                    .with_workflow_version(self.workflow_version);
                                let comp_append_result = store.append(wf_id, row).await;
                                #[cfg(feature = "metrics")]
                                crate::metrics::checkpoint_append(comp_append_result.is_ok());
                                if let Err(e) = comp_append_result {
                                    let err = CanoError::checkpoint_store(format!(
                                        "append compensation checkpoint: {e}"
                                    ));
                                    compensation_stack.push(CompensationEntry {
                                        task_id: task_name,
                                        output_blob,
                                    });
                                    return self
                                        .wrap_and_drain(
                                            workflow_id.as_deref(),
                                            compensation_stack,
                                            &current_state,
                                            &transitions_so_far,
                                            err,
                                            total_budget,
                                        )
                                        .await;
                                }
                                notify_observers(&self.observers, |o| {
                                    o.on_checkpoint(wf_id, sequence)
                                });
                                sequence += 1;
                            }
                            compensation_stack.push(CompensationEntry {
                                task_id: task_name,
                                output_blob,
                            });
                            Ok(next_state)
                        }
                        Err(e) => Err(e),
                    }
                }
                StateEntry::Stepped { task, config } => {
                    // Pop the resume cursor for this state (if any). Using `remove` instead
                    // of `get` so that if the FSM visits the same state again later (e.g. a
                    // loop), that second visit starts fresh from `None`.
                    let resume_cursor = state_label
                        .as_deref()
                        .and_then(|label| resume_cursors.remove(label));
                    let task_name = task.name();
                    let fut = self.execute_stepped_task(
                        task.clone(),
                        Arc::clone(config),
                        &workflow_id,
                        state_label.as_deref().unwrap_or_default(),
                        &mut sequence,
                        resume_cursor,
                    );
                    Self::dispatch_with_budget(
                        step_budget,
                        &token,
                        state_label.as_deref(),
                        &self.observers,
                        fut,
                        |o, err| {
                            o.on_task_failure(task_name.as_ref(), err);
                        },
                    )
                    .await
                }
            };

            // CompensatableSingle records its own duration earlier (before the
            // completion-row append) so it is excluded here to avoid double-counting.
            #[cfg(feature = "metrics")]
            if let Some(kind) = match state_entry.as_ref() {
                StateEntry::Single { .. } => Some("single"),
                StateEntry::Router { .. } => Some("router"),
                StateEntry::Split { .. } => Some("split"),
                StateEntry::CompensatableSingle { .. } => None,
                StateEntry::Stepped { .. } => Some("stepped"),
            } {
                crate::metrics::task_dispatch_duration(
                    _state_label,
                    kind,
                    _dispatch_started.elapsed(),
                );
            }

            current_state = match step {
                Ok(s) => s,
                Err(e) => {
                    // `on_cancelled` for a mid-task cancel already fired inside
                    // `dispatch_with_budget` (the between-state case fires in the
                    // top-of-loop guard), so this arm stays generic.
                    // Route through `wrap_and_drain` so the wrap + bounded-vs-
                    // unbounded decision live in one place (it derives the
                    // attempt count from `e` itself). The bounded drain bounds
                    // rollback time for *any* failure, not just the engine-fired
                    // `WorkflowTimeout`: a hung task surfaced as
                    // `RetryExhausted { source: Timeout }`, a circuit-open burst,
                    // or a checkpoint-store error all benefit from the same
                    // wall-clock cap.
                    return self
                        .wrap_and_drain(
                            workflow_id.as_deref(),
                            compensation_stack,
                            &current_state,
                            &transitions_so_far,
                            e,
                            total_budget,
                        )
                        .await;
                }
            };
        }
    }

    /// Wrap a state-dispatch future in the per-iteration step-budget and race it
    /// against the cancellation `token`, or pass it through unchanged when neither
    /// is active.
    ///
    /// When the wrapped future trips the deadline, the engine synthesizes a
    /// `WorkflowTimeout` error, fires `on_workflow_timeout` once per
    /// registered observer, and lets the caller supply the
    /// `on_task_failure` fan-out via `task_failure_fan_out` — single-task
    /// arms fire one failure; the Split arm iterates the per-branch task
    /// names so observer `active_tasks` gauges stay balanced (every
    /// `on_task_start` already fired by the dropped inner future is paired
    /// with a matching `on_task_failure`).
    ///
    /// When the `token` fires first, the dispatch is dropped, the same
    /// `task_failure_fan_out` runs (gauge balance), `on_cancelled(state_label)`
    /// fires once, and `CanoError::Cancelled` is returned. This is the *only*
    /// place a token-driven mid-task cancel is recognized — the caller's error
    /// arm stays generic.
    ///
    /// `fut.await` is the zero-cost path when `step_budget` is `None` and the
    /// token can never fire; no observer plumbing runs in that case.
    async fn dispatch_with_budget<T, F>(
        step_budget: Option<(
            std::time::Instant,
            std::time::Duration,
            tokio::time::Instant,
        )>,
        token: &CancellationToken,
        state_label: Option<&str>,
        observers: &[Arc<dyn crate::observer::WorkflowObserver>],
        fut: F,
        task_failure_fan_out: impl Fn(&dyn crate::observer::WorkflowObserver, &CanoError),
    ) -> Result<T, CanoError>
    where
        F: std::future::Future<Output = Result<T, CanoError>>,
    {
        let fan = &task_failure_fan_out;
        // The existing budget logic, untouched: `timeout_at` when a budget is set,
        // otherwise a bare `fut.await`. Captured as a future so the cancellation
        // arm below can race it. `async move` takes ownership of `fut`; `observers`
        // and `fan` are `Copy` references, so they remain usable in the cancel arm.
        let budgeted = async move {
            let Some((start, limit, deadline)) = step_budget else {
                return fut.await;
            };
            match tokio::time::timeout_at(deadline, fut).await {
                Ok(inner) => inner,
                Err(_) => {
                    let elapsed = start.elapsed();
                    let err = CanoError::workflow_timeout(elapsed, limit);
                    notify_observers(observers, |o| {
                        fan(o, &err);
                        o.on_workflow_timeout(elapsed, limit);
                    });
                    Err(err)
                }
            }
        };

        // Zero-cost path: the "never" token can't fire, so skip the `select!`
        // entirely and run the budgeted future exactly as before.
        if !token.can_cancel() {
            return budgeted.await;
        }

        // Race cancellation against the budgeted dispatch. `biased` checks the
        // cancel arm first so cancellation deterministically wins a tie against
        // the per-state timeout. On cancel the inner `fut` is dropped (for splits
        // this drops the `JoinSet`, aborting its children); we fire the same
        // per-task fan-out the timeout path uses so observer gauges stay balanced.
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                let err = CanoError::cancelled();
                notify_observers(observers, |o| fan(o, &err));
                if let Some(label) = state_label {
                    notify_observers(observers, |o| o.on_cancelled(label));
                }
                Err(err)
            }
            res = budgeted => res,
        }
    }

    /// Wrap `err` with FSM state context and drain compensations under the
    /// bounded vs unbounded variant chosen by `total_budget`. Every error
    /// site in `execute_workflow_from` routes through this helper so two
    /// invariants hold uniformly across pre-dispatch arms (missing workflow
    /// id, checkpoint-append failure, missing handler, compensation-
    /// completion append failure) and the post-dispatch arm:
    ///
    /// - **F1**: when `with_total_timeout` is set, the drain runs under the
    ///   user-configured `with_compensation_timeout` (resolved from the
    ///   total budget). Without a single entry point, pre-dispatch arms
    ///   used to silently call the unbounded drain.
    /// - **F6**: the original failure is wrapped in `WithStateContext`
    ///   before reaching the drain, so any later `CompensationFailed`
    ///   envelope's `errors[0]` carries the FSM state/path/attempt
    ///   annotation. The wrap is idempotent for already-wrapped errors
    ///   and recurses into `CompensationFailed.errors[0]` (handled by the
    ///   `with_state_context` constructor).
    #[allow(clippy::too_many_arguments)]
    async fn wrap_and_drain(
        &self,
        workflow_id: Option<&str>,
        compensation_stack: Vec<CompensationEntry>,
        current_state: &TState,
        transitions_so_far: &[TState],
        err: CanoError,
        total_budget: Option<(std::time::Instant, std::time::Duration)>,
    ) -> Result<TState, CanoError> {
        // Format the transition path into `Vec<String>` here so the FSM loop
        // can keep its tracker as `Vec<TState>` and skip per-iteration String
        // allocations on the success path. `CanoError::with_state_context`
        // short-circuits when `err` is already a `WithStateContext` (preserving
        // nested-workflow context) or a `CompensationFailed` (in which case
        // it recurses into `errors[0]` so the F6 invariant still holds at
        // every drain entry point).
        let path: Vec<String> = transitions_so_far
            .iter()
            .map(|s| format!("{s:?}"))
            .collect();
        // Derive the attempt count from the error itself. Pre-dispatch failures
        // (missing id/handler, checkpoint-append) are first-attempt by
        // construction (`attempts_from_error` returns 1 for them); the
        // post-dispatch arm's `err` carries the structural `RetryExhausted`
        // count or the `CircuitOpen`/`WorkflowTimeout` short-circuit sentinel.
        // `with_state_context` ignores this for already-wrapped or
        // `CompensationFailed` errors, so it only takes effect on a fresh wrap.
        let attempt = Self::attempts_from_error(&err);
        let wrapped =
            CanoError::with_state_context(format!("{current_state:?}"), attempt, path, err);
        match total_budget {
            Some((_, limit)) => {
                let comp_budget = resolve_compensation_deadline(limit, self.compensation_timeout);
                let comp_deadline = tokio::time::Instant::now() + comp_budget;
                self.run_compensations_bounded(
                    workflow_id,
                    compensation_stack,
                    wrapped,
                    Some(comp_deadline),
                )
                .await
            }
            None => {
                self.run_compensations(workflow_id, compensation_stack, wrapped)
                    .await
            }
        }
    }

    /// Compute the attempt number that produced `err`.
    ///
    /// `RetryExhausted` carries the structural count. `CircuitOpen` and
    /// `WorkflowTimeout` return 0 because they short-circuit (or cancel) the
    /// in-flight future *before* an attempt body runs to completion — surfacing
    /// `attempt = 1` would be indistinguishable from "completed one full
    /// attempt then failed", which misleads alerting that buckets on the
    /// attempt count. `WithStateContext` is unwrapped so a nested cause still
    /// surfaces the truthful count rather than always 1. Everything else means
    /// we stopped on the first attempt.
    pub(super) fn attempts_from_error(err: &CanoError) -> u32 {
        match err {
            CanoError::RetryExhausted { attempts, .. } => *attempts,
            CanoError::CircuitOpen(_)
            | CanoError::WorkflowTimeout { .. }
            | CanoError::Cancelled => 0,
            CanoError::WithStateContext { source, .. } => Self::attempts_from_error(source),
            _ => 1,
        }
    }

    async fn execute_single_task(
        &self,
        task: Arc<dyn Task<TState, TResourceKey> + Send + Sync>,
        config: Arc<crate::task::TaskConfig>,
    ) -> Result<TState, CanoError> {
        let observers = self.observer_slice();
        let task_name = task.name();
        if let Some(ref slice) = observers {
            notify_observers(slice, |o| o.on_task_start(task_name.as_ref()));
        }
        // Stamp observers + task name onto a per-dispatch config so
        // `run_with_retries` can emit `on_retry` / `on_circuit_open`. Zero-cost
        // (returns the same Arc) when no observers are attached.
        let config = Self::config_with_observers(&config, &observers, &task_name);

        #[cfg(feature = "tracing")]
        let task_span = if tracing::enabled!(tracing::Level::INFO) {
            info_span!("single_task_execution")
        } else {
            tracing::Span::none()
        };

        // Wrap the retry-driving future in `catch_unwind` (via
        // `catch_panic_to_error`) so a panic inside the task body becomes a
        // `CanoError::TaskExecution` instead of unwinding through the
        // workflow loop and aborting the runtime worker. Split tasks apply
        // the same catch-unwind pattern inside each spawned task so their
        // task index and panic payload are preserved.
        let run_future = async {
            run_with_retries(&config, || {
                let task_clone = task.clone();
                let resources_clone = Arc::clone(&self.resources);
                async move { task_clone.run(&*resources_clone).await }
            })
            .await
        };

        #[cfg(feature = "tracing")]
        let result = {
            let _enter = task_span.enter();
            super::catch_panic_to_error(run_future, "Single task").await
        };
        #[cfg(not(feature = "tracing"))]
        let result = super::catch_panic_to_error(run_future, "Single task").await;

        let outcome: Result<TState, CanoError> = match result {
            Ok(TaskResult::Single(next_state)) => {
                #[cfg(feature = "tracing")]
                debug!(next_state = ?next_state, "Single task completed");
                Ok(next_state)
            }
            Ok(TaskResult::Split(_)) => Err(CanoError::workflow(
                "Single task returned split result - use register_split() for split tasks",
            )),
            Err(e) => Err(e),
        };

        if let Some(ref slice) = observers {
            match &outcome {
                Ok(_) => notify_observers(slice, |o| o.on_task_success(task_name.as_ref())),
                Err(e) => notify_observers(slice, |o| o.on_task_failure(task_name.as_ref(), e)),
            }
        }
        outcome
    }

    /// Drive the step loop for a `Stepped` state, persisting each cursor as a
    /// [`RowKind::StepCursor`] row after every `StepOutcome::More`.
    ///
    /// - `resume_cursor`: `Some(bytes)` when resuming from a prior cursor row; `None`
    ///   on a fresh run or when no cursor was persisted for this state.
    /// - `sequence`: mutated in-place — each persisted cursor row consumes one number.
    ///   The caller already consumed one for the state-entry row before calling here.
    ///
    /// Returns the next `TState` on `StepOutcome::Done(TaskResult::Single)`. Returns an error
    /// if `StepOutcome::Done(TaskResult::Split)` is returned (split is unsupported here), or on
    /// any task / checkpoint failure.
    #[allow(clippy::too_many_arguments)]
    async fn execute_stepped_task(
        &self,
        task: Arc<dyn ErasedSteppedTask<TState, TResourceKey>>,
        config: Arc<crate::task::TaskConfig>,
        workflow_id: &Option<Arc<str>>,
        state_label: &str,
        sequence: &mut u64,
        resume_cursor: Option<Vec<u8>>,
    ) -> Result<TState, CanoError> {
        let observers = self.observer_slice();
        let task_name = task.name();
        if let Some(ref slice) = observers {
            notify_observers(slice, |o| o.on_task_start(task_name.as_ref()));
        }
        let config = Self::config_with_observers(&config, &observers, &task_name);

        // Current cursor: starts from the resume cursor (if any), advances each iteration.
        let mut current_cursor: Option<Vec<u8>> = resume_cursor;

        let outcome: Result<TState, CanoError> = loop {
            // Capture the cursor for this attempt so that the retry closure can clone it.
            let attempt_cursor = current_cursor.clone();
            // Wrap the retry-driving future in `catch_unwind` so a panic
            // inside the `step` body becomes a `CanoError::TaskExecution`
            // instead of unwinding through the step loop and the FSM,
            // bypassing the compensation drain and resource teardown.
            // Matches the pattern in `execute_single_task`,
            // `execute_split_join`, and `execute_compensatable_task`.
            let run_future = async {
                run_with_retries(&config, || {
                    let c = attempt_cursor.clone();
                    let t = Arc::clone(&task);
                    let r = Arc::clone(&self.resources);
                    async move { t.step(&*r, c).await }
                })
                .await
            };
            let step_result = super::catch_panic_to_error(run_future, "Stepped task").await;

            match step_result {
                Err(e) => break Err(e),
                Ok(ErasedStep::Done(TaskResult::Single(next_state))) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(next_state = ?next_state, "Stepped task completed");
                    #[cfg(feature = "metrics")]
                    crate::metrics::step_iteration(true);
                    break Ok(next_state);
                }
                Ok(ErasedStep::Done(TaskResult::Split(_))) => {
                    #[cfg(feature = "metrics")]
                    crate::metrics::step_iteration(true);
                    break Err(CanoError::workflow(
                        "Stepped task returned split result — split is not supported for Stepped states",
                    ));
                }
                Ok(ErasedStep::More(new_cursor_bytes)) => {
                    #[cfg(feature = "metrics")]
                    crate::metrics::step_iteration(false);
                    // Persist the cursor before advancing so a crash between steps
                    // can resume from this exact position.
                    if let (Some(store), Some(wf_id)) =
                        (&self.checkpoint_store, workflow_id.as_deref())
                    {
                        let row = CheckpointRow::new(*sequence, state_label, task_name.as_ref())
                            .with_cursor(new_cursor_bytes.clone())
                            .with_workflow_version(self.workflow_version);
                        let cursor_append_result = store.append(wf_id, row).await;
                        #[cfg(feature = "metrics")]
                        crate::metrics::checkpoint_append(cursor_append_result.is_ok());
                        if let Err(e) = cursor_append_result {
                            break Err(CanoError::checkpoint_store(format!(
                                "append step cursor checkpoint: {e}"
                            )));
                        }
                        notify_observers(&self.observers, |o| o.on_checkpoint(wf_id, *sequence));
                        *sequence += 1;
                    }
                    current_cursor = Some(new_cursor_bytes);
                }
            }
        };

        if let Some(ref slice) = observers {
            match &outcome {
                Ok(_) => notify_observers(slice, |o| o.on_task_success(task_name.as_ref())),
                Err(e) => notify_observers(slice, |o| o.on_task_failure(task_name.as_ref(), e)),
            }
        }
        outcome
    }

    async fn execute_split_join(
        &self,
        tasks: Vec<Arc<dyn Task<TState, TResourceKey> + Send + Sync>>,
        configs: Arc<Vec<Arc<crate::task::TaskConfig>>>,
        join_config: Arc<JoinConfig<TState>>,
    ) -> Result<TState, CanoError> {
        let resources = Arc::clone(&self.resources);
        let total_tasks = tasks.len();

        #[cfg(feature = "tracing")]
        info!(
            total_tasks = total_tasks,
            strategy = ?join_config.strategy,
            "Starting split execution"
        );

        // Validate strategy configuration before spawning tasks. `validate()`
        // catches these at build/start time; keep the execution-time check as
        // defense-in-depth for internal callers that bypass public orchestration.
        Self::validate_join_config(join_config.as_ref(), total_tasks)?;

        // Optional bulkhead: gate task bodies on a shared semaphore so at
        // most `n` split tasks execute concurrently.
        let bulkhead = join_config
            .bulkhead
            .map(|n| Arc::new(tokio::sync::Semaphore::new(n)));

        let mut join_set: tokio::task::JoinSet<(usize, Result<TaskResult<TState>, CanoError>)> =
            tokio::task::JoinSet::new();

        // Observer wiring. `task_ids[idx]` holds each split task's reported id
        // (`"{name}[{idx}]"`); kept on the parent so `on_task_success` /
        // `on_task_failure` can be fired from here once results are collected.
        // Both stay empty / `None` and cost nothing when no observers are attached.
        let observers = self.observer_slice();
        let mut task_ids: Vec<Cow<'static, str>> = if observers.is_some() {
            Vec::with_capacity(total_tasks)
        } else {
            Vec::new()
        };

        #[cfg_attr(not(feature = "tracing"), allow(unused_variables))]
        for (idx, task) in tasks.into_iter().enumerate() {
            let task_id: Cow<'static, str> = if observers.is_some() {
                Cow::Owned(format!("{}[{}]", task.name(), idx))
            } else {
                Cow::Borrowed("<split-task>")
            };
            if let Some(ref slice) = observers {
                notify_observers(slice, |o| o.on_task_start(&task_id));
            }
            // Use cached config from registration time, with observers + task id
            // stamped on when present (a no-op clone-free path otherwise).
            let config = Self::config_with_observers(&configs[idx], &observers, &task_id);
            if observers.is_some() {
                task_ids.push(task_id);
            }
            let resources_clone = Arc::clone(&resources);
            let bulkhead_clone = bulkhead.clone();

            #[cfg(feature = "tracing")]
            let task_span = if tracing::enabled!(tracing::Level::INFO) {
                info_span!("split_task", task_id = idx)
            } else {
                tracing::Span::none()
            };

            join_set.spawn(async move {
                let run_future = async {
                    #[cfg(feature = "tracing")]
                    let _enter = task_span.enter();

                    #[cfg(feature = "tracing")]
                    debug!(task_id = idx, "Executing split task");

                    // Hold the permit (if any) until this future returns. The
                    // semaphore is never closed here, so `acquire_owned` only
                    // fails if it has been closed elsewhere — treat that as a
                    // task-execution error rather than panicking.
                    let _permit = match bulkhead_clone {
                        Some(sem) => match sem.acquire_owned().await {
                            Ok(p) => Some(p),
                            Err(e) => {
                                return (
                                    idx,
                                    Err(CanoError::task_execution(format!(
                                        "bulkhead semaphore closed: {e}"
                                    ))),
                                );
                            }
                        },
                        None => None,
                    };

                    let result = run_with_retries(&config, || {
                        let t = task.clone();
                        let r = Arc::clone(&resources_clone);
                        async move { t.run(&*r).await }
                    })
                    .await;

                    #[cfg(feature = "tracing")]
                    match &result {
                        Ok(_) => debug!(task_id = idx, "Split task completed successfully"),
                        Err(e) => warn!(task_id = idx, error = %e, "Split task failed"),
                    }

                    (idx, result)
                };

                match AssertUnwindSafe(run_future).catch_unwind().await {
                    Ok(outcome) => outcome,
                    Err(payload) => {
                        let payload_str = panic_payload_message(&*payload);
                        #[cfg(feature = "tracing")]
                        tracing::error!(task_id = idx, panic = %payload_str, "Split task panicked");
                        (
                            idx,
                            Err(CanoError::task_execution(format!(
                                "panic in split task {idx}: {payload_str}"
                            ))),
                        )
                    }
                }
            });
        }

        // Collect results using the unified strategy handler
        let split_result = self
            .collect_results(join_set, &join_config, total_tasks)
            .await?;

        #[cfg(feature = "metrics")]
        crate::metrics::split_branch_results(
            split_result.successes.len(),
            split_result.errors.len(),
            split_result.cancelled.len(),
        );

        // Fire per-task observer events for everything that ran to completion.
        // Cancelled tasks are intentionally silent (no dedicated hook), and an
        // aggregated error entry carries `task_index == usize::MAX`, which
        // `task_ids.get` filters out.
        if let Some(ref slice) = observers {
            for success in &split_result.successes {
                if let Some(id) = task_ids.get(success.task_index) {
                    notify_observers(slice, |o| o.on_task_success(id));
                }
            }
            for failure in &split_result.errors {
                if let (Some(id), Err(err)) = (task_ids.get(failure.task_index), &failure.result) {
                    notify_observers(slice, |o| o.on_task_failure(id, err));
                }
            }
        }

        let successful = split_result.successes.len();
        let _failed = split_result.errors.len();
        let _cancelled = split_result.cancelled.len();

        #[cfg(feature = "tracing")]
        info!(
            successful = successful,
            failed = _failed,
            cancelled = _cancelled,
            total = total_tasks,
            "Split execution completed"
        );

        // Check if join condition is met
        match &join_config.strategy {
            JoinStrategy::PartialResults(_) => {
                // For PartialResults, we always continue if minimum tasks completed successfully
                if join_config.strategy.is_satisfied(successful, total_tasks) {
                    Ok(join_config.join_state.clone())
                } else {
                    let mut message = format!(
                        "Partial results condition not met: {} completed successfully, {} required",
                        successful,
                        match &join_config.strategy {
                            JoinStrategy::PartialResults(min) => *min,
                            _ => 0,
                        }
                    );
                    if !split_result.errors.is_empty() {
                        message.push_str("; errors: ");
                        message.push_str(&split_error_summary(&split_result.errors));
                    }
                    Err(CanoError::workflow(message))
                }
            }
            JoinStrategy::PartialTimeout => {
                // For PartialTimeout, proceed with whatever completed before timeout
                if split_result.completed_count() >= 1 {
                    Ok(join_config.join_state.clone())
                } else {
                    Err(CanoError::workflow(
                        "PartialTimeout: No tasks completed before timeout",
                    ))
                }
            }
            _ => {
                // For other strategies, check successful tasks only
                if join_config.strategy.is_satisfied(successful, total_tasks) {
                    Ok(join_config.join_state.clone())
                } else {
                    let mut message = format!(
                        "Join condition not met: {} of {} tasks completed successfully, strategy: {:?}",
                        successful, total_tasks, join_config.strategy
                    );
                    if !split_result.errors.is_empty() {
                        message.push_str("; errors: ");
                        message.push_str(&split_error_summary(&split_result.errors));
                    }
                    Err(CanoError::workflow(message))
                }
            }
        }
    }

    async fn collect_results(
        &self,
        mut join_set: tokio::task::JoinSet<(usize, Result<TaskResult<TState>, CanoError>)>,
        join_config: &JoinConfig<TState>,
        total_tasks: usize,
    ) -> Result<SplitResult<TState>, CanoError> {
        let mut split_result = SplitResult::with_capacity(total_tasks);
        // Bitset over task indices; cheaper than HashSet<usize> on cache and alloc.
        let mut completed_indices: Vec<bool> = vec![false; total_tasks];

        // Determine deadline if timeout is configured
        let deadline = join_config.timeout.map(|d| tokio::time::Instant::now() + d);

        loop {
            // Wait for next completion or timeout
            let next_result = if let Some(d) = deadline {
                match tokio::time::timeout_at(d, join_set.join_next()).await {
                    Ok(res) => res,
                    Err(_) => {
                        // Timeout reached
                        if matches!(join_config.strategy, JoinStrategy::PartialTimeout) {
                            // For PartialTimeout, abort remaining and return what we have
                            join_set.abort_all();
                            break;
                        } else {
                            // For other strategies, timeout is an error
                            join_set.abort_all();
                            return Err(CanoError::workflow("Split task timeout exceeded"));
                        }
                    }
                }
            } else {
                join_set.join_next().await
            };

            match next_result {
                Some(Ok((index, Ok(task_result)))) => {
                    completed_indices[index] = true;
                    split_result.successes.push(SplitTaskResult {
                        task_index: index,
                        result: Ok(task_result),
                    });
                }
                Some(Ok((index, Err(e)))) => {
                    completed_indices[index] = true;
                    split_result.errors.push(SplitTaskResult {
                        task_index: index,
                        result: Err(e),
                    });
                }
                Some(Err(join_err)) => {
                    // Task panicked or was aborted; index correlation is lost.
                    // Record as anonymous error so caller still sees the failure count.
                    split_result.errors.push(SplitTaskResult {
                        task_index: usize::MAX,
                        result: Err(CanoError::workflow(format!("Task panic: {:?}", join_err))),
                    });
                }
                None => break, // JoinSet drained
            }

            // Check if we can return early based on strategy
            match &join_config.strategy {
                JoinStrategy::Any if !split_result.successes.is_empty() => {
                    join_set.abort_all();
                    break;
                }
                JoinStrategy::PartialResults(min) if split_result.successes.len() >= *min => {
                    join_set.abort_all();
                    break;
                }
                _ => {} // Continue for other strategies
            }
        }

        // Identify cancelled tasks (those that didn't complete).
        // JoinSet::abort_all() and drop handle cleanup automatically.
        for (idx, completed) in completed_indices.iter().enumerate() {
            if !completed {
                split_result.cancelled.push(idx);
            }
        }

        Ok(split_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resources;
    use crate::task as task_mod;
    use crate::workflow::test_support::*;
    use cano_macros::task;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[tokio::test]
    async fn test_split_all_strategy() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_any_strategy() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Any, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_quorum_strategy() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::Quorum(3), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_percentage_strategy() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];

        // 75% of 4 tasks = 3 tasks
        let join_config = JoinConfig::new(JoinStrategy::Percentage(0.75), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_with_failures_all_strategy() {
        let tasks = vec![
            FailTask::new(false),
            FailTask::new(true), // This will fail
            FailTask::new(false),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_split_with_failures_quorum_strategy() {
        let tasks = vec![
            FailTask::new(false),
            FailTask::new(false),
            FailTask::new(true), // This will fail
        ];

        // Quorum of 2, so should succeed despite one failure
        let join_config = JoinConfig::new(JoinStrategy::Quorum(2), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_with_timeout() {
        // Task that sleeps longer than timeout
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SlowTask, SlowTask];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete)
            .with_timeout(Duration::from_millis(50));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_split_with_data_sharing() {
        let store = crate::store::MemoryStore::new();
        let resources = Resources::new().insert("store", store.clone());

        let tasks = vec![
            DataTask::new("task1", "value1", TestState::Join),
            DataTask::new("task2", "value2", TestState::Join),
            DataTask::new("task3", "value3", TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::new(resources)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify all tasks wrote their data
        let data1: String = store.get("task1").unwrap();
        let data2: String = store.get("task2").unwrap();
        let data3: String = store.get("task3").unwrap();

        assert_eq!(data1, "value1");
        assert_eq!(data2, "value2");
        assert_eq!(data3, "value3");
    }

    #[tokio::test]
    async fn test_complex_workflow_with_split_join() {
        let store = crate::store::MemoryStore::new();
        let resources = Resources::new().insert("store", store.clone());

        let split_tasks = vec![
            DataTask::new("parallel1", "data1", TestState::Join),
            DataTask::new("parallel2", "data2", TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Process);

        let workflow = Workflow::new(resources)
            .register(
                TestState::Start,
                DataTask::new("init", "initialized", TestState::Split),
            )
            .register_split(TestState::Split, split_tasks, join_config)
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify all data was written
        let init: String = store.get("init").unwrap();
        let parallel1: String = store.get("parallel1").unwrap();
        let parallel2: String = store.get("parallel2").unwrap();

        assert_eq!(init, "initialized");
        assert_eq!(parallel1, "data1");
        assert_eq!(parallel2, "data2");
    }

    #[tokio::test]
    async fn test_partial_results_strategy() {
        // Create tasks with varying delays - some will be cancelled
        #[derive(Clone)]
        struct DelayedTask {
            delay_ms: u64,
            #[allow(dead_code)]
            task_id: usize,
        }

        #[task]
        impl Task<TestState> for DelayedTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![
            DelayedTask {
                delay_ms: 50,
                task_id: 1,
            },
            DelayedTask {
                delay_ms: 100,
                task_id: 2,
            },
            DelayedTask {
                delay_ms: 500,
                task_id: 3,
            }, // This should be cancelled
            DelayedTask {
                delay_ms: 600,
                task_id: 4,
            }, // This should be cancelled
        ];

        // Wait for 2 tasks to complete, then cancel the rest
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_results_with_failures() {
        // Mix of fast success, fast failure, and slow tasks
        #[derive(Clone)]
        struct MixedTask {
            delay_ms: u64,
            should_fail: bool,
        }

        #[task]
        impl Task<TestState> for MixedTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                if self.should_fail {
                    Err(CanoError::task_execution("Task failed"))
                } else {
                    Ok(TaskResult::Single(TestState::Complete))
                }
            }
        }

        let tasks = vec![
            MixedTask {
                delay_ms: 50,
                should_fail: false,
            }, // Success
            MixedTask {
                delay_ms: 100,
                should_fail: true,
            }, // Failure
            MixedTask {
                delay_ms: 500,
                should_fail: false,
            }, // Should be cancelled
            MixedTask {
                delay_ms: 600,
                should_fail: false,
            }, // Should be cancelled
        ];

        // Wait for 2 tasks to complete (success or failure), then cancel rest
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_results_minimum_not_met() {
        // All tasks will timeout before minimum is reached
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SlowTask, SlowTask, SlowTask];

        // Require 3 tasks but timeout after 100ms
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(3), TestState::Complete)
            .with_timeout(Duration::from_millis(100));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_partial_timeout_strategy() {
        // Create tasks with varying delays
        #[derive(Clone)]
        struct DelayedTask {
            delay_ms: u64,
            #[allow(dead_code)]
            task_id: usize,
        }

        #[task]
        impl Task<TestState> for DelayedTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![
            DelayedTask {
                delay_ms: 50,
                task_id: 1,
            },
            DelayedTask {
                delay_ms: 100,
                task_id: 2,
            },
            DelayedTask {
                delay_ms: 500,
                task_id: 3,
            }, // Won't complete in time
            DelayedTask {
                delay_ms: 600,
                task_id: 4,
            }, // Won't complete in time
        ];

        // Timeout after 200ms - should get 2 completions
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete)
            .with_timeout(Duration::from_millis(200));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_timeout_with_failures() {
        // Mix of fast success, fast failure, and slow tasks
        #[derive(Clone)]
        struct MixedTask {
            delay_ms: u64,
            should_fail: bool,
        }

        #[task]
        impl Task<TestState> for MixedTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                if self.should_fail {
                    Err(CanoError::task_execution("Task failed"))
                } else {
                    Ok(TaskResult::Single(TestState::Complete))
                }
            }
        }

        let tasks = vec![
            MixedTask {
                delay_ms: 50,
                should_fail: false,
            }, // Success
            MixedTask {
                delay_ms: 100,
                should_fail: true,
            }, // Failure
            MixedTask {
                delay_ms: 150,
                should_fail: false,
            }, // Success
            MixedTask {
                delay_ms: 500,
                should_fail: false,
            }, // Won't complete
        ];

        // Timeout after 200ms
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete)
            .with_timeout(Duration::from_millis(200));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_timeout_all_complete() {
        // All tasks complete before timeout
        #[derive(Clone)]
        struct FastTask {
            delay_ms: u64,
        }

        #[task]
        impl Task<TestState> for FastTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![
            FastTask { delay_ms: 20 },
            FastTask { delay_ms: 30 },
            FastTask { delay_ms: 40 },
        ];

        // Generous timeout
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete)
            .with_timeout(Duration::from_millis(500));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_timeout_no_timeout_configured() {
        #[derive(Clone)]
        struct SimpleTaskLocal;

        #[task]
        impl Task<TestState> for SimpleTaskLocal {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let tasks = vec![SimpleTaskLocal, SimpleTaskLocal];

        // PartialTimeout without timeout should fail
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("requires a timeout")
        );
    }

    #[tokio::test]
    async fn test_empty_split_task_list() {
        let tasks: Vec<SimpleTask> = vec![];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_percentage_zero() {
        // Invalid percentage (0.0) should return a configuration error
        let tasks = vec![FailTask::new(true), FailTask::new(true)];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(0.0), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CanoError::Configuration(_)),
            "expected Configuration error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_percentage_one() {
        let tasks = vec![
            SimpleTask::new(TestState::Join),
            SimpleTask::new(TestState::Join),
        ];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(1.0), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        assert_eq!(
            workflow
                .orchestrate(TestState::Start, CancellationToken::disabled())
                .await
                .unwrap(),
            TestState::Complete
        );

        let tasks_fail = vec![FailTask::new(false), FailTask::new(true)];
        let join_config2 = JoinConfig::new(JoinStrategy::Percentage(1.0), TestState::Complete);
        let workflow2 = Workflow::bare()
            .register_split(TestState::Start, tasks_fail, join_config2)
            .add_exit_state(TestState::Complete);
        assert!(
            workflow2
                .orchestrate(TestState::Start, CancellationToken::disabled())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_percentage_over_one() {
        // Invalid percentage (>1.0) should return a configuration error
        let tasks = vec![
            FailTask::new(false), // One task succeeds
            FailTask::new(true),  // One task fails
        ];
        let join_config = JoinConfig::new(JoinStrategy::Percentage(1.5), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CanoError::Configuration(_)),
            "expected Configuration error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_quorum_zero() {
        let tasks = vec![FailTask::new(true), FailTask::new(true)];
        let join_config = JoinConfig::new(JoinStrategy::Quorum(0), TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_single_task_register_split() {
        let tasks = vec![SimpleTask::new(TestState::Complete)];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_workflow_no_exit_states() {
        // No exit states means validate() rejects the workflow before any task runs.
        let workflow =
            Workflow::bare().register(TestState::Start, SimpleTask::new(TestState::Complete));
        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        let err = result.unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(err.to_string().contains("no exit states"));
    }

    #[tokio::test]
    async fn test_split_task_from_single_register() {
        #[derive(Clone)]
        struct SplitReturningTask;

        #[task]
        impl Task<TestState> for SplitReturningTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Split(vec![TestState::Complete]))
            }
        }

        let workflow = Workflow::bare()
            .register(TestState::Start, SplitReturningTask)
            .add_exit_state(TestState::Complete);
        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("register_split"));
    }

    /// Regression test: a task registered in a workflow is retried exactly once per the
    /// configured retry count by the outer `execute_single_task` `run_with_retries`.
    #[tokio::test]
    async fn test_task_in_workflow_no_double_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        struct CountingTask {
            call_count: Arc<std::sync::atomic::AtomicUsize>,
        }

        #[task]
        impl Task<TestState> for CountingTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1))
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                Err(CanoError::task_execution("always fails"))
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let task = CountingTask {
            call_count: Arc::clone(&call_count),
        };

        let workflow = Workflow::bare()
            .register(TestState::Start, task)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(result.is_err());

        // With max_retries=2, there should be exactly 3 attempts (1 initial + 2 retries).
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            3,
            "task should be called exactly 3 times (1 + 2 retries)"
        );
    }

    /// Regression test: tasks registered via register_split() must honour their TaskConfig
    /// retry settings. Before the fix, split tasks called task.run() directly and retries
    /// were silently ignored.
    #[tokio::test]
    async fn test_split_task_retry_config_honoured() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone)]
        struct RetryCountingTask {
            call_count: Arc<AtomicUsize>,
            succeed_after: usize,
        }

        #[task]
        impl Task<TestState> for RetryCountingTask {
            fn config(&self) -> crate::task::TaskConfig {
                // Allow up to 4 retries so the task can eventually succeed
                crate::task::TaskConfig::new()
                    .with_fixed_retry(4, std::time::Duration::from_millis(1))
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
                if count >= self.succeed_after {
                    Ok(TaskResult::Single(TestState::Complete))
                } else {
                    Err(CanoError::task_execution("not ready yet"))
                }
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let tasks = vec![RetryCountingTask {
            call_count: Arc::clone(&call_count),
            succeed_after: 3, // fails twice, succeeds on third attempt
        }];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(result.is_ok(), "workflow should succeed after retries");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            3,
            "task should have been called exactly 3 times (2 failures + 1 success)"
        );
    }

    // ------------------------------------------------------------------
    // Resilience primitives: panic safety + bulkhead
    // ------------------------------------------------------------------

    struct PanickingTask;

    #[task]
    impl Task<TestState> for PanickingTask {
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::minimal()
        }

        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            panic!("boom");
        }
    }

    #[tokio::test]
    async fn test_single_task_panic_caught() {
        let workflow = Workflow::bare()
            .register(TestState::Start, PanickingTask)
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .expect_err("panic must surface as Err");
        // The FSM wraps the failure with state context; `.inner()` peels one layer.
        match err.inner() {
            CanoError::TaskExecution(msg) => {
                assert!(msg.contains("panic"), "expected 'panic' in: {msg}");
                assert!(msg.contains("boom"), "expected 'boom' in: {msg}");
            }
            other => panic!("expected TaskExecution, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_split_task_panic_reports_index_and_payload() {
        let workflow = Workflow::bare()
            .register_split(
                TestState::Start,
                vec![PanickingTask],
                JoinConfig::new(JoinStrategy::All, TestState::Complete),
            )
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .expect_err("split panic must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("task 0"),
            "expected split error to include task index, got: {msg}"
        );
        assert!(
            msg.contains("boom"),
            "expected split error to include panic payload, got: {msg}"
        );
    }

    #[derive(Clone)]
    struct ConcurrencyProbe {
        live: Arc<std::sync::atomic::AtomicUsize>,
        max: Arc<std::sync::atomic::AtomicUsize>,
        sleep: Duration,
    }

    #[task]
    impl Task<TestState> for ConcurrencyProbe {
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::minimal()
        }

        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            // Update peak via a CAS loop.
            let mut peak = self.max.load(Ordering::SeqCst);
            while now > peak {
                match self
                    .max
                    .compare_exchange(peak, now, Ordering::SeqCst, Ordering::SeqCst)
                {
                    Ok(_) => break,
                    Err(actual) => peak = actual,
                }
            }
            tokio::time::sleep(self.sleep).await;
            self.live.fetch_sub(1, Ordering::SeqCst);
            Ok(TaskResult::Single(TestState::Complete))
        }
    }

    #[tokio::test]
    async fn test_split_bulkhead_caps_concurrency() {
        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tasks: Vec<ConcurrencyProbe> = (0..10)
            .map(|_| ConcurrencyProbe {
                live: Arc::clone(&live),
                max: Arc::clone(&max),
                sleep: Duration::from_millis(50),
            })
            .collect();

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete).with_bulkhead(2);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
        let observed = max.load(Ordering::SeqCst);
        assert!(
            observed <= 2,
            "bulkhead breached: observed concurrency = {observed}"
        );
        assert!(observed >= 1, "no tasks ran?");
    }

    #[tokio::test]
    async fn test_split_bulkhead_zero_rejected() {
        let tasks = vec![SimpleTask::new(TestState::Complete)];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete).with_bulkhead(0);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .expect_err("bulkhead=0 must error");
        assert!(matches!(err, CanoError::Configuration(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn test_attempt_timeout_via_workflow_retries() {
        // Sanity check: per-attempt timeout integrates with the workflow's
        // single-task path through TaskConfig.
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::new()
                    .with_fixed_retry(1, Duration::from_millis(1))
                    .with_attempt_timeout(Duration::from_millis(20))
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let err = Workflow::bare()
            .register(TestState::Start, SlowTask)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .expect_err("expected attempt timeout to exhaust retries");
        // The FSM wraps the failure with state context; `.inner()` peels one layer.
        assert!(
            matches!(err.inner(), CanoError::RetryExhausted { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_split_tasks_share_circuit_breaker() {
        // N parallel split tasks share one Arc<CircuitBreaker>. They all fail
        // in the same workflow run; the breaker — protected by a std Mutex —
        // must record every failure correctly across the JoinSet workers and
        // end up Open.
        use crate::circuit::{CircuitBreaker, CircuitPolicy, CircuitState};

        struct FailingTask {
            breaker: Arc<CircuitBreaker>,
        }

        #[task]
        impl Task<TestState> for FailingTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal().with_circuit_breaker(Arc::clone(&self.breaker))
            }

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Err(CanoError::task_execution("always fails"))
            }
        }

        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 4,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        }));

        let tasks: Vec<FailingTask> = (0..4)
            .map(|_| FailingTask {
                breaker: Arc::clone(&breaker),
            })
            .collect();

        // `All` waits for every task to run to completion (and record its failure
        // against the shared breaker) before the workflow returns. We discard the
        // workflow result — `All` propagates an Err when any task fails — because
        // we only care about the breaker side-effect.
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let _ = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        assert!(
            matches!(breaker.state(), CircuitState::Open { .. }),
            "shared breaker must trip after 4 concurrent failures, got {:?}",
            breaker.state()
        );
    }

    // ------------------------------------------------------------------
    // register_stepped tests — no-store path (in-memory loop via engine)
    // ------------------------------------------------------------------

    use crate::TaskConfig;
    use crate::task::stepped::{StepOutcome, SteppedTask};

    #[tokio::test]
    async fn test_register_stepped_no_store_runs_to_completion() {
        // register_stepped with no checkpoint store behaves like register (in-memory loop).
        struct Counter {
            target: u32,
        }

        #[task_mod::stepped]
        impl SteppedTask<TestState> for Counter {
            type Cursor = u32;
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }
            async fn step(
                &self,
                _res: &Resources,
                cursor: Option<u32>,
            ) -> Result<StepOutcome<u32, TestState>, CanoError> {
                let n = cursor.unwrap_or(0) + 1;
                if n >= self.target {
                    Ok(StepOutcome::Done(TaskResult::Single(TestState::Complete)))
                } else {
                    Ok(StepOutcome::More(n))
                }
            }
        }

        let result = Workflow::bare()
            .register_stepped(TestState::Start, Counter { target: 5 })
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_register_stepped_split_result_is_rejected() {
        // StepOutcome::Done(TaskResult::Split) must return a workflow error.
        struct SplitStepper;

        #[task_mod::stepped]
        impl SteppedTask<TestState> for SplitStepper {
            type Cursor = u32;
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }
            async fn step(
                &self,
                _res: &Resources,
                _cursor: Option<u32>,
            ) -> Result<StepOutcome<u32, TestState>, CanoError> {
                Ok(StepOutcome::Done(TaskResult::Split(vec![
                    TestState::Complete,
                ])))
            }
        }

        let err = Workflow::bare()
            .register_stepped(TestState::Start, SplitStepper)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .expect_err("split result from stepped must error");
        // The FSM wraps the failure with state context; `.inner()` peels one layer.
        assert!(
            matches!(err.inner(), CanoError::Workflow(_)),
            "expected Workflow error, got {err:?}"
        );
        assert!(err.to_string().contains("split"), "got: {err}");
    }

    // ---- WithStateContext wrapping ----

    #[tokio::test]
    async fn orchestrate_wraps_task_failure_with_state_context() {
        // Use a no-retry fail task so the wrapped error's `source` is the raw
        // `TaskExecution`, not a `RetryExhausted`. `FailTask` itself uses the
        // default exponential-backoff retry policy, which would turn `attempt`
        // into the total attempt count and the source into `RetryExhausted`.
        #[derive(Clone)]
        struct FailNoRetry;
        #[task]
        impl Task<TestState> for FailNoRetry {
            fn config(&self) -> task_mod::TaskConfig {
                task_mod::TaskConfig::minimal()
            }
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Err(CanoError::task_execution("boom"))
            }
        }

        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, FailNoRetry)
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        match err {
            CanoError::WithStateContext {
                state,
                attempt,
                transitions_so_far,
                source,
            } => {
                assert_eq!(state, "Process");
                assert_eq!(attempt, 1);
                assert_eq!(
                    transitions_so_far,
                    vec!["Start".to_string(), "Process".to_string()]
                );
                assert!(matches!(*source, CanoError::TaskExecution(_)));
            }
            other => panic!("expected WithStateContext, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrapped_error_attempt_equals_max_attempts_on_retry_exhausted() {
        #[derive(Clone)]
        struct AlwaysFails;
        #[task]
        impl Task<TestState> for AlwaysFails {
            fn config(&self) -> task_mod::TaskConfig {
                task_mod::TaskConfig::new().with_fixed_retry(2, Duration::from_millis(0))
            }
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Err(CanoError::task_execution("nope"))
            }
        }
        let err = Workflow::bare()
            .register(TestState::Start, AlwaysFails)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        match err {
            CanoError::WithStateContext {
                attempt, source, ..
            } => {
                assert_eq!(attempt, 3, "1 initial + 2 retries");
                assert!(matches!(*source, CanoError::RetryExhausted { .. }));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn router_states_appear_in_wrapped_path() {
        use crate::task::RouterTask;

        struct AlwaysRoute;

        #[task_mod::router]
        impl RouterTask<TestState> for AlwaysRoute {
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Process))
            }
        }

        let workflow = Workflow::bare()
            .register_router(TestState::Start, AlwaysRoute)
            .register(TestState::Process, FailTask::new(true))
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        match err {
            CanoError::WithStateContext {
                transitions_so_far,
                state,
                ..
            } => {
                assert_eq!(state, "Process");
                assert_eq!(
                    transitions_so_far,
                    vec!["Start".to_string(), "Process".to_string()],
                    "router state must appear in path"
                );
            }
            other => panic!("expected WithStateContext, got {other:?}"),
        }
    }

    // ---- Total timeout integration into the forward FSM loop ----

    #[derive(Clone)]
    struct SleepyTask {
        sleep_ms: u64,
        next: TestState,
    }

    #[task]
    impl Task<TestState> for SleepyTask {
        fn config(&self) -> crate::task::TaskConfig {
            crate::task::TaskConfig::minimal()
        }
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            Ok(TaskResult::Single(self.next.clone()))
        }
    }

    #[tokio::test]
    async fn workflow_timeout_reports_attempt_zero_not_one() {
        // Regression: `attempts_from_error(WorkflowTimeout)` previously fell
        // through the `_` arm to `1`, which was indistinguishable from "one
        // full attempt completed and failed". The total-timeout drops the
        // in-flight future *before* an attempt body returns — same as
        // CircuitOpen — so the surfaced attempt count must be 0.
        let workflow = Workflow::bare()
            .with_total_timeout(Duration::from_millis(20))
            .register(
                TestState::Start,
                SleepyTask {
                    sleep_ms: 500,
                    next: TestState::Complete,
                },
            )
            .add_exit_state(TestState::Complete);
        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        match err {
            CanoError::WithStateContext {
                attempt, source, ..
            } => {
                assert!(
                    matches!(*source, CanoError::WorkflowTimeout { .. }),
                    "expected wrapped WorkflowTimeout, got {source:?}"
                );
                assert_eq!(
                    attempt, 0,
                    "WorkflowTimeout fires before an attempt completes — attempt should be 0"
                );
            }
            other => panic!("expected WithStateContext, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn total_timeout_returns_workflow_timeout_error() {
        let workflow = Workflow::bare()
            .with_total_timeout(Duration::from_millis(20))
            .register(
                TestState::Start,
                SleepyTask {
                    sleep_ms: 200,
                    next: TestState::Complete,
                },
            )
            .add_exit_state(TestState::Complete);
        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        // The engine wraps the timeout with state context; `.inner()` peels one layer.
        assert!(
            matches!(err.inner(), CanoError::WorkflowTimeout { .. }),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn total_timeout_fires_on_workflow_timeout_observer_hook() {
        let (obs, rec) = EventLog::new();
        let workflow = Workflow::bare()
            .with_total_timeout(Duration::from_millis(20))
            .with_observer(Arc::new(obs))
            .register(
                TestState::Start,
                SleepyTask {
                    sleep_ms: 200,
                    next: TestState::Complete,
                },
            )
            .add_exit_state(TestState::Complete);
        let _ = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await;
        let events = rec.timeouts();
        assert_eq!(events.len(), 1, "hook should fire exactly once");
        let (elapsed, limit) = events[0];
        assert_eq!(limit, Duration::from_millis(20));
        assert!(
            elapsed >= limit,
            "elapsed should be >= limit, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn total_timeout_triggers_compensation_drain() {
        // Saga safety: the FSM does NOT cancel compensatable tasks mid-await — a
        // committed side effect must always end up on the compensation stack. So
        // the way to land in compensation under total_timeout is for a *plain*
        // task after a compensatable step to overrun the budget; the engine then
        // cancels that plain task, surfaces `WorkflowTimeout`, and drains the
        // compensation stack the earlier compensatable step had pushed.
        use crate::saga::CompensatableTask;
        use std::sync::Mutex;

        type CompLog = Arc<Mutex<Vec<String>>>;
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));

        #[derive(Clone)]
        struct A {
            log: CompLog,
        }
        #[crate::saga::task]
        impl CompensatableTask<TestState> for A {
            type Output = u32;
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }
            fn name(&self) -> std::borrow::Cow<'static, str> {
                "A".into()
            }
            async fn run(
                &self,
                _res: &crate::resource::Resources,
            ) -> Result<(TaskResult<TestState>, u32), CanoError> {
                Ok((TaskResult::Single(TestState::Process), 7))
            }
            async fn compensate(
                &self,
                _res: &crate::resource::Resources,
                _output: u32,
            ) -> Result<(), CanoError> {
                self.log.lock().unwrap().push("A".to_string());
                Ok(())
            }
        }

        let workflow = Workflow::bare()
            .with_total_timeout(Duration::from_millis(40))
            .register_with_compensation(TestState::Start, A { log: log.clone() })
            .register(
                TestState::Process,
                SleepyTask {
                    sleep_ms: 500,
                    next: TestState::Complete,
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap_err();
        // Clean rollback → original error surfaced, wrapped with state context.
        assert!(
            matches!(err.inner(), CanoError::WorkflowTimeout { .. }),
            "got: {err}"
        );
        let log_entries = log.lock().unwrap().clone();
        assert_eq!(
            log_entries,
            vec!["A".to_string()],
            "A should have compensated after Process timed out"
        );
    }

    #[tokio::test]
    async fn compensatable_task_runs_to_completion_despite_total_timeout() {
        // Saga safety guarantee: a compensatable task is NEVER cancelled mid-await
        // by `with_total_timeout`. Otherwise an in-task side effect could commit
        // without a matching `CompensationEntry` ever being pushed.
        use crate::saga::CompensatableTask;
        use std::sync::Mutex;

        type CompLog = Arc<Mutex<Vec<String>>>;
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));

        #[derive(Clone)]
        struct SlowComp {
            log: CompLog,
        }
        #[crate::saga::task]
        impl CompensatableTask<TestState> for SlowComp {
            type Output = u32;
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::minimal()
            }
            fn name(&self) -> std::borrow::Cow<'static, str> {
                "SlowComp".into()
            }
            async fn run(
                &self,
                _res: &crate::resource::Resources,
            ) -> Result<(TaskResult<TestState>, u32), CanoError> {
                tokio::time::sleep(Duration::from_millis(80)).await;
                self.log.lock().unwrap().push("ran".to_string());
                Ok((TaskResult::Single(TestState::Complete), 42))
            }
            async fn compensate(
                &self,
                _res: &crate::resource::Resources,
                _output: u32,
            ) -> Result<(), CanoError> {
                self.log.lock().unwrap().push("compensated".to_string());
                Ok(())
            }
        }

        let workflow = Workflow::bare()
            .with_total_timeout(Duration::from_millis(20))
            .register_with_compensation(TestState::Start, SlowComp { log: log.clone() })
            .add_exit_state(TestState::Complete);

        // The compensatable task runs ~80ms despite the 20ms total budget — and the
        // workflow exits cleanly (no compensation runs, no error surfaces) because
        // there's no further state for the budget to cancel.
        let outcome = workflow
            .orchestrate(TestState::Start, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(outcome, TestState::Complete);
        let log_entries = log.lock().unwrap().clone();
        assert_eq!(log_entries, vec!["ran".to_string()]);
    }
}

/// Edge-case unit tests for the `wrap_and_drain` helper. The integration
/// sentinels (`pre_dispatch_failure_honors_bounded_compensation_deadline`,
/// `inline_compensate_failure_wraps_errors_0_in_with_state_context`,
/// `compensations_run_in_reverse_on_terminal_failure`) cover the helper
/// end-to-end through `execute_workflow_from`; this module pins down the
/// boundary behaviour in isolation.
#[cfg(test)]
mod wrap_and_drain_tests {
    use super::*;
    use crate::saga::CompensationEntry;
    use crate::workflow::test_support::TestState;

    fn bare_workflow() -> Workflow<TestState> {
        Workflow::<TestState>::bare()
    }

    #[tokio::test]
    async fn empty_stack_unbounded_returns_wrapped_original() {
        // No compensators to run, no total budget → just the wrapped error.
        let w = bare_workflow();
        let err = w
            .wrap_and_drain(
                None,
                Vec::new(),
                &TestState::Start,
                &[TestState::Start],
                CanoError::task_execution("boom"),
                None,
            )
            .await
            .expect_err("empty stack with err returns err");
        match err {
            CanoError::WithStateContext {
                state,
                attempt,
                source,
                ..
            } => {
                assert_eq!(state, "Start");
                assert_eq!(attempt, 1);
                assert!(matches!(*source, CanoError::TaskExecution(_)));
            }
            other => panic!("expected WithStateContext, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_stack_bounded_still_returns_wrapped_original() {
        // With a budget but no compensators on the stack, the deadline never
        // matters — the helper short-circuits via `run_compensations_bounded`'s
        // empty-stack guard and surfaces the wrapped original.
        let w = bare_workflow();
        let total_budget = Some((
            std::time::Instant::now(),
            std::time::Duration::from_millis(50),
        ));
        let err = w
            .wrap_and_drain(
                None,
                Vec::new(),
                &TestState::Start,
                &[TestState::Start],
                CanoError::task_execution("boom-bounded"),
                total_budget,
            )
            .await
            .expect_err("empty stack returns the wrapped err");
        assert!(matches!(err, CanoError::WithStateContext { .. }));
    }

    #[tokio::test]
    async fn already_wrapped_with_state_context_is_idempotent() {
        // F6 invariant: the constructor refuses to double-wrap, so an err
        // that is *already* WithStateContext flows through unchanged. This
        // matters when an inner workflow's error surfaces from a task.
        let w = bare_workflow();
        let already = CanoError::with_state_context(
            "InnerState",
            3,
            vec!["InnerStart".to_string(), "InnerState".to_string()],
            CanoError::task_execution("nested"),
        );
        let err = w
            .wrap_and_drain(
                None,
                Vec::new(),
                &TestState::Start,
                &[TestState::Start],
                already,
                None,
            )
            .await
            .expect_err("already-wrapped err returned");
        match err {
            CanoError::WithStateContext { state, attempt, .. } => {
                // Inner state/attempt preserved — outer wrap was a no-op.
                assert_eq!(state, "InnerState");
                assert_eq!(attempt, 3);
            }
            other => panic!("expected WithStateContext, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compensation_failed_envelope_wraps_errors_zero() {
        // F6 invariant: when the err is a CompensationFailed envelope, the
        // constructor wraps `errors[0]` so downstream code matching on the
        // first error still sees WithStateContext.
        let w = bare_workflow();
        let envelope = CanoError::compensation_failed(vec![
            CanoError::task_execution("original cause"),
            CanoError::task_execution("inline-compensate failure"),
        ]);
        let err = w
            .wrap_and_drain(
                None,
                Vec::new(),
                &TestState::Start,
                &[TestState::Start],
                envelope,
                None,
            )
            .await
            .expect_err("envelope returned");
        match err {
            CanoError::CompensationFailed { errors } => {
                assert_eq!(errors.len(), 2);
                match &errors[0] {
                    CanoError::WithStateContext { state, attempt, .. } => {
                        assert_eq!(state, "Start");
                        // Derived from the envelope by `attempts_from_error`,
                        // which returns 1 for a `CompensationFailed` (it doesn't
                        // dig into `errors[0]`) — matching the production
                        // post-dispatch arm, which passes the same derived value.
                        assert_eq!(*attempt, 1);
                    }
                    other => panic!("errors[0] must be wrapped, got {other:?}"),
                }
                // errors[1] must still be the inline-compensate error verbatim.
                assert!(errors[1].message().contains("inline-compensate failure"));
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_compensator_for_stack_entry_emits_orphaned_compensation() {
        // A rehydrated stack entry whose `task_id` has no registered
        // compensator must surface as `OrphanedCompensation` carrying the
        // persisted blob — without this, the operator's manual-rollback
        // path loses the data.
        let w = bare_workflow();
        let stack = vec![CompensationEntry {
            task_id: Arc::from("UnknownTask"),
            output_blob: b"forty-two".to_vec(),
        }];
        let err = w
            .wrap_and_drain(
                None,
                stack,
                &TestState::Start,
                &[TestState::Start],
                CanoError::task_execution("trigger"),
                None,
            )
            .await
            .expect_err("orphan compensator surfaces error");
        match err {
            CanoError::CompensationFailed { errors } => {
                let blob = errors.iter().find_map(|e| match e {
                    CanoError::OrphanedCompensation {
                        task_id,
                        output_blob,
                    } => Some((task_id.to_string(), output_blob.clone())),
                    _ => None,
                });
                let (task_id, blob) = blob.expect("must include OrphanedCompensation");
                assert_eq!(task_id, "UnknownTask");
                assert_eq!(blob, b"forty-two");
            }
            other => panic!("expected CompensationFailed envelope, got {other:?}"),
        }
    }
}

/// Edge-case unit tests for the `dispatch_with_budget` helper. Each variant
/// of `StateEntry` is already exercised end-to-end by the orchestrate-level
/// tests above; these tests pin down the helper's behavior in isolation —
/// what happens when there are no observers, when the future returns first,
/// when the deadline is already past, etc.
#[cfg(test)]
mod dispatch_with_budget_tests {
    use super::*;
    use crate::observer::WorkflowObserver;
    use crate::workflow::test_support::{EventLog, TestState};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn budget_for(limit: Duration) -> Option<(std::time::Instant, Duration, tokio::time::Instant)> {
        let start = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + limit;
        Some((start, limit, deadline))
    }

    #[tokio::test]
    async fn no_budget_passes_future_result_through_unchanged() {
        // When `step_budget` is None we are on the zero-cost path; the helper
        // must NOT touch observers and must return the inner result verbatim.
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];

        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);
        let result = Workflow::<TestState>::dispatch_with_budget(
            None,
            &CancellationToken::disabled(),
            None,
            &observers,
            async { Ok::<TestState, CanoError>(TestState::Complete) },
            |_o, _err| {
                cc.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .expect("no budget = pass-through");
        assert_eq!(result, TestState::Complete);
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "fan-out closure must not run on the no-budget path"
        );
        assert!(rec.is_empty());
    }

    #[tokio::test]
    async fn budget_with_timely_future_passes_through_without_observer_fanout() {
        // The deadline is comfortably in the future; the inner future returns
        // immediately. Helper returns the result; fan-out closure stays silent.
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];

        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);
        let result = Workflow::<TestState>::dispatch_with_budget(
            budget_for(Duration::from_secs(60)),
            &CancellationToken::disabled(),
            None,
            &observers,
            async { Ok::<TestState, CanoError>(TestState::Complete) },
            |_o, _err| {
                cc.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .expect("timely future succeeds");
        assert_eq!(result, TestState::Complete);
        assert_eq!(call_count.load(Ordering::SeqCst), 0);
        assert!(rec.is_empty());
    }

    #[tokio::test]
    async fn budget_trip_synthesizes_workflow_timeout_and_fires_fan_out_once() {
        // A deadline already in the past trips the wrapper immediately. The
        // fan-out closure must run for each registered observer; the helper
        // must always fire `on_workflow_timeout`. Result is WorkflowTimeout.
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];

        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);
        // Long-running future + already-expired deadline forces the timeout.
        let err = Workflow::<TestState>::dispatch_with_budget(
            Some((
                std::time::Instant::now() - Duration::from_secs(1),
                Duration::from_millis(50),
                tokio::time::Instant::now() - Duration::from_millis(1),
            )),
            &CancellationToken::disabled(),
            None,
            &observers,
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<TestState, CanoError>(TestState::Complete)
            },
            |o, err| {
                cc.fetch_add(1, Ordering::SeqCst);
                o.on_task_failure("synthetic", err);
            },
        )
        .await
        .expect_err("expired deadline must trip");
        assert!(
            matches!(err, CanoError::WorkflowTimeout { .. }),
            "got: {err}"
        );
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "fan-out closure must fire exactly once (per registered observer)"
        );
        assert_eq!(
            rec.labels(),
            vec![
                "task_failure:synthetic".to_string(),
                "workflow_timeout:50ms".to_string(),
            ],
            "observer must see both events, in this order"
        );
    }

    #[tokio::test]
    async fn budget_trip_with_empty_observer_slice_still_returns_workflow_timeout() {
        // Empty observer slice: the fan-out closure is never called, but the
        // helper still synthesizes the WorkflowTimeout error.
        let observers: [Arc<dyn WorkflowObserver>; 0] = [];
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);
        let err = Workflow::<TestState>::dispatch_with_budget(
            Some((
                std::time::Instant::now() - Duration::from_secs(1),
                Duration::from_millis(10),
                tokio::time::Instant::now() - Duration::from_millis(1),
            )),
            &CancellationToken::disabled(),
            None,
            &observers,
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<TestState, CanoError>(TestState::Complete)
            },
            |_o, _err| {
                cc.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .expect_err("must still time out");
        assert!(matches!(err, CanoError::WorkflowTimeout { .. }));
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "fan-out closure never runs without observers"
        );
    }

    #[tokio::test]
    async fn budget_trip_fires_fan_out_once_per_observer() {
        // Two observers: the fan-out closure must run once per observer, and
        // each observer must see `on_workflow_timeout` exactly once.
        let (a_obs, a_rec) = EventLog::new();
        let (b_obs, b_rec) = EventLog::new();
        let observers: [Arc<dyn WorkflowObserver>; 2] = [Arc::new(a_obs), Arc::new(b_obs)];
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);
        let err = Workflow::<TestState>::dispatch_with_budget(
            Some((
                std::time::Instant::now() - Duration::from_secs(1),
                Duration::from_millis(5),
                tokio::time::Instant::now() - Duration::from_millis(1),
            )),
            &CancellationToken::disabled(),
            None,
            &observers,
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<TestState, CanoError>(TestState::Complete)
            },
            |o, err| {
                cc.fetch_add(1, Ordering::SeqCst);
                o.on_task_failure("multi", err);
            },
        )
        .await
        .expect_err("must time out");
        assert!(matches!(err, CanoError::WorkflowTimeout { .. }));
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            2,
            "fan-out closure runs once per observer"
        );
        // Each observer must see both events exactly once.
        let expected = vec![
            "task_failure:multi".to_string(),
            "workflow_timeout:5ms".to_string(),
        ];
        assert_eq!(a_rec.labels(), expected);
        assert_eq!(b_rec.labels(), expected);
    }

    #[tokio::test]
    async fn budget_does_not_swallow_inner_future_errors() {
        // When the inner future returns Err *before* the deadline, the helper
        // must surface that error unchanged — NOT convert it to WorkflowTimeout.
        let observers: [Arc<dyn WorkflowObserver>; 0] = [];
        let err = Workflow::<TestState>::dispatch_with_budget(
            budget_for(Duration::from_secs(60)),
            &CancellationToken::disabled(),
            None,
            &observers,
            async { Err::<TestState, _>(CanoError::task_execution("custom inner err")) },
            |_o, _err| {},
        )
        .await
        .expect_err("inner future errored before deadline");
        assert!(
            matches!(err, CanoError::TaskExecution(ref m) if m == "custom inner err"),
            "must propagate the inner err verbatim, got: {err}"
        );
    }

    // ----- cancellation path (token can fire ⇒ the `select!` arm is taken) -----

    #[tokio::test]
    async fn live_token_uncancelled_passes_future_through() {
        // A real (not `never`) token that never fires takes the `select!` path, but the
        // `res = budgeted` arm must still return the future's Ok with no observer events.
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];
        let (_handle, token) = CancellationToken::new();
        let result = Workflow::<TestState>::dispatch_with_budget(
            None,
            &token,
            None,
            &observers,
            async { Ok::<TestState, CanoError>(TestState::Complete) },
            |o, err| o.on_task_failure("synthetic", err),
        )
        .await
        .expect("uncancelled live token returns the future's Ok");
        assert_eq!(result, TestState::Complete);
        assert!(
            rec.is_empty(),
            "no observer events when the task completes normally"
        );
    }

    #[tokio::test]
    async fn live_token_with_expired_budget_still_times_out() {
        // Regression: adding the cancellation race must not break the budget timeout.
        // A live-but-uncancelled token + an already-expired deadline must still trip
        // `WorkflowTimeout` (and fire the timeout hooks, not `on_cancelled`).
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];
        let (_handle, token) = CancellationToken::new();
        let err = Workflow::<TestState>::dispatch_with_budget(
            Some((
                std::time::Instant::now() - Duration::from_secs(1),
                Duration::from_millis(5),
                tokio::time::Instant::now() - Duration::from_millis(1),
            )),
            &token,
            Some("StateX"),
            &observers,
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<TestState, CanoError>(TestState::Complete)
            },
            |o, err| o.on_task_failure("synthetic", err),
        )
        .await
        .expect_err("expired budget must trip even with a live token");
        assert!(
            matches!(err, CanoError::WorkflowTimeout { .. }),
            "got: {err}"
        );
        let labels = rec.labels();
        assert!(
            labels.iter().any(|l| l.starts_with("workflow_timeout:")),
            "timeout hook fired: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.starts_with("cancelled:")),
            "on_cancelled must NOT fire on a timeout: {labels:?}"
        );
    }

    #[tokio::test]
    async fn precancelled_token_returns_cancelled_and_fires_hooks() {
        // The new cancel arm: a pre-cancelled token (biased `select!` picks it) must drop
        // the inner future, return `Cancelled`, fire the per-task fan-out (gauge balance)
        // AND `on_cancelled(state_label)` — but not the timeout hook.
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];
        let (handle, token) = CancellationToken::new();
        handle.cancel();
        let err = Workflow::<TestState>::dispatch_with_budget(
            None,
            &token,
            Some("StateX"),
            &observers,
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<TestState, CanoError>(TestState::Complete)
            },
            |o, err| o.on_task_failure("synthetic", err),
        )
        .await
        .expect_err("pre-cancelled token returns Cancelled");
        assert!(matches!(err, CanoError::Cancelled), "got: {err}");
        let labels = rec.labels();
        assert!(
            labels.iter().any(|l| l == "task_failure:synthetic"),
            "fan-out (gauge balance) fired: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "cancelled:StateX"),
            "on_cancelled fired with the state label: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.starts_with("workflow_timeout:")),
            "timeout hook must NOT fire on a cancel: {labels:?}"
        );
    }

    #[tokio::test]
    async fn precancelled_token_without_state_label_skips_on_cancelled() {
        // When no state label is available (no observers/checkpoint/metrics need it),
        // the cancel arm still returns `Cancelled` and runs the fan-out, but does not
        // attempt to fire `on_cancelled`.
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];
        let (handle, token) = CancellationToken::new();
        handle.cancel();
        let err = Workflow::<TestState>::dispatch_with_budget(
            None,
            &token,
            None, // no label
            &observers,
            async { Ok::<TestState, CanoError>(TestState::Complete) },
            |o, err| o.on_task_failure("synthetic", err),
        )
        .await
        .expect_err("pre-cancelled token returns Cancelled");
        assert!(matches!(err, CanoError::Cancelled), "got: {err}");
        let labels = rec.labels();
        assert!(
            !labels.iter().any(|l| l.starts_with("cancelled:")),
            "on_cancelled must not fire without a state label: {labels:?}"
        );
    }
}
