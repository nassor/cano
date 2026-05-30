//! # Rate Limiter — Token Bucket for Outbound Call Throttling
//!
//! A [`RateLimiter`] caps how fast tasks make outbound calls, smoothing bursty load into a
//! steady rate a downstream dependency can absorb. Where a [`CircuitBreaker`](crate::circuit::CircuitBreaker)
//! stops calls to a dependency that is *down*, a rate limiter paces calls to a dependency
//! that is *up but rate-sensitive* (a third-party API with a quota, a database, an LLM
//! endpoint billed per request).
//!
//! ## The algorithm: a token bucket
//!
//! The limiter holds a bucket of fractional `tokens`. Each acquisition consumes one token;
//! tokens replenish at a fixed `tokens_per_period / refill_period` rate, up to a maximum
//! capacity. Refill is **lazy**: there is no background task. Every
//! [`try_acquire`](RateLimiter::try_acquire) reads a single [`Instant`], adds
//! `elapsed × refill_per_sec` tokens (capped at capacity), then decides. A workflow that
//! never constructs a limiter pays nothing.
//!
//! The bucket starts **full**, so a burst of up to `capacity` calls is admitted instantly;
//! sustained traffic then settles to the refill rate. Capacity is
//! `max_tokens + burst` — `max_tokens` is the steady-state ceiling and `burst` is extra
//! headroom for short spikes.
//!
//! ## Acquiring
//!
//! - [`try_acquire`](RateLimiter::try_acquire) — non-blocking; returns `Some(Permit)` if a
//!   token was available, `None` if the bucket is empty. Use it to shed load.
//! - [`acquire`](RateLimiter::acquire) — async; if the bucket is empty it computes exactly
//!   how long until the next token refills, `tokio::time::sleep`s that long, and retries.
//!   Use it to *pace* work.
//!
//! A [`Permit`] is a lightweight RAII marker for the rate-limited call's scope. Unlike a
//! [`CircuitBreaker`](crate::circuit::CircuitBreaker) permit (which records a success/failure
//! outcome) or a semaphore permit (which returns capacity on drop), a token-bucket permit's
//! drop is a **no-op** — the token was already spent at acquisition time and the bucket
//! refills on the clock, not on release.
//!
//! ## Sharing
//!
//! A limiter is cheap to clone (`Arc` inside). **Share one `Arc<RateLimiter>` across every
//! task that hits the same quota** so the budget is enforced globally, including across tasks
//! running in parallel inside a [split/join](crate::workflow) state. Internally it's a
//! synchronous [`parking_lot::Mutex`] with no awaits held across the critical section.
//!
//! [`RateLimiter`] also implements [`Resource`] (no-op lifecycle), so it can be registered in
//! [`Resources`](crate::resource::Resources) and looked up by key inside a task body.
//!
//! ```rust
//! use std::sync::Arc;
//! use cano::prelude::*;
//!
//! // 5 tokens/sec; the bucket starts full.
//! let limiter = Arc::new(RateLimiter::new(RateLimiterPolicy::per_second(5)));
//! let permit = limiter.try_acquire().expect("a fresh bucket has tokens");
//! drop(permit); // dropping a token-bucket permit is a no-op
//! ```

use crate::resource::Resource;
use cano_macros::resource;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Policy parameters controlling a [`RateLimiter`]'s token bucket.
///
/// Construct one with [`per_second`](Self::per_second) (or [`new`](Self::new) for an
/// arbitrary period) and tune it with the [`with_max_tokens`](Self::with_max_tokens) /
/// [`with_burst`](Self::with_burst) builders.
#[derive(Debug, Clone)]
pub struct RateLimiterPolicy {
    /// Steady-state bucket ceiling: the most tokens the bucket holds in the absence of a
    /// configured `burst`. Also the size of the instantaneous burst a freshly-built limiter
    /// admits (the bucket starts full).
    pub max_tokens: u32,
    /// Tokens added per [`refill_period`](Self::refill_period).
    pub tokens_per_period: u32,
    /// How long it takes to add [`tokens_per_period`](Self::tokens_per_period) tokens.
    pub refill_period: Duration,
    /// Extra capacity above [`max_tokens`](Self::max_tokens) for short spikes. The bucket's
    /// total capacity is `max_tokens + burst`.
    pub burst: u32,
}

