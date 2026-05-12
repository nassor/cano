//! Retry policy and the retry-driving loop for task execution.
//!
//! [`RetryMode`] / [`TaskConfig`] describe *how* a task should be retried;
//! [`run_with_retries`] is the loop that applies that policy (and consults an
//! attached [`CircuitBreaker`]) around a single unit of work. The workflow and
//! scheduler dispatchers call [`run_with_retries`] with a closure that runs one
//! task attempt.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;

use crate::circuit::CircuitBreaker;
use crate::error::CanoError;
use crate::observer::WorkflowObserver;

#[cfg(feature = "tracing")]
use tracing::{debug, error, info, info_span, instrument, warn};

/// Retry modes for task execution
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
    pub fn max_attempts(&self) -> usize {
        match self {
            Self::None => 1,
            Self::Fixed { retries, .. } => retries + 1,
            Self::ExponentialBackoff { max_retries, .. } => max_retries + 1,
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
                    let base_ms = base_delay.as_millis() as f64;
                    let exponential_delay = base_ms * multiplier.powi(attempt as i32);
                    let capped_delay = exponential_delay.min(max_delay.as_millis() as f64);

                    // Add jitter: delay * (1 ± jitter * random_factor)
                    let jitter_factor = if *jitter > 0.0 {
                        let mut rng = rand::rng();
                        let random_factor: f64 = rng.random_range(-1.0..=1.0);
                        1.0 + (jitter * random_factor)
                    } else {
                        1.0
                    };

                    let final_delay_f = (capped_delay * jitter_factor).max(0.0);
                    // Saturate rather than wrap or panic when the computed delay
                    // exceeds u64::MAX milliseconds (e.g. enormous max_delay + jitter).
                    let final_delay = if final_delay_f >= u64::MAX as f64 {
                        u64::MAX
                    } else {
                        final_delay_f as u64
                    };
                    Some(Duration::from_millis(final_delay))
                } else {
                    None
                }
            }
        }
    }
}

impl Default for RetryMode {
    fn default() -> Self {
        Self::ExponentialBackoff {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            multiplier: 2.0,
            max_delay: Duration::from_secs(30),
            jitter: 0.1,
        }
    }
}

/// Task configuration for retry behavior and parameters
///
/// This struct provides configuration for task execution behavior,
/// including retry logic and custom parameters.
#[must_use]
#[derive(Clone, Default)]
pub struct TaskConfig {
    /// Retry strategy for failed executions
    pub retry_mode: RetryMode,
    /// Optional per-attempt timeout. When set, each attempt inside
    /// [`run_with_retries`] is wrapped with [`tokio::time::timeout`]; an
    /// expired attempt produces a [`CanoError::Timeout`] and the retry loop
    /// continues per `retry_mode`. `None` (the default) preserves the
    /// previous behavior of letting attempts run unbounded.
    pub attempt_timeout: Option<Duration>,
    /// Optional shared [`CircuitBreaker`] consulted before each attempt inside
    /// [`run_with_retries`]. When the breaker is open the call short-circuits
    /// with [`CanoError::CircuitOpen`] without running the task or burning a
    /// retry slot. Share one breaker across multiple tasks to make a trip from
    /// any caller protect every caller.
    pub circuit_breaker: Option<Arc<CircuitBreaker>>,
    /// Observers notified of retry and circuit-open events from inside
    /// [`run_with_retries`]. The workflow engine stamps its registered observer
    /// list (and [`task_name`](Self::task_name)) onto a per-dispatch copy of the
    /// config when any observer is attached; left `None` there is zero overhead
    /// on the hot path. See [`WorkflowObserver`].
    pub observers: Option<Arc<[Arc<dyn WorkflowObserver>]>>,
    /// Human-readable task identifier reported to [`observers`](Self::observers).
    /// Populated by the workflow engine from [`Task::name`]; `None` when
    /// [`run_with_retries`] is invoked directly without observers wired.
    pub task_name: Option<Cow<'static, str>>,
}

