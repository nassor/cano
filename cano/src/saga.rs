//! # Saga / Compensation — undo successful work when a later step fails
//!
//! Once a workflow step has mutated an external system — charged a card, reserved
//! inventory — the FSM can't roll it back by itself; only an explicit *refund* or
//! *release* undoes it. The saga pattern handles this by pairing each irreversible
//! forward step with a **compensating action**, and replaying those compensations
//! in reverse order if a later step fails.
//!
//! In Cano:
//!
//! - Implement [`CompensatableTask`] for a state's task. Its [`run`](CompensatableTask::run)
//!   returns the next state **and** an `Output` value describing what it did;
//!   [`compensate`](CompensatableTask::compensate) takes that `Output` back and undoes it.
//! - Register it with [`Workflow::register_with_compensation`](crate::workflow::Workflow::register_with_compensation).
//! - The engine keeps a per-run **compensation stack**: each successful compensatable
//!   task pushes `(task name, serialized output)`. If a later state's task fails, the
//!   stack is drained LIFO and every `compensate` runs (errors are collected, the drain
//!   never stops early).
//! - With a [`CheckpointStore`](crate::recovery::CheckpointStore) attached, those outputs
//!   are persisted as [`CheckpointRow::output_blob`](crate::recovery::CheckpointRow::output_blob),
//!   so a resumed run rehydrates the stack and can still compensate work done in an
//!   *earlier process*. `compensate` therefore receives only `(res, output)` — it must
//!   not rely on any state left behind by the original `run`, and the workflow definition
//!   (state labels + compensator registrations) must match across processes (the same
//!   constraint that already applies to [resume](crate::recovery)).
//! - Compensation is supported for **single-task states only** — split states cannot
//!   register compensators in this version.
//!
//! On a clean rollback (every `compensate` succeeded) the original failure is returned
//! unchanged and, if a checkpoint store is attached, its log is cleared. If any
//! `compensate` fails, the result is a
//! [`CanoError::CompensationFailed`] carrying
//! the original error followed by every compensation error, and the log is left intact for
//! manual recovery.

use std::borrow::Cow;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;

use cano_macros::compensatable_task;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{TaskConfig, TaskResult};

/// A workflow task that records an `Output` when it succeeds and can later be
/// undone via [`compensate`](Self::compensate).
///
/// Register it with [`Workflow::register_with_compensation`](crate::workflow::Workflow::register_with_compensation).
/// Apply [`#[cano::compensatable_task(state = …)]`](macro@crate::compensatable_task) to an
/// inherent `impl` block — like `#[task(state = …)]`, the macro builds the
/// `impl CompensatableTask<…> for …` header for you (you can also write that header
/// yourself with a bare `#[cano::compensatable_task]`):
///
/// ```rust
/// use cano::prelude::*;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// enum Step { Reserve, Ship, Done }
///
/// #[derive(Serialize, Deserialize)]
/// struct Reservation { sku: String, qty: u32 }
///
/// struct ReserveInventory;
///
/// #[cano::compensatable_task(state = Step)]
/// impl ReserveInventory {
///     type Output = Reservation;
///     async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
///         // ... actually reserve ...
///         Ok((TaskResult::Single(Step::Ship), Reservation { sku: "ABC".into(), qty: 2 }))
///     }
///     async fn compensate(&self, _res: &Resources, output: Reservation) -> Result<(), CanoError> {
///         // ... release `output.qty` of `output.sku` ...
///         let _ = output;
///         Ok(())
///     }
/// }
/// ```
///
/// The `Output` is the **only** thing carried from `run` to `compensate`; the two may
/// execute in different processes after a [crash-recovery](crate::recovery) resume, so
/// `compensate` must work purely from `(res, output)`.
#[compensatable_task]
pub trait CompensatableTask<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Data describing what [`run`](Self::run) did, handed back to
    /// [`compensate`](Self::compensate). Serialized with `serde_json` onto the
    /// compensation stack and, when a checkpoint store is attached, persisted in the
    /// checkpoint row.
    type Output: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Retry / timeout / circuit configuration for the forward [`run`](Self::run).
    /// Defaults to the standard exponential-backoff policy (same as [`Task::config`](crate::task::Task::config)).
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
    }

    /// Stable identifier for this task: the compensation-stack key, and what shows up in
    /// observer events. Defaults to [`std::any::type_name`] of the implementing type.
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Run the forward action. On success, return the next state **and** the output
    /// needed to compensate this step later.
    async fn run(
        &self,
        res: &Resources<TResourceKey>,
    ) -> Result<(TaskResult<TState>, Self::Output), CanoError>;

    /// Undo the effects of a successful [`run`](Self::run), given its `output`. Must be
    /// **idempotent** — it may run more than once (for example after a resume that re-ran
    /// `run`).
    async fn compensate(
        &self,
        res: &Resources<TResourceKey>,
        output: Self::Output,
    ) -> Result<(), CanoError>;
}

