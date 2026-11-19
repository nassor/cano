//! Internal async runtime and time shim separating the default Tokio backend
//! (`runtime-tokio`) from the WebAssembly / WASI Preview 3 backend (`wasm`).

#[cfg(not(any(feature = "runtime-tokio", feature = "wasm")))]
compile_error!(
    "cano requires an execution runtime feature: enable `runtime-tokio` (on by default) or `wasm` (for WebAssembly / WASI Preview 3)"
);

#[cfg(all(target_arch = "wasm32", feature = "runtime-tokio"))]
compile_error!(
    "`runtime-tokio` (and features that require it: `scheduler`, `store-redb`, `store-postgres`, `store-nats`, `all`) is not supported on `wasm32` targets; use `default-features = false, features = [\"wasm\"]`"
);

#[cfg(all(
    feature = "wasm",
    not(feature = "runtime-tokio"),
    not(target_arch = "wasm32")
))]
compile_error!(
    "the `wasm` feature (without `runtime-tokio`) requires a `wasm32` target such as `wasm32-wasip3`; for native builds enable `runtime-tokio`"
);

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// Error returned when [`timeout`] or [`timeout_at`] elapses before the inner
/// future completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Elapsed;

#[cfg(feature = "runtime-tokio")]
mod backend {
    use super::*;

    pub(crate) type Instant = tokio::time::Instant;
    pub(crate) type SleepBox = Pin<Box<tokio::time::Sleep>>;

    #[inline]
    pub(crate) fn deadline_after(d: Duration) -> Instant {
        let now = tokio::time::Instant::now();
        now.checked_add(d)
            .unwrap_or_else(|| now + Duration::from_secs(u32::MAX as u64))
    }

    #[inline]
    pub(crate) fn instant_from_std(i: std::time::Instant) -> Instant {
        tokio::time::Instant::from_std(i)
    }

    #[inline]
    pub(crate) fn is_elapsed(deadline: Instant) -> bool {
        tokio::time::Instant::now() >= deadline
    }

    #[inline]
    pub(crate) async fn sleep(d: Duration) {
        tokio::time::sleep(d).await;
    }

    #[inline]
    pub(crate) async fn sleep_until(deadline: Instant) {
        tokio::time::sleep_until(deadline).await;
    }

    #[inline]
    pub(crate) fn sleep_until_boxed(deadline: Instant) -> SleepBox {
        Box::pin(tokio::time::sleep_until(deadline))
    }

    #[inline]
    pub(crate) async fn timeout<F: Future>(d: Duration, fut: F) -> Result<F::Output, Elapsed> {
        tokio::time::timeout(d, fut).await.map_err(|_| Elapsed)
    }

    #[inline]
    pub(crate) async fn timeout_at<F: Future>(
        deadline: Instant,
        fut: F,
    ) -> Result<F::Output, Elapsed> {
        tokio::time::timeout_at(deadline, fut)
            .await
            .map_err(|_| Elapsed)
    }

    #[inline]
    pub(crate) async fn yield_now() {
        tokio::task::yield_now().await;
    }

    pub(crate) struct JoinSet<T>(tokio::task::JoinSet<T>);

    impl<T: 'static> JoinSet<T> {
        #[inline]
        pub(crate) fn new() -> Self {
            Self(tokio::task::JoinSet::new())
        }

        #[inline]
        pub(crate) fn spawn<F>(&mut self, task: F)
        where
            F: Future<Output = T> + Send + 'static,
            T: Send,
        {
            self.0.spawn(task);
        }

        #[inline]
        pub(crate) async fn join_next(&mut self) -> Option<Result<T, tokio::task::JoinError>> {
            self.0.join_next().await
        }

        #[inline]
        pub(crate) fn abort_all(&mut self) {
            self.0.abort_all();
        }
    }
}

#[cfg(all(
    not(feature = "runtime-tokio"),
    feature = "wasm",
    target_arch = "wasm32"
))]
mod backend {
    use super::*;
    use futures_util::StreamExt as _;
    use futures_util::stream::FuturesUnordered;
    use std::task::Poll;
    use wasip3::clocks::monotonic_clock;

    /// Monotonic clock deadline backed by WASI P3 `monotonic_clock::Mark` (nanoseconds).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub(crate) struct Instant(u64);

    pub(crate) type SleepBox = Pin<Box<dyn Future<Output = ()> + Send>>;

    #[inline]
    fn duration_to_nanos(d: Duration) -> u64 {
        u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
    }

    #[inline]
    pub(crate) fn deadline_after(d: Duration) -> Instant {
        Instant(monotonic_clock::now().saturating_add(duration_to_nanos(d)))
    }

    #[inline]
    pub(crate) fn instant_from_std(i: std::time::Instant) -> Instant {
        deadline_after(i.saturating_duration_since(std::time::Instant::now()))
    }

    #[inline]
    pub(crate) fn is_elapsed(deadline: Instant) -> bool {
        monotonic_clock::now() >= deadline.0
    }

    #[inline]
    pub(crate) async fn sleep(d: Duration) {
        if !d.is_zero() {
            monotonic_clock::wait_for(duration_to_nanos(d)).await;
        }
    }

    #[inline]
    pub(crate) async fn sleep_until(deadline: Instant) {
        if !is_elapsed(deadline) {
            monotonic_clock::wait_until(deadline.0).await;
        }
    }

    #[inline]
    pub(crate) fn sleep_until_boxed(deadline: Instant) -> SleepBox {
        Box::pin(sleep_until(deadline))
    }

    #[inline]
    async fn race_delay<F: Future, D: Future<Output = ()>>(
        fut: F,
        delay: D,
    ) -> Result<F::Output, Elapsed> {
        let mut fut = std::pin::pin!(fut);
        let mut delay = std::pin::pin!(delay);
        std::future::poll_fn(move |cx| {
            if let Poll::Ready(out) = fut.as_mut().poll(cx) {
                return Poll::Ready(Ok(out));
            }
            if delay.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(Elapsed));
            }
            Poll::Pending
        })
        .await
    }

    #[inline]
    pub(crate) async fn timeout<F: Future>(d: Duration, fut: F) -> Result<F::Output, Elapsed> {
        race_delay(fut, sleep(d)).await
    }

    #[inline]
    pub(crate) async fn timeout_at<F: Future>(
        deadline: Instant,
        fut: F,
    ) -> Result<F::Output, Elapsed> {
        race_delay(fut, sleep_until(deadline)).await
    }

    #[inline]
    pub(crate) async fn yield_now() {
        let mut yielded = false;
        std::future::poll_fn(move |cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    #[derive(Debug)]
    pub(crate) struct JoinError;

    pub(crate) struct JoinSet<T>(FuturesUnordered<Pin<Box<dyn Future<Output = T> + Send>>>);

    impl<T: 'static> JoinSet<T> {
        #[inline]
        pub(crate) fn new() -> Self {
            Self(FuturesUnordered::new())
        }

        #[inline]
        pub(crate) fn spawn<F>(&mut self, task: F)
        where
            F: Future<Output = T> + Send + 'static,
            T: Send,
        {
            self.0.push(Box::pin(task));
        }

        #[inline]
        pub(crate) async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
            self.0.next().await.map(Ok)
        }

        #[inline]
        pub(crate) fn abort_all(&mut self) {
            self.0.clear();
        }
    }
}

pub(crate) use backend::{
    Instant, JoinSet, SleepBox, deadline_after, instant_from_std, is_elapsed, sleep, sleep_until,
    sleep_until_boxed, timeout, timeout_at, yield_now,
};
