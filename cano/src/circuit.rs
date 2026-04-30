//! # Circuit Breaker — Fail-Fast Protection for Flaky Dependencies
//!
//! A [`CircuitBreaker`] short-circuits calls to a failing dependency so the system
//! degrades gracefully instead of hammering a broken backend with retries.
//!
//! State machine:
//!
//! - **`Closed`** — calls flow through. Each consecutive failure increments a counter;
//!   when the counter reaches `failure_threshold` the breaker trips to `Open`.
//!   A success resets the counter.
//! - **`Open { until }`** — every call is rejected with [`CanoError::CircuitOpen`]
//!   without invoking the underlying task. After `reset_timeout` elapses the next
//!   `try_acquire` call lazily transitions the breaker into `HalfOpen`.
//! - **`HalfOpen`** — at most `half_open_max_calls` trial calls are admitted concurrently.
//!   Each trial that returns success bumps a success counter; once that counter
//!   reaches `half_open_max_calls` the breaker closes. Any failure during HalfOpen
//!   immediately reopens the breaker (with a fresh `until = now + reset_timeout`).
//!
//! ## Default integration: wire into `TaskConfig`
//!
//! The recommended path is to attach the breaker to a [`crate::task::TaskConfig`] via
//! [`crate::task::TaskConfig::with_circuit_breaker`]. The retry loop in
//! [`crate::task::run_with_retries`] then:
//!
//! 1. Calls [`CircuitBreaker::try_acquire`] **before** each attempt — an open breaker
//!    short-circuits the entire retry loop with [`CanoError::CircuitOpen`] (no retries
//!    consumed, no task body invocation).
//! 2. Records the attempt's outcome **after** it returns — so the breaker observes the
//!    call's success/failure before the workflow propagates it.
//!
//! ```rust
//! use std::sync::Arc;
//! use std::time::Duration;
//! use cano::prelude::*;
//!
//! let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
//!     failure_threshold: 5,
//!     reset_timeout: Duration::from_secs(30),
//!     half_open_max_calls: 1,
//! }));
//!
//! let config = TaskConfig::new().with_circuit_breaker(Arc::clone(&breaker));
//! # let _ = config;
//! ```
//!
//! Sharing one breaker across multiple tasks lets the trip from any task protect every
//! task that depends on the same flaky resource.
//!
//! ## Advanced: register as a `Resource`
//!
//! [`CircuitBreaker`] also implements [`Resource`] (no-op lifecycle) so it can be
//! registered in [`Resources`] and looked up by key inside a task body. **This bypasses
//! the retry-loop integration**: callers must invoke [`try_acquire`](CircuitBreaker::try_acquire),
//! [`record_success`](CircuitBreaker::record_success), and
//! [`record_failure`](CircuitBreaker::record_failure) themselves, and they lose both
//! (a) the automatic retry-loop short-circuit on `CircuitOpen` and (b) the
//! before-call/after-call ordering guarantee that `with_circuit_breaker` provides. Use
//! this only for advanced cases where one task body needs to share one breaker across
//! several internal sub-calls.
//!
//! [`Resources`]: crate::resource::Resources
//! [`Resource`]: crate::resource::Resource

use crate::error::CanoError;
use crate::resource::Resource;
use cano_macros::resource;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "tracing")]
use tracing::info;

/// Policy parameters controlling [`CircuitBreaker`] state transitions.
#[derive(Debug, Clone)]
pub struct CircuitPolicy {
    /// Consecutive failures in `Closed` that trip the breaker into `Open`.
    pub failure_threshold: u32,
    /// Time spent in `Open` before the next `try_acquire` lazily promotes to `HalfOpen`.
    pub reset_timeout: Duration,
    /// Cap on concurrent trial calls admitted while `HalfOpen`, **and** the number of
    /// consecutive trial successes required to close the breaker.
    ///
    /// These two roles are intentionally fused: any failure during `HalfOpen`
    /// immediately reopens the breaker, so to reach the close-threshold every admitted
    /// trial must succeed. Setting this to `N > 1` therefore means "admit up to `N`
    /// concurrent probes; close only after `N` of them have all succeeded". The
    /// default is `1` — one probe, one success closes — which is the right choice for
    /// almost every workload.
    pub half_open_max_calls: u32,
}

impl Default for CircuitPolicy {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            reset_timeout: Duration::from_secs(30),
            half_open_max_calls: 1,
        }
    }
}

