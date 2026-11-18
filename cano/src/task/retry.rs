//! Retry policy and the retry-driving loop for task execution.
//!
//! **Availability:** since `0.1.4` (`attempt_timeout` since `0.10.3`) · **wasm-supported**
//!
//! [`RetryMode`] / [`TaskConfig`] describe *how* a task should be retried;
//! [`run_with_retries`] is the loop that applies that policy (and consults an
//! attached [`CircuitBreaker`]) around a single unit of work. The workflow and
//! scheduler dispatchers call [`run_with_retries`] with a closure that runs one
//! task attempt.

use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;

use crate::circuit::CircuitBreaker;
use crate::error::CanoError;
use crate::observer::WorkflowObserver;

#[cfg(feature = "tracing")]
use tracing::{Instrument, debug, error, info, info_span, instrument, warn};

/// Retry modes for task execution
///
/// **Availability:** since `0.1.4` · **wasm-supported**
///
/// Defines different retry strategies that can be used when task execution fails.
#[derive(Debug, Clone)]
pub enum RetryMode {
    /// No retries - fail immediately on first error
    None,

    /// Fixed number of retries with constant delay
    ///
    /// # Fields
    /// - `retries`: Number of retry attempts
    /// - `delay`: Fixed delay between attempts
    Fixed { retries: usize, delay: Duration },

    /// Exponential backoff with optional jitter
    ///
    /// Implements exponential backoff: delay = base_delay * multiplier^attempt + jitter
    ///
    /// # Fields
    /// - `max_retries`: Maximum number of retry attempts
    /// - `base_delay`: Initial delay duration
    /// - `multiplier`: Exponential multiplier (typically 2.0)
    /// - `max_delay`: Maximum delay cap to prevent excessive waits
    /// - `jitter`: Add randomness to prevent thundering herd (0.0 to 1.0)
    ExponentialBackoff {
        max_retries: usize,
        base_delay: Duration,
        multiplier: f64,
        max_delay: Duration,
        jitter: f64,
    },
}

impl RetryMode {
    /// Create a fixed retry mode with specified retries and delay
    pub fn fixed(retries: usize, delay: Duration) -> Self {
        Self::Fixed { retries, delay }
    }

    /// Create an exponential backoff retry mode with sensible defaults
    ///
    /// Uses base_delay=100ms, multiplier=2.0, max_delay=30s, jitter=0.1
    pub fn exponential(max_retries: usize) -> Self {
        Self::ExponentialBackoff {
            max_retries,
            base_delay: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(30),
            jitter: 0.1,
        }
    }

    /// Create a custom exponential backoff retry mode
    pub fn exponential_custom(
        max_retries: usize,
        base_delay: Duration,
        multiplier: f64,
        max_delay: Duration,
        jitter: f64,
    ) -> Self {
        Self::ExponentialBackoff {
            max_retries,
            base_delay,
            multiplier,
            max_delay,
            jitter: jitter.clamp(0.0, 1.0), // Ensure jitter is between 0 and 1
        }
    }

    /// Get the maximum number of attempts (initial + retries)
    ///
    /// Saturates at `usize::MAX` so pathological direct constructions (the
    /// fields are `pub`) can't overflow: `retries + 1` on `usize::MAX` would
    /// panic in debug and wrap to `0` in release — the wrapped value would
    /// make the first error fail the task with zero retries.
    pub fn max_attempts(&self) -> usize {
        match self {
            Self::None => 1,
            Self::Fixed { retries, .. } => retries.saturating_add(1),
            Self::ExponentialBackoff { max_retries, .. } => max_retries.saturating_add(1),
        }
    }

    /// Calculate delay for a specific attempt number (0-based)
    pub fn delay_for_attempt(&self, attempt: usize) -> Option<Duration> {
        match self {
            Self::None => None,
            Self::Fixed { retries, delay } => {
                if attempt < *retries {
                    Some(*delay)
                } else {
                    None
                }
            }
            Self::ExponentialBackoff {
                max_retries,
                base_delay,
                multiplier,
                max_delay,
                jitter,
            } => {
                if attempt < *max_retries {
                    // Sanitize against bad inputs. The struct fields are `pub`
                    // and the `ExponentialBackoff { .. }` variant can be
                    // constructed directly with non-finite or non-positive
                    // `multiplier`/`jitter`. Without this clamp the math chain
                    // ends in `NaN as u64 = 0` (a zero-delay retry storm) or
                    // similarly broken delays. A non-finite or non-positive
                    // multiplier collapses backoff to "no growth": use 1.0 so
                    // each attempt waits `base_delay`, capped by `max_delay`.
                    let mult = if multiplier.is_finite() && *multiplier > 0.0 {
                        *multiplier
                    } else {
                        1.0
                    };
                    let safe_jitter = if jitter.is_finite() {
                        jitter.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };

                    let base_ns = base_delay.as_nanos() as f64;
                    // Saturate the exponent before the i32 cast. Without this clamp
                    // an `attempt` of `i32::MAX as usize + 1` wraps to a large
                    // negative i32 and `mult.powi(NEG)` collapses to ~0, producing
                    // a zero-delay retry storm against the failing dependency on
                    // pathological `max_retries` values.
                    let exponent = attempt.min(i32::MAX as usize) as i32;
                    let exponential_ns = base_ns * mult.powi(exponent);
                    let capped_ns = exponential_ns.min(max_delay.as_nanos() as f64);

                    // Add jitter: delay * (1 ± jitter * random_factor)
                    let jitter_factor = if safe_jitter > 0.0 {
                        let mut rng = rand::rng();
                        let random_factor: f64 = rng.random_range(-1.0..=1.0);
                        1.0 + (safe_jitter * random_factor)
                    } else {
                        1.0
                    };

                    let final_ns_f = (capped_ns * jitter_factor).max(0.0);
                    // Convert nanoseconds back to `Duration`, saturating at `Duration::MAX`
                    // when enormous `max_delay + jitter` exceeds `u64::MAX` nanoseconds (~584y).
                    let final_dur = if final_ns_f >= u64::MAX as f64 {
                        Duration::try_from_secs_f64(final_ns_f / 1_000_000_000.0)
                            .unwrap_or(Duration::MAX)
                    } else {
                        Duration::from_nanos(final_ns_f.round() as u64)
                    };
                    // Floor to at least `min(1ms, base_delay, max_delay)` when there is a
                    // non-zero configured backoff so a jitter draw of `-1.0` with `jitter == 1.0`
                    // can't collapse the inter-attempt delay to `Duration::ZERO`. An explicit
                    // `base_delay == 0` still yields `Duration::ZERO`.
                    let final_dur = if !base_delay.is_zero() && !max_delay.is_zero() {
                        let floor = Duration::from_millis(1).min(*base_delay).min(*max_delay);
                        final_dur.max(floor)
                    } else {
                        final_dur
                    };
                    Some(final_dur)
                } else {
                    None
                }
            }
        }
    }
}

