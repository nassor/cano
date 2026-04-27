use cano::prelude::*;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::Arc;

/// Benchmark 1: Resources::get with string key (hot path overhead vs direct &MemoryStore)
fn bench_resources_get_string_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("resources_get");

    // Direct MemoryStore reference (baseline — old API pattern)
    group.bench_function("direct_store_get", |b| {
        let store = MemoryStore::new();
        store.put("counter", 42u64).unwrap();
        b.iter(|| {
            let _: u64 = store.get("counter").unwrap();
        });
    });

    // Resources::get → Arc<MemoryStore> → store.get (new API)
    group.bench_function("resources_get_string_key", |b| {
        let resources: Resources<String> =
            Resources::new().insert("store".to_owned(), MemoryStore::new());
        resources
            .get::<MemoryStore, str>("store")
            .unwrap()
            .put("counter", 42u64)
            .unwrap();
        b.iter(|| {
            let store = resources.get::<MemoryStore, str>("store").unwrap();
            let _: u64 = store.get("counter").unwrap();
        });
    });

    group.finish();
}

/// Benchmark 2: Resources::get with enum key
fn bench_resources_get_enum_key(c: &mut Criterion) {
    #[derive(Hash, Eq, PartialEq)]
    enum Key {
        Store,
    }

    let mut group = c.benchmark_group("resources_get_enum");
    group.bench_function("enum_key_get", |b| {
        let resources = Resources::<Key>::new().insert(Key::Store, MemoryStore::new());
        resources
            .get::<MemoryStore, Key>(&Key::Store)
            .unwrap()
            .put("counter", 42u64)
            .unwrap();
        b.iter(|| {
            let store = resources.get::<MemoryStore, Key>(&Key::Store).unwrap();
            let _: u64 = store.get("counter").unwrap();
        });
    });
    group.finish();
}

/// Benchmark 3: Lifecycle overhead for N no-op resources
fn bench_lifecycle_n_resources(c: &mut Criterion) {
    struct Noop;

    #[async_trait::async_trait]
    impl cano::resource::Resource for Noop {}

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("lifecycle_n_resources");

    for n in [1usize, 5, 10, 50] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            // Build the dictionary once per parameter — key allocation and insert
            // chaining are not part of what we want to measure. setup_all and
            // teardown_all take &self and Noop's hooks are no-ops, so the same
            // instance is reused across iterations.
            let mut resources: Resources<String> = Resources::new();
            for i in 0..n {
                resources = resources.insert(format!("r{i}"), Noop);
            }
            b.to_async(&runtime).iter(|| async {
                resources.setup_all().await.unwrap();
                // teardown_all returns (); per-resource errors are logged
                // internally. Noop never fails, so nothing to assert here.
                resources.teardown_all().await;
            });
        });
    }
    group.finish();
}

/// Benchmark 4: Concurrent Arc clone from get() (split pattern simulation)
fn bench_resources_get_concurrent(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("resources_concurrent_get");

    for concurrency in [2usize, 8, 16] {
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &concurrency| {
                let resources: Arc<Resources<String>> =
                    Arc::new(Resources::new().insert("store".to_owned(), MemoryStore::new()));
                b.iter(|| {
                    runtime.block_on(async {
                        let mut handles = Vec::with_capacity(concurrency);
                        for _ in 0..concurrency {
                            let res = Arc::clone(&resources);
                            handles.push(tokio::spawn(async move {
                                let _store = res.get::<MemoryStore, str>("store").unwrap();
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
    bench_resources_get_string_key,
    bench_resources_get_enum_key,
    bench_lifecycle_n_resources,
    bench_resources_get_concurrent,
);
criterion_main!(resource_benches);
