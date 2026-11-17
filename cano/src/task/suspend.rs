//! # SuspendTask: durable suspend and resume
//!
//! A [`SuspendTask`] parks a run at its state until an outside party (a human reviewer, a
//! webhook, another service) delivers a typed input. When the FSM enters the state the
//! engine runs [`suspend`](SuspendTask::suspend), writes a
//! [`RowKind::Suspended`](crate::recovery::RowKind::Suspended) checkpoint row and returns
//! `Ok(state)` from [`orchestrate`](crate::workflow::Workflow::orchestrate): the run is
//! parked, not failed, so the compensation stack and the checkpoint log are kept.
//!
//! [`Workflow::resume_with`](crate::workflow::Workflow::resume_with) delivers the input:
//! the engine records it as a [`RowKind::ResumeInput`](crate::recovery::RowKind::ResumeInput)
//! row, calls [`resume`](SuspendTask::resume) with it, and continues the FSM from the state
//! `resume` returns. The pause can span process restarts: only the checkpoint store and the
//! application's own storage carry state across it.
//!
//! [`Workflow::resume_from`](crate::workflow::Workflow::resume_from) on a parked run returns
//! the parked state without running anything. On a run whose input was recorded but whose
//! `resume` did not finish (a crash, or `resume` returned an error), it delivers the stored
//! input again.
//!
//! ## Requirements
//!
//! - A checkpoint store and a workflow id
//!   ([`with_checkpoint_store`](crate::workflow::Workflow::with_checkpoint_store),
//!   [`with_workflow_id`](crate::workflow::Workflow::with_workflow_id)).
//!   [`validate`](crate::workflow::Workflow::validate) rejects a workflow with a suspend
//!   state and no store.
//! - `suspend` and `resume` are at-least-once and must be idempotent: a crash inside either
//!   re-runs it on the next resume.
//! - Data needed after the pause lives in a durable store the application owns.
//!   [`Resources`] are not checkpointed.
//! - There is no built-in deadline. Deliver one as an ordinary input, for example a
//!   `Decision::Expired` variant sent by a sweeper job.
//! - A suspend task has no companion `Task` impl, so it cannot be a
//!   [`register_split`](crate::workflow::Workflow::register_split) branch.
//!
//! ## Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//! use serde::{Deserialize, Serialize};
//! use std::sync::Arc;
//! # use std::collections::HashMap;
//! # use std::sync::Mutex;
//! # #[derive(Default)]
//! # struct MapStore(Mutex<HashMap<String, Vec<CheckpointRow>>>);
//! # #[cano::checkpoint_store]
//! # impl MapStore {
//! #     async fn append(&self, id: &str, row: CheckpointRow) -> Result<(), CanoError> {
//! #         let mut runs = self.0.lock().unwrap();
//! #         let rows = runs.entry(id.to_string()).or_default();
//! #         if rows.iter().any(|r| r.sequence == row.sequence) {
//! #             return Err(CanoError::checkpoint_store("duplicate sequence"));
//! #         }
//! #         rows.push(row);
//! #         Ok(())
//! #     }
//! #     async fn load_run(&self, id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
//! #         Ok(self.0.lock().unwrap().get(id).cloned().unwrap_or_default())
//! #     }
//! #     async fn clear(&self, id: &str) -> Result<(), CanoError> {
//! #         self.0.lock().unwrap().remove(id);
//! #         Ok(())
//! #     }
//! # }
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum Step { Review, Approved, Rejected }
//!
//! #[derive(Debug, Clone, Serialize, Deserialize)]
//! enum Decision { Approve, Reject }
//!
//! struct Review;
//!
//! #[task::suspend(state = Step)]
//! impl Review {
//!     async fn suspend(&self) -> Result<(), CanoError> {
//!         // Notify the reviewer here.
//!         Ok(())
//!     }
//!
//!     async fn resume(&self, decision: Decision) -> Result<Step, CanoError> {
//!         Ok(match decision {
//!             Decision::Approve => Step::Approved,
//!             Decision::Reject => Step::Rejected,
//!         })
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), CanoError> {
//! let store = Arc::new(MapStore::default());
//! let workflow = Workflow::bare()
//!     .register(Step::Review, Review)
//!     .add_exit_states([Step::Approved, Step::Rejected])
//!     .with_checkpoint_store(store)
//!     .with_workflow_id("review-1");
//!
//! // The run parks at `Review`.
//! assert_eq!(workflow.orchestrate(Step::Review).await?, Step::Review);
//! // Later, possibly from another process: deliver the decision.
//! assert_eq!(workflow.resume_with("review-1", &Decision::Approve).await?, Step::Approved);
//! # Ok(())
//! # }
//! ```

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::TaskConfig;
use crate::task::retry::{ObserverCtx, run_with_retries_observed};
use futures_util::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::fmt;

/// A task that parks the run until an outside party delivers an input.
///
/// See the [module documentation](self) for the run lifecycle and requirements.
///
/// # Associated Types
///
/// - **`Input`**: the value delivered through
///   [`Workflow::resume_with`](crate::workflow::Workflow::resume_with). It is persisted as
///   JSON in the checkpoint log before `resume` runs.
///
/// # At-least-once
///
/// `suspend` and `resume` run at least once per visit and must be idempotent. A crash
/// inside `suspend` re-runs it on the next `resume_from`; a crash inside `resume`, or a
/// `resume` that returns an error, leaves the input recorded so `resume_from` delivers it
/// again. Keep data needed after the pause in a durable store the application owns:
/// [`Resources`] are not checkpointed.
///
/// # Default config
///
/// [`TaskConfig::new()`] (one attempt, no retries). The config's retry policy applies to
/// `suspend` and to `resume` separately.
#[crate::task::suspend]
pub trait SuspendTask<TState>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
{
    /// Value an outside party supplies through
    /// [`Workflow::resume_with`](crate::workflow::Workflow::resume_with).
    type Input: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;

    /// Get the task configuration that controls execution behaviour.
    ///
    /// Defaults to [`TaskConfig::new()`] (one attempt, no retries).
    fn config(&self) -> TaskConfig {
        TaskConfig::new()
    }

    /// Human-readable identifier for this task, reported to [`WorkflowObserver`] hooks and
    /// stored in checkpoint rows.
    ///
    /// Defaults to [`std::any::type_name`] of the implementing type.
    ///
    /// [`WorkflowObserver`]: crate::observer::WorkflowObserver
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Runs when the FSM enters the state, before the run parks. Typically notifies the
    /// party that will deliver the input.
    ///
    /// # Errors
    ///
    /// An error fails the run like any task error: the compensation stack drains.
    async fn suspend(&self, res: &Resources) -> Result<(), CanoError>;

    /// Runs when an input is delivered; returns the next state.
    ///
    /// # Errors
    ///
    /// An error fails the call like any task error. With an empty compensation stack the
    /// recorded input is kept, so [`resume_from`](crate::workflow::Workflow::resume_from)
    /// delivers it again.
    async fn resume(&self, res: &Resources, input: Self::Input) -> Result<TState, CanoError>;
}

// ---------------------------------------------------------------------------
// Type-erased suspend-task infrastructure (for the engine's `Suspend` entry)
// ---------------------------------------------------------------------------

/// Object-safe, type-erased view of a [`SuspendTask`]; inputs cross the boundary as
/// JSON bytes.
pub(crate) trait ErasedSuspendTask<TState>: Send + Sync
where
    TState: Clone + Send + Sync + 'static,
{
    /// Run `suspend` under `config`'s retry policy.
    fn suspend<'a>(
        &'a self,
        res: &'a Resources,
        config: &'a TaskConfig,
        obs: Option<ObserverCtx<'a>>,
    ) -> BoxFuture<'a, Result<(), CanoError>>;

    /// Check that `input` decodes as the task's `Input` type.
    fn check_input(&self, input: &[u8]) -> Result<(), CanoError>;

    /// Decode `input` and run `resume` with it under `config`'s retry policy.
    fn resume<'a>(
        &'a self,
        res: &'a Resources,
        config: &'a TaskConfig,
        obs: Option<ObserverCtx<'a>>,
        input: &'a [u8],
    ) -> BoxFuture<'a, Result<TState, CanoError>>;
}

