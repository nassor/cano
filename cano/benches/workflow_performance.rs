//! # Workflow Performance Benchmarks
//!
//! Simple benchmarks focusing on the current API functionality.

use cano::prelude::*;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::time::Duration;

/// Simple task for benchmarking
#[derive(Clone)]
struct SimpleTask {
    next_state: TestState,
    duration: Duration,
}

impl SimpleTask {
    fn new(next_state: TestState, duration: Duration) -> Self {
        Self {
            next_state,
            duration,
        }
    }
}

/// IO-bound task that simulates network/disk operations
#[derive(Clone)]
struct IOBoundTask {
    next_state: TestState,
    io_duration: Duration,
    operation_type: String,
}

impl IOBoundTask {
    fn new(next_state: TestState, io_duration: Duration, operation_type: &str) -> Self {
        Self {
            next_state,
            io_duration,
            operation_type: operation_type.to_string(),
        }
    }
}

/// CPU-bound task that performs intensive calculations
#[derive(Clone)]
struct CPUBoundTask {
    next_state: TestState,
    iterations: usize,
    operation_type: String,
}

impl CPUBoundTask {
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

#[task]
impl Task<TestState> for SimpleTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        tokio::time::sleep(self.duration / 4).await;
        // simulate exec phase
        tokio::time::sleep(self.duration / 2).await;
        // simulate post phase
        tokio::time::sleep(self.duration / 4).await;
        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

#[task]
impl Task<TestState> for IOBoundTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        // Simulate database connection or file opening (prep phase)
        tokio::time::sleep(self.io_duration / 8).await;

        let store = res.get::<MemoryStore, str>("store")?;
        // Store some metadata about the operation
        let metadata = format!("io_prep_{}", self.operation_type);
        let _ = store.put("io_metadata", metadata);

        // Simulate the main IO operation (exec phase)
        tokio::time::sleep(self.io_duration).await;
        let exec_res = format!(
            "io_result_{}_io_prepared_{}",
            self.operation_type, self.operation_type
        );

        // Simulate cleanup and result storage (post phase)
        tokio::time::sleep(self.io_duration / 16).await;
        let result_key = format!("io_result_{}", self.operation_type);
        let _ = store.put(&result_key, exec_res);

        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

#[task]
impl Task<TestState> for CPUBoundTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        // Prepare data for CPU-intensive computation (prep phase)
        let data: Vec<u64> = (0..self.iterations as u64).collect();

        let store = res.get::<MemoryStore, str>("store")?;
        // Store computation metadata
        let metadata = format!("cpu_prep_{}_{}", self.operation_type, self.iterations);
        let _ = store.put("cpu_metadata", metadata);

        // Perform CPU-intensive calculation (exec phase)
        let result: u64 = match self.operation_type.as_str() {
            "fibonacci" => {
                // Calculate Fibonacci numbers
                data.iter().map(|&n| fibonacci(n % 40)).sum()
            }
            "prime_check" => {
                // Check for prime numbers
                data.iter().filter(|&&n| is_prime(n)).count() as u64
            }
            "matrix_multiply" => {
                // Simulate matrix multiplication
                data.iter().map(|&n| n * n).sum()
            }
            _ => {
                // Default: simple sum
                data.iter().sum()
            }
        };

        // Store computation result (post phase)
        let result_key = format!("cpu_result_{}", self.operation_type);
        let _ = store.put(&result_key, result);

        Ok(TaskResult::Single(self.next_state.clone()))
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

fn bench_task_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("workflow_task_performance");

    let durations = vec![
        Duration::from_micros(1),
        Duration::from_micros(10),
        Duration::from_micros(100),
        Duration::from_millis(1),
    ];

    for &duration in &durations {
        group.bench_with_input(
            BenchmarkId::new("single_task_execution", duration.as_micros()),
            &duration,
            |b, &duration| {
                let task = SimpleTask::new(TestState::Complete, duration);
                let resources = Resources::new().insert("store", MemoryStore::new());

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let result = Task::<TestState>::run(&task, &resources).await;
                        assert!(result.is_ok());
                    });
            },
        );
    }

    group.finish();
}

