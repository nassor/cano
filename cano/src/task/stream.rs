//! # StreamTask — A Genuine Stream-Processing Model
//!
//! A [`StreamTask`] consumes an `impl Stream` **continuously**, processes each item, and
//! flushes per-[`StreamWindow`] window — so memory stays bounded and downstream sees
//! progress before the source ends. It terminates in one of three ways:
//!
//! - **Exhausted** — the source returns `None`: the partial window is flushed and
//!   [`on_close`](StreamTask::on_close)`(Exhausted)` chooses the next state.
//! - **Stop** — [`flush_window`](StreamTask::flush_window) returns [`WindowSignal::Stop`]:
//!   transition to that result.
//! - **Cancelled** — the workflow's [`CancellationToken`](crate::cancel::CancellationToken)
//!   fires: cooperative drain — the in-flight window is flushed, its cursor is committed,
//!   `on_close(Cancelled)` runs for cleanup (its returned state is *ignored*), and the run
//!   ends as [`CanoError::Cancelled`](crate::error::CanoError::Cancelled) so a later
//!   [`resume_from`](crate::workflow::Workflow::resume_from) continues from the committed
//!   cursor. Cancel means "stop cleanly + resumable", not "transition onward".
//!
//! ## Batch vs. stream
//!
//! This is **not** [`BatchTask`](crate::task::batch::BatchTask). Batch loads a *bounded*
//! `Vec`, processes all of it, and aggregates **once** at the end — O(N) memory, one
//! emission, requires the data to end. `StreamTask` is for *unbounded* / continuous
//! sources (Kafka, SSE, file-tail, WebSocket): incremental per-window emission, bounded
//! memory, runs until stopped, and **resumable** from a persisted cursor.
//!
//! ## Cursor persistence & resume
//!
//! Register with [`Workflow::register_stream`](crate::workflow::Workflow::register_stream)
//! and attach a [`CheckpointStore`](crate::recovery::CheckpointStore) + a workflow id: the
//! engine persists the cursor returned by the **last item of each flushed window** (as a
//! [`RowKind::StepCursor`](crate::recovery::RowKind::StepCursor) row), and a resumed run
//! re-opens the source from that position. Registering via plain
//! [`Workflow::register`](crate::workflow::Workflow::register) runs the in-memory loop
//! with **no** persistence and **no** cancellation — the companion `Task` path is for
//! convenience / tests only.
//!
//! ## Idempotency (at-least-once)
//!
//! The FSM writes the state-entry checkpoint *before* running the task, so a resumed run
//! re-enters the state and calls [`open`](StreamTask::open) again from the last committed
//! cursor. The window *after* that cursor may be partially processed then replayed —
//! [`open`](StreamTask::open), [`process_item`](StreamTask::process_item), and
//! [`on_close`](StreamTask::on_close) **must be idempotent**. `config` defaults to
//! [`TaskConfig::minimal()`] (no outer retry) because an outer retry would re-invoke
//! `open()` and re-consume the stream; only [`attempt_timeout`](crate::task::TaskConfig)
//! is honored — as a per-[`process_item`](StreamTask::process_item) bound.

use crate::cancel::CancellationToken;
use crate::error::CanoError;
use crate::resource::Resources;
use crate::task::{TaskConfig, TaskResult};
use futures_util::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// Controls how the per-item windowed loop responds when
/// [`process_item`](StreamTask::process_item) returns an [`Err`]. Modelled on
/// [`PollErrorPolicy`](crate::task::poll::PollErrorPolicy), with an extra
/// [`SkipAndContinue`](StreamErrorPolicy::SkipAndContinue) for poison-message handling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StreamErrorPolicy {
    /// Propagate the first item error — the loop stops and the run fails.
    #[default]
    FailFast,
    /// Log/observe the bad item, drop it, and keep consuming. The skipped item's
    /// cursor is not committed (the next good item advances it).
    SkipAndContinue,
    /// Tolerate up to `max_errors` **consecutive** item errors before failing. The
    /// counter resets on every successfully processed item.
    RetryOnError {
        /// Maximum number of consecutive item errors before the loop fails.
        max_errors: u32,
    },
}

/// Tumbling-window trigger: how often [`flush_window`](StreamTask::flush_window) fires and
/// how much the driver buffers. Defaults to per-item ([`Count(1)`](StreamWindow::Count));
/// larger windows amortise flush + checkpoint cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamWindow {
    /// Flush after this many successfully processed items (clamped to a minimum of 1).
    Count(usize),
    /// Flush after this much wall-clock elapses, tumbling. Empty windows are skipped
    /// (no [`flush_window`](StreamTask::flush_window) call) so an idle source does not
    /// emit spurious empty flushes.
    Duration(std::time::Duration),
}

/// The result of one [`flush_window`](StreamTask::flush_window) call.
#[derive(Debug)]
pub enum WindowSignal<TState> {
    /// Keep consuming the stream.
    Continue,
    /// Stop and transition the FSM to this result. The driver commits the window's
    /// cursor first.
    Stop(TaskResult<TState>),
}

/// Why the consume loop is ending — passed to [`on_close`](StreamTask::on_close).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The source stream returned `None`.
    Exhausted,
    /// The workflow's [`CancellationToken`](crate::cancel::CancellationToken) fired
    /// (cooperative shutdown). The in-flight partial window was flushed first.
    Cancelled,
}

// ---------------------------------------------------------------------------
// StreamTask trait
// ---------------------------------------------------------------------------

