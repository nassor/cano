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

use crate::error::CanoError;
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
        Self {
            inner: Arc::new(Mutex::new(State {
                tokens: capacity,
                last_refill: Instant::now(),
            })),
            policy,
        }
    }

    /// Return the policy this limiter was constructed with.
    pub fn policy(&self) -> &RateLimiterPolicy {
        &self.policy
    }

    /// Try to consume one token without blocking. Equivalent to
    /// [`try_acquire_n(1)`](Self::try_acquire_n).
    pub fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        self.try_acquire_n(1)
    }

    /// Try to consume `cost` tokens without blocking.
    ///
    /// Returns `Some(Permit)` if at least `cost` tokens were available (and consumes them), or
    /// `None` otherwise. Lazily refills the bucket from the clock before deciding. `cost == 0`
    /// always succeeds without consuming; a `cost` larger than the bucket's capacity can never
    /// be satisfied and returns `None`.
    pub fn try_acquire_n(self: &Arc<Self>, cost: u64) -> Option<Permit> {
        let acquired = self.debit(cost);
        // Emit metrics after releasing the lock so the recorder lookup never
        // lengthens the critical section (mirrors `CircuitBreaker::try_acquire`).
        #[cfg(feature = "metrics")]
        if acquired {
            crate::metrics::rate_limiter_acquired();
            crate::metrics::rate_limiter_tokens_consumed(cost);
        } else {
            crate::metrics::rate_limiter_throttled("rejected");
        }
        acquired.then(|| Permit {
            _limiter: Arc::clone(self),
        })
    }

    /// Reserve `cost` tokens, returning a refundable [`Reservation`].
    ///
    /// The tokens are debited immediately, but the returned [`Reservation`] **refunds them on
    /// drop** unless [`commit`](Reservation::commit) is called first. This is the building block
    /// for atomic multi-limiter acquisition ([`MultiRateLimiter`]): reserve from several
    /// limiters and, if any rejects, drop the reservations gathered so far to hand their tokens
    /// back. Emits no metrics — the committing caller owns that accounting.
    pub fn try_reserve_n(self: &Arc<Self>, cost: u64) -> Option<Reservation> {
        if self.debit(cost) {
            let arc = Arc::clone(self);
            let meter: Arc<dyn Meter> = arc;
            Some(Reservation {
                meter,
                cost,
                epoch: 0, // bucket refunds are epoch-independent
                committed: false,
            })
        } else {
            None
        }
    }

    /// Consume one token, parking until it is available. Equivalent to
    /// [`acquire_n(1)`](Self::acquire_n).
    pub async fn acquire(self: &Arc<Self>) -> Permit {
        self.acquire_n(1).await
    }

    /// Consume `cost` tokens, parking until they are available.
    ///
    /// If the bucket is short, computes the time until enough tokens refill, sleeps that long
    /// via `tokio::time::sleep`, and retries. The internal mutex is never held across the await.
    /// A `cost` larger than capacity can never be satisfied and parks indefinitely — callers
    /// must keep `cost <= capacity`.
    ///
    /// # Fairness
    ///
    /// Wakeups are **best-effort**, not queued: when many tasks park on the same empty bucket
    /// they each sleep for their own computed deadline, so a refill can wake several at once and
    /// only one wins — the rest recompute and re-park. The configured rate is always respected
    /// (no over-admission), but under heavy contention this trades some wasted wakeups for a
    /// lock-free, allocation-free wait path. Reach for [`try_acquire_n`](Self::try_acquire_n)
    /// plus your own backoff if you need strict ordering.
    pub async fn acquire_n(self: &Arc<Self>, cost: u64) -> Permit {
        #[cfg(feature = "metrics")]
        let start = Instant::now();
        #[cfg(feature = "metrics")]
        let mut waited = false;
        let cost_f = cost as f64;
        let capacity = self.policy.capacity() as f64;
        loop {
            let wait = {
                let mut st = self.inner.lock();
                self.refill_locked(&mut st);
                if st.tokens >= cost_f {
                    st.tokens -= cost_f;
                    None
                } else if cost_f > capacity {
                    // Never satisfiable — park on a single MAX sleep rather than busy-looping
                    // on a finite deficit that the cap never lets the bucket reach.
                    Some(Duration::MAX)
                } else {
                    let deficit = cost_f - st.tokens;
                    // `try_from_secs_f64` clamps an astronomically slow refill (a wait that
                    // would overflow `Duration`) to `Duration::MAX` instead of panicking.
                    Some(
                        Duration::try_from_secs_f64(
                            (deficit / self.policy.refill_per_sec()).max(0.0),
                        )
                        .unwrap_or(Duration::MAX),
                    )
                }
            };
            match wait {
                None => {
                    #[cfg(feature = "metrics")]
                    {
                        crate::metrics::rate_limiter_acquired();
                        crate::metrics::rate_limiter_tokens_consumed(cost);
                        // Only record the wait histogram when this call actually parked, so it
                        // measures blocked time rather than every successful acquisition.
                        if waited {
                            crate::metrics::rate_limiter_wait(start.elapsed());
                        }
                    }
                    return Permit {
                        _limiter: Arc::clone(self),
                    };
                }
                Some(dur) => {
                    #[cfg(feature = "metrics")]
                    {
                        crate::metrics::rate_limiter_throttled("waited");
                        waited = true;
                    }
                    tokio::time::sleep(dur).await;
                }
            }
        }
    }

    /// Whole tokens currently available (after a lazy refill). For observability.
    pub fn tokens_available(&self) -> u64 {
        let mut st = self.inner.lock();
        self.refill_locked(&mut st);
        st.tokens.floor().max(0.0) as u64
    }

    /// Time until `cost` tokens are available; [`Duration::ZERO`] if they already are.
    ///
    /// Clamped to [`Duration::MAX`] for an astronomically slow refill (and for a `cost` larger
    /// than capacity, which never becomes available).
    pub fn time_until(&self, cost: u64) -> Duration {
        if cost == 0 {
            return Duration::ZERO;
        }
        let cost_f = cost as f64;
        // A cost larger than capacity can never be satisfied — report it as never (matching
        // `WindowedRateLimiter::time_until`) so a parking caller sleeps once on MAX instead of
        // busy-looping on a finite-but-unsatisfiable deficit.
        if cost_f > self.policy.capacity() as f64 {
            return Duration::MAX;
        }
        let mut st = self.inner.lock();
        self.refill_locked(&mut st);
        if st.tokens >= cost_f {
            Duration::ZERO
        } else {
            let deficit = cost_f - st.tokens;
            Duration::try_from_secs_f64((deficit / self.policy.refill_per_sec()).max(0.0))
                .unwrap_or(Duration::MAX)
        }
    }

    /// Debit `cost` tokens if available; returns whether the debit happened. Lazy refill first.
    fn debit(&self, cost: u64) -> bool {
        if cost == 0 {
            return true;
        }
        let cost_f = cost as f64;
        let mut st = self.inner.lock();
        self.refill_locked(&mut st);
        if st.tokens >= cost_f {
            st.tokens -= cost_f;
            true
        } else {
            false
        }
    }

    /// Return `cost` tokens to the bucket (capped at capacity). Used by [`Reservation`] rollback.
    ///
    /// Deliberately does *not* refill first (unlike [`debit`](Self::debit)): bucket tokens are
    /// fungible, so the elapsed-time credit is applied lazily on the next access and re-adding it
    /// here would only be swallowed by the capacity cap. Contrast `WindowedRateLimiter`, whose
    /// refund must roll first because a count belongs to a specific window.
    fn refund(&self, cost: u64) {
        if cost == 0 {
            return;
        }
        let mut st = self.inner.lock();
        st.tokens = (st.tokens + cost as f64).min(self.policy.capacity() as f64);
    }

    fn refill_locked(&self, st: &mut State) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(st.last_refill);
        if elapsed.is_zero() {
            return;
        }
        let credit = elapsed.as_secs_f64() * self.policy.refill_per_sec();
        st.tokens = (st.tokens + credit).min(self.policy.capacity() as f64);
        st.last_refill = now;
    }
}

