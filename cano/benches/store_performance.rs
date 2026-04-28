use cano::{MemoryStore, store::KeyValueStore};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

fn bench_storage_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage_operations");

    let operation_counts = vec![10, 100];

    for count in operation_counts {
        group.bench_with_input(
            BenchmarkId::new("put_operations", count),
            &count,
            |b, &count| {
                b.iter(|| {
                    let store = MemoryStore::new();
                    for i in 0..count {
                        store
                            .put(&format!("key_{i}"), format!("value_{i}"))
                            .unwrap();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("get_operations", count),
            &count,
            |b, &count| {
                let store = MemoryStore::new();
                // Pre-populate store
                for i in 0..count {
                    store
                        .put(&format!("key_{i}"), format!("value_{i}"))
                        .unwrap();
                }

                b.iter(|| {
                    for i in 0..count {
                        let _: Result<String, _> = store.get(&format!("key_{i}"));
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("put_list_operations", count),
            &count,
            |b, &count| {
                b.iter(|| {
                    let store = MemoryStore::new();
                    let mut list_items = Vec::new();
                    for i in 0..count {
                        list_items.push(format!("value_{i}"));
                    }
                    store.put("list_key", list_items).unwrap();
                });
            },
        );
    }

    group.finish();
}

/// Measures repeated put against the same key — the hot path for state-heavy workflows.
fn bench_put_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("put_throughput");

    for n in [10usize, 100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let store = MemoryStore::new();
            b.iter(|| {
                for i in 0..n {
                    store.put("hot_key", i as u64).unwrap();
                }
            });
        });
    }
    group.finish();
}

/// Measures repeated append against the same key — exercises the existing-key
/// fast path. Used as the gate for change 2.4 (in-place Arc replacement).
fn bench_append_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_throughput");

    for n in [10usize, 100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let store = MemoryStore::new();
                for i in 0..n {
                    store.append("hot_key", i as u64).unwrap();
                }
            });
        });
    }
    group.finish();
}

/// Measures `get` cost when this is the sole Arc holder (sole_owner) vs when
/// another holder exists (shared_owner). Documents the cost model for change
/// 2.3, which was rejected on analysis (the map's Arc holder always prevents
/// try_unwrap from succeeding without changing get's semantics).
fn bench_get_clone_vs_owned(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_clone_vs_owned");

    group.bench_function("sole_owner", |b| {
        let store = MemoryStore::new();
        store.put("k", vec![1u64; 1024]).unwrap();
        b.iter(|| {
            let _v: Vec<u64> = store.get("k").unwrap();
        });
    });

    group.bench_function("shared_owner", |b| {
        let store = MemoryStore::new();
        store.put("k", vec![1u64; 1024]).unwrap();
        // Hold a separate Arc to the value; this forces try_unwrap to fail and clone.
        let _holder = store.get_shared::<Vec<u64>>("k").unwrap();
        b.iter(|| {
            let _v: Vec<u64> = store.get("k").unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_storage_operations,
    bench_put_throughput,
    bench_append_throughput,
    bench_get_clone_vs_owned
);
criterion_main!(benches);
