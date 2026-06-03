use cano::prelude::*;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use std::time::Duration;

/// A bucket so large and fast-refilling that `try_acquire` always finds a token — isolates the
/// success-path cost (one mutex lock, a refill computation, a token decrement, an `Arc::clone`).
fn big_bucket() -> RateLimiterPolicy {
    RateLimiterPolicy::per_second(u32::MAX / 2).with_max_tokens(u32::MAX / 2)
}

/// `try_acquire` on a bucket that always has tokens.
///
/// The tightest microbenchmark of the limiter primitive on the success path.
fn bench_try_acquire_success(c: &mut Criterion) {
    let mut group = c.benchmark_group("rate_limiter_try_acquire_success");
    let limiter = Arc::new(RateLimiter::new(big_bucket()));
    group.bench_function("single_thread", |b| {
        b.iter(|| {
            let permit = limiter.try_acquire();
            std::hint::black_box(&permit);
        });
    });
    group.finish();
}

/// `try_acquire` on a drained, slow-refilling bucket — always rejected.
///
/// Exercises the rejection path: the refill computation finds no whole token and returns
/// `None` without allocating.
fn bench_try_acquire_rejected(c: &mut Criterion) {
    let mut group = c.benchmark_group("rate_limiter_try_acquire_rejected");
    // 1 token / hour, capacity 1: drain the only token and it stays empty for the whole bench.
    let limiter = Arc::new(RateLimiter::new(
        RateLimiterPolicy::new(1, Duration::from_secs(3600)).with_max_tokens(1),
    ));
    let _ = limiter.try_acquire().expect("drain the only token");

    group.bench_function("rejected", |b| {
        b.iter(|| {
            let permit = limiter.try_acquire();
            std::hint::black_box(&permit);
        });
    });
    group.finish();
}

/// Concurrent throughput on a shared limiter.
///
/// N parallel tasks share one `Arc<RateLimiter>` and all hit the success path. This is the
/// contention scenario that motivates keeping the critical section tiny.
fn bench_shared_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("rate_limiter_shared_concurrent");
    let rt = tokio::runtime::Runtime::new().unwrap();

    for &n in &[16usize, 64, 256] {
        let limiter = Arc::new(RateLimiter::new(big_bucket()));
        group.bench_with_input(BenchmarkId::new("tasks", n), &n, |b, &n| {
            b.to_async(&rt).iter(|| {
                let limiter = Arc::clone(&limiter);
                async move {
                    let mut handles = Vec::with_capacity(n);
                    for _ in 0..n {
                        let limiter = Arc::clone(&limiter);
                        handles.push(tokio::spawn(async move {
                            let permit = limiter.try_acquire();
                            std::hint::black_box(&permit);
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

criterion_group!(
    benches,
    bench_try_acquire_success,
    bench_try_acquire_rejected,
    bench_shared_concurrent,
);
criterion_main!(benches);