/// The default is [`RetryMode::None`]: a task runs once unless it opts into retries.
impl Default for RetryMode {
    fn default() -> Self {
        Self::None
    }
}

/// Task configuration for retry behavior and parameters
///
/// **Availability:** since `0.1.4` (`attempt_timeout` since `0.10.3`) · **wasm-supported**
///
/// This struct provides configuration for task execution behavior,
/// including retry logic and custom parameters.
#[must_use]
#[derive(Clone, Default)]
pub struct TaskConfig {
    /// Retry strategy for failed executions
    pub retry_mode: RetryMode,
    /// Optional per-attempt timeout. When set, each attempt inside
    /// [`run_with_retries`] is bounded by `attempt_timeout`; an
    /// expired attempt produces a [`CanoError::Timeout`] and the retry loop
    /// continues per `retry_mode`. `None` (the default) preserves the
    /// previous behavior of letting attempts run unbounded.
    pub attempt_timeout: Option<Duration>,
    /// Optional shared [`CircuitBreaker`] consulted before each attempt inside
    /// [`run_with_retries`]. When the breaker is open the call short-circuits
    /// with [`CanoError::CircuitOpen`] without running the task or burning a
    /// retry slot. Share one breaker across multiple tasks to make a trip from
    /// any caller protect every caller.
    pub circuit_breaker: Option<CircuitBreaker>,
}

impl TaskConfig {
    /// A configuration with one attempt, no retries, no timeout and no circuit breaker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the retry mode for this configuration
    pub fn with_retry(mut self, retry_mode: RetryMode) -> Self {
        self.retry_mode = retry_mode;
        self
    }

    /// Convenience method for fixed retry configuration
    pub fn with_fixed_retry(self, retries: usize, delay: Duration) -> Self {
        self.with_retry(RetryMode::fixed(retries, delay))
    }

    /// Convenience method for exponential backoff retry configuration
    pub fn with_exponential_retry(self, max_retries: usize) -> Self {
        self.with_retry(RetryMode::exponential(max_retries))
    }

    /// Apply a per-attempt timeout. Each retry attempt gets a fresh deadline.
    ///
    /// # Cancellation safety
    ///
    /// When the timeout fires, the in-flight task body is **dropped at its
    /// next await point** — i.e. `tokio::time::timeout` cancels it. Any
    /// non-RAII external state the task started (an open DB transaction
    /// without a `Drop` guard, an HTTP POST that already hit the wire, a
    /// partially-written file) is left as-is. If the retry policy schedules
    /// another attempt, that attempt may collide with the orphaned state.
    ///
    /// Tasks whose bodies are not cancellation-safe should either:
    /// 1. Wrap external resources in a type whose `Drop` releases them
    ///    cleanly, or
    /// 2. Set `attempt_timeout = None` and rely on the task itself (or the
    ///    workflow's [`with_total_timeout`](crate::workflow::Workflow::with_total_timeout))
    ///    to bound runtime via a cooperative mechanism (e.g. a select! on a
    ///    cancellation token the task body polls between operations).
    pub fn with_attempt_timeout(mut self, timeout: Duration) -> Self {
        self.attempt_timeout = Some(timeout);
        self
    }

    /// Attach a shared [`CircuitBreaker`] to this configuration.
    ///
    /// This is the recommended way to integrate a breaker — the retry loop calls
    /// [`CircuitBreaker::try_acquire`] **before** every attempt and records the
    /// outcome **after** it returns, so the breaker observes the call's
    /// success/failure before the workflow propagates it.
    ///
    /// Pass clones of the same breaker (cheap: an `Arc` inside) to every task that depends on the same
    /// flaky resource so the breaker protects them collectively.
    ///
    /// # Short-circuit behavior
    ///
    /// An `Open` breaker short-circuits the call with [`CanoError::CircuitOpen`] —
    /// no retries are consumed, the task body is not invoked. A `HalfOpen` breaker
    /// whose trial slots are all occupied short-circuits with
    /// [`CanoError::CircuitHalfOpenBusy`] instead, so callers can tell "still
    /// cooling down" apart from "probing the dependency right now". Either way the
    /// loop returns immediately without retrying. A breaker tripped **mid-loop**
    /// ends the retry loop immediately even when remaining retry attempts could
    /// outlast the breaker's `reset_timeout`; recovery requires a fresh
    /// `run_with_retries` call after the cool-down. This is intentional: retries
    /// against an open (or fully busy) breaker would only add load to a dependency
    /// the breaker is already protecting.
    pub fn with_circuit_breaker(mut self, breaker: CircuitBreaker) -> Self {
        self.circuit_breaker = Some(breaker);
        self
    }
}

/// The workflow's observers plus the name of the task being retried, notified of
/// `on_retry` / `on_circuit_open` from inside [`run_with_retries_observed`].
#[derive(Clone, Copy)]
pub(crate) struct ObserverCtx<'a> {
    pub(crate) observers: &'a [Arc<dyn WorkflowObserver>],
    pub(crate) task_name: &'a str,
}