#[resource]
impl Resource for RateLimiter {}

impl Meter for RateLimiter {
    fn try_debit(&self, cost: u64) -> Option<u64> {
        // Bucket tokens are fungible, so any refund is valid regardless of when it happened —
        // a single fixed epoch (0) suffices and `credit` ignores it.
        self.debit(cost).then_some(0)
    }
    fn credit(&self, cost: u64, _epoch: u64) {
        self.refund(cost);
    }
    fn time_until(&self, cost: u64) -> Duration {
        RateLimiter::time_until(self, cost)
    }
    fn snapshot(&self) -> MeterStatus {
        let capacity = self.policy.capacity() as u64;
        let available = self.tokens_available();
        MeterStatus {
            used: capacity.saturating_sub(available),
            limit: capacity,
            available_in: self.time_until(1),
            // A token bucket has no reset boundary — only a smooth drip.
            resets_at: None,
        }
    }
}

/// A throttle that admits or rejects weighted units, exposing enough state to compose several
/// into a [`MultiRateLimiter`] and to answer "which limit blocked me, and for how long".
///
/// Implemented by both [`RateLimiter`] (continuous token bucket) and
/// [`WindowedRateLimiter`] (fixed-window counter), so a single gate can mix the two. The trait
/// is deliberately **synchronous** — the async `acquire` paths are built on `try_debit` +
/// `time_until` + `tokio::time::sleep`, which keeps `Meter` object-safe and lets a composite
/// never double-debit. Units are weighted (`cost`): a request-count tier uses `cost = 1`; a
/// usage-metered tier uses the call's cost (e.g. token count).
pub trait Meter: Send + Sync + std::fmt::Debug {
    /// Debit `cost` units if capacity allows. Returns `Some(epoch)` on success — an opaque token
    /// identifying the accounting session the debit belongs to, which must be passed back to
    /// [`credit`](Self::credit) to refund it — or `None` if rejected. No metrics.
    fn try_debit(&self, cost: u64) -> Option<u64>;
    /// Return `cost` units previously debited in `epoch` (used by [`Reservation`] rollback). If
    /// the meter's accounting has moved on (e.g. a fixed window has since reset), the refund no
    /// longer applies and is silently dropped. No metrics.
    fn credit(&self, cost: u64, epoch: u64);
    /// Time until `cost` units are available; [`Duration::ZERO`] if they already are.
    fn time_until(&self, cost: u64) -> Duration;
    /// A point-in-time view for observability.
    fn snapshot(&self) -> MeterStatus;
}

/// A point-in-time view of a [`Meter`]'s capacity, for observability.
#[derive(Debug, Clone)]
pub struct MeterStatus {
    /// Units currently consumed.
    pub used: u64,
    /// The meter's capacity (token-bucket capacity, or the window's limit).
    pub limit: u64,
    /// Time until at least one more unit frees up; [`Duration::ZERO`] when capacity is available.
    pub available_in: Duration,
    /// When a fixed window next resets; `None` for a (boundary-less) token bucket.
    pub resets_at: Option<Instant>,
}

/// A refundable debit against a [`Meter`].
///
/// Dropping a `Reservation` **returns** its units to the meter unless [`commit`](Self::commit)
/// has been called. That refund is exactly what lets a multi-limiter acquisition be
/// all-or-nothing without leaking budget: reserve from each tier, and if a later tier rejects,
/// drop the earlier reservations to give their units back. See [`MultiRateLimiter`].
#[must_use = "a dropped, uncommitted Reservation refunds its units; commit it to keep the debit"]
pub struct Reservation {
    meter: Arc<dyn Meter>,
    cost: u64,
    epoch: u64,
    committed: bool,
}

impl Reservation {
    /// Make the debit permanent: the reserved units are no longer refunded on drop.
    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            self.meter.credit(self.cost, self.epoch);
        }
    }
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("cost", &self.cost)
            .field("committed", &self.committed)
            .finish_non_exhaustive()
    }
}

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

/// Policy for a [`WindowedRateLimiter`]: at most `limit` units per fixed `window`.
#[derive(Debug, Clone)]
pub struct WindowPolicy {
    /// Maximum units admitted within one window.
    pub limit: u64,
    /// Window length. The window opens on the first call and resets `window` later.
    pub window: Duration,
}

impl WindowPolicy {
    /// `limit` units per `window`.
    pub fn new(limit: u64, window: Duration) -> Self {
        Self { limit, window }
    }
    /// `limit` units per `hours` hours.
    pub fn per_hours(limit: u64, hours: u64) -> Self {
        Self::new(limit, Duration::from_secs(hours.saturating_mul(3600)))
    }
    /// `limit` units per `days` days.
    pub fn per_days(limit: u64, days: u64) -> Self {
        Self::new(limit, Duration::from_secs(days.saturating_mul(86_400)))
    }
}

#[derive(Debug)]
struct WindowState {
    window_start: Instant,
    used: u64,
    /// Bumped on every reset. A [`Reservation`] captures it at debit time so a refund that
    /// arrives after the window has rolled (a stale reservation) is discarded instead of
    /// erasing usage from the *new* window.
    generation: u64,
}

/// A fixed-window rate limiter: admits up to `limit` units per `window`, with a hard reset.
///
/// Where the continuous [`RateLimiter`] token bucket drips capacity back smoothly, a window makes
/// the full quota available at once and resets it as a **step** at the boundary — the shape of
/// Claude-Code-style "N per 5 hours, resets at …" limits — and exposes a
/// [`resets_at`](Self::resets_at) instant the bucket structurally cannot. The window opens on the
/// first call and resets **lazily** (no background task) on the first call after `window` has
/// elapsed. It uses a monotonic [`Instant`], so there is no wall-clock boundary alignment.
///
/// Cheap to clone (`Arc` inside) — share one across tasks drawing on the same quota. Implements
/// [`Meter`], so it can be a tier in a [`MultiRateLimiter`] alongside token buckets, and
/// [`Resource`] so it can live in [`Resources`](crate::resource::Resources).
#[derive(Debug, Clone)]
pub struct WindowedRateLimiter {
    inner: Arc<Mutex<WindowState>>,
    policy: WindowPolicy,
}

impl WindowedRateLimiter {
    /// Construct an empty window.
    ///
    /// # Panics
    ///
    /// Panics if `policy.limit == 0` (no call could ever be admitted) or `policy.window` is zero
    /// (a window with no duration is meaningless). Programmer errors caught at construction,
    /// consistent with [`RateLimiter::new`].
    pub fn new(policy: WindowPolicy) -> Self {
        assert!(policy.limit >= 1, "WindowPolicy::limit must be >= 1");
        assert!(!policy.window.is_zero(), "WindowPolicy::window must be > 0");
        Self {
            inner: Arc::new(Mutex::new(WindowState {
                window_start: Instant::now(),
                used: 0,
                generation: 0,
            })),
            policy,
        }
    }