/// One entry on a run's compensation stack: which compensatable task ran, and the
/// `serde_json`-serialized [`Output`](CompensatableTask::Output) it produced.
#[derive(Debug, Clone)]
pub(crate) struct CompensationEntry {
    /// The compensatable task's [`name`](CompensatableTask::name) — the key the engine
    /// uses to find the matching compensator.
    pub task_id: String,
    /// The serialized output, replayed into [`CompensatableTask::compensate`].
    pub output_blob: Vec<u8>,
}

/// Object-safe, type-erased view of a [`CompensatableTask`].
///
/// [`Workflow::register_with_compensation`](crate::workflow::Workflow::register_with_compensation)
/// builds one of these so the engine can dispatch the forward run and replay
/// compensations without naming the task's concrete `Output` type. **You should not need
/// to implement or name this directly** — it's the saga analogue of
/// [`TaskObject`](crate::task::TaskObject).
pub trait ErasedCompensatable<TState, TResourceKey>: Send + Sync
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// The underlying task's [`name`](CompensatableTask::name).
    fn name(&self) -> Cow<'static, str>;
    /// The forward task's retry / timeout configuration.
    fn config(&self) -> TaskConfig;
    /// Run the forward action; on success return the next state and the
    /// `serde_json`-serialized output.
    fn run<'a>(&'a self, res: &'a Resources<TResourceKey>) -> ForwardRunFuture<'a, TState>;
    /// Deserialize `output_blob` and run the task's [`compensate`](CompensatableTask::compensate).
    fn compensate<'a>(
        &'a self,
        res: &'a Resources<TResourceKey>,
        output_blob: &'a [u8],
    ) -> CompensateFuture<'a>;
}

/// Future returned by [`ErasedCompensatable::run`] — yields the next state and the
/// serialized [`Output`](CompensatableTask::Output).
pub type ForwardRunFuture<'a, TState> =
    Pin<Box<dyn Future<Output = Result<(TaskResult<TState>, Vec<u8>), CanoError>> + Send + 'a>>;

/// Future returned by [`ErasedCompensatable::compensate`].
pub type CompensateFuture<'a> = Pin<Box<dyn Future<Output = Result<(), CanoError>> + Send + 'a>>;

/// Bridges a concrete [`CompensatableTask`] to the object-safe [`ErasedCompensatable`].
pub(crate) struct CompensatableAdapter<T>(pub Arc<T>);

impl<TState, TResourceKey, T> ErasedCompensatable<TState, TResourceKey> for CompensatableAdapter<T>
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
    T: CompensatableTask<TState, TResourceKey> + 'static,
{
    fn name(&self) -> Cow<'static, str> {
        self.0.name()
    }

    fn config(&self) -> TaskConfig {
        self.0.config()
    }

    fn run<'a>(&'a self, res: &'a Resources<TResourceKey>) -> ForwardRunFuture<'a, TState> {
        Box::pin(async move {
            let (state, output) = self.0.run(res).await?;
            let blob = serde_json::to_vec(&output).map_err(|e| {
                CanoError::task_execution(format!(
                    "serialize compensation output for `{}`: {e}",
                    self.0.name()
                ))
            })?;
            Ok((state, blob))
        })
    }

    fn compensate<'a>(
        &'a self,
        res: &'a Resources<TResourceKey>,
        output_blob: &'a [u8],
    ) -> CompensateFuture<'a> {
        Box::pin(async move {
            let output: T::Output = serde_json::from_slice(output_blob).map_err(|e| {
                CanoError::generic(format!(
                    "deserialize compensation output for `{}`: {e}",
                    self.0.name()
                ))
            })?;
            self.0.compensate(res, output).await
        })
    }
}