impl<'a> ObserverCtx<'a> {
    /// `None` when there is nobody to notify, so the retry loop skips the fan-out.
    #[inline]
    pub(crate) fn new(
        observers: &'a [Arc<dyn WorkflowObserver>],
        task_name: &'a str,
    ) -> Option<Self> {
        (!observers.is_empty()).then_some(Self {
            observers,
            task_name,
        })
    }
}

/// Who hears about retries and breaker rejections inside the retry loop.
///
/// Generic rather than an `Option` argument so the public [`run_with_retries`]
/// passes `()`, which compiles away: the retry future carries no observer state.
pub(crate) trait RetryHooks {
    /// A failed attempt is about to be retried; `failed_attempts` counts it.
    fn on_retry(&self, failed_attempts: u32);
    /// The circuit breaker refused the call.
    fn on_circuit_open(&self);
}

impl RetryHooks for () {
    #[inline]
    fn on_retry(&self, _failed_attempts: u32) {}
    #[inline]
    fn on_circuit_open(&self) {}
}

/// Fans out through the workflow's `notify_observers`, so an observer panic is
/// isolated like everywhere else.
impl RetryHooks for Option<&ObserverCtx<'_>> {
    fn on_retry(&self, failed_attempts: u32) {
        if let Some(ctx) = self {
            crate::workflow::notify_observers(ctx.observers, |o| {
                o.on_retry(ctx.task_name, failed_attempts)
            });
        }
    }
    fn on_circuit_open(&self) {
        if let Some(ctx) = self {
            crate::workflow::notify_observers(ctx.observers, |o| o.on_circuit_open(ctx.task_name));
        }
    }
}

/// One step of the shared retry loop ([`run_retry_loop`]).
///
/// The loop itself is deliberately dumb — it only counts attempts, sleeps the
/// configured backoff between retryable failures, and stops when told to. Each
/// caller decides what *its* errors mean: the batch task helper treats every
/// failure as retryable, while `run_with_retries` uses [`RetryStep::Rejected`] for
/// circuit-breaker short-circuits (retrying an open breaker would only add load)
/// and [`RetryStep::Done`] for the final exhausted attempt (wrapped in `RetryExhausted`).
pub(crate) enum RetryStep<T> {
    /// Stop the loop and return this outcome verbatim.
    Done(Result<T, CanoError>),
    /// The attempt was refused before it ran (an open circuit breaker): report it
    /// through the loop's `on_rejected` hook and return this error.
    Rejected(CanoError),
    /// The attempt failed retryably — back off (per `RetryMode`) and run again.
    Retry(CanoError),
}

/// Drive a retry loop shared by `run_with_retries` and the batch task's per-item
/// retry helper.
///
/// Runs `run_attempt(attempt)` with the 0-based attempt index until it returns
/// [`RetryStep::Done`], or `max_attempts` retryable failures have accumulated
/// (the last failure's error is returned). Between retryable failures the loop
/// sleeps for `retry_mode.delay_for_attempt(failed_attempt_index)`, skipping
/// zero delays so a configured-but-zero backoff (possibly collapsed by jitter)
/// doesn't re-register a timer for nothing.
///
/// `on_retry(failed_attempts)` fires before each backoff and `on_rejected` on a
/// [`RetryStep::Rejected`]; they run here, outside the attempt future, so a caller
/// with no one to notify pays nothing per attempt.
pub(crate) async fn run_retry_loop<T, F, Fut>(
    retry_mode: &RetryMode,
    mut run_attempt: F,
    on_retry: impl Fn(u32),
    on_rejected: impl Fn(),
) -> Result<T, CanoError>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = RetryStep<T>>,
{
    let max_attempts = retry_mode.max_attempts();
    let mut attempt = 0usize;
    loop {
        match run_attempt(attempt).await {
            RetryStep::Done(outcome) => return outcome,
            RetryStep::Rejected(e) => {
                on_rejected();
                return Err(e);
            }
            RetryStep::Retry(e) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(e);
                }
                on_retry(u32::try_from(attempt).unwrap_or(u32::MAX));
                // `delay_for_attempt` takes the 0-based index of the attempt
                // that just failed — `attempt - 1` after the increment above.
                if let Some(delay) = retry_mode.delay_for_attempt(attempt - 1)
                    && !delay.is_zero()
                {
                    crate::rt::sleep(delay).await;
                }
            }
        }
    }
}

/// Run `run_fn` under `config`'s retry mode, attempt timeout and circuit breaker.
///
/// Use this inside custom task bodies; the workflow engine applies it to every
/// registered task already.
// A plain `fn` returning the observed future (not an `async fn` awaiting it), so the
// public entry point adds no extra state-machine layer.
#[inline]
pub fn run_with_retries<TState, F, Fut>(
    config: &TaskConfig,
    run_fn: F,
) -> impl std::future::Future<Output = Result<TState, CanoError>>
where
    TState: Send + Sync,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<TState, CanoError>>,
{
    run_with_retries_observed(config, (), run_fn)
}