impl TaskConfig {
    /// Create a new TaskConfig with default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a minimal configuration with no retries
    ///
    /// Useful for tasks that should fail fast without any retry attempts.
    pub fn minimal() -> Self {
        Self {
            retry_mode: RetryMode::None,
            attempt_timeout: None,
            circuit_breaker: None,
            observers: None,
            task_name: None,
        }
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
    /// Pass the same `Arc<CircuitBreaker>` to every task that depends on the same
    /// flaky resource so the breaker protects them collectively.
    ///
    /// # Short-circuit behavior
    ///
    /// An `Open` breaker short-circuits the call with [`CanoError::CircuitOpen`] —
    /// no retries are consumed, the task body is not invoked. A breaker tripped
    /// **mid-loop** ends the retry loop immediately even when remaining retry
    /// attempts could outlast the breaker's `reset_timeout`; recovery requires a
    /// fresh `run_with_retries` call after the cool-down. This is intentional:
    /// retries against an open breaker would only add load to a dependency the
    /// breaker is already protecting.
    pub fn with_circuit_breaker(mut self, breaker: Arc<CircuitBreaker>) -> Self {
        self.circuit_breaker = Some(breaker);
        self
    }
}

/// Invoke `f` for every observer attached to `config`, passing the resolved task name.
///
/// No-op (and no allocation) when `config.observers` is `None` — the common case
/// on the hot path when no observer is wired into the workflow.
#[inline]
fn notify_observers(config: &TaskConfig, f: impl Fn(&dyn WorkflowObserver, &str)) {
    if let Some(observers) = config.observers.as_deref() {
        let name = config.task_name.as_deref().unwrap_or("<task>");
        for observer in observers {
            f(observer.as_ref(), name);
        }
    }
}

/// Default implementation for retry logic that can be used by any task
///
/// This function provides a standard retry mechanism that can be used by any task
/// that implements a simple run function.
#[cfg_attr(feature = "tracing", instrument(
    skip(config, run_fn),
    fields(max_attempts = config.retry_mode.max_attempts())
))]
pub async fn run_with_retries<TState, F, Fut>(
    config: &TaskConfig,
    run_fn: F,
) -> Result<TState, CanoError>
where
    TState: Send + Sync,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<TState, CanoError>>,
{
    let max_attempts = config.retry_mode.max_attempts();
    let mut attempt = 0;
    // Hoisted: the breaker reference is immutable for the lifetime of this call,
    // so re-reading the `Option` every iteration is wasted work on the hot path.
    let breaker = config.circuit_breaker.as_ref();

    #[cfg(feature = "tracing")]
    info!(max_attempts, "Starting task execution with retry logic");

    loop {
        #[cfg(feature = "tracing")]
        let attempt_span = info_span!("task_attempt", attempt = attempt + 1, max_attempts);

        #[cfg(feature = "tracing")]
        let _span_guard = attempt_span.enter();

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
                    notify_observers(config, |o, name| o.on_circuit_open(name));
                    #[cfg(feature = "metrics")]
                    crate::metrics::circuit_rejection();
                    return Err(e);
                }
            },
            None => None,
        };

        let attempt_outcome = match config.attempt_timeout {
            Some(d) => match tokio::time::timeout(d, run_fn()).await {
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
                return Ok(result);
            }
            Err(e) => {
                attempt += 1;

                #[cfg(feature = "tracing")]
                if attempt >= max_attempts {
                    error!(
                        error = %e,
                        final_attempt = attempt,
                        max_attempts,
                        "Task execution failed after all retry attempts"
                    );
                } else {
                    warn!(
                        error = %e,
                        attempt,
                        max_attempts,
                        "Task execution failed, will retry"
                    );
                }

                if attempt >= max_attempts {
                    if max_attempts <= 1 {
                        return Err(e);
                    }
                    return Err(CanoError::retry_exhausted(format!(
                        "Task failed after {} attempt(s): {}",
                        attempt, e
                    )));
                } else {
                    notify_observers(config, |o, name| o.on_retry(name, attempt as u32));
                    if let Some(delay) = config.retry_mode.delay_for_attempt(attempt - 1) {
                        #[cfg(feature = "tracing")]
                        debug!(delay_ms = delay.as_millis(), "Waiting before retry");

                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskResult;
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
    fn test_retry_mode_default() {
        let retry_mode = RetryMode::default();

        // Default should be exponential backoff with 3 retries
        assert_eq!(retry_mode.max_attempts(), 4);

        // Should have delays for first 3 attempts
        assert!(retry_mode.delay_for_attempt(0).is_some());
        assert!(retry_mode.delay_for_attempt(1).is_some());
        assert!(retry_mode.delay_for_attempt(2).is_some());
        assert!(retry_mode.delay_for_attempt(3).is_none());
    }

    #[test]
    fn test_task_config_creation() {
        let config = TaskConfig::new();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_task_config_default() {
        let config = TaskConfig::default();
        assert_eq!(config.retry_mode.max_attempts(), 4);
    }

    #[test]
    fn test_task_config_minimal() {
        let config = TaskConfig::minimal();
        assert_eq!(config.retry_mode.max_attempts(), 1);
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

        let config = TaskConfig::minimal();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("success".to_string()))
            }
        })
        .await
        .unwrap();

        assert_eq!(result, TaskResult::Single("success".to_string()));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_run_with_retries_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    Err(CanoError::task_execution("failure"))
                } else {
                    Ok::<TaskResult<String>, CanoError>(TaskResult::Single("success".to_string()))
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(result, TaskResult::Single("success".to_string()));
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_run_with_retries_exhausted() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution("always fails"))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_run_with_retries_mode_none() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::minimal();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution("immediate fail"))
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

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("never".to_string()))
            }
        })
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::RetryExhausted(_)),
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

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok::<TaskResult<String>, CanoError>(TaskResult::Single("ok".to_string()))
        })
        .await
        .unwrap();
        assert_eq!(result, TaskResult::Single("ok".to_string()));
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

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("ok".to_string()))
            }
        })
        .await
        .unwrap();
        assert_eq!(result, TaskResult::Single("ok".to_string()));
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_retry_exhausted_error_type() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution(
                    "persistent failure",
                ))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        let err = result.unwrap_err();
        assert!(
            matches!(err, CanoError::RetryExhausted(_)),
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

    #[tokio::test]
    async fn test_circuit_open_short_circuits_without_invoking_task() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Threshold = 3, no retries, so the first three failing calls trip the breaker
        // and the fourth call must return CircuitOpen *without* invoking the task body.
        let breaker = Arc::new(CircuitBreaker::new(cb_policy(3)));
        let config = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));
        let counter = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let counter = Arc::clone(&counter);
            let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<TaskResult<String>, CanoError>(CanoError::task_execution("boom"))
                }
            })
            .await;
            assert!(result.is_err());
        }
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // Fourth call: short-circuited.
        let counter_before = counter.load(Ordering::SeqCst);
        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("never".into()))
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
        let breaker = Arc::new(CircuitBreaker::new(cb_policy(1)));
        let config = TaskConfig::new()
            .with_fixed_retry(5, Duration::from_millis(1))
            .with_circuit_breaker(Arc::clone(&breaker));

        // First call: attempt 1 invokes the closure, fails, and trips the breaker
        // (threshold = 1 reached after a single recorded failure). On the next
        // loop iteration `try_acquire` sees the open breaker and the retry loop
        // returns immediately with raw CircuitOpen — the remaining 4 retry slots
        // are *not* consumed. We discard the outcome here and verify the
        // short-circuit explicitly with the second call below.
        let _ = run_with_retries::<TaskResult<String>, _, _>(&config, || async {
            Err::<TaskResult<String>, CanoError>(CanoError::task_execution("boom"))
        })
        .await;
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // Second call: breaker rejects on the very first attempt — surface CircuitOpen
        // raw, not wrapped in RetryExhausted.
        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || async {
            Ok::<TaskResult<String>, CanoError>(TaskResult::Single("never".into()))
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

        let breaker = Arc::new(CircuitBreaker::new(cb_policy(2)));
        let config = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));

        // Trip the breaker.
        for _ in 0..2 {
            let _ = run_with_retries::<TaskResult<String>, _, _>(&config, || async {
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution("boom"))
            })
            .await;
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        // Wait for reset_timeout, then a successful call should close the breaker
        // (half_open_max_calls = 1).
        tokio::time::sleep(Duration::from_millis(40)).await;
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("ok".into()))
            }
        })
        .await
        .unwrap();
        assert_eq!(result, TaskResult::Single("ok".to_string()));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_circuit_breaker_shared_across_tasks() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Two tasks share one Arc<CircuitBreaker>; failures from task A trip it for B.
        let breaker = Arc::new(CircuitBreaker::new(cb_policy(3)));
        let config_a = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));
        let config_b = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));

        for _ in 0..3 {
            let _ = run_with_retries::<TaskResult<String>, _, _>(&config_a, || async {
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution("A failed"))
            })
            .await;
        }
        assert!(matches!(breaker.state(), CircuitState::Open { .. }));

        let b_invocations = Arc::new(AtomicUsize::new(0));
        let b_invocations_clone = Arc::clone(&b_invocations);
        let result = run_with_retries::<TaskResult<String>, _, _>(&config_b, || {
            let b = Arc::clone(&b_invocations_clone);
            async move {
                b.fetch_add(1, Ordering::SeqCst);
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("B ok".into()))
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

        let breaker = Arc::new(CircuitBreaker::new(cb_policy(2)));
        let config = TaskConfig::new()
            .with_fixed_retry(5, Duration::from_millis(1))
            .with_circuit_breaker(Arc::clone(&breaker));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let counter = Arc::clone(&counter_clone);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<TaskResult<String>, CanoError>(CanoError::task_execution("boom"))
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

        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || async {
            Ok::<TaskResult<String>, CanoError>(TaskResult::Single("ok".into()))
        })
        .await
        .unwrap();
        assert_eq!(result, TaskResult::Single("ok".into()));
    }

    #[tokio::test]
    async fn test_attempt_timeout_counts_as_circuit_failure() {
        // A timed-out attempt produces CanoError::Timeout, which must flow through
        // the same `record_failure` arm as any other error so the breaker counts
        // it toward the threshold. With `failure_threshold=2` and `RetryMode::None`,
        // two timed-out calls should trip the breaker; the third call must surface
        // CircuitOpen without ever invoking the task body.
        use std::sync::atomic::{AtomicUsize, Ordering};

        let breaker = Arc::new(CircuitBreaker::new(cb_policy(2)));
        let config = TaskConfig::minimal()
            .with_attempt_timeout(Duration::from_millis(10))
            .with_circuit_breaker(Arc::clone(&breaker));
        let invocations = Arc::new(AtomicUsize::new(0));

        for _ in 0..2 {
            let invocations = Arc::clone(&invocations);
            let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
                let invocations = Arc::clone(&invocations);
                async move {
                    invocations.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok::<TaskResult<String>, CanoError>(TaskResult::Single("never".into()))
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
        let result = run_with_retries::<TaskResult<String>, _, _>(&config, || {
            let invocations = Arc::clone(&invocations);
            async move {
                invocations.fetch_add(1, Ordering::SeqCst);
                Ok::<TaskResult<String>, CanoError>(TaskResult::Single("never".into()))
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
}

#[cfg(all(test, feature = "metrics"))]
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
        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 1,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        }));
        let p = breaker.try_acquire().unwrap();
        breaker.record_failure(p);
        let config = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));
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