/// The current state of a [`CircuitBreaker`].
///
/// Returned from [`CircuitBreaker::state`] for inspection / observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Calls flow through. Failures are counted toward the trip threshold.
    Closed,
    /// All calls are rejected until `until`.
    Open {
        /// The earliest instant at which the breaker may transition to `HalfOpen`.
        until: Instant,
    },
    /// A bounded number of trial calls are admitted to probe the dependency.
    HalfOpen,
}

#[derive(Debug)]
struct Inner {
    state: CircuitState,
    consecutive_failures: u32,
    half_open_in_flight: u32,
    half_open_successes: u32,
    /// Monotonic epoch bumped on every state transition. Permits capture the
    /// epoch at issuance; outcomes from a stale epoch are ignored at
    /// consumption time. This prevents a slow caller — whose call straddles
    /// one or more `Open ↔ HalfOpen ↔ Closed` transitions — from corrupting
    /// the *next* state-machine session by bumping its counters.
    epoch: u64,
}

impl Inner {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            half_open_in_flight: 0,
            half_open_successes: 0,
            epoch: 0,
        }
    }

    fn reset_half_open(&mut self) {
        self.half_open_in_flight = 0;
        self.half_open_successes = 0;
    }

    fn bump_epoch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Apply a state transition: install the new state, reset the half-open
    /// counters, and bump the epoch so any in-flight permits from the prior
    /// session are filtered as stale.
    fn transition(&mut self, new_state: CircuitState) {
        self.state = new_state;
        self.reset_half_open();
        self.bump_epoch();
    }
}

/// A reusable circuit breaker.
///
/// Cheap to clone (`Arc` internally). Share one breaker across every task that
/// depends on the same external resource so a trip from any caller protects every
/// caller. See the [module documentation](self) for the state-machine semantics.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    inner: Arc<Mutex<Inner>>,
    policy: CircuitPolicy,
}

/// Upper bound on `CircuitPolicy::half_open_max_calls`.
///
/// Pathological values (very close to [`u32::MAX`]) would either silently never close
/// the breaker — once `half_open_successes` reaches its `saturating_add` ceiling no
/// further increment can land — or just take effectively forever to do so. Either way
/// it's a configuration mistake, so we reject it at construction.
const HALF_OPEN_MAX_CALLS_LIMIT: u32 = u32::MAX / 2;

impl CircuitBreaker {
    /// Construct a breaker in the `Closed` state.
    ///
    /// # Panics
    ///
    /// Panics if `policy.failure_threshold == 0`,
    /// `policy.half_open_max_calls == 0`, or
    /// `policy.half_open_max_calls > u32::MAX / 2`:
    ///
    /// - `failure_threshold == 0` is nonsensical: every failed call would meet the
    ///   trip threshold before any positive failure count exists.
    /// - `0` would deadlock the breaker permanently in `HalfOpen` (no probe could ever
    ///   be admitted, so the success counter could never advance).
    /// - Values approaching `u32::MAX` interact badly with the `saturating_add` on
    ///   `half_open_successes` — either the counter saturates before the threshold is
    ///   reached and the breaker never closes, or recovery takes effectively forever.
    ///
    /// Misconfigured policies are programmer errors caught at construction, before any
    /// task runs — consistent with the panic in `Resources::insert` for duplicate keys.
    pub fn new(policy: CircuitPolicy) -> Self {
        assert!(
            policy.failure_threshold >= 1,
            "CircuitPolicy::failure_threshold must be >= 1; 0 would make the breaker trip semantics nonsensical"
        );
        assert!(
            policy.half_open_max_calls >= 1,
            "CircuitPolicy::half_open_max_calls must be >= 1; 0 deadlocks the breaker in HalfOpen (no trial would ever be admitted)"
        );
        assert!(
            policy.half_open_max_calls <= HALF_OPEN_MAX_CALLS_LIMIT,
            "CircuitPolicy::half_open_max_calls must be <= {HALF_OPEN_MAX_CALLS_LIMIT}; got {} (values near u32::MAX may prevent the breaker from ever closing)",
            policy.half_open_max_calls
        );
        Self {
            inner: Arc::new(Mutex::new(Inner::new())),
            policy,
        }
    }

    /// Return the policy this breaker was constructed with.
    pub fn policy(&self) -> &CircuitPolicy {
        &self.policy
    }

    /// Inspect the current state. Useful for observability and tests.
    pub fn state(&self) -> CircuitState {
        let inner = self.inner.lock().expect("circuit breaker mutex poisoned");
        inner.state
    }