    /// Return the policy this limiter was constructed with.
    pub fn policy(&self) -> &WindowPolicy {
        &self.policy
    }

    /// Try to consume one unit without blocking. Equivalent to `try_acquire_n(1)`.
    pub fn try_acquire(self: &Arc<Self>) -> Option<WindowPermit> {
        self.try_acquire_n(1)
    }

    /// Try to consume `cost` units without blocking. `cost == 0` always succeeds without
    /// consuming; a `cost` larger than `limit` can never be satisfied and returns `None`.
    pub fn try_acquire_n(self: &Arc<Self>, cost: u64) -> Option<WindowPermit> {
        let acquired = self.debit(cost).is_some();
        #[cfg(feature = "metrics")]
        if acquired {
            crate::metrics::rate_limiter_acquired();
            crate::metrics::rate_limiter_tokens_consumed(cost);
        } else {
            crate::metrics::rate_limiter_throttled("rejected");
        }
        acquired.then(|| WindowPermit {
            _limiter: Arc::clone(self),
        })
    }

    /// Reserve `cost` units, returning a refundable [`Reservation`] (see
    /// [`RateLimiter::try_reserve_n`]). The reservation captures the current window generation,
    /// so if it is dropped after the window has reset the refund is discarded rather than
    /// erasing usage from the new window. Emits no metrics.
    pub fn try_reserve_n(self: &Arc<Self>, cost: u64) -> Option<Reservation> {
        self.debit(cost).map(|epoch| {
            let arc = Arc::clone(self);
            let meter: Arc<dyn Meter> = arc;
            Reservation {
                meter,
                cost,
                epoch,
                committed: false,
            }
        })
    }

    /// Consume one unit, parking until the window has room. Equivalent to `acquire_n(1)`.
    pub async fn acquire(self: &Arc<Self>) -> WindowPermit {
        self.acquire_n(1).await
    }

    /// Consume `cost` units, parking until the window has room (until the next reset if the
    /// window is currently full). A `cost` larger than `limit` parks indefinitely.
    pub async fn acquire_n(self: &Arc<Self>, cost: u64) -> WindowPermit {
        #[cfg(feature = "metrics")]
        let start = Instant::now();
        #[cfg(feature = "metrics")]
        let mut waited = false;
        loop {
            let wait = {
                let mut st = self.inner.lock();
                self.roll_locked(&mut st);
                if st.used.saturating_add(cost) <= self.policy.limit {
                    st.used += cost;
                    None
                } else if cost > self.policy.limit {
                    Some(Duration::MAX)
                } else {
                    Some(self.time_until_reset(&st))
                }
            };
            match wait {
                None => {
                    #[cfg(feature = "metrics")]
                    {
                        crate::metrics::rate_limiter_acquired();
                        crate::metrics::rate_limiter_tokens_consumed(cost);
                        if waited {
                            crate::metrics::rate_limiter_wait(start.elapsed());
                        }
                    }
                    return WindowPermit {
                        _limiter: Arc::clone(self),
                    };
                }
                Some(dur) => {
                    #[cfg(feature = "metrics")]
                    {
                        crate::metrics::rate_limiter_throttled("waited");
                        waited = true;
                    }
                    tokio::time::sleep(dur).await;
                }
            }
        }
    }

    /// Units consumed in the current window (after a lazy reset).
    pub fn used(&self) -> u64 {
        let mut st = self.inner.lock();
        self.roll_locked(&mut st);
        st.used
    }

    /// Units still available in the current window.
    pub fn remaining(&self) -> u64 {
        let mut st = self.inner.lock();
        self.roll_locked(&mut st);
        self.policy.limit.saturating_sub(st.used)
    }

    /// When the current window resets.
    ///
    /// For a window so long that `window_start + window` overflows [`Instant`] (an absurd,
    /// effectively never-resetting window), returns the window start as a sentinel;
    /// [`Meter::snapshot`]'s `resets_at` reports `None` for that same case.
    pub fn resets_at(&self) -> Instant {
        let mut st = self.inner.lock();
        self.roll_locked(&mut st);
        self.boundary(&st).unwrap_or(st.window_start)
    }

    /// Time until `cost` units are available; [`Duration::ZERO`] if they already are. When the
    /// window is full this is the time until the next reset; [`Duration::MAX`] if `cost` exceeds
    /// `limit` (never satisfiable).
    pub fn time_until(&self, cost: u64) -> Duration {
        if cost == 0 {
            return Duration::ZERO;
        }
        let mut st = self.inner.lock();
        self.roll_locked(&mut st);
        if st.used.saturating_add(cost) <= self.policy.limit {
            Duration::ZERO
        } else if cost > self.policy.limit {
            Duration::MAX
        } else {
            self.time_until_reset(&st)
        }
    }

    /// Debit `cost` units if the current window has room. Returns `Some(generation)` (the epoch a
    /// refund must match) on success, `None` if rejected. `cost == 0` always succeeds.
    fn debit(&self, cost: u64) -> Option<u64> {
        let mut st = self.inner.lock();
        self.roll_locked(&mut st);
        if cost == 0 {
            return Some(st.generation);
        }
        if st.used.saturating_add(cost) <= self.policy.limit {
            st.used += cost;
            Some(st.generation)
        } else {
            None
        }
    }

    /// Refund `cost` units debited in `epoch` — only if the window has not rolled since (else the
    /// debit's window is gone and there is nothing to refund).
    fn refund(&self, cost: u64, epoch: u64) {
        if cost == 0 {
            return;
        }
        let mut st = self.inner.lock();
        self.roll_locked(&mut st);
        if epoch == st.generation {
            st.used = st.used.saturating_sub(cost);
        }
    }

    /// The instant the current window resets, or `None` if it would overflow `Instant` (an
    /// absurdly long window — treated as never resetting).
    fn boundary(&self, st: &WindowState) -> Option<Instant> {
        st.window_start.checked_add(self.policy.window)
    }

    /// Time until the current window resets ([`Duration::MAX`] for an overflowing window).
    fn time_until_reset(&self, st: &WindowState) -> Duration {
        self.boundary(st)
            .map(|b| b.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::MAX)
    }

    /// Reset the window if it has fully elapsed. Lazy — called on every access; no background task.
    fn roll_locked(&self, st: &mut WindowState) {
        let now = Instant::now();
        if now.saturating_duration_since(st.window_start) >= self.policy.window {
            st.window_start = now;
            st.used = 0;
            st.generation = st.generation.wrapping_add(1);
        }
    }
}

#[resource]
impl Resource for WindowedRateLimiter {}

impl Meter for WindowedRateLimiter {
    fn try_debit(&self, cost: u64) -> Option<u64> {
        self.debit(cost)
    }
    fn credit(&self, cost: u64, epoch: u64) {
        self.refund(cost, epoch);
    }
    fn time_until(&self, cost: u64) -> Duration {
        WindowedRateLimiter::time_until(self, cost)
    }
    fn snapshot(&self) -> MeterStatus {
        let mut st = self.inner.lock();
        self.roll_locked(&mut st);
        let resets_at = self.boundary(&st);
        let available_in = if st.used < self.policy.limit {
            Duration::ZERO
        } else {
            self.time_until_reset(&st)
        };
        MeterStatus {
            used: st.used,
            limit: self.policy.limit,
            available_in,
            resets_at,
        }
    }
}

/// A unit consumed from a [`WindowedRateLimiter`], marking the scope of one rate-limited call.
/// Dropping it is a **no-op** — the window holds the count until it resets.
#[must_use = "the permit marks the rate-limited call's scope"]
pub struct WindowPermit {
    _limiter: Arc<WindowedRateLimiter>,
}

