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

use crate::error::CanoError;
use crate::recovery::RowKind;
use crate::saga::{CompensationEntry, ErasedCompensatable};
use crate::task::{TaskResult, run_with_retries};

use super::{Workflow, notify_observers, panic_payload_message};

#[cfg(feature = "tracing")]
use tracing::{debug, info, info_span};

impl<TState, TResourceKey> Workflow<TState, TResourceKey>
where
    TState: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Drain the compensation stack (LIFO) after a terminal workflow failure.
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
        mut stack: Vec<CompensationEntry>,
        original: CanoError,
    ) -> Result<TState, CanoError> {
        if stack.is_empty() {
            return Err(original);
        }
        let mut errors = vec![original];
        while let Some(entry) = stack.pop() {
            match self.compensators.get(&entry.task_id) {
                None => errors.push(CanoError::workflow(format!(
                    "no compensator registered for task {:?} — cannot roll it back",
                    entry.task_id
                ))),
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
                    let bounded = async {
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
                    match AssertUnwindSafe(bounded).catch_unwind().await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(task_id = %entry.task_id, error = %e, "compensation failed");
                            errors.push(e);
                        }
                        Err(payload) => {
                            let msg = panic_payload_message(&*payload);
                            #[cfg(feature = "tracing")]
                            tracing::error!(task_id = %entry.task_id, panic = %msg, "compensation panicked");
                            errors.push(CanoError::task_execution(format!(
                                "compensate for {:?} panicked: {msg}",
                                entry.task_id
                            )));
                        }
                    }
                }
            }
        }
        if errors.len() == 1 {
            // Clean rollback — clear the log (best-effort), surface the original error.
            self.clear_checkpoint_log(workflow_id).await;
            Err(errors
                .into_iter()
                .next()
                .expect("errors has exactly one element"))
        } else {
            Err(CanoError::compensation_failed(errors))
        }
    }

    /// Best-effort `clear` on the checkpoint store: a successful run, or a run that was
    /// fully rolled back, shouldn't leave a recovery log behind — but a hiccup cleaning
    /// it up shouldn't turn the run's result into an error, so a `clear` failure is logged
    /// and swallowed. No-op when no store or no workflow id is in play.
    pub(super) async fn clear_checkpoint_log(&self, workflow_id: Option<&str>) {
        let Some((store, wf_id)) = self.checkpoint_store.as_ref().zip(workflow_id) else {
            return;
        };
        #[cfg(feature = "tracing")]
        if let Err(e) = store.clear(wf_id).await {
            tracing::warn!(workflow_id = %wf_id, error = %e, "failed to clear checkpoint log");
        }
        #[cfg(not(feature = "tracing"))]
        let _ = store.clear(wf_id).await;
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
        let unwind_result = {
            let _enter = task_span.enter();
            AssertUnwindSafe(run_future).catch_unwind().await
        };
        #[cfg(not(feature = "tracing"))]
        let unwind_result = AssertUnwindSafe(run_future).catch_unwind().await;

        let result: Result<(TaskResult<TState>, Vec<u8>), CanoError> = match unwind_result {
            Ok(inner) => inner,
            Err(payload) => {
                let payload_str = panic_payload_message(&*payload);
                #[cfg(feature = "tracing")]
                tracing::error!(panic = %payload_str, "Compensatable task panicked");
                Err(CanoError::task_execution(format!("panic: {payload_str}")))
            }
        };

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
    /// [`RowKind::CompensationCompletion`](crate::recovery::RowKind::CompensationCompletion)
    /// becomes a stack entry (in sequence order), so a failure after the resume point can
    /// still compensate work done before the crash. Rows with other kinds — ordinary
    /// [`RowKind::StateEntry`](crate::recovery::RowKind::StateEntry) rows and
    /// [`RowKind::StepCursor`](crate::recovery::RowKind::StepCursor) rows — are ignored
    /// by the rehydration.
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
    pub async fn resume_from(&self, workflow_id: impl Into<String>) -> Result<TState, CanoError> {
        let workflow_id = workflow_id.into();

        #[cfg(feature = "tracing")]
        let workflow_span = self.tracing_span.clone().unwrap_or_else(|| {
            if tracing::enabled!(tracing::Level::INFO) {
                info_span!("workflow_resume")
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

        let rows = store.load_run(&workflow_id).await.map_err(|e| {
            CanoError::checkpoint_store(format!("load checkpoint run {workflow_id:?}: {e}"))
        })?;
        let last = rows.iter().max_by_key(|r| r.sequence).ok_or_else(|| {
            CanoError::checkpoint_store(format!(
                "no checkpoint rows for workflow id {workflow_id:?}"
            ))
        })?;
        let resume_state = self.state_from_label(&last.state).ok_or_else(|| {
            CanoError::workflow(format!(
                "checkpoint state {:?} is not a registered or exit state of this workflow",
                last.state
            ))
        })?;
        let start_sequence = last.sequence + 1;

        // Rehydrate the compensation stack from CompensationCompletion rows. `rows` is
        // ascending by sequence (the `load_run` contract), so the stack is in the right
        // order for a LIFO drain. Rows with other kinds — StateEntry and StepCursor —
        // are ignored; a StepCursor row also carries an output_blob but must not be
        // mistaken for a compensation entry.
        //
        // Skip the resume point's own row: if the crash landed after a compensatable
        // state's completion row but before the next state's entry row, that state is the
        // resume point — it re-runs below and re-pushes its own compensation entry, so
        // keeping the persisted one too would compensate that step twice on a later
        // failure. Earlier compensatable states' rows are kept (their tasks don't re-run).
        let resume_sequence = last.sequence;
        let compensation_stack: Vec<CompensationEntry> = rows
            .iter()
            .filter(|r| r.sequence != resume_sequence)
            .filter(|r| r.kind == RowKind::CompensationCompletion)
            .filter_map(|r| {
                r.output_blob.as_ref().map(|blob| CompensationEntry {
                    task_id: r.task_id.clone(),
                    output_blob: blob.clone(),
                })
            })
            .collect();

        // Collect the latest StepCursor row per state. `rows` is ascending by sequence,
        // so a simple fold keeps the last (highest-sequence) cursor for each state label.
        // The resume-sequence filter is intentionally NOT applied here: the StepCursor rows
        // all belong to Stepped states, not to the resume point's own state entry, so no
        // double-application risk. The cursor for the resumed state itself is included —
        // the Stepped state re-runs, and passing its last persisted cursor lets it pick up
        // exactly where the loop left off rather than restarting from None.
        let resume_cursors: HashMap<String, Vec<u8>> = rows
            .iter()
            .filter(|r| r.kind == RowKind::StepCursor)
            .filter_map(|r| {
                r.output_blob
                    .as_ref()
                    .map(|blob| (r.state.clone(), blob.clone()))
            })
            .fold(HashMap::new(), |mut acc, (state, blob)| {
                // Fold keeps the latest (highest-sequence) cursor per state because
                // `rows` is in ascending sequence order.
                acc.insert(state, blob);
                acc
            });

        #[cfg(feature = "tracing")]
        info!(workflow_id = %workflow_id, resume_state = ?resume_state, last_sequence = last.sequence, compensation_entries = compensation_stack.len(), cursor_states = resume_cursors.len(), "Resuming workflow from checkpoint");
        notify_observers(&self.observers, |o| {
            o.on_resume(&workflow_id, last.sequence)
        });

        self.resources.setup_all().await?;
        let exec = self.execute_workflow_from(
            resume_state,
            start_sequence,
            Some(workflow_id),
            compensation_stack,
            resume_cursors,
        );
        let result = if let Some(timeout_duration) = self.workflow_timeout {
            match tokio::time::timeout(timeout_duration, exec).await {
                Ok(result) => result,
                Err(_) => Err(CanoError::workflow("Workflow timeout exceeded")),
            }
        } else {
            exec.await
        };
        self.resources
            .teardown_range(0..self.resources.lifecycle_len())
            .await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::WorkflowObserver;
    use crate::resource::Resources;
    use crate::saga::CompensatableTask;
    use crate::task::Task;
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

    /// In-memory [`CheckpointStore`] test double. `live` is the real store state (`clear`
    /// empties it); `audit` records every row ever appended, in order, and is *never*
    /// cleared — so tests can inspect what was written even after a successful run cleared
    /// the live log. (Linear scans; fine for the tiny test scenarios here, not for scale.)
    #[derive(Default)]
    struct MemCheckpoints {
        live: Mutex<HashMap<String, Vec<CheckpointRow>>>,
        audit: Mutex<Vec<(String, CheckpointRow)>>,
    }

    #[cano_macros::checkpoint_store]
    impl CheckpointStore for MemCheckpoints {
        async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
            let mut live = self.live.lock().unwrap();
            let rows = live.entry(workflow_id.to_string()).or_default();
            if rows.iter().any(|r| r.sequence == row.sequence) {
                return Err(CanoError::checkpoint_store(format!(
                    "checkpoint conflict: {workflow_id:?} already has sequence {}",
                    row.sequence
                )));
            }
            self.audit
                .lock()
                .unwrap()
                .push((workflow_id.to_string(), row.clone()));
            rows.push(row);
            Ok(())
        }
        async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
            Ok(self.rows(workflow_id))
        }
        async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
            self.live.lock().unwrap().remove(workflow_id);
            Ok(())
        }
    }

    impl MemCheckpoints {
        /// Live rows for `workflow_id`, sorted by sequence (empty after a `clear`).
        fn rows(&self, workflow_id: &str) -> Vec<CheckpointRow> {
            let mut rows = self
                .live
                .lock()
                .unwrap()
                .get(workflow_id)
                .cloned()
                .unwrap_or_default();
            rows.sort_by_key(|r| r.sequence);
            rows
        }
        /// Every row ever appended for `workflow_id`, in append order — survives `clear`.
        fn audit_rows(&self, workflow_id: &str) -> Vec<CheckpointRow> {
            self.audit
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| id == workflow_id)
                .map(|(_, r)| r.clone())
                .collect()
        }
        fn audit_states(&self, workflow_id: &str) -> Vec<(u64, String)> {
            self.audit_rows(workflow_id)
                .into_iter()
                .map(|r| (r.sequence, r.state))
                .collect()
        }
    }

    /// Records `(event, workflow_id, sequence)` for `on_checkpoint` / `on_resume`.
    #[derive(Default)]
    struct CkptObserver(Mutex<Vec<(&'static str, String, u64)>>);
    impl WorkflowObserver for CkptObserver {
        fn on_checkpoint(&self, workflow_id: &str, sequence: u64) {
            self.0
                .lock()
                .unwrap()
                .push(("checkpoint", workflow_id.to_string(), sequence));
        }
        fn on_resume(&self, workflow_id: &str, sequence: u64) {
            self.0
                .lock()
                .unwrap()
                .push(("resume", workflow_id.to_string(), sequence));
        }
    }
    impl CkptObserver {
        fn events(&self) -> Vec<(&'static str, String, u64)> {
            self.0.lock().unwrap().clone()
        }
    }

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
        let observer = Arc::new(CkptObserver::default());
        let workflow = Workflow::bare()
            .register(TestState::Start, start_task.clone())
            .register(TestState::Process, process_task.clone())
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_observer(observer.clone());

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
        assert_eq!(
            observer.events(),
            vec![
                ("resume", "run-2".to_string(), 1),
                ("checkpoint", "run-2".to_string(), 2),
                ("checkpoint", "run-2".to_string(), 3),
            ]
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
        assert_eq!(err.category(), "checkpoint_store");
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
        // Note: inside the `cano` crate the inherent `#[router_task(state = S)]` form emits
        // `::cano::RouterTask<...>` paths that don't resolve. Use the trait-impl form instead.
        use crate::observer::WorkflowObserver;
        use crate::task::{RouterTask, TaskConfig};
        use cano_macros::router_task;

        struct RouteToWork;

        #[router_task]
        impl RouterTask<TestState> for RouteToWork {
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn route(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
                Ok(TaskResult::Single(TestState::Process))
            }
        }

        // Observe on_state_enter and on_checkpoint to verify router fires the
        // former but NOT the latter.
        #[derive(Default)]
        struct StateEnterObserver(Mutex<Vec<String>>);
        impl WorkflowObserver for StateEnterObserver {
            fn on_state_enter(&self, state: &str) {
                self.0.lock().unwrap().push(state.to_string());
            }
        }

        let store = Arc::new(MemCheckpoints::default());
        let observer = Arc::new(StateEnterObserver::default());
        let ckpt_obs = Arc::new(CkptObserver::default());

        // Route (router) → Process (single) → Complete (exit)
        let workflow = Workflow::bare()
            .register_router(TestState::Start, RouteToWork)
            .register(TestState::Process, SimpleTask::new(TestState::Complete))
            .add_exit_state(TestState::Complete)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("router-run")
            .with_observer(observer.clone())
            .with_observer(ckpt_obs.clone());

        assert_eq!(
            workflow.orchestrate(TestState::Start).await.unwrap(),
            TestState::Complete
        );

        // `on_state_enter` fires for ALL states (including the router).
        assert_eq!(
            *observer.0.lock().unwrap(),
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
        let ckpt_events = ckpt_obs.events();
        assert_eq!(
            ckpt_events,
            vec![
                ("checkpoint", "router-run".to_string(), 0),
                ("checkpoint", "router-run".to_string(), 1),
            ],
            "on_checkpoint fires only for non-router states"
        );
    }

    #[tokio::test]
    async fn resume_from_skips_router_rows_not_present_in_checkpoint_log() {
        // Seed a run that was interrupted after `Process` (sequence 0). No Route row exists
        // because `Start` was a router state (skipped). `resume_from` should re-enter at
        // `Process` and finish normally.
        use crate::task::{RouterTask, TaskConfig};
        use cano_macros::router_task;

        struct RouteToWork;

        #[router_task]
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

    use crate::task::TaskConfig;
    use cano_macros::compensatable_task;

    /// Shared, ordered log of `(task name, output value)` for every `compensate` call.
    type CompLog = Arc<Mutex<Vec<(String, u32)>>>;

    /// A compensatable test task. Its forward `run` returns `next_state` and `value`
    /// (unless `fail_forward`); `compensate` records `(name, output)` onto `log` (and
    /// errors if `fail_compensate`). No retries, so a forward failure surfaces immediately.
    #[derive(Clone)]
    struct CompTask {
        name: &'static str,
        value: u32,
        next_state: TestState,
        log: CompLog,
        fail_forward: bool,
        fail_compensate: bool,
    }

    impl CompTask {
        fn ok(name: &'static str, value: u32, next_state: TestState, log: &CompLog) -> Self {
            Self {
                name,
                value,
                next_state,
                log: log.clone(),
                fail_forward: false,
                fail_compensate: false,
            }
        }
    }

    #[compensatable_task]
    impl CompensatableTask<TestState> for CompTask {
        type Output = u32;
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        fn name(&self) -> Cow<'static, str> {
            Cow::Borrowed(self.name)
        }
        async fn run(&self, _res: &Resources) -> Result<(TaskResult<TestState>, u32), CanoError> {
            if self.fail_forward {
                return Err(CanoError::task_execution(format!(
                    "{} forward failed",
                    self.name
                )));
            }
            Ok((TaskResult::Single(self.next_state.clone()), self.value))
        }
        async fn compensate(&self, _res: &Resources, output: u32) -> Result<(), CanoError> {
            self.log
                .lock()
                .unwrap()
                .push((self.name.to_string(), output));
            if self.fail_compensate {
                return Err(CanoError::generic(format!(
                    "{} compensate failed",
                    self.name
                )));
            }
            Ok(())
        }
    }

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
        // Clean rollback → the original failure is surfaced unchanged.
        assert_eq!(err.category(), "task_execution");
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
                // [original (D forward failed), B's compensate failure].
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
        //  seq 4 — StepCursor for C          (kind = StepCursor,            cursor = [9,9])
        //
        // The highest-sequence row (seq 4) is the resume point → C is the resume state.
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
        // StepCursor row for the Split state — has output_blob but is NOT a CompensationCompletion.
        let cursor_row = CheckpointRow::new(4, "Split", "C").with_cursor(vec![9, 9]);
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
        id: impl Into<String>,
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
                Err(e) => assert_eq!(e.category(), "checkpoint_store", "unexpected error: {e}"),
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
        assert_eq!(err.category(), "checkpoint_store");
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
                // [the append failure that ended the run, then A's compensate failure].
                assert_eq!(errors[0].category(), "checkpoint_store");
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
    }
    #[compensatable_task]
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
        assert_eq!(err.category(), "task_execution");
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
        #[compensatable_task]
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
        #[compensatable_task]
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
    use crate::task::stepped::{Step, SteppedTask};
    use cano_macros::stepped_task;
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

    #[stepped_task]
    impl SteppedTask<TestState> for CountStepper {
        type Cursor = u32;
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        async fn step(
            &self,
            _res: &Resources,
            cursor: Option<u32>,
        ) -> Result<Step<u32, TestState>, CanoError> {
            self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            let n = cursor.unwrap_or(0) + 1;
            if n >= self.target {
                Ok(Step::Done(TaskResult::Single(TestState::Complete)))
            } else {
                Ok(Step::More(n))
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

        // Seed: A (compensatable, seq 0+1), cursor for a stepped state (seq 2), then crash.
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
                CheckpointRow::new(2, "Process", "CountStepper")
                    .with_cursor(serde_json::to_vec(&1u32).unwrap()),
            )
            .await
            .unwrap();

        // Process is a Stepped state; it's the resume point. Make the stepper fail so
        // the compensation stack (rehydrated A) drains and we can inspect it.
        struct FailingStepper;
        #[stepped_task]
        impl SteppedTask<TestState> for FailingStepper {
            type Cursor = u32;
            fn config(&self) -> TaskConfig {
                TaskConfig::minimal()
            }
            async fn step(
                &self,
                _res: &Resources,
                _cursor: Option<u32>,
            ) -> Result<Step<u32, TestState>, CanoError> {
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
}
