//! The forward FSM execution loop: dispatching state entries, running single
//! and split tasks, and collecting split results against a [`JoinStrategy`].
//!
//! [`StateEntry`] lives here because the dispatch `match` in
//! [`execute_workflow_from`](Workflow::execute_workflow_from) is its primary
//! consumer. Compensation drain / resume live in the sibling
//! [`compensation`](super::compensation) module.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;

use crate::checkpoint::CheckpointRow;
use crate::error::CanoError;
use crate::saga::{
    CompensatableAdapter, CompensatableTask, CompensationEntry, ErasedCompensatable,
};
use crate::task::stepped::{CursorSink, ErasedSteppedTask, SteppedAdapter, SteppedTask};
use crate::task::stream::{ErasedStreamTask, ErasedWindowStep, StreamAdapter, StreamTask};
use crate::task::suspend::{ErasedSuspendTask, SuspendAdapter, SuspendTask};
use crate::task::{ObserverCtx, RouterTask, Task, TaskConfig, run_with_retries_observed};
use futures_util::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use super::compensation::resolve_compensation_deadline;
use super::join::{JoinConfig, JoinStrategy};
use super::{Workflow, notify_observers, panic_payload_message};

#[cfg(feature = "tracing")]
use tracing::{Instrument, debug, info, info_span, warn};

/// Entry in the workflow state machine
/// How many of the most recent states `CanoError::WithStateContext::transitions_so_far`
/// reports.
pub(super) const MAX_TRANSITION_PATH: usize = 64;

/// What a state runs, as produced by [`Registrable::into_entry`](super::Registrable::into_entry).
///
/// Opaque: build one through `Workflow::register` (the task macros implement
/// [`Registrable`](super::Registrable) for you).
#[doc(hidden)]
pub struct StateEntry<TState>(pub(crate) Entry<TState>);

/// A registered state's engine path, with the task's name and config captured once
/// at registration (no per-dispatch virtual calls).
pub(crate) enum Entry<TState> {
    /// A plain task. `checkpoint: false` marks a router: dispatched the same way, but
    /// skipped by the checkpoint writer (no row, no sequence number) since it has no
    /// side effects to recover.
    Single {
        task: Arc<dyn Task<TState>>,
        config: TaskConfig,
        name: Cow<'static, str>,
        checkpoint: bool,
    },
    /// Parallel tasks joined by `join_config` (`Workflow::register_split`).
    Split {
        tasks: Vec<Arc<dyn Task<TState>>>,
        configs: Vec<Arc<TaskConfig>>,
        names: Vec<Cow<'static, str>>,
        join_config: JoinConfig<TState>,
    },
    /// A task whose output is pushed onto the compensation stack.
    Compensatable {
        task: Arc<dyn ErasedCompensatable<TState>>,
        config: TaskConfig,
        name: Arc<str>,
    },
    /// An engine-driven step loop persisting each cursor as a
    /// [`RowKind::StepCursor`](crate::checkpoint::RowKind::StepCursor) row.
    Stepped {
        task: Arc<dyn ErasedSteppedTask<TState>>,
        config: TaskConfig,
        name: Cow<'static, str>,
    },
    /// An engine-driven windowed stream; the driver observes the cancellation token
    /// itself (a cooperative drain) instead of being dropped by `dispatch_with_budget`.
    Stream {
        task: Arc<dyn ErasedStreamTask<TState>>,
        config: TaskConfig,
        name: Cow<'static, str>,
    },
    /// A task that parks the run after `suspend` (writing a
    /// [`RowKind::Suspended`](crate::checkpoint::RowKind::Suspended) row) and continues with
    /// `resume` when `Workflow::resume_with` delivers an input.
    Suspend {
        task: Arc<dyn ErasedSuspendTask<TState>>,
        config: TaskConfig,
        name: Cow<'static, str>,
    },
}

#[doc(hidden)]
impl<TState> StateEntry<TState>
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// A plain task (also used for poll, timer and batch tasks).
    pub fn single<T: Task<TState> + 'static>(task: T) -> Self {
        Self(Entry::Single {
            config: task.config(),
            name: task.name(),
            task: Arc::new(task),
            checkpoint: true,
        })
    }

    /// A router: dispatched like [`single`](Self::single) but never checkpointed.
    pub fn router<T: RouterTask<TState> + Task<TState> + 'static>(task: T) -> Self {
        Self(Entry::Single {
            config: Task::config(&task),
            name: Task::name(&task),
            task: Arc::new(task),
            checkpoint: false,
        })
    }

    /// An engine-driven stepped task.
    pub fn stepped<T: SteppedTask<TState> + 'static>(task: T) -> Self {
        Self(Entry::Stepped {
            config: SteppedTask::config(&task),
            name: SteppedTask::name(&task),
            task: Arc::new(SteppedAdapter(task)),
        })
    }

    /// An engine-driven stream task.
    pub fn stream<T: StreamTask<TState> + 'static>(task: T) -> Self {
        Self(Entry::Stream {
            config: StreamTask::config(&task),
            name: StreamTask::name(&task),
            task: Arc::new(StreamAdapter(Arc::new(task))),
        })
    }

    /// An engine-driven suspend task.
    pub fn suspend<T: SuspendTask<TState> + 'static>(task: T) -> Self {
        Self(Entry::Suspend {
            config: SuspendTask::config(&task),
            name: SuspendTask::name(&task),
            task: Arc::new(SuspendAdapter(task)),
        })
    }

    /// A compensatable (saga) task. The retry loop's config drops `attempt_timeout`:
    /// the adapter applies it to the forward call only, so a timeout can never drop
    /// the inline-compensate path and lose the typed output.
    pub fn compensatable<T: CompensatableTask<TState> + 'static>(task: T) -> Self {
        Self(Entry::Compensatable {
            config: TaskConfig {
                attempt_timeout: None,
                ..task.config()
            },
            name: Arc::from(task.name()),
            task: Arc::new(CompensatableAdapter(Arc::new(task))),
        })
    }
}

impl<TState> Entry<TState> {
    /// The `task_id` recorded on this state's checkpoint rows (`""` for a split).
    pub(crate) fn task_id(&self) -> &str {
        match self {
            Entry::Single { name, .. }
            | Entry::Stepped { name, .. }
            | Entry::Stream { name, .. }
            | Entry::Suspend { name, .. } => name,
            Entry::Compensatable { name, .. } => name,
            Entry::Split { .. } => "",
        }
    }
}

/// Writes a stepped task's cursors as [`RowKind::StepCursor`](crate::checkpoint::RowKind::StepCursor)
/// rows through [`Workflow::append_row`].
pub(super) struct CheckpointSink<'a, TState>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
{
    workflow: &'a Workflow<TState>,
    workflow_id: &'a str,
    state_label: &'a str,
    task_name: &'a str,
    sequence: &'a mut u64,
}

impl<TState> CursorSink for CheckpointSink<'_, TState>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
{
    fn save<'a>(&'a mut self, cursor_json: Vec<u8>) -> BoxFuture<'a, Result<(), CanoError>> {
        Box::pin(async move {
            let row = CheckpointRow::new(*self.sequence, self.state_label, self.task_name)
                .with_cursor(cursor_json)
                .with_workflow_version(self.workflow.workflow_version);
            self.workflow
                .append_row(
                    self.workflow_id,
                    self.sequence,
                    row,
                    "step cursor checkpoint",
                )
                .await
        })
    }
}

/// What a split's branches did: successful branch indices, failed branches with their
/// errors (`usize::MAX` = a branch whose `JoinSet` slot panicked or was aborted, index
/// lost), and how many never completed.
struct SplitOutcome {
    successes: Vec<usize>,
    errors: Vec<(usize, CanoError)>,
    cancelled: usize,
    completed_indices: Vec<bool>,
    timed_out: bool,
}

fn split_error_summary(errors: &[(usize, CanoError)]) -> String {
    const MAX_ERRORS_TO_REPORT: usize = 3;

    let mut parts: Vec<String> = errors
        .iter()
        .take(MAX_ERRORS_TO_REPORT)
        .map(|(idx, e)| format!("task {idx}: {e}"))
        .collect();

    if errors.len() > MAX_ERRORS_TO_REPORT {
        parts.push(format!(
            "... and {} more error(s)",
            errors.len() - MAX_ERRORS_TO_REPORT
        ));
    }

    parts.join("; ")
}

