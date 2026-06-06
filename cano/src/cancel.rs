//! Cooperative cancellation for workflow runs.
//!
//! [`CancellationToken`] / [`CancellationHandle`] form a clonable signal pair built on
//! [`tokio::sync::watch`] — no extra dependency. Hand a token to
//! [`Workflow::orchestrate_with_cancel`](crate::workflow::Workflow::orchestrate_with_cancel)
//! (or [`resume_from_with_cancel`](crate::workflow::Workflow::resume_from_with_cancel)) and keep
//! the handle; calling [`CancellationHandle::cancel`] aborts the in-flight cancellable task at its
//! next await point, drains the saga compensation stack, and surfaces
//! [`CanoError::Cancelled`](crate::error::CanoError::Cancelled).
//!
//! Cancellation is **cooperative**: the engine drops the running task future at its next `.await`,
//! so a task doing uninterrupted synchronous/CPU work is not interrupted until it next yields. A
//! [`CompensatableTask`](crate::saga::CompensatableTask) is deliberately *never* interrupted
//! mid-run (that would orphan a committed side effect with no entry to roll back) — it runs to
//! completion and the cancel is honoured at the next state boundary. The compensation drain itself
//! is uncancellable.
//!
//! ```
//! use cano::prelude::*;
//! use cano::CancellationToken;
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash)]
//! enum Step { Start, Done }
//!
//! struct Noop;
//! #[task]
//! impl Task<Step> for Noop {
//!     async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
//!         Ok(TaskResult::Single(Step::Done))
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() {
//! let (handle, token) = CancellationToken::new();
//! let workflow = Workflow::bare()
//!     .register(Step::Start, Noop)
//!     .add_exit_state(Step::Done);
//!
//! // Cancel from anywhere (another task, a signal handler, …):
//! handle.cancel();
//!
//! let result = workflow.orchestrate_with_cancel(Step::Start, token).await;
//! assert!(matches!(result, Err(e) if e.category() == "cancelled"));
//! # }
//! ```

/// The observing half of a cancellation signal. Clonable and cheap to pass into a workflow.
///
/// A token built via [`CancellationToken::new`] observes its paired [`CancellationHandle`]; the
/// internal "never" token (used by [`orchestrate`](crate::workflow::Workflow::orchestrate) and
/// [`resume_from`](crate::workflow::Workflow::resume_from)) never fires and adds no overhead.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    rx: Option<tokio::sync::watch::Receiver<bool>>,
}

/// The controlling half of a cancellation signal. Call [`cancel`](Self::cancel) to fire it.
///
/// Clonable, so several owners can trigger cancellation; [`cancel`](Self::cancel) is idempotent.
#[derive(Clone, Debug)]
pub struct CancellationHandle {
    tx: tokio::sync::watch::Sender<bool>,
}

impl CancellationToken {
    /// Create a fresh handle/token pair. The token is not cancelled until the handle's
    /// [`cancel`](CancellationHandle::cancel) is called.
    #[must_use]
    pub fn new() -> (CancellationHandle, CancellationToken) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (
            CancellationHandle { tx },
            CancellationToken { rx: Some(rx) },
        )
    }

    /// The internal "never cancels" token. No channel is allocated, so the no-token path the
    /// existing entry points use stays allocation- and overhead-free.
    pub(crate) fn never() -> Self {
        Self { rx: None }
    }

    /// Whether this token has already been cancelled. Non-blocking poll.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.rx.as_ref().is_some_and(|rx| *rx.borrow())
    }

    /// Whether this token can ever observe a cancellation. `false` for the internal "never"
    /// token, letting the FSM hot path skip the cancellation `select!` entirely.
    pub(crate) fn can_cancel(&self) -> bool {
        self.rx.is_some()
    }

    /// Resolve once the token is cancelled. A "never" token (or one whose handle was dropped
    /// without cancelling) stays pending forever — making it safe to use as a `select!` branch
    /// that simply never wins.
    pub async fn cancelled(&self) {
        match &self.rx {
            None => std::future::pending::<()>().await,
            Some(rx) => {
                let mut rx = rx.clone();
                if *rx.borrow() {
                    return;
                }
                while rx.changed().await.is_ok() {
                    if *rx.borrow() {
                        return;
                    }
                }
                // Sender dropped without ever sending `true`: never cancels.
                std::future::pending::<()>().await;
            }
        }
    }
}