/// A genuine stream-processing model: consume an `impl Stream` continuously, flush per
/// window, run until cancelled/exhausted, and resume from a persisted cursor.
///
/// # Generic Types
///
/// - **`TState`**: The workflow state enum (`Clone + Debug + Send + Sync`).
/// - **`TResourceKey`**: The resource-lookup key type (defaults to [`Cow<'static, str>`]).
///
/// # Associated Types
///
/// - **`Item`**: one element pulled from the source stream.
/// - **`Output`**: the per-item result accumulated into a window.
/// - **`Cursor`**: the resumable position; `Serialize + DeserializeOwned + Send + Sync + 'static`.
///
/// Prefer the inherent `#[task::stream(state = S)]` form, which infers `Item` from
/// `process_item`'s owned `item` parameter and `Output` / `Cursor` from the `Ok` tuple of
/// its return type.
#[crate::task::stream]
pub trait StreamTask<TState, TResourceKey = Cow<'static, str>>: Send + Sync
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// One element pulled from the source stream.
    type Item: Send + 'static;
    /// The per-item result accumulated into a window.
    type Output: Send + 'static;
    /// The resumable position, persisted as a cursor for crash-resume.
    type Cursor: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Windowing policy. Defaults to [`StreamWindow::Count(1)`] (flush per item).
    fn window(&self) -> StreamWindow {
        StreamWindow::Count(1)
    }

    /// Per-item error policy. Defaults to [`StreamErrorPolicy::FailFast`].
    fn on_item_error(&self) -> StreamErrorPolicy {
        StreamErrorPolicy::FailFast
    }

    /// Task configuration. Defaults to [`TaskConfig::minimal()`].
    ///
    /// Only [`attempt_timeout`](crate::task::TaskConfig) is applied — as a bound on each
    /// [`process_item`](StreamTask::process_item) call (a timeout becomes an item error
    /// governed by [`on_item_error`](StreamTask::on_item_error)). **Outer retry
    /// (`max_attempts`) is intentionally not applied**: it would re-invoke
    /// [`open`](StreamTask::open) and re-consume the stream. The per-item error policy, the
    /// `CancellationToken`, and the window loop are the resilience surface.
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    /// Human-readable identifier, reported to
    /// [`WorkflowObserver`](crate::observer::WorkflowObserver) hooks.
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(std::any::type_name::<Self>())
    }

    /// Open (or resume) the source stream. `cursor` is the last committed position, or
    /// `None` on a fresh run. Must be idempotent (see the module docs).
    async fn open(
        &self,
        res: &Resources<TResourceKey>,
        cursor: Option<Self::Cursor>,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, CanoError>;

    /// Process one item; return its output and the cursor reached by consuming it (the
    /// position to commit once this item's window flushes).
    async fn process_item(
        &self,
        res: &Resources<TResourceKey>,
        item: Self::Item,
    ) -> Result<(Self::Output, Self::Cursor), CanoError>;

    /// Flush one full window: commit side effects, then decide whether to continue or
    /// stop. The driver persists the window's cursor after this returns.
    async fn flush_window(
        &self,
        res: &Resources<TResourceKey>,
        outputs: Vec<Self::Output>,
    ) -> Result<WindowSignal<TState>, CanoError>;

    /// Close hook, called after the in-flight partial window has been flushed.
    ///
    /// - [`CloseReason::Exhausted`]: the returned [`TaskResult`] is the **next state**.
    /// - [`CloseReason::Cancelled`]: a **cleanup** hook — the returned `TaskResult` is
    ///   **ignored** and the run ends as
    ///   [`CanoError::Cancelled`](crate::error::CanoError::Cancelled) (an `Err` returned
    ///   here *is* propagated). Use it to release resources / commit final offsets.
    ///
    /// **At-least-once:** like [`open`](StreamTask::open) / [`process_item`](StreamTask::process_item),
    /// `on_close` runs once per run but may be **re-invoked on crash-resume** (a crash
    /// between `on_close` and the cursor commit replays the boundary window). It **must be
    /// idempotent** — e.g. committing final offsets here must tolerate a repeat.
    async fn on_close(
        &self,
        res: &Resources<TResourceKey>,
        reason: CloseReason,
    ) -> Result<TaskResult<TState>, CanoError>;

    /// Drive the in-memory windowed loop (no cursor persistence, no cancellation). Used by
    /// the macro-synthesised `impl Task::run` so a `StreamTask` can be
    /// [`register`](crate::workflow::Workflow::register)ed like any task. The durable,
    /// cancellable path is [`Workflow::register_stream`](crate::workflow::Workflow::register_stream).
    ///
    /// Written as a hand-desugared `fn` (not `async fn`) so no `for<'async_trait>` binder
    /// is introduced; it returns the future produced by the crate-private driver.
    #[doc(hidden)]
    fn run_in_memory<'life0, 'life1, 'async_trait>(
        &'life0 self,
        res: &'life1 Resources<TResourceKey>,
    ) -> Pin<Box<dyn Future<Output = Result<TaskResult<TState>, CanoError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: Sync + 'async_trait + Sized,
    {
        Box::pin(run_stream_in_memory(self, res))
    }
}

// ---------------------------------------------------------------------------
// drive_window — the single per-window loop body (shared by both drivers)
// ---------------------------------------------------------------------------

/// One window of consumption: returned per [`flush_window`](StreamTask::flush_window) or
/// per terminal close. The cursor is the concrete `Cursor` of the last item in the window.
pub(crate) enum WindowStep<TCursor, TState> {
    /// A full window was flushed and the task asked to continue. Commit `cursor`.
    Window { cursor: TCursor },
    /// Natural termination: the stream ended or a window returned `Stop`. Commit
    /// `final_cursor` (if any), then transition to `result`.
    Done {
        final_cursor: Option<TCursor>,
        result: TaskResult<TState>,
    },
    /// The run was cancelled: the in-flight window was flushed and `on_close` ran for
    /// cleanup. Commit `final_cursor` (if any), then end as
    /// [`CanoError::Cancelled`](crate::error::CanoError::Cancelled) so
    /// [`resume_from`](crate::workflow::Workflow::resume_from) continues from this position.
    Cancelled { final_cursor: Option<TCursor> },
}

