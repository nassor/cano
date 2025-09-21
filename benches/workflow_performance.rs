//! # Workflow Performance Benchmarks
//!
//! Simple benchmarks focusing on the current API functionality.

use async_trait::async_trait;
use cano::prelude::*;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::time::Duration;

/// Simple node for benchmarking
#[derive(Clone)]
struct SimpleNode {
    next_state: TestState,
    duration: Duration,
}

impl SimpleNode {
    fn new(next_state: TestState, duration: Duration) -> Self {
        Self {
            next_state,
            duration,
        }
    }
}

/// IO-bound node that simulates network/disk operations
#[derive(Clone)]
struct IOBoundNode {
    next_state: TestState,
    io_duration: Duration,
    operation_type: String,
}

impl IOBoundNode {
    fn new(next_state: TestState, io_duration: Duration, operation_type: &str) -> Self {
        Self {
            next_state,
            io_duration,
            operation_type: operation_type.to_string(),
        }
    }
}

/// CPU-bound node that performs intensive calculations
#[derive(Clone)]
struct CPUBoundNode {
    next_state: TestState,
    iterations: usize,
    operation_type: String,
}

impl CPUBoundNode {
    fn new(next_state: TestState, iterations: usize, operation_type: &str) -> Self {
        Self {
            next_state,
            iterations,
            operation_type: operation_type.to_string(),
        }
    }
}

/// Test states for workflow benchmarking
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TestState {
    Step1,
    Complete,
}

#[async_trait]
impl Node<TestState> for SimpleNode {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        tokio::time::sleep(self.duration / 4).await;
        Ok("prepared".to_string())
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        tokio::time::sleep(self.duration / 2).await;
        format!("processed_{}", prep_res)
    }

    async fn post(
        &self,
        _store: &MemoryStore,
        _exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        tokio::time::sleep(self.duration / 4).await;
        Ok(self.next_state.clone())
    }
}

#[async_trait]
impl Node<TestState> for IOBoundNode {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        // Simulate database connection or file opening
        tokio::time::sleep(self.io_duration / 8).await;

        // Store some metadata about the operation
        let metadata = format!("io_prep_{}", self.operation_type);
        let _ = store.put("io_metadata", metadata);

        Ok(format!("io_prepared_{}", self.operation_type))
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        // Simulate the main IO operation (network request, file read, database query)
        tokio::time::sleep(self.io_duration).await;
        format!("io_result_{}_{}", self.operation_type, prep_res)
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        // Simulate cleanup and result storage
        tokio::time::sleep(self.io_duration / 16).await;

        // Store the result for potential downstream nodes
        let result_key = format!("io_result_{}", self.operation_type);
        let _ = store.put(&result_key, exec_res);

        Ok(self.next_state.clone())
    }
}

#[async_trait]
impl Node<TestState> for CPUBoundNode {
    type PrepResult = Vec<u64>;
    type ExecResult = u64;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        // Prepare data for CPU-intensive computation
        let data: Vec<u64> = (0..self.iterations as u64).collect();

        // Store computation metadata
        let metadata = format!("cpu_prep_{}_{}", self.operation_type, self.iterations);
        let _ = store.put("cpu_metadata", metadata);

        Ok(data)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        // Perform CPU-intensive calculation
        match self.operation_type.as_str() {
            "fibonacci" => {
                // Calculate Fibonacci numbers
                prep_res.iter().map(|&n| fibonacci(n % 40)).sum()
            }
            "prime_check" => {
                // Check for prime numbers
                prep_res.iter().filter(|&&n| is_prime(n)).count() as u64
            }
            "matrix_multiply" => {
                // Simulate matrix multiplication
                prep_res.iter().map(|&n| n * n).sum()
            }
            _ => {
                // Default: simple sum
                prep_res.iter().sum()
            }
        }
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        // Store computation result
        let result_key = format!("cpu_result_{}", self.operation_type);
        let _ = store.put(&result_key, exec_res);

        Ok(self.next_state.clone())
    }
}

