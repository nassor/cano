//! Saga compensation drain and crash-recovery resume.
//!
//! When the forward FSM loop in [`execution`](super::execution) hits a terminal
//! failure it calls [`run_compensations`](Workflow::run_compensations) to undo
//! the work recorded on the compensation stack (LIFO). [`resume_from`](Workflow::resume_from)
//! is the entry point in the other direction: replay a checkpointed run from its
//! last recorded state, rehydrating the compensation stack from the log.

use std::collections::HashMap;
use std::hash::Hash;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;

use crate::cancel::CancellationToken;
use crate::error::CanoError;
use crate::recovery::RowKind;
use crate::saga::{CompensationEntry, ErasedCompensatable};
use crate::task::{TaskResult, run_with_retries};

use super::{Workflow, notify_observers, panic_payload_message};

#[cfg(feature = "tracing")]
use tracing::{debug, info, info_span};

/// Resolve the per-drain compensation deadline.
///
/// Precedence: the user-set [`with_compensation_timeout`](crate::workflow::Workflow::with_compensation_timeout)
/// wins outright. Otherwise the budget is derived as `total_limit / 2`, capped at 30s.
///
/// `total_limit` is the workflow's [`with_total_timeout`](crate::workflow::Workflow::with_total_timeout)
/// (not the remaining-after-trip budget, which is effectively zero at the moment a
/// timeout fires). Scaling against the configured total keeps the compensation budget
/// proportional to the user's intent: `with_total_timeout(100ms)` gets a 50 ms
/// compensation budget by default, not a 30 s grace.
pub(super) fn resolve_compensation_deadline(
    total_limit: std::time::Duration,
    override_: Option<std::time::Duration>,
) -> std::time::Duration {
    if let Some(d) = override_ {
        return d;
    }
    let cap = std::time::Duration::from_secs(30);
    (total_limit / 2).min(cap)
}

/// Single-pass rehydration of a checkpoint log into the four pieces of state
/// `execute_workflow_from` needs to resume: the LIFO compensation stack, the
/// per-state step cursors, the pre-crash transition path, and the cutoff
/// sequence used to avoid double-counting the resume state in the path.
///
/// Replaces five separate filter+collect passes over `rows`; rows are
/// touched once in the main walk and a second time only to apply the
/// `resume_state_entry_cutoff` (which is only known after the walk completes,
/// since the *highest* sequence row for `resume_label` is the cutoff).
///
/// Observer fan-outs and `tracing` warns are emitted **inline** so the
/// behavior is identical to the previous multi-pass code:
///
/// - **F3**: mixed-version rows (older `workflow_version` than expected)
///   emit a `tracing::warn!` per row — the version mismatch on `last` is
///   already validated upstream by the caller, so the walk only warns.
/// - **F4**: when no `StateEntry` row exists for `resume_label`, the cutoff
///   falls back to `resume_sequence + 1` (with a tracing warn).
/// - **F11**: `StateEntry` rows whose label doesn't resolve via
///   `resolve_state` are dropped from `prior_transitions` and reported
///   via `on_unknown_resume_state` on every observer.
pub(super) struct RehydratedRun<TState> {
    pub(super) compensation_stack: Vec<CompensationEntry>,
    pub(super) resume_cursors: HashMap<String, Vec<u8>>,
    pub(super) prior_transitions: Vec<TState>,
    /// Highest-sequence `StateEntry` row whose `state == resume_label`, or
    /// `resume_sequence + 1` when no such row exists (F4 fallback). Exposed
    /// for tests; the caller doesn't need it for `execute_workflow_from`
    /// since it's only used internally to filter `prior_transitions`.
    #[allow(dead_code)]
    pub(super) resume_state_entry_cutoff: u64,
}

impl<TState> RehydratedRun<TState>
where
    TState: Clone,
{
    /// Build a `RehydratedRun` from the ascending-by-sequence `rows` slice.
    ///
    /// `resolve_state` maps a stored state label (the `Debug` rendering of
    /// `TState`) back to a `TState`; returns `None` when the label is no
    /// longer registered on the current workflow (renamed/removed states).
    ///
    /// `observers` and `workflow_id` are threaded through so the F11
    /// `on_unknown_resume_state` hook can fire inline; the caller does
    /// not need a second pass.
    pub(super) fn from_rows(
        rows: &[crate::recovery::CheckpointRow],
        resume_label: &str,
        resume_sequence: u64,
        expected_workflow_version: u32,
        observers: &[Arc<dyn crate::observer::WorkflowObserver>],
        workflow_id: &str,
        mut resolve_state: impl FnMut(&str) -> Option<TState>,
    ) -> Self {
        // Anti-bind unused params on no-tracing builds so the per-row warn
        // arm doesn't fail to compile.
        #[cfg(not(feature = "tracing"))]
        let _ = (expected_workflow_version, workflow_id);

        let mut compensation_stack: Vec<CompensationEntry> = Vec::new();
        let mut resume_cursors: HashMap<String, Vec<u8>> = HashMap::new();
        // Buffer of (sequence, &state_label) for StateEntry rows. We defer
        // the resolve_state call until after the walk so we know the final
        // cutoff and can drop rows whose sequence >= cutoff without a second
        // ascending walk over `rows`.
        let mut state_entries: Vec<(u64, &str)> = Vec::new();
        let mut resume_state_entry_cutoff: Option<u64> = None;

        for r in rows {
            #[cfg(feature = "tracing")]
            if r.workflow_version != expected_workflow_version {
                tracing::warn!(
                    workflow_id = %workflow_id,
                    sequence = r.sequence,
                    stored_version = r.workflow_version,
                    expected_version = expected_workflow_version,
                    "checkpoint row stamps a workflow version older than the current one — keeping the row but flagging the mixed log"
                );
            }
            match r.kind {
                RowKind::StateEntry => {
                    if r.state == resume_label {
                        // `rows` is ascending — overwriting with each match
                        // yields the highest-sequence row, which is the cutoff.
                        resume_state_entry_cutoff = Some(r.sequence);
                    }
                    state_entries.push((r.sequence, r.state.as_str()));
                }
                RowKind::CompensationCompletion => {
                    // Skip the resume point's own row: the resumed state
                    // re-runs and re-pushes its compensation entry, so
                    // keeping the persisted one too would compensate that
                    // step twice on a later failure.
                    if r.sequence != resume_sequence
                        && let Some(blob) = r.output_blob.as_ref()
                    {
                        compensation_stack.push(CompensationEntry {
                            task_id: Arc::from(r.task_id.as_str()),
                            output_blob: blob.clone(),
                        });
                    }
                }
                RowKind::StepCursor => {
                    // Last-write-wins per state label; `rows` ascending order
                    // guarantees the final insert wins for each state.
                    if let Some(blob) = r.output_blob.as_ref() {
                        resume_cursors.insert(r.state.clone(), blob.clone());
                    }
                }
            }
        }

        let cutoff = resume_state_entry_cutoff.unwrap_or_else(|| {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                workflow_id = %workflow_id,
                resume_state = %resume_label,
                "no StateEntry row found for resume state — using last.sequence + 1 as cutoff for the rehydrated path"
            );
            resume_sequence + 1
        });

        let mut prior_transitions: Vec<TState> = Vec::new();
        for (seq, label) in state_entries {
            if seq < cutoff {
                match resolve_state(label) {
                    Some(state) => prior_transitions.push(state),
                    None => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            workflow_id = %workflow_id,
                            sequence = seq,
                            state = %label,
                            "checkpoint row's state label is not registered on the current workflow — dropped from rehydrated path"
                        );
                        super::notify_observers(observers, |o| {
                            o.on_unknown_resume_state(workflow_id, seq, label)
                        });
                    }
                }
            }
        }

        Self {
            compensation_stack,
            resume_cursors,
            prior_transitions,
            resume_state_entry_cutoff: cutoff,
        }
    }
}

