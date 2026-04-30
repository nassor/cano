use cano::prelude::*;
use cano::task::run_with_retries;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn fast_policy() -> CircuitPolicy {
    CircuitPolicy {
        failure_threshold: u32::MAX / 2,
        reset_timeout: Duration::from_secs(60),
        half_open_max_calls: 1,
    }
}

/// `try_acquire` + `record_success` round-trip on a Closed breaker.
///
/// Measures the per-call overhead of the breaker on the success path: two mutex
/// acquisitions, one `Arc::clone` into the permit, one drop. No retry loop, no
/// task body. This is the tightest microbenchmark of the breaker primitive.
fn bench_acquire_record_closed(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_acquire_record_closed");
    let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
    group.bench_function("single_thread", |b| {
        b.iter(|| {
            let permit = breaker.try_acquire().unwrap();
            breaker.record_success(permit);
        });
    });
    group.finish();
}

/// `try_acquire` against an Open breaker.
///
/// Exercises the rejection path — the previous diff replaced a per-rejection
/// `format!` allocation with a `&'static str`. This bench pins the post-fix cost.
fn bench_try_acquire_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_try_acquire_open");
    let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
        failure_threshold: 1,
        reset_timeout: Duration::from_secs(3600),
        half_open_max_calls: 1,
    }));
    // Trip it.
    let p = breaker.try_acquire().unwrap();
    breaker.record_failure(p);

    group.bench_function("rejected", |b| {
        b.iter(|| {
            let _ = breaker.try_acquire().unwrap_err();
        });
    });
    group.finish();
}

/// `run_with_retries` with vs without a breaker, success path.
///
/// Quantifies how much overhead the breaker integration adds to the retry loop
/// in the steady-state success case (no retries, no timeout).
fn bench_run_with_retries_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_run_with_retries");
    let rt = tokio::runtime::Runtime::new().unwrap();

    let no_breaker = TaskConfig::minimal();
    group.bench_function("no_breaker", |b| {
        b.to_async(&rt).iter(|| async {
            let _ = run_with_retries::<u32, _, _>(&no_breaker, || async { Ok(42u32) }).await;
        });
    });

    let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
    let with_breaker = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));
    group.bench_function("with_breaker", |b| {
        b.to_async(&rt).iter(|| async {
            let _ = run_with_retries::<u32, _, _>(&with_breaker, || async { Ok(42u32) }).await;
        });
    });
    group.finish();
}

/// Concurrent throughput on a shared breaker.
///
/// N parallel tasks share one Arc<CircuitBreaker>. This is the contention
/// scenario that motivates atomic-fast-path optimization.
fn bench_shared_breaker_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_shared_concurrent");
    let rt = tokio::runtime::Runtime::new().unwrap();

    for &n in &[16usize, 64, 256] {
        let breaker = Arc::new(CircuitBreaker::new(fast_policy()));
        group.bench_with_input(BenchmarkId::new("tasks", n), &n, |b, &n| {
            b.to_async(&rt).iter(|| {
                let breaker = Arc::clone(&breaker);
                async move {
                    let mut handles = Vec::with_capacity(n);
                    for _ in 0..n {
                        let breaker = Arc::clone(&breaker);
                        handles.push(tokio::spawn(async move {
                            let permit = breaker.try_acquire().unwrap();
                            breaker.record_success(permit);
                        }));
                    }
                    for h in handles {
                        let _ = h.await;
                    }
                }
            });
        });
    }
    group.finish();
}

/// `run_with_retries` with `attempt_timeout` set short enough to always trip.
///
/// Exercises the timeout-error allocation path (`format!("Task attempt {} ...")`)
/// — the candidate for P2 (replace with `&'static str`).
fn bench_attempt_timeout_error_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_timeout_error_path");
    let rt = tokio::runtime::Runtime::new().unwrap();

    // 1ms attempt_timeout + 5ms task body → every attempt times out.
    // 0 retries → exactly one attempt per `run_with_retries` call.
    let config = TaskConfig::minimal().with_attempt_timeout(Duration::from_millis(1));

    group.bench_function("single_timeout", |b| {
        b.to_async(&rt).iter(|| async {
            let _ = run_with_retries::<u32, _, _>(&config, || async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok::<u32, CanoError>(0)
            })
            .await;
        });
    });
    group.finish();
}

/// Isolated cost of constructing the timeout error.
///
/// The full `run_with_retries` timeout bench is dominated by `tokio::time::sleep`,
/// hiding the construction cost in noise. This microbench pins the per-call cost
/// — guards against a regression that reintroduces dynamic formatting on this path.
fn bench_timeout_error_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_timeout_error_construction");
    group.bench_function("static_str", |b| {
        b.iter(|| {
            let err = CanoError::timeout("task attempt exceeded attempt_timeout");
            std::hint::black_box(err);
        });
    });
    group.finish();
}

/// Repeated rejection by an Open breaker through `run_with_retries`.
///
/// Companion to `bench_try_acquire_open` but exercising the full retry-loop
/// short-circuit. Measures per-call cost when the dependency is hard-down.
fn bench_run_with_retries_open_short_circuit(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_run_with_retries_open");
    let rt = tokio::runtime::Runtime::new().unwrap();

    let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
        failure_threshold: 1,
        reset_timeout: Duration::from_secs(3600),
        half_open_max_calls: 1,
    }));
    let p = breaker.try_acquire().unwrap();
    breaker.record_failure(p);

    let config = TaskConfig::minimal().with_circuit_breaker(Arc::clone(&breaker));
    let calls = AtomicUsize::new(0);

    group.bench_function("rejected", |b| {
        b.to_async(&rt).iter(|| async {
            let _ = run_with_retries::<u32, _, _>(&config, || async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<u32, CanoError>(0)
            })
            .await;
        });
    });
    // Sanity: the body must never run while the breaker is open.
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    group.finish();
}

criterion_group!(
    benches,
    bench_acquire_record_closed,
    bench_try_acquire_open,
    bench_run_with_retries_overhead,
    bench_shared_breaker_concurrent,
    bench_attempt_timeout_error_path,
    bench_run_with_retries_open_short_circuit,
    bench_timeout_error_construction,
);
criterion_main!(benches);