impl std::fmt::Debug for WindowPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowPermit").finish_non_exhaustive()
    }
}

/// One tier in a [`MultiRateLimiter`]: a named [`Meter`] and the per-acquire `cost` charged to it.
///
/// `cost == 0` makes the tier inert (never blocks, never debited) — handy for conditionally
/// disabling a tier without rebuilding the limiter.
#[derive(Debug, Clone)]
pub struct Tier {
    /// A stable label, surfaced in [`CanoError::RateLimited`] and metrics.
    pub name: &'static str,
    /// The underlying meter — a [`RateLimiter`] (token bucket) or [`WindowedRateLimiter`] (window).
    pub meter: Arc<dyn Meter>,
    /// Units charged to this tier per acquisition.
    pub cost: u64,
}

/// Floor on the sleep between [`MultiRateLimiter::acquire`] retries, so a `retry_after` that
/// rounds to zero (a tier that freed up between the check and the read) can't busy-spin.
const MULTI_MIN_BACKOFF: Duration = Duration::from_millis(1);

/// Enforces several rate limits at once: an acquisition succeeds only if **every** applicable
/// tier has capacity, and either all tiers are debited or none are.
///
/// This is the Claude-Code-style multi-level limiter: compose a 5-hour tier, a 7-day tier, and a
/// model-scoped 7-day tier, mixing [`RateLimiter`] (token bucket) and [`WindowedRateLimiter`]
/// (fixed window) freely, each metering its own currency via `cost`. On rejection it reports
/// **which** tier blocked and the **retry-after** for it via [`CanoError::RateLimited`].
///
/// ## Atomicity (no leak)
///
/// [`try_acquire`](Self::try_acquire) reserves each tier in turn; if any tier rejects, it drops
/// the [`Reservation`]s gathered so far — refunding their units — so no tier loses budget for a
/// call that didn't fully succeed. At most one tier's lock is held at a time, so there is no
/// lock-ordering deadlock. On full success every reservation is committed.
///
/// Cheap to clone (the tiers are `Arc`s). Implements [`Resource`] so it can live in
/// [`Resources`](crate::resource::Resources).
#[derive(Debug, Clone, Default)]
pub struct MultiRateLimiter {
    tiers: Vec<Tier>,
}

impl MultiRateLimiter {
    /// An empty limiter. Add tiers with [`with_tier`](Self::with_tier).
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tier metering `cost` units per acquisition against `meter`.
    ///
    /// `meter` is any [`Meter`] — typically `Arc::new(RateLimiter::new(..))` or
    /// `Arc::new(WindowedRateLimiter::new(..))`.
    ///
    /// Each tier debits **independently**: passing the same `Arc<dyn Meter>` to two tiers (or
    /// reusing a `name`) charges that meter once *per tier* on one acquisition. Tier names are not
    /// deduplicated — they are labels for [`CanoError::RateLimited`] / metrics / the `_for` filter,
    /// not keys. Give distinct meters (or distinct names) unless you intend the double charge.
    pub fn with_tier(mut self, name: &'static str, meter: Arc<dyn Meter>, cost: u64) -> Self {
        self.tiers.push(Tier { name, meter, cost });
        self
    }

    /// The configured tiers.
    pub fn tiers(&self) -> &[Tier] {
        &self.tiers
    }

    /// Try to acquire against **all** tiers without blocking.
    ///
    /// Returns `Ok(MultiPermit)` only if every tier admitted (each debited its `cost`); otherwise
    /// `Err(CanoError::RateLimited { tier, retry_after })` naming the first blocking tier, with
    /// every other tier left untouched (no leak).
    pub fn try_acquire(&self) -> Result<MultiPermit, CanoError> {
        self.try_acquire_for(&[])
    }

    /// Like [`try_acquire`](Self::try_acquire) but only enforces tiers whose name is in `only`
    /// (an empty slice means *all* tiers). Use it for a per-request subset — e.g. a request for
    /// one model hits the shared tiers plus only that model's tier.
    pub fn try_acquire_for(&self, only: &[&str]) -> Result<MultiPermit, CanoError> {
        self.reserve_all(only).map_err(|(tier, retry_after)| {
            Self::record_throttle(tier);
            CanoError::rate_limited(tier, retry_after)
        })
    }

    /// Acquire against all tiers, parking until every tier admits simultaneously.
    pub async fn acquire(&self) -> MultiPermit {
        self.acquire_for(&[]).await
    }

    /// Like [`acquire`](Self::acquire) but only enforces the tiers named in `only` (empty = all).
    ///
    /// Loops [`try_acquire_for`](Self::try_acquire_for), sleeping the blocking tier's retry-after
    /// between attempts — so it never double-debits and waits on the *binding* tier rather than
    /// head-of-line-blocking on a fixed order. Each blocked attempt records a throttle, like the
    /// shed-load path, so paced callers aren't invisible to the metric.
    pub async fn acquire_for(&self, only: &[&str]) -> MultiPermit {
        loop {
            match self.reserve_all(only) {
                Ok(permit) => return permit,
                Err((tier, retry_after)) => {
                    Self::record_throttle(tier);
                    // `retry_after` is already floored to `MULTI_MIN_BACKOFF` by `reserve_all`.
                    tokio::time::sleep(retry_after).await;
                }
            }
        }
    }

    /// Record a per-tier throttle (no-op without the `metrics` feature).
    fn record_throttle(tier: &'static str) {
        #[cfg(feature = "metrics")]
        crate::metrics::multi_rate_limiter_throttled(tier);
        #[cfg(not(feature = "metrics"))]
        let _ = tier;
    }

    /// Reserve every applicable tier atomically. `Ok(permit)` if all admit; `Err((tier, retry))`
    /// for the first that rejects, after refunding the reservations gathered before it.
    fn reserve_all(&self, only: &[&str]) -> Result<MultiPermit, (&'static str, Duration)> {
        let mut held: Vec<Reservation> = Vec::with_capacity(self.tiers.len());
        for tier in &self.tiers {
            if (!only.is_empty() && !only.contains(&tier.name)) || tier.cost == 0 {
                continue;
            }
            match tier.meter.try_debit(tier.cost) {
                Some(epoch) => held.push(Reservation {
                    meter: Arc::clone(&tier.meter),
                    cost: tier.cost,
                    epoch,
                    committed: false,
                }),
                None => {
                    // Floor the retry-after so the natural `loop { sleep(retry_after) }` retry
                    // pattern can't busy-spin if the tier freed up between try_debit and this read.
                    let retry_after = tier.meter.time_until(tier.cost).max(MULTI_MIN_BACKOFF);
                    // Dropping `held` refunds every reservation gathered so far → zero leak.
                    drop(held);
                    return Err((tier.name, retry_after));
                }
            }
        }
        for r in &mut held {
            r.commit();
        }
        Ok(MultiPermit {
            _reservations: held,
        })
    }
}

#[resource]
impl Resource for MultiRateLimiter {}

/// A successful [`MultiRateLimiter`] acquisition, marking the scope of one multi-limited call.
///
/// Holds the committed reservations; dropping it is a no-op (the units stay spent until each tier
/// refills or resets on its own clock).
#[must_use = "the permit marks the rate-limited call's scope"]
pub struct MultiPermit {
    _reservations: Vec<Reservation>,
}

