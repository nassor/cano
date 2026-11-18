//! Bounded, classified attempts for the network-backed checkpoint stores.
//!
//! Unlike the task retry loop, a store distinguishes transient failures (a dropped
//! connection, a timeout) from fatal ones (a conflict, a corrupt row) and exposes the
//! attempt index, so an append can recognise its own earlier write on a retry.

use std::future::Future;
use std::time::Duration;

use rand::RngExt;

use crate::RetryMode;
use crate::error::CanoError;

/// One failed attempt and whether another attempt may succeed.
pub(super) struct Failure {
    pub(super) error: CanoError,
    pub(super) transient: bool,
}

impl Failure {
    pub(super) fn transient(error: CanoError) -> Self {
        Self {
            error,
            transient: true,
        }
    }

    pub(super) fn fatal(error: CanoError) -> Self {
        Self {
            error,
            transient: false,
        }
    }
}

/// Equal jitter in `[50%, 100%]` so concurrent instances retrying after a store outage
/// desynchronize without ever exceeding the caller's configured delay.
fn jittered_delay(delay: Duration) -> Duration {
    if delay.is_zero() {
        return Duration::ZERO;
    }
    let factor = rand::rng().random_range(0.5_f64..=1.0);
    delay.mul_f64(factor).max(Duration::from_nanos(1))
}

/// Calls `attempt(n)` for n = 0, 1, ...; sleeps a jittered `retry.delay_for_attempt(n)`
/// after a transient failure and stops with that failure's error when it returns `None` or
/// the failure is fatal.
pub(super) async fn run<T, F, Fut>(retry: &RetryMode, mut attempt: F) -> Result<T, CanoError>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<T, Failure>>,
{
    let mut n = 0;
    loop {
        let failure = match attempt(n).await {
            Ok(value) => return Ok(value),
            Err(failure) => failure,
        };
        if !failure.transient {
            return Err(failure.error);
        }
        match retry.delay_for_attempt(n) {
            Some(delay) => {
                let sleep_for = jittered_delay(delay);
                if !sleep_for.is_zero() {
                    tokio::time::sleep(sleep_for).await;
                }
            }
            None => return Err(failure.error),
        }
        n += 1;
    }
}

/// `tokio::time::timeout(limit, fut)`; an elapsed limit is a transient
/// `"{what}: timed out after {limit:?}"` failure.
pub(super) async fn bounded<T>(
    what: &str,
    limit: Duration,
    fut: impl Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(Failure::transient(CanoError::checkpoint_store(format!(
            "{what}: timed out after {limit:?}"
        )))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn recording(
        calls: &Mutex<Vec<usize>>,
        transient: bool,
    ) -> impl FnMut(usize) -> std::future::Ready<Result<(), Failure>> + '_ {
        move |n| {
            calls.lock().unwrap().push(n);
            let error = CanoError::checkpoint_store(format!("attempt {n}"));
            std::future::ready(Err(if transient {
                Failure::transient(error)
            } else {
                Failure::fatal(error)
            }))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failures_use_every_attempt_in_order() {
        let calls = Mutex::new(Vec::new());
        let mode = RetryMode::fixed(3, Duration::from_millis(50));
        let err = run(&mode, recording(&calls, true)).await.unwrap_err();
        assert_eq!(*calls.lock().unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(calls.lock().unwrap().len(), mode.max_attempts());
        assert!(err.to_string().contains("attempt 3"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_fatal_failure_stops_at_once() {
        let calls = Mutex::new(Vec::new());
        let mode = RetryMode::fixed(3, Duration::from_millis(50));
        run(&mode, recording(&calls, false)).await.unwrap_err();
        assert_eq!(*calls.lock().unwrap(), vec![0]);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_none_attempts_once() {
        let calls = Mutex::new(Vec::new());
        run(&RetryMode::None, recording(&calls, true))
            .await
            .unwrap_err();
        assert_eq!(*calls.lock().unwrap(), vec![0]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_later_success_is_returned() {
        let value = run(
            &RetryMode::fixed(5, Duration::from_millis(10)),
            |n| async move {
                if n < 2 {
                    Err(Failure::transient(CanoError::checkpoint_store("flaky")))
                } else {
                    Ok(n)
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(value, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_turns_an_elapsed_limit_into_a_transient_failure() {
        let failure = bounded("op", Duration::from_secs(1), async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<(), Failure>(())
        })
        .await
        .expect_err("must time out");
        assert!(failure.transient);
        assert!(
            failure.error.to_string().contains("op: timed out after 1s"),
            "{}",
            failure.error
        );
    }

    #[test]
    fn jittered_delay_stays_within_half_to_full_and_varies() {
        assert_eq!(jittered_delay(Duration::ZERO), Duration::ZERO);
        let base = Duration::from_millis(100);
        let samples: Vec<Duration> = (0..64).map(|_| jittered_delay(base)).collect();
        for &d in &samples {
            assert!(
                d >= Duration::from_millis(50) && d <= base,
                "jittered delay {d:?} outside [50ms, 100ms]"
            );
        }
        assert!(
            samples.windows(2).any(|w| w[0] != w[1]),
            "jittered delays must vary across samples: {samples:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn run_applies_jitter_to_fixed_retry_sleeps() {
        let mode = RetryMode::fixed(8, Duration::from_millis(100));
        let mut elapsed = Vec::new();
        for _ in 0..8 {
            let start = tokio::time::Instant::now();
            let _ = run(&mode, |_| async {
                Err::<(), _>(Failure::transient(CanoError::checkpoint_store("transient")))
            })
            .await;
            let total = start.elapsed();
            assert!(
                total >= Duration::from_millis(400) && total <= Duration::from_millis(800),
                "total jittered retry sleep {total:?} outside [400ms, 800ms]"
            );
            elapsed.push(total);
        }
        assert!(
            elapsed.windows(2).any(|w| w[0] != w[1]),
            "retry totals must reflect randomized jitter: {elapsed:?}"
        );
    }
}