    /// Attempt to acquire permission to make a call.
    ///
    /// Returns a [`Permit`] that the caller must consume by calling
    /// [`record_success`](Self::record_success) or [`record_failure`](Self::record_failure)
    /// once the call completes. If neither is called the permit's `Drop` impl treats
    /// the call as a failure — this keeps the in-flight counter accurate even on panic
    /// or early return.
    ///
    /// # Errors
    ///
    /// Returns [`CanoError::CircuitOpen`] when the breaker is `Open` (and the cool-down
    /// has not elapsed) or when `HalfOpen` already has `half_open_max_calls` trials in
    /// flight.
    pub fn try_acquire(self: &Arc<Self>) -> Result<Permit, CanoError> {
        let mut inner = self.inner.lock().expect("circuit breaker mutex poisoned");

        // Lazy Open -> HalfOpen transition. We only flip when a caller actually
        // tries to acquire; no background task is required. The epoch bump
        // here invalidates any still-in-flight permits issued during the prior
        // Open / Closed session, so their outcomes won't corrupt this probe.
        if let CircuitState::Open { until } = inner.state
            && Instant::now() >= until
        {
            inner.transition(CircuitState::HalfOpen);
            #[cfg(feature = "tracing")]
            info!(
                half_open_max_calls = self.policy.half_open_max_calls,
                "circuit breaker transition: Open -> HalfOpen"
            );
        }

        let epoch = inner.epoch;
        match inner.state {
            CircuitState::Closed => Ok(Permit::new(Arc::clone(self), false, epoch)),
            CircuitState::Open { .. } => Err(CanoError::circuit_open(
                "circuit breaker open: rejecting call",
            )),
            CircuitState::HalfOpen => {
                if inner.half_open_in_flight >= self.policy.half_open_max_calls {
                    return Err(CanoError::circuit_open(
                        "circuit breaker half-open: trial slot exhausted",
                    ));
                }
                inner.half_open_in_flight += 1;
                Ok(Permit::new(Arc::clone(self), true, epoch))
            }
        }
    }

    /// Record a successful call, consuming the permit.
    ///
    /// In `Closed` this resets the consecutive-failure counter. In `HalfOpen` it bumps
    /// the success counter and closes the breaker once `half_open_max_calls` successes
    /// have been observed.
    pub fn record_success(&self, mut permit: Permit) {
        permit.consumed = true;
        let mut inner = self.inner.lock().expect("circuit breaker mutex poisoned");

        // Stale outcome from a prior epoch: the in-flight slot, if any, was
        // already wiped by `reset_half_open()` at the transition. Skip both
        // counter accounting and the in-flight decrement.
        if permit.epoch != inner.epoch {
            return;
        }

        if permit.was_half_open && inner.half_open_in_flight > 0 {
            inner.half_open_in_flight -= 1;
        }

        match inner.state {
            CircuitState::Closed => {
                inner.consecutive_failures = 0;
            }
            CircuitState::HalfOpen => {
                inner.half_open_successes = inner.half_open_successes.saturating_add(1);
                if inner.half_open_successes >= self.policy.half_open_max_calls {
                    inner.consecutive_failures = 0;
                    inner.transition(CircuitState::Closed);
                    #[cfg(feature = "tracing")]
                    info!(
                        successes = self.policy.half_open_max_calls,
                        "circuit breaker transition: HalfOpen -> Closed"
                    );
                }
            }
            CircuitState::Open { .. } => {
                // Unreachable when epoch is current — any transition out of
                // Closed/HalfOpen would have bumped the epoch.
                debug_assert!(
                    false,
                    "record_success on Open with current epoch is unreachable; epoch tracking should have filtered the stale outcome"
                );
            }
        }
    }

    /// Record a failed call, consuming the permit.
    ///
    /// In `Closed` this increments the consecutive-failure counter and trips the breaker
    /// when the threshold is reached. In `HalfOpen` it immediately reopens the breaker
    /// with a fresh cool-down deadline.
    pub fn record_failure(&self, mut permit: Permit) {
        permit.consumed = true;
        self.do_record_failure(permit.was_half_open, permit.epoch);
    }

