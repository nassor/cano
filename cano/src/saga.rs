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

// Re-export the attribute macro so it's accessible as `cano::saga::task`,
// enabling `#[saga::task]` when `cano::saga` is in scope.
pub use cano_macros::compensatable_task as task;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{TaskConfig, TaskResult};

/// A workflow task that records an `Output` when it succeeds and can later be
/// undone via [`compensate`](Self::compensate).
///
/// Register it with [`Workflow::register_with_compensation`](crate::workflow::Workflow::register_with_compensation).
/// Use `#[saga::task(state = S)]` on an inherent `impl` block — the macro builds
/// the `impl CompensatableTask<S> for T` header, requiring only `type Output`, `run`, and
/// `compensate` (plus optional `config` / `name` overrides). A bare
/// `#[saga::task]` accepts a hand-written `impl CompensatableTask<S> for T` header:
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
/// #[saga::task(state = Step)]
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
#[crate::saga::task]
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
    ///
    /// # Saga safety contract
    ///
    /// **Do not commit a side effect before `Ok((next_state, output))` is
    /// returned.** A panic between the commit and the return — or a `?` that
    /// propagates after the commit — leaves the engine with no compensation
    /// stack entry, so a later failure cannot roll the side effect back.
    /// The recommended pattern is to compute the output value, validate it,
    /// then commit the side effect immediately before the final `Ok(...)`:
    ///
    /// ```ignore
    /// let prepared = self.prepare_charge(res).await?;        // safe to retry
    /// let auth = self.commit_charge(prepared).await?;        // side effect
    /// Ok((TaskResult::Single(NextState), auth))              // entry pushed
    /// ```
    async fn run(
        &self,
        res: &Resources<TResourceKey>,
    ) -> Result<(TaskResult<TState>, Self::Output), CanoError>;

    /// Undo the effects of a successful [`run`](Self::run), given its `output`. Must be
    /// **idempotent** — it may run more than once (for example after a resume that re-ran
    /// `run`).
    ///
    /// # Cancellation safety
    ///
    /// The drain bounds each `compensate` call with the task's
    /// [`attempt_timeout`](crate::task::TaskConfig::with_attempt_timeout) and
    /// the workflow's
    /// [`with_compensation_timeout`](crate::workflow::Workflow::with_compensation_timeout)
    /// — both implemented as `tokio::time::timeout` / `timeout_at`. When
    /// either fires the in-flight `compensate` future is dropped at its next
    /// await point. The drain records the timeout and moves on **without
    /// retrying** the compensator, so a compensator that yields mid-rollback
    /// can leave external state half-undone.
    ///
    /// Make `compensate` either short enough to fit any configured budget,
    /// or structurally cancellation-safe (RAII guards, idempotent transitions
    /// over external resources).
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
    pub task_id: Arc<str>,
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