impl CancellationHandle {
    /// Signal cancellation to every token observing this handle. Idempotent — calling it again
    /// after the first cancel is a no-op.
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancel_propagates_to_receivers() {
        let (handle, token) = CancellationToken::new();
        assert!(!token.is_cancelled());
        handle.cancel();
        // `cancelled()` resolves promptly.
        tokio::time::timeout(Duration::from_secs(1), token.cancelled())
            .await
            .expect("cancelled() should resolve after cancel");
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn clone_after_cancel_still_observes() {
        let (handle, token) = CancellationToken::new();
        handle.cancel();
        let cloned = token.clone();
        assert!(cloned.is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), cloned.cancelled())
            .await
            .expect("a clone made after cancel still observes it");
    }

    #[test]
    fn is_cancelled_polls_without_await() {
        let (handle, token) = CancellationToken::new();
        assert!(!token.is_cancelled());
        handle.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_is_idempotent() {
        let (handle, token) = CancellationToken::new();
        handle.cancel();
        handle.cancel(); // second call is a no-op
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn dropping_handle_without_cancel_keeps_token_pending() {
        let (handle, token) = CancellationToken::new();
        drop(handle);
        assert!(!token.is_cancelled());
        // `cancelled()` must NOT resolve just because the sender dropped.
        let res = tokio::time::timeout(Duration::from_millis(50), token.cancelled()).await;
        assert!(
            res.is_err(),
            "cancelled() should stay pending after handle drop"
        );
    }

    #[tokio::test]
    async fn never_is_never_cancelled_and_can_cancel_false() {
        let token = CancellationToken::never();
        assert!(!token.is_cancelled());
        assert!(!token.can_cancel());
        let res = tokio::time::timeout(Duration::from_millis(50), token.cancelled()).await;
        assert!(res.is_err(), "never token should stay pending");
    }

    #[test]
    fn can_cancel_true_for_new_token() {
        let (_handle, token) = CancellationToken::new();
        assert!(token.can_cancel());
    }

    // Cancel fires *after* the await begins — exercises the `rx.changed().await`
    // wakeup path (the other tests cancel first and hit the fast-path `borrow()`).
    #[tokio::test]
    async fn cancelled_resolves_when_cancel_fires_while_awaiting() {
        let (handle, token) = CancellationToken::new();
        let waiter = tokio::spawn(async move { token.cancelled().await });
        // Let the waiter park inside `changed().await` before cancelling.
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.cancel();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter should wake on cancel")
            .expect("waiter task should not panic");
    }

    // A clone taken *before* the cancel still observes it (both halves share state).
    #[tokio::test]
    async fn clone_before_cancel_is_observed() {
        let (handle, token) = CancellationToken::new();
        let cloned = token.clone();
        assert!(!cloned.is_cancelled());
        handle.cancel();
        assert!(cloned.is_cancelled());
        assert!(token.is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), cloned.cancelled())
            .await
            .expect("a clone made before cancel still resolves");
    }

    // `CancellationHandle` is `Clone`; cancelling via a clone still fires, even after
    // the original handle is dropped.
    #[tokio::test]
    async fn cloned_handle_triggers_cancellation() {
        let (handle, token) = CancellationToken::new();
        let handle2 = handle.clone();
        drop(handle); // only the clone remains
        assert!(!token.is_cancelled());
        handle2.cancel();
        assert!(token.is_cancelled());
    }

    // One cancel wakes every concurrent awaiter.
    #[tokio::test]
    async fn multiple_awaiters_all_wake_on_cancel() {
        let (handle, token) = CancellationToken::new();
        let waiters: Vec<_> = (0..5)
            .map(|_| {
                let t = token.clone();
                tokio::spawn(async move { t.cancelled().await })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.cancel();
        for w in waiters {
            tokio::time::timeout(Duration::from_secs(1), w)
                .await
                .expect("every awaiter should wake")
                .expect("awaiter task should not panic");
        }
    }
}