/// [`run_with_retries`] that also reports `on_retry` / `on_circuit_open` to `obs`.
#[cfg_attr(feature = "tracing", instrument(
    name = "run_with_retries",
    skip(config, obs, run_fn),
    fields(max_attempts = config.retry_mode.max_attempts())
))]
pub(crate) async fn run_with_retries_observed<TState, F, Fut>(
    config: &TaskConfig,
    obs: impl RetryHooks,
    run_fn: F,
) -> Result<TState, CanoError>
where
    TState: Send + Sync,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<TState, CanoError>>,
{
    let max_attempts = config.retry_mode.max_attempts();
    // Hoisted: the breaker reference is immutable for the lifetime of this call,
    // so re-reading the `Option` every iteration is wasted work on the hot path.
    let breaker = config.circuit_breaker.as_ref();

    #[cfg(feature = "tracing")]
    info!(max_attempts, "Starting task execution with retry logic");

    // The attempt/backoff accounting lives in `run_retry_loop`; this closure
    // supplies the per-attempt work and decides, per failure, whether the loop
    // may retry (`RetryStep::Retry`) or must stop (`RetryStep::Done`).
    run_retry_loop(
        &config.retry_mode,
        |attempt| {
            // `F: Fn` can be invoked through a shared reference, so reborrow instead
            // of moving the non-Copy `run_fn` into the per-attempt future (an `FnMut`
            // closure cannot move non-Copy captures out). `breaker` is a Copy
            // `Option<&Arc<_>>`, captured directly by the async block.
            let run_fn = &run_fn;
            let attempt_future = async move {
                #[cfg(feature = "tracing")]
                debug!(attempt = attempt + 1, "Executing task attempt");

                // An open breaker short-circuits the entire retry loop: we do not invoke
                // the task and we do not consume a retry attempt — immediate retries
                // would only add load to a dependency the breaker is already protecting.
                let permit = match breaker {
                    Some(b) => match b.try_acquire() {
                        Ok(p) => Some(p),
                        Err(e) => {
                            #[cfg(feature = "tracing")]
                            warn!(error = %e, "Circuit breaker open; short-circuiting task");
                            #[cfg(feature = "metrics")]
                            crate::metrics::circuit_rejection();
                            return RetryStep::Rejected(e);
                        }
                    },
                    None => None,
                };

                let attempt_outcome = match config.attempt_timeout {
                    Some(d) => match crate::rt::timeout(d, run_fn()).await {
                        Ok(inner) => inner,
                        Err(_) => Err(CanoError::timeout("task attempt exceeded attempt_timeout")),
                    },
                    None => run_fn().await,
                };

                if let (Some(b), Some(p)) = (breaker, permit) {
                    match &attempt_outcome {
                        Ok(_) => b.record_success(p),
                        Err(_) => b.record_failure(p),
                    }
                }

                #[cfg(feature = "metrics")]
                crate::metrics::task_attempt(attempt_outcome.is_ok());

                match attempt_outcome {
                    Ok(result) => {
                        #[cfg(feature = "tracing")]
                        info!(attempt = attempt + 1, "Task execution successful");
                        RetryStep::Done(Ok(result))
                    }
                    Err(e) => {
                        // `attempt` is 0-based, so the 1-based count of attempts made
                        // so far (including this failure) is `attempt + 1`.
                        let attempts_made = attempt + 1;

                        #[cfg(feature = "tracing")]
                        if attempts_made >= max_attempts {
                            error!(
                                error = %e,
                                final_attempt = attempts_made,
                                max_attempts,
                                "Task execution failed after all retry attempts"
                            );
                        } else {
                            warn!(
                                error = %e,
                                attempt = attempts_made,
                                max_attempts,
                                "Task execution failed, will retry"
                            );
                        }

                        if attempts_made >= max_attempts {
                            return RetryStep::Done(Err(if max_attempts <= 1 {
                                // No retries configured: surface the raw error.
                                e
                            } else {
                                CanoError::retry_exhausted(
                                    u32::try_from(attempts_made).unwrap_or(u32::MAX),
                                    e,
                                )
                            }));
                        }
                        #[cfg(feature = "tracing")]
                        if let Some(delay) = config.retry_mode.delay_for_attempt(attempt) {
                            debug!(delay_ms = delay.as_millis(), "Waiting before retry");
                        }
                        // Guard zero delays (a legitimately configured zero base delay,
                        // possibly collapsed by jitter) so the loop doesn't re-register a
                        // timer for nothing — `run_retry_loop` applies that guard.
                        RetryStep::Retry(e)
                    }
                }
            };
            // Attach with `Instrument`, never `Span::enter()`: a guard held across `.await` stays
            // entered on this thread while other tasks run here (or exits on the wrong thread after
            // a migration), corrupting every trace on the runtime.
            #[cfg(feature = "tracing")]
            let attempt_future = attempt_future.instrument(info_span!(
                "task_attempt",
                attempt = attempt + 1,
                max_attempts
            ));
            attempt_future
        },
        |failed_attempts| obs.on_retry(failed_attempts),
        || obs.on_circuit_open(),
    )
    .await
}