/// Pull and process items until one window flushes (or the loop terminates). Shared by the
/// in-memory companion and the engine-driven session — there is exactly one loop body.
#[allow(clippy::too_many_arguments)]
async fn drive_window<T, S, K>(
    task: &T,
    res: &Resources<K>,
    stream: &mut Pin<Box<dyn Stream<Item = T::Item> + Send>>,
    consecutive_errors: &mut u32,
    window: &StreamWindow,
    policy: &StreamErrorPolicy,
    attempt_timeout: Option<std::time::Duration>,
    token: &CancellationToken,
) -> Result<WindowStep<T::Cursor, S>, CanoError>
where
    T: StreamTask<S, K> + ?Sized,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    use futures_util::StreamExt as _;

    let count_limit = match window {
        StreamWindow::Count(n) => Some((*n).max(1)),
        StreamWindow::Duration(_) => None,
    };
    let mut deadline: Option<tokio::time::Instant> = match window {
        StreamWindow::Duration(d) => Some(tokio::time::Instant::now() + *d),
        StreamWindow::Count(_) => None,
    };

    let mut buf: Vec<T::Output> = Vec::new();
    let mut last_cursor: Option<T::Cursor> = None;

    loop {
        // Count-window flush.
        if let Some(limit) = count_limit
            && buf.len() >= limit
        {
            #[cfg(feature = "metrics")]
            crate::metrics::stream_window();
            let outputs = std::mem::take(&mut buf);
            return Ok(match task.flush_window(res, outputs).await? {
                WindowSignal::Continue => WindowStep::Window {
                    cursor: last_cursor.expect("a non-empty window always has a cursor"),
                },
                WindowSignal::Stop(result) => WindowStep::Done {
                    final_cursor: last_cursor,
                    result,
                },
            });
        }

        // Resolves at the duration-window deadline, or never (count windows).
        let tick = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            biased;
            _ = token.cancelled() => {
                if !buf.is_empty() {
                    #[cfg(feature = "metrics")]
                    crate::metrics::stream_window();
                    // On cancel, `flush_window` runs only to commit the partial window's
                    // side effects; its `WindowSignal` is ignored (the run ends as
                    // `Cancelled` regardless — honoring `Stop` here would contradict that).
                    let _ = task.flush_window(res, std::mem::take(&mut buf)).await?;
                }
                // `on_close(Cancelled)` is a cleanup hook; its returned state is ignored —
                // a cancelled run ends as `CanoError::Cancelled` (an `Err` it returns IS
                // propagated). Resume continues from the committed `final_cursor`.
                let _ = task.on_close(res, CloseReason::Cancelled).await?;
                return Ok(WindowStep::Cancelled { final_cursor: last_cursor });
            }
            _ = tick => {
                // Duration window elapsed.
                if buf.is_empty() {
                    // Empty tumbling window: advance the deadline and keep waiting.
                    if let (Some(d), StreamWindow::Duration(dur)) = (deadline.as_mut(), window) {
                        *d = tokio::time::Instant::now() + *dur;
                    }
                    continue;
                }
                #[cfg(feature = "metrics")]
                crate::metrics::stream_window();
                let outputs = std::mem::take(&mut buf);
                return Ok(match task.flush_window(res, outputs).await? {
                    WindowSignal::Continue => WindowStep::Window {
                        cursor: last_cursor.expect("a non-empty window always has a cursor"),
                    },
                    WindowSignal::Stop(result) => WindowStep::Done {
                        final_cursor: last_cursor,
                        result,
                    },
                });
            }
            item = stream.next() => {
                match item {
                    Some(item) => {
                        // Bound a single `process_item` by `config().attempt_timeout` when set
                        // (a hung source item is the realistic failure mode). A timeout becomes
                        // an ordinary item error governed by `on_item_error()` below. Outer
                        // retry (`max_attempts`) is intentionally NOT applied — the per-item
                        // policy + the loop are the resilience surface.
                        let processed = match attempt_timeout {
                            Some(d) => match tokio::time::timeout(d, task.process_item(res, item)).await {
                                Ok(inner) => inner,
                                Err(_elapsed) => Err(CanoError::timeout(
                                    "stream process_item exceeded attempt_timeout",
                                )),
                            },
                            None => task.process_item(res, item).await,
                        };
                        match processed {
                            Ok((out, cursor)) => {
                                *consecutive_errors = 0;
                                buf.push(out);
                                last_cursor = Some(cursor);
                                #[cfg(feature = "metrics")]
                                crate::metrics::stream_items(1, 0);
                            }
                            Err(e) => {
                                #[cfg(feature = "metrics")]
                                crate::metrics::stream_items(0, 1);
                                match policy {
                                    StreamErrorPolicy::FailFast => return Err(e),
                                    StreamErrorPolicy::SkipAndContinue => {}
                                    StreamErrorPolicy::RetryOnError { max_errors } => {
                                        *consecutive_errors += 1;
                                        if *consecutive_errors > *max_errors {
                                            return Err(e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        // Source exhausted: flush the final partial window. Honor a `Stop`
                        // here (transition to it) just like a full window; on `Continue`
                        // fall through to `on_close(Exhausted)` for the terminal transition.
                        if !buf.is_empty() {
                            #[cfg(feature = "metrics")]
                            crate::metrics::stream_window();
                            match task.flush_window(res, std::mem::take(&mut buf)).await? {
                                WindowSignal::Stop(result) => {
                                    return Ok(WindowStep::Done {
                                        final_cursor: last_cursor,
                                        result,
                                    });
                                }
                                WindowSignal::Continue => {}
                            }
                        }
                        let result = task.on_close(res, CloseReason::Exhausted).await?;
                        return Ok(WindowStep::Done { final_cursor: last_cursor, result });
                    }
                }
            }
        }
    }
}

/// In-memory companion loop: drive windows with a disabled token (no cancellation), no
/// cursor persistence. Backs [`StreamTask::run_in_memory`].
async fn run_stream_in_memory<T, S, K>(
    task: &T,
    res: &Resources<K>,
) -> Result<TaskResult<S>, CanoError>
where
    T: StreamTask<S, K> + ?Sized,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    let token = CancellationToken::disabled();
    let window = task.window();
    let policy = task.on_item_error();
    let attempt_timeout = task.config().attempt_timeout;
    let mut consecutive_errors: u32 = 0;

    let result: Result<TaskResult<S>, CanoError> = async {
        let mut stream = task.open(res, None).await?;
        loop {
            match drive_window(
                task,
                res,
                &mut stream,
                &mut consecutive_errors,
                &window,
                &policy,
                attempt_timeout,
                &token,
            )
            .await?
            {
                WindowStep::Window { .. } => continue,
                WindowStep::Done { result, .. } => return Ok(result),
                // Unreachable: the in-memory companion drives with a disabled token.
                WindowStep::Cancelled { .. } => return Err(CanoError::cancelled()),
            }
        }
    }
    .await;

    // The in-memory companion uses a disabled token, so it never cancels.
    #[cfg(feature = "metrics")]
    crate::metrics::stream_run(if result.is_ok() {
        "completed"
    } else {
        "failed"
    });
    result
}

// ---------------------------------------------------------------------------
// Type-erased infrastructure (for StateEntry::Stream / register_stream)
// ---------------------------------------------------------------------------

/// One erased window step: serialized cursor bytes in place of the concrete `Cursor`.
pub enum ErasedWindowStep<TState> {
    /// A full window flushed; persist `cursor` and continue.
    Window { cursor: Vec<u8> },
    /// Natural termination: persist `final_cursor` (if any) then transition to `result`.
    Done {
        final_cursor: Option<Vec<u8>>,
        result: TaskResult<TState>,
    },
    /// Cancelled: persist `final_cursor` (if any) then end as `CanoError::Cancelled`.
    Cancelled { final_cursor: Option<Vec<u8>> },
}

/// Future returned by [`ErasedStreamSession::next_window`].
pub type WindowFuture<'a, TState> =
    Pin<Box<dyn Future<Output = Result<ErasedWindowStep<TState>, CanoError>> + Send + 'a>>;

/// Object-safe view of an opened stream session. The engine advances it one window at a
/// time, persisting the returned cursor between windows.
pub trait ErasedStreamSession<TState, TResourceKey>: Send
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Consume until one window flushes (or the loop terminates).
    fn next_window<'a>(
        &'a mut self,
        res: &'a Resources<TResourceKey>,
        token: &'a CancellationToken,
    ) -> WindowFuture<'a, TState>;
}

/// Future returned by [`ErasedStreamTask::open_session`].
pub type OpenSessionFuture<'a, TState, TResourceKey> = Pin<
    Box<
        dyn Future<Output = Result<Box<dyn ErasedStreamSession<TState, TResourceKey>>, CanoError>>
            + Send
            + 'a,
    >,
>;

/// Object-safe, type-erased view of a [`StreamTask`] for the engine's
/// [`StateEntry::Stream`](crate::workflow::execution::StateEntry) path.
pub trait ErasedStreamTask<TState, TResourceKey>: Send + Sync
where
    TState: Clone + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    fn name(&self) -> Cow<'static, str>;
    /// Open (or resume) the source from `cursor_bytes`, returning a driven session.
    /// `attempt_timeout` (from the registered `config()`) bounds each `process_item`.
    fn open_session<'a>(
        &'a self,
        res: &'a Resources<TResourceKey>,
        cursor_bytes: Option<Vec<u8>>,
        attempt_timeout: Option<std::time::Duration>,
    ) -> OpenSessionFuture<'a, TState, TResourceKey>;
}