impl std::fmt::Debug for MultiPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiPermit")
            .field("tiers", &self._reservations.len())
            .finish_non_exhaustive()
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

    #[tokio::test]
    async fn acquire_parks_until_a_token_refills() {
        // A real (unpaused) runtime so the `std::Instant` refill clock and `tokio::time::sleep`
        // share one wall clock. 100 tokens/sec, capacity 1 → ~10ms to refill one token.
        // `start` is captured BEFORE the drain so the limiter's refill clock is anchored at or
        // after `start`: the park ends >= drain_instant + 10ms >= start + 10ms, so scheduling
        // jitter can only inflate the measurement (never make this lower bound flake under load).
        let start = std::time::Instant::now();
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(100).with_max_tokens(1),
        ));
        let _ = limiter.try_acquire().expect("initial token");
        let _permit = limiter.acquire().await;
        assert!(
            start.elapsed() >= Duration::from_millis(5),
            "acquire should have parked for a refill, waited {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn acquire_returns_immediately_when_tokens_available() {
        // fast_policy starts the bucket full (cap 5). acquire takes the fast path and consumes
        // exactly one token. (A wall-clock upper bound would flake on a loaded runner, so we
        // assert the consumed count instead, which also catches over/under-consumption.)
        let limiter = Arc::new(RateLimiter::new(fast_policy()));
        let _permit = limiter.acquire().await;
        assert_eq!(limiter.tokens_available(), 4);
    }

    #[tokio::test]
    async fn tokens_refill_over_real_time() {
        // 50 tokens/sec, capacity 1: after draining, one refill interval (~20ms) must pass
        // before another token is available — exercises the refill_locked accrual math.
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(50).with_max_tokens(1),
        ));
        let _ = limiter.try_acquire().expect("initial token");
        assert!(
            limiter.try_acquire().is_none(),
            "bucket is empty right after draining"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            limiter.try_acquire().is_some(),
            "a token should have refilled after >1 interval"
        );
    }

    #[tokio::test]
    async fn acquire_clamps_extreme_wait_without_panicking() {
        // 1 token per Duration::MAX: the computed wait overflows Duration. acquire must clamp
        // it to Duration::MAX (via try_from_secs_f64) rather than panic — pre-fix this path
        // called Duration::from_secs_f64 and panicked.
        let limiter = Arc::new(RateLimiter::new(RateLimiterPolicy {
            max_tokens: 1,
            tokens_per_period: 1,
            refill_period: Duration::MAX,
            burst: 0,
        }));
        let _ = limiter.try_acquire().expect("initial token");
        // The clamped wait parks effectively forever; a short timeout proves the wait was
        // computed and slept without panicking.
        let res = tokio::time::timeout(Duration::from_millis(50), limiter.acquire()).await;
        assert!(
            res.is_err(),
            "acquire should still be parking on the clamped wait"
        );
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

    #[test]
    #[should_panic(expected = "refill rate must be > 0")]
    fn rejects_zero_tokens_per_period() {
        // A bucket whose refill adds zero tokens never replenishes.
        let _ =
            RateLimiter::new(RateLimiterPolicy::new(0, Duration::from_secs(1)).with_max_tokens(1));
    }

    #[test]
    #[should_panic(expected = "refill rate must be > 0")]
    fn rejects_zero_refill_period() {
        // A zero refill period would make the refill rate infinite (division by zero).
        let _ = RateLimiter::new(RateLimiterPolicy {
            max_tokens: 1,
            tokens_per_period: 1,
            refill_period: Duration::ZERO,
            burst: 0,
        });
    }

    // ----- weighted token bucket -----

    #[test]
    fn try_acquire_n_debits_cost() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(10).with_max_tokens(10),
        ));
        assert!(limiter.try_acquire_n(3).is_some());
        assert_eq!(limiter.tokens_available(), 7);
        assert!(limiter.try_acquire_n(7).is_some());
        assert_eq!(limiter.tokens_available(), 0);
        assert!(limiter.try_acquire_n(1).is_none());
    }

    #[test]
    fn try_acquire_n_zero_cost_always_admits_without_debit() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(10).with_max_tokens(2),
        ));
        for _ in 0..100 {
            assert!(limiter.try_acquire_n(0).is_some());
        }
        assert_eq!(limiter.tokens_available(), 2);
    }

    #[test]
    fn try_acquire_n_over_capacity_rejects_without_draining() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(10).with_max_tokens(5),
        ));
        assert!(limiter.try_acquire_n(6).is_none());
        assert_eq!(limiter.tokens_available(), 5);
    }

    #[tokio::test]
    async fn acquire_n_parks_for_weighted_cost() {
        // 100 tokens/sec, capacity 5: drain, then acquire_n(3) waits ~30ms for 3 tokens.
        // `start` before the drain anchors the refill clock at/after it, so the lower bound is
        // robust to scheduling jitter (jitter only inflates the measured elapsed).
        let start = std::time::Instant::now();
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(100).with_max_tokens(5),
        ));
        assert!(limiter.try_acquire_n(5).is_some());
        let _p = limiter.acquire_n(3).await;
        assert!(start.elapsed() >= Duration::from_millis(15));
    }

    // ----- reservation refund / commit -----

    #[test]
    fn reservation_drop_refunds() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(10).with_max_tokens(10),
        ));
        {
            let _r = limiter.try_reserve_n(4).expect("reserve");
            assert_eq!(limiter.tokens_available(), 6);
        } // _r drops -> refunds
        assert_eq!(limiter.tokens_available(), 10);
    }

    #[test]
    fn reservation_commit_keeps_debit() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(10).with_max_tokens(10),
        ));
        {
            let mut r = limiter.try_reserve_n(4).expect("reserve");
            r.commit();
        } // committed -> no refund
        assert_eq!(limiter.tokens_available(), 6);
    }

    #[test]
    fn reservation_refund_is_capped_at_capacity() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(1_000_000).with_max_tokens(5),
        ));
        let r = limiter.try_reserve_n(5).expect("reserve"); // tokens -> 0
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(limiter.tokens_available(), 5); // fast refill tops it back to the cap
        drop(r); // refund 5 on top of a full bucket must stay capped at 5
        assert_eq!(limiter.tokens_available(), 5);
    }

    // ----- windowed limiter -----

    #[test]
    fn window_admits_up_to_limit() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            3,
            Duration::from_secs(60),
        )));
        for _ in 0..3 {
            assert!(w.try_acquire().is_some());
        }
        assert!(w.try_acquire().is_none());
        assert_eq!(w.used(), 3);
        assert_eq!(w.remaining(), 0);
    }

    #[test]
    fn window_weighted_admits_by_cost() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            1000,
            Duration::from_secs(60),
        )));
        assert!(w.try_acquire_n(1500).is_none()); // over the limit, never fits
        assert!(w.try_acquire_n(600).is_some());
        assert!(w.try_acquire_n(600).is_none()); // 600 + 600 > 1000
        assert!(w.try_acquire_n(400).is_some()); // exactly fills
        assert_eq!(w.used(), 1000);
    }

    #[tokio::test]
    async fn window_resets_after_duration() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            2,
            Duration::from_millis(50),
        )));
        assert!(w.try_acquire().is_some());
        assert!(w.try_acquire().is_some());
        assert!(w.try_acquire().is_none());
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert!(w.try_acquire().is_some()); // window elapsed -> full quota again
        assert_eq!(w.used(), 1);
    }

    #[test]
    fn window_time_until_is_reset_when_full() {
        // A 10s window so the "until reset" magnitude is unmistakable and the lower-bound check
        // can't flake under load (setup would have to consume >9s to break it).
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            1,
            Duration::from_secs(10),
        )));
        assert!(w.try_acquire().is_some());
        let t = w.time_until(1);
        // Exhausted -> wait ~ until the window reset (seconds), not a tiny per-unit deficit.
        assert!(t > Duration::from_secs(1), "expected ~window, got {t:?}");
        assert!(t <= Duration::from_secs(10));
    }

    // ----- Meter trait object -----

    #[test]
    fn meter_trait_object_works_for_both_types() {
        let bucket: Arc<dyn Meter> = Arc::new(RateLimiter::new(RateLimiterPolicy::per_second(5)));
        let window: Arc<dyn Meter> = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            5,
            Duration::from_secs(60),
        )));
        assert!(bucket.try_debit(1).is_some());
        assert!(window.try_debit(1).is_some());
        assert!(bucket.snapshot().resets_at.is_none()); // bucket has no boundary
        assert!(window.snapshot().resets_at.is_some()); // window does
    }

    #[tokio::test]
    async fn window_reservation_across_boundary_does_not_corrupt_new_window() {
        // R1 regression: a Reservation taken in window 1 and dropped in window 2 must NOT refund
        // into window 2 (which would erase legitimate usage and over-admit).
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            5,
            Duration::from_millis(50),
        )));
        let r = w.try_reserve_n(3).expect("reserve in window 1"); // window 1: used 3
        tokio::time::sleep(Duration::from_millis(70)).await; // window rolls
        assert!(w.try_acquire_n(2).is_some()); // window 2: used 2
        drop(r); // stale refund must be discarded, not subtract from window 2
        assert_eq!(
            w.used(),
            2,
            "stale reservation refund corrupted the new window"
        );
        // And the new window still enforces its real limit (only 3 more fit).
        assert!(w.try_acquire_n(3).is_some());
        assert!(w.try_acquire_n(1).is_none());
    }

    #[test]
    fn time_until_is_max_for_cost_over_capacity() {
        // R2 regression: an unsatisfiable cost reports "never" on BOTH meter types, so a parking
        // caller sleeps once on MAX instead of busy-looping on a finite deficit.
        let bucket = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(1_000_000).with_max_tokens(5),
        ));
        assert_eq!(bucket.time_until(6), Duration::MAX);
        let window = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            5,
            Duration::from_secs(60),
        )));
        assert_eq!(window.time_until(6), Duration::MAX);
    }

    #[tokio::test]
    async fn acquire_n_over_capacity_parks_rather_than_busy_looping() {
        // R2 regression: with a huge refill rate, the pre-fix code recomputed a ~1us deficit each
        // pass and spun. acquire_n must now park on a single MAX sleep, so a short timeout elapses.
        let bucket = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(1_000_000).with_max_tokens(5),
        ));
        let res = tokio::time::timeout(Duration::from_millis(50), bucket.acquire_n(6)).await;
        assert!(
            res.is_err(),
            "acquire_n(cost>capacity) should park, not return"
        );
    }

    // ----- MultiRateLimiter -----

    fn rl(tokens: u32) -> Arc<dyn Meter> {
        Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(tokens).with_max_tokens(tokens),
        ))
    }
    fn win(limit: u64) -> Arc<dyn Meter> {
        Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            limit,
            Duration::from_secs(60),
        )))
    }

    #[test]
    fn multi_admits_when_all_have_capacity() {
        let m = MultiRateLimiter::new()
            .with_tier("a", rl(5), 1)
            .with_tier("b", win(5), 1);
        assert!(m.try_acquire().is_ok());
    }

    #[test]
    fn multi_rejects_and_reports_binding_tier() {
        let m = MultiRateLimiter::new()
            .with_tier("a", rl(100), 1)
            .with_tier("b", win(1), 1);
        assert!(m.try_acquire().is_ok());
        match m.try_acquire().unwrap_err() {
            CanoError::RateLimited { tier, retry_after } => {
                assert_eq!(tier, "b");
                assert!(retry_after > Duration::ZERO);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn multi_no_leak_on_partial_rejection() {
        // "a" generous (window, no refill drift), "b" exhausted. Rejected attempts must NOT
        // permanently debit "a" — the reservation is refunded when "b" rejects.
        let a = win(100);
        let m = MultiRateLimiter::new()
            .with_tier("a", Arc::clone(&a), 1)
            .with_tier("b", win(1), 1);
        assert!(m.try_acquire().is_ok()); // a:1, b:1
        let a_used_before = a.snapshot().used;
        for _ in 0..10 {
            assert!(m.try_acquire().is_err()); // each blocked by "b"
        }
        assert_eq!(a.snapshot().used, a_used_before, "tier a leaked budget");
    }

    #[test]
    fn multi_weighted_mixed_tiers() {
        // A request-count tier plus a usage-cost (token) tier under one gate.
        let m = MultiRateLimiter::new()
            .with_tier("requests", win(2), 1)
            .with_tier("tokens", win(1000), 600);
        assert!(m.try_acquire().is_ok()); // requests:1, tokens:600
        // tokens: 600 + 600 > 1000 -> blocked by "tokens".
        assert!(
            matches!(m.try_acquire().unwrap_err(), CanoError::RateLimited { tier, .. } if tier == "tokens")
        );
    }

    #[test]
    fn multi_per_request_tier_selection() {
        let m = MultiRateLimiter::new()
            .with_tier("shared", win(10), 1)
            .with_tier("model_x", win(1), 1);
        assert!(m.try_acquire_for(&["shared", "model_x"]).is_ok());
        // model_x exhausted; a request touching only "shared" still succeeds...
        assert!(m.try_acquire_for(&["shared"]).is_ok());
        // ...but one touching model_x is blocked.
        assert!(m.try_acquire_for(&["shared", "model_x"]).is_err());
    }

    #[test]
    fn multi_zero_cost_tier_is_inert() {
        let m = MultiRateLimiter::new()
            .with_tier("off", win(1), 0) // cost 0 -> never blocks or debits
            .with_tier("on", win(2), 1);
        for _ in 0..2 {
            assert!(m.try_acquire().is_ok());
        }
        assert!(m.try_acquire().is_err()); // blocked by "on", never by "off"
    }

    #[tokio::test]
    async fn multi_acquire_parks_then_admits() {
        // "b" is a fast bucket (100/s, cap 1): after draining, acquire parks ~10ms then admits.
        // `start` before constructing the tiers anchors b's refill clock at/after it, so the
        // lower bound is robust to scheduling jitter.
        let start = std::time::Instant::now();
        let b: Arc<dyn Meter> = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(100).with_max_tokens(1),
        ));
        let m = MultiRateLimiter::new()
            .with_tier("a", rl(1000), 1)
            .with_tier("b", b, 1);
        assert!(m.try_acquire().is_ok()); // drain b's single token
        let _p = m.acquire().await; // must park for b to refill
        assert!(start.elapsed() >= Duration::from_millis(5));
    }

    #[test]
    fn multi_commit_makes_all_tiers_permanent() {
        let a = win(2);
        let b = win(2);
        let m = MultiRateLimiter::new()
            .with_tier("a", Arc::clone(&a), 1)
            .with_tier("b", Arc::clone(&b), 1);
        let permit = m.try_acquire().expect("ok");
        drop(permit); // committed -> no refund
        assert_eq!(a.snapshot().used, 1);
        assert_eq!(b.snapshot().used, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multi_concurrent_never_exceeds_scarcest() {
        // The scarce tier admits exactly 5 within its window; "a" is generous. 24 tasks race.
        let scarce = win(5);
        let m = Arc::new(
            MultiRateLimiter::new()
                .with_tier("a", rl(1000), 1)
                .with_tier("scarce", Arc::clone(&scarce), 1),
        );
        let mut handles = Vec::new();
        for _ in 0..24 {
            let m = Arc::clone(&m);
            handles.push(tokio::spawn(async move { m.try_acquire().is_ok() }));
        }
        let mut ok = 0;
        for h in handles {
            if h.await.unwrap() {
                ok += 1;
            }
        }
        assert_eq!(
            ok, 5,
            "exactly the scarcest tier's capacity should be admitted"
        );
        assert_eq!(scarce.snapshot().used, 5);
    }

    // ----- edge cases: WindowedRateLimiter construction & policy -----

    #[test]
    #[should_panic(expected = "WindowPolicy::limit must be >= 1")]
    fn window_rejects_zero_limit() {
        let _ = WindowedRateLimiter::new(WindowPolicy::new(0, Duration::from_secs(1)));
    }

    #[test]
    #[should_panic(expected = "WindowPolicy::window must be > 0")]
    fn window_rejects_zero_window() {
        let _ = WindowedRateLimiter::new(WindowPolicy::new(5, Duration::ZERO));
    }

    #[test]
    fn window_policy_constructors_scale_durations() {
        assert_eq!(
            WindowPolicy::per_hours(100, 5).window,
            Duration::from_secs(5 * 3600)
        );
        assert_eq!(
            WindowPolicy::per_days(100, 7).window,
            Duration::from_secs(7 * 86_400)
        );
        assert_eq!(WindowPolicy::per_hours(42, 5).limit, 42);
    }

    // ----- edge cases: exact-boundary cost -----

    #[test]
    fn bucket_cost_equal_to_capacity_succeeds_then_empty() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::new(5, Duration::from_secs(3600)).with_max_tokens(5),
        ));
        assert!(limiter.try_acquire_n(5).is_some()); // exactly capacity
        assert!(limiter.try_acquire_n(1).is_none());
    }

    #[test]
    fn window_cost_equal_to_limit_succeeds_then_full() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            5,
            Duration::from_secs(60),
        )));
        assert!(w.try_acquire_n(5).is_some()); // exactly the limit
        assert!(w.try_acquire_n(1).is_none());
    }

    #[test]
    fn window_zero_cost_is_inert() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            1,
            Duration::from_secs(60),
        )));
        for _ in 0..100 {
            assert!(w.try_acquire_n(0).is_some());
        }
        assert_eq!(w.used(), 0);
    }

    // ----- edge cases: WindowedRateLimiter async park & reservation lifecycle -----

    #[tokio::test]
    async fn window_acquire_n_parks_until_reset() {
        // limit 1, window 100ms: drain, then acquire must park ~until the reset. `start` is
        // captured BEFORE construction — the window resets at window_start (= construction) +
        // 100ms >= start + 100ms, so `acquire` returns >= start + 100ms and the lower bound is
        // robust to scheduling jitter (jitter only inflates the measured elapsed).
        let start = std::time::Instant::now();
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            1,
            Duration::from_millis(100),
        )));
        assert!(w.try_acquire().is_some());
        let _p = w.acquire().await;
        assert!(
            start.elapsed() >= Duration::from_millis(50),
            "should park until the window reset, waited {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn window_reservation_within_window_refunds() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            5,
            Duration::from_secs(60),
        )));
        {
            let _r = w.try_reserve_n(3).expect("reserve");
            assert_eq!(w.used(), 3);
        } // dropped within the same window -> refund
        assert_eq!(w.used(), 0);
    }

    #[test]
    fn window_reservation_commit_keeps_usage() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            5,
            Duration::from_secs(60),
        )));
        {
            let mut r = w.try_reserve_n(3).expect("reserve");
            r.commit();
        }
        assert_eq!(w.used(), 3);
    }

    // ----- edge cases: snapshot semantics -----

    #[test]
    fn bucket_snapshot_reports_no_reset_boundary() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::per_second(10).with_max_tokens(10),
        ));
        let _ = limiter.try_acquire_n(4);
        let s = limiter.snapshot();
        assert_eq!(s.limit, 10);
        assert_eq!(s.used, 4);
        assert_eq!(s.available_in, Duration::ZERO); // tokens still available
        assert!(s.resets_at.is_none()); // a bucket has no boundary
    }

    #[test]
    fn window_snapshot_reports_reset_boundary() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            10,
            Duration::from_secs(60),
        )));
        let _ = w.try_acquire_n(4);
        let s = w.snapshot();
        assert_eq!(s.limit, 10);
        assert_eq!(s.used, 4);
        assert_eq!(s.available_in, Duration::ZERO); // room remains
        assert!(s.resets_at.is_some()); // a window has a reset instant
    }

    #[test]
    fn exhausted_window_snapshot_available_in_is_until_reset() {
        // A 10s window so the "until reset" magnitude is unmistakable regardless of setup jitter.
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            1,
            Duration::from_secs(10),
        )));
        assert!(w.try_acquire().is_some());
        let s = w.snapshot();
        assert_eq!(s.used, 1);
        assert!(
            s.available_in > Duration::from_secs(1),
            "exhausted window should report ~window until free, got {:?}",
            s.available_in
        );
    }

    // ----- edge cases: shared state via Clone -----

    #[test]
    fn cloning_rate_limiter_shares_the_bucket() {
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::new(5, Duration::from_secs(3600)).with_max_tokens(5),
        ));
        let clone = Arc::new((*limiter).clone());
        assert!(limiter.try_acquire_n(5).is_some()); // drain via the original
        assert!(clone.try_acquire().is_none()); // the clone sees the shared, drained bucket
    }

    #[test]
    fn cloning_window_shares_state() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            2,
            Duration::from_secs(60),
        )));
        let clone = Arc::new((*w).clone());
        assert!(w.try_acquire().is_some());
        assert!(w.try_acquire().is_some());
        assert!(clone.try_acquire().is_none()); // shared count exhausted
    }

    // ----- edge cases: MultiRateLimiter composition -----

    #[test]
    fn empty_multi_rate_limiter_admits() {
        let m = MultiRateLimiter::new();
        assert!(m.try_acquire().is_ok());
    }

    #[test]
    fn multi_acquire_for_unknown_tier_enforces_nothing() {
        // A name in `only` that matches no configured tier simply enforces no tier (the filter is
        // a whitelist intersected with the configured tiers), so the call is admitted.
        let m = MultiRateLimiter::new().with_tier("a", win(1), 1);
        assert!(m.try_acquire().is_ok());
        assert!(m.try_acquire().is_err()); // "a" is now full
        assert!(m.try_acquire_for(&["nonexistent"]).is_ok());
    }

    #[test]
    fn multi_first_tier_rejection_leaves_later_tiers_untouched() {
        let first = win(1);
        let second = win(100);
        let m = MultiRateLimiter::new()
            .with_tier("first", Arc::clone(&first), 1)
            .with_tier("second", Arc::clone(&second), 1);
        assert!(m.try_acquire().is_ok()); // first:1, second:1
        let second_used = second.snapshot().used;
        for _ in 0..5 {
            match m.try_acquire().unwrap_err() {
                CanoError::RateLimited { tier, .. } => assert_eq!(tier, "first"),
                other => panic!("expected RateLimited(first), got {other:?}"),
            }
        }
        // The first tier rejects before the second is ever debited -> second unchanged.
        assert_eq!(second.snapshot().used, second_used);
    }

    #[test]
    fn same_meter_in_two_tiers_debits_once_per_tier() {
        // Documented aliasing: one shared meter under two tiers is charged once per tier.
        let shared = win(10);
        let m = MultiRateLimiter::new()
            .with_tier("a", Arc::clone(&shared), 1)
            .with_tier("b", Arc::clone(&shared), 1);
        assert!(m.try_acquire().is_ok());
        assert_eq!(shared.snapshot().used, 2);
    }

    #[tokio::test]
    async fn multi_acquire_parks_on_window_tier_reset() {
        // Binding tier is a fast WINDOW: acquire must park until it resets (retry_after = reset).
        // `start` before construction anchors the window at/after it, so the lower bound is robust
        // to scheduling jitter.
        let start = std::time::Instant::now();
        let w: Arc<dyn Meter> = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            1,
            Duration::from_millis(100),
        )));
        let m = MultiRateLimiter::new().with_tier("w", w, 1);
        assert!(m.try_acquire().is_ok()); // drain
        let _p = m.acquire().await;
        assert!(
            start.elapsed() >= Duration::from_millis(50),
            "should park until the window tier reset, waited {:?}",
            start.elapsed()
        );
    }

    // ----- edge cases: concurrency on the standalone primitives -----

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bucket_concurrent_admits_exactly_capacity() {
        // Capacity 10, refill negligible over the test -> exactly 10 of 40 racers succeed.
        let limiter = Arc::new(RateLimiter::new(
            RateLimiterPolicy::new(10, Duration::from_secs(86_400)).with_max_tokens(10),
        ));
        let mut handles = Vec::new();
        for _ in 0..40 {
            let l = Arc::clone(&limiter);
            handles.push(tokio::spawn(async move { l.try_acquire().is_some() }));
        }
        let mut ok = 0;
        for h in handles {
            if h.await.unwrap() {
                ok += 1;
            }
        }
        assert_eq!(ok, 10);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn window_concurrent_admits_exactly_limit() {
        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            10,
            Duration::from_secs(60),
        )));
        let mut handles = Vec::new();
        for _ in 0..40 {
            let w = Arc::clone(&w);
            handles.push(tokio::spawn(async move { w.try_acquire().is_some() }));
        }
        let mut ok = 0;
        for h in handles {
            if h.await.unwrap() {
                ok += 1;
            }
        }
        assert_eq!(ok, 10);
        assert_eq!(w.used(), 10);
    }

    // ----- edge cases: impossible / extreme costs (graceful, no panic or busy-loop) -----

    #[test]
    fn multi_tier_cost_over_meter_capacity_is_permanently_rejected() {
        // A tier asking for more than its meter can ever provide: try_acquire rejects immediately
        // with retry_after == MAX, never looping or panicking.
        let m = MultiRateLimiter::new().with_tier("impossible", win(5), 10);
        match m.try_acquire().unwrap_err() {
            CanoError::RateLimited { tier, retry_after } => {
                assert_eq!(tier, "impossible");
                assert_eq!(retry_after, Duration::MAX);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn huge_cost_rejects_without_panicking() {
        let bucket = Arc::new(RateLimiter::new(RateLimiterPolicy::per_second(5)));
        assert!(bucket.try_acquire_n(u64::MAX).is_none());
        assert_eq!(bucket.time_until(u64::MAX), Duration::MAX);

        let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
            5,
            Duration::from_secs(60),
        )));
        assert!(w.try_acquire_n(u64::MAX).is_none());
        assert_eq!(w.time_until(u64::MAX), Duration::MAX);
    }
}