impl<TState> Workflow<TState>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
{
    /// The FSM loop, parameterized by the starting checkpoint sequence, the workflow id
    /// to record under, and the (possibly pre-populated, on resume) compensation stack.
    /// `run_workflow` enters with `(state, 0, self.workflow_id, [])`;
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
        // Input delivered by `resume_with` (or re-delivered by `resume_from`) for the
        // `Suspend` state the run re-enters. `Some` only on that resume; the re-entry
        // writes no `StateEntry` row so the log keeps ending in the `ResumeInput` row
        // until the next state's row is written.
        mut resume_input: Option<Vec<u8>>,
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
        // Cooperative-cancellation signal. `None` (plain `orchestrate`/`resume_from`)
        // skips the cancellation `select!` entirely — the zero-cost path.
        token: Option<&CancellationToken>,
    ) -> Result<TState, CanoError> {
        let mut current_state = initial_state;
        let mut sequence = start_sequence;
        // Path of states entered so far (one push per loop iteration, including
        // router states and the terminal exit state). Pre-populated with the
        // resumed run's pre-crash path so the wrapped error's `path` field is
        // truthful across crashes. Stored as `TState` to avoid per-iteration
        // `format!` allocations on the success path; formatted to `Vec<String>`
        // only when folded into a `WithStateContext` on failure.
        let mut transitions_so_far: VecDeque<TState> = prior_transitions.into();
        if let Some(excess) = transitions_so_far.len().checked_sub(MAX_TRANSITION_PATH) {
            transitions_so_far.drain(..excess);
        }

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
        let step_budget: Option<(std::time::Instant, std::time::Duration, crate::rt::Instant)> =
            total_budget.map(|(start, limit)| (start, limit, super::deadline_after(limit)));

        let mut skip_entry_row = resume_input.is_some();
        loop {
            // Cooperative cancellation observed at a state boundary: stop before
            // entering this state, fire `on_cancelled` once, and drain whatever
            // compensatable work has completed so far. `is_cancelled()` is a
            // non-blocking poll; the no-token path skips it. `current_state` is not
            // yet pushed onto `transitions_so_far`, so the reported path stops
            // *before* the state we declined to run.
            if token.is_some_and(CancellationToken::is_cancelled) {
                let label = format!("{current_state:?}");
                notify_observers(&self.observers, |o| o.on_cancelled(&label));
                return self
                    .wrap_and_drain(
                        workflow_id.as_deref(),
                        compensation_stack,
                        &current_state,
                        transitions_so_far.make_contiguous(),
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
            // states) appears in `transitions_so_far`, which keeps only the most
            // recent `MAX_TRANSITION_PATH` states so a cyclic workflow can't grow it
            // without bound.
            if transitions_so_far.len() == MAX_TRANSITION_PATH {
                transitions_so_far.pop_front();
            }
            transitions_so_far.push_back(current_state.clone());

            // Notify observers of every state entry, including the initial state
            // and any terminal exit state.
            if let Some(ref label) = state_label {
                notify_observers(&self.observers, |o| o.on_state_enter(label));
            }

            // One map lookup per iteration; everything below borrows from it.
            let entry: Option<&Entry<TState>> = self.states.get(&current_state).map(|e| &**e);

            // Checkpoint this state entry before touching its task. One row per state —
            // a split is one state, hence one row regardless of fan-out. Router states
            // (`checkpoint: false`) are pure routing with nothing to recover: no row and
            // no sequence number, so sequence numbers stay dense for `resume_from`.
            // A suspend state re-entered with its delivered input writes no row either
            // (`skip_entry_row`, first iteration only).
            // `on_state_enter` already fired above — the FSM did enter the state.
            let skip = std::mem::take(&mut skip_entry_row);
            if self.checkpoint_store.is_some()
                && !skip
                && !matches!(
                    entry,
                    Some(Entry::Single {
                        checkpoint: false,
                        ..
                    })
                )
            {
                let Some(wf_id) = workflow_id.as_deref() else {
                    let err = CanoError::checkpoint_store(
                        "checkpointing requires a workflow id (call with_workflow_id)",
                    );
                    return self
                        .wrap_and_drain(
                            workflow_id.as_deref(),
                            compensation_stack,
                            &current_state,
                            transitions_so_far.make_contiguous(),
                            err,
                            total_budget,
                        )
                        .await;
                };
                // `state_label` is always `Some` here (checkpoint_store ⇒ label computed).
                let label = state_label.as_deref().unwrap_or_default();
                let row = CheckpointRow::new(sequence, label, entry.map_or("", Entry::task_id))
                    .with_workflow_version(self.workflow_version);
                if let Err(err) = self
                    .append_row(wf_id, &mut sequence, row, "checkpoint")
                    .await
                {
                    return self
                        .wrap_and_drain(
                            workflow_id.as_deref(),
                            compensation_stack,
                            &current_state,
                            transitions_so_far.make_contiguous(),
                            err,
                            total_budget,
                        )
                        .await;
                }
            }

            // Reached an exit state — the run succeeded. Clear the log (best-effort) and return.
            if self.exit_states.contains(&current_state) {
                #[cfg(feature = "tracing")]
                info!(final_state = ?current_state, "Workflow completed successfully");
                self.clear_checkpoint_log(workflow_id.as_deref()).await;
                return Ok(current_state);
            }

            let Some(entry) = entry else {
                let err = CanoError::workflow(format!(
                    "No task registered for state: {:?}",
                    current_state
                ));
                return self
                    .wrap_and_drain(
                        workflow_id.as_deref(),
                        compensation_stack,
                        &current_state,
                        transitions_so_far.make_contiguous(),
                        err,
                        total_budget,
                    )
                    .await;
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
            let step: Result<TState, CanoError> = match entry {
                Entry::Single {
                    task, config, name, ..
                } => {
                    let fut = self.execute_single_task(task.as_ref(), config, name);
                    Self::dispatch_with_budget(
                        step_budget,
                        token,
                        state_label.as_deref(),
                        &self.observers,
                        fut,
                        |o, err| {
                            // `execute_single_task` fired `on_task_start` inside the
                            // dropped future; pair it with `on_task_failure` so observer
                            // gauges (`active_tasks` etc.) remain balanced.
                            o.on_task_failure(name, err);
                        },
                    )
                    .await
                }
                Entry::Split {
                    tasks,
                    configs,
                    names,
                    join_config,
                } => {
                    let fut = self.execute_split_join(tasks, configs, names, join_config);
                    // Pre-format branch ids once (instead of per observer) — the
                    // helper invokes `task_failure_fan_out` once per observer, so
                    // formatting inside the closure would re-allocate every time.
                    // `execute_split_join` fires `on_task_start` per branch; on
                    // outer cancellation OR a total-timeout trip those branches are
                    // dropped, so we fire a synthetic per-branch `on_task_failure`
                    // to keep observer gauges balanced — needed whenever the
                    // dispatch can be aborted (a budget deadline or a live token) and
                    // someone is listening.
                    let branch_ids: Vec<String> = if (step_budget.is_some() || token.is_some())
                        && !self.observers.is_empty()
                    {
                        names
                            .iter()
                            .enumerate()
                            .map(|(idx, name)| format!("{name}[{idx}]"))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Self::dispatch_with_budget(
                        step_budget,
                        token,
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
                Entry::Compensatable { task, config, name } => {
                    // Saga safety: do NOT wrap `execute_compensatable_task` in `timeout_at`.
                    // The compensation entry is pushed onto the stack *after* the task body
                    // returns its serialized output (see the `Ok((next_state, output_blob))`
                    // branch below). A mid-await cancel would drop the future *after* an
                    // in-task side effect committed, leaving the engine with no entry to
                    // roll back. Bounding via the task's own `attempt_timeout` is the right
                    // level — there the task author controls the commit-vs-yield ordering.
                    // The cost is at most one task duration of total-budget drift; the
                    // alternative — losing track of a committed side effect — is worse.
                    let comp_result = self
                        .execute_compensatable_task(task.as_ref(), config, name)
                        .await;
                    #[cfg(feature = "metrics")]
                    crate::metrics::task_dispatch_duration(
                        _state_label,
                        "compensatable",
                        _dispatch_started.elapsed(),
                    );
                    match comp_result {
                        Ok((next_state, output_blob)) => {
                            // Persist a completion row carrying the output so a resumed
                            // run rehydrates this compensation entry.
                            if self.checkpoint_store.is_some()
                                && let Some(wf_id) = workflow_id.as_deref()
                            {
                                let label = state_label.as_deref().unwrap_or_default();
                                let row = CheckpointRow::new(sequence, label, &**name)
                                    .with_output(output_blob.clone())
                                    .with_workflow_version(self.workflow_version);
                                if let Err(err) = self
                                    .append_row(
                                        wf_id,
                                        &mut sequence,
                                        row,
                                        "compensation checkpoint",
                                    )
                                    .await
                                {
                                    compensation_stack.push(CompensationEntry {
                                        task_id: Arc::clone(name),
                                        output_blob,
                                    });
                                    return self
                                        .wrap_and_drain(
                                            workflow_id.as_deref(),
                                            compensation_stack,
                                            &current_state,
                                            transitions_so_far.make_contiguous(),
                                            err,
                                            total_budget,
                                        )
                                        .await;
                                }
                            }
                            compensation_stack.push(CompensationEntry {
                                task_id: Arc::clone(name),
                                output_blob,
                            });
                            Ok(next_state)
                        }
                        Err(e) => Err(e),
                    }
                }
                Entry::Stepped { task, config, name } => {
                    // Pop the resume cursor for this state (if any). Using `remove` instead
                    // of `get` so that if the FSM visits the same state again later (e.g. a
                    // loop), that second visit starts fresh from `None`.
                    let resume_cursor = state_label
                        .as_deref()
                        .and_then(|label| resume_cursors.remove(label));
                    let fut = self.execute_stepped_task(
                        task.as_ref(),
                        config,
                        name,
                        workflow_id.as_deref(),
                        state_label.as_deref().unwrap_or_default(),
                        &mut sequence,
                        resume_cursor,
                    );
                    Self::dispatch_with_budget(
                        step_budget,
                        token,
                        state_label.as_deref(),
                        &self.observers,
                        fut,
                        |o, err| {
                            o.on_task_failure(name, err);
                        },
                    )
                    .await
                }
                Entry::Stream { task, config, name } => {
                    // A stream runs until cancelled/exhausted and must FLUSH the in-flight
                    // window + transition cleanly on cancel or timeout. So — like
                    // Compensatable — it is NOT wrapped in `dispatch_with_budget`'s
                    // drop-on-cancel `select!` (which would orphan the partial window).
                    // Instead `token` and `step_budget` are threaded INTO
                    // `execute_stream_task`, which folds them into one forwarded token
                    // observed cooperatively at the next item boundary, flushes, and
                    // returns a clean next state (or a clean `Cancelled`/`WorkflowTimeout`).
                    let resume_cursor = state_label
                        .as_deref()
                        .and_then(|label| resume_cursors.remove(label));
                    self.execute_stream_task(
                        task.as_ref(),
                        config,
                        name,
                        workflow_id.as_deref(),
                        state_label.as_deref().unwrap_or_default(),
                        &mut sequence,
                        resume_cursor,
                        token,
                        step_budget,
                    )
                    .await
                }
                Entry::Suspend { task, config, name } => {
                    let obs = ObserverCtx::new(&self.observers, name);
                    let fan = |o: &dyn crate::observer::WorkflowObserver, err: &CanoError| {
                        o.on_task_failure(name, err);
                    };
                    match resume_input.take() {
                        Some(input) => {
                            let fut = self.execute_suspend_call(
                                name,
                                task.resume(&self.resources, config, obs, &input),
                            );
                            Self::dispatch_with_budget(
                                step_budget,
                                token,
                                state_label.as_deref(),
                                &self.observers,
                                fut,
                                fan,
                            )
                            .await
                        }
                        None => {
                            let fut = self.execute_suspend_call(
                                name,
                                task.suspend(&self.resources, config, obs),
                            );
                            match Self::dispatch_with_budget(
                                step_budget,
                                token,
                                state_label.as_deref(),
                                &self.observers,
                                fut,
                                fan,
                            )
                            .await
                            {
                                Err(e) => Err(e),
                                Ok(()) => {
                                    // Park: record the `Suspended` row and return the state.
                                    // The log and the compensation stack are kept (its
                                    // entries are already persisted and are rehydrated on
                                    // resume).
                                    let appended = match workflow_id.as_deref() {
                                        Some(wf_id) => {
                                            let row = CheckpointRow::new(
                                                sequence,
                                                state_label.as_deref().unwrap_or_default(),
                                                &**name,
                                            )
                                            .suspended()
                                            .with_workflow_version(self.workflow_version);
                                            self.append_row(
                                                wf_id,
                                                &mut sequence,
                                                row,
                                                "suspend checkpoint",
                                            )
                                            .await
                                        }
                                        None => Err(CanoError::checkpoint_store(
                                            "checkpointing requires a workflow id (call with_workflow_id)",
                                        )),
                                    };
                                    match appended {
                                        Ok(()) => {
                                            #[cfg(feature = "metrics")]
                                            crate::metrics::task_dispatch_duration(
                                                _state_label,
                                                "suspend",
                                                _dispatch_started.elapsed(),
                                            );
                                            #[cfg(feature = "tracing")]
                                            info!(state = ?current_state, "Workflow suspended");
                                            return Ok(current_state);
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                            }
                        }
                    }
                }
            };

            // Compensatable records its own duration earlier (before the
            // completion-row append) so it is excluded here to avoid double-counting.
            #[cfg(feature = "metrics")]
            if let Some(kind) = match entry {
                Entry::Single {
                    checkpoint: true, ..
                } => Some("single"),
                Entry::Single {
                    checkpoint: false, ..
                } => Some("router"),
                Entry::Split { .. } => Some("split"),
                Entry::Compensatable { .. } => None,
                Entry::Stepped { .. } => Some("stepped"),
                Entry::Stream { .. } => Some("stream"),
                Entry::Suspend { .. } => Some("suspend"),
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
                            transitions_so_far.make_contiguous(),
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
    /// `fut.await` is the zero-cost path when `step_budget` and `token` are both
    /// `None`; no observer plumbing runs in that case.
    async fn dispatch_with_budget<T, F>(
        step_budget: Option<(std::time::Instant, std::time::Duration, crate::rt::Instant)>,
        token: Option<&CancellationToken>,
        state_label: Option<&str>,
        observers: &[Arc<dyn crate::observer::WorkflowObserver>],
        fut: F,
        task_failure_fan_out: impl Fn(&dyn crate::observer::WorkflowObserver, &CanoError),
    ) -> Result<T, CanoError>
    where
        F: std::future::Future<Output = Result<T, CanoError>>,
    {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Zero-cost path: neither budget nor token is active.
        if step_budget.is_none() && token.is_none() {
            return fut.await;
        }

        let fan = &task_failure_fan_out;
        let polled = AtomicBool::new(false);
        let polled_ref = &polled;
        let mut pinned_fut = std::pin::pin!(fut);
        let tracked_fut = std::future::poll_fn(move |cx| {
            polled_ref.store(true, Ordering::Relaxed);
            pinned_fut.as_mut().poll(cx)
        });

        let budgeted = async move {
            let Some((start, limit, deadline)) = step_budget else {
                return tracked_fut.await;
            };
            match crate::rt::timeout_at(deadline, tracked_fut).await {
                Ok(inner) => inner,
                Err(_) => {
                    let elapsed = start.elapsed();
                    let err = CanoError::workflow_timeout(elapsed, limit);
                    let did_poll = polled_ref.load(Ordering::Relaxed);
                    notify_observers(observers, |o| {
                        if did_poll {
                            fan(o, &err);
                        }
                        o.on_workflow_timeout(elapsed, limit);
                    });
                    Err(err)
                }
            }
        };

        let Some(token) = token else {
            return budgeted.await;
        };

        // Race cancellation against the budgeted dispatch. `biased` checks the
        // cancel arm first so cancellation deterministically wins a tie against
        // the per-state timeout. Only emit the per-task failure fan-out if `fut`
        // was actually polled (and therefore fired `on_task_start`), keeping
        // observer gauges balanced even when `token` was cancelled during the
        // preceding `append_row` before `fut` was ever polled.
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                let err = CanoError::cancelled();
                if polled.load(Ordering::Relaxed) {
                    notify_observers(observers, |o| fan(o, &err));
                }
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
                let comp_deadline = super::deadline_after(comp_budget);
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
    /// `RetryExhausted` carries the structural count. `CircuitOpen`,
    /// `CircuitHalfOpenBusy`, and `WorkflowTimeout` return 0 because they
    /// short-circuit (or cancel) the in-flight future *before* an attempt body
    /// runs to completion — surfacing
    /// `attempt = 1` would be indistinguishable from "completed one full
    /// attempt then failed", which misleads alerting that buckets on the
    /// attempt count. `WithStateContext` is unwrapped so a nested cause still
    /// surfaces the truthful count rather than always 1. Everything else means
    /// we stopped on the first attempt.
    pub(super) fn attempts_from_error(err: &CanoError) -> u32 {
        match err {
            CanoError::RetryExhausted { attempts, .. } => *attempts,
            CanoError::CircuitOpen(_)
            | CanoError::CircuitHalfOpenBusy(_)
            | CanoError::WorkflowTimeout { .. }
            | CanoError::Cancelled => 0,
            CanoError::WithStateContext { source, .. } => Self::attempts_from_error(source),
            _ => 1,
        }
    }

    async fn execute_single_task(
        &self,
        task: &dyn Task<TState>,
        config: &TaskConfig,
        task_name: &str,
    ) -> Result<TState, CanoError> {
        let obs = ObserverCtx::new(&self.observers, task_name);
        notify_observers(&self.observers, |o| o.on_task_start(task_name));

        #[cfg(feature = "tracing")]
        let task_span = if tracing::enabled!(tracing::Level::INFO) {
            info_span!("single_task_execution")
        } else {
            tracing::Span::none()
        };

        // `catch_panic_to_error` turns a panic inside the task body into a
        // `CanoError::TaskExecution` instead of unwinding through the workflow loop.
        // Split branches apply the same pattern inside each spawned task so their
        // index and panic payload are preserved.
        let resources = &*self.resources;
        let run_future = run_with_retries_observed(config, obs.as_ref(), || task.run(resources));
        let run = super::catch_panic_to_error(run_future, "Single task");
        // Attach with `Instrument`, never `Span::enter()`: a guard held across `.await` stays
        // entered on this thread while other tasks run here (or exits on the wrong thread after
        // a migration), corrupting every trace on the runtime.
        #[cfg(feature = "tracing")]
        let run = run.instrument(task_span);
        let outcome: Result<TState, CanoError> = run.await;
        #[cfg(feature = "tracing")]
        if let Ok(next_state) = &outcome {
            debug!(next_state = ?next_state, "Single task completed");
        }

        match &outcome {
            Ok(_) => notify_observers(&self.observers, |o| o.on_task_success(task_name)),
            Err(e) => notify_observers(&self.observers, |o| o.on_task_failure(task_name, e)),
        }
        outcome
    }

    /// Append `row` to the checkpoint log, then fire `on_checkpoint` and advance
    /// `sequence`. `what` names the row in the error (`"append {what}: {e}"`).
    /// A no-op without a checkpoint store.
    pub(super) async fn append_row(
        &self,
        workflow_id: &str,
        sequence: &mut u64,
        row: CheckpointRow,
        what: &str,
    ) -> Result<(), CanoError> {
        let Some(store) = &self.checkpoint_store else {
            return Ok(());
        };
        let result = store.append(workflow_id, row).await;
        #[cfg(feature = "metrics")]
        crate::metrics::checkpoint_append(result.is_ok());
        if let Err(e) = result {
            return Err(CanoError::checkpoint_store(format!("append {what}: {e}")));
        }
        notify_observers(&self.observers, |o| o.on_checkpoint(workflow_id, *sequence));
        *sequence = sequence
            .checked_add(1)
            .ok_or_else(|| CanoError::checkpoint_store("checkpoint sequence overflow"))?;
        Ok(())
    }

    /// A cursor sink writing [`RowKind::StepCursor`] rows for this state, or `None`
    /// when nothing is persisted (no checkpoint store or no workflow id).
    fn cursor_sink<'a>(
        &'a self,
        workflow_id: Option<&'a str>,
        state_label: &'a str,
        task_name: &'a str,
        sequence: &'a mut u64,
    ) -> Option<CheckpointSink<'a, TState>> {
        let workflow_id = workflow_id.filter(|_| self.checkpoint_store.is_some())?;
        Some(CheckpointSink {
            workflow: self,
            workflow_id,
            state_label,
            task_name,
            sequence,
        })
    }

    /// Drive a `Stepped` state's step loop (each step under the task's retry
    /// policy), persisting every `StepOutcome::More` cursor as a
    /// [`RowKind::StepCursor`] row before the next step. `resume_cursor` is the last
    /// persisted cursor when resuming; `sequence` advances once per row.
    #[allow(clippy::too_many_arguments)]
    async fn execute_stepped_task(
        &self,
        task: &dyn ErasedSteppedTask<TState>,
        config: &TaskConfig,
        task_name: &str,
        workflow_id: Option<&str>,
        state_label: &str,
        sequence: &mut u64,
        resume_cursor: Option<Vec<u8>>,
    ) -> Result<TState, CanoError> {
        let obs = ObserverCtx::new(&self.observers, task_name);
        notify_observers(&self.observers, |o| o.on_task_start(task_name));

        let mut sink = self.cursor_sink(workflow_id, state_label, task_name, sequence);
        let run = task.drive(
            &self.resources,
            config,
            obs,
            resume_cursor,
            sink.as_mut().map(|s| s as &mut dyn CursorSink),
        );
        let outcome = super::catch_panic_to_error(run, "Stepped task").await;
        #[cfg(feature = "tracing")]
        if let Ok(next_state) = &outcome {
            debug!(next_state = ?next_state, "Stepped task completed");
        }

        match &outcome {
            Ok(_) => notify_observers(&self.observers, |o| o.on_task_success(task_name)),
            Err(e) => notify_observers(&self.observers, |o| o.on_task_failure(task_name, e)),
        }
        outcome
    }

    /// Run one `suspend` or `resume` call of a `Suspend` state, pairing
    /// `on_task_start` with `on_task_success` / `on_task_failure` and turning a panic
    /// into an error.
    async fn execute_suspend_call<T>(
        &self,
        task_name: &str,
        call: impl Future<Output = Result<T, CanoError>>,
    ) -> Result<T, CanoError> {
        notify_observers(&self.observers, |o| o.on_task_start(task_name));
        let outcome = super::catch_panic_to_error(call, "Suspend task").await;
        match &outcome {
            Ok(_) => notify_observers(&self.observers, |o| o.on_task_success(task_name)),
            Err(e) => notify_observers(&self.observers, |o| o.on_task_failure(task_name, e)),
        }
        outcome
    }

    /// Drive the windowed consume loop for a `Stream` state.
    ///
    /// Opens (or resumes from `resume_cursor`) the source, then advances the session one
    /// window at a time, persisting each flushed window's cursor as a
    /// [`RowKind::StepCursor`] row before continuing (crash-safe ordering, identical to
    /// `execute_stepped_task`). `token` and `step_budget` are folded into one *effective*
    /// token — with a budget, a child of the caller's token that a spawned forwarder also
    /// cancels at the total-timeout deadline, whichever is first — and observed
    /// cooperatively inside the session so either cause flushes the in-flight window, commits
    /// its cursor, runs `on_close` for cleanup, and returns a clean `Err` (resumable) rather
    /// than dropping the future mid-window. The two causes are told apart afterwards by
    /// polling the ORIGINAL `token` (not the child): still cancelled ⇒ surfaces as
    /// `Cancelled`; not ⇒ the deadline must have fired ⇒ reclassified as `WorkflowTimeout`
    /// (mirroring `dispatch_with_budget`'s handling for every other state kind). Opening the
    /// session races the same effective token too, so a hung `open()` is cancellable /
    /// timeoutable — no cursor exists yet at that point, so there is nothing to flush or
    /// commit; the future is simply dropped (cooperative, like every other task kind).
    /// Returns the next `TState` on a terminal window; rejects a `Split` result
    /// (single-next-state only, like `Stepped`).
    #[allow(clippy::too_many_arguments)]
    async fn execute_stream_task(
        &self,
        task: &dyn ErasedStreamTask<TState>,
        config: &TaskConfig,
        task_name: &str,
        workflow_id: Option<&str>,
        state_label: &str,
        sequence: &mut u64,
        resume_cursor: Option<Vec<u8>>,
        token: Option<&CancellationToken>,
        step_budget: Option<(std::time::Instant, std::time::Duration, crate::rt::Instant)>,
    ) -> Result<TState, CanoError> {
        use std::sync::atomic::{AtomicBool, Ordering};

        // Aborts the spawned deadline-forwarder task when dropped, so a run that ends
        // normally (or via a real cancel, well before any deadline) doesn't leave that task
        // sleeping until the total-timeout deadline for no reason.
        #[cfg(feature = "runtime-tokio")]
        struct AbortOnDrop(tokio::task::JoinHandle<()>);
        #[cfg(feature = "runtime-tokio")]
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }

        notify_observers(&self.observers, |o| o.on_task_start(task_name));
        // Cursor rows are written (and cursors encoded at all) only when they can be
        // persisted: a checkpoint store and a workflow id.
        let persist = self.checkpoint_store.is_some() && workflow_id.is_some();

        // Fold `token` and `step_budget` into one effective token so both a real cancel and
        // a total-timeout trip drive the SAME cooperative drain (flush the in-flight window,
        // commit its cursor, run `on_close`) instead of a `timeout_at` wrapper that would
        // drop the future mid-window — the exact hazard this arm bypasses
        // `dispatch_with_budget` to avoid. Without a budget (the common case) the caller's
        // token is used as is and no task/timer is attached. With one, a child token
        // inherits the caller's cancellation and fires at the deadline.
        #[cfg(feature = "runtime-tokio")]
        let deadline_child: Option<(CancellationToken, AbortOnDrop)> =
            step_budget.map(|(_, _, deadline)| {
                let child =
                    token.map_or_else(CancellationToken::new, CancellationToken::child_token);
                let fire = child.clone();
                let join = tokio::spawn(async move {
                    crate::rt::sleep_until(deadline).await;
                    fire.cancel();
                });
                (child, AbortOnDrop(join))
            });
        #[cfg(not(feature = "runtime-tokio"))]
        let (deadline_token, mut deadline_sleep): (
            Option<CancellationToken>,
            Option<crate::rt::SleepBox>,
        ) = match step_budget {
            Some((_, _, deadline)) => (
                Some(token.map_or_else(CancellationToken::new, CancellationToken::child_token)),
                Some(crate::rt::sleep_until_boxed(deadline)),
            ),
            None => (None, None),
        };

        #[cfg(feature = "runtime-tokio")]
        let effective_token: Option<&CancellationToken> = match &deadline_child {
            Some((child, _)) => Some(child),
            None => token,
        };
        #[cfg(not(feature = "runtime-tokio"))]
        let effective_token: Option<&CancellationToken> = deadline_token.as_ref().or(token);

        // Set when the engine's own cooperative-drain path ends the run (the
        // `ErasedWindowStep::Cancelled` arm below, or the cancel-during-`open_session` race
        // just below) — never by a `StreamTask` organically returning `Err(Cancelled)` from
        // `process_item`/`flush_window`/`on_close`, which must NOT flip metrics/observers
        // onto the cancelled path. `AtomicBool` (not `Cell`) so the spawned workflow future
        // stays `Send`.
        let was_cancelled = AtomicBool::new(false);
        let stream_body = async {
            let open = task.open_session(&self.resources, resume_cursor, config.attempt_timeout);
            let mut session = match effective_token {
                Some(effective) => tokio::select! {
                    biased;
                    _ = effective.cancelled() => {
                        was_cancelled.store(true, Ordering::Relaxed);
                        return Err(CanoError::cancelled());
                    }
                    res = open => res?,
                },
                None => open.await?,
            };
            loop {
                // Catch a panic inside `process_item`/`flush_window`/`on_close` so it
                // becomes a `CanoError` instead of unwinding past the FSM and skipping
                // resource teardown — same discipline as the other drivers.
                let step = super::catch_panic_to_error(
                    session.next_window(&self.resources, effective_token, persist),
                    "Stream task",
                )
                .await?;

                // `terminal == Some(_)` ends the loop with that result after the cursor is
                // persisted. A cancelled stream commits its final cursor, then surfaces
                // `Cancelled` (so the log is NOT cleared and `resume_from` continues).
                let (cursor_to_commit, terminal) = match step {
                    ErasedWindowStep::Window { cursor } => (cursor, None),
                    ErasedWindowStep::Done {
                        final_cursor,
                        result,
                    } => (final_cursor, Some(Ok(result))),
                    ErasedWindowStep::Cancelled { final_cursor } => {
                        was_cancelled.store(true, Ordering::Relaxed);
                        (final_cursor, Some(Err(CanoError::cancelled())))
                    }
                };

                // Persist the window cursor before advancing so a crash/cancel resumes
                // from this exact position. Cursors are only encoded when `persist`.
                if let Some(bytes) = cursor_to_commit
                    && let Some(wf_id) = workflow_id
                {
                    let row = CheckpointRow::new(*sequence, state_label, task_name)
                        .with_cursor(bytes)
                        .with_workflow_version(self.workflow_version);
                    self.append_row(wf_id, sequence, row, "stream cursor checkpoint")
                        .await?;
                }

                if let Some(result) = terminal {
                    return result;
                }
            }
        };
        #[cfg(feature = "runtime-tokio")]
        let outcome: Result<TState, CanoError> = stream_body.await;
        #[cfg(not(feature = "runtime-tokio"))]
        let outcome: Result<TState, CanoError> = {
            let mut body = std::pin::pin!(stream_body);
            std::future::poll_fn(|cx| {
                if let Some(sleep) = deadline_sleep.as_mut()
                    && sleep.as_mut().poll(cx).is_ready()
                {
                    if let Some(fire) = deadline_token.as_ref() {
                        fire.cancel();
                    }
                    deadline_sleep = None;
                }
                body.as_mut().poll(cx)
            })
            .await
        };

        // The engine's drain path fired (`was_cancelled`) AND the outcome it produced is
        // still `Cancelled` (not overwritten by e.g. a later checkpoint-append failure,
        // which must report as a failure, not a cancel).
        let drained_as_cancelled =
            was_cancelled.load(Ordering::Relaxed) && matches!(outcome, Err(CanoError::Cancelled));

        // The drain fired but the caller's ORIGINAL token was never cancelled — so the
        // *deadline* must be what fired the effective token. Reclassify, mirroring
        // `dispatch_with_budget`'s `WorkflowTimeout` handling for every other state kind.
        let (outcome, timed_out) = if drained_as_cancelled
            && let Some((start, limit, _)) = step_budget
            && !token.is_some_and(CancellationToken::is_cancelled)
        {
            let elapsed = start.elapsed();
            (
                Err(CanoError::workflow_timeout(elapsed, limit)),
                Some((elapsed, limit)),
            )
        } else {
            (outcome, None)
        };
        let cancelled = drained_as_cancelled && timed_out.is_none();

        #[cfg(feature = "metrics")]
        crate::metrics::stream_run(if outcome.is_ok() {
            "completed"
        } else if cancelled {
            "cancelled"
        } else {
            "failed"
        });

        match &outcome {
            Ok(_) => notify_observers(&self.observers, |o| o.on_task_success(task_name)),
            Err(e) => {
                notify_observers(&self.observers, |o| o.on_task_failure(task_name, e));
                if let Some((elapsed, limit)) = timed_out {
                    // Mirrors `dispatch_with_budget`: a timeout fires
                    // `on_workflow_timeout`, never `on_cancelled`.
                    notify_observers(&self.observers, |o| o.on_workflow_timeout(elapsed, limit));
                } else if cancelled {
                    // The stream arm bypasses `dispatch_with_budget`, so it fires
                    // `on_cancelled` itself — exactly once per cancelled run, mirroring
                    // the fan-out + `on_cancelled` pairing dispatch does for other task
                    // types.
                    notify_observers(&self.observers, |o| o.on_cancelled(state_label));
                }
            }
        }
        outcome
    }

    async fn execute_split_join(
        &self,
        tasks: &[Arc<dyn Task<TState>>],
        configs: &[Arc<TaskConfig>],
        names: &[Cow<'static, str>],
        join_config: &JoinConfig<TState>,
    ) -> Result<TState, CanoError> {
        let resources = Arc::clone(&self.resources);
        let total_tasks = tasks.len();

        #[cfg(feature = "tracing")]
        info!(
            total_tasks = total_tasks,
            strategy = ?join_config.strategy,
            "Starting split execution"
        );

        // Optional bulkhead: gate task bodies on a shared semaphore so at
        // most `n` split tasks execute concurrently.
        let bulkhead = join_config
            .bulkhead
            .map(|n| Arc::new(tokio::sync::Semaphore::new(n)));

        let mut join_set: crate::rt::JoinSet<(usize, Result<TState, CanoError>)> =
            crate::rt::JoinSet::new();

        // Observer wiring. `task_ids[idx]` is each branch's reported id
        // (`"{name}[{idx}]"`); spawned branches report retries through a shared copy
        // of the observer list, and `on_task_success` / `on_task_failure` fire from
        // here once results are collected. Both stay empty / `None` and cost nothing
        // when no observers are attached.
        let observers = (!self.observers.is_empty()).then(|| Arc::clone(&self.observers));
        let task_ids: Vec<Arc<str>> = if observers.is_some() {
            names
                .iter()
                .enumerate()
                .map(|(idx, name)| Arc::from(format!("{name}[{idx}]")))
                .collect()
        } else {
            Vec::new()
        };

        #[cfg_attr(not(feature = "tracing"), allow(unused_variables))]
        for (idx, task) in tasks.iter().enumerate() {
            let task_id = task_ids.get(idx).cloned();
            if let Some(id) = &task_id {
                notify_observers(&self.observers, |o| o.on_task_start(id));
            }
            let task = Arc::clone(task);
            let config = Arc::clone(&configs[idx]);
            let branch_observers = observers.clone();
            let resources_clone = Arc::clone(&resources);
            let bulkhead_clone = bulkhead.clone();

            #[cfg(feature = "tracing")]
            let task_span = if tracing::enabled!(tracing::Level::INFO) {
                info_span!("split_task", task_id = idx)
            } else {
                tracing::Span::none()
            };

            let branch = async move {
                let run_future = async {
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

                    let obs = branch_observers
                        .as_deref()
                        .zip(task_id.as_deref())
                        .and_then(|(observers, name)| ObserverCtx::new(observers, name));
                    let resources = &*resources_clone;
                    let result =
                        run_with_retries_observed(&config, obs.as_ref(), || task.run(resources))
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
            };
            // Attach with `Instrument`, never `Span::enter()`: a guard held across `.await` stays
            // entered on this thread while other tasks run here (or exits on the wrong thread after
            // a migration), corrupting every trace on the runtime.
            #[cfg(feature = "tracing")]
            let branch = branch.instrument(task_span);
            join_set.spawn(branch);
        }

        // Collect results using the unified strategy handler
        let split_result = self
            .collect_results(join_set, join_config, total_tasks)
            .await?;

        #[cfg(feature = "metrics")]
        crate::metrics::split_branch_results(
            split_result.successes.len(),
            split_result.errors.len(),
            split_result.cancelled,
        );

        // Fire per-task observer events for everything that ran to completion.
        // Cancelled tasks are intentionally silent on early-return success, and an
        // aggregated error entry carries `task_index == usize::MAX`, which
        // `task_ids.get` filters out. When `join_config.timeout` aborted a
        // non-PartialTimeout split, pair every incomplete branch's `on_task_start`
        // with `on_task_failure` so observer gauges remain balanced.
        if let Some(ref slice) = observers {
            for &idx in &split_result.successes {
                if let Some(id) = task_ids.get(idx) {
                    notify_observers(slice, |o| o.on_task_success(id));
                }
            }
            for (idx, err) in &split_result.errors {
                if let Some(id) = task_ids.get(*idx) {
                    notify_observers(slice, |o| o.on_task_failure(id, err));
                }
            }
        }

        if split_result.timed_out {
            let timeout_err = CanoError::workflow("Split task timeout exceeded");
            if let Some(ref slice) = observers {
                for (idx, &done) in split_result.completed_indices.iter().enumerate() {
                    if !done && let Some(id) = task_ids.get(idx) {
                        notify_observers(slice, |o| o.on_task_failure(id, &timeout_err));
                    }
                }
            }
            return Err(timeout_err);
        }

        let successful = split_result.successes.len();
        let _failed = split_result.errors.len();
        let _cancelled = split_result.cancelled;

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
                if successful + split_result.errors.len() >= 1 {
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
        mut join_set: crate::rt::JoinSet<(usize, Result<TState, CanoError>)>,
        join_config: &JoinConfig<TState>,
        total_tasks: usize,
    ) -> Result<SplitOutcome, CanoError> {
        let mut split_result = SplitOutcome {
            successes: Vec::with_capacity(total_tasks),
            errors: Vec::new(),
            cancelled: 0,
            completed_indices: Vec::new(),
            timed_out: false,
        };
        // Bitset over task indices; cheaper than HashSet<usize> on cache and alloc.
        let mut completed_indices: Vec<bool> = vec![false; total_tasks];

        // Determine deadline if timeout is configured
        let deadline = join_config.timeout.map(super::deadline_after);

        loop {
            // Wait for next completion or timeout
            let next_result = if let Some(d) = deadline {
                match crate::rt::timeout_at(d, join_set.join_next()).await {
                    Ok(res) => res,
                    Err(_) => {
                        // Timeout reached: abort remaining branches and return collected
                        // state so `execute_split_join` can record metrics and balance
                        // observer hooks before failing non-PartialTimeout strategies.
                        join_set.abort_all();
                        if !matches!(join_config.strategy, JoinStrategy::PartialTimeout) {
                            split_result.timed_out = true;
                        }
                        break;
                    }
                }
            } else {
                join_set.join_next().await
            };

            match next_result {
                Some(Ok((index, Ok(_)))) => {
                    completed_indices[index] = true;
                    split_result.successes.push(index);
                }
                Some(Ok((index, Err(e)))) => {
                    completed_indices[index] = true;
                    split_result.errors.push((index, e));
                }
                Some(Err(join_err)) => {
                    // Task panicked or was aborted; index correlation is lost.
                    // Record as anonymous error so caller still sees the failure count.
                    split_result.errors.push((
                        usize::MAX,
                        CanoError::workflow(format!("Task panic: {:?}", join_err)),
                    ));
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
        split_result.cancelled = completed_indices.iter().filter(|done| !**done).count();
        split_result.completed_indices = completed_indices;

        Ok(split_result)
    }
}

#[cfg(all(test, feature = "runtime-tokio"))]
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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

        let result = workflow.orchestrate(TestState::Start).await;
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_split_with_timeout() {
        // Task that sleeps longer than timeout
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TestState::Complete)
            }
        }

        let tasks = vec![SlowTask, SlowTask];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete)
            .with_timeout(Duration::from_millis(50));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_split_with_data_sharing() {
        let board = crate::blackboard::Blackboard::new();
        let resources = Resources::new().insert(board.clone());

        let tasks = vec![
            DataTask::new("task1", "value1", TestState::Join),
            DataTask::new("task2", "value2", TestState::Join),
            DataTask::new("task3", "value3", TestState::Join),
        ];

        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);

        let workflow = Workflow::new(resources)
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify all tasks wrote their data
        let data1: String = board.get("task1").unwrap();
        let data2: String = board.get("task2").unwrap();
        let data3: String = board.get("task3").unwrap();

        assert_eq!(data1, "value1");
        assert_eq!(data2, "value2");
        assert_eq!(data3, "value3");
    }

    #[tokio::test]
    async fn test_complex_workflow_with_split_join() {
        let board = crate::blackboard::Blackboard::new();
        let resources = Resources::new().insert(board.clone());

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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);

        // Verify all data was written
        let init: String = board.get("init").unwrap();
        let parallel1: String = board.get("parallel1").unwrap();
        let parallel2: String = board.get("parallel2").unwrap();

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
            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TestState::Complete)
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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
                crate::task::TaskConfig::new()
            }

            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                if self.should_fail {
                    Err(CanoError::task_execution("Task failed"))
                } else {
                    Ok(TestState::Complete)
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_results_minimum_not_met() {
        // All tasks will timeout before minimum is reached
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(TestState::Complete)
            }
        }

        let tasks = vec![SlowTask, SlowTask, SlowTask];

        // Require 3 tasks but timeout after 100ms
        let join_config = JoinConfig::new(JoinStrategy::PartialResults(3), TestState::Complete)
            .with_timeout(Duration::from_millis(100));

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
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
            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TestState::Complete)
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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
                crate::task::TaskConfig::new()
            }

            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                if self.should_fail {
                    Err(CanoError::task_execution("Task failed"))
                } else {
                    Ok(TestState::Complete)
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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
            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                Ok(TestState::Complete)
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_partial_timeout_no_timeout_configured() {
        #[derive(Clone)]
        struct SimpleTaskLocal;

        #[task]
        impl Task<TestState> for SimpleTaskLocal {
            async fn run(&self) -> Result<TestState, CanoError> {
                Ok(TestState::Complete)
            }
        }

        let tasks = vec![SimpleTaskLocal, SimpleTaskLocal];

        // PartialTimeout without timeout should fail
        let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, TestState::Complete);

        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
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
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
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
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );

        let tasks_fail = vec![FailTask::new(false), FailTask::new(true)];
        let join_config2 = JoinConfig::new(JoinStrategy::Percentage(1.0), TestState::Complete);
        let workflow2 = Workflow::bare()
            .register_split(TestState::Start, tasks_fail, join_config2)
            .add_exit_state(TestState::Complete);
        assert!(workflow2.orchestrate(TestState::Start).await.is_err());
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
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
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
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_single_task_register_split() {
        let tasks = vec![SimpleTask::new(TestState::Complete)];
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete);
        let result = workflow.orchestrate(TestState::Start).await.unwrap();
        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_workflow_no_exit_states() {
        // No exit states means validate() rejects the workflow before any task runs.
        let workflow =
            Workflow::bare().register(TestState::Start, SimpleTask::new(TestState::Complete));
        let result = workflow.orchestrate(TestState::Start).await;
        let err = result.unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(err.to_string().contains("no exit states"));
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

            async fn run(&self) -> Result<TestState, CanoError> {
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

        let result = workflow.orchestrate(TestState::Start).await;
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

            async fn run(&self) -> Result<TestState, CanoError> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
                if count >= self.succeed_after {
                    Ok(TestState::Complete)
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

        let result = workflow.orchestrate(TestState::Start).await;
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
            crate::task::TaskConfig::new()
        }

        async fn run(&self) -> Result<TestState, CanoError> {
            panic!("boom");
        }
    }

    #[tokio::test]
    async fn test_single_task_panic_caught() {
        let workflow = Workflow::bare()
            .register(TestState::Start, PanickingTask)
            .add_exit_state(TestState::Complete);

        let err = workflow
            .orchestrate(TestState::Start)
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
            .orchestrate(TestState::Start)
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
            crate::task::TaskConfig::new()
        }

        async fn run(&self) -> Result<TestState, CanoError> {
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
            Ok(TestState::Complete)
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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
            .orchestrate(TestState::Start)
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

            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TestState::Complete)
            }
        }

        let err = Workflow::bare()
            .register(TestState::Start, SlowTask)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
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
        // N parallel split tasks share one CircuitBreaker. They all fail
        // in the same workflow run; the breaker — protected by a std Mutex —
        // must record every failure correctly across the JoinSet workers and
        // end up Open.
        use crate::circuit::{CircuitBreaker, CircuitPolicy, CircuitState};

        struct FailingTask {
            breaker: CircuitBreaker,
        }

        #[task]
        impl Task<TestState> for FailingTask {
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::new().with_circuit_breaker(self.breaker.clone())
            }

            async fn run(&self) -> Result<TestState, CanoError> {
                Err(CanoError::task_execution("always fails"))
            }
        }

        let breaker = CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 4,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        });

        let tasks: Vec<FailingTask> = (0..4)
            .map(|_| FailingTask {
                breaker: breaker.clone(),
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

        let _ = workflow.orchestrate(TestState::Start).await;
        assert!(
            matches!(breaker.state(), CircuitState::Open { .. }),
            "shared breaker must trip after 4 concurrent failures, got {:?}",
            breaker.state()
        );
    }

    // ------------------------------------------------------------------
    // stepped tests — no-store path
    // ------------------------------------------------------------------

    use crate::task::stepped::{StepOutcome, SteppedTask};

    #[tokio::test]
    async fn test_stepped_no_store_runs_to_completion() {
        // A stepped task with no checkpoint store runs to completion and writes no rows.
        struct Counter {
            target: u32,
        }

        #[task_mod::stepped]
        impl SteppedTask<TestState> for Counter {
            type Cursor = u32;
            fn config(&self) -> crate::task::TaskConfig {
                crate::task::TaskConfig::new()
            }
            async fn step(
                &self,
                _res: &Resources,
                cursor: Option<u32>,
            ) -> Result<StepOutcome<u32, TestState>, CanoError> {
                let n = cursor.unwrap_or(0) + 1;
                if n >= self.target {
                    Ok(StepOutcome::Done(TestState::Complete))
                } else {
                    Ok(StepOutcome::More(n))
                }
            }
        }

        let result = Workflow::bare()
            .register(TestState::Start, Counter { target: 5 })
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
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
                task_mod::TaskConfig::new()
            }
            async fn run(&self) -> Result<TestState, CanoError> {
                Err(CanoError::task_execution("boom"))
            }
        }

        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, FailNoRetry)
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
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
            async fn run(&self) -> Result<TestState, CanoError> {
                Err(CanoError::task_execution("nope"))
            }
        }
        let err = Workflow::bare()
            .register(TestState::Start, AlwaysFails)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
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
            async fn route(&self, _res: &Resources) -> Result<TestState, CanoError> {
                Ok(TestState::Process)
            }
        }

        let workflow = Workflow::bare()
            .register(TestState::Start, AlwaysRoute)
            .register(TestState::Process, FailTask::new(true))
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
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
            crate::task::TaskConfig::new()
        }
        async fn run(&self) -> Result<TestState, CanoError> {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            Ok(self.next.clone())
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
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
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
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
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
        let _ = workflow.orchestrate(TestState::Start).await;
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
                crate::task::TaskConfig::new()
            }
            fn name(&self) -> std::borrow::Cow<'static, str> {
                "A".into()
            }
            async fn run(
                &self,
                _res: &crate::resource::Resources,
            ) -> Result<(TestState, u32), CanoError> {
                Ok((TestState::Process, 7))
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
            .register(TestState::Start, A { log: log.clone() })
            .register(
                TestState::Process,
                SleepyTask {
                    sleep_ms: 500,
                    next: TestState::Complete,
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
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
                crate::task::TaskConfig::new()
            }
            fn name(&self) -> std::borrow::Cow<'static, str> {
                "SlowComp".into()
            }
            async fn run(
                &self,
                _res: &crate::resource::Resources,
            ) -> Result<(TestState, u32), CanoError> {
                tokio::time::sleep(Duration::from_millis(80)).await;
                self.log.lock().unwrap().push("ran".to_string());
                Ok((TestState::Complete, 42))
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
            .register(TestState::Start, SlowComp { log: log.clone() })
            .add_exit_state(TestState::Complete);

        // The compensatable task runs ~80ms despite the 20ms total budget — and the
        // workflow exits cleanly (no compensation runs, no error surfaces) because
        // there's no further state for the budget to cancel.
        let outcome = workflow.orchestrate(TestState::Start).await.unwrap();
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
#[cfg(all(test, feature = "runtime-tokio"))]
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
#[cfg(all(test, feature = "runtime-tokio"))]
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
        let token = CancellationToken::new();
        let result = Workflow::<TestState>::dispatch_with_budget(
            None,
            Some(&token),
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
        let token = CancellationToken::new();
        let err = Workflow::<TestState>::dispatch_with_budget(
            Some((
                std::time::Instant::now() - Duration::from_secs(1),
                Duration::from_millis(5),
                tokio::time::Instant::now() - Duration::from_millis(1),
            )),
            Some(&token),
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
    async fn precancelled_token_returns_cancelled_without_spurious_failure_when_unpolled() {
        // When `token` is already cancelled before `dispatch_with_budget` polls `fut`
        // (e.g. cancelled during `append_row`), `fut` never ran `on_task_start`, so
        // `task_failure_fan_out` must NOT fire (keeping observer gauges balanced),
        // while `on_cancelled(state_label)` still fires once.
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];
        let token = CancellationToken::new();
        token.cancel();
        let err = Workflow::<TestState>::dispatch_with_budget(
            None,
            Some(&token),
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
            !labels.iter().any(|l| l == "task_failure:synthetic"),
            "fan-out must NOT fire when fut was never polled: {labels:?}"
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
    async fn mid_flight_cancelled_token_fires_failure_fanout_and_on_cancelled() {
        let (observer, rec) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(observer);
        let observers = [observer_dyn];
        let token = CancellationToken::new();
        let handle = token.clone();
        let err = Workflow::<TestState>::dispatch_with_budget(
            None,
            Some(&token),
            Some("StateX"),
            &observers,
            async {
                handle.cancel();
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<TestState, CanoError>(TestState::Complete)
            },
            |o, err| o.on_task_failure("synthetic", err),
        )
        .await
        .expect_err("mid-flight cancelled token returns Cancelled");
        assert!(matches!(err, CanoError::Cancelled), "got: {err}");
        let labels = rec.labels();
        assert!(
            labels.iter().any(|l| l == "task_failure:synthetic"),
            "fan-out (gauge balance) fired after fut was polled: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "cancelled:StateX"),
            "on_cancelled fired with the state label: {labels:?}"
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
        let token = CancellationToken::new();
        let handle = token.clone();
        handle.cancel();
        let err = Workflow::<TestState>::dispatch_with_budget(
            None,
            Some(&token),
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

#[cfg(all(test, feature = "runtime-tokio"))]
mod register_paths_tests {
    use crate::checkpoint::RowKind;
    use crate::error::CanoError;
    use crate::resource::Resources;
    use crate::task::poll::PollOutcome;
    use crate::task::stepped::StepOutcome;
    use crate::task::stream::{CloseReason, WindowSignal};
    use crate::task::{self, TaskConfig};
    use crate::workflow::Workflow;
    use crate::workflow::test_support::{InMemoryCheckpointStore, audit_rows};
    use futures_util::stream::{self, Stream};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        Route,
        Poll,
        Step,
        Stream,
        Saga,
        Fail,
        Done,
    }

    struct Route;
    #[task::router(state = S)]
    impl Route {
        async fn route(&self) -> Result<S, CanoError> {
            Ok(S::Poll)
        }
    }

    struct Poll;
    #[task::poll(state = S)]
    impl Poll {
        async fn poll(&self) -> Result<PollOutcome<S>, CanoError> {
            Ok(PollOutcome::Ready(S::Step))
        }
    }

    struct Stepper;
    #[task::stepped(state = S)]
    impl Stepper {
        async fn step(&self, cursor: Option<u32>) -> Result<StepOutcome<u32, S>, CanoError> {
            Ok(match cursor {
                None => StepOutcome::More(1),
                Some(1) => StepOutcome::More(2),
                Some(_) => StepOutcome::Done(S::Stream),
            })
        }
    }

    struct Feed;
    #[task::stream(state = S)]
    impl Feed {
        async fn open(
            &self,
            _cursor: Option<u32>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(vec![1u32, 2])) as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }
        async fn process_item(&self, item: u32) -> Result<(u32, u32), CanoError> {
            Ok((item, item))
        }
        async fn flush_window(&self, _outputs: Vec<u32>) -> Result<WindowSignal<S>, CanoError> {
            Ok(WindowSignal::Continue)
        }
        async fn on_close(&self, _reason: CloseReason) -> Result<S, CanoError> {
            Ok(S::Saga)
        }
    }

    struct Saga(Arc<Mutex<Vec<u32>>>);
    #[crate::saga::task(state = S)]
    impl Saga {
        type Output = u32;
        async fn run(&self) -> Result<(S, u32), CanoError> {
            Ok((S::Fail, 7))
        }
        async fn compensate(&self, output: u32) -> Result<(), CanoError> {
            self.0.lock().unwrap().push(output);
            Ok(())
        }
    }

    struct Fail;
    #[cano_macros::task]
    impl crate::task::Task<S> for Fail {
        async fn run(&self) -> Result<S, CanoError> {
            Err(CanoError::task_execution("boom"))
        }
    }

    /// Every model goes through the one `register`, and each lands on its engine
    /// path: routers write no row, stepped and stream tasks write cursor rows, and the
    /// compensatable task's output is rolled back when a later state fails.
    #[tokio::test]
    async fn register_selects_the_engine_path_per_model() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let compensated = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .with_checkpoint_store(store.clone())
            .with_workflow_id("models")
            .register(S::Route, Route)
            .register(S::Poll, Poll)
            .register(S::Step, Stepper)
            .register(S::Stream, Feed)
            .register(S::Saga, Saga(Arc::clone(&compensated)))
            .register(S::Fail, Fail)
            .add_exit_state(S::Done);

        let err = workflow.orchestrate(S::Route).await.unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
        assert_eq!(*compensated.lock().unwrap(), vec![7]);

        let rows = audit_rows(&store, "models");
        let cursor_rows = |state: &str| {
            rows.iter()
                .filter(|r| r.state == state && r.kind == RowKind::StepCursor)
                .count()
        };
        assert!(rows.iter().all(|r| r.state != "Route"), "{rows:?}");
        assert_eq!(cursor_rows("Step"), 2, "{rows:?}");
        assert!(cursor_rows("Stream") >= 1, "{rows:?}");
    }

    struct FlakyStepper {
        failed_once: AtomicU32,
        seen_none: Arc<AtomicU32>,
    }
    #[task::stepped(state = S)]
    impl FlakyStepper {
        fn config(&self) -> TaskConfig {
            TaskConfig::new().with_fixed_retry(1, Duration::ZERO)
        }
        async fn step(&self, cursor: Option<u32>) -> Result<StepOutcome<u32, S>, CanoError> {
            match cursor {
                None => {
                    self.seen_none.fetch_add(1, Ordering::SeqCst);
                    Ok(StepOutcome::More(1))
                }
                Some(1) => Ok(StepOutcome::More(2)),
                Some(2) if self.failed_once.fetch_add(1, Ordering::SeqCst) == 0 => {
                    Err(CanoError::task_execution("transient"))
                }
                Some(_) => Ok(StepOutcome::Done(S::Done)),
            }
        }
    }

    /// A retried step re-runs from its own cursor, not from the start of the loop.
    #[tokio::test]
    async fn stepped_retry_repeats_only_the_failed_step() {
        let seen_none = Arc::new(AtomicU32::new(0));
        let task = FlakyStepper {
            failed_once: AtomicU32::new(0),
            seen_none: Arc::clone(&seen_none),
        };
        let workflow = Workflow::new(Resources::new())
            .register(S::Step, task)
            .add_exit_state(S::Done);
        assert_eq!(workflow.orchestrate(S::Step).await.unwrap(), S::Done);
        assert_eq!(seen_none.load(Ordering::SeqCst), 1);
    }
}

#[cfg(all(test, feature = "runtime-tokio"))]
mod engine_contract_tests {
    use crate::error::CanoError;
    use crate::observer::WorkflowObserver;
    use crate::resource::{Resource, Resources};
    use crate::task::{Task, TaskConfig};
    use crate::workflow::Workflow;
    use cano_macros::{resource, task};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        Loop,
        Charge,
        Refund,
        Done,
    }

    struct LoopThenFail(AtomicU32);
    #[task]
    impl Task<S> for LoopThenFail {
        async fn run(&self) -> Result<S, CanoError> {
            if self.0.fetch_add(1, Ordering::SeqCst) < 200 {
                Ok(S::Loop)
            } else {
                Err(CanoError::task_execution("done looping"))
            }
        }
    }

    #[tokio::test]
    async fn transition_path_keeps_only_the_most_recent_states() {
        let workflow = Workflow::bare()
            .register(S::Loop, LoopThenFail(AtomicU32::new(0)))
            .add_exit_state(S::Done);
        match workflow.orchestrate(S::Loop).await.unwrap_err() {
            CanoError::WithStateContext {
                transitions_so_far, ..
            } => assert_eq!(transitions_so_far.len(), super::MAX_TRANSITION_PATH),
            other => panic!("expected WithStateContext, got {other:?}"),
        }
    }

    struct Charge;
    #[crate::saga::task(state = S)]
    impl Charge {
        type Output = ();
        async fn run(&self) -> Result<(S, ()), CanoError> {
            Ok((S::Done, ()))
        }
        async fn compensate(&self, _output: ()) -> Result<(), CanoError> {
            Ok(())
        }
    }

    #[test]
    fn one_compensatable_type_at_two_states_fails_validation() {
        let workflow = Workflow::bare()
            .register(S::Charge, Charge)
            .register(S::Refund, Charge)
            .add_exit_state(S::Done);
        let err = workflow.validate().unwrap_err();
        assert!(matches!(err, CanoError::Configuration(_)), "{err:?}");
        assert!(err.to_string().contains("share the task name"), "{err}");
    }

    struct BadTeardown;
    #[resource]
    impl Resource for BadTeardown {
        async fn teardown(&self) -> Result<(), CanoError> {
            Err(CanoError::generic("teardown failed"))
        }
    }

    struct Outcome(bool);
    #[task]
    impl Task<S> for Outcome {
        async fn run(&self) -> Result<S, CanoError> {
            if self.0 {
                Ok(S::Done)
            } else {
                Err(CanoError::task_execution("run failed"))
            }
        }
    }

    #[tokio::test]
    async fn teardown_error_surfaces_only_after_a_successful_run() {
        for (succeeds, expected) in [(true, "teardown failed"), (false, "run failed")] {
            let workflow = Workflow::new(Resources::new().insert(BadTeardown))
                .register(S::Loop, Outcome(succeeds))
                .add_exit_state(S::Done);
            let err = workflow.orchestrate(S::Loop).await.unwrap_err();
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    struct PanicsOnRetry;
    impl WorkflowObserver for PanicsOnRetry {
        fn on_retry(&self, _task_id: &str, _attempt: u32) {
            panic!("observer bug");
        }
    }

    struct FailsOnce(AtomicU32);
    #[task]
    impl Task<S> for FailsOnce {
        fn config(&self) -> TaskConfig {
            TaskConfig::new().with_fixed_retry(1, Duration::ZERO)
        }
        async fn run(&self) -> Result<S, CanoError> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(CanoError::task_execution("transient"))
            } else {
                Ok(S::Done)
            }
        }
    }

    #[tokio::test]
    async fn observer_panic_in_on_retry_does_not_fail_the_task() {
        let workflow = Workflow::bare()
            .with_observer(Arc::new(PanicsOnRetry))
            .register(S::Loop, FailsOnce(AtomicU32::new(0)))
            .add_exit_state(S::Done);
        assert_eq!(workflow.orchestrate(S::Loop).await.unwrap(), S::Done);
    }

    #[tokio::test]
    async fn split_timeout_balances_observer_start_and_terminal_events() {
        use crate::testing::{RecordedEvent, RecordingObserver};
        use crate::workflow::{JoinConfig, JoinStrategy};

        #[derive(Clone)]
        enum BranchKind {
            Fast,
            Slow,
        }
        #[crate::task(state = S)]
        impl BranchKind {
            async fn run(&self) -> Result<S, CanoError> {
                match self {
                    BranchKind::Fast => Ok(S::Done),
                    BranchKind::Slow => {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        Ok(S::Done)
                    }
                }
            }
        }

        let obs = Arc::new(RecordingObserver::new());
        let wf = Workflow::bare()
            .with_observer(obs.clone())
            .register_split(
                S::Loop,
                vec![BranchKind::Fast, BranchKind::Slow, BranchKind::Slow],
                JoinConfig::new(JoinStrategy::All, S::Done).with_timeout(Duration::from_millis(25)),
            )
            .add_exit_state(S::Done);

        let err = wf.orchestrate(S::Loop).await.unwrap_err();
        assert!(
            err.to_string().contains("Split task timeout exceeded"),
            "{err}"
        );
        let events = obs.events();
        let starts = events
            .iter()
            .filter(|e| matches!(e, RecordedEvent::TaskStarted { .. }))
            .count();
        let terminals = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    RecordedEvent::TaskSucceeded { .. } | RecordedEvent::TaskFailed { .. }
                )
            })
            .count();
        assert_eq!(starts, 3);
        assert_eq!(
            terminals, 3,
            "every split branch that fired on_task_start must fire on_task_success or on_task_failure on timeout: {events:?}"
        );
    }

    #[tokio::test]
    async fn duration_max_compensation_split_and_stream_timeouts_do_not_panic() {
        use crate::workflow::test_support::{CompLog, CompTask, FailTask, SimpleTask, TestState};
        use crate::workflow::{JoinConfig, JoinStrategy};
        use std::sync::Mutex;

        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let wf = Workflow::bare()
            .with_total_timeout(Duration::MAX)
            .with_compensation_timeout(Duration::MAX)
            .register(
                TestState::Start,
                CompTask::ok("c1", 1, TestState::Split, &log),
            )
            .register_split(
                TestState::Split,
                vec![SimpleTask::new(TestState::Process)],
                JoinConfig::new(JoinStrategy::All, TestState::Process).with_timeout(Duration::MAX),
            )
            .register(TestState::Process, FailTask::new(true))
            .add_exit_state(TestState::Complete);

        let err = wf.orchestrate(TestState::Start).await.unwrap_err();
        assert!(err.to_string().contains("intentionally failed"), "{err}");
        assert_eq!(*log.lock().unwrap(), vec![("c1".to_string(), 1)]);
    }

    #[tokio::test]
    async fn observer_panics_in_cancelled_timeout_resume_and_clear_failed_are_isolated() {
        use crate::CancellationToken;
        use crate::checkpoint::{CheckpointRow, CheckpointStore};
        use crate::testing::InMemoryCheckpointStore;
        use crate::workflow::test_support::{SimpleTask, TestState};

        struct PanickyLifecycleObserver;
        impl WorkflowObserver for PanickyLifecycleObserver {
            fn on_cancelled(&self, _state: &str) {
                panic!("panic in on_cancelled");
            }
            fn on_workflow_timeout(&self, _elapsed: Duration, _limit: Duration) {
                panic!("panic in on_workflow_timeout");
            }
            fn on_resume(&self, _workflow_id: &str, _sequence: u64) {
                panic!("panic in on_resume");
            }
            fn on_checkpoint_clear_failed(&self, _workflow_id: &str, _error: &CanoError) {
                panic!("panic in on_checkpoint_clear_failed");
            }
        }

        struct ClearFailStore(InMemoryCheckpointStore);
        #[crate::checkpoint_store]
        impl CheckpointStore for ClearFailStore {
            async fn append(&self, wf_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
                self.0.append(wf_id, row).await
            }
            async fn load_run(&self, wf_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
                self.0.load_run(wf_id).await
            }
            async fn clear(&self, _wf_id: &str) -> Result<(), CanoError> {
                Err(CanoError::checkpoint_store("disk read-only"))
            }
        }

        struct Slow;
        #[crate::task(state = TestState)]
        impl Slow {
            async fn run(&self) -> Result<TestState, CanoError> {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(TestState::Complete)
            }
        }

        let obs: Arc<dyn WorkflowObserver> = Arc::new(PanickyLifecycleObserver);
        let store = Arc::new(ClearFailStore(InMemoryCheckpointStore::new()));

        // 1. `on_checkpoint_clear_failed` + `on_resume` panic isolation
        let wf = Workflow::bare()
            .with_observer(Arc::clone(&obs))
            .with_checkpoint_store(store.clone())
            .with_workflow_id("panic-obs")
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);
        assert_eq!(
            wf.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );
        assert_eq!(
            wf.resume_from("panic-obs").await.unwrap(),
            TestState::Complete
        );

        // 2. `on_cancelled` panic isolation
        let token = CancellationToken::new();
        token.cancel();
        let err = Workflow::bare()
            .with_observer(Arc::clone(&obs))
            .register(TestState::Start, Slow)
            .add_exit_state(TestState::Complete)
            .orchestrate_with_token(TestState::Start, &token)
            .await
            .unwrap_err();
        assert!(err.is_cancelled());

        // 3. `on_workflow_timeout` panic isolation
        let err = Workflow::bare()
            .with_observer(obs)
            .with_total_timeout(Duration::from_millis(15))
            .register(TestState::Start, Slow)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
            .await
            .unwrap_err();
        assert!(matches!(err.inner(), CanoError::WorkflowTimeout { .. }));
    }
}