/// An opened, concretely-typed stream session: owns the task handle + the stream + the
/// per-stream error counter. Holds the single windowed loop body.
struct StreamSession<T, S, K>
where
    T: StreamTask<S, K> + 'static,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    task: Arc<T>,
    stream: Pin<Box<dyn Stream<Item = T::Item> + Send>>,
    window: StreamWindow,
    policy: StreamErrorPolicy,
    attempt_timeout: Option<std::time::Duration>,
    consecutive_errors: u32,
}

impl<T, S, K> ErasedStreamSession<S, K> for StreamSession<T, S, K>
where
    T: StreamTask<S, K> + 'static,
    S: Clone + fmt::Debug + Send + Sync + 'static,
    K: Hash + Eq + Send + Sync + 'static,
{
    fn next_window<'a>(
        &'a mut self,
        res: &'a Resources<K>,
        token: &'a CancellationToken,
    ) -> WindowFuture<'a, S> {
        Box::pin(async move {
            let task = Arc::clone(&self.task);
            let step = drive_window(
                &*task,
                res,
                &mut self.stream,
                &mut self.consecutive_errors,
                &self.window,
                &self.policy,
                self.attempt_timeout,
                token,
            )
            .await?;
            Ok(match step {
                WindowStep::Window { cursor } => ErasedWindowStep::Window {
                    cursor: encode_cursor(&cursor, &self.task.name())?,
                },
                WindowStep::Done {
                    final_cursor,
                    result,
                } => ErasedWindowStep::Done {
                    final_cursor: match final_cursor {
                        Some(c) => Some(encode_cursor(&c, &self.task.name())?),
                        None => None,
                    },
                    result,
                },
                WindowStep::Cancelled { final_cursor } => ErasedWindowStep::Cancelled {
                    final_cursor: match final_cursor {
                        Some(c) => Some(encode_cursor(&c, &self.task.name())?),
                        None => None,
                    },
                },
            })
        })
    }
}