impl RateLimiterPolicy {
    /// A policy that refills `tokens` every `period`. `max_tokens` defaults to `tokens` (one
    /// period's worth) and `burst` to `0`.
    pub fn new(tokens: u32, period: Duration) -> Self {
        Self {
            max_tokens: tokens,
            tokens_per_period: tokens,
            refill_period: period,
            burst: 0,
        }
    }
    /// Shorthand for `new(tokens, Duration::from_secs(1))` — `tokens` per second.
    pub fn per_second(tokens: u32) -> Self {
        Self::new(tokens, Duration::from_secs(1))
    }
    /// Override the steady-state bucket ceiling without changing the refill rate.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
    /// Add `burst` tokens of extra capacity above `max_tokens` for short spikes.
    pub fn with_burst(mut self, burst: u32) -> Self {
        self.burst = burst;
        self
    }
    fn refill_per_sec(&self) -> f64 {
        self.tokens_per_period as f64 / self.refill_period.as_secs_f64()
    }
    fn capacity(&self) -> u32 {
        self.max_tokens.saturating_add(self.burst)
    }
}

#[derive(Debug)]
struct State {
    tokens: f64,
    last_refill: Instant,
}

/// A reusable token-bucket rate limiter.
///
/// Cheap to clone (`Arc` internally). Share one limiter across every task that draws on the
/// same quota so the budget is enforced globally. See the [module documentation](self) for
/// the token-bucket semantics.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<State>>,
    policy: RateLimiterPolicy,
    refill_per_sec: f64,
    capacity: f64,
}

impl RateLimiter {
    /// Construct a limiter with a full bucket.
    ///
    /// # Panics
    ///
    /// Panics if `policy.max_tokens == 0` (a zero-capacity bucket could never admit a call)
    /// or if the refill rate is zero (`tokens_per_period == 0` or `refill_period` is zero) —
    /// the bucket would never replenish. Both are programmer errors caught at construction,
    /// consistent with [`CircuitBreaker::new`](crate::circuit::CircuitBreaker::new).
    pub fn new(policy: RateLimiterPolicy) -> Self {
        assert!(policy.max_tokens >= 1, "max_tokens must be >= 1");
        assert!(
            policy.tokens_per_period >= 1 && !policy.refill_period.is_zero(),
            "refill rate must be > 0"
        );
        let capacity = policy.capacity() as f64;
        let refill_per_sec = policy.refill_per_sec();
        Self {
            inner: Arc::new(Mutex::new(State {
                tokens: capacity,
                last_refill: Instant::now(),
            })),
            policy,
            refill_per_sec,
            capacity,
        }
    }

    /// Return the policy this limiter was constructed with.
    pub fn policy(&self) -> &RateLimiterPolicy {
        &self.policy
    }

    /// Try to consume one token without blocking.
    ///
    /// Returns `Some(Permit)` if a token was available (and consumes it), or `None` if the
    /// bucket is empty. Lazily refills the bucket from the clock before deciding.
    pub fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        let mut st = self.inner.lock();
        self.refill_locked(&mut st);
        if st.tokens >= 1.0 {
            st.tokens -= 1.0;
            #[cfg(feature = "metrics")]
            crate::metrics::rate_limiter_acquired();
            Some(Permit {
                _limiter: Arc::clone(self),
            })
        } else {
            #[cfg(feature = "metrics")]
            crate::metrics::rate_limiter_throttled("rejected");
            None
        }
    }

    /// Consume one token, parking until one is available.
    ///
    /// If the bucket is empty, computes the time until the next token refills, sleeps that
    /// long via `tokio::time::sleep`, and retries. The internal mutex is never held across
    /// the await.
    pub async fn acquire(self: &Arc<Self>) -> Permit {
        #[cfg(feature = "metrics")]
        let start = Instant::now();
        loop {
            let wait = {
                let mut st = self.inner.lock();
                self.refill_locked(&mut st);
                if st.tokens >= 1.0 {
                    st.tokens -= 1.0;
                    None
                } else {
                    let deficit = 1.0 - st.tokens;
                    Some(Duration::from_secs_f64(
                        (deficit / self.refill_per_sec).max(0.0),
                    ))
                }
            };
            match wait {
                None => {
                    #[cfg(feature = "metrics")]
                    {
                        crate::metrics::rate_limiter_acquired();
                        crate::metrics::rate_limiter_wait(start.elapsed());
                    }
                    return Permit {
                        _limiter: Arc::clone(self),
                    };
                }
                Some(dur) => {
                    #[cfg(feature = "metrics")]
                    crate::metrics::rate_limiter_throttled("waited");
                    tokio::time::sleep(dur).await;
                }
            }
        }
    }

    fn refill_locked(&self, st: &mut State) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(st.last_refill);
        if elapsed.is_zero() {
            return;
        }
        let credit = elapsed.as_secs_f64() * self.refill_per_sec;
        st.tokens = (st.tokens + credit).min(self.capacity);
        st.last_refill = now;
    }
}

