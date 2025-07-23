use cano::{MemoryStore, store::Store};
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

criterion_group!(benches, bench_storage_operations);
criterion_main!(benches);