/// Bridges a concrete [`StreamTask`] to the object-safe [`ErasedStreamTask`]. Handles
/// `serde_json` cursor (de)serialization so the engine only sees `Vec<u8>`.
pub(crate) struct StreamAdapter<T>(pub Arc<T>);

impl<TState, TResourceKey, T> ErasedStreamTask<TState, TResourceKey> for StreamAdapter<T>
where
    TState: Clone + fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
    T: StreamTask<TState, TResourceKey> + 'static,
{
    fn name(&self) -> Cow<'static, str> {
        self.0.name()
    }
    fn open_session<'a>(
        &'a self,
        res: &'a Resources<TResourceKey>,
        cursor_bytes: Option<Vec<u8>>,
        attempt_timeout: Option<std::time::Duration>,
    ) -> OpenSessionFuture<'a, TState, TResourceKey> {
        Box::pin(async move {
            let cursor: Option<T::Cursor> = match cursor_bytes {
                None => None,
                Some(ref b) => Some(serde_json::from_slice(b).map_err(|e| {
                    CanoError::task_execution(format!(
                        "deserialize stream cursor for `{}`: {e}",
                        self.0.name()
                    ))
                })?),
            };
            let stream = self.0.open(res, cursor).await?;
            let session = StreamSession {
                task: Arc::clone(&self.0),
                stream,
                window: self.0.window(),
                policy: self.0.on_item_error(),
                attempt_timeout,
                consecutive_errors: 0,
            };
            Ok(Box::new(session) as Box<dyn ErasedStreamSession<TState, TResourceKey>>)
        })
    }
}