#[resource]
impl Resource for RateLimiter {}

/// A token consumed from a [`RateLimiter`], marking the scope of one rate-limited call.
///
/// Holding the permit keeps the `RateLimiter` alive for the call's duration. Dropping it is a
/// **no-op** — a token bucket spends the token at acquisition and refills on the clock, so
/// there is nothing to return on release. (Contrast a semaphore, where the permit *is* the
/// reserved capacity.)
#[must_use = "the permit marks the rate-limited call's scope"]
pub struct Permit {
    _limiter: Arc<RateLimiter>,
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permit").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn fast_policy() -> RateLimiterPolicy {
        RateLimiterPolicy::per_second(10).with_max_tokens(5)
    }

    #[test]
    fn try_acquire_succeeds_when_tokens_available() {
        let limiter = Arc::new(RateLimiter::new(fast_policy()));
        for _ in 0..5 {
            assert!(limiter.try_acquire().is_some());
        }
    }

    #[test]
    fn try_acquire_returns_none_when_bucket_empty() {
        let limiter = Arc::new(RateLimiter::new(fast_policy()));
        for _ in 0..5 {
            let _ = limiter.try_acquire().expect("first 5 succeed");
        }
        assert!(limiter.try_acquire().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_parks_then_succeeds() {
        let limiter = Arc::new(RateLimiter::new(fast_policy()));
        for _ in 0..5 {
            let _ = limiter.try_acquire().expect("first 5");
        }
        let start = tokio::time::Instant::now();
        let _ = limiter.acquire().await;
        assert!(start.elapsed() >= Duration::from_millis(90));
    }

    #[test]
    fn burst_respected() {
        let policy = RateLimiterPolicy::per_second(10)
            .with_max_tokens(2)
            .with_burst(5);
        let limiter = Arc::new(RateLimiter::new(policy));
        let mut acquired = 0;
        while limiter.try_acquire().is_some() {
            acquired += 1;
            if acquired > 7 {
                break;
            }
        }
        assert_eq!(acquired, 7);
    }

    #[test]
    #[should_panic(expected = "max_tokens must be >= 1")]
    fn rejects_zero_max_tokens() {
        let _ = RateLimiter::new(RateLimiterPolicy::per_second(1).with_max_tokens(0));
    }
}

#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use super::*;
    use crate::metrics::test_support::*;
    use std::sync::Arc;

    #[test]
    fn try_acquire_records_acquired_and_rejected() {
        let ((), rows) = run_with_recorder(|| async {
            let limiter = Arc::new(RateLimiter::new(
                RateLimiterPolicy::per_second(10).with_max_tokens(2),
            ));
            assert!(limiter.try_acquire().is_some());
            assert!(limiter.try_acquire().is_some());
            // Bucket drained → this one is rejected.
            assert!(limiter.try_acquire().is_none());
        });
        assert_eq!(counter(&rows, "cano_rate_limiter_acquired_total", &[]), 2);
        assert_eq!(
            counter(
                &rows,
                "cano_rate_limiter_throttled_total",
                &[("result", "rejected")]
            ),
            1
        );
    }

    #[test]
    fn acquire_waits_then_records_acquired_and_histogram() {
        let ((), rows) = run_with_recorder(|| async {
            // Single-token bucket refilling at 100/s → ~10ms wait after draining.
            let limiter = Arc::new(RateLimiter::new(
                RateLimiterPolicy::per_second(100).with_max_tokens(1),
            ));
            let _ = limiter.try_acquire().expect("first token is available");
            // Empty bucket: acquire must park for a refill, then succeed.
            let _ = limiter.acquire().await;
        });
        // One token from try_acquire + one from acquire.
        assert_eq!(counter(&rows, "cano_rate_limiter_acquired_total", &[]), 2);
        // acquire parked at least once before the token refilled.
        assert!(
            counter(
                &rows,
                "cano_rate_limiter_throttled_total",
                &[("result", "waited")]
            ) >= 1
        );
        // The blocked-time histogram recorded exactly one sample (on success).
        assert_eq!(
            histogram_count(&rows, "cano_rate_limiter_wait_seconds", &[]),
            1
        );
    }
}