#[cfg(all(test, feature = "runtime-tokio"))]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Tests for RetryMode and TaskConfig
    #[test]
    fn test_retry_mode_none() {
        let retry_mode = RetryMode::None;

        assert_eq!(retry_mode.max_attempts(), 1);
        assert_eq!(retry_mode.delay_for_attempt(0), None);
        assert_eq!(retry_mode.delay_for_attempt(1), None);
    }

    #[test]
    fn test_retry_mode_fixed() {
        let retry_mode = RetryMode::fixed(3, Duration::from_millis(100));

        assert_eq!(retry_mode.max_attempts(), 4); // 1 initial + 3 retries

        // Test delay calculations
        assert_eq!(
            retry_mode.delay_for_attempt(0),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(1),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(2),
            Some(Duration::from_millis(100))
        );
        assert_eq!(retry_mode.delay_for_attempt(3), None); // No more retries
        assert_eq!(retry_mode.delay_for_attempt(4), None);
    }

    #[test]
    fn test_retry_mode_exponential_basic() {
        let retry_mode = RetryMode::exponential(3);

        assert_eq!(retry_mode.max_attempts(), 4); // 1 initial + 3 retries

        // Test that delays increase (exact values may vary due to jitter)
        let delay0 = retry_mode.delay_for_attempt(0).unwrap();
        let delay1 = retry_mode.delay_for_attempt(1).unwrap();
        let delay2 = retry_mode.delay_for_attempt(2).unwrap();

        // With exponential backoff, each delay should generally be larger
        // (allowing for some jitter variance)
        assert!(delay1.as_millis() >= delay0.as_millis() / 2); // Account for negative jitter
        assert!(delay2.as_millis() >= delay1.as_millis() / 2);

        // No delay for attempts beyond max_retries
        assert_eq!(retry_mode.delay_for_attempt(3), None);
        assert_eq!(retry_mode.delay_for_attempt(4), None);
    }

    #[test]
    fn test_retry_mode_exponential_custom() {
        let retry_mode = RetryMode::exponential_custom(
            2,                         // max_retries
            Duration::from_millis(50), // base_delay
            3.0,                       // multiplier
            Duration::from_secs(5),    // max_delay
            0.0,                       // no jitter
        );

        assert_eq!(retry_mode.max_attempts(), 3);

        // With no jitter, delays should be predictable
        // attempt 0: 50ms * 3^0 = 50ms
        // attempt 1: 50ms * 3^1 = 150ms
        // attempt 2: None (beyond max_retries)
        assert_eq!(
            retry_mode.delay_for_attempt(0),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            retry_mode.delay_for_attempt(1),
            Some(Duration::from_millis(150))
        );
        assert_eq!(retry_mode.delay_for_attempt(2), None);
    }

    #[test]
    fn test_retry_mode_exponential_max_delay_cap() {
        let retry_mode = RetryMode::exponential_custom(
            5,
            Duration::from_millis(100), // base_delay
            10.0,                       // high multiplier
            Duration::from_millis(500), // low max_delay cap
            0.0,                        // no jitter
        );

        // All delays should be capped at max_delay
        let delay0 = retry_mode.delay_for_attempt(0).unwrap();
        let delay1 = retry_mode.delay_for_attempt(1).unwrap();
        let delay2 = retry_mode.delay_for_attempt(2).unwrap();

        assert_eq!(delay0, Duration::from_millis(100)); // 100 * 10^0 = 100
        assert_eq!(delay1, Duration::from_millis(500)); // 100 * 10^1 = 1000, capped to 500
        assert_eq!(delay2, Duration::from_millis(500)); // Capped to 500
    }

    #[test]
    fn test_retry_mode_exponential_jitter_bounds() {
        let retry_mode = RetryMode::exponential_custom(
            3,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            0.5, // 50% jitter
        );

        // Run multiple times to test jitter variability
        let mut delays = Vec::new();
        for _ in 0..20 {
            if let Some(delay) = retry_mode.delay_for_attempt(0) {
                delays.push(delay.as_millis());
            }
        }

        // With 50% jitter, delays should vary between 50ms and 150ms (100ms ± 50%)
        // Due to randomness, we'll check that we get some variation
        let min_delay = delays.iter().min().unwrap();
        let max_delay = delays.iter().max().unwrap();

        // Should have some variation due to jitter
        assert!(*min_delay >= 50); // 100ms - 50% = 50ms minimum
        assert!(*max_delay <= 150); // 100ms + 50% = 150ms maximum
    }

    #[test]
    fn test_retry_mode_exponential_sanitizes_nan_multiplier() {
        // The `ExponentialBackoff` variant's fields are `pub`, so a user can
        // construct it directly with a non-finite multiplier — bypassing the
        // `exponential_custom` builder. `delay_for_attempt` must defend itself
        // by treating any non-finite multiplier or jitter as a 1.0 / 0.0
        // collapse so each attempt waits at most `base_delay`, capped at
        // `max_delay`. Otherwise `NaN.powi(_) = NaN` chains through to
        // `NaN as u64 = 0`, producing a zero-delay retry storm against the
        // failing dependency.
        let bad = RetryMode::ExponentialBackoff {
            max_retries: 5,
            base_delay: Duration::from_millis(100),
            multiplier: f64::NAN,
            max_delay: Duration::from_secs(30),
            jitter: f64::NAN,
        };
        // Attempt 0 — base_delay * 1.0 = 100ms (multiplier collapsed to 1.0,
        // jitter collapsed to 0.0 since NaN).
        let d0 = bad.delay_for_attempt(0).expect("attempt within budget");
        assert_eq!(
            d0,
            Duration::from_millis(100),
            "NaN multiplier must NOT collapse to a 0-delay retry storm"
        );
        let d3 = bad.delay_for_attempt(3).expect("attempt within budget");
        assert_eq!(
            d3,
            Duration::from_millis(100),
            "every attempt with NaN multiplier should stay at base_delay"
        );
    }

    #[test]
    fn test_retry_mode_exponential_clamps_attempt_cast() {
        // `attempt as i32` wraps for usize values above i32::MAX. With a
        // multiplier > 1.0, the wrapped negative exponent makes `powi` collapse
        // toward zero, and the resulting delay would round down to 0 — a
        // zero-delay retry storm. The clamp keeps the schedule capped at
        // `max_delay` regardless of how huge `attempt` is.
        let mode = RetryMode::ExponentialBackoff {
            max_retries: usize::MAX,
            base_delay: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(30),
            jitter: 0.0,
        };
        let huge_attempt = (i32::MAX as usize) + 1;
        let d = mode.delay_for_attempt(huge_attempt).expect("within budget");
        // With multiplier=2.0 and any reasonable exponent (clamped to i32::MAX),
        // the exponential overflows immediately and is capped at max_delay.
        assert_eq!(
            d,
            Duration::from_secs(30),
            "huge attempt counter must be clamped, not wrapped to a zero-delay storm"
        );
    }

    #[test]
    fn test_retry_mode_exponential_sanitizes_nonpositive_multiplier() {
        // Direct construction with multiplier=0.0 or negative would also
        // produce nonsensical schedules (0 * x = 0; negative powi oscillates).
        for bad_mult in [0.0_f64, -1.0_f64, f64::NEG_INFINITY] {
            let mode = RetryMode::ExponentialBackoff {
                max_retries: 3,
                base_delay: Duration::from_millis(100),
                multiplier: bad_mult,
                max_delay: Duration::from_secs(30),
                jitter: 0.0,
            };
            let d = mode.delay_for_attempt(2).expect("within budget");
            assert_eq!(
                d,
                Duration::from_millis(100),
                "multiplier {bad_mult} must collapse to base_delay, not zero"
            );
        }
    }

    #[test]
    fn test_retry_mode_jitter_clamping() {
        // Test that jitter values outside [0, 1] are clamped
        let retry_mode1 = RetryMode::exponential_custom(
            1,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            -0.5, // Should be clamped to 0.0
        );

        let retry_mode2 = RetryMode::exponential_custom(
            1,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(30),
            1.5, // Should be clamped to 1.0
        );

        // Both should work without panicking
        assert!(retry_mode1.delay_for_attempt(0).is_some());
        assert!(retry_mode2.delay_for_attempt(0).is_some());
    }

    #[test]
    fn test_retry_mode_full_jitter_is_floored_above_zero() {
        // Regression: with `jitter == 1.0`, `random_range(-1.0..=1.0)` admits the
        // legal endpoint `-1.0`, which makes `jitter_factor = 1.0 + 1.0 * -1.0 = 0.0`.
        // Before flooring, `final_delay = (capped_delay * 0.0) as u64 = 0`, so the
        // retry sleep collapsed to `Duration::ZERO` and the loop bursted against
        // the failing dependency. Sampling thousands of draws, we must never see
        // a returned `Duration::ZERO` when `base_delay > 0`.
        let mode = RetryMode::ExponentialBackoff {
            max_retries: 1,
            base_delay: Duration::from_millis(100),
            multiplier: 1.0,
            max_delay: Duration::from_secs(30),
            jitter: 1.0,
        };
        // 4096 samples should hit unlucky low draws comfortably.
        for _ in 0..4096 {
            let d = mode
                .delay_for_attempt(0)
                .expect("attempt 0 within max_retries");
            assert!(
                d >= Duration::from_millis(1),
                "100% jitter must NOT collapse delay to zero; got {d:?}"
            );
        }
    }

    #[test]
    fn test_retry_mode_zero_base_delay_stays_zero() {
        // Counter-test: when the user explicitly chooses zero-delay retries
        // (`base_delay = 0`), the floor must NOT inflate them to 1ms. Zero-delay
        // is the user's documented choice; the floor only kicks in when the
        // schedule would have been non-zero.
        let mode = RetryMode::ExponentialBackoff {
            max_retries: 1,
            base_delay: Duration::ZERO,
            multiplier: 2.0,
            max_delay: Duration::from_secs(30),
            jitter: 1.0,
        };
        let d = mode.delay_for_attempt(0).expect("within budget");
        assert_eq!(
            d,
            Duration::ZERO,
            "explicit base_delay=0 should still produce a zero delay"
        );
    }

    #[test]
    fn default_config_runs_once() {
        assert_eq!(RetryMode::default().max_attempts(), 1);
        assert!(RetryMode::default().delay_for_attempt(0).is_none());
        assert_eq!(TaskConfig::new().retry_mode.max_attempts(), 1);
        assert_eq!(TaskConfig::default().retry_mode.max_attempts(), 1);
    }

    #[test]
    fn test_task_config_with_fixed_retry() {
        let config = TaskConfig::new().with_fixed_retry(5, Duration::from_millis(100));

        assert_eq!(config.retry_mode.max_attempts(), 6);
    }

    #[test]
    fn test_task_config_builder_pattern() {
        let config = TaskConfig::new().with_fixed_retry(10, Duration::from_secs(1));

        assert_eq!(config.retry_mode.max_attempts(), 11);
    }

    #[tokio::test]
    async fn test_run_with_retries_success() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<String, CanoError>("success".to_string())
            }
        })
        .await
        .unwrap();

        assert_eq!(result, "success".to_string());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_with_retries_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    Err(CanoError::task_execution("failure"))
                } else {
                    Ok::<String, CanoError>("success".to_string())
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(result, "success".to_string());
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_run_with_retries_exhausted() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<String, CanoError>(CanoError::task_execution("always fails"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_run_with_retries_mode_none() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<String, CanoError>(CanoError::task_execution("immediate fail"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::TaskExecution(_)),
            "expected original TaskExecution variant when retries disabled, got: {err}"
        );
        assert!(err.to_string().contains("immediate fail"));
    }

    #[tokio::test]
    async fn test_attempt_timeout_triggers_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new()
            .with_fixed_retry(2, Duration::from_millis(1))
            .with_attempt_timeout(Duration::from_millis(20));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok::<String, CanoError>("never".to_string())
            }
        })
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::RetryExhausted { .. }),
            "expected RetryExhausted, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("Timeout error") || msg.contains("attempt_timeout"),
            "expected timeout context in error, got: {msg}"
        );
        // 1 initial + 2 retries — every attempt times out before the sleep returns.
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_attempt_timeout_none_unchanged() {
        let config = TaskConfig::new();
        assert!(config.attempt_timeout.is_none());

        let result = run_with_retries::<String, _, _>(&config, || async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok::<String, CanoError>("ok".to_string())
        })
        .await
        .unwrap();
        assert_eq!(result, "ok".to_string());
    }

    #[tokio::test]
    async fn test_attempt_timeout_resets_per_attempt() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // First attempt sleeps long enough to trip the timeout; second attempt
        // returns immediately. Verifies each attempt gets a fresh deadline.
        let config = TaskConfig::new()
            .with_fixed_retry(1, Duration::from_millis(1))
            .with_attempt_timeout(Duration::from_millis(30));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok::<String, CanoError>("ok".to_string())
            }
        })
        .await
        .unwrap();
        assert_eq!(result, "ok".to_string());
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_retry_exhausted_error_type() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<String, CanoError>(CanoError::task_execution("persistent failure"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::RetryExhausted { .. }),
            "expected RetryExhausted after retry exhaustion, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // Circuit breaker integration tests
    // ------------------------------------------------------------------

    use crate::circuit::{CircuitBreaker, CircuitPolicy, CircuitState};

    fn cb_policy(threshold: u32) -> CircuitPolicy {
        CircuitPolicy {
            failure_threshold: threshold,
            reset_timeout: Duration::from_millis(20),
            half_open_max_calls: 1,
        }
    }

    /// Same thresholds as [`cb_policy`], but with a `reset_timeout` long enough that a
    /// tripped breaker cannot lapse back into `HalfOpen` while the test is running.
    ///
    /// Tests that assert *short-circuit* behaviour need the `Open` window to outlive the
    /// whole test body. With the 20ms default a loaded machine can drift past the
    /// deadline between two attempts (the fixed retry sleep is only 1ms), which lazily
    /// promotes the breaker to `HalfOpen`, admits a probe call, and inflates the
    /// invocation count. `test_circuit_half_open_recovery_via_run_with_retries` is the
    /// only test that actually exercises the reset deadline, so it keeps `cb_policy`.
    fn cb_policy_stable_open(threshold: u32) -> CircuitPolicy {
        CircuitPolicy {
            reset_timeout: Duration::from_secs(60),
            ..cb_policy(threshold)
        }
    }

    #[tokio::test]
    async fn test_circuit_open_short_circuits_without_invoking_task() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Threshold = 3, no retries, so the first three failing calls trip the breaker
        // and the fourth call must return CircuitOpen *without* invoking the task body.
        let breaker = CircuitBreaker::new(cb_policy_stable_open(3));
        let config = TaskConfig::new().with_circuit_breaker(breaker.clone());
        let counter = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let counter = Arc::clone(&counter);
            let result = run_with_retries::<String, _, _>(&config, || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<String, CanoError>(CanoError::task_execution("boom"))
                }
            })
            .await;
            assert!(result.is_err());
        }
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // Fourth call: short-circuited.
        let counter_before = counter.load(Ordering::SeqCst);
        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<String, CanoError>("never".into())
            }
        })
        .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::CircuitOpen(_)),
            "expected CircuitOpen, got: {err}"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            counter_before,
            "task body must not run when breaker is open"
        );
    }

    #[tokio::test]
    async fn test_circuit_open_does_not_consume_retry_attempts() {
        // Even with retries enabled, a CircuitOpen must short-circuit immediately —
        // retries against an open breaker would only add load to the failing
        // dependency. The error should surface raw, not wrapped in RetryExhausted.
        let breaker = CircuitBreaker::new(cb_policy_stable_open(1));
        let config = TaskConfig::new()
            .with_fixed_retry(5, Duration::from_millis(1))
            .with_circuit_breaker(breaker.clone());

        // First call: attempt 1 invokes the closure, fails, and trips the breaker
        // (threshold = 1 reached after a single recorded failure). On the next
        // loop iteration `try_acquire` sees the open breaker and the retry loop
        // returns immediately with raw CircuitOpen — the remaining 4 retry slots
        // are *not* consumed. We discard the outcome here and verify the
        // short-circuit explicitly with the second call below.
        let _ = run_with_retries::<String, _, _>(&config, || async {
            Err::<String, CanoError>(CanoError::task_execution("boom"))
        })
        .await;
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // Second call: breaker rejects on the very first attempt — surface CircuitOpen
        // raw, not wrapped in RetryExhausted.
        let result = run_with_retries::<String, _, _>(&config, || async {
            Ok::<String, CanoError>("never".into())
        })
        .await;
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::CircuitOpen(_)),
            "expected raw CircuitOpen, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_circuit_half_open_recovery_via_run_with_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let breaker = CircuitBreaker::new(cb_policy(2));
        let config = TaskConfig::new().with_circuit_breaker(breaker.clone());

        // Trip the breaker.
        for _ in 0..2 {
            let _ = run_with_retries::<String, _, _>(&config, || async {
                Err::<String, CanoError>(CanoError::task_execution("boom"))
            })
            .await;
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // Wait for reset_timeout, then a successful call should close the breaker
        // (half_open_max_calls = 1).
        tokio::time::sleep(Duration::from_millis(40)).await;
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<String, CanoError>("ok".into())
            }
        })
        .await
        .unwrap();
        assert_eq!(result, "ok".to_string());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_circuit_breaker_shared_across_tasks() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Two tasks share one CircuitBreaker; failures from task A trip it for B.
        let breaker = CircuitBreaker::new(cb_policy_stable_open(3));
        let config_a = TaskConfig::new().with_circuit_breaker(breaker.clone());
        let config_b = TaskConfig::new().with_circuit_breaker(breaker.clone());

        for _ in 0..3 {
            let _ = run_with_retries::<String, _, _>(&config_a, || async {
                Err::<String, CanoError>(CanoError::task_execution("A failed"))
            })
            .await;
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        let b_invocations = Arc::new(AtomicUsize::new(0));
        let b_invocations_clone = Arc::clone(&b_invocations);
        let result = run_with_retries::<String, _, _>(&config_b, || {
            let b = Arc::clone(&b_invocations_clone);
            async move {
                b.fetch_add(1, Ordering::SeqCst);
                Ok::<String, CanoError>("B ok".into())
            }
        })
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, CanoError::CircuitOpen(_)));
        assert_eq!(
            b_invocations.load(Ordering::SeqCst),
            0,
            "task B must not run while breaker tripped by task A is open"
        );
    }

    #[tokio::test]
    async fn test_circuit_open_mid_loop_with_fixed_retry_short_circuits_remaining_attempts() {
        // Threshold=2 + 5 retries: attempt 1 fails (Closed, counter=1), attempt 2 fails
        // (Closed→Open trip on counter=2), attempt 3's `try_acquire` returns
        // CircuitOpen and the loop returns immediately — so only 2 invocations land
        // even though 6 attempts (1 + 5 retries) were budgeted. The error must be
        // raw CircuitOpen, not RetryExhausted.
        use std::sync::atomic::{AtomicUsize, Ordering};

        let breaker = CircuitBreaker::new(cb_policy_stable_open(2));
        let config = TaskConfig::new()
            .with_fixed_retry(5, Duration::from_millis(1))
            .with_circuit_breaker(breaker.clone());
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<String, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<String, CanoError>(CanoError::task_execution("boom"))
            }
        })
        .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::CircuitOpen(_)),
            "expected raw CircuitOpen after mid-loop trip, got: {err}"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "task body must run exactly threshold times before the breaker trips short-circuits the rest of the retry budget"
        );
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));
    }

    #[tokio::test]
    async fn test_circuit_breaker_unused_does_not_change_behavior() {
        // Sanity: with no breaker configured the retry loop behaves exactly as before.
        let config = TaskConfig::new().with_fixed_retry(1, Duration::from_millis(1));
        assert!(config.circuit_breaker.is_none());

        let result = run_with_retries::<String, _, _>(&config, || async {
            Ok::<String, CanoError>("ok".into())
        })
        .await
        .unwrap();
        assert_eq!(result, "ok");
    }

    #[tokio::test]
    async fn test_attempt_timeout_counts_as_circuit_failure() {
        // A timed-out attempt produces CanoError::Timeout, which must flow through
        // the same `record_failure` arm as any other error so the breaker counts
        // it toward the threshold. With `failure_threshold=2` and `RetryMode::None`,
        // two timed-out calls should trip the breaker; the third call must surface
        // CircuitOpen without ever invoking the task body.
        use std::sync::atomic::{AtomicUsize, Ordering};

        let breaker = CircuitBreaker::new(cb_policy_stable_open(2));
        let config = TaskConfig::new()
            .with_attempt_timeout(Duration::from_millis(10))
            .with_circuit_breaker(breaker.clone());
        let invocations = Arc::new(AtomicUsize::new(0));

        for _ in 0..2 {
            let invocations = Arc::clone(&invocations);
            let result = run_with_retries::<String, _, _>(&config, || {
                let invocations = Arc::clone(&invocations);
                async move {
                    invocations.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok::<String, CanoError>("never".into())
                }
            })
            .await;
            // The attempt times out → Timeout error → record_failure on the breaker.
            assert!(matches!(result, Err(CanoError::Timeout(_))));
        }

        assert_eq!(invocations.load(Ordering::SeqCst), 2);
        assert!(
            matches!(breaker.state(), CircuitState::Open { .. }),
            "two timed-out attempts must trip the breaker, got {:?}",
            breaker.state()
        );

        // Third call: breaker open → short-circuit, body not invoked.
        let before = invocations.load(Ordering::SeqCst);
        let result = run_with_retries::<String, _, _>(&config, || {
            let invocations = Arc::clone(&invocations);
            async move {
                invocations.fetch_add(1, Ordering::SeqCst);
                Ok::<String, CanoError>("never".into())
            }
        })
        .await;
        assert!(matches!(result, Err(CanoError::CircuitOpen(_))));
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            before,
            "task body must not run when the breaker is open"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn sub_millisecond_fixed_and_exponential_retries_sleep_nonzero_delays() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fixed = RetryMode::Fixed {
            retries: 2,
            delay: Duration::from_micros(400),
        };
        let cfg = TaskConfig::new().with_retry(fixed);
        let t0 = tokio::time::Instant::now();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_ref = Arc::clone(&calls);
        let out = run_with_retries(&cfg, move || {
            let calls_ref = Arc::clone(&calls_ref);
            async move {
                let n = calls_ref.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err::<u8, _>(CanoError::task_execution("try again"))
                } else {
                    Ok(42)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(out, 42);
        assert!(
            t0.elapsed() >= Duration::from_micros(800),
            "sub-ms fixed retry must sleep on each retry, elapsed {:?}",
            t0.elapsed()
        );

        let exp = RetryMode::ExponentialBackoff {
            max_retries: 3,
            base_delay: Duration::from_micros(200),
            multiplier: 2.0,
            max_delay: Duration::from_micros(700),
            jitter: 0.0,
        };
        assert_eq!(exp.delay_for_attempt(0), Some(Duration::from_micros(200)));
        assert_eq!(exp.delay_for_attempt(1), Some(Duration::from_micros(400)));
        assert_eq!(exp.delay_for_attempt(2), Some(Duration::from_micros(700)));

        let max_cap = RetryMode::ExponentialBackoff {
            max_retries: usize::MAX,
            base_delay: Duration::from_secs(10),
            multiplier: 10.0,
            max_delay: Duration::MAX,
            jitter: 1.0,
        };
        assert!(max_cap.delay_for_attempt(usize::MAX - 1).is_some());
    }
}

#[cfg(all(test, feature = "metrics", feature = "runtime-tokio"))]
mod metrics_tests {
    use super::*;
    use crate::circuit::{CircuitBreaker, CircuitPolicy};
    use crate::error::CanoError;
    use crate::metrics::test_support::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn retry_loop_counts_attempts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let config = TaskConfig::new().with_fixed_retry(3, Duration::from_millis(1));
        let (res, rows) = run_with_recorder(move || async move {
            run_with_retries::<u8, _, _>(&config, move || {
                let n = calls2.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        Err(CanoError::task_execution("transient"))
                    } else {
                        Ok(7u8)
                    }
                }
            })
            .await
        });
        assert_eq!(res.unwrap(), 7);
        assert_eq!(
            counter(&rows, "cano_task_attempts_total", &[("outcome", "failed")]),
            2
        );
        assert_eq!(
            counter(
                &rows,
                "cano_task_attempts_total",
                &[("outcome", "completed")]
            ),
            1
        );
        assert_eq!(
            counter_opt(&rows, "cano_circuit_rejections_total", &[]),
            None
        );
    }

    #[test]
    fn open_breaker_records_a_circuit_rejection() {
        let breaker = CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 1,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        });
        let p = breaker.try_acquire().unwrap();
        breaker.record_failure(p);
        let config = TaskConfig::new().with_circuit_breaker(breaker.clone());
        let (res, rows) = run_with_recorder(move || async move {
            run_with_retries::<u8, _, _>(&config, || async { Ok(1u8) }).await
        });
        assert!(matches!(res, Err(CanoError::CircuitOpen(_))));
        assert_eq!(counter(&rows, "cano_circuit_rejections_total", &[]), 1);
        assert_eq!(
            counter_opt(
                &rows,
                "cano_task_attempts_total",
                &[("outcome", "completed")]
            ),
            None
        );
    }
}