#[cfg(all(test, feature = "metrics"))]
mod metrics_tests {
    use super::*;
    use crate::metrics::test_support::*;
    use std::sync::Arc;
    use std::time::Duration;

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

    #[test]
    fn immediate_acquire_does_not_record_wait_histogram() {
        let ((), rows) = run_with_recorder(|| async {
            // Bucket starts full → acquire returns without parking.
            let limiter = Arc::new(RateLimiter::new(
                RateLimiterPolicy::per_second(10).with_max_tokens(2),
            ));
            let _ = limiter.acquire().await;
        });
        // The token was consumed...
        assert_eq!(counter(&rows, "cano_rate_limiter_acquired_total", &[]), 1);
        // ...but since no sleep occurred, the blocked-time histogram has no sample at all.
        assert_eq!(
            histogram_count_opt(&rows, "cano_rate_limiter_wait_seconds", &[]),
            None,
            "wait_seconds must not record a sample when acquire did not park"
        );
        // And nothing was throttled.
        assert_eq!(
            counter_opt(
                &rows,
                "cano_rate_limiter_throttled_total",
                &[("result", "waited")]
            ),
            None
        );
    }

    #[test]
    fn weighted_acquire_records_tokens_consumed() {
        let ((), rows) = run_with_recorder(|| async {
            let limiter = Arc::new(RateLimiter::new(
                RateLimiterPolicy::per_second(100).with_max_tokens(100),
            ));
            let _ = limiter.try_acquire_n(7);
            let _ = limiter.try_acquire_n(3);
        });
        assert_eq!(
            counter(&rows, "cano_rate_limiter_tokens_consumed_total", &[]),
            10
        );
        assert_eq!(counter(&rows, "cano_rate_limiter_acquired_total", &[]), 2);
    }