/// Run `task.compensate(output)` inline as the rollback for an irrecoverable
/// failure detected by the adapter itself (serialize failed, or the task
/// returned a Split). Mirrors the safeguards the standard drain applies:
///
/// - Wrapped in `catch_unwind` so a panicking `compensate` cannot escape.
/// - `tracing::debug!`/`error!` events so the inline rollback is observable
///   in the same channel as the drain's per-entry events.
///
/// Deliberately does **not** wrap compensate in `tokio::time::timeout`. The
/// inline path consumes the typed `output` value into the compensate future;
/// dropping that future mid-await (the behavior of `tokio::time::timeout`)
/// would destroy `output` and leave the FSM with no recovery path — the
/// compensation entry was never persisted (this branch runs before the FSM
/// pushes anything onto the stack), so a partial rollback cannot be retried
/// from a checkpoint. Letting compensate run to completion is the safer
/// failure mode; a genuinely hanging compensate is still bounded by the
/// workflow-level
/// [`with_total_timeout`](crate::workflow::Workflow::with_total_timeout)
/// (which cancels the entire FSM, not just compensate, so the operator gets
/// a single coherent error).
///
/// Returns a flat `Result<(TState, Vec<u8>), CanoError>` — the caller treats
/// the `Err` as the FSM forward-failure for this state. On `Ok(())` from
/// compensate (rollback succeeded), the surfaced error is `original_err`; on
/// `Err`/panic from compensate, both are collected into a `CompensationFailed`
/// (the existing constructor flattens nested CompensationFailed values, so a
/// later drain that aggregates won't produce a doubly-nested error).
async fn run_inline_compensate<TState, TResourceKey, T>(
    task: &T,
    res: &Resources<TResourceKey>,
    output: T::Output,
    original_err: CanoError,
) -> Result<(TaskResult<TState>, Vec<u8>), CanoError>
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
    T: CompensatableTask<TState, TResourceKey> + ?Sized + 'static,
{
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;

    let task_name = task.name();

    #[cfg(feature = "tracing")]
    tracing::debug!(
        task = %task_name,
        "running compensate inline (adapter rejected the run result)"
    );

    let compensate_fut = task.compensate(res, output);
    let outcome = AssertUnwindSafe(compensate_fut).catch_unwind().await;

    match outcome {
        Ok(Ok(())) => Err(original_err),
        Ok(Err(compensate_err)) => {
            #[cfg(feature = "tracing")]
            tracing::error!(
                task = %task_name,
                error = %compensate_err,
                "inline compensate failed"
            );
            Err(CanoError::compensation_failed(vec![
                original_err,
                compensate_err,
            ]))
        }
        Err(payload) => {
            let panic_msg = crate::workflow::panic_payload_message(&*payload);
            #[cfg(feature = "tracing")]
            tracing::error!(task = %task_name, panic = %panic_msg, "inline compensate panicked");
            Err(CanoError::compensation_failed(vec![
                original_err,
                CanoError::task_execution(format!(
                    "inline compensate for `{task_name}` panicked: {panic_msg}"
                )),
            ]))
        }
    }
}

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
        let attempt_timeout = self.0.config().attempt_timeout;
        Box::pin(async move {
            // Apply the user's `attempt_timeout` *only* to the forward call.
            // `execute_compensatable_task` strips it from the config passed to
            // `run_with_retries` so the outer retry loop doesn't blanket-cancel
            // the inline-compensate path along with the forward call — a mid-
            // await drop there would destroy the typed `output` value with no
            // recovery path, since the compensation entry isn't persisted
            // until the forward call succeeds. See `run_inline_compensate`'s
            // docstring for the cancel-safety rationale.
            let forward_fut = self.0.run(res);
            let (state, output) = match attempt_timeout {
                Some(d) => match tokio::time::timeout(d, forward_fut).await {
                    Ok(inner) => inner?,
                    Err(_) => {
                        return Err(CanoError::timeout(format!(
                            "compensatable task `{}` forward run exceeded attempt_timeout {d:?}",
                            self.0.name()
                        )));
                    }
                },
                None => forward_fut.await?,
            };
            // Reject Split *before* serialization so its inline-compensate
            // path is identical to the serialize-failure case. If we let the
            // Split bubble up and be rejected by `execute_compensatable_task`,
            // the side effect committed by `run` would have no rollback path.
            if let TaskResult::Split(_) = &state {
                let split_err = CanoError::workflow(format!(
                    "Compensatable task `{}` returned a split result — split states cannot be compensatable",
                    self.0.name()
                ));
                return run_inline_compensate(self.0.as_ref(), res, output, split_err).await;
            }
            match serde_json::to_vec(&output) {
                Ok(blob) => Ok((state, blob)),
                Err(serialize_err) => {
                    // The forward task already ran and may have committed a
                    // real-world side effect, but the output can't be persisted
                    // for crash-recovery rollback. Run `compensate` inline with
                    // the typed output to undo the side effect now, then surface
                    // the failure to the FSM so the compensation drain doesn't
                    // try to compensate again.
                    let serialize_err = CanoError::task_execution(format!(
                        "serialize compensation output for `{}`: {serialize_err}",
                        self.0.name()
                    ));
                    run_inline_compensate(self.0.as_ref(), res, output, serialize_err).await
                }
            }
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

#[cfg(test)]
mod tests {
    //! Focused unit tests for the saga adapter (`CompensatableAdapter`) and the inline-compensate
    //! helper (`run_inline_compensate`). These exercise the adapter's own branches in isolation —
    //! forward dispatch, output serialization, and the inline-rollback safety paths — distinct
    //! from the FSM-level compensation-stack drain covered in `workflow/compensation.rs`.

    use super::*;
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        A,
        B,
    }

    type Log = Arc<std::sync::Mutex<Vec<u32>>>;
    fn log() -> Log {
        Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    /// A `u32`-output compensatable task with knobs for each forward / compensate behavior.
    /// `log` records every output passed to `compensate`, so a test can assert whether — and with
    /// what — compensation ran.
    #[derive(Clone)]
    struct Comp {
        output: u32,
        forward_split: bool,
        forward_err: bool,
        comp_fail: bool,
        comp_panic: bool,
        log: Log,
    }

    impl Comp {
        fn ok(output: u32, log: &Log) -> Self {
            Self {
                output,
                forward_split: false,
                forward_err: false,
                comp_fail: false,
                comp_panic: false,
                log: Arc::clone(log),
            }
        }
    }

    #[crate::saga::task]
    impl CompensatableTask<S> for Comp {
        type Output = u32;
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal()
        }
        async fn run(&self, _res: &Resources) -> Result<(TaskResult<S>, u32), CanoError> {
            if self.forward_err {
                return Err(CanoError::task_execution("forward failed"));
            }
            if self.forward_split {
                return Ok((TaskResult::Split(vec![S::A]), self.output));
            }
            Ok((TaskResult::Single(S::B), self.output))
        }
        async fn compensate(&self, _res: &Resources, output: u32) -> Result<(), CanoError> {
            self.log.lock().unwrap().push(output);
            if self.comp_panic {
                panic!("compensate panicked");
            }
            if self.comp_fail {
                return Err(CanoError::generic("compensate failed"));
            }
            Ok(())
        }
    }

    /// An `Output` whose `Serialize` always fails — exercises the serialize-failure inline path.
    #[derive(Clone)]
    struct FailSer;
    impl serde::Serialize for FailSer {
        fn serialize<Ser>(&self, _s: Ser) -> Result<Ser::Ok, Ser::Error>
        where
            Ser: serde::Serializer,
        {
            Err(<Ser::Error as serde::ser::Error>::custom(
                "intentional serialize failure",
            ))
        }
    }
    impl<'de> serde::Deserialize<'de> for FailSer {
        fn deserialize<D>(_d: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Ok(FailSer)
        }
    }

    #[derive(Clone)]
    struct UnserTask {
        log: Log,
    }
    #[crate::saga::task]
    impl CompensatableTask<S> for UnserTask {
        type Output = FailSer;
        async fn run(&self, _res: &Resources) -> Result<(TaskResult<S>, FailSer), CanoError> {
            Ok((TaskResult::Single(S::B), FailSer))
        }
        async fn compensate(&self, _res: &Resources, _output: FailSer) -> Result<(), CanoError> {
            self.log.lock().unwrap().push(999); // mark that inline compensate ran
            Ok(())
        }
    }

    #[derive(Clone)]
    struct SlowTask;
    #[crate::saga::task]
    impl CompensatableTask<S> for SlowTask {
        type Output = u32;
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(20))
        }
        async fn run(&self, _res: &Resources) -> Result<(TaskResult<S>, u32), CanoError> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok((TaskResult::Single(S::B), 1))
        }
        async fn compensate(&self, _res: &Resources, _output: u32) -> Result<(), CanoError> {
            Ok(())
        }
    }

    // ----- adapter delegation -----

    #[test]
    fn name_and_config_delegate_to_the_inner_task() {
        let l = log();
        let adapter = CompensatableAdapter(Arc::new(Comp::ok(1, &l)));
        assert!(adapter.name().contains("Comp"));
        assert!(adapter.config().attempt_timeout.is_none());

        let slow = CompensatableAdapter(Arc::new(SlowTask));
        assert_eq!(
            slow.config().attempt_timeout,
            Some(Duration::from_millis(20))
        );
    }

    // ----- forward run dispatch -----

    #[tokio::test]
    async fn forward_success_serializes_output_and_round_trips_through_compensate() {
        let l = log();
        let adapter = CompensatableAdapter(Arc::new(Comp::ok(42, &l)));
        let res = Resources::new();

        let (state, blob) = adapter.run(&res).await.expect("forward run ok");
        assert!(matches!(state, TaskResult::Single(S::B)));
        assert_eq!(serde_json::from_slice::<u32>(&blob).unwrap(), 42);

        adapter
            .compensate(&res, &blob)
            .await
            .expect("compensate ok");
        assert_eq!(
            *l.lock().unwrap(),
            vec![42],
            "compensate received the round-tripped output"
        );
    }

    #[tokio::test]
    async fn forward_error_propagates_without_running_compensate() {
        let l = log();
        let mut task = Comp::ok(1, &l);
        task.forward_err = true;
        let adapter = CompensatableAdapter(Arc::new(task));
        let res = Resources::new();

        let err = adapter.run(&res).await.unwrap_err();
        assert!(err.to_string().contains("forward failed"));
        assert!(
            l.lock().unwrap().is_empty(),
            "compensate must not run on a forward failure"
        );
    }

    #[tokio::test]
    async fn forward_run_respects_attempt_timeout() {
        // SlowTask sleeps 5s with a 20ms attempt_timeout -> the forward run is cancelled and the
        // adapter reports a timeout. Asserts the error kind (not a duration) so it can't flake.
        let adapter = CompensatableAdapter(Arc::new(SlowTask));
        let res = Resources::new();
        let err = adapter.run(&res).await.unwrap_err();
        assert!(
            err.to_string().contains("exceeded attempt_timeout"),
            "got: {err}"
        );
    }

    // ----- inline-compensate safety paths -----

    #[tokio::test]
    async fn split_result_runs_inline_compensate_then_returns_split_error() {
        let l = log();
        let mut task = Comp::ok(7, &l);
        task.forward_split = true;
        let adapter = CompensatableAdapter(Arc::new(task));
        let res = Resources::new();

        let err = adapter.run(&res).await.unwrap_err();
        assert!(
            err.to_string().contains("split result")
                || err
                    .to_string()
                    .contains("split states cannot be compensatable"),
            "got: {err}"
        );
        // The committed side effect was undone inline: compensate ran with the typed output.
        assert_eq!(*l.lock().unwrap(), vec![7]);
    }

    #[tokio::test]
    async fn split_with_failing_compensate_yields_compensation_failed() {
        let l = log();
        let mut task = Comp::ok(7, &l);
        task.forward_split = true;
        task.comp_fail = true;
        let adapter = CompensatableAdapter(Arc::new(task));
        let res = Resources::new();

        match adapter.run(&res).await.unwrap_err() {
            CanoError::CompensationFailed { errors } => {
                assert_eq!(errors.len(), 2, "original split error + compensate error");
                assert!(errors[1].to_string().contains("compensate failed"));
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
        assert_eq!(*l.lock().unwrap(), vec![7], "compensate was attempted");
    }

    #[tokio::test]
    async fn unserializable_output_runs_inline_compensate() {
        let l = log();
        let adapter = CompensatableAdapter(Arc::new(UnserTask {
            log: Arc::clone(&l),
        }));
        let res = Resources::new();

        let err = adapter.run(&res).await.unwrap_err();
        assert!(
            err.to_string().contains("serialize compensation output"),
            "got: {err}"
        );
        // The side effect was undone inline since the output couldn't be persisted for recovery.
        assert_eq!(*l.lock().unwrap(), vec![999]);
    }

    #[tokio::test]
    async fn inline_compensate_panic_becomes_compensation_failed() {
        let l = log();
        let mut task = Comp::ok(7, &l);
        task.forward_split = true;
        task.comp_panic = true;
        let adapter = CompensatableAdapter(Arc::new(task));
        let res = Resources::new();

        match adapter.run(&res).await.unwrap_err() {
            CanoError::CompensationFailed { errors } => {
                assert_eq!(errors.len(), 2);
                assert!(
                    errors[1].to_string().contains("panicked"),
                    "got: {}",
                    errors[1]
                );
            }
            other => panic!("expected CompensationFailed, got {other:?}"),
        }
        // The panic was caught after compensate recorded its output (no unwind escaped).
        assert_eq!(*l.lock().unwrap(), vec![7]);
    }

    #[tokio::test]
    async fn run_inline_compensate_returns_original_error_on_clean_rollback() {
        // Direct test of the helper's contract: a clean rollback returns the *original* error
        // unchanged, and compensate ran with the supplied output.
        let l = log();
        let task = Comp::ok(42, &l);
        let res = Resources::new();
        let original = CanoError::task_execution("the real failure");
        let out: Result<(TaskResult<S>, Vec<u8>), CanoError> =
            run_inline_compensate(&task, &res, 42, original).await;
        assert!(out.unwrap_err().to_string().contains("the real failure"));
        assert_eq!(*l.lock().unwrap(), vec![42]);
    }

    // ----- compensate (replay) path -----

    #[tokio::test]
    async fn compensate_with_corrupt_blob_errors() {
        let l = log();
        let adapter = CompensatableAdapter(Arc::new(Comp::ok(1, &l)));
        let res = Resources::new();

        let err = adapter
            .compensate(&res, b"not valid json")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("deserialize compensation output"),
            "got: {err}"
        );
        assert!(
            l.lock().unwrap().is_empty(),
            "compensate body must not run when the blob can't be deserialized"
        );
    }
}
