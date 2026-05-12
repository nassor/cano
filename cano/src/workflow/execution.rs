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

use crate::error::CanoError;
use crate::recovery::CheckpointRow;
use crate::saga::{CompensationEntry, ErasedCompensatable};
use crate::task::stepped::{ErasedStep, ErasedSteppedTask};
use crate::task::{Task, TaskResult, run_with_retries};

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
    ) -> Result<TState, CanoError> {
        self.execute_workflow_from(
            initial_state,
            0,
            self.workflow_id.clone(),
            Vec::new(),
            HashMap::new(),
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
    pub(super) async fn execute_workflow_from(
        &self,
        initial_state: TState,
        start_sequence: u64,
        workflow_id: Option<String>,
        mut compensation_stack: Vec<CompensationEntry>,
        // Map of `state_label → latest serialized cursor bytes` for `Stepped` states
        // being resumed. Populated by `resume_from`; empty on a fresh run.
        // Entries are consumed via `remove` so visiting the same state twice (on the
        // resumed run continuing past it) starts fresh from `None`.
        mut resume_cursors: HashMap<String, Vec<u8>>,
    ) -> Result<TState, CanoError> {
        let mut current_state = initial_state;
        let mut sequence = start_sequence;

        #[cfg(feature = "tracing")]
        info!(initial_state = ?current_state, "Starting workflow execution");

        loop {
            // The `Debug` label of the state being entered. Needed for observer
            // `on_state_enter` and for checkpoint rows; skipped (no allocation)
            // when neither is in play — the common, zero-overhead case.
            let state_label: Option<String> =
                if !self.observers.is_empty() || self.checkpoint_store.is_some() {
                    Some(format!("{current_state:?}"))
                } else {
                    None
                };

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
                            .run_compensations(workflow_id.as_deref(), compensation_stack, err)
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
                if let Err(e) = store
                    .append(wf_id, CheckpointRow::new(sequence, label, task_id))
                    .await
                {
                    let err = CanoError::checkpoint_store(format!("append checkpoint: {e}"));
                    return self
                        .run_compensations(workflow_id.as_deref(), compensation_stack, err)
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
                        .run_compensations(workflow_id.as_deref(), compensation_stack, err)
                        .await;
                }
            };

            #[cfg(feature = "tracing")]
            debug!(current_state = ?current_state, "Executing state");

            // Dispatch by entry type. Any `Err` triggers a compensation drain.
            let step: Result<TState, CanoError> = match state_entry.as_ref() {
                StateEntry::Single { task, config } => {
                    self.execute_single_task(task.clone(), Arc::clone(config))
                        .await
                }
                StateEntry::Router { task, config } => {
                    // Dispatched like Single; checkpoint write is skipped in the
                    // block above (is_router guard).
                    self.execute_single_task(task.clone(), Arc::clone(config))
                        .await
                }
                StateEntry::Split {
                    tasks,
                    configs,
                    join_config,
                } => {
                    self.execute_split_join(tasks.clone(), Arc::clone(configs), join_config.clone())
                        .await
                }
                StateEntry::CompensatableSingle { task, config } => {
                    match self
                        .execute_compensatable_task(task.clone(), Arc::clone(config))
                        .await
                    {
                        Ok((next_state, output_blob)) => {
                            let task_name = task.name().into_owned();
                            // Persist a completion row carrying the output so a resumed
                            // run rehydrates this compensation entry.
                            if let (Some(store), Some(wf_id)) =
                                (&self.checkpoint_store, workflow_id.as_deref())
                            {
                                let label = state_label.as_deref().unwrap_or_default();
                                let row = CheckpointRow::new(sequence, label, task_name.clone())
                                    .with_output(output_blob.clone());
                                if let Err(e) = store.append(wf_id, row).await {
                                    let err = CanoError::checkpoint_store(format!(
                                        "append compensation checkpoint: {e}"
                                    ));
                                    compensation_stack.push(CompensationEntry {
                                        task_id: task_name,
                                        output_blob,
                                    });
                                    return self
                                        .run_compensations(
                                            workflow_id.as_deref(),
                                            compensation_stack,
                                            err,
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
                    self.execute_stepped_task(
                        task.clone(),
                        Arc::clone(config),
                        &workflow_id,
                        state_label.as_deref().unwrap_or_default(),
                        &mut sequence,
                        resume_cursor,
                    )
                    .await
                }
            };

            current_state = match step {
                Ok(s) => s,
                Err(e) => {
                    return self
                        .run_compensations(workflow_id.as_deref(), compensation_stack, e)
                        .await;
                }
            };
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

        // Wrap the retry-driving future in `catch_unwind` so a panic inside
        // the task body becomes a `CanoError::TaskExecution` instead of
        // unwinding through the workflow loop and aborting the runtime
        // worker. Split tasks apply the same catch-unwind pattern inside each
        // spawned task so their task index and panic payload are preserved.
        let run_future = async {
            run_with_retries(&config, || {
                let task_clone = task.clone();
                let resources_clone = Arc::clone(&self.resources);
                async move { task_clone.run(&*resources_clone).await }
            })
            .await
        };

        #[cfg(feature = "tracing")]
        let unwind_result = {
            let _enter = task_span.enter();
            AssertUnwindSafe(run_future).catch_unwind().await
        };

        #[cfg(not(feature = "tracing"))]
        let unwind_result = AssertUnwindSafe(run_future).catch_unwind().await;

        let result = match unwind_result {
            Ok(inner) => inner,
            Err(payload) => {
                // Forward the inner trait object so `downcast_ref` inspects
                // the actual panic payload type rather than the surrounding
                // `Box<dyn Any>`.
                let payload_str = panic_payload_message(&*payload);
                #[cfg(feature = "tracing")]
                tracing::error!(panic = %payload_str, "Single task panicked");
                Err(CanoError::task_execution(format!("panic: {payload_str}")))
            }
        };

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
        workflow_id: &Option<String>,
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
            let step_result = match AssertUnwindSafe(run_future).catch_unwind().await {
                Ok(inner) => inner,
                Err(payload) => {
                    let payload_str = panic_payload_message(&*payload);
                    #[cfg(feature = "tracing")]
                    tracing::error!(panic = %payload_str, "Stepped task panicked");
                    Err(CanoError::task_execution(format!("panic: {payload_str}")))
                }
            };

            match step_result {
                Err(e) => break Err(e),
                Ok(ErasedStep::Done(TaskResult::Single(next_state))) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(next_state = ?next_state, "Stepped task completed");
                    break Ok(next_state);
                }
                Ok(ErasedStep::Done(TaskResult::Split(_))) => {
                    break Err(CanoError::workflow(
                        "Stepped task returned split result — split is not supported for Stepped states",
                    ));
                }
                Ok(ErasedStep::More(new_cursor_bytes)) => {
                    // Persist the cursor before advancing so a crash between steps
                    // can resume from this exact position.
                    if let (Some(store), Some(wf_id)) =
                        (&self.checkpoint_store, workflow_id.as_deref())
                    {
                        let row = CheckpointRow::new(*sequence, state_label, task_name.as_ref())
                            .with_cursor(new_cursor_bytes.clone());
                        if let Err(e) = store.append(wf_id, row).await {
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

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_workflow_timeout() {
        // Task that sleeps longer than workflow timeout
        #[derive(Clone)]
        struct SlowTask;

        #[task]
        impl Task<TestState> for SlowTask {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let workflow = Workflow::bare()
            .with_timeout(Duration::from_millis(50))
            .register(TestState::Start, SlowTask)
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Workflow timeout"));
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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

        let result = workflow.orchestrate(TestState::Start).await.unwrap();
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
        let result = workflow.orchestrate(TestState::Start).await;
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
            .orchestrate(TestState::Start)
            .await
            .expect_err("panic must surface as Err");
        match err {
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

            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let err = Workflow::bare()
            .register(TestState::Start, SlowTask)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
            .await
            .expect_err("expected attempt timeout to exhaust retries");
        assert!(matches!(err, CanoError::RetryExhausted(_)), "got {err:?}");
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

        let _ = workflow.orchestrate(TestState::Start).await;
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
            .orchestrate(TestState::Start)
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
            .orchestrate(TestState::Start)
            .await
            .expect_err("split result from stepped must error");
        assert!(
            matches!(err, CanoError::Workflow(_)),
            "expected Workflow error, got {err:?}"
        );
        assert!(err.to_string().contains("split"), "got: {err}");
    }
}
