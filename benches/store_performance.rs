use cano::{MemoryStore, store::StoreTrait};
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
                    let storage = MemoryStore::new();
                    for i in 0..count {
                        storage
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
                let storage = MemoryStore::new();
                // Pre-populate storage
                for i in 0..count {
                    storage
                        .put(&format!("key_{i}"), format!("value_{i}"))
                        .unwrap();
                }

                b.iter(|| {
                    for i in 0..count {
                        let _: Result<String, _> = storage.get(&format!("key_{i}"));
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("put_list_operations", count),
            &count,
            |b, &count| {
                b.iter(|| {
                    let storage = MemoryStore::new();
                    let mut list_items = Vec::new();
                    for i in 0..count {
                        list_items.push(format!("value_{i}"));
                    }
                    storage.put("list_key", list_items).unwrap();
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_storage_operations);
criterion_main!(benches);