// Helper functions for CPU-bound operations
fn fibonacci(n: u64) -> u64 {
    if n <= 1 {
        n
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
}

fn is_prime(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    for i in 2..=(n as f64).sqrt() as u64 {
        if n.is_multiple_of(i) {
            return false;
        }
    }
    true
}

fn bench_node_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_performance");

    let durations = vec![
        Duration::from_micros(1),
        Duration::from_micros(10),
        Duration::from_micros(100),
        Duration::from_millis(1),
    ];

    for &duration in &durations {
        group.bench_with_input(
            BenchmarkId::new("single_node_execution", duration.as_micros()),
            &duration,
            |b, &duration| {
                let node = SimpleNode::new(TestState::Complete, duration);
                let store = MemoryStore::new();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let result = cano::Node::run(&node, &store).await;
                        assert!(result.is_ok());
                    });
            },
        );
    }

    group.finish();
}

fn bench_concurrent_node_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_node_execution");

    let counts = vec![2, 5, 10, 20];
    let duration = Duration::from_micros(100);

    for &count in &counts {
        group.bench_with_input(
            BenchmarkId::new("parallel_nodes", count),
            &count,
            |b, &count| {
                let nodes: Vec<SimpleNode> = (0..count)
                    .map(|_| SimpleNode::new(TestState::Complete, duration))
                    .collect();
                let store = MemoryStore::new();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let tasks: Vec<_> = nodes
                            .iter()
                            .map(|node| cano::Node::run(node, &store))
                            .collect();

                        let results = futures::future::join_all(tasks).await;
                        for result in results {
                            assert!(result.is_ok());
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("sequential_nodes", count),
            &count,
            |b, &count| {
                let nodes: Vec<SimpleNode> = (0..count)
                    .map(|_| SimpleNode::new(TestState::Complete, duration))
                    .collect();
                let store = MemoryStore::new();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        for node in &nodes {
                            let result = cano::Node::run(node, &store).await;
                            assert!(result.is_ok());
                        }
                    });
            },
        );
    }

    group.finish();
}

fn bench_store_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_operations");

    let operation_counts = vec![10, 100, 1000];

    for &count in &operation_counts {
        group.bench_with_input(
            BenchmarkId::new("put_operations", count),
            &count,
            |b, &count| {
                b.iter(|| {
                    let store = MemoryStore::new();
                    for i in 0..count {
                        let key = format!("key_{i}");
                        let _ = store.put(&key, i);
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("get_operations", count),
            &count,
            |b, &count| {
                let store = MemoryStore::new();
                // Pre-populate the store
                for i in 0..count {
                    let key = format!("key_{i}");
                    let _ = store.put(&key, i);
                }

                b.iter(|| {
                    for i in 0..count {
                        let key = format!("key_{i}");
                        let _ = store.get::<i32>(&key);
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    let sizes = vec![100, 1000, 10000];

    for &size in &sizes {
        group.bench_with_input(
            BenchmarkId::new("node_creation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _nodes: Vec<SimpleNode> = (0..size)
                        .map(|_| SimpleNode::new(TestState::Complete, Duration::from_micros(1)))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("store_creation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _stores: Vec<MemoryStore> = (0..size).map(|_| MemoryStore::new()).collect();
                });
            },
        );
    }

    group.finish();
}

fn bench_io_bound_workflows(c: &mut Criterion) {
    let mut group = c.benchmark_group("io_bound_workflows");

    let io_durations = vec![
        Duration::from_millis(1),
        Duration::from_millis(5),
        Duration::from_millis(10),
        Duration::from_millis(25),
    ];

    let node_counts = vec![2, 5, 10];

    for &duration in &io_durations {
        for &count in &node_counts {
            // Sequential IO-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "sequential_io",
                    format!("{}ms_{}nodes", duration.as_millis(), count),
                ),
                &(duration, count),
                |b, &(duration, count)| {
                    let nodes: Vec<IOBoundNode> = (0..count)
                        .map(|i| {
                            IOBoundNode::new(
                                TestState::Complete,
                                duration,
                                &format!("db_query_{i}"),
                            )
                        })
                        .collect();
                    let store = MemoryStore::new();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            for node in &nodes {
                                let result = cano::Node::run(node, &store).await;
                                assert!(result.is_ok());
                            }
                        });
                },
            );

            // Concurrent IO-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "concurrent_io",
                    format!("{}ms_{}nodes", duration.as_millis(), count),
                ),
                &(duration, count),
                |b, &(duration, count)| {
                    let nodes: Vec<IOBoundNode> = (0..count)
                        .map(|i| {
                            IOBoundNode::new(
                                TestState::Complete,
                                duration,
                                &format!("api_call_{i}"),
                            )
                        })
                        .collect();
                    let store = MemoryStore::new();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let tasks: Vec<_> = nodes
                                .iter()
                                .map(|node| cano::Node::run(node, &store))
                                .collect();

                            let results = futures::future::join_all(tasks).await;
                            for result in results {
                                assert!(result.is_ok());
                            }
                        });
                },
            );
        }
    }

    group.finish();
}

