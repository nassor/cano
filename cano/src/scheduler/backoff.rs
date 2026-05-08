//! Backoff policy for failed scheduled runs.
//!
//! Wraps a single [`BackoffPolicy`] type that the scheduler consults after a
//! workflow fails. Distinct from the task-level [`crate::circuit::CircuitBreaker`]:
//! the breaker gates a single task's call to a dependency; this policy gates
//! the scheduler from re-firing an entire flow on a base interval after it just
//! failed, and trips the flow when a streak limit is reached.

use rand::RngExt;
use std::time::Duration;

/// Policy controlling how the scheduler delays subsequent runs of a flow that
/// just failed and when to stop dispatching it altogether.
///
/// A flow registered without a policy keeps the legacy behavior: failures set
/// `Status::Failed(err)` and the next scheduled tick fires per the base
/// schedule. Attaching a policy with [`Scheduler::set_backoff`](super::Scheduler::set_backoff)
/// activates the new code path.
///
/// # Defaults
///
/// `Default` gives 1s initial, 2.0x multiplier, 5min cap, 0.1 jitter, and
/// **no trip limit** (`streak_limit: None`) — the flow keeps retrying with
/// backoff forever. Use [`BackoffPolicy::with_trip`] to ask for a trip after
/// N consecutive failures. The defaults exist for callers who want a
/// reasonable policy without tuning every knob — they are **never** applied
/// silently to existing flows.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    /// Delay applied after the first failure in a streak.
    pub initial: Duration,
    /// Multiplier applied per additional consecutive failure (`initial * multiplier^(streak-1)`).
    pub multiplier: f64,
    /// Hard cap on the computed delay — never sleep longer than this.
    pub max_delay: Duration,
    /// Jitter factor in `[0.0, 1.0]`. Applied as `delay *= 1.0 + rand_in(-jitter, jitter)`.
    pub jitter: f64,
    /// `Some(n)` trips the flow after `n` consecutive failures (skips dispatch
    /// until [`Scheduler::reset_flow`](super::Scheduler::reset_flow)). `None`
    /// retries forever.
    pub streak_limit: Option<u32>,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            multiplier: 2.0,
            max_delay: Duration::from_secs(300),
            jitter: 0.1,
            streak_limit: None,
        }
    }
}

impl BackoffPolicy {
    /// Build a policy from [`Default`] but with `streak_limit: Some(n)` so
    /// the flow trips after `n` consecutive failures. Equivalent to writing
    /// `BackoffPolicy { streak_limit: Some(n), ..Default::default() }`.
    ///
    /// `with_trip(0)` is accepted and trips on the first failure (since
    /// [`is_tripped`](Self::is_tripped) checks `streak >= limit` and the
    /// post-failure streak is at least `1`). Use it as a "trip immediately"
    /// policy when you want backoff observability without retrying.
    pub fn with_trip(n: u32) -> Self {
        Self {
            streak_limit: Some(n),
            ..Self::default()
        }
    }

    /// Compute the delay to apply after `streak` consecutive failures.
    ///
    /// `streak` is the failure count, indexed from 1 — i.e. `streak=1` after
    /// the first failure returns `initial`, `streak=2` returns
    /// `initial * multiplier`, and so on. Returns at most `max_delay`. Jitter
    /// draws from [`rand::rng()`] when `jitter > 0`; otherwise the result is
    /// deterministic. `streak=0` is treated as `1` defensively for direct
    /// callers; the scheduler always passes a value `>= 1`.
    pub fn compute_delay(&self, streak: u32) -> Duration {
        let streak = streak.max(1);
        let exp = self.multiplier.powi((streak - 1) as i32);
        let base = self.initial.as_secs_f64() * exp;
        let cap = self.max_delay.as_secs_f64();
        let capped = base.min(cap);

        let jitter_factor = if self.jitter > 0.0 {
            let j = self.jitter.clamp(0.0, 1.0);
            let mut rng = rand::rng();
            let r: f64 = rng.random_range(-1.0..=1.0);
            1.0 + (j * r)
        } else {
            1.0
        };

        let delayed = (capped * jitter_factor).max(0.0);
        // Jitter can push us slightly above max_delay; clamp again.
        Duration::from_secs_f64(delayed).min(self.max_delay)
    }

    /// `true` when `failure_streak` has reached `streak_limit`.
    pub fn is_tripped(&self, failure_streak: u32) -> bool {
        matches!(self.streak_limit, Some(limit) if failure_streak >= limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_delay_no_jitter_is_exponential() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(60),
            jitter: 0.0,
            streak_limit: None,
        };
        assert_eq!(p.compute_delay(1), Duration::from_millis(100));
        assert_eq!(p.compute_delay(2), Duration::from_millis(200));
        assert_eq!(p.compute_delay(3), Duration::from_millis(400));
        assert_eq!(p.compute_delay(4), Duration::from_millis(800));
    }

    #[test]
    fn compute_delay_caps_at_max_delay() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(100),
            multiplier: 10.0,
            max_delay: Duration::from_millis(500),
            jitter: 0.0,
            streak_limit: None,
        };
        assert_eq!(p.compute_delay(5), Duration::from_millis(500));
        assert_eq!(p.compute_delay(20), Duration::from_millis(500));
    }

    #[test]
    fn compute_delay_with_jitter_stays_within_bounds() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(1000),
            multiplier: 1.0,
            max_delay: Duration::from_secs(60),
            jitter: 0.5,
            streak_limit: None,
        };
        // jitter 0.5 → delay in [500ms, 1500ms] (capped at max_delay otherwise)
        for _ in 0..50 {
            let d = p.compute_delay(1);
            assert!(d >= Duration::from_millis(500), "too small: {d:?}");
            assert!(d <= Duration::from_millis(1500), "too big: {d:?}");
        }
    }

    #[test]
    fn compute_delay_streak_zero_treated_as_one() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(10),
            jitter: 0.0,
            streak_limit: None,
        };
        assert_eq!(p.compute_delay(0), Duration::from_millis(100));
    }

    #[test]
    fn is_tripped_respects_limit() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(60),
            jitter: 0.0,
            streak_limit: Some(3),
        };
        assert!(!p.is_tripped(0));
        assert!(!p.is_tripped(2));
        assert!(p.is_tripped(3));
        assert!(p.is_tripped(99));
    }

    #[test]
    fn is_tripped_none_never_trips() {
        let p = BackoffPolicy {
            streak_limit: None,
            ..Default::default()
        };
        assert!(!p.is_tripped(0));
        assert!(!p.is_tripped(u32::MAX));
    }

    #[test]
    fn default_policy_values() {
        let p = BackoffPolicy::default();
        assert_eq!(p.initial, Duration::from_secs(1));
        assert!((p.multiplier - 2.0).abs() < f64::EPSILON);
        assert_eq!(p.max_delay, Duration::from_secs(300));
        assert!((p.jitter - 0.1).abs() < f64::EPSILON);
        assert_eq!(p.streak_limit, None);
    }

    #[test]
    fn with_trip_inherits_default_and_sets_limit() {
        let p = BackoffPolicy::with_trip(7);
        let d = BackoffPolicy::default();
        assert_eq!(p.initial, d.initial);
        assert_eq!(p.multiplier, d.multiplier);
        assert_eq!(p.max_delay, d.max_delay);
        assert_eq!(p.jitter, d.jitter);
        assert_eq!(p.streak_limit, Some(7));
    }
}