fn encode_cursor<C: Serialize>(cursor: &C, task_name: &str) -> Result<Vec<u8>, CanoError> {
    serde_json::to_vec(cursor).map_err(|e| {
        CanoError::task_execution(format!("serialize stream cursor for `{task_name}`: {e}"))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task;
    use crate::task::Task;
    use futures_util::stream;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum Step {
        Consume,
        Done,
    }

    #[test]
    fn value_type_defaults() {
        assert_eq!(StreamErrorPolicy::default(), StreamErrorPolicy::FailFast);
        let _ = StreamWindow::Count(8);
        let _ = StreamWindow::Duration(std::time::Duration::from_millis(5));
        assert_eq!(CloseReason::Exhausted, CloseReason::Exhausted);
    }

    // Note: in-crate impls use the trait-impl form (`impl StreamTask<S> for T`); the
    // inherent form emits `::cano::` paths that don't resolve inside this crate. The
    // inherent form is exercised in `cano-macros/tests/stream_task_impl.rs`.

    #[derive(Default)]
    struct Collector {
        seen: Mutex<Vec<u32>>,
        windows: Mutex<Vec<Vec<u32>>>,
    }

    #[task::stream]
    impl StreamTask<Step> for Collector {
        type Item = u32;
        type Output = u32;
        type Cursor = u64;

        fn window(&self) -> StreamWindow {
            StreamWindow::Count(2)
        }

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(vec![10u32, 20, 30, 40, 50]))
                as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u32) -> Result<(u32, u64), CanoError> {
            self.seen.lock().unwrap().push(item);
            Ok((item * 2, item as u64))
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            outputs: Vec<u32>,
        ) -> Result<WindowSignal<Step>, CanoError> {
            self.windows.lock().unwrap().push(outputs);
            Ok(WindowSignal::Continue)
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn in_memory_windows_and_order() {
        let task = Collector::default();
        let res = Resources::new();
        let result = Task::run(&task, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
        assert_eq!(*task.seen.lock().unwrap(), vec![10, 20, 30, 40, 50]);
        // Count(2): windows [20,40], [60,80], then the partial [100] flushed on close.
        assert_eq!(
            *task.windows.lock().unwrap(),
            vec![vec![20u32, 40], vec![60, 80], vec![100]]
        );
    }

    struct FailOnSecond {
        policy: StreamErrorPolicy,
        flushed: Mutex<Vec<u32>>,
    }

    #[task::stream]
    impl StreamTask<Step> for FailOnSecond {
        type Item = u32;
        type Output = u32;
        type Cursor = u64;

        fn on_item_error(&self) -> StreamErrorPolicy {
            self.policy.clone()
        }

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(vec![1u32, 2, 3])) as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u32) -> Result<(u32, u64), CanoError> {
            if item == 2 {
                Err(CanoError::task_execution("item 2 failed"))
            } else {
                Ok((item, item as u64))
            }
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            outputs: Vec<u32>,
        ) -> Result<WindowSignal<Step>, CanoError> {
            self.flushed.lock().unwrap().extend(outputs);
            Ok(WindowSignal::Continue)
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn fail_fast_propagates() {
        let task = FailOnSecond {
            policy: StreamErrorPolicy::FailFast,
            flushed: Mutex::new(Vec::new()),
        };
        let res = Resources::new();
        let err = Task::run(&task, &res).await.unwrap_err();
        assert!(matches!(err, CanoError::TaskExecution(_)));
    }

    #[tokio::test]
    async fn skip_and_continue_drops_bad_item() {
        let task = FailOnSecond {
            policy: StreamErrorPolicy::SkipAndContinue,
            flushed: Mutex::new(Vec::new()),
        };
        let res = Resources::new();
        let result = Task::run(&task, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
        // item 2 dropped; 1 and 3 survive.
        assert_eq!(*task.flushed.lock().unwrap(), vec![1u32, 3]);
    }

    struct StopAfterFirst;

    #[task::stream]
    impl StreamTask<Step> for StopAfterFirst {
        type Item = u32;
        type Output = u32;
        type Cursor = u64;

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(vec![1u32, 2, 3, 4]))
                as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u32) -> Result<(u32, u64), CanoError> {
            Ok((item, item as u64))
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            _outputs: Vec<u32>,
        ) -> Result<WindowSignal<Step>, CanoError> {
            // Window is Count(1); stop after the very first window.
            Ok(WindowSignal::Stop(TaskResult::Single(Step::Done)))
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<Step>, CanoError> {
            panic!("on_close must not run when a window returns Stop");
        }
    }

    #[tokio::test]
    async fn window_stop_short_circuits() {
        let res = Resources::new();
        let result = Task::run(&StopAfterFirst, &res).await.unwrap();
        assert_eq!(result, TaskResult::Single(Step::Done));
    }

    #[tokio::test]
    async fn integrates_with_workflow_via_register() {
        use crate::cancel::CancellationToken;
        use crate::workflow::Workflow;

        let workflow = Workflow::bare()
            .register(Step::Consume, Collector::default())
            .add_exit_state(Step::Done);
        let result = workflow
            .orchestrate(Step::Consume, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(result, Step::Done);
    }

    // -----------------------------------------------------------------------
    // Engine-path tests: register_stream + cancellation + cursor persistence
    // -----------------------------------------------------------------------

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Minimal in-memory `CheckpointStore` for the resume test.
    #[derive(Default)]
    struct InMemoryStore {
        rows: Mutex<HashMap<String, Vec<crate::recovery::CheckpointRow>>>,
    }

    #[crate::checkpoint_store]
    impl crate::recovery::CheckpointStore for InMemoryStore {
        async fn append(
            &self,
            workflow_id: &str,
            row: crate::recovery::CheckpointRow,
        ) -> Result<(), CanoError> {
            let mut g = self.rows.lock().unwrap();
            let v = g.entry(workflow_id.to_string()).or_default();
            if v.iter().any(|r| r.sequence == row.sequence) {
                return Err(CanoError::checkpoint_store("duplicate sequence"));
            }
            v.push(row);
            Ok(())
        }
        async fn load_run(
            &self,
            workflow_id: &str,
        ) -> Result<Vec<crate::recovery::CheckpointRow>, CanoError> {
            let g = self.rows.lock().unwrap();
            let mut v = g.get(workflow_id).cloned().unwrap_or_default();
            v.sort_by_key(|r| r.sequence);
            Ok(v)
        }
        async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
            self.rows.lock().unwrap().remove(workflow_id);
            Ok(())
        }
    }

    struct Forever {
        closed_cancelled: Arc<AtomicBool>,
        flushed_windows: Arc<AtomicU32>,
    }

    #[task::stream]
    impl StreamTask<Step> for Forever {
        type Item = u64;
        type Output = u64;
        type Cursor = u64;

        fn window(&self) -> StreamWindow {
            StreamWindow::Count(2)
        }

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
            // Effectively infinite source.
            Ok(Box::pin(stream::iter(0u64..)) as Pin<Box<dyn Stream<Item = u64> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u64) -> Result<(u64, u64), CanoError> {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            Ok((item, item))
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            _outputs: Vec<u64>,
        ) -> Result<WindowSignal<Step>, CanoError> {
            self.flushed_windows.fetch_add(1, Ordering::SeqCst);
            Ok(WindowSignal::Continue)
        }

        async fn on_close(
            &self,
            _res: &Resources,
            reason: CloseReason,
        ) -> Result<TaskResult<Step>, CanoError> {
            if reason == CloseReason::Cancelled {
                self.closed_cancelled.store(true, Ordering::SeqCst);
            }
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn cancel_drains_and_surfaces_cancelled() {
        use crate::cancel::CancellationToken;
        use crate::workflow::Workflow;

        let closed = Arc::new(AtomicBool::new(false));
        let task = Forever {
            closed_cancelled: Arc::clone(&closed),
            flushed_windows: Arc::new(AtomicU32::new(0)),
        };
        let (handle, token) = CancellationToken::new();
        let workflow = Workflow::bare()
            .register_stream(Step::Consume, task)
            .add_exit_state(Step::Done);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            handle.cancel();
        });

        let result = workflow.orchestrate(Step::Consume, token).await;
        assert!(
            matches!(&result, Err(e) if e.category() == "cancelled"),
            "a cancelled stream must surface as cancelled, got {result:?}"
        );
        assert!(
            closed.load(Ordering::SeqCst),
            "on_close(Cancelled) must run (cooperative drain reached the close hook)"
        );
    }

    struct Resumable {
        opened: Arc<Mutex<Vec<Option<u64>>>>,
        processed: Arc<Mutex<Vec<u64>>>,
        fail_third: Arc<AtomicBool>,
    }

    #[task::stream]
    impl StreamTask<Step> for Resumable {
        type Item = u64;
        type Output = u64;
        type Cursor = u64;

        fn window(&self) -> StreamWindow {
            StreamWindow::Count(2)
        }

        async fn open(
            &self,
            _res: &Resources,
            cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
            self.opened.lock().unwrap().push(cursor);
            let start = cursor.map(|c| c + 1).unwrap_or(1);
            let items: Vec<u64> = (start..=6).collect();
            Ok(Box::pin(stream::iter(items)) as Pin<Box<dyn Stream<Item = u64> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u64) -> Result<(u64, u64), CanoError> {
            self.processed.lock().unwrap().push(item);
            Ok((item, item)) // cursor == item id
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            outputs: Vec<u64>,
        ) -> Result<WindowSignal<Step>, CanoError> {
            // Simulate a crash flushing the [5,6] window — only on the first run.
            if outputs == vec![5u64, 6] && self.fail_third.swap(false, Ordering::SeqCst) {
                return Err(CanoError::task_execution("simulated crash in window [5,6]"));
            }
            Ok(WindowSignal::Continue)
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn persists_cursor_and_resumes() {
        use crate::cancel::CancellationToken;
        use crate::recovery::{CheckpointStore, RowKind};
        use crate::workflow::Workflow;

        let opened = Arc::new(Mutex::new(Vec::new()));
        let processed = Arc::new(Mutex::new(Vec::new()));
        let task = Resumable {
            opened: Arc::clone(&opened),
            processed: Arc::clone(&processed),
            fail_third: Arc::new(AtomicBool::new(true)),
        };
        let store = Arc::new(InMemoryStore::default());
        let workflow = Workflow::bare()
            .register_stream(Step::Consume, task)
            .add_exit_state(Step::Done)
            .with_checkpoint_store(store.clone())
            .with_workflow_id("resume-test");

        // Run 1: fails flushing window [5,6].
        let r1 = workflow
            .orchestrate(Step::Consume, CancellationToken::disabled())
            .await;
        assert!(r1.is_err(), "run 1 should fail mid-stream: {r1:?}");

        // Windows [1,2] (cursor 2) and [3,4] (cursor 4) committed; [5,6] failed.
        let rows = store.load_run("resume-test").await.unwrap();
        let cursors: Vec<u64> = rows
            .iter()
            .filter(|r| r.kind == RowKind::StepCursor)
            .map(|r| serde_json::from_slice::<u64>(r.output_blob.as_ref().unwrap()).unwrap())
            .collect();
        assert_eq!(
            cursors,
            vec![2, 4],
            "only fully-flushed windows commit a cursor"
        );

        // Resume: re-open at cursor 4 and finish [5,6].
        let r2 = workflow
            .resume_from("resume-test", CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(r2, Step::Done);

        assert_eq!(*opened.lock().unwrap(), vec![None, Some(4)]);
        assert_eq!(
            *processed.lock().unwrap(),
            vec![1u64, 2, 3, 4, 5, 6, 5, 6],
            "resume reprocesses only the items after the committed cursor"
        );
    }

    // -----------------------------------------------------------------------
    // Fix 1: WindowSignal::Stop from the terminal (exhaustion) partial flush is honored.
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum S3 {
        Consume,
        ViaStop,
        ViaClose,
    }

    struct StopOnFinalWindow;

    #[task::stream]
    impl StreamTask<S3> for StopOnFinalWindow {
        type Item = u32;
        type Output = u32;
        type Cursor = u64;

        fn window(&self) -> StreamWindow {
            StreamWindow::Count(3) // never fills for a 2-item stream → terminal partial flush
        }

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(vec![1u32, 2])) as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u32) -> Result<(u32, u64), CanoError> {
            Ok((item, item as u64))
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            _outputs: Vec<u32>,
        ) -> Result<WindowSignal<S3>, CanoError> {
            Ok(WindowSignal::Stop(TaskResult::Single(S3::ViaStop)))
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<S3>, CanoError> {
            // Must NOT run: the terminal partial flush returned Stop.
            Ok(TaskResult::Single(S3::ViaClose))
        }
    }

    #[tokio::test]
    async fn terminal_flush_stop_is_honored() {
        use crate::cancel::CancellationToken;
        use crate::workflow::Workflow;

        let workflow = Workflow::bare()
            .register_stream(S3::Consume, StopOnFinalWindow)
            .add_exit_states([S3::ViaStop, S3::ViaClose]);
        let result = workflow
            .orchestrate(S3::Consume, CancellationToken::disabled())
            .await
            .unwrap();
        assert_eq!(
            result,
            S3::ViaStop,
            "a Stop from the final partial flush must win over on_close(Exhausted)"
        );
    }

    // -----------------------------------------------------------------------
    // Fix 2: cooperative cancel fires on_cancelled exactly once.
    // -----------------------------------------------------------------------

    #[derive(Default)]
    struct CancelCounter {
        cancels: AtomicU32,
    }

    impl crate::observer::WorkflowObserver for CancelCounter {
        fn on_cancelled(&self, _state: &str) {
            self.cancels.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancel_fires_on_cancelled_once() {
        use crate::cancel::CancellationToken;
        use crate::workflow::Workflow;

        let task = Forever {
            closed_cancelled: Arc::new(AtomicBool::new(false)),
            flushed_windows: Arc::new(AtomicU32::new(0)),
        };
        let counter = Arc::new(CancelCounter::default());
        let (handle, token) = CancellationToken::new();
        let workflow = Workflow::bare()
            .register_stream(Step::Consume, task)
            .add_exit_state(Step::Done)
            .with_observer(counter.clone());

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            handle.cancel();
        });

        let result = workflow.orchestrate(Step::Consume, token).await;
        assert!(matches!(&result, Err(e) if e.category() == "cancelled"));
        assert_eq!(
            counter.cancels.load(Ordering::SeqCst),
            1,
            "on_cancelled must fire exactly once on a stream cancel"
        );
    }

    // -----------------------------------------------------------------------
    // Fix 4: config().attempt_timeout bounds each process_item.
    // -----------------------------------------------------------------------

    struct SlowItem;

    #[task::stream]
    impl StreamTask<Step> for SlowItem {
        type Item = u32;
        type Output = u32;
        type Cursor = u64;

        fn config(&self) -> TaskConfig {
            TaskConfig::minimal().with_attempt_timeout(std::time::Duration::from_millis(10))
        }

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(vec![1u32])) as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u32) -> Result<(u32, u64), CanoError> {
            // Far longer than the 10ms attempt_timeout.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok((item, item as u64))
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            _outputs: Vec<u32>,
        ) -> Result<WindowSignal<Step>, CanoError> {
            Ok(WindowSignal::Continue)
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<Step>, CanoError> {
            Ok(TaskResult::Single(Step::Done))
        }
    }

    #[tokio::test]
    async fn attempt_timeout_bounds_process_item() {
        use crate::cancel::CancellationToken;
        use crate::workflow::Workflow;

        let workflow = Workflow::bare()
            .register_stream(Step::Consume, SlowItem)
            .add_exit_state(Step::Done);
        // FailFast (default) → the timed-out item fails the run promptly.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            workflow.orchestrate(Step::Consume, CancellationToken::disabled()),
        )
        .await
        .expect("attempt_timeout must bound the hung process_item well under 5s");
        assert!(
            matches!(&result, Err(e) if e.category() == "timeout"),
            "a process_item exceeding attempt_timeout must surface a timeout error, got {result:?}"
        );
    }
}

#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use super::*;
    use crate::cancel::CancellationToken;
    use crate::metrics::test_support::*;
    use crate::task;
    use crate::task::Task;
    use crate::workflow::Workflow;
    use futures_util::stream;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum St {
        Consume,
        Done,
    }

    struct FiveItems;

    #[task::stream]
    impl StreamTask<St> for FiveItems {
        type Item = u32;
        type Output = u32;
        type Cursor = u64;

        fn window(&self) -> StreamWindow {
            StreamWindow::Count(2)
        }

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(vec![1u32, 2, 3, 4, 5]))
                as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u32) -> Result<(u32, u64), CanoError> {
            Ok((item, item as u64))
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            _outputs: Vec<u32>,
        ) -> Result<WindowSignal<St>, CanoError> {
            Ok(WindowSignal::Continue)
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<St>, CanoError> {
            Ok(TaskResult::Single(St::Done))
        }
    }

    #[test]
    fn stream_metrics_counted_correctly() {
        let (result, rows) = run_with_recorder(|| async {
            let workflow = Workflow::bare()
                .register_stream(St::Consume, FiveItems)
                .add_exit_state(St::Done);
            workflow
                .orchestrate(St::Consume, CancellationToken::disabled())
                .await
        });
        assert!(result.is_ok(), "workflow should succeed: {result:?}");
        assert_eq!(
            counter(&rows, "cano_stream_runs_total", &[("outcome", "completed")]),
            1,
            "one completed stream run"
        );
        // Count(2) over 5 items → windows [1,2], [3,4], then partial [5] on close.
        assert_eq!(
            counter(&rows, "cano_stream_windows_total", &[]),
            3,
            "three windows flushed"
        );
        assert_eq!(
            counter(&rows, "cano_stream_items_total", &[("result", "ok")]),
            5,
            "five ok items"
        );
    }

    /// Cancels itself after the first window (deterministic — no spawn/sleep).
    struct SelfCancel {
        handle: crate::cancel::CancellationHandle,
    }

    #[task::stream]
    impl StreamTask<St> for SelfCancel {
        type Item = u32;
        type Output = u32;
        type Cursor = u64;

        async fn open(
            &self,
            _res: &Resources,
            _cursor: Option<u64>,
        ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
            Ok(Box::pin(stream::iter(0u32..)) as Pin<Box<dyn Stream<Item = u32> + Send>>)
        }

        async fn process_item(&self, _res: &Resources, item: u32) -> Result<(u32, u64), CanoError> {
            Ok((item, item as u64))
        }

        async fn flush_window(
            &self,
            _res: &Resources,
            _outputs: Vec<u32>,
        ) -> Result<WindowSignal<St>, CanoError> {
            // Default window is Count(1); fire cancel after the first window — the next
            // loop iteration observes it and drains cooperatively.
            self.handle.cancel();
            Ok(WindowSignal::Continue)
        }

        async fn on_close(
            &self,
            _res: &Resources,
            _reason: CloseReason,
        ) -> Result<TaskResult<St>, CanoError> {
            Ok(TaskResult::Single(St::Done))
        }
    }

    #[test]
    fn cancelled_stream_records_cancelled_outcome() {
        let (handle, token) = CancellationToken::new();
        let (result, rows) = run_with_recorder(|| async move {
            let workflow = Workflow::bare()
                .register_stream(St::Consume, SelfCancel { handle })
                .add_exit_state(St::Done);
            workflow.orchestrate(St::Consume, token).await
        });
        assert!(result.is_err(), "a cancelled run is Err: {result:?}");
        assert_eq!(
            counter(&rows, "cano_stream_runs_total", &[("outcome", "cancelled")]),
            1,
            "a cooperative cancel is recorded as cancelled, not failed"
        );
    }
}