/// Bridges a concrete [`SuspendTask`] to the object-safe [`ErasedSuspendTask`].
pub(crate) struct SuspendAdapter<T>(pub T);

impl<T> SuspendAdapter<T> {
    fn decode<TState>(&self, input: &[u8]) -> Result<T::Input, CanoError>
    where
        TState: Clone + fmt::Debug + Send + Sync + 'static,
        T: SuspendTask<TState>,
    {
        serde_json::from_slice(input).map_err(|e| {
            CanoError::task_execution(format!("deserialize input for `{}`: {e}", self.0.name()))
        })
    }
}

impl<TState, T> ErasedSuspendTask<TState> for SuspendAdapter<T>
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    T: SuspendTask<TState> + 'static,
{
    fn suspend<'a>(
        &'a self,
        res: &'a Resources,
        config: &'a TaskConfig,
        obs: Option<ObserverCtx<'a>>,
    ) -> BoxFuture<'a, Result<(), CanoError>> {
        Box::pin(async move {
            run_with_retries_observed(config, obs.as_ref(), || self.0.suspend(res)).await
        })
    }

    fn check_input(&self, input: &[u8]) -> Result<(), CanoError> {
        self.decode::<TState>(input).map(drop)
    }

    fn resume<'a>(
        &'a self,
        res: &'a Resources,
        config: &'a TaskConfig,
        obs: Option<ObserverCtx<'a>>,
        input: &'a [u8],
    ) -> BoxFuture<'a, Result<TState, CanoError>> {
        Box::pin(async move {
            let input = self.decode::<TState>(input)?;
            run_with_retries_observed(config, obs.as_ref(), || self.0.resume(res, input.clone()))
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::recovery::RowKind;
    use crate::saga::CompensatableTask;
    use crate::task::Task;
    use crate::testing::InMemoryCheckpointStore;
    use crate::workflow::Workflow;
    use serde::Deserialize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    // In-crate usages take the trait-impl form: the inherent form emits `::cano::` paths
    // that are covered by `cano-macros/tests/suspend_task_impl.rs`.

    const ID: &str = "run-1";

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        Prep,
        Saga,
        Review,
        Fail,
        Done,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    enum D {
        Approve,
        Reject,
        Again,
    }

    #[derive(Clone, Default)]
    struct Counters {
        suspends: Arc<AtomicU32>,
        resumes: Arc<AtomicU32>,
        inputs: Arc<parking_lot::Mutex<Vec<D>>>,
    }

    /// `Approve` goes to `Done`, `Reject` to `Fail`, `Again` to `again`. The first
    /// `fail_resumes` calls to `resume` return an error.
    struct Gate {
        counters: Counters,
        again: S,
        fail_resumes: u32,
    }

    impl Gate {
        fn new(counters: &Counters) -> Self {
            Self {
                counters: counters.clone(),
                again: S::Prep,
                fail_resumes: 0,
            }
        }
    }

    #[crate::task::suspend]
    impl SuspendTask<S> for Gate {
        type Input = D;

        async fn suspend(&self, _res: &Resources) -> Result<(), CanoError> {
            self.counters.suspends.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn resume(&self, _res: &Resources, input: D) -> Result<S, CanoError> {
            let call = self.counters.resumes.fetch_add(1, Ordering::SeqCst);
            self.counters.inputs.lock().push(input.clone());
            if call < self.fail_resumes {
                return Err(CanoError::task_execution("resume failed"));
            }
            Ok(match input {
                D::Approve => S::Done,
                D::Reject => S::Fail,
                D::Again => self.again.clone(),
            })
        }
    }

    struct Goto(S);

    #[crate::task]
    impl Task<S> for Goto {
        async fn run(&self, _res: &Resources) -> Result<S, CanoError> {
            Ok(self.0.clone())
        }
    }

    struct Failing;

    #[crate::task]
    impl Task<S> for Failing {
        async fn run(&self, _res: &Resources) -> Result<S, CanoError> {
            Err(CanoError::task_execution("rejected"))
        }
    }

    struct Reserve(Arc<AtomicU32>);

    #[crate::saga::task]
    impl CompensatableTask<S> for Reserve {
        type Output = u32;

        async fn run(&self, _res: &Resources) -> Result<(S, u32), CanoError> {
            Ok((S::Review, 7))
        }

        async fn compensate(&self, _res: &Resources, _output: u32) -> Result<(), CanoError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn workflow(store: &Arc<InMemoryCheckpointStore>, gate: Gate) -> Workflow<S> {
        Workflow::bare()
            .register(S::Review, gate)
            .add_exit_state(S::Done)
            .with_checkpoint_store(store.clone())
            .with_workflow_id(ID)
    }

    fn kinds(store: &InMemoryCheckpointStore) -> Vec<RowKind> {
        store.rows(ID).into_iter().map(|r| r.kind).collect()
    }

    #[tokio::test]
    async fn orchestrate_parks_at_suspend_state() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let wf = workflow(&store, Gate::new(&c));

        assert_eq!(wf.orchestrate(S::Review).await.unwrap(), S::Review);
        assert_eq!(c.suspends.load(Ordering::SeqCst), 1);
        assert_eq!(c.resumes.load(Ordering::SeqCst), 0);
        assert_eq!(kinds(&store), [RowKind::StateEntry, RowKind::Suspended]);
    }

    #[tokio::test]
    async fn resume_from_on_parked_run_runs_nothing() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let wf = workflow(&store, Gate::new(&c));
        wf.orchestrate(S::Review).await.unwrap();

        assert_eq!(wf.resume_from(ID).await.unwrap(), S::Review);
        assert_eq!(c.suspends.load(Ordering::SeqCst), 1);
        assert_eq!(c.resumes.load(Ordering::SeqCst), 0);
        assert_eq!(store.rows(ID).len(), 2);
    }

    #[tokio::test]
    async fn resume_with_delivers_input_and_finishes() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let wf = workflow(&store, Gate::new(&c));
        wf.orchestrate(S::Review).await.unwrap();

        assert_eq!(wf.resume_with(ID, &D::Approve).await.unwrap(), S::Done);
        assert_eq!(*c.inputs.lock(), [D::Approve]);
        assert!(store.rows(ID).is_empty(), "a finished run clears its log");
        // The re-entry into `Review` writes no second `StateEntry` row.
        let history: Vec<(RowKind, String)> = store
            .history()
            .into_iter()
            .map(|(_, r)| (r.kind, r.state))
            .collect();
        assert_eq!(
            history,
            [
                (RowKind::StateEntry, "Review".to_string()),
                (RowKind::Suspended, "Review".to_string()),
                (RowKind::ResumeInput, "Review".to_string()),
                (RowKind::StateEntry, "Done".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn failed_resume_keeps_input_for_resume_from() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let wf = workflow(
            &store,
            Gate {
                fail_resumes: 1,
                ..Gate::new(&c)
            },
        );
        wf.orchestrate(S::Review).await.unwrap();

        assert!(wf.resume_with(ID, &D::Approve).await.is_err());
        assert_eq!(store.rows(ID).last().unwrap().kind, RowKind::ResumeInput);

        let err = wf.resume_with(ID, &D::Reject).await.unwrap_err();
        assert!(err.to_string().contains("is not suspended"), "{err}");

        assert_eq!(wf.resume_from(ID).await.unwrap(), S::Done);
        assert_eq!(*c.inputs.lock(), [D::Approve, D::Approve]);
        assert_eq!(c.suspends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn compensation_stack_survives_the_pause() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let compensated = Arc::new(AtomicU32::new(0));
        let wf = workflow(&store, Gate::new(&c))
            .register(S::Saga, Reserve(compensated.clone()))
            .register(S::Fail, Failing);

        assert_eq!(wf.orchestrate(S::Saga).await.unwrap(), S::Review);
        assert_eq!(compensated.load(Ordering::SeqCst), 0);

        assert!(wf.resume_with(ID, &D::Reject).await.is_err());
        assert_eq!(compensated.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn wrongly_typed_input_is_rejected_before_recording() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let wf = workflow(&store, Gate::new(&c));
        wf.orchestrate(S::Review).await.unwrap();

        let err = wf.resume_with(ID, &42u32).await.unwrap_err();
        assert!(err.to_string().contains("deserialize input"), "{err}");
        assert_eq!(store.rows(ID).len(), 2);
        assert_eq!(c.resumes.load(Ordering::SeqCst), 0);

        assert_eq!(wf.resume_with(ID, &D::Approve).await.unwrap(), S::Done);
    }

    #[tokio::test]
    async fn revisiting_the_suspend_state_parks_again() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let wf = workflow(&store, Gate::new(&c)).register(S::Prep, Goto(S::Review));

        assert_eq!(wf.orchestrate(S::Prep).await.unwrap(), S::Review);
        assert_eq!(wf.resume_with(ID, &D::Again).await.unwrap(), S::Review);
        assert_eq!(c.suspends.load(Ordering::SeqCst), 2);
        assert_eq!(store.rows(ID).last().unwrap().kind, RowKind::Suspended);

        assert_eq!(wf.resume_with(ID, &D::Approve).await.unwrap(), S::Done);
        assert_eq!(*c.inputs.lock(), [D::Again, D::Approve]);
    }

    #[tokio::test]
    async fn self_loop_parks_again_without_replaying_input() {
        let store = Arc::new(InMemoryCheckpointStore::new());
        let c = Counters::default();
        let wf = workflow(
            &store,
            Gate {
                again: S::Review,
                ..Gate::new(&c)
            },
        );
        wf.orchestrate(S::Review).await.unwrap();

        assert_eq!(wf.resume_with(ID, &D::Again).await.unwrap(), S::Review);
        assert_eq!(c.suspends.load(Ordering::SeqCst), 2);
        assert_eq!(c.resumes.load(Ordering::SeqCst), 1);

        assert_eq!(wf.resume_from(ID).await.unwrap(), S::Review);
        assert_eq!(c.resumes.load(Ordering::SeqCst), 1);
        assert_eq!(c.suspends.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn suspend_task_requires_a_checkpoint_store() {
        let c = Counters::default();
        let err = Workflow::bare()
            .register(S::Review, Gate::new(&c))
            .add_exit_state(S::Done)
            .validate()
            .unwrap_err();
        assert!(matches!(err, CanoError::Configuration(_)), "{err:?}");
        assert!(err.to_string().contains("checkpoint store"), "{err}");
    }
}