    fn do_record_failure(&self, was_half_open: bool, permit_epoch: u64) {
        let mut inner = self.inner.lock().expect("circuit breaker mutex poisoned");

        // Stale outcome from a prior epoch — see `record_success` for rationale.
        if permit_epoch != inner.epoch {
            return;
        }

        if was_half_open && inner.half_open_in_flight > 0 {
            inner.half_open_in_flight -= 1;
        }

        match inner.state {
            CircuitState::Closed => {
                inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
                if inner.consecutive_failures >= self.policy.failure_threshold {
                    inner.transition(CircuitState::Open {
                        until: Instant::now() + self.policy.reset_timeout,
                    });
                    #[cfg(feature = "tracing")]
                    info!(
                        failure_threshold = self.policy.failure_threshold,
                        reset_timeout_ms = self.policy.reset_timeout.as_millis() as u64,
                        "circuit breaker transition: Closed -> Open"
                    );
                }
            }
            CircuitState::HalfOpen => {
                // `consecutive_failures` is only read in the Closed arm; the path back
                // to Closed (via HalfOpen -> Closed) resets it. No write needed here.
                inner.transition(CircuitState::Open {
                    until: Instant::now() + self.policy.reset_timeout,
                });
                #[cfg(feature = "tracing")]
                info!(
                    reset_timeout_ms = self.policy.reset_timeout.as_millis() as u64,
                    "circuit breaker transition: HalfOpen -> Open"
                );
            }
            CircuitState::Open { .. } => {
                // Unreachable when epoch is current; the epoch check above filters
                // stale outcomes from prior state-machine sessions.
                debug_assert!(
                    false,
                    "record_failure on Open with current epoch is unreachable; epoch tracking should have filtered the stale outcome"
                );
            }
        }
    }
}

#[resource]
impl Resource for CircuitBreaker {}

/// A token that authorises one call against a [`CircuitBreaker`].
///
/// Consume it by passing it to [`CircuitBreaker::record_success`] or
/// [`CircuitBreaker::record_failure`]. If a `Permit` is dropped without being
/// consumed it is treated as a failure — this keeps the in-flight counter accurate
/// when the caller panics or returns early.
#[must_use = "drop a Permit only as a deliberate failure signal; pass it to record_success or record_failure to indicate the call outcome"]
pub struct Permit {
    breaker: Arc<CircuitBreaker>,
    was_half_open: bool,
    consumed: bool,
    /// Epoch the permit was issued in. Used at consumption time to ignore
    /// outcomes whose state-machine session is no longer current.
    epoch: u64,
}