fn bench_concurrent_task_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_task_execution");

    let counts = vec![2, 5, 10, 20];
    let duration = Duration::from_micros(100);

    for &count in &counts {
        group.bench_with_input(
            BenchmarkId::new("parallel_tasks", count),
            &count,
            |b, &count| {
                let tasks: Vec<SimpleTask> = (0..count)
                    .map(|_| SimpleTask::new(TestState::Complete, duration))
                    .collect();
                let resources = Resources::new().insert("store", MemoryStore::new());

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let futs: Vec<_> = tasks
                            .iter()
                            .map(|task| Task::<TestState>::run(task, &resources))
                            .collect();

                        let results = futures_util::future::join_all(futs).await;
                        for result in results {
                            assert!(result.is_ok());
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("sequential_tasks", count),
            &count,
            |b, &count| {
                let tasks: Vec<SimpleTask> = (0..count)
                    .map(|_| SimpleTask::new(TestState::Complete, duration))
                    .collect();
                let resources = Resources::new().insert("store", MemoryStore::new());

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        for task in &tasks {
                            let result = Task::<TestState>::run(task, &resources).await;
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
            BenchmarkId::new("task_creation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _tasks: Vec<SimpleTask> = (0..size)
                        .map(|_| SimpleTask::new(TestState::Complete, Duration::from_micros(1)))
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

    let task_counts = vec![2, 5, 10];

    for &duration in &io_durations {
        for &count in &task_counts {
            // Sequential IO-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "sequential_io",
                    format!("{}ms_{}tasks", duration.as_millis(), count),
                ),
                &(duration, count),
                |b, &(duration, count)| {
                    let tasks: Vec<IOBoundTask> = (0..count)
                        .map(|i| {
                            IOBoundTask::new(
                                TestState::Complete,
                                duration,
                                &format!("db_query_{i}"),
                            )
                        })
                        .collect();
                    let resources = Resources::new().insert("store", MemoryStore::new());

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            for task in &tasks {
                                let result = Task::<TestState>::run(task, &resources).await;
                                assert!(result.is_ok());
                            }
                        });
                },
            );

            // Concurrent IO-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "concurrent_io",
                    format!("{}ms_{}tasks", duration.as_millis(), count),
                ),
                &(duration, count),
                |b, &(duration, count)| {
                    let tasks: Vec<IOBoundTask> = (0..count)
                        .map(|i| {
                            IOBoundTask::new(
                                TestState::Complete,
                                duration,
                                &format!("api_call_{i}"),
                            )
                        })
                        .collect();
                    let resources = Resources::new().insert("store", MemoryStore::new());

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let futs: Vec<_> = tasks
                                .iter()
                                .map(|task| Task::<TestState>::run(task, &resources))
                                .collect();

                            let results = futures_util::future::join_all(futs).await;
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

    let task_counts = vec![2, 4, 8];

    for &(iterations, operation) in &cpu_configs {
        for &count in &task_counts {
            // Sequential CPU-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "sequential_cpu",
                    format!("{operation}_{iterations}_{count}tasks"),
                ),
                &(iterations, operation, count),
                |b, &(iterations, operation, count)| {
                    let tasks: Vec<CPUBoundTask> = (0..count)
                        .map(|i| {
                            CPUBoundTask::new(
                                TestState::Complete,
                                iterations,
                                &format!("{operation}_{i}"),
                            )
                        })
                        .collect();
                    let resources = Resources::new().insert("store", MemoryStore::new());

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            for task in &tasks {
                                let result = Task::<TestState>::run(task, &resources).await;
                                assert!(result.is_ok());
                            }
                        });
                },
            );

            // Concurrent CPU-bound workflow
            group.bench_with_input(
                BenchmarkId::new(
                    "concurrent_cpu",
                    format!("{operation}_{iterations}_{count}tasks"),
                ),
                &(iterations, operation, count),
                |b, &(iterations, operation, count)| {
                    let tasks: Vec<CPUBoundTask> = (0..count)
                        .map(|i| {
                            CPUBoundTask::new(
                                TestState::Complete,
                                iterations,
                                &format!("{operation}_{i}"),
                            )
                        })
                        .collect();
                    let resources = Resources::new().insert("store", MemoryStore::new());

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let futs: Vec<_> = tasks
                                .iter()
                                .map(|task| Task::<TestState>::run(task, &resources))
                                .collect();

                            let results = futures_util::future::join_all(futs).await;
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
                let io_task = IOBoundTask::new(TestState::Step1, io_duration, "database");
                let cpu_task = CPUBoundTask::new(TestState::Complete, cpu_iterations, "processing");
                let resources = Resources::new().insert("store", MemoryStore::new());

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let io_result = Task::<TestState>::run(&io_task, &resources).await;
                        assert!(io_result.is_ok());

                        let cpu_result = Task::<TestState>::run(&cpu_task, &resources).await;
                        assert!(cpu_result.is_ok());
                    });
            },
        );

        // Concurrent mixed workflow (IO and CPU in parallel)
        group.bench_with_input(
            BenchmarkId::new("concurrent_mixed", scenario_name),
            &(io_duration, cpu_iterations),
            |b, &(io_duration, cpu_iterations)| {
                let io_task = IOBoundTask::new(TestState::Complete, io_duration, "api_service");
                let cpu_task = CPUBoundTask::new(TestState::Complete, cpu_iterations, "analytics");
                let resources = Resources::new().insert("store", MemoryStore::new());

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let io_fut = Task::<TestState>::run(&io_task, &resources);
                        let cpu_fut = Task::<TestState>::run(&cpu_task, &resources);

                        let (io_result, cpu_result) =
                            futures_util::future::join(io_fut, cpu_fut).await;
                        assert!(io_result.is_ok());
                        assert!(cpu_result.is_ok());
                    });
            },
        );
    }

    group.finish();
}