fn bench_cpu_bound_workflows(c: &mut Criterion) {
    let mut group = c.benchmark_group("cpu_bound_workflows");

    let cpu_configs = vec![
        (100, "fibonacci"),
        (500, "fibonacci"),
        (1000, "prime_check"),
        (5000, "matrix_multiply"),
    ];

    let node_counts = vec![2, 4, 8];

    for &(iterations, operation) in &cpu_configs {
        for &count in &node_counts {
            // Sequential CPU-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "sequential_cpu",
                    format!("{operation}_{iterations}_{count}nodes"),
                ),
                &(iterations, operation, count),
                |b, &(iterations, operation, count)| {
                    let nodes: Vec<CPUBoundNode> = (0..count)
                        .map(|i| {
                            CPUBoundNode::new(
                                TestState::Complete,
                                iterations,
                                &format!("{operation}_{i}"),
                            )
                        })
                        .collect();
                    let store = MemoryStore::new();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            for node in &nodes {
                                let result = <_ as Node<TestState>>::run(node, &store).await;
                                assert!(result.is_ok());
                            }
                        });
                },
            );

            // Concurrent CPU-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "concurrent_cpu",
                    format!("{operation}_{iterations}_{count}nodes"),
                ),
                &(iterations, operation, count),
                |b, &(iterations, operation, count)| {
                    let nodes: Vec<CPUBoundNode> = (0..count)
                        .map(|i| {
                            CPUBoundNode::new(
                                TestState::Complete,
                                iterations,
                                &format!("{operation}_{i}"),
                            )
                        })
                        .collect();
                    let store = MemoryStore::new();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let tasks: Vec<_> = nodes
                                .iter()
                                .map(|node| <_ as Node<TestState>>::run(node, &store))
                                .collect();

                            let results = futures::future::join_all(tasks).await;
                            for result in results {
                                assert!(result.is_ok());
                            }
                        });
                },
            );
        }
    }

    group.finish();
}

fn bench_mixed_workload_workflows(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_workload_workflows");

    let scenarios = vec![
        ("light_mixed", Duration::from_millis(1), 100),
        ("medium_mixed", Duration::from_millis(5), 500),
        ("heavy_mixed", Duration::from_millis(10), 1000),
    ];

    for &(scenario_name, io_duration, cpu_iterations) in &scenarios {
        // Sequential mixed workflow (IO + CPU alternating)
        group.bench_with_input(
            BenchmarkId::new("sequential_mixed", scenario_name),
            &(io_duration, cpu_iterations),
            |b, &(io_duration, cpu_iterations)| {
                let io_node = IOBoundNode::new(TestState::Step1, io_duration, "database");
                let cpu_node = CPUBoundNode::new(TestState::Complete, cpu_iterations, "processing");
                let store = MemoryStore::new();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let io_result = <_ as Node<TestState>>::run(&io_node, &store).await;
                        assert!(io_result.is_ok());

                        let cpu_result = <_ as Node<TestState>>::run(&cpu_node, &store).await;
                        assert!(cpu_result.is_ok());
                    });
            },
        );

        // Concurrent mixed workflow (IO and CPU in parallel)
        group.bench_with_input(
            BenchmarkId::new("concurrent_mixed", scenario_name),
            &(io_duration, cpu_iterations),
            |b, &(io_duration, cpu_iterations)| {
                let io_node = IOBoundNode::new(TestState::Complete, io_duration, "api_service");
                let cpu_node = CPUBoundNode::new(TestState::Complete, cpu_iterations, "analytics");
                let store = MemoryStore::new();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let io_task = <_ as Node<TestState>>::run(&io_node, &store);
                        let cpu_task = <_ as Node<TestState>>::run(&cpu_node, &store);

                        let (io_result, cpu_result) =
                            futures::future::join(io_task, cpu_task).await;
                        assert!(io_result.is_ok());
                        assert!(cpu_result.is_ok());
                    });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_node_performance,
    bench_concurrent_node_execution,
    bench_store_operations,
    bench_memory_patterns,
    bench_io_bound_workflows,
    bench_cpu_bound_workflows,
    bench_mixed_workload_workflows,
);

criterion_main!(benches);