impl Permit {
    fn new(breaker: Arc<CircuitBreaker>, was_half_open: bool, epoch: u64) -> Self {
        Self {
            breaker,
            was_half_open,
            consumed: false,
            epoch,
        }
    }
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permit")
            .field("was_half_open", &self.was_half_open)
            .field("consumed", &self.consumed)
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if !self.consumed {
            self.breaker
                .do_record_failure(self.was_half_open, self.epoch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_policy() -> CircuitPolicy {
        CircuitPolicy {
            failure_threshold: 3,
            reset_timeout: Duration::from_millis(20),
            half_open_max_calls: 2,
        }
    }

    #[test]
    fn closed_initial_state() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn opens_after_threshold_failures() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..3 {
            let permit = breaker.try_acquire().unwrap();
            breaker.record_failure(permit);
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        let err = breaker.try_acquire().unwrap_err();
        assert_eq!(err.category(), "circuit_open");
    }

    #[test]
    fn success_resets_consecutive_failures() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..2 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        // One success resets the counter, so two more failures should not trip yet.
        let p = breaker.try_acquire().unwrap();
        breaker.record_success(p);
        for _ in 0..2 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn open_to_half_open_after_reset_timeout() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..3 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        tokio::time::sleep(Duration::from_millis(40)).await;

        // Lazy transition: try_acquire flips Open -> HalfOpen.
        let permit = breaker.try_acquire().unwrap();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        breaker.record_success(permit);
    }

    #[tokio::test]
    async fn half_open_full_success_closes() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..3 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;

        // half_open_max_calls = 2, so two consecutive successes close the breaker.
        let p1 = breaker.try_acquire().unwrap();
        breaker.record_success(p1);
        let p2 = breaker.try_acquire().unwrap();
        breaker.record_success(p2);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn half_open_failure_reopens() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..3 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;

        let p = breaker.try_acquire().unwrap();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        breaker.record_failure(p);
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));
    }

    #[tokio::test]
    async fn half_open_caps_concurrent_trials() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..3 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;

        // half_open_max_calls = 2: two trial permits in flight, third must be rejected.
        let p1 = breaker.try_acquire().unwrap();
        let p2 = breaker.try_acquire().unwrap();
        let err = breaker.try_acquire().unwrap_err();
        assert_eq!(err.category(), "circuit_open");
        breaker.record_success(p1);
        breaker.record_success(p2);
    }

    #[test]
    fn dropped_permit_counts_as_failure() {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..3 {
            let _p = breaker.try_acquire().unwrap();
            // _p drops here without consume → treated as failure.
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));
    }

    #[test]
    fn shared_breaker_trips_for_all_callers() {
        // Two callers share one breaker; failures from caller A trip it for caller B.
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        let breaker_a = Arc::clone(&breaker);
        let breaker_b = Arc::clone(&breaker);

        for _ in 0..3 {
            let p = breaker_a.try_acquire().unwrap();
            breaker_a.record_failure(p);
        }

        let err = breaker_b.try_acquire().unwrap_err();
        assert_eq!(err.category(), "circuit_open");
    }

    #[test]
    #[should_panic(expected = "half_open_max_calls must be >= 1")]
    fn rejects_zero_half_open_max_calls() {
        // Misconfigured policy: zero trial slots would deadlock the breaker
        // permanently in HalfOpen. Construction must fail loudly.
        let _ = CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 1,
            reset_timeout: Duration::from_millis(1),
            half_open_max_calls: 0,
        });
    }

    #[test]
    #[should_panic(expected = "failure_threshold must be >= 1")]
    fn rejects_zero_failure_threshold() {
        let _ = CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 0,
            reset_timeout: Duration::from_millis(1),
            half_open_max_calls: 1,
        });
    }

    #[tokio::test]
    async fn stale_closed_permit_does_not_close_a_later_half_open() {
        // Reproduces the epoch-tracking concern: a Closed permit issued before
        // a trip is consumed during a later HalfOpen probe. Without epoch
        // tracking, `record_success` would see `state == HalfOpen` and bump
        // half_open_successes — closing the breaker on a stale outcome that
        // was never a probe call. With epoch tracking the stale outcome must
        // be ignored and the HalfOpen probe must still need its own success
        // to close the breaker.
        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 2,
            reset_timeout: Duration::from_millis(20),
            half_open_max_calls: 1,
        }));

        // Acquire a permit while still Closed — this is the "slow caller".
        let stale_permit = breaker.try_acquire().unwrap();
        assert_eq!(breaker.state(), CircuitState::Closed);

        // Trip the breaker via two other failures (independent of the stale permit).
        for _ in 0..2 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // Wait out the cool-down so the next try_acquire promotes to HalfOpen.
        tokio::time::sleep(Duration::from_millis(40)).await;
        // Provoke the lazy promotion without consuming the trial slot.
        let probe_permit = breaker.try_acquire().unwrap();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Now consume the *stale* permit with a success. With epoch tracking
        // this is a no-op — it must NOT close the breaker.
        breaker.record_success(stale_permit);
        assert_eq!(
            breaker.state(),
            CircuitState::HalfOpen,
            "stale Closed-epoch success must not close a later HalfOpen"
        );

        // The actual probe still needs its own success to close.
        breaker.record_success(probe_permit);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn open_state_is_debug_loggable() {
        // Operators typically log breaker state via Debug; pin the format so
        // accidental refactors that drop the `Open { until }` shape are caught.
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        for _ in 0..3 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        let dbg = format!("{:?}", breaker.state());
        assert!(
            dbg.starts_with("Open { until:"),
            "expected Debug to start with `Open {{ until:`, got: {dbg}"
        );
    }

    #[test]
    #[should_panic(expected = "must be <=")]
    fn rejects_pathological_half_open_max_calls() {
        // Values near u32::MAX would either saturate half_open_successes before
        // crossing the threshold or take effectively forever — both classed as
        // misconfiguration and rejected at construction.
        let _ = CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 1,
            reset_timeout: Duration::from_millis(1),
            half_open_max_calls: u32::MAX,
        });
    }

    #[tokio::test]
    async fn open_half_open_open_round_trip() {
        // Full lifecycle: trip → cool-down → HalfOpen → probe fails → re-Open
        // → cool-down → HalfOpen → probe succeeds → Closed.
        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 2,
            reset_timeout: Duration::from_millis(20),
            half_open_max_calls: 1,
        }));

        // 1. Trip from Closed.
        for _ in 0..2 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // 2. Cool-down → HalfOpen → failed probe → re-Open with fresh deadline.
        tokio::time::sleep(Duration::from_millis(40)).await;
        let p = breaker.try_acquire().unwrap();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        breaker.record_failure(p);
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // 3. Second cool-down → HalfOpen → successful probe → Closed.
        tokio::time::sleep(Duration::from_millis(40)).await;
        let p = breaker.try_acquire().unwrap();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        breaker.record_success(p);
        assert_eq!(breaker.state(), CircuitState::Closed);

        // 4. Sanity: the closed breaker once again admits calls and fresh
        //    failures count toward a new threshold (counters were reset).
        for _ in 0..2 {
            let p = breaker.try_acquire().unwrap();
            breaker.record_failure(p);
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));
    }
}
