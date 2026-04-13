use cano::{MemoryStore, store::KeyValueStore};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::Arc;

fn bench_store_shared_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_shared_performance");

    // Test with different data sizes
    // Small: 1KB
    // Medium: 100KB
    // Large: 10MB
    let sizes = vec![1024, 100 * 1024, 10 * 1024 * 1024];

    for size in sizes {
        let data = vec![0u8; size];
        let size_label = if size < 1024 * 1024 {
            format!("{}KB", size / 1024)
        } else {
            format!("{}MB", size / 1024 / 1024)
        };

        group.bench_with_input(
            BenchmarkId::new("get_clone", &size_label),
            &size,
            |b, &_size| {
                let store = MemoryStore::new();
                store.put("data", data.clone()).unwrap();

                b.iter(|| {
                    let _: Vec<u8> = store.get("data").unwrap();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("get_shared", &size_label),
            &size,
            |b, &_size| {
                let store = MemoryStore::new();
                store.put("data", data.clone()).unwrap();

                b.iter(|| {
                    let _: Arc<Vec<u8>> = store.get_shared("data").unwrap();
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_store_shared_operations);
criterion_main!(benches);