    #[test]
    fn multi_rejection_records_throttled_with_tier() {
        let ((), rows) = run_with_recorder(|| async {
            let a: Arc<dyn Meter> = Arc::new(RateLimiter::new(
                RateLimiterPolicy::per_second(100).with_max_tokens(100),
            ));
            let b: Arc<dyn Meter> = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
                1,
                Duration::from_secs(60),
            )));
            let m = MultiRateLimiter::new()
                .with_tier("a", a, 1)
                .with_tier("b", b, 1);
            assert!(m.try_acquire().is_ok());
            assert!(m.try_acquire().is_err());
        });
        assert_eq!(
            counter(
                &rows,
                "cano_multi_rate_limiter_throttled_total",
                &[("tier", "b")]
            ),
            1
        );
    }

    #[test]
    fn window_acquire_records_acquired_tokens_and_rejected() {
        let ((), rows) = run_with_recorder(|| async {
            let w = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
                2,
                Duration::from_secs(60),
            )));
            assert!(w.try_acquire_n(2).is_some());
            assert!(w.try_acquire().is_none()); // window full
        });
        assert_eq!(counter(&rows, "cano_rate_limiter_acquired_total", &[]), 1);
        assert_eq!(
            counter(&rows, "cano_rate_limiter_tokens_consumed_total", &[]),
            2
        );
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
    fn multi_acquire_for_records_throttle_while_parking() {
        // The binding tier is a fast bucket (100/s, cap 1): acquire drains it, parks ~10ms, then
        // admits — recording at least one per-tier throttle along the way (the R3 fix).
        let ((), rows) = run_with_recorder(|| async {
            let b: Arc<dyn Meter> = Arc::new(RateLimiter::new(
                RateLimiterPolicy::per_second(100).with_max_tokens(1),
            ));
            let m = MultiRateLimiter::new().with_tier("b", b, 1);
            assert!(m.try_acquire().is_ok()); // drain
            let _p = m.acquire().await; // parks, then admits
        });
        assert!(
            counter(
                &rows,
                "cano_multi_rate_limiter_throttled_total",
                &[("tier", "b")]
            ) >= 1,
            "acquire() should record a throttle for each blocked attempt while parking"
        );
    }
}