/// Measures per-call overhead of orchestrate() on a trivial 1-state workflow.
/// Exercises validate(), validate_initial_state(), setup_all, FSM loop entry,
/// teardown. Used as the gate for change 3.1 (cache validate via OnceLock).
fn bench_orchestrate_overhead(c: &mut Criterion) {
    use std::sync::Arc;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum S {
        Done,
    }

    #[derive(Clone)]
    struct Noop;

    #[cano::task]
    impl Task<S> for Noop {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(S::Done))
        }
    }

    let workflow = Arc::new(
        Workflow::<S>::bare()
            .register(S::Done, Noop)
            .add_exit_state(S::Done),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("orchestrate_overhead", |b| {
        let workflow = Arc::clone(&workflow);
        b.to_async(&runtime).iter(|| {
            let workflow = Arc::clone(&workflow);
            async move {
                let _ = workflow.orchestrate(S::Done).await;
            }
        });
    });
}

/// Bench `collect_results` for splits of N tasks where N is large enough that
/// the SplitResult Vec/HashSet allocations matter. Used to gate change 1.2
/// (pre-allocate SplitResult, replace HashSet completion tracker with Vec<bool>).
fn bench_large_split_collect(c: &mut Criterion) {
    let mut group = c.benchmark_group("large_split_collect");
    let runtime = tokio::runtime::Runtime::new().unwrap();

    for n in [10usize, 100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            #[derive(Clone, Debug, PartialEq, Eq, Hash)]
            enum S {
                Start,
                Done,
            }

            #[derive(Clone)]
            struct Noop;

            #[cano::task]
            impl Task<S> for Noop {
                async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
                    Ok(TaskResult::Single(S::Done))
                }
            }

            let workflow = Workflow::<S>::bare()
                .register_split(
                    S::Start,
                    (0..n).map(|_| Noop).collect::<Vec<_>>(),
                    JoinConfig::new(JoinStrategy::All, S::Done),
                )
                .add_exit_state(S::Done);

            b.to_async(&runtime).iter(|| async {
                let _ = workflow.orchestrate(S::Start).await;
            });
        });
    }
    group.finish();
}

/// Measures cost of info_span! / debug! macros when tracing feature is enabled
/// but no subscriber is attached. Without optimization, span allocations
/// happen even with no consumer. Gate for change 3.2 (skip span when no subscriber).
#[cfg(feature = "tracing")]
fn bench_tracing_overhead(c: &mut Criterion) {
    use std::sync::Arc;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum S {
        Done,
    }

    #[derive(Clone)]
    struct Noop;

    #[cano::task]
    impl Task<S> for Noop {
        async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
            Ok(TaskResult::Single(S::Done))
        }
    }

    // Note: no tracing subscriber initialized — that's the point.
    let workflow = Arc::new(
        Workflow::<S>::bare()
            .register(S::Done, Noop)
            .add_exit_state(S::Done),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("tracing_overhead_no_subscriber", |b| {
        let workflow = Arc::clone(&workflow);
        b.to_async(&runtime).iter(|| {
            let workflow = Arc::clone(&workflow);
            async move {
                let _ = workflow.orchestrate(S::Done).await;
            }
        });
    });
}

criterion_group!(
    benches,
    bench_task_performance,
    bench_concurrent_task_execution,
    bench_store_operations,
    bench_memory_patterns,
    bench_io_bound_workflows,
    bench_cpu_bound_workflows,
    bench_orchestrate_overhead,
    bench_large_split_collect,
    bench_mixed_workload_workflows,
);

#[cfg(feature = "tracing")]
criterion_group!(tracing_benches, bench_tracing_overhead);

#[cfg(feature = "tracing")]
criterion_main!(benches, tracing_benches);

#[cfg(not(feature = "tracing"))]
criterion_main!(benches);
