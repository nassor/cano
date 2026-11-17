use cano::prelude::*;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::Arc;

/// Benchmark 1: Resources lookups (hot path overhead vs direct &Blackboard)
fn bench_resources_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("resources_get");

    // Direct Blackboard reference (baseline)
    group.bench_function("direct_store_get", |b| {
        let board = Blackboard::new();
        board.put("counter", 42u64);
        b.iter(|| {
            let _: u64 = board.get("counter").unwrap();
        });
    });

    // Resources::get by type → &Blackboard → board.get. The id predates the
    // type-keyed API and is kept so the baseline stays comparable.
    group.bench_function("resources_get_string_key", |b| {
        let resources = Resources::new().insert(Blackboard::new());
        resources.get::<Blackboard>().unwrap().put("counter", 42u64);
        b.iter(|| {
            let board = resources.get::<Blackboard>().unwrap();
            let _: u64 = board.get("counter").unwrap();
        });
    });

    // Resources::get_named → &Blackboard → board.get
    group.bench_function("named", |b| {
        let resources = Resources::new().insert_named("board", Blackboard::new());
        resources
            .get_named::<Blackboard>("board")
            .unwrap()
            .put("counter", 42u64);
        b.iter(|| {
            let board = resources.get_named::<Blackboard>("board").unwrap();
            let _: u64 = board.get("counter").unwrap();
        });
    });

    group.finish();
}

/// Benchmark 2: Lifecycle overhead for N no-op resources
fn bench_lifecycle_n_resources(c: &mut Criterion) {
    struct Noop;

    #[cano::resource]
    impl cano::resource::Resource for Noop {}

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("lifecycle_n_resources");

    for n in [1usize, 5, 10, 50] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            // Build the dictionary once per parameter — name allocation and insert
            // chaining are not part of what we want to measure. setup_all and
            // teardown_all take &self and Noop's hooks are no-ops, so the same
            // instance is reused across iterations.
            let mut resources = Resources::new();
            for i in 0..n {
                resources = resources.insert_named(format!("r{i}"), Noop);
            }
            b.to_async(&runtime).iter(|| async {
                resources.setup_all().await.unwrap();
                resources.teardown_all().await;
            });
        });
    }
    group.finish();
}

/// Benchmark 3: Concurrent lookups from spawned tasks (split pattern simulation)
fn bench_resources_get_concurrent(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("resources_concurrent_get");

    for concurrency in [2usize, 8, 16] {
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &concurrency| {
                let resources: Arc<Resources> =
                    Arc::new(Resources::new().insert(Blackboard::new()));
                b.iter(|| {
                    runtime.block_on(async {
                        let mut handles = Vec::with_capacity(concurrency);
                        for _ in 0..concurrency {
                            let res = Arc::clone(&resources);
                            handles.push(tokio::spawn(async move {
                                let _store = res.get::<Blackboard>().unwrap();
                            }));
                        }
                        for h in handles {
                            h.await.unwrap();
                        }
                    });
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    resource_benches,
    bench_resources_get,
    bench_lifecycle_n_resources,
    bench_resources_get_concurrent,
);
criterion_main!(resource_benches);