impl<TState, TResourceKey> Workflow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Drain the compensation stack (LIFO) after a terminal workflow failure.
    ///
    /// Thin shim over [`run_compensations_bounded`](Self::run_compensations_bounded)
    /// without a wall-clock deadline — each compensator runs until it returns
    /// (bounded only by its own `attempt_timeout`).
    ///
    /// Each entry's [`compensate`](crate::saga::CompensatableTask::compensate) runs once
    /// (bounded by the task's [`attempt_timeout`](crate::task::TaskConfig::attempt_timeout)
    /// if set; a panic inside it is caught) — errors, timeouts and panics are collected and
    /// the drain never stops early. If every compensation succeeds, `original` is returned
    /// unchanged and — when a checkpoint store is attached — its log is cleared. If any
    /// compensation fails (including a stack entry with no matching registered compensator),
    /// the result is [`CanoError::CompensationFailed`] carrying `original` followed by every
    /// compensation error in drain (LIFO) order, and the log is left intact for manual
    /// recovery. An empty stack is a no-op: `original` is returned and nothing is cleared
    /// (the checkpoint log, if any, is kept so the run can still be resumed).
    pub(super) async fn run_compensations(
        &self,
        workflow_id: Option<&str>,
        stack: Vec<CompensationEntry>,
        original: CanoError,
    ) -> Result<TState, CanoError> {
        self.run_compensations_bounded(workflow_id, stack, original, None)
            .await
    }

    /// Like [`run_compensations`](Self::run_compensations) but bounded by an optional
    /// wall-clock deadline.
    ///
    /// When `comp_deadline` is `Some(d)`, each per-entry compensation future is wrapped in
    /// [`tokio::time::timeout_at`]. Once the deadline elapses the drain stops short and
    /// records a single `CanoError::Timeout` naming how many entries were skipped — it
    /// does *not* iterate the remaining stack with a past deadline (which would produce
    /// N noisy "exceeded compensation deadline" errors and drop each compensator's body
    /// after a single poll).
    ///
    /// When `comp_deadline` is `None`, behaviour is identical to the previous
    /// `run_compensations` — only the per-task `attempt_timeout` bounds individual
    /// compensators.
    pub(super) async fn run_compensations_bounded(
        &self,
        workflow_id: Option<&str>,
        mut stack: Vec<CompensationEntry>,
        original: CanoError,
        comp_deadline: Option<tokio::time::Instant>,
    ) -> Result<TState, CanoError> {
        if stack.is_empty() {
            return Err(original);
        }
        let mut errors = vec![original];
        while let Some(entry) = stack.pop() {
            // Once the wall-clock deadline has passed, every remaining entry would only
            // produce another "exceeded compensation deadline" error if we kept iterating.
            // Preserve each remaining entry's `task_id` + `output_blob` as an
            // `OrphanedCompensation` so an operator can decode the persisted output
            // and manually undo the side effect. Surface a single summary timeout
            // error so the overall failure is still clearly attributable to the
            // budget. Without the per-entry preservation the bounded drain
            // would silently drop blobs the new `OrphanedCompensation` variant
            // was specifically introduced to keep.
            if let Some(d) = comp_deadline
                && tokio::time::Instant::now() >= d
            {
                let remaining_count = stack.len() + 1; // +1 for the just-popped entry
                // Push the just-popped entry back so a single drain turns it and
                // every remaining entry — in unchanged LIFO order — into an
                // `OrphanedCompensation`, one `compensation_run(false)` apiece.
                stack.push(entry);
                while let Some(CompensationEntry {
                    task_id,
                    output_blob,
                }) = stack.pop()
                {
                    #[cfg(feature = "metrics")]
                    crate::metrics::compensation_run(false);
                    errors.push(CanoError::orphaned_compensation(task_id, output_blob));
                }
                errors.push(CanoError::timeout(format!(
                    "compensation deadline elapsed; {remaining_count} entries left unprocessed"
                )));
                break;
            }
            // Look up the compensator under a short-lived borrow of `entry.task_id`
            // (the `Arc` clone is cheap and ends the borrow). The None arm can
            // then destructure `entry` and move its blob into the orphan error
            // rather than cloning a potentially-large Vec<u8>.
            let compensator = self.compensators.get(&entry.task_id).cloned();
            match compensator {
                None => {
                    let CompensationEntry {
                        task_id,
                        output_blob,
                    } = entry;
                    errors.push(CanoError::orphaned_compensation(task_id, output_blob));
                }
                Some(compensator) => {
                    #[cfg(feature = "tracing")]
                    debug!(task_id = %entry.task_id, "compensating");
                    // Bound each `compensate` with the task's `attempt_timeout` (if it
                    // configured one) and catch panics: a slow or panicking compensator
                    // becomes one collected error instead of stalling — or unwinding
                    // through — the rest of the drain. (`compensate` is contractually
                    // idempotent, so we don't retry it; surfacing the failure beats an
                    // unbounded rollback retry storm.)
                    let attempt_timeout = compensator.config().attempt_timeout;
                    let compensate_fut =
                        compensator.compensate(&self.resources, &entry.output_blob);
                    let inner = async {
                        match attempt_timeout {
                            Some(d) => tokio::time::timeout(d, compensate_fut)
                                .await
                                .unwrap_or_else(|_| {
                                    Err(CanoError::timeout(format!(
                                        "compensate for {:?} exceeded attempt_timeout {d:?}",
                                        entry.task_id
                                    )))
                                }),
                            None => compensate_fut.await,
                        }
                    };
                    let task_id_for_deadline = entry.task_id.clone();
                    let with_deadline = async move {
                        match comp_deadline {
                            Some(d) => match tokio::time::timeout_at(d, inner).await {
                                Ok(inner_result) => inner_result,
                                Err(_) => Err(CanoError::timeout(format!(
                                    "compensate for {task_id_for_deadline:?} exceeded compensation deadline"
                                ))),
                            },
                            None => inner.await,
                        }
                    };
                    #[cfg_attr(not(feature = "metrics"), allow(unused_variables))]
                    let compensate_ok = match AssertUnwindSafe(with_deadline).catch_unwind().await {
                        Ok(Ok(())) => true,
                        Ok(Err(e)) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(task_id = %entry.task_id, error = %e, "compensation failed");
                            errors.push(e);
                            false
                        }
                        Err(payload) => {
                            let msg = panic_payload_message(&*payload);
                            #[cfg(feature = "tracing")]
                            tracing::error!(task_id = %entry.task_id, panic = %msg, "compensation panicked");
                            errors.push(CanoError::task_execution(format!(
                                "compensate for {:?} panicked: {msg}",
                                entry.task_id
                            )));
                            false
                        }
                    };
                    #[cfg(feature = "metrics")]
                    crate::metrics::compensation_run(compensate_ok);
                }
            }
        }
        if errors.len() == 1 {
            // Clean rollback — clear the log (best-effort), surface the original error.
            #[cfg(feature = "metrics")]
            crate::metrics::compensation_drain(true);
            self.clear_checkpoint_log(workflow_id).await;
            Err(errors
                .into_iter()
                .next()
                .expect("errors has exactly one element"))
        } else {
            #[cfg(feature = "metrics")]
            crate::metrics::compensation_drain(false);
            Err(CanoError::compensation_failed(errors))
        }
    }

    /// Best-effort `clear` on the checkpoint store: a successful run, or a run that was
    /// fully rolled back, shouldn't leave a recovery log behind — but a hiccup cleaning
    /// it up shouldn't turn the run's result into an error, so a `clear` failure is logged
    /// and swallowed. No-op when no store or no workflow id is in play.
    ///
    /// The `on_checkpoint_clear_failed` observer hook fires on failure so the silent
    /// swallow is still observable through the user-supplied pipeline — without this
    /// the stranded log would only show up later as a duplicate-sequence conflict on
    /// a fresh run, or as duplicate compensation on a subsequent `resume_from`.
    pub(super) async fn clear_checkpoint_log(&self, workflow_id: Option<&str>) {
        let Some((store, wf_id)) = self.checkpoint_store.as_ref().zip(workflow_id) else {
            return;
        };
        let clear_result = store.clear(wf_id).await;
        #[cfg(feature = "metrics")]
        crate::metrics::checkpoint_clear(clear_result.is_ok());
        if let Err(e) = &clear_result {
            #[cfg(feature = "tracing")]
            tracing::warn!(workflow_id = %wf_id, error = %e, "failed to clear checkpoint log");
            notify_observers(&self.observers, |o| o.on_checkpoint_clear_failed(wf_id, e));
        }
        #[cfg(not(feature = "tracing"))]
        let _ = clear_result;
    }

    pub(super) async fn execute_compensatable_task(
        &self,
        task: Arc<dyn ErasedCompensatable<TState, TResourceKey>>,
        config: Arc<crate::task::TaskConfig>,
    ) -> Result<(TState, Vec<u8>), CanoError> {
        let observers = self.observer_slice();
        let task_name = task.name();
        if let Some(ref slice) = observers {
            notify_observers(slice, |o| o.on_task_start(task_name.as_ref()));
        }
        let config = Self::config_with_observers(&config, &observers, &task_name);
        // Strip `attempt_timeout` before handing the config to
        // `run_with_retries`. The compensatable adapter applies the original
        // attempt_timeout *only* to the forward call (see
        // `CompensatableAdapter::run`); leaving it on the retry-loop config
        // would also wrap the inline-compensate path in `tokio::time::timeout`
        // — dropping that future mid-await destroys the typed `output` value
        // with no recovery path (the compensation entry was never persisted),
        // leaving partial side effects with no automated rollback. Stripping
        // here keeps the per-attempt bound on the forward call while letting
        // inline compensate run to completion.
        let config = if config.attempt_timeout.is_some() {
            Arc::new(crate::task::TaskConfig {
                attempt_timeout: None,
                ..(*config).clone()
            })
        } else {
            config
        };

        #[cfg(feature = "tracing")]
        let task_span = if tracing::enabled!(tracing::Level::INFO) {
            info_span!("compensatable_task_execution")
        } else {
            tracing::Span::none()
        };

        let run_future = async {
            run_with_retries(&config, || {
                let task_clone = task.clone();
                let resources_clone = Arc::clone(&self.resources);
                async move { task_clone.run(&*resources_clone).await }
            })
            .await
        };

        #[cfg(feature = "tracing")]
        let result: Result<(TaskResult<TState>, Vec<u8>), CanoError> = {
            let _enter = task_span.enter();
            super::catch_panic_to_error(run_future, "Compensatable task").await
        };
        #[cfg(not(feature = "tracing"))]
        let result: Result<(TaskResult<TState>, Vec<u8>), CanoError> =
            super::catch_panic_to_error(run_future, "Compensatable task").await;

        let outcome: Result<(TState, Vec<u8>), CanoError> = match result {
            Ok((TaskResult::Single(next_state), blob)) => Ok((next_state, blob)),
            Ok((TaskResult::Split(_), _)) => Err(CanoError::workflow(
                "Compensatable task returned a split result — split states cannot be compensatable",
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

    /// Map a checkpoint state label (the `Debug` rendering of a `TState`) back to
    /// the matching registered or exit state, or `None` if no state has that label.
    fn state_from_label(&self, label: &str) -> Option<TState> {
        self.states
            .keys()
            .chain(self.exit_states.iter())
            .find(|s| format!("{s:?}") == label)
            .cloned()
    }

    /// Resume a previously checkpointed run.
    ///
    /// Loads every [`CheckpointRow`](crate::recovery::CheckpointRow) recorded for
    /// `workflow_id` from the attached [`CheckpointStore`](crate::recovery::CheckpointStore),
    /// takes the highest-`sequence` row, maps its state label back to a registered (or exit)
    /// state, and re-enters the FSM loop from there — re-running that state's task. **Tasks
    /// at and after the resume point must be idempotent**: the workflow has no way to know
    /// whether the resumed state's side effects already completed before the crash.
    ///
    /// If the run's last recorded state is an exit state, this returns it immediately
    /// (the run had already completed).
    ///
    /// Resources' `setup`/`teardown` run around the resumed execution, exactly as in
    /// [`orchestrate`](Self::orchestrate). Subsequent checkpoint rows are recorded
    /// under the same `workflow_id` and continue the sequence.
    ///
    /// The [compensation stack](crate::saga) is rehydrated from the loaded rows: every
    /// row whose [`kind`](crate::recovery::CheckpointRow::kind) is
    /// [`RowKind::CompensationCompletion`] becomes a stack entry (in sequence order), so a
    /// failure after the resume point can still compensate work done before the crash.
    /// Rows with other kinds — ordinary [`RowKind::StateEntry`] rows and
    /// [`RowKind::StepCursor`] rows — are ignored by the rehydration.
    ///
    /// # Errors
    ///
    /// - [`CanoError::Configuration`] — no checkpoint store attached, or the workflow
    ///   shape is invalid (see [`validate`](Self::validate)).
    /// - [`CanoError::CheckpointStore`] — the store failed, or there are no recorded
    ///   rows for `workflow_id`.
    /// - [`CanoError::Workflow`] — the recorded state label doesn't match any state of
    ///   this workflow (e.g. resuming against a different workflow definition).
    /// - Any [`CanoError`] propagated from a task during the resumed execution.
    pub async fn resume_from(&self, workflow_id: impl Into<Arc<str>>) -> Result<TState, CanoError> {
        self.resume_from_with_cancel(workflow_id, CancellationToken::never())
            .await
    }

    /// Like [`resume_from`](Self::resume_from), but cooperatively cancellable via `token`.
    ///
    /// Behaves identically to `resume_from` except that firing the paired
    /// [`CancellationHandle`](crate::cancel::CancellationHandle) aborts the resumed run at the next
    /// await point and drains the rehydrated compensation stack, returning
    /// [`CanoError::Cancelled`]. See [`orchestrate_with_cancel`](Self::orchestrate_with_cancel) and
    /// the [`cancel`](crate::cancel) module for the full cancellation semantics.
    ///
    /// `resume_from(id)` is exactly `resume_from_with_cancel(id, <never-cancelled token>)`.
    pub async fn resume_from_with_cancel(
        &self,
        workflow_id: impl Into<Arc<str>>,
        token: CancellationToken,
    ) -> Result<TState, CanoError> {
        let workflow_id: Arc<str> = workflow_id.into();

        #[cfg(feature = "tracing")]
        let workflow_span = self.tracing_span.clone().unwrap_or_else(|| {
            if tracing::enabled!(tracing::Level::INFO) {
                // `workflow_id` is recorded so the `metrics-tracing-context` bridge can
                // attach it as a label to Cano metrics emitted during the resumed run.
                info_span!("workflow_resume", workflow_id = workflow_id.as_ref())
            } else {
                tracing::Span::none()
            }
        });
        #[cfg(feature = "tracing")]
        let _enter = workflow_span.enter();

        let store = self.checkpoint_store.clone().ok_or_else(|| {
            CanoError::configuration(
                "resume_from requires a checkpoint store (call with_checkpoint_store)",
            )
        })?;

        let cached_validation = self.validated.get_or_init(|| self.validate());
        if let Err(e) = cached_validation {
            return Err(e.clone());
        }

        // Set up resources before any rehydration work. If `setup_all` fails
        // we'd otherwise have a fully-rebuilt compensation stack and a fired
        // `on_resume` event with no way to use them — the rehydrated entries
        // would be silently dropped. Doing setup first means a failure leaves
        // the on-disk log unchanged, the observer pipeline un-notified, and
        // the caller can retry `resume_from` after fixing their resources.
        self.resources.setup_all().await?;

        // Once setup_all succeeds, EVERY exit path from this function must
        // run teardown — even the rehydration `?`/`return Err` early-returns
        // between here and `execute_workflow_from`. Wrap the body so a single
        // `teardown_range` call at the bottom handles every path uniformly.
        let result: Result<TState, CanoError> =
            self.execute_resume_inner(workflow_id, store, token).await;
        self.resources
            .teardown_range(0..self.resources.lifecycle_len())
            .await;
        result
    }

    /// The body of `resume_from` between `setup_all` and the unconditional
    /// teardown. Extracted so every `?`/`return Err` early-return is still
    /// followed by `teardown_range` in the caller — without this, an error
    /// path between `load_run` and `execute_workflow_from` (e.g. unknown
    /// workflow_id, version mismatch, corrupt log) would leak the
    /// freshly-set-up resources.
    async fn execute_resume_inner(
        &self,
        workflow_id: Arc<str>,
        store: Arc<dyn crate::recovery::CheckpointStore>,
        token: CancellationToken,
    ) -> Result<TState, CanoError> {
        let mut rows = store.load_run(&workflow_id).await.map_err(|e| {
            CanoError::checkpoint_store(format!("load checkpoint run {workflow_id:?}: {e}"))
        })?;
        // Defense in depth against a backend that forgot to sort: the trait
        // promises ascending-by-sequence and the rehydration logic below relies
        // on that, but a custom backend (e.g. a Postgres impl that forgot
        // `ORDER BY sequence`) can silently violate it. Sorting in the engine
        // is O(N log N) on an already-sorted log so the overhead for compliant
        // backends is negligible.
        rows.sort_by_key(|r| r.sequence);
        let last = rows.last().ok_or_else(|| {
            CanoError::checkpoint_store(format!(
                "no checkpoint rows for workflow id {workflow_id:?}"
            ))
        })?;
        // Validate the *last* row's version — the one that determines which
        // workflow definition the resume continues against. Older rows in the
        // log may legitimately stamp earlier versions when a workflow was
        // bumped mid-run (e.g. the operator deployed a new version after the
        // last checkpoint of a v0 run was written, then resumed under v1).
        // Rejecting on a per-row scan would refuse exactly the migration
        // scenario `workflow_version` was meant to support; a `tracing::warn!`
        // for older mixed rows keeps the operator informed without aborting.
        if last.workflow_version != self.workflow_version {
            return Err(CanoError::workflow_version_mismatch(
                last.workflow_version,
                self.workflow_version,
            ));
        }
        let resume_state = self.state_from_label(&last.state).ok_or_else(|| {
            CanoError::workflow(format!(
                "checkpoint state {:?} is not a registered or exit state of this workflow",
                last.state
            ))
        })?;
        let start_sequence = last.sequence + 1;
        let resume_sequence = last.sequence;

        // Resolve checkpoint state labels (the `Debug` rendering of each state)
        // back to a `TState` in O(1). `from_rows` invokes the closure below once
        // per `StateEntry` row; building this index once keeps resume O(states)
        // instead of the O(rows × states) `format!` scan a per-row
        // `state_from_label` would do.
        let label_index: HashMap<String, TState> = self
            .states
            .keys()
            .chain(self.exit_states.iter())
            .map(|s| (format!("{s:?}"), s.clone()))
            .collect();

        // Single-pass rehydration. See `RehydratedRun::from_rows` for the
        // walk semantics + F3/F4/F11 warning emission; the multi-pass
        // version this replaced did the same work in five separate filter
        // chains over `rows`.
        let resume_label = last.state.clone();
        let RehydratedRun {
            compensation_stack,
            resume_cursors,
            prior_transitions,
            resume_state_entry_cutoff: _,
        } = RehydratedRun::<TState>::from_rows(
            &rows,
            &resume_label,
            resume_sequence,
            self.workflow_version,
            &self.observers,
            &workflow_id,
            |label| label_index.get(label).cloned(),
        );

        #[cfg(feature = "tracing")]
        info!(workflow_id = %workflow_id, resume_state = ?resume_state, last_sequence = last.sequence, compensation_entries = compensation_stack.len(), cursor_states = resume_cursors.len(), "Resuming workflow from checkpoint");
        notify_observers(&self.observers, |o| {
            o.on_resume(&workflow_id, last.sequence)
        });

        // Resources were set up at the top of `resume_from` so a setup
        // failure is surfaced before any rehydration runs — we don't repeat
        // the call here.
        #[cfg(feature = "metrics")]
        let _active = crate::metrics::WorkflowActiveGuard::new();
        let started = std::time::Instant::now();
        let total_budget = self.resolve_total_budget(started);
        let exec = self.execute_workflow_from(
            resume_state,
            start_sequence,
            Some(workflow_id),
            compensation_stack,
            resume_cursors,
            prior_transitions,
            total_budget,
            token,
        );
        // Teardown happens in the outer `resume_from` after this function
        // returns, so this branch only produces the error value and lets
        // the caller clean up. Emit the workflow-run outcome metric here, the
        // same way `run_workflow` does for the forward direction.
        let result = exec.await;
        Self::record_run_outcome(&result, started);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PollErrorPolicy;
    use crate::resource::Resources;
    use crate::saga;
    use crate::saga::CompensatableTask;
    use crate::task as task_mod;
    use crate::task::{Task, TaskConfig};
    use crate::workflow::test_support::*;
    use crate::workflow::{JoinConfig, JoinStrategy};
    use cano_macros::task;
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    // ---- Checkpoint / resume ----

    use crate::recovery::{CheckpointRow, CheckpointStore};
    use std::sync::Mutex;

    #[tokio::test]
    async fn checkpoint_row_written_for_each_state_entered() {
        let store = Arc::new(MemCheckpoints::default());
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("run-1");

        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );

        // One row per state entered, in order — including the terminal exit state.
        assert_eq!(
            store.audit_states("run-1"),
            vec![
                (0, "Start".to_string()),
                (1, "Process".to_string()),
                (2, "Complete".to_string()),
            ]
        );
        let rows = store.audit_rows("run-1");
        assert!(
            !rows[0].task_id.is_empty() && !rows[1].task_id.is_empty(),
            "rows for registered states carry the task name"
        );
        assert!(
            rows[2].task_id.is_empty(),
            "the exit state has no task, so its row's task_id is empty"
        );
        // A successful run clears its live log.
        assert!(store.rows("run-1").is_empty());
    }

    #[tokio::test]
    async fn no_checkpoint_store_means_no_rows_and_resume_is_rejected() {
        // A store-free workflow runs exactly as before — nothing to assert beyond
        // "it still works"; the interesting bit is that resume_from refuses.
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete);
        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );
        let err = workflow.resume_from("whatever").await.unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(err.message().contains("checkpoint store"));
    }

    #[tokio::test]
    async fn resume_continues_from_last_checkpointed_state() {
        let store = Arc::new(MemCheckpoints::default());
        // Seed: the run got as far as entering `Process` before "crashing".
        store
            .append("run-2", CheckpointRow::new(0, "Start", ""))
            .await
            .unwrap();
        store
            .append("run-2", CheckpointRow::new(1, "Process", ""))
            .await
            .unwrap();

        let start_task = SimpleTask::new(TestState::Process);
        let process_task = SimpleTask::new(TestState::Complete);
        let (observer, rec) = EventLog::new();
        let workflow = Workflow::bare()
            .register(TestState::Start, start_task.clone())
            .register(TestState::Process, process_task.clone())
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_observer(Arc::new(observer));

        assert_eq!(
            workflow.resume_from("run-2").await.unwrap(),
            TestState::Complete
        );
        assert_eq!(
            start_task.count(),
            0,
            "state before the resume point never runs"
        );
        assert_eq!(
            process_task.count(),
            1,
            "the resumed state's task re-runs once"
        );

        // Continuation appended rows from sequence 2 onward (re-entering `Process`, then `Complete`).
        assert_eq!(
            store.audit_states("run-2"),
            vec![
                (0, "Start".to_string()),
                (1, "Process".to_string()),
                (2, "Process".to_string()),
                (3, "Complete".to_string()),
            ]
        );
        assert_eq!(rec.resumes(), vec![("run-2".to_string(), 1)]);
        assert_eq!(
            rec.checkpoints(),
            vec![("run-2".to_string(), 2), ("run-2".to_string(), 3)]
        );
    }

    #[tokio::test]
    async fn resume_from_exit_state_returns_immediately() {
        let store = Arc::new(MemCheckpoints::default());
        store
            .append("done-run", CheckpointRow::new(0, "Start", ""))
            .await
            .unwrap();
        store
            .append("done-run", CheckpointRow::new(1, "Complete", ""))
            .await
            .unwrap();
        let work = SimpleTask::new(TestState::Complete);
        let workflow = Workflow::bare()
            .register(TestState::Start, work.clone())
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        assert_eq!(
            workflow.resume_from("done-run").await.unwrap(),
            TestState::Complete
        );
        assert_eq!(
            work.count(),
            0,
            "no task runs when resuming into an exit state"
        );
    }

    #[tokio::test]
    async fn split_state_writes_a_single_checkpoint_row() {
        let store = Arc::new(MemCheckpoints::default());
        let tasks: Vec<SimpleTask> = (0..5)
            .map(|_| SimpleTask::new(TestState::Complete))
            .collect();
        let join_config = JoinConfig::new(JoinStrategy::All, TestState::Complete);
        let workflow = Workflow::bare()
            .register_split(TestState::Start, tasks, join_config)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("split-run");

        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );

        // The 5-way split is one state ⇒ one entry row, not five.
        let rows = store.audit_rows("split-run");
        assert_eq!(
            rows.iter().filter(|r| r.state == "Start").count(),
            1,
            "the split state is checkpointed exactly once"
        );
        assert_eq!(
            store.audit_states("split-run"),
            vec![(0, "Start".to_string()), (1, "Complete".to_string())]
        );
        assert!(
            rows[0].task_id.is_empty(),
            "a split state's checkpoint row has no single task id"
        );
    }

    #[tokio::test]
    async fn checkpoint_requires_workflow_id_on_orchestrate() {
        let store = Arc::new(MemCheckpoints::default());
        // Store attached but no workflow id — orchestrate must error at the first checkpoint write.
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        // Errors raised inside `execute_workflow_from` are wrapped with state context;
        // `.inner()` peels one layer back to the underlying checkpoint_store error.
        assert_eq!(err.inner().category(), "checkpoint_store");
        assert!(err.message().contains("workflow id"));
    }

    #[tokio::test]
    async fn resume_unknown_workflow_id_errors() {
        let store = Arc::new(MemCheckpoints::default());
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());
        let err = workflow.resume_from("never-ran").await.unwrap_err();
        assert_eq!(err.category(), "checkpoint_store");
        assert!(err.message().contains("no checkpoint rows"));
    }

    #[tokio::test]
    async fn resume_with_unrecognized_state_label_errors() {
        let store = Arc::new(MemCheckpoints::default());
        store
            .append(
                "wrong-defn",
                CheckpointRow::new(0, "NotAStateOfThisWorkflow", ""),
            )
            .await
            .unwrap();
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());
        let err = workflow.resume_from("wrong-defn").await.unwrap_err();
        assert_eq!(err.category(), "workflow");
        assert!(err.message().contains("is not a registered or exit state"));
    }

    // ---- Router checkpoint-skipping ----

    #[tokio::test]
    async fn router_state_produces_no_checkpoint_row_and_sequences_are_dense() {
        // Note: inside the `cano` crate the inherent `#[task::router(state = S)]` form emits
        // `::cano::RouterTask<...>` paths that don't resolve. Use the trait-impl form instead.
        use crate::task::{RouterTask, TaskConfig};

        struct RouteToWork;

        #[task_mod::router]
        impl RouterTask<TestState> for RouteToWork {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Process))
            }
        }

        let store = Arc::new(MemCheckpoints::default());
        // One `EventLog` observes both on_state_enter and on_checkpoint, to verify the
        // router fires the former but NOT the latter.
        let (obs, rec) = EventLog::new();

        // Route (router) → Process (single) → Complete (exit)
        let workflow = Workflow::bare()
            .register_router(TestState::Start, RouteToWork)
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("router-run")
            .with_observer(Arc::new(obs));

        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );

        // `on_state_enter` fires for ALL states (including the router).
        assert_eq!(
            rec.state_enters(),
            vec!["Start", "Process", "Complete"],
            "on_state_enter fires for every state including the router"
        );

        // No router row: only Process (seq 0) and Complete (seq 1).
        assert_eq!(
            store.audit_states("router-run"),
            vec![(0, "Process".to_string()), (1, "Complete".to_string())],
            "router state leaves no checkpoint row; sequences are dense from 0"
        );

        // `on_checkpoint` must not have fired for the Start/router state.
        assert_eq!(
            rec.checkpoints(),
            vec![("router-run".to_string(), 0), ("router-run".to_string(), 1)],
            "on_checkpoint fires only for non-router states"
        );
    }

    #[tokio::test]
    async fn resume_from_skips_router_rows_not_present_in_checkpoint_log() {
        // Seed a run that was interrupted after `Process` (sequence 0). No Route row exists
        // because `Start` was a router state (skipped). `resume_from` should re-enter at
        // `Process` and finish normally.
        use crate::task::{RouterTask, TaskConfig};

        struct RouteToWork;

        #[task_mod::router]
        impl RouterTask<TestState> for RouteToWork {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Process))
            }
        }

        let store = Arc::new(MemCheckpoints::default());
        // Only the Process row exists — as if the run crashed right after entering Process
        // (the Start router left no row).
        store
            .append("router-resume", CheckpointRow::new(0, "Process", ""))
            .await
            .unwrap();

        let process_task = SimpleTask::new(TestState::Complete);
        let workflow = Workflow::bare()
            .register_router(TestState::Start, RouteToWork)
            .register(TestState::Process, process_task.clone())
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        assert_eq!(
            workflow.resume_from("router-resume").await.unwrap(),
            TestState::Complete
        );
        assert_eq!(
            process_task.count(),
            1,
            "Process task re-runs once on resume"
        );
        // Continuation wrote Process (seq 1) and Complete (seq 2).
        assert_eq!(
            store.audit_states("router-resume"),
            vec![
                (0, "Process".to_string()),
                (1, "Process".to_string()),
                (2, "Complete".to_string()),
            ]
        );
    }

    // ---- Saga / compensation ----

    // `CompLog` and `CompTask` (with `fail_forward` / `fail_compensate` flags)
    // live in `workflow::test_support` so both the saga unit tests here and
    // the metrics test mod can share them.

    #[tokio::test]
    async fn compensations_run_in_reverse_on_terminal_failure() {
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 2, TestState::Split, &log),
            )
            .register_with_compensation(
                TestState::Split,
                CompTask::ok("C", 3, TestState::Join, &log),
            )
            .register_with_compensation(
                TestState::Join,
                CompTask {
                    name: "D",
                    value: 4,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        // Clean rollback → the original failure is surfaced, wrapped with state context.
        assert_eq!(err.inner().category(), "task_execution");
        assert_eq!(err.message(), "D forward failed");
        // A, B, C were compensated in reverse registration order. D failed forward, so it
        // never produced an output and is not on the stack.
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                ("C".to_string(), 3),
                ("B".to_string(), 2),
                ("A".to_string(), 1),
            ]
        );
    }

    #[tokio::test]
    async fn only_compensatable_tasks_are_compensated() {
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 10, TestState::Process, &log),
            )
            // Plain task — produces no compensation-stack entry.
            .register(TestState::Process, SimpleTask::new(TestState::Split))
            .register_with_compensation(
                TestState::Split,
                CompTask::ok("C", 30, TestState::Join, &log),
            )
            .register_with_compensation(
                TestState::Join,
                CompTask {
                    name: "D",
                    value: 40,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert_eq!(err.message(), "D forward failed");
        // Only the two compensatable tasks rolled back — the plain `Process` task didn't.
        assert_eq!(
            *log.lock().unwrap(),
            vec![("C".to_string(), 30), ("A".to_string(), 10)]
        );
    }

    #[tokio::test]
    async fn compensation_failure_aggregates_errors_and_keeps_going() {
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask {
                    name: "B",
                    value: 2,
                    next_state: TestState::Split,
                    log: log.clone(),
                    fail_forward: false,
                    fail_compensate: true, // B's compensate errors
                },
            )
            .register_with_compensation(
                TestState::Split,
                CompTask::ok("C", 3, TestState::Join, &log),
            )
            .register_with_compensation(
                TestState::Join,
                CompTask {
                    name: "D",
                    value: 4,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        match err {
            CanoError::CompensationFailed { errors } => {
                // [original (D forward failed, wrapped with state context), B's compensate failure].
                assert_eq!(errors.len(), 2);
                assert_eq!(errors[0].message(), "D forward failed");
                assert!(errors[1].message().contains("B compensate failed"));
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
        // All three compensations ran (C, B, A) even though B's errored.
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                ("C".to_string(), 3),
                ("B".to_string(), 2),
                ("A".to_string(), 1),
            ]
        );
    }

    #[tokio::test]
    async fn compensation_failed_errors_first_is_state_context_wrapped() {
        // S1 (Start, "A") commits a compensation entry; S2 (Process, "B") fails forward;
        // S1's compensate also fails on rollback. The resulting CompensationFailed must have
        // errors[0] = WithStateContext { state = "Process", source = original task failure }
        // and errors[1] = A's compensate failure.
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask {
                    name: "A",
                    value: 1,
                    next_state: TestState::Process,
                    log: log.clone(),
                    fail_forward: false,
                    fail_compensate: true,
                },
            )
            .register_with_compensation(
                TestState::Process,
                CompTask {
                    name: "B",
                    value: 2,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        match err {
            CanoError::CompensationFailed { errors } => {
                assert!(
                    errors.len() >= 2,
                    "original + at least one comp failure, got {errors:?}"
                );
                match &errors[0] {
                    CanoError::WithStateContext {
                        state,
                        attempt,
                        transitions_so_far,
                        source,
                    } => {
                        assert_eq!(state, "Process");
                        assert_eq!(*attempt, 1);
                        assert_eq!(
                            *transitions_so_far,
                            vec!["Start".to_string(), "Process".to_string()]
                        );
                        assert!(
                            matches!(**source, CanoError::TaskExecution(_)),
                            "expected inner TaskExecution, got {source:?}"
                        );
                    }
                    other => panic!("errors[0] should be WithStateContext, got {other:?}"),
                }
                assert!(errors[1].message().contains("A compensate failed"));
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_rehydrates_compensation_stack_and_rolls_back() {
        let store = Arc::new(MemCheckpoints::default());
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));

        // Seed a log as if a previous process: entered Start, ran compensatable A (output 7),
        // entered Process, ran compensatable B (output 8), entered Split — then crashed
        // before C ran.
        store
            .append("saga-run", CheckpointRow::new(0, "Start", "A"))
            .await
            .unwrap();
        store
            .append(
                "saga-run",
                CheckpointRow::new(1, "Start", "A").with_output(serde_json::to_vec(&7u32).unwrap()),
            )
            .await
            .unwrap();
        store
            .append("saga-run", CheckpointRow::new(2, "Process", "B"))
            .await
            .unwrap();
        store
            .append(
                "saga-run",
                CheckpointRow::new(3, "Process", "B")
                    .with_output(serde_json::to_vec(&8u32).unwrap()),
            )
            .await
            .unwrap();
        store
            .append("saga-run", CheckpointRow::new(4, "Split", "C"))
            .await
            .unwrap();

        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 7, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 8, TestState::Split, &log),
            )
            .register_with_compensation(
                TestState::Split,
                CompTask {
                    name: "C",
                    value: 9,
                    next_state: TestState::Join,
                    log: log.clone(),
                    fail_forward: true, // C fails when re-run on resume
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let err = workflow.resume_from("saga-run").await.unwrap_err();
        assert_eq!(err.message(), "C forward failed");
        // The rehydrated stack [A=7, B=8] drains in reverse, using the outputs persisted
        // before the crash. C never produced an output (it failed forward).
        assert_eq!(
            *log.lock().unwrap(),
            vec![("B".to_string(), 8), ("A".to_string(), 7)]
        );
    }

    /// Regression test: a `StepCursor` row (which also carries an `output_blob`) must NOT
    /// be mistaken for a `CompensationCompletion` entry during compensation-stack rehydration.
    /// Plain `StateEntry` rows must also be ignored. Only `CompensationCompletion` rows seed
    /// the stack — everything else passes through silently.
    #[tokio::test]
    async fn resume_step_cursor_row_is_not_added_to_compensation_stack() {
        use crate::recovery::RowKind;

        let store = Arc::new(MemCheckpoints::default());
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));

        // Seed a mixed log:
        //  seq 0 — StateEntry for A          (kind = StateEntry,            no output_blob)
        //  seq 1 — CompensationCompletion A  (kind = CompensationCompletion, output = 101)
        //  seq 2 — StateEntry for B          (kind = StateEntry,            no output_blob)
        //  seq 3 — CompensationCompletion B  (kind = CompensationCompletion, output = 202)
        //  seq 4 — StateEntry for C          (kind = StateEntry — the engine always writes
        //                                     this before any other row for the state)
        //  seq 5 — StepCursor for C          (kind = StepCursor,            cursor = [9,9])
        //
        // The highest-sequence row (seq 5) is the resume point → C is the resume state.
        // Rehydration must produce exactly [A=101, B=202] (in that order), not [A, B, StepCursor-C].
        store
            .append("mixed-run", CheckpointRow::new(0, "Start", "A"))
            .await
            .unwrap();
        store
            .append(
                "mixed-run",
                CheckpointRow::new(1, "Start", "A")
                    .with_output(serde_json::to_vec(&101u32).unwrap()),
            )
            .await
            .unwrap();
        store
            .append("mixed-run", CheckpointRow::new(2, "Process", "B"))
            .await
            .unwrap();
        store
            .append(
                "mixed-run",
                CheckpointRow::new(3, "Process", "B")
                    .with_output(serde_json::to_vec(&202u32).unwrap()),
            )
            .await
            .unwrap();
        store
            .append("mixed-run", CheckpointRow::new(4, "Split", "C"))
            .await
            .unwrap();
        // StepCursor row for the Split state — has output_blob but is NOT a CompensationCompletion.
        let cursor_row = CheckpointRow::new(5, "Split", "C").with_cursor(vec![9, 9]);
        assert_eq!(
            cursor_row.kind,
            RowKind::StepCursor,
            "sanity: with_cursor sets RowKind::StepCursor"
        );
        store.append("mixed-run", cursor_row).await.unwrap();

        // C (Split state) is the resume point; configure it to fail forward so the compensation
        // stack drains and we can inspect which entries were rehydrated.
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 101, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 202, TestState::Split, &log),
            )
            .register_with_compensation(
                TestState::Split,
                CompTask {
                    name: "C",
                    value: 999,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true, // C fails when re-run → triggers rollback
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let err = workflow.resume_from("mixed-run").await.unwrap_err();
        // C failed forward (original error is "C forward failed").
        assert_eq!(err.message(), "C forward failed");

        // Only B and A were in the rehydrated stack (in LIFO order: B first, then A).
        // The StepCursor row for C must NOT appear — and C failed forward so it never
        // pushed its own entry either.
        assert_eq!(
            *log.lock().unwrap(),
            vec![("B".to_string(), 202), ("A".to_string(), 101)],
            "StepCursor and StateEntry rows must not become compensation entries"
        );
    }

    #[test]
    fn re_registering_a_state_drops_the_stale_compensator() {
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        // Replacing a compensatable state with a different compensatable task.
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A2", 2, TestState::Process, &log),
            );
        assert!(
            !workflow.compensators.contains_key("A"),
            "the replaced task's compensator must not linger"
        );
        assert!(workflow.compensators.contains_key("A2"));

        // Replacing a compensatable state with a plain `register` also drops it.
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("B", 1, TestState::Process, &log),
            )
            .register(TestState::Start, SimpleTask::new(TestState::Process));
        assert!(!workflow.compensators.contains_key("B"));
    }

    // ---- hardening: concurrency & high-error scenarios ----

    fn three_state_checkpointed(
        store: Arc<MemCheckpoints>,
        id: impl Into<Arc<str>>,
    ) -> Workflow<TestState> {
        Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store)
            .with_workflow_id(id)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_workflows_share_one_store_without_cross_talk() {
        let store = Arc::new(MemCheckpoints::default());
        const RUNS: usize = 16;

        let mut handles = Vec::new();
        for i in 0..RUNS {
            let s = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                three_state_checkpointed(s, format!("run-{i}"))
                    .orchestrate(TestState::Start)
                    .await
            }));
        }
        for h in handles {
            assert_eq!(h.await.unwrap().unwrap(), TestState::Complete);
        }

        for i in 0..RUNS {
            let id = format!("run-{i}");
            assert_eq!(
                store.audit_states(&id),
                vec![
                    (0, "Start".to_string()),
                    (1, "Process".to_string()),
                    (2, "Complete".to_string()),
                ],
                "{id}: exactly its own three rows, in order"
            );
            assert!(
                store.rows(&id).is_empty(),
                "{id}: a successful run clears its live log"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn racing_runs_of_one_id_never_corrupt_and_at_least_one_completes() {
        let store = Arc::new(MemCheckpoints::default());
        let mut handles = Vec::new();
        for _ in 0..2 {
            let s = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                three_state_checkpointed(s, "dup")
                    .orchestrate(TestState::Start)
                    .await
            }));
        }
        let mut completed = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(TestState::Complete) => completed += 1,
                Ok(other) => panic!("unexpected success state {other:?}"),
                // The loser of the seq-0 race fails fast with a conflict, not corruption.
                Err(e) => {
                    assert_eq!(
                        e.inner().category(),
                        "checkpoint_store",
                        "unexpected error: {e}"
                    );
                }
            }
        }
        assert!(
            completed >= 1,
            "whichever run wins sequence 0 must run to completion"
        );
    }

    #[tokio::test]
    async fn fresh_run_over_an_uncleared_log_is_rejected() {
        let store = Arc::new(MemCheckpoints::default());
        // A previous run that failed left rows behind (no `clear`).
        store
            .append("run", CheckpointRow::new(0, "Start", "SimpleTask"))
            .await
            .unwrap();
        store
            .append("run", CheckpointRow::new(1, "Process", "SimpleTask"))
            .await
            .unwrap();

        let err = three_state_checkpointed(store.clone(), "run")
            .orchestrate(TestState::Start)
            .await
            .unwrap_err();
        assert_eq!(err.inner().category(), "checkpoint_store");
        assert!(err.message().contains("conflict"), "got: {err}");
        // The leftover log is intact — the run can still be resumed.
        assert_eq!(store.rows("run").len(), 2);
    }

    #[tokio::test]
    async fn append_failure_mid_run_rolls_back_and_keeps_the_log() {
        // A store that errors on the n-th `append` — a stand-in for a disk-full crash.
        struct FailAfter {
            inner: MemCheckpoints,
            ok_appends: std::sync::atomic::AtomicUsize,
        }
        #[cano_macros::checkpoint_store]
        impl CheckpointStore for FailAfter {
            async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
                if self.ok_appends.fetch_sub(1, Ordering::SeqCst) == 0 {
                    return Err(CanoError::checkpoint_store("simulated disk failure"));
                }
                self.inner.append(workflow_id, row).await
            }
            async fn load_run(&self, id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
                self.inner.load_run(id).await
            }
            async fn clear(&self, id: &str) -> Result<(), CanoError> {
                self.inner.clear(id).await
            }
        }

        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(FailAfter {
            inner: MemCheckpoints::default(),
            // Allow: Start entry (1), Start completion (2). Fail on the 3rd append (Process entry).
            ok_appends: std::sync::atomic::AtomicUsize::new(2),
        });
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                // A's compensate *also* fails → dirty rollback → log left for manual recovery.
                CompTask {
                    name: "A",
                    value: 1,
                    next_state: TestState::Process,
                    log: log.clone(),
                    fail_forward: false,
                    fail_compensate: true,
                },
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 2, TestState::Complete, &log),
            )
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("disk");

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        match err {
            CanoError::CompensationFailed { errors } => {
                // [the append failure that ended the run (now wrapped with state context),
                // then A's compensate failure].
                assert_eq!(errors[0].inner().category(), "checkpoint_store");
                assert!(errors[1].message().contains("A compensate failed"));
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
        assert_eq!(*log.lock().unwrap(), vec![("A".to_string(), 1)]);
        // B never ran (its checkpoint append is the one that failed). The durable prefix
        // (Start entry + completion) is NOT cleared — a dirty rollback leaves it for recovery.
        assert_eq!(store.inner.rows("disk").len(), 2);
    }

    // -- saga hardening --

    /// A compensatable task that succeeds forward (→ `next`, output `value`) and then,
    /// on `compensate`, either panics or hangs forever (the latter bounded by `attempt_timeout`).
    #[derive(Clone)]
    struct CompFault {
        name: &'static str,
        value: u32,
        next: TestState,
        on_compensate: CompFaultKind,
        attempt_timeout: Option<Duration>,
    }
    #[derive(Clone, Copy)]
    enum CompFaultKind {
        Panic,
        Hang,
        /// Sleep for the given duration inside `compensate`, then return `Ok(())`.
        /// Used to exercise the bounded-deadline drain path.
        Sleep(Duration),
    }
    #[saga::task]
    impl CompensatableTask<TestState> for CompFault {
        type Output = u32;
        fn config(&self) -> TaskConfig {
            let cfg = TaskConfig::minimal();
            match self.attempt_timeout {
                Some(d) => cfg.with_attempt_timeout(d),
                None => cfg,
            }
        }
        fn name(&self) -> Cow<'static, str> {
            Cow::Borrowed(self.name)
        }
        async fn run(&self, _res: &Resources) -> Result<(TaskResult<TestState>, u32), CanoError> {
            Ok((TaskResult::Single(self.next.clone()), self.value))
        }
        async fn compensate(&self, _res: &Resources, _output: u32) -> Result<(), CanoError> {
            match self.on_compensate {
                CompFaultKind::Panic => panic!("{} compensate exploded", self.name),
                CompFaultKind::Hang => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
                CompFaultKind::Sleep(d) => {
                    tokio::time::sleep(d).await;
                    Ok(())
                }
            }
        }
    }

    #[tokio::test]
    async fn panicking_compensator_is_caught_and_the_drain_continues() {
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompFault {
                    name: "B",
                    value: 2,
                    next: TestState::Split,
                    on_compensate: CompFaultKind::Panic,
                    attempt_timeout: None,
                },
            )
            .register_with_compensation(
                TestState::Split,
                CompTask {
                    name: "C",
                    value: 3,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        match err {
            CanoError::CompensationFailed { errors } => {
                assert_eq!(errors[0].message(), "C forward failed");
                assert!(
                    errors[1..].iter().any(|e| e.message().contains("panicked")),
                    "the caught panic must be one of the collected errors: {errors:?}"
                );
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
        // The drain didn't stop at B's panic — A was still compensated.
        assert_eq!(*log.lock().unwrap(), vec![("A".to_string(), 1)]);
    }

    #[tokio::test]
    async fn hanging_compensator_is_bounded_by_attempt_timeout() {
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompFault {
                    name: "H",
                    value: 2,
                    next: TestState::Split,
                    on_compensate: CompFaultKind::Hang,
                    attempt_timeout: Some(Duration::from_millis(50)),
                },
            )
            .register_with_compensation(
                TestState::Split,
                CompTask {
                    name: "C",
                    value: 3,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete);

        let started = std::time::Instant::now();
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a hanging compensator must be bounded, not block the drain forever"
        );
        match err {
            CanoError::CompensationFailed { errors } => {
                assert_eq!(errors[0].message(), "C forward failed");
                assert!(
                    errors[1..]
                        .iter()
                        .any(|e| matches!(e, CanoError::Timeout(_))),
                    "H's compensate must time out: {errors:?}"
                );
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
        // A (registered before H) was still compensated after H timed out.
        assert_eq!(*log.lock().unwrap(), vec![("A".to_string(), 1)]);
    }

    #[tokio::test]
    async fn run_compensations_bounded_aborts_slow_compensations_and_reports_them() {
        // Drive `run_compensations_bounded` directly. Build a workflow with two compensators
        // (Fast + Slow), hand-craft the compensation stack with serialized outputs, and pass
        // a 50ms deadline. Slow sleeps ~1s in `compensate` — the deadline must abort it and
        // surface a `CompensationFailed` whose collected errors mention the compensation
        // deadline. The drain must NOT block on Slow for the full second.
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("Fast", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompFault {
                    name: "Slow",
                    value: 2,
                    next: TestState::Complete,
                    on_compensate: CompFaultKind::Sleep(Duration::from_secs(1)),
                    attempt_timeout: None,
                },
            )
            .add_exit_state(TestState::Complete);

        // Hand-build the stack: Fast registered first, Slow registered second, so a LIFO
        // drain pops Slow first.
        let stack = vec![
            CompensationEntry {
                task_id: Arc::from("Fast"),
                output_blob: serde_json::to_vec(&1u32).unwrap(),
            },
            CompensationEntry {
                task_id: Arc::from("Slow"),
                output_blob: serde_json::to_vec(&2u32).unwrap(),
            },
        ];
        let original = CanoError::task_execution("forward boom");

        let started = std::time::Instant::now();
        let result = workflow
            .run_compensations_bounded(
                None,
                stack,
                original,
                Some(tokio::time::Instant::now() + Duration::from_millis(50)),
            )
            .await;
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "bounded drain must abort Slow well under its 1s sleep"
        );

        let err = result.unwrap_err();
        match err {
            CanoError::CompensationFailed { errors } => {
                assert_eq!(errors[0].message(), "forward boom");
                assert!(
                    errors[1..]
                        .iter()
                        .any(|e| e.message().contains("compensation deadline")),
                    "at least one error must mention the compensation deadline: {errors:?}"
                );
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
        // Fast was registered before Slow on the stack. With a 50ms deadline that's already
        // elapsed by the time the second iteration starts, Fast is also aborted — and even
        // if it managed to squeak through, the test passes as long as Slow surfaced a
        // deadline error. We only assert the deadline-error shape above.
    }

    #[tokio::test]
    async fn pre_dispatch_failure_honors_bounded_compensation_deadline() {
        // Regression for F1: previously, pre-dispatch failure arms (missing
        // workflow id, checkpoint append failure, missing handler,
        // compensation-completion append failure) called the *unbounded*
        // `run_compensations`, silently bypassing `with_compensation_timeout`
        // even when `with_total_timeout` was set. A long-running compensator
        // could blow past the user's configured wall-clock cap. Every
        // drain entry point now routes through `drain_for_budget` and
        // honors the bounded budget consistently.

        // No-op compensator that sleeps long enough to *exceed* the
        // bounded comp deadline. Without the fix this would block the
        // whole drain; with the fix it is aborted by `tokio::time::timeout_at`.
        #[derive(Clone)]
        struct SlowCompensate {
            log: CompLog,
            name: &'static str,
            value: u32,
            next_state: TestState,
        }
        #[saga::task]
        impl CompensatableTask<TestState> for SlowCompensate {
            type Output = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            fn name(&self) -> Cow<'static, str> {
                Cow::Borrowed(self.name)
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, u32), CanoError> {
                Ok((TaskResult::Single(self.next_state.clone()), self.value))
            }
            async fn compensate(&self, _res: &Resources, output: u32) -> Result<(), CanoError> {
                tokio::time::sleep(Duration::from_secs(1)).await;
                self.log
                    .lock()
                    .unwrap()
                    .push((self.name.to_string(), output));
                Ok(())
            }
        }

        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        // Build a workflow whose dispatch reaches `Process` and then finds
        // no registered handler for it — triggering the pre-dispatch
        // "no task registered" arm. Before the fix this called the
        // unbounded drain.
        let workflow = Workflow::bare()
            .with_total_timeout(Duration::from_secs(10))
            .with_compensation_timeout(Duration::from_millis(50))
            .register_with_compensation(
                TestState::Start,
                SlowCompensate {
                    log: log.clone(),
                    name: "A",
                    value: 1,
                    next_state: TestState::Process,
                },
            )
            // `Process` deliberately has no registered task — its dispatch
            // hits the "no task registered" arm at execution.rs.
            .add_exit_state(TestState::Complete);

        let started = std::time::Instant::now();
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        let elapsed = started.elapsed();

        // With the bounded drain (50ms cap) the test finishes well under
        // SlowCompensate's 1s sleep. Without the fix this would have taken
        // ~1s as the unbounded drain awaits the compensator's full sleep.
        assert!(
            elapsed < Duration::from_millis(500),
            "bounded compensation timeout must bound the drain even on pre-dispatch failures; took {elapsed:?}"
        );
        // The surfaced error should carry the deadline summary among the
        // collected compensation errors.
        match err {
            CanoError::CompensationFailed { errors } => {
                assert!(
                    errors
                        .iter()
                        .any(|e| e.message().contains("compensation deadline")
                            || e.message().contains("exceeded compensation")),
                    "expected a deadline-related error in the compensation failure, got: {errors:?}"
                );
            }
            other => panic!("expected CompensationFailed, got: {other:?}"),
        }
        // SlowCompensate's compensate body was aborted before reaching the
        // log push — proving the bounded drain dropped the future.
        assert!(
            log.lock().unwrap().is_empty(),
            "compensate body must have been aborted before completion: {:?}",
            log.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn run_compensations_bounded_preserves_blobs_when_deadline_already_elapsed() {
        // Regression for F2: the pre-iteration deadline check used to drop the
        // just-popped entry on the floor (`break` after a generic "N entries
        // left unprocessed" string) — silently discarding the persisted
        // `output_blob` operators need for manual rollback. The popped entry
        // and every remaining one must now be preserved as
        // `OrphanedCompensation` errors carrying the blob.
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 2, TestState::Complete, &log),
            )
            .add_exit_state(TestState::Complete);

        let stack = vec![
            CompensationEntry {
                task_id: Arc::from("A"),
                output_blob: serde_json::to_vec(&111u32).unwrap(),
            },
            CompensationEntry {
                task_id: Arc::from("B"),
                output_blob: serde_json::to_vec(&222u32).unwrap(),
            },
        ];
        let original = CanoError::task_execution("forward boom");
        // Deadline already in the past — the very first iteration trips it.
        let already_elapsed = tokio::time::Instant::now() - Duration::from_millis(1);
        let result = workflow
            .run_compensations_bounded(None, stack, original, Some(already_elapsed))
            .await;
        let err = result.unwrap_err();
        let errors = match err {
            CanoError::CompensationFailed { errors } => errors,
            other => panic!("expected CompensationFailed, got {other:?}"),
        };
        // errors[0] = original failure. The middle errors[1..n-1] must be
        // OrphanedCompensation entries (one per stack entry, both preserving
        // their blob). The final error is the timeout summary.
        assert_eq!(errors[0].message(), "forward boom");
        let blobs: Vec<(String, Vec<u8>)> = errors
            .iter()
            .filter_map(|e| match e {
                CanoError::OrphanedCompensation {
                    task_id,
                    output_blob,
                } => Some((task_id.to_string(), output_blob.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            blobs.len(),
            2,
            "both stack entries must be surfaced as OrphanedCompensation (got: {errors:?})"
        );
        // LIFO drain → B popped first, then A. Both blobs must carry the
        // correct serialized payload so an operator can decode and manually
        // undo the side effect.
        let by_id: HashMap<String, Vec<u8>> = blobs.into_iter().collect();
        assert_eq!(by_id["B"], serde_json::to_vec(&222u32).unwrap());
        assert_eq!(by_id["A"], serde_json::to_vec(&111u32).unwrap());
        assert!(
            errors
                .iter()
                .any(|e| e.message().contains("compensation deadline")),
            "summary timeout error must still be present: {errors:?}"
        );
        // No compensator should have actually run — deadline tripped on the
        // first pop, both entries were preserved without running compensate.
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn resolve_compensation_deadline_uses_override_when_set() {
        use crate::workflow::compensation::resolve_compensation_deadline;
        let d =
            resolve_compensation_deadline(Duration::from_secs(60), Some(Duration::from_secs(7)));
        assert_eq!(d, Duration::from_secs(7));
    }

    #[test]
    fn resolve_compensation_deadline_caps_at_thirty_seconds() {
        use crate::workflow::compensation::resolve_compensation_deadline;
        let d = resolve_compensation_deadline(Duration::from_secs(120), None);
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn resolve_compensation_deadline_uses_half_remaining() {
        use crate::workflow::compensation::resolve_compensation_deadline;
        let d = resolve_compensation_deadline(Duration::from_secs(10), None);
        assert_eq!(d, Duration::from_secs(5));
    }

    #[test]
    fn resolve_compensation_deadline_zero_total_limit_yields_zero() {
        // A pathological `with_total_timeout(0)` collapses the drain budget to
        // zero — every compensator times out immediately. Callers must set
        // `with_compensation_timeout` explicitly to opt back into a grace period.
        use crate::workflow::compensation::resolve_compensation_deadline;
        let d = resolve_compensation_deadline(Duration::from_secs(0), None);
        assert_eq!(d, Duration::from_secs(0));
    }

    #[tokio::test]
    async fn double_compensate_on_resume_is_avoided() {
        let store = Arc::new(MemCheckpoints::default());
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));

        // Seed a log that crashed *after* B's completion row but *before* the next state's
        // entry row — so `Process` (B) is the resume point and re-runs.
        store
            .append("crash-after-b", CheckpointRow::new(0, "Start", "A"))
            .await
            .unwrap();
        store
            .append(
                "crash-after-b",
                CheckpointRow::new(1, "Start", "A").with_output(serde_json::to_vec(&1u32).unwrap()),
            )
            .await
            .unwrap();
        store
            .append("crash-after-b", CheckpointRow::new(2, "Process", "B"))
            .await
            .unwrap();
        store
            .append(
                "crash-after-b",
                CheckpointRow::new(3, "Process", "B")
                    .with_output(serde_json::to_vec(&2u32).unwrap()),
            )
            .await
            .unwrap();

        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 2, TestState::Split, &log),
            )
            .register_with_compensation(
                TestState::Split,
                CompTask {
                    name: "C",
                    value: 3,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let err = workflow.resume_from("crash-after-b").await.unwrap_err();
        assert_eq!(err.message(), "C forward failed");
        // B re-ran on resume and re-pushed its entry; the persisted B-completion row at the
        // resume point must NOT be replayed too, or B would compensate twice. Expect exactly
        // one B compensation, then A's (rehydrated).
        assert_eq!(
            *log.lock().unwrap(),
            vec![("B".to_string(), 2), ("A".to_string(), 1)],
            "B compensated exactly once"
        );
    }

    #[tokio::test]
    async fn resume_from_honors_total_timeout() {
        // `resume_from` must thread `total_timeout` into the FSM loop the same way the
        // forward `run`/`orchestrate` path does. Without it, a slow resumed task would
        // run to completion regardless of the configured budget.
        let store = Arc::new(MemCheckpoints::default());
        // Seed: run got as far as entering `Process` before crashing — resume re-runs Process.
        store
            .append("resume-budget", CheckpointRow::new(0, "Process", ""))
            .await
            .unwrap();

        // SlowProcess sleeps well past the total budget so the only way the test passes
        // is if the engine-level timeout fires.
        #[derive(Clone)]
        struct SlowProcess;
        #[task]
        impl Task<TestState> for SlowProcess {
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let (obs, rec) = EventLog::new();

        let workflow = Workflow::bare()
            .with_total_timeout(Duration::from_millis(40))
            .with_observer(Arc::new(obs))
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SlowProcess)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let started = std::time::Instant::now();
        let err = workflow.resume_from("resume-budget").await.unwrap_err();
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(300),
            "resume_from should not wait for the slow task: {elapsed:?}"
        );

        assert!(
            matches!(err.inner(), CanoError::WorkflowTimeout { .. }),
            "got: {err}"
        );

        let events = rec.timeouts();
        assert_eq!(events.len(), 1, "on_workflow_timeout should fire once");
        let (elapsed_hook, limit_hook) = events[0];
        assert_eq!(limit_hook, Duration::from_millis(40));
        assert!(elapsed_hook >= limit_hook);
    }

    // ---- Cross-model interoperability ----

    /// Chain all four processing models (router → poll → batch → stepped → done)
    /// in one workflow and assert:
    ///
    /// 1. The workflow completes at `Done`.
    /// 2. The `Route` router state never writes a `CheckpointRow` (router rows are skipped).
    /// 3. Sequence numbers are dense — no gap where `Route` would have been.
    /// 4. `Wait`, `Crunch`, `Grind` (entry + cursor rows), and `Done` each appear in the
    ///    audit log under `RowKind::StateEntry` / `RowKind::StepCursor`.
    #[tokio::test]
    async fn cross_model_chain_router_poll_batch_stepped_interop() {
        use crate::PollOutcome;
        use crate::recovery::RowKind;
        use crate::task::{BatchTask, PollTask, RouterTask, StepOutcome, SteppedTask, TaskConfig};
        use serde::{Deserialize, Serialize};
        use std::sync::atomic::AtomicU32;

        // --- local state enum ---
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        enum TourStage {
            Route,
            Wait,
            Crunch,
            Grind,
            Done,
        }

        // --- Route (RouterTask) ---
        // Reads nothing meaningful, unconditionally routes to Wait.
        struct TourRouter;

        #[task_mod::router]
        impl RouterTask<TourStage> for TourRouter {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TourStage>, CanoError> {
                Ok(TaskResult::Single(TourStage::Wait))
            }
        }

        // --- Wait (PollTask) ---
        // Shares an AtomicU32. Returns Ready after 2 polls (counter becomes ≥ 2).
        struct TourPoller {
            counter: Arc<AtomicU32>,
        }

        #[task_mod::poll]
        impl PollTask<TourStage> for TourPoller {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn poll(&self, _res: &Resources) -> Result<PollOutcome<TourStage>, CanoError> {
                let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
                if n >= 2 {
                    Ok(PollOutcome::Ready(TaskResult::Single(TourStage::Crunch)))
                } else {
                    Ok(PollOutcome::Pending { delay_ms: 0 })
                }
            }
        }

        // --- Crunch (BatchTask) ---
        // Fan-out over [1,2,3]: each item → item * item. finish sums them.
        struct TourBatch;

        #[task_mod::batch]
        impl BatchTask<TourStage> for TourBatch {
            type Item = u32;
            type ItemOutput = u32;

            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            fn concurrency(&self) -> usize {
                4
            }
            async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
                Ok(vec![1, 2, 3])
            }
            async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
                Ok(item * item)
            }
            async fn finish(
                &self,
                _res: &Resources,
                outputs: Vec<Result<u32, CanoError>>,
            ) -> Result<TaskResult<TourStage>, CanoError> {
                let sum: u32 = outputs.into_iter().filter_map(|r| r.ok()).sum();
                assert_eq!(sum, 1 + 4 + 9, "1²+2²+3² = 14");
                Ok(TaskResult::Single(TourStage::Grind))
            }
        }

        // --- Grind (SteppedTask) ---
        // Counts 0 → 3 (3 steps total: More(1), More(2), More(3), Done).
        struct TourStepper;

        #[derive(Serialize, Deserialize)]
        struct GrindCursor(u32);

        #[task_mod::stepped]
        impl SteppedTask<TourStage> for TourStepper {
            type Cursor = GrindCursor;

            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn step(
                &self,
                _res: &Resources,
                cursor: Option<GrindCursor>,
            ) -> Result<StepOutcome<GrindCursor, TourStage>, CanoError> {
                let n = cursor.map(|c| c.0).unwrap_or(0);
                if n >= 3 {
                    Ok(StepOutcome::Done(TaskResult::Single(TourStage::Done)))
                } else {
                    Ok(StepOutcome::More(GrindCursor(n + 1)))
                }
            }
        }

        // --- build workflow ---
        let store = Arc::new(MemCheckpoints::default());
        let poll_counter = Arc::new(AtomicU32::new(0));

        let workflow = Workflow::bare()
            .register_router(TourStage::Route, TourRouter)
            .register(
                TourStage::Wait,
                TourPoller {
                    counter: Arc::clone(&poll_counter),
                },
            )
            .register(TourStage::Crunch, TourBatch)
            .register_stepped(TourStage::Grind, TourStepper)
            .add_exit_state(TourStage::Done)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("tour-interop");

        let result = workflow.orchestrate(TourStage::Route).await.unwrap();
        assert_eq!(result, TourStage::Done);

        // --- assertions on the audit log ---
        let audit = store.audit_rows("tour-interop");

        // (1) No row whose state label is "Route" (the router skips the checkpoint).
        assert!(
            audit.iter().all(|r| r.state != "Route"),
            "router state must leave no checkpoint row; got: {audit:?}"
        );

        // (2) Sequence numbers are dense from 0 (no gap where Route would have been).
        for (idx, row) in audit.iter().enumerate() {
            assert_eq!(
                row.sequence, idx as u64,
                "sequence gap at index {idx}: got {} expected {idx}",
                row.sequence
            );
        }

        // (3) The StateEntry rows, in order, must be: Wait, Crunch, Grind, Done.
        let state_entry_states: Vec<&str> = audit
            .iter()
            .filter(|r| r.kind == RowKind::StateEntry)
            .map(|r| r.state.as_str())
            .collect();
        assert_eq!(
            state_entry_states,
            vec!["Wait", "Crunch", "Grind", "Done"],
            "StateEntry rows must cover all non-router states in order"
        );

        // (4) At least one StepCursor row exists for Grind.
        assert!(
            audit
                .iter()
                .any(|r| r.kind == RowKind::StepCursor && r.state == "Grind"),
            "Grind stepped task must produce at least one StepCursor row"
        );
    }

    #[tokio::test]
    async fn resume_then_advance_then_fail_rolls_back_mixed_outputs_and_clears() {
        let store = Arc::new(MemCheckpoints::default());
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));

        // Crash mid-A (entry written, no completion) — A is the resume point and re-runs.
        store
            .append("mid-a", CheckpointRow::new(0, "Start", "A"))
            .await
            .unwrap();

        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 11, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 22, TestState::Split, &log),
            )
            .register_with_compensation(
                TestState::Split,
                CompTask {
                    name: "C",
                    value: 33,
                    next_state: TestState::Complete,
                    log: log.clone(),
                    fail_forward: true,
                    fail_compensate: false,
                },
            )
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let err = workflow.resume_from("mid-a").await.unwrap_err();
        assert_eq!(err.message(), "C forward failed");
        // A re-ran (fresh output 11), B ran (22), C failed → drain B then A.
        assert_eq!(
            *log.lock().unwrap(),
            vec![("B".to_string(), 22), ("A".to_string(), 11)]
        );
        // Clean rollback ⇒ the recovery log is cleared.
        assert!(store.rows("mid-a").is_empty());
    }

    #[tokio::test]
    async fn empty_compensation_stack_failure_keeps_the_log_for_resume() {
        // A plain, retry-free failing task — nothing ever lands on the compensation stack.
        #[derive(Clone)]
        struct FailFast;
        #[task]
        impl Task<TestState> for FailFast {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                Err(CanoError::task_execution("boom"))
            }
        }

        let store = Arc::new(MemCheckpoints::default());
        let workflow = Workflow::bare()
            .register(TestState::Start, FailFast)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("nope");
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert_eq!(err.inner().category(), "task_execution");
        // The Start checkpoint row is kept (empty stack ⇒ original error, no `clear`) —
        // so the run can still be resumed.
        assert_eq!(store.rows("nope").len(), 1);
    }

    #[tokio::test]
    async fn deep_compensation_stack_drains_all_in_reverse() {
        const N: u32 = 60;
        let log = Arc::new(Mutex::new(Vec::<u32>::new()));

        #[derive(Clone)]
        struct NumComp {
            idx: u32,
            fail: bool,
            log: Arc<Mutex<Vec<u32>>>,
        }
        #[saga::task]
        impl CompensatableTask<u32> for NumComp {
            type Output = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            fn name(&self) -> Cow<'static, str> {
                Cow::Owned(format!("n{}", self.idx))
            }
            async fn run(&self, _res: &Resources) -> Result<(TaskResult<u32>, u32), CanoError> {
                if self.fail {
                    return Err(CanoError::task_execution(format!("n{} failed", self.idx)));
                }
                Ok((TaskResult::Single(self.idx + 1), self.idx))
            }
            async fn compensate(&self, _res: &Resources, output: u32) -> Result<(), CanoError> {
                self.log.lock().unwrap().push(output);
                Ok(())
            }
        }

        let mut workflow = Workflow::<u32>::bare().add_exit_state(N);
        for i in 0..N {
            workflow = workflow.register_with_compensation(
                i,
                NumComp {
                    idx: i,
                    fail: i == N - 1,
                    log: Arc::clone(&log),
                },
            );
        }

        let err = workflow.orchestrate(0).await.unwrap_err();
        assert_eq!(err.message(), format!("n{} failed", N - 1));
        // States 0..N-1 succeeded forward (the last one failed), so 0..N-1 compensate in reverse.
        let expected: Vec<u32> = (0..N - 1).rev().collect();
        assert_eq!(*log.lock().unwrap(), expected);
    }

    #[tokio::test]
    async fn failure_at_every_step_compensates_exactly_the_completed_prefix() {
        // For each possible failure point k in 0..=N (k == N means "no failure"), the run
        // either completes (k == N) or rolls back states 0..k in reverse, cleanly.
        const N: u32 = 5;

        #[derive(Clone)]
        struct Step {
            idx: u32,
            fail_at: u32,
            log: Arc<Mutex<Vec<u32>>>,
        }
        #[saga::task]
        impl CompensatableTask<u32> for Step {
            type Output = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            fn name(&self) -> Cow<'static, str> {
                Cow::Owned(format!("s{}", self.idx))
            }
            async fn run(&self, _res: &Resources) -> Result<(TaskResult<u32>, u32), CanoError> {
                if self.idx == self.fail_at {
                    return Err(CanoError::task_execution(format!("s{} failed", self.idx)));
                }
                Ok((TaskResult::Single(self.idx + 1), self.idx))
            }
            async fn compensate(&self, _res: &Resources, output: u32) -> Result<(), CanoError> {
                self.log.lock().unwrap().push(output);
                Ok(())
            }
        }

        for fail_at in 0..=N {
            let log = Arc::new(Mutex::new(Vec::<u32>::new()));
            let mut workflow = Workflow::<u32>::bare().add_exit_state(N);
            for i in 0..N {
                workflow = workflow.register_with_compensation(
                    i,
                    Step {
                        idx: i,
                        fail_at,
                        log: Arc::clone(&log),
                    },
                );
            }
            let result = workflow.orchestrate(0).await;
            if fail_at == N {
                assert_eq!(result.unwrap(), N, "no failure ⇒ run completes");
                assert!(
                    log.lock().unwrap().is_empty(),
                    "completed run compensates nothing"
                );
            } else {
                let err = result.unwrap_err();
                assert_eq!(err.message(), format!("s{fail_at} failed"));
                let expected: Vec<u32> = (0..fail_at).rev().collect();
                assert_eq!(
                    *log.lock().unwrap(),
                    expected,
                    "fail at {fail_at}: states 0..{fail_at} compensate in reverse"
                );
            }
        }
    }

    // ---- SteppedTask with checkpoint store ----

    use crate::recovery::RowKind;
    use crate::task::stepped::{StepOutcome, SteppedTask};
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

    /// A counting stepper: each step increments the cursor by 1 until it hits `target`.
    struct CountStepper {
        target: u32,
        /// Counts how many `step` calls were made (for retry/resume assertions).
        calls: Arc<AtomicU32>,
    }

    impl CountStepper {
        fn new(target: u32) -> (Self, Arc<AtomicU32>) {
            let calls = Arc::new(AtomicU32::new(0));
            (
                Self {
                    target,
                    calls: Arc::clone(&calls),
                },
                calls,
            )
        }
    }

    #[task_mod::stepped]
    impl SteppedTask<TestState> for CountStepper {
        type Cursor = u32;
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        async fn step(
            &self,
            _res: &Resources,
            cursor: Option<u32>,
        ) -> Result<StepOutcome<u32, TestState>, CanoError> {
            self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            let n = cursor.unwrap_or(0) + 1;
            if n >= self.target {
                Ok(StepOutcome::Done(TaskResult::Single(TestState::Complete)))
            } else {
                Ok(StepOutcome::More(n))
            }
        }
    }

    #[tokio::test]
    async fn stepped_forward_run_writes_cursor_rows_and_clears_on_success() {
        let store = Arc::new(MemCheckpoints::default());
        let (stepper, calls) = CountStepper::new(4); // steps: None→1, 1→2, 2→3, 3→Done

        let workflow = Workflow::bare()
            .register_stepped(TestState::Start, stepper)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("step-fwd");

        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 4);

        // Audit: Start (seq 0), cursor 1 (seq 1), cursor 2 (seq 2), cursor 3 (seq 3),
        // Complete (seq 4). Live rows must be empty on success.
        let audit = store.audit_rows("step-fwd");
        assert_eq!(audit.len(), 5, "start + 3 cursors + exit = 5 rows");
        assert_eq!(audit[0].kind, RowKind::StateEntry);
        assert_eq!(audit[0].state, "Start");
        assert_eq!(audit[1].kind, RowKind::StepCursor);
        assert_eq!(audit[2].kind, RowKind::StepCursor);
        assert_eq!(audit[3].kind, RowKind::StepCursor);
        assert_eq!(audit[4].kind, RowKind::StateEntry);
        assert_eq!(audit[4].state, "Complete");
        // cursor values are serde_json-encoded u32 bytes
        assert_eq!(
            serde_json::from_slice::<u32>(audit[1].output_blob.as_ref().unwrap()).unwrap(),
            1
        );
        assert_eq!(
            serde_json::from_slice::<u32>(audit[3].output_blob.as_ref().unwrap()).unwrap(),
            3
        );
        assert!(
            store.rows("step-fwd").is_empty(),
            "live log cleared on success"
        );
    }

    #[tokio::test]
    async fn stepped_resume_continues_from_last_cursor() {
        let store = Arc::new(MemCheckpoints::default());

        // Seed: the run had entered Start (seq 0), written cursor=1 (seq 1), cursor=2 (seq 2),
        // then crashed. Resume should start from cursor=2, not from None.
        store
            .append(
                "step-resume",
                CheckpointRow::new(0, "Start", "CountStepper"),
            )
            .await
            .unwrap();
        store
            .append(
                "step-resume",
                CheckpointRow::new(1, "Start", "CountStepper")
                    .with_cursor(serde_json::to_vec(&1u32).unwrap()),
            )
            .await
            .unwrap();
        store
            .append(
                "step-resume",
                CheckpointRow::new(2, "Start", "CountStepper")
                    .with_cursor(serde_json::to_vec(&2u32).unwrap()),
            )
            .await
            .unwrap();

        // target=4 → steps: cursor=2→3, 3→Done (only 2 more calls, not 4).
        let (stepper, calls) = CountStepper::new(4);

        let workflow = Workflow::bare()
            .register_stepped(TestState::Start, stepper)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        assert_eq!(
            workflow.resume_from("step-resume").await.unwrap(),
            TestState::Complete
        );
        // Only 2 step calls: cursor=2→More(3), cursor=3→Done.
        assert_eq!(
            calls.load(AtomicOrdering::Relaxed),
            2,
            "resumed run must not restart from None"
        );
    }

    #[tokio::test]
    async fn stepped_sequences_are_dense_after_cursors() {
        // Verify that cursor rows consume sequence numbers and subsequent state rows
        // (e.g. the exit state) continue at the right sequence.
        let store = Arc::new(MemCheckpoints::default());
        let (stepper, _) = CountStepper::new(3); // 2 cursor rows (1 and 2), then Done

        let workflow = Workflow::bare()
            .register_stepped(TestState::Start, stepper)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("dense");

        workflow.orchestrate(TestState::Start).await.unwrap();

        let audit = store.audit_rows("dense");
        // seq 0: Start (StateEntry), seq 1: cursor=1, seq 2: cursor=2, seq 3: Complete (exit)
        assert_eq!(audit[0].sequence, 0);
        assert_eq!(audit[1].sequence, 1);
        assert_eq!(audit[2].sequence, 2);
        assert_eq!(audit[3].sequence, 3);
        for (i, row) in audit.iter().enumerate() {
            assert_eq!(
                row.sequence, i as u64,
                "sequences must be contiguous, gap at {i}"
            );
        }
    }

    #[tokio::test]
    async fn stepped_resume_without_cursor_rows_restarts_from_none() {
        // If the run crashed before any cursor was persisted (only the StateEntry row exists),
        // resume should start from None (same as a fresh run).
        let store = Arc::new(MemCheckpoints::default());
        store
            .append(
                "step-fresh-resume",
                CheckpointRow::new(0, "Start", "CountStepper"),
            )
            .await
            .unwrap();

        let (stepper, calls) = CountStepper::new(3); // needs 2 successful steps

        let workflow = Workflow::bare()
            .register_stepped(TestState::Start, stepper)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        assert_eq!(
            workflow.resume_from("step-fresh-resume").await.unwrap(),
            TestState::Complete
        );
        // Full 2 steps: None→1, 1→2, 2→Done = 3 calls
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 3);
    }

    #[tokio::test]
    async fn stepped_no_store_path_unchanged() {
        // Without a checkpoint store, register_stepped behaves like register (no rows written).
        let (stepper, calls) = CountStepper::new(3);

        let result = Workflow::bare()
            .register_stepped(TestState::Start, stepper)
            .add_exit_state(TestState::Complete)
            .orchestrate(TestState::Start)
            .await
            .unwrap();
        assert_eq!(result, TestState::Complete);
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 3);
    }

    #[tokio::test]
    async fn stepped_cursor_rows_do_not_become_compensation_entries() {
        // Verifies the existing regression test still passes with the full engine path
        // (StepCursor rows from register_stepped must not pollute the compensation stack).
        let store = Arc::new(MemCheckpoints::default());
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));

        // Seed: A (compensatable, seq 0+1), Process StateEntry (seq 2 — the engine
        // always writes one before any other row for the state), cursor for the
        // stepped state (seq 3), then crash.
        store
            .append("mixed-stepped", CheckpointRow::new(0, "Start", "A"))
            .await
            .unwrap();
        store
            .append(
                "mixed-stepped",
                CheckpointRow::new(1, "Start", "A")
                    .with_output(serde_json::to_vec(&42u32).unwrap()),
            )
            .await
            .unwrap();
        store
            .append(
                "mixed-stepped",
                CheckpointRow::new(2, "Process", "CountStepper"),
            )
            .await
            .unwrap();
        store
            .append(
                "mixed-stepped",
                CheckpointRow::new(3, "Process", "CountStepper")
                    .with_cursor(serde_json::to_vec(&1u32).unwrap()),
            )
            .await
            .unwrap();

        // Process is a Stepped state; it's the resume point. Make the stepper fail so
        // the compensation stack (rehydrated A) drains and we can inspect it.
        struct FailingStepper;
        #[task_mod::stepped]
        impl SteppedTask<TestState> for FailingStepper {
            type Cursor = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn step(
                &self,
                _res: &Resources,
                _cursor: Option<u32>,
            ) -> Result<StepOutcome<u32, TestState>, CanoError> {
                Err(CanoError::task_execution("stepper failed"))
            }
        }

        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 42, TestState::Process, &log),
            )
            .register_stepped(TestState::Process, FailingStepper)
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let err = workflow.resume_from("mixed-stepped").await.unwrap_err();
        assert_eq!(err.message(), "stepper failed");
        // A must have been compensated with value 42 from the rehydrated stack.
        assert_eq!(
            *log.lock().unwrap(),
            vec![("A".to_string(), 42u32)],
            "StepCursor row must not become a compensation entry"
        );
    }

    #[tokio::test]
    async fn workflow_version_is_stamped_on_every_appended_row() {
        let store = Arc::new(MemCheckpoints::default());
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("ver-run")
            .with_workflow_version(7);
        workflow.orchestrate(TestState::Start).await.unwrap();
        let rows = store.audit_rows("ver-run");
        assert!(rows.iter().all(|r| r.workflow_version == 7));
        assert!(!rows.is_empty(), "expected at least one appended row");
    }

    #[tokio::test]
    async fn resume_from_rejects_workflow_version_mismatch() {
        let store = Arc::new(MemCheckpoints::default());
        store
            .append(
                "ver-mismatch",
                CheckpointRow::new(0, "Start", "").with_workflow_version(1),
            )
            .await
            .unwrap();
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_version(2);
        let err = workflow.resume_from("ver-mismatch").await.unwrap_err();
        assert_eq!(err, CanoError::workflow_version_mismatch(1, 2));
    }

    #[tokio::test]
    async fn resume_from_falls_back_when_no_state_entry_for_resume_label() {
        // Regression for F4: a strict `.ok_or_else(...)` requirement on a
        // `StateEntry` row for the resume label refused to resume legitimate
        // logs whose StateEntry was lost (corrupted backend, coalesced
        // writes, older schema). The resume should still succeed when there
        // are still rehydrate-able earlier rows; the rehydrated path is
        // best-effort, so missing it is not a fatal corruption.
        let store = Arc::new(MemCheckpoints::default());
        // Only a single `StepCursor` row exists for the resume state — no
        // leading `StateEntry`. Previously this configuration was rejected.
        store
            .append(
                "wf-no-se",
                CheckpointRow::new(0, "Start", "S")
                    .with_cursor(b"cursor".to_vec())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());
        let out = workflow
            .resume_from("wf-no-se")
            .await
            .expect("resume should fall back instead of refusing on missing StateEntry");
        assert_eq!(out, TestState::Complete);
    }

    #[tokio::test]
    async fn resume_from_accepts_mixed_version_log_when_last_row_matches() {
        // Regression for F3: previously the per-row version check refused any
        // mid-run version bump — exactly the scenario `with_workflow_version`
        // was meant to support. A log with older v0 rows plus a newer v1 row
        // at the tail must now resume cleanly; the only authoritative version
        // is the latest one, since that determines which workflow definition
        // continues. Older rows just inform the rehydrated path.
        let store = Arc::new(MemCheckpoints::default());
        // Older rows under workflow_version=0.
        store
            .append(
                "mixed-ver",
                CheckpointRow::new(0, "Start", "S").with_workflow_version(0),
            )
            .await
            .unwrap();
        // Final row stamped with the new workflow_version=1 — this is what
        // we resume from.
        store
            .append(
                "mixed-ver",
                CheckpointRow::new(1, "Process", "P").with_workflow_version(1),
            )
            .await
            .unwrap();
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_version(1);
        let out = workflow
            .resume_from("mixed-ver")
            .await
            .expect("mixed-version log with matching tail must resume cleanly");
        assert_eq!(out, TestState::Complete);
    }

    // A backend that violates the `load_run` ascending-by-sequence contract.
    // Used only to verify the engine's defensive sort.
    #[derive(Default)]
    struct UnsortedStore(MemCheckpoints);

    #[cano_macros::checkpoint_store]
    impl CheckpointStore for UnsortedStore {
        async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
            self.0.append(workflow_id, row).await
        }
        async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
            // Deliberately return rows in REVERSE sequence order.
            let mut rows = self.0.load_run(workflow_id).await?;
            rows.reverse();
            Ok(rows)
        }
        async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
            self.0.clear(workflow_id).await
        }
    }

    #[tokio::test]
    async fn resume_from_sorts_unsorted_load_run_results() {
        // A backend that returns rows out of order must not corrupt the LIFO
        // compensation drain. The trait contract requires ascending-by-sequence,
        // but the engine sorts defensively in `resume_from` so a buggy custom
        // store can't silently invert the rollback order.
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(UnsortedStore::default());
        // Build a log with three compensatable completions, then a final
        // StateEntry that fails the resumed task (forcing a drain).
        store
            .append(
                "unsorted",
                CheckpointRow::new(0, "Start", "A").with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "unsorted",
                CheckpointRow::new(1, "Start", "A")
                    .with_output(serde_json::to_vec(&1u32).unwrap())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "unsorted",
                CheckpointRow::new(2, "Process", "B").with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "unsorted",
                CheckpointRow::new(3, "Process", "B")
                    .with_output(serde_json::to_vec(&2u32).unwrap())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "unsorted",
                CheckpointRow::new(4, "Error", "FailFast").with_workflow_version(0),
            )
            .await
            .unwrap();

        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 2, TestState::Error, &log),
            )
            .register(TestState::Error, FailTask::new(true))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let _ = workflow.resume_from("unsorted").await.unwrap_err();
        // LIFO drain: B compensates first (output 2), then A (output 1).
        // Without the engine-side sort, the rehydrated stack would have been
        // built in reverse and `A` would have compensated before `B`.
        assert_eq!(
            *log.lock().unwrap(),
            vec![("B".to_string(), 2u32), ("A".to_string(), 1u32)],
        );
    }

    #[tokio::test]
    async fn compensation_attempt_timeout_drops_compensate_future_mid_await() {
        // The compensation drain bounds each `compensate()` with
        // `attempt_timeout` via `tokio::time::timeout`. When it fires the
        // in-flight future is dropped — work past the await point does NOT
        // run. The drain records the timeout and moves on; the timed-out
        // compensator is NOT retried. This pins the documented cancellation
        // contract on `CompensatableTask::compensate`.
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Clone)]
        struct HangingCompensator {
            began: Arc<AtomicBool>,
            finished: Arc<AtomicBool>,
        }
        #[saga::task]
        impl CompensatableTask<TestState> for HangingCompensator {
            type Output = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(30))
            }
            fn name(&self) -> Cow<'static, str> {
                Cow::Borrowed("HangingCompensator")
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, u32), CanoError> {
                Ok((TaskResult::Single(TestState::Process), 1))
            }
            async fn compensate(&self, _res: &Resources, _o: u32) -> Result<(), CanoError> {
                self.began.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.finished.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        let began = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                HangingCompensator {
                    began: began.clone(),
                    finished: finished.clone(),
                },
            )
            // A forward state that fails to trigger the drain.
            .register(TestState::Process, FailTask::new(true))
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        // The drain ran compensate (it started) but timed out before finishing.
        assert!(began.load(Ordering::SeqCst), "compensate must have started");
        assert!(
            !finished.load(Ordering::SeqCst),
            "compensate future MUST be dropped at attempt_timeout — finished must NOT be set"
        );
        // The drain surfaces a CompensationFailed with the compensator timeout in errors.
        match err {
            CanoError::CompensationFailed { errors } => {
                let timeout_present = errors.iter().any(|e| {
                    let msg = e.message();
                    msg.contains("attempt_timeout") || msg.contains("compensation deadline")
                });
                assert!(timeout_present, "expected timeout error in: {errors:?}");
            }
            other => panic!("expected CompensationFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn attempt_timeout_drops_task_future_mid_await() {
        // `tokio::time::timeout` cancels the in-flight task body. A task that
        // begins side-effect work then yields gets its future dropped — the
        // side effect's "after" handler does NOT run. This pins the
        // documented cancellation contract on `with_attempt_timeout` so a
        // regression that quietly switches to a cancellation-safe wrapper
        // would be caught.
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Clone)]
        struct YieldingTask {
            started: Arc<AtomicBool>,
            finished: Arc<AtomicBool>,
        }
        #[crate::task]
        impl Task<TestState> for YieldingTask {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(30))
            }
            async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
                self.started.store(true, Ordering::SeqCst);
                // Yield well past the attempt_timeout — the future is dropped
                // here and the `finished` flip below never executes.
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.finished.store(true, Ordering::SeqCst);
                Ok(TaskResult::Single(TestState::Complete))
            }
        }

        let started = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let workflow = Workflow::bare()
            .register(
                TestState::Start,
                YieldingTask {
                    started: started.clone(),
                    finished: finished.clone(),
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert!(
            started.load(Ordering::SeqCst),
            "task body must have started"
        );
        assert!(
            !finished.load(Ordering::SeqCst),
            "task future MUST be dropped mid-await — finished flag must NOT be set"
        );
        assert_eq!(err.inner().category(), "timeout");
    }

    #[tokio::test]
    async fn compensatable_panic_after_commit_does_not_push_entry() {
        // The saga safety rule says compensatable tasks must not commit side
        // effects before returning `Ok((next, output))`. This pins what
        // happens when the rule is violated — a panic mid-task surfaces as a
        // failure but no compensation entry was ever pushed, so the side
        // effect leaks. Future work that auto-recovers from mid-task panics
        // must explicitly update this expectation.
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Clone)]
        struct LeakyTask {
            committed: Arc<AtomicBool>,
            compensated: Arc<AtomicBool>,
        }
        #[saga::task]
        impl CompensatableTask<TestState> for LeakyTask {
            type Output = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            fn name(&self) -> Cow<'static, str> {
                Cow::Borrowed("LeakyTask")
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, u32), CanoError> {
                // Simulate the contract violation: commit FIRST, panic SECOND.
                self.committed.store(true, Ordering::SeqCst);
                panic!("commit happened, never reached Ok");
            }
            async fn compensate(&self, _res: &Resources, _o: u32) -> Result<(), CanoError> {
                self.compensated.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        let committed = Arc::new(AtomicBool::new(false));
        let compensated = Arc::new(AtomicBool::new(false));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                LeakyTask {
                    committed: committed.clone(),
                    compensated: compensated.clone(),
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert!(
            committed.load(Ordering::SeqCst),
            "commit must have happened"
        );
        // Documented behavior: the contract violation produces an uncompensated leak.
        // The engine catches the panic and surfaces it, but does NOT call compensate.
        assert!(
            !compensated.load(Ordering::SeqCst),
            "compensate must NOT run when commit happens before Ok((state, output)) returns — \
             this pins the documented saga safety contract; a future fix that auto-compensates \
             panic-mid-commit must explicitly update this test"
        );
        assert!(err.message().contains("panic"), "got: {err}");
    }

    #[tokio::test]
    async fn checkpoint_clear_failure_fires_observer_hook() {
        // A `clear` failure on the success path is best-effort by design and
        // does not turn a successful run into an error — but the failure must
        // still be observable, otherwise a stranded log would later block a
        // fresh run with a duplicate-sequence error. The
        // `on_checkpoint_clear_failed` hook surfaces it through the standard
        // observer pipeline.

        // A store that succeeds on append but always errors on clear.
        struct ClearFailsStore(MemCheckpoints);
        #[cano_macros::checkpoint_store]
        impl CheckpointStore for ClearFailsStore {
            async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
                self.0.append(workflow_id, row).await
            }
            async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
                self.0.load_run(workflow_id).await
            }
            async fn clear(&self, _workflow_id: &str) -> Result<(), CanoError> {
                Err(CanoError::checkpoint_store("clear deliberately fails"))
            }
        }

        let store = Arc::new(ClearFailsStore(MemCheckpoints::default()));
        let (obs, rec) = EventLog::new();

        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("clear-test")
            .with_observer(Arc::new(obs));

        // Successful run that triggers the clear-on-success path.
        workflow.orchestrate(TestState::Start).await.unwrap();

        let calls = rec.clear_failures();
        assert_eq!(calls.len(), 1, "expected one clear-failure event");
        assert_eq!(calls[0].0, "clear-test");
        assert!(
            calls[0].1.contains("clear deliberately fails"),
            "got: {}",
            calls[0].1
        );
    }

    #[tokio::test]
    async fn inline_compensate_runs_to_completion_ignoring_attempt_timeout() {
        // Regression for F7: previously the inline-compensate path applied the
        // task's `attempt_timeout` to its compensate future. Because that
        // future has already consumed the typed `output` value, dropping it
        // mid-await destroyed `output` with no recovery path (the compensation
        // entry was never persisted on this path — it runs before the FSM
        // pushes anything onto the stack). The fix removes the timeout: the
        // inline compensate runs to completion and the operator gets a
        // coherent rollback, bounded by the workflow-level total timeout if
        // they want a wall-clock cap.
        use serde::{Deserialize, Serialize, Serializer};

        #[derive(Debug, Deserialize)]
        struct NeverSerialize;
        impl Serialize for NeverSerialize {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional"))
            }
        }

        #[derive(Clone)]
        struct CountingCompensate {
            ran: Arc<std::sync::atomic::AtomicBool>,
        }
        #[saga::task]
        impl CompensatableTask<TestState> for CountingCompensate {
            type Output = NeverSerialize;
            fn config(&self) -> TaskConfig {
                // Configure a per-attempt timeout that USED to cancel inline
                // compensate. The fix is that compensate ignores it.
                TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(30))
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, NeverSerialize), CanoError> {
                Ok((TaskResult::Single(TestState::Complete), NeverSerialize))
            }
            async fn compensate(
                &self,
                _res: &Resources,
                _o: NeverSerialize,
            ) -> Result<(), CanoError> {
                // Sleep well past the configured `attempt_timeout` to prove
                // it isn't applied to the inline path. ~80ms keeps the test
                // fast while exceeding the 30ms attempt_timeout 2x.
                tokio::time::sleep(Duration::from_millis(80)).await;
                self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let workflow = Workflow::bare()
            .register_with_compensation(TestState::Start, CountingCompensate { ran: ran.clone() })
            .add_exit_state(TestState::Complete);

        let started = std::time::Instant::now();
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            ran.load(std::sync::atomic::Ordering::SeqCst),
            "compensate must run to completion despite attempt_timeout"
        );
        assert!(
            elapsed >= Duration::from_millis(70),
            "compensate slept ~80ms; orchestrate elapsed should reflect that, got {elapsed:?}"
        );
        // Surfaces the original serialize error (compensate succeeded). The
        // pre-fix path would have surfaced `CompensationFailed` because the
        // attempt_timeout fired before compensate finished.
        assert!(
            err.message().contains("serialize"),
            "inline compensate ran cleanly; surfaced error should be the original serialize_err, got: {err}"
        );
    }

    #[tokio::test]
    async fn inline_compensate_catches_panic_and_preserves_original_error() {
        // A panicking `compensate` called inline must not abort the workflow,
        // and must NOT lose the serialize_err that explains why the inline
        // path triggered.
        use serde::{Deserialize, Serialize, Serializer};

        #[derive(Debug, Deserialize)]
        struct NeverSerialize;
        impl Serialize for NeverSerialize {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional"))
            }
        }

        #[derive(Clone)]
        struct PanickyCompensate;
        #[saga::task]
        impl CompensatableTask<TestState> for PanickyCompensate {
            type Output = NeverSerialize;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, NeverSerialize), CanoError> {
                Ok((TaskResult::Single(TestState::Complete), NeverSerialize))
            }
            async fn compensate(
                &self,
                _res: &Resources,
                _o: NeverSerialize,
            ) -> Result<(), CanoError> {
                panic!("compensate boom");
            }
        }

        let workflow = Workflow::bare()
            .register_with_compensation(TestState::Start, PanickyCompensate)
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        match err.inner() {
            CanoError::CompensationFailed { errors } => {
                assert!(
                    errors.iter().any(|e| e.message().contains("serialize")),
                    "serialize_err must be preserved: {errors:?}"
                );
                assert!(
                    errors.iter().any(|e| e.message().contains("panic")),
                    "panic must surface alongside serialize_err: {errors:?}"
                );
            }
            other => panic!("expected CompensationFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn inline_compensate_failure_wraps_errors_0_in_with_state_context() {
        // Regression for F6: when the saga adapter falls back to
        // `run_inline_compensate` (Split return or non-serializable Output)
        // and the inline compensate ALSO fails, the resulting
        // `CompensationFailed` was previously surfaced with a bare
        // `TaskExecution` at `errors[0]` — violating the documented
        // invariant that "errors[0] is the original failure as wrapped by
        // the FSM in WithStateContext". The constructor now wraps
        // `errors[0]` so the invariant holds regardless of which path
        // produced the aggregate.
        use serde::{Deserialize, Serialize, Serializer};

        #[derive(Debug, Deserialize)]
        struct NeverSerialize;
        impl Serialize for NeverSerialize {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional"))
            }
        }

        #[derive(Clone)]
        struct InlineFailingCompensate;
        #[saga::task]
        impl CompensatableTask<TestState> for InlineFailingCompensate {
            type Output = NeverSerialize;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, NeverSerialize), CanoError> {
                Ok((TaskResult::Single(TestState::Complete), NeverSerialize))
            }
            async fn compensate(
                &self,
                _res: &Resources,
                _o: NeverSerialize,
            ) -> Result<(), CanoError> {
                Err(CanoError::task_execution("compensate failed too"))
            }
        }

        let workflow = Workflow::bare()
            .register_with_compensation(TestState::Start, InlineFailingCompensate)
            .add_exit_state(TestState::Complete);
        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        let errors = match err {
            CanoError::CompensationFailed { errors } => errors,
            other => panic!("expected CompensationFailed, got: {other:?}"),
        };
        assert_eq!(errors.len(), 2, "got: {errors:?}");
        match &errors[0] {
            CanoError::WithStateContext {
                state,
                attempt: _,
                transitions_so_far,
                source,
            } => {
                assert_eq!(state, "Start");
                assert!(transitions_so_far.contains(&"Start".to_string()));
                assert!(
                    source.message().contains("serialize"),
                    "wrapped source must still surface the serialize error: {source:?}"
                );
            }
            other => panic!(
                "errors[0] must be WithStateContext per the documented invariant; got: {other:?}"
            ),
        }
        // errors[1] is the inline-compensate failure — left bare, since the
        // wrap only annotates the original failure that triggered rollback.
        assert!(
            errors[1].message().contains("compensate failed too"),
            "errors[1] should still be the compensate failure: {:?}",
            errors[1]
        );
    }

    #[tokio::test]
    async fn compensation_failed_constructor_flattens_nested() {
        // The constructor flattens nested CompensationFailed so the drain
        // never produces a `CompensationFailed { errors: [CompensationFailed { ... }, ...] }`.
        let nested = CanoError::compensation_failed(vec![
            CanoError::task_execution("a"),
            CanoError::task_execution("b"),
        ]);
        let outer = CanoError::compensation_failed(vec![nested, CanoError::task_execution("c")]);
        match outer {
            CanoError::CompensationFailed { errors } => {
                assert_eq!(errors.len(), 3);
                assert_eq!(errors[0].message(), "a");
                assert_eq!(errors[1].message(), "b");
                assert_eq!(errors[2].message(), "c");
            }
            other => panic!("expected CompensationFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn compensatable_split_return_triggers_inline_compensate() {
        // A CompensatableTask returning Split is rejected by the adapter BEFORE
        // serialize, and the side effect committed in `run` is rolled back via
        // the inline-compensate path — mirroring the serialize-failure case.
        use std::sync::atomic::{AtomicBool, Ordering};

        type Effect = Arc<Mutex<Vec<&'static str>>>;

        #[derive(Clone)]
        struct LeakySplit {
            log: Effect,
            charged: Arc<AtomicBool>,
        }
        #[saga::task]
        impl CompensatableTask<TestState> for LeakySplit {
            type Output = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, u32), CanoError> {
                self.log.lock().unwrap().push("charged");
                self.charged.store(true, Ordering::SeqCst);
                // Misuse: a saga task must never return Split. Pre-fix the
                // FSM rejected this *after* the run already committed, leaking
                // the side effect. Now the adapter rejects it AND calls
                // compensate inline.
                Ok((TaskResult::Split(vec![TestState::Process]), 7))
            }
            async fn compensate(&self, _res: &Resources, _o: u32) -> Result<(), CanoError> {
                self.log.lock().unwrap().push("refunded");
                Ok(())
            }
        }

        let log: Effect = Arc::new(Mutex::new(Vec::new()));
        let charged = Arc::new(AtomicBool::new(false));
        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                LeakySplit {
                    log: log.clone(),
                    charged: charged.clone(),
                },
            )
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        assert!(charged.load(Ordering::SeqCst));
        // The error mentions the split rejection.
        assert!(err.message().contains("split"), "got: {err}");
        // Crucially: compensate ran inline → side effect undone.
        assert_eq!(
            *log.lock().unwrap(),
            vec!["charged", "refunded"],
            "Split-return must trigger inline compensate, not leak the side effect"
        );
    }

    #[tokio::test]
    async fn compensatable_serialize_failure_runs_inline_compensate() {
        // When the adapter cannot serialize `Output` into a checkpoint blob,
        // the forward `run` has already executed and any committed side effect
        // is on the wire. The adapter therefore calls `compensate(output)`
        // inline with the typed value before surfacing the failure, so the
        // rollback still happens even when persistence isn't possible.
        use serde::{Deserialize, Serialize, Serializer};

        // A type whose Serialize impl ALWAYS errors. serde_json::to_vec
        // therefore cannot persist a CompensatableTask whose Output is this.
        #[derive(Debug, Deserialize)]
        struct NeverSerialize;
        impl Serialize for NeverSerialize {
            fn serialize<S: Serializer>(&self, _ser: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional serialize failure"))
            }
        }

        type SideEffect = Arc<Mutex<Vec<&'static str>>>;

        #[derive(Clone)]
        struct LeakyCharge {
            log: SideEffect,
        }
        #[saga::task]
        impl CompensatableTask<TestState> for LeakyCharge {
            type Output = NeverSerialize;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            fn name(&self) -> Cow<'static, str> {
                Cow::Borrowed("LeakyCharge")
            }
            async fn run(
                &self,
                _res: &Resources,
            ) -> Result<(TaskResult<TestState>, NeverSerialize), CanoError> {
                // Simulated commit of an external side effect.
                self.log.lock().unwrap().push("charged");
                Ok((TaskResult::Single(TestState::Complete), NeverSerialize))
            }
            async fn compensate(
                &self,
                _res: &Resources,
                _output: NeverSerialize,
            ) -> Result<(), CanoError> {
                self.log.lock().unwrap().push("refunded");
                Ok(())
            }
        }

        let log: SideEffect = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::bare()
            .register_with_compensation(TestState::Start, LeakyCharge { log: log.clone() })
            .add_exit_state(TestState::Complete);

        let err = workflow.orchestrate(TestState::Start).await.unwrap_err();
        // The serialize error is what surfaces (wrapped with state context).
        assert!(err.message().contains("serialize"), "got: {err}");

        // Crucial: compensate ran inline — the side effect is undone even
        // though it could not be persisted for crash-recovery rollback.
        assert_eq!(
            *log.lock().unwrap(),
            vec!["charged", "refunded"],
            "compensate must run inline when serialize fails"
        );
    }

    #[tokio::test]
    async fn resume_from_setup_failure_leaves_checkpoint_log_untouched() {
        // `setup_all` runs before any rehydration work, so a setup failure
        // surfaces the resource error without firing `on_resume` and without
        // silently dropping the rehydratable compensation stack.
        use crate::resource::{Resource, Resources};
        use std::sync::atomic::{AtomicBool, Ordering};

        struct FailingResource {
            triggered: Arc<AtomicBool>,
        }
        #[crate::resource]
        impl Resource for FailingResource {
            async fn setup(&self) -> Result<(), CanoError> {
                self.triggered.store(true, Ordering::SeqCst);
                Err(CanoError::generic("db unavailable"))
            }
        }

        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(MemCheckpoints::default());
        // Pre-existing CompCompletion row so there's something to rehydrate.
        store
            .append(
                "f3",
                CheckpointRow::new(0, "Start", "A").with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "f3",
                CheckpointRow::new(1, "Start", "A")
                    .with_output(serde_json::to_vec(&1u32).unwrap())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "f3",
                CheckpointRow::new(2, "Process", "B").with_workflow_version(0),
            )
            .await
            .unwrap();
        let rows_before = store.audit_rows("f3").len();

        let triggered = Arc::new(AtomicBool::new(false));
        let resources = Resources::new().insert(
            "db",
            FailingResource {
                triggered: triggered.clone(),
            },
        );

        // Observer that records every on_resume call.
        let (obs, rec) = EventLog::new();

        let workflow = Workflow::new(resources)
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register(TestState::Process, FailTask::new(true))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_observer(Arc::new(obs));

        let err = workflow.resume_from("f3").await.unwrap_err();
        // Setup ran and failed.
        assert!(triggered.load(Ordering::SeqCst));
        // The resource error surfaces (not wrapped in WithStateContext, not
        // mistaken for a CompensationFailed).
        assert!(err.message().contains("db unavailable"), "got: {err}");
        // The checkpoint log is unchanged — no new rows appended.
        assert_eq!(
            store.audit_rows("f3").len(),
            rows_before,
            "checkpoint log must be untouched on setup failure"
        );
        // on_resume must NOT have fired — the resume didn't actually start.
        assert!(
            rec.resumes().is_empty(),
            "on_resume must not fire when setup_all fails"
        );
    }

    #[tokio::test]
    async fn orphan_compensator_surfaces_structured_error_with_blob() {
        // A missing compensator must surface as the structured
        // `OrphanedCompensation { task_id, output_blob }` variant so operators
        // can match on it and decode the blob for manual recovery — a bare
        // string error would lose the data needed to undo the side effect.
        let store = Arc::new(MemCheckpoints::default());
        // Build a log: CompCompletion for a renamed task `ghost_v1`, then a
        // StateEntry for the resume point whose task fails forward.
        let blob = serde_json::to_vec(&99u32).unwrap();
        store
            .append(
                "orphan",
                CheckpointRow::new(0, "Start", "ghost_v1").with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "orphan",
                CheckpointRow::new(1, "Start", "ghost_v1")
                    .with_output(blob.clone())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();
        // The resume row: a StateEntry for `Process` whose task always fails.
        store
            .append(
                "orphan",
                CheckpointRow::new(2, "Process", "FailTask").with_workflow_version(0),
            )
            .await
            .unwrap();

        // Workflow with no `ghost_v1` compensator — only `Process` which fails
        // forward, triggering a drain over the orphan ghost entry.
        let workflow = Workflow::bare()
            .register(TestState::Process, FailTask::new(true))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let err = workflow.resume_from("orphan").await.unwrap_err();
        // The drain produced two errors (original + orphan) → CompensationFailed.
        let inner = match &err {
            CanoError::CompensationFailed { errors } => errors.clone(),
            other => panic!("expected CompensationFailed, got: {other:?}"),
        };
        assert!(inner.len() >= 2, "expected at least 2 errors: {inner:?}");
        // The second error must be the structured orphan variant, with the
        // blob intact for the operator.
        match &inner[1] {
            CanoError::OrphanedCompensation {
                task_id,
                output_blob,
            } => {
                assert_eq!(task_id.as_ref(), "ghost_v1");
                assert_eq!(output_blob, &blob);
                // Round-trip decode to confirm the operator can recover.
                let decoded: u32 = serde_json::from_slice(output_blob).unwrap();
                assert_eq!(decoded, 99);
            }
            other => panic!("expected OrphanedCompensation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_from_does_not_duplicate_resume_state_in_path() {
        // The resume state must appear exactly once in `transitions_so_far`
        // even when the crash row is a `CompensationCompletion` (or
        // `StepCursor`): its own `StateEntry` lives at an earlier sequence
        // than the crash row, so a naive `sequence != resume_sequence` filter
        // would keep it AND the FSM would re-push it when the state re-runs.
        let log: CompLog = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(MemCheckpoints::default());
        store
            .append(
                "f9",
                CheckpointRow::new(0, "Start", "A").with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "f9",
                CheckpointRow::new(1, "Start", "A")
                    .with_output(serde_json::to_vec(&1u32).unwrap())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "f9",
                CheckpointRow::new(2, "Process", "B").with_workflow_version(0),
            )
            .await
            .unwrap();
        // Highest sequence is a CompCompletion row whose `state` matches the
        // seq-2 StateEntry — this is the case that exercises the duplicate path.
        store
            .append(
                "f9",
                CheckpointRow::new(3, "Process", "B")
                    .with_output(serde_json::to_vec(&2u32).unwrap())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();

        let workflow = Workflow::bare()
            .register_with_compensation(
                TestState::Start,
                CompTask::ok("A", 1, TestState::Process, &log),
            )
            .register_with_compensation(
                TestState::Process,
                CompTask::ok("B", 2, TestState::Error, &log),
            )
            .register(TestState::Error, FailTask::new(true))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let err = workflow.resume_from("f9").await.unwrap_err();
        // The drain ran two compensators (clean rollback) so the surfaced
        // error is just the original wrapped failure.
        let ctx = match &err {
            CanoError::WithStateContext {
                transitions_so_far,
                state,
                ..
            } => (state.clone(), transitions_so_far.clone()),
            other => panic!("expected WithStateContext, got: {other:?}"),
        };
        assert_eq!(ctx.0, "Error");
        // Expect [Start, Process, Error] — Process must appear exactly once
        // even though the rehydrated log contains both its StateEntry and a
        // CompensationCompletion at a higher sequence.
        assert_eq!(
            ctx.1,
            vec![
                "Start".to_string(),
                "Process".to_string(),
                "Error".to_string()
            ],
            "resume state must appear exactly once in transitions_so_far"
        );
    }

    #[tokio::test]
    async fn unknown_state_label_in_log_fires_observer_hook_during_resume() {
        // Regression for F11: previously `prior_transitions` rehydration used
        // `filter_map(state_from_label)` which silently dropped any row whose
        // label was no longer registered on the workflow (typical after a
        // rename without a `with_workflow_version` bump). Operators couldn't
        // see the gap. Now the engine fires `on_unknown_resume_state` per
        // unrecognized row so observers/metrics can surface the drift.
        let store = Arc::new(MemCheckpoints::default());
        // Pre-populate the log with a StateEntry referencing a state ("Charge")
        // the current workflow no longer registers, plus a registered state
        // ("Start") whose label IS valid. The resume must drop the Charge row
        // from the rehydrated path AND fire the observer hook for it.
        store
            .append(
                "f-rename",
                CheckpointRow::new(0, "Start", "S").with_workflow_version(0),
            )
            .await
            .unwrap();
        store
            .append(
                "f-rename",
                CheckpointRow::new(1, "Charge", "C").with_workflow_version(0),
            )
            .await
            .unwrap();
        // Resume row — the workflow re-enters this state. `Process` is the
        // resume label and is registered (it'll re-run and fail).
        store
            .append(
                "f-rename",
                CheckpointRow::new(2, "Process", "P").with_workflow_version(0),
            )
            .await
            .unwrap();

        let (obs, rec) = EventLog::new();
        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, FailTask::new(true))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_observer(Arc::new(obs));

        // Resume succeeds (label dropped from path; gap surfaced via observer).
        let _ = workflow.resume_from("f-rename").await;
        let recorded = rec.unknown_states();
        assert_eq!(
            recorded.len(),
            1,
            "exactly one unknown-state row should have fired the hook"
        );
        assert_eq!(recorded[0].0, "f-rename");
        assert_eq!(recorded[0].1, 1);
        assert_eq!(recorded[0].2, "Charge");
    }

    #[tokio::test]
    async fn resume_from_runs_teardown_on_every_post_setup_error_path() {
        // After setup_all succeeds, every early-return between rehydration
        // and execute_workflow_from must still run teardown. Without the
        // inner-fn refactor, a load_run failure or no-rows error leaked the
        // setup. Verify by counting setup/teardown calls.
        use crate::resource::{Resource, Resources};
        use std::sync::atomic::{AtomicU32, Ordering};

        #[derive(Default)]
        struct CountingResource {
            setups: Arc<AtomicU32>,
            teardowns: Arc<AtomicU32>,
        }
        #[crate::resource]
        impl Resource for CountingResource {
            async fn setup(&self) -> Result<(), CanoError> {
                self.setups.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            async fn teardown(&self) -> Result<(), CanoError> {
                self.teardowns.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        // Build a workflow with the counting resource attached, no checkpoint
        // store rows for the chosen id — `load_run` returns Ok(vec![]) and the
        // empty-rows check fails post-setup.
        let setups = Arc::new(AtomicU32::new(0));
        let teardowns = Arc::new(AtomicU32::new(0));
        let resources = Resources::new().insert(
            "counter",
            CountingResource {
                setups: setups.clone(),
                teardowns: teardowns.clone(),
            },
        );
        let store = Arc::new(MemCheckpoints::default());
        let workflow = Workflow::new(resources)
            .register(TestState::Start, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        // Case 1: no rows for the id — `load_run` returns empty.
        let err = workflow.resume_from("never-existed").await.unwrap_err();
        assert!(err.message().contains("no checkpoint rows"), "got: {err}");
        assert_eq!(setups.load(Ordering::SeqCst), 1, "setup must have run");
        assert_eq!(
            teardowns.load(Ordering::SeqCst),
            1,
            "teardown must run on empty-rows error path"
        );

        // Case 2: version mismatch — every-row version check rejects.
        store
            .append(
                "ver-mismatch",
                CheckpointRow::new(0, "Start", "x").with_workflow_version(42),
            )
            .await
            .unwrap();
        let err = workflow.resume_from("ver-mismatch").await.unwrap_err();
        assert!(matches!(err, CanoError::WorkflowVersionMismatch { .. }));
        assert_eq!(setups.load(Ordering::SeqCst), 2);
        assert_eq!(
            teardowns.load(Ordering::SeqCst),
            2,
            "teardown must run on version-mismatch error path"
        );

        // Case 3: state label doesn't map.
        store
            .append(
                "bad-label",
                CheckpointRow::new(0, "Unknown", "x").with_workflow_version(0),
            )
            .await
            .unwrap();
        let err = workflow.resume_from("bad-label").await.unwrap_err();
        assert!(err.message().contains("is not a registered or exit state"));
        assert_eq!(setups.load(Ordering::SeqCst), 3);
        assert_eq!(
            teardowns.load(Ordering::SeqCst),
            3,
            "teardown must run on unknown-state error path"
        );
    }

    #[tokio::test]
    async fn resume_from_falls_back_when_missing_resume_state_entry() {
        // F4 follow-up: previously `resume_from` *rejected* logs whose resume
        // row was a `CompensationCompletion`/`StepCursor` without a preceding
        // `StateEntry` for the same label (a backend that batched/coalesced
        // writes, or an older log written before the engine wrote
        // `StateEntry` rows for every non-router state). The engine now falls
        // back to `last.sequence + 1` as cutoff and lets the resume succeed
        // — losing the prior_transitions row is acceptable; refusing to
        // resume recoverable work is not.
        let store = Arc::new(MemCheckpoints::default());
        store
            .append(
                "missing-entry",
                CheckpointRow::new(0, "Start", "A").with_workflow_version(0),
            )
            .await
            .unwrap();
        // Resume row is a CompensationCompletion for `Process` with NO
        // StateEntry for `Process`. The pre-fix code refused to resume; the
        // post-fix code resumes (the absent prior `Process` row is just a
        // best-effort gap in the rehydrated path).
        store
            .append(
                "missing-entry",
                CheckpointRow::new(1, "Process", "B")
                    .with_output(serde_json::to_vec(&42u32).unwrap())
                    .with_workflow_version(0),
            )
            .await
            .unwrap();

        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone());

        let out = workflow
            .resume_from("missing-entry")
            .await
            .expect("resume should fall back rather than refuse on missing StateEntry");
        assert_eq!(out, TestState::Complete);
    }

    #[tokio::test]
    async fn resume_from_accepts_mixed_version_log_with_matching_tail() {
        // F3 follow-up: only the last row's `workflow_version` is
        // authoritative — that determines which workflow definition the
        // resume continues against. Older rows in a mixed-version log are
        // kept (with a tracing warning) so a mid-run version bump can resume
        // cleanly. Rejecting on a per-row scan refused exactly the migration
        // path `workflow_version` was meant to support.
        let store = Arc::new(MemCheckpoints::default());
        store
            .append(
                "mixed-ver",
                CheckpointRow::new(0, "Start", "A").with_workflow_version(1), // older version
            )
            .await
            .unwrap();
        store
            .append(
                "mixed-ver",
                CheckpointRow::new(1, "Process", "B").with_workflow_version(2), // bumped after
            )
            .await
            .unwrap();

        let workflow = Workflow::bare()
            .register(TestState::Start, SimpleTask::new(TestState::Process))
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_version(2);

        let out = workflow
            .resume_from("mixed-ver")
            .await
            .expect("post-F3, mixed-version log with matching tail must resume");
        assert_eq!(out, TestState::Complete);
    }
}

/// Edge-case unit tests for `RehydratedRun::from_rows`. Integration sentinels
/// (`resume_from_does_not_duplicate_resume_state_in_path`,
/// `resume_from_falls_back_when_missing_resume_state_entry`,
/// `resume_from_accepts_mixed_version_log_with_matching_tail`,
/// `unknown_state_label_in_log_fires_observer_hook_during_resume`,
/// `double_compensate_on_resume_is_avoided`) cover the builder end-to-end
/// through `execute_resume_inner`; this module pins down the boundary
/// behavior in isolation.
#[cfg(test)]
mod rehydrated_run_tests {
    use super::*;
    use crate::observer::WorkflowObserver;
    use crate::recovery::CheckpointRow;
    use crate::workflow::test_support::{EventLog, TestState};

    /// Resolver that matches stored labels back to `TestState`. Returns None
    /// for any label not in this table — used to exercise the F11 fan-out.
    fn resolve_test_state(label: &str) -> Option<TestState> {
        match label {
            "Start" => Some(TestState::Start),
            "Process" => Some(TestState::Process),
            "Split" => Some(TestState::Split),
            "Join" => Some(TestState::Join),
            "Complete" => Some(TestState::Complete),
            "Error" => Some(TestState::Error),
            _ => None,
        }
    }

    fn no_observers() -> [Arc<dyn WorkflowObserver>; 0] {
        []
    }

    #[test]
    fn empty_rows_yields_default_struct() {
        // Caller errors before reaching the builder when `rows` is empty
        // (`last.ok_or_else` fires first), but the builder itself must not
        // panic if called with an empty slice and must produce default state.
        let rh = RehydratedRun::<TestState>::from_rows(
            &[],
            "Start",
            0,
            0,
            &no_observers(),
            "wf",
            resolve_test_state,
        );
        assert!(rh.compensation_stack.is_empty());
        assert!(rh.resume_cursors.is_empty());
        assert!(rh.prior_transitions.is_empty());
        // F4 fallback: resume_sequence + 1 when no StateEntry rows exist.
        assert_eq!(rh.resume_state_entry_cutoff, 1);
    }

    #[test]
    fn single_state_entry_matching_resume_label_has_empty_prior_transitions() {
        // The cutoff equals that row's sequence; nothing else is in the
        // log, so prior_transitions is empty.
        let rows = vec![CheckpointRow::new(5, "Start", "TaskA").with_workflow_version(0)];
        let rh = RehydratedRun::<TestState>::from_rows(
            &rows,
            "Start",
            5,
            0,
            &no_observers(),
            "wf",
            resolve_test_state,
        );
        assert_eq!(rh.resume_state_entry_cutoff, 5);
        assert!(rh.prior_transitions.is_empty());
        assert!(rh.compensation_stack.is_empty());
        assert!(rh.resume_cursors.is_empty());
    }

    #[test]
    fn missing_state_entry_falls_back_to_resume_sequence_plus_one() {
        // F4: when the resume label has no StateEntry row, cutoff falls back
        // to `resume_sequence + 1` so all earlier rows count as "pre-crash".
        let rows = vec![
            CheckpointRow::new(0, "Start", "A").with_workflow_version(0),
            // Resume row is a StepCursor for `Process` with NO preceding
            // StateEntry for `Process`.
            CheckpointRow::new(1, "Process", "B")
                .with_cursor(b"cursor".to_vec())
                .with_workflow_version(0),
        ];
        let rh = RehydratedRun::<TestState>::from_rows(
            &rows,
            "Process",
            1,
            0,
            &no_observers(),
            "wf",
            resolve_test_state,
        );
        assert_eq!(rh.resume_state_entry_cutoff, 2);
        // The Start StateEntry survives (sequence 0 < cutoff 2).
        assert_eq!(rh.prior_transitions, vec![TestState::Start]);
        // The Process step cursor was rehydrated.
        assert_eq!(rh.resume_cursors.get("Process"), Some(&b"cursor".to_vec()));
    }

    #[test]
    fn mixed_version_rows_pass_through_when_caller_already_validated_last() {
        // F3: per-row workflow_version is logged but not enforced — that's
        // the caller's responsibility (it validates `last.workflow_version`
        // before invoking the builder). Older rows survive.
        let rows = vec![
            CheckpointRow::new(0, "Start", "A").with_workflow_version(0), // older
            CheckpointRow::new(1, "Process", "B").with_workflow_version(1), // matches
        ];
        let rh = RehydratedRun::<TestState>::from_rows(
            &rows,
            "Process",
            1,
            1,
            &no_observers(),
            "wf",
            resolve_test_state,
        );
        assert_eq!(rh.resume_state_entry_cutoff, 1);
        assert_eq!(rh.prior_transitions, vec![TestState::Start]);
    }

    #[test]
    fn unknown_state_label_fires_observer_and_drops_row() {
        // F11: a label that doesn't resolve (e.g. renamed state) is dropped
        // from prior_transitions and the observer hook fires per drop.
        let rows = vec![
            CheckpointRow::new(0, "Start", "A").with_workflow_version(0),
            // "Charge" no longer maps to any current state.
            CheckpointRow::new(1, "Charge", "B").with_workflow_version(0),
            CheckpointRow::new(2, "Process", "C").with_workflow_version(0),
        ];
        let (obs, recorder) = EventLog::new();
        let observer_dyn: Arc<dyn WorkflowObserver> = Arc::new(obs);
        let observers = [observer_dyn];
        let rh = RehydratedRun::<TestState>::from_rows(
            &rows,
            "Process",
            2,
            0,
            &observers,
            "wf-rename",
            resolve_test_state,
        );
        assert_eq!(rh.resume_state_entry_cutoff, 2);
        // Only Start made it (Charge dropped, Process is the resume state).
        assert_eq!(rh.prior_transitions, vec![TestState::Start]);
        let events = recorder.unknown_states();
        assert_eq!(events.len(), 1, "Charge row must fire the hook once");
        assert_eq!(
            events[0],
            ("wf-rename".to_string(), 1, "Charge".to_string())
        );
    }

    #[test]
    fn multiple_step_cursors_for_same_state_keep_highest_sequence() {
        // The fold uses last-write-wins; ascending order guarantees the
        // highest-sequence cursor for each state survives.
        let rows = vec![
            CheckpointRow::new(0, "Start", "S")
                .with_cursor(b"old".to_vec())
                .with_workflow_version(0),
            CheckpointRow::new(1, "Start", "S")
                .with_cursor(b"newer".to_vec())
                .with_workflow_version(0),
            CheckpointRow::new(2, "Start", "S")
                .with_cursor(b"newest".to_vec())
                .with_workflow_version(0),
        ];
        let rh = RehydratedRun::<TestState>::from_rows(
            &rows,
            "Start",
            2,
            0,
            &no_observers(),
            "wf",
            resolve_test_state,
        );
        assert_eq!(rh.resume_cursors.get("Start"), Some(&b"newest".to_vec()));
    }

    #[test]
    fn compensation_at_resume_sequence_is_excluded() {
        // Pairs with `double_compensate_on_resume_is_avoided` — the resume
        // row itself, when a CompensationCompletion, must NOT make it into
        // the rehydrated compensation_stack (the resumed task re-runs and
        // re-pushes its own entry, so keeping the persisted one would
        // double-compensate later).
        let rows = vec![
            CheckpointRow::new(0, "Start", "A").with_workflow_version(0),
            CheckpointRow::new(1, "Start", "A")
                .with_output(serde_json::to_vec(&1u32).unwrap())
                .with_workflow_version(0),
            CheckpointRow::new(2, "Process", "B").with_workflow_version(0),
            // This is the resume row at sequence 3 — must be excluded.
            CheckpointRow::new(3, "Process", "B")
                .with_output(serde_json::to_vec(&2u32).unwrap())
                .with_workflow_version(0),
        ];
        let rh = RehydratedRun::<TestState>::from_rows(
            &rows,
            "Process",
            3,
            0,
            &no_observers(),
            "wf",
            resolve_test_state,
        );
        // Only Start's comp completion at sequence 1 must be on the stack;
        // Process's at sequence 3 (== resume_sequence) is excluded.
        assert_eq!(rh.compensation_stack.len(), 1);
        assert_eq!(&*rh.compensation_stack[0].task_id, "A");
        // Path cutoff = highest Process StateEntry = sequence 2; prior
        // transitions are Start (sequence 0). Sequence 2 itself (the
        // Process StateEntry) sits AT the cutoff so is excluded too.
        assert_eq!(rh.resume_state_entry_cutoff, 2);
        assert_eq!(rh.prior_transitions, vec![TestState::Start]);
    }

    #[test]
    fn perf_smoke_ten_thousand_rows_finishes_quickly() {
        // Sanity check that the single-pass builder scales linearly.
        let mut rows = Vec::with_capacity(10_000);
        for i in 0..10_000u64 {
            // Alternate state labels so we exercise multiple branches.
            let label = if i % 2 == 0 { "Start" } else { "Process" };
            rows.push(CheckpointRow::new(i, label, "T").with_workflow_version(0));
        }
        let started = std::time::Instant::now();
        let rh = RehydratedRun::<TestState>::from_rows(
            &rows,
            "Process",
            9_999,
            0,
            &no_observers(),
            "wf",
            resolve_test_state,
        );
        let elapsed = started.elapsed();
        // Cutoff = the highest Process StateEntry = sequence 9999.
        assert_eq!(rh.resume_state_entry_cutoff, 9_999);
        // prior_transitions = every row at seq < 9999 → 9999 entries.
        assert_eq!(rh.prior_transitions.len(), 9_999);
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "10k-row rehydration should be sub-50ms even in a debug build, took {elapsed:?}"
        );
    }
}

#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use crate::metrics::test_support::*;
    use crate::prelude::*;
    use crate::task::TaskConfig;
    use crate::workflow::test_support::{CompLog, CompTask, MemCheckpoints, SimpleTask, TestState};
    use std::sync::{Arc, Mutex};

    // ---- Recovery: checkpoint append + clear counters ----

    #[test]
    fn checkpoint_append_and_clear_counters_on_successful_run() {
        let (res, rows) = run_with_recorder(|| async {
            let store = Arc::new(MemCheckpoints::default());
            let workflow = Workflow::bare()
                .with_checkpoint_store(store.clone())
                .with_workflow_id("metrics-wf")
                .register(TestState::Start, SimpleTask::new(TestState::Process))
                .register(TestState::Process, SimpleTask::new(TestState::Complete))
                .add_exit_state(TestState::Complete);
            workflow.orchestrate(TestState::Start).await
        });
        assert_eq!(res.unwrap(), TestState::Complete);
        // At minimum one append per state entered (Start, Process, Complete = 3 rows)
        assert!(
            counter(&rows, "cano_checkpoint_appends_total", &[("result", "ok")]) >= 1,
            "expected at least one successful checkpoint append"
        );
        // A successful run clears its log exactly once
        assert_eq!(
            counter(&rows, "cano_checkpoint_clears_total", &[("result", "ok")]),
            1,
            "expected exactly one checkpoint clear on successful run"
        );
    }

    // ---- Saga: compensation run + drain counters ----

    // `CompLog` + `CompTask` live in `workflow::test_support` and are imported
    // above. Pre-consolidation this module defined its own simpler `CompTask`
    // (no `name` / `fail_*` fields); now it uses the shared fixture with
    // `name = "comp"` to preserve the prior log entries.

    struct AlwaysFailTask;

    #[crate::task]
    impl Task<TestState> for AlwaysFailTask {
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        async fn run_bare(&self) -> Result<TaskResult<TestState>, CanoError> {
            Err(CanoError::task_execution(
                "intentional failure for saga test",
            ))
        }
    }

    #[test]
    fn compensation_run_and_drain_counters_on_clean_rollback() {
        let (res, rows) = run_with_recorder(|| async {
            let log: CompLog = Arc::new(Mutex::new(Vec::new()));
            // CompensatableTask succeeds (Start→Process), then AlwaysFailTask fails.
            // This triggers a LIFO drain with one compensate() call which succeeds.
            let workflow = Workflow::bare()
                .register_with_compensation(
                    TestState::Start,
                    CompTask::ok("comp", 42, TestState::Process, &log),
                )
                .register(TestState::Process, AlwaysFailTask)
                .add_exit_state(TestState::Complete);
            workflow.orchestrate(TestState::Start).await
        });
        // Clean rollback: the original error is returned (not CompensationFailed)
        assert!(res.is_err(), "expected workflow to fail");
        // compensate() was called once and succeeded
        assert_eq!(
            counter(&rows, "cano_compensations_run_total", &[("result", "ok")]),
            1,
            "expected one successful compensate() call"
        );
        // The drain was clean (all compensations ok)
        assert_eq!(
            counter(
                &rows,
                "cano_compensation_drains_total",
                &[("outcome", "clean")]
            ),
            1,
            "expected a clean compensation drain"
        );
    }
}
