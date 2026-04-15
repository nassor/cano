use async_trait::async_trait;
use cano::prelude::*;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

/// Simple do-nothing task for benchmarking
#[derive(Clone)]
struct DoNothingTask {
    next_state: TestState,
}

impl DoNothingTask {
    fn new(next_state: TestState) -> Self {
        Self { next_state }
    }
}

/// Test states for task benchmarking
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TestState {
    #[allow(dead_code)]
    Start,
    Task(usize),
    Complete,
}

#[async_trait]
impl Task<TestState> for DoNothingTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        // Do nothing - minimal overhead
        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

/// CPU-intensive task for benchmarking computational overhead
#[derive(Clone)]
struct CpuIntensiveTask {
    next_state: TestState,
    iterations: usize,
}

impl CpuIntensiveTask {
    fn new(next_state: TestState, iterations: usize) -> Self {
        Self {
            next_state,
            iterations,
        }
    }
}

#[async_trait]
impl Task<TestState> for CpuIntensiveTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;

        // Generate some data to process
        let data: Vec<u64> = (0..self.iterations as u64).collect();

        // Perform some CPU-intensive work
        let result: u64 = data.iter().map(|&x| x * x).sum();

        // Store the result
        store.put("cpu_result", result)?;
        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

/// I/O simulation task for benchmarking async overhead
#[derive(Clone)]
struct IoSimulationTask {
    next_state: TestState,
    delay_ms: u64,
}

impl IoSimulationTask {
    fn new(next_state: TestState, delay_ms: u64) -> Self {
        Self {
            next_state,
            delay_ms,
        }
    }
}

#[async_trait]
impl Task<TestState> for IoSimulationTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;

        // Simulate I/O preparation delay
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        let prepared_data = "prepared_data".to_string();

        // Simulate processing delay
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        let processed_data = format!("processed_{prepared_data}");

        // Simulate final I/O delay
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        store.put("io_result", processed_data)?;

        Ok(TaskResult::Single(self.next_state.clone()))
    }
}

/// Task with configurable retry behavior for benchmarking retry overhead
#[derive(Clone)]
struct ConfigurableTask {
    next_state: TestState,
    config: TaskConfig,
    should_fail: bool,
}

impl ConfigurableTask {
    fn new(next_state: TestState, config: TaskConfig, should_fail: bool) -> Self {
        Self {
            next_state,
            config,
            should_fail,
        }
    }
}

#[async_trait]
impl Task<TestState> for ConfigurableTask {
    fn config(&self) -> TaskConfig {
        self.config.clone()
    }

    async fn run(&self, _res: &Resources) -> Result<TaskResult<TestState>, CanoError> {
        if self.should_fail {
            Err(CanoError::TaskExecution("Intentional failure".to_string()))
        } else {
            Ok(TaskResult::Single(self.next_state.clone()))
        }
    }
}

/// Benchmark individual task creation performance
fn bench_task_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_creation");

    let task_counts = vec![1, 10, 100, 1000, 10000];

    for &task_count in &task_counts {
        group.bench_with_input(
            BenchmarkId::new("do_nothing_single", task_count),
            &task_count,
            |b, &task_count| {
                b.iter(|| {
                    for i in 0..task_count {
                        let _task = DoNothingTask::new(TestState::Task(i));
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("do_nothing_batch", task_count),
            &task_count,
            |b, &task_count| {
                b.iter(|| {
                    let _tasks: Vec<DoNothingTask> = (0..task_count)
                        .map(|i| DoNothingTask::new(TestState::Task(i)))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("cpu_intensive_creation", task_count),
            &task_count,
            |b, &task_count| {
                b.iter(|| {
                    let _tasks: Vec<CpuIntensiveTask> = (0..task_count)
                        .map(|i| CpuIntensiveTask::new(TestState::Task(i), 100))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("io_simulation_creation", task_count),
            &task_count,
            |b, &task_count| {
                b.iter(|| {
                    let _tasks: Vec<IoSimulationTask> = (0..task_count)
                        .map(|i| IoSimulationTask::new(TestState::Task(i), 1))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("configurable_creation", task_count),
            &task_count,
            |b, &task_count| {
                b.iter(|| {
                    let _tasks: Vec<ConfigurableTask> = (0..task_count)
                        .map(|i| {
                            ConfigurableTask::new(TestState::Task(i), TaskConfig::minimal(), false)
                        })
                        .collect();
                });
            },
        );
    }

    group.finish();
}

/// Benchmark individual task execution performance
fn bench_task_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_execution");

    let task_counts = vec![1, 10, 100, 1000];

    for &task_count in &task_counts {
        group.bench_with_input(
            BenchmarkId::new("do_nothing_sequential", task_count),
            &task_count,
            |b, &task_count| {
                let tasks: Vec<DoNothingTask> = (0..task_count)
                    .map(|i| DoNothingTask::new(TestState::Task(i)))
                    .collect();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = Resources::new().insert("store", MemoryStore::new());
                        for task in &tasks {
                            let _result = task.run(&res).await;
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("single_task_repeated", task_count),
            &task_count,
            |b, &task_count| {
                let task = DoNothingTask::new(TestState::Complete);

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = Resources::new().insert("store", MemoryStore::new());
                        for _i in 0..task_count {
                            let _result = task.run(&res).await;
                        }
                    });
            },
        );

        // CPU intensive execution (smaller counts for performance)
        if task_count <= 100 {
            group.bench_with_input(
                BenchmarkId::new("cpu_intensive_execution", task_count),
                &task_count,
                |b, &task_count| {
                    let tasks: Vec<CpuIntensiveTask> = (0..task_count)
                        .map(|i| CpuIntensiveTask::new(TestState::Task(i), 100))
                        .collect();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let res = Resources::new().insert("store", MemoryStore::new());
                            for task in &tasks {
                                let _result = task.run(&res).await;
                            }
                        });
                },
            );
        }

        // I/O simulation with very short delays to avoid long benchmark times
        if task_count <= 10 {
            group.bench_with_input(
                BenchmarkId::new("io_simulation_execution", task_count),
                &task_count,
                |b, &task_count| {
                    let tasks: Vec<IoSimulationTask> = (0..task_count)
                        .map(|i| IoSimulationTask::new(TestState::Task(i), 1))
                        .collect();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let res = Resources::new().insert("store", MemoryStore::new());
                            for task in &tasks {
                                let _result = task.run(&res).await;
                            }
                        });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark task memory allocation patterns
fn bench_task_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_memory_patterns");

    let sizes = vec![10, 100, 1000, 10000];

    for &size in &sizes {
        group.bench_with_input(
            BenchmarkId::new("stack_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _tasks: Vec<DoNothingTask> = (0..size)
                        .map(|i| DoNothingTask::new(TestState::Task(i)))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("heap_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _tasks: Vec<Box<DoNothingTask>> = (0..size)
                        .map(|i| Box::new(DoNothingTask::new(TestState::Task(i))))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("arc_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _tasks: Vec<std::sync::Arc<DoNothingTask>> = (0..size)
                        .map(|i| std::sync::Arc::new(DoNothingTask::new(TestState::Task(i))))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("pre_allocated_capacity", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut tasks = Vec::with_capacity(size);
                    for i in 0..size {
                        tasks.push(DoNothingTask::new(TestState::Task(i)));
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("dynamic_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut tasks = Vec::new();
                    for i in 0..size {
                        tasks.push(DoNothingTask::new(TestState::Task(i)));
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark task cloning performance
fn bench_task_cloning(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_cloning");

    let sizes = vec![1, 10, 100, 1000];

    for &size in &sizes {
        group.bench_with_input(
            BenchmarkId::new("clone_single_task", size),
            &size,
            |b, &size| {
                let original_task = DoNothingTask::new(TestState::Complete);

                b.iter(|| {
                    for _i in 0..size {
                        let _cloned = original_task.clone();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("clone_task_vector", size),
            &size,
            |b, &size| {
                let original_tasks: Vec<DoNothingTask> = (0..size)
                    .map(|i| DoNothingTask::new(TestState::Task(i)))
                    .collect();

                b.iter(|| {
                    let _cloned_tasks = original_tasks.clone();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("deep_clone_individual", size),
            &size,
            |b, &size| {
                let original_tasks: Vec<DoNothingTask> = (0..size)
                    .map(|i| DoNothingTask::new(TestState::Task(i)))
                    .collect();

                b.iter(|| {
                    let _cloned_tasks: Vec<DoNothingTask> = original_tasks.to_vec();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("clone_cpu_intensive", size),
            &size,
            |b, &size| {
                let original_task = CpuIntensiveTask::new(TestState::Complete, 100);

                b.iter(|| {
                    for _i in 0..size {
                        let _cloned = original_task.clone();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("clone_configurable_task", size),
            &size,
            |b, &size| {
                let original_task =
                    ConfigurableTask::new(TestState::Complete, TaskConfig::default(), false);

                b.iter(|| {
                    for _i in 0..size {
                        let _cloned = original_task.clone();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark concurrent task execution
fn bench_concurrent_task_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_task_execution");

    let concurrency_levels = vec![1, 2, 4, 8, 16];
    let task_count = 100;

    for &concurrency in &concurrency_levels {
        group.bench_with_input(
            BenchmarkId::new("parallel_do_nothing", concurrency),
            &concurrency,
            |b, &concurrency| {
                let tasks: Vec<std::sync::Arc<DoNothingTask>> = (0..task_count)
                    .map(|i| std::sync::Arc::new(DoNothingTask::new(TestState::Task(i))))
                    .collect();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = std::sync::Arc::new(
                            Resources::new().insert("store", MemoryStore::new()),
                        );
                        let chunk_size = std::cmp::max(1, task_count / concurrency);

                        let handles: Vec<_> = tasks
                            .chunks(chunk_size)
                            .map(|chunk| {
                                let chunk = chunk.to_vec();
                                let res = res.clone();
                                tokio::spawn(async move {
                                    for task in chunk {
                                        let _result = task.run(&*res).await;
                                    }
                                })
                            })
                            .collect();

                        for handle in handles {
                            let _ = handle.await;
                        }
                    });
            },
        );

        // CPU intensive parallel execution (smaller scale)
        if concurrency <= 8 && task_count <= 50 {
            group.bench_with_input(
                BenchmarkId::new("parallel_cpu_intensive", concurrency),
                &concurrency,
                |b, &concurrency| {
                    let small_count = 20; // Smaller for CPU intensive
                    let tasks: Vec<std::sync::Arc<CpuIntensiveTask>> = (0..small_count)
                        .map(|i| std::sync::Arc::new(CpuIntensiveTask::new(TestState::Task(i), 50)))
                        .collect();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let res = std::sync::Arc::new(
                                Resources::new().insert("store", MemoryStore::new()),
                            );
                            let chunk_size = std::cmp::max(1, small_count / concurrency);

                            let handles: Vec<_> = tasks
                                .chunks(chunk_size)
                                .map(|chunk| {
                                    let chunk = chunk.to_vec();
                                    let res = res.clone();
                                    tokio::spawn(async move {
                                        for task in chunk {
                                            let _result = task.run(&*res).await;
                                        }
                                    })
                                })
                                .collect();

                            for handle in handles {
                                let _ = handle.await;
                            }
                        });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark task vs trait object performance
fn bench_task_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_dispatch");

    let sizes = vec![10, 100, 1000];

    for &size in &sizes {
        group.bench_with_input(
            BenchmarkId::new("direct_task_calls", size),
            &size,
            |b, &size| {
                let tasks: Vec<DoNothingTask> = (0..size)
                    .map(|i| DoNothingTask::new(TestState::Task(i)))
                    .collect();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = Resources::new().insert("store", MemoryStore::new());
                        for task in &tasks {
                            let _result = task.run(&res).await;
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("boxed_task_calls", size),
            &size,
            |b, &size| {
                let tasks: Vec<Box<dyn Task<TestState>>> = (0..size)
                    .map(|i| {
                        Box::new(DoNothingTask::new(TestState::Task(i))) as Box<dyn Task<TestState>>
                    })
                    .collect();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = Resources::new().insert("store", MemoryStore::new());
                        for task in &tasks {
                            let _result = task.run(&res).await;
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("mixed_task_types", size),
            &size,
            |b, &size| {
                let mut tasks: Vec<Box<dyn Task<TestState>>> = Vec::new();
                for i in 0..size {
                    if i % 2 == 0 {
                        tasks.push(Box::new(DoNothingTask::new(TestState::Task(i))));
                    } else {
                        tasks.push(Box::new(CpuIntensiveTask::new(TestState::Task(i), 10)));
                    }
                }

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = Resources::new().insert("store", MemoryStore::new());
                        for task in &tasks {
                            let _result = task.run(&res).await;
                        }
                    });
            },
        );
    }

    group.finish();
}

/// Benchmark different task configuration scenarios
fn bench_task_config_scenarios(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_config_scenarios");

    let task_count = 100;

    group.bench_function("minimal_config_tasks", |b| {
        let tasks: Vec<ConfigurableTask> = (0..task_count)
            .map(|i| ConfigurableTask::new(TestState::Task(i), TaskConfig::minimal(), false))
            .collect();

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let res = Resources::new().insert("store", MemoryStore::new());
                for task in &tasks {
                    let _result = task.run(&res).await;
                }
            });
    });

    group.bench_function("default_config_tasks", |b| {
        let tasks: Vec<ConfigurableTask> = (0..task_count)
            .map(|i| ConfigurableTask::new(TestState::Task(i), TaskConfig::default(), false))
            .collect();

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let res = Resources::new().insert("store", MemoryStore::new());
                for task in &tasks {
                    let _result = task.run(&res).await;
                }
            });
    });

    group.bench_function("fixed_retry_config_tasks", |b| {
        let tasks: Vec<ConfigurableTask> = (0..task_count)
            .map(|i| {
                ConfigurableTask::new(
                    TestState::Task(i),
                    TaskConfig::new().with_fixed_retry(3, std::time::Duration::from_millis(10)),
                    false,
                )
            })
            .collect();

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let res = Resources::new().insert("store", MemoryStore::new());
                for task in &tasks {
                    let _result = task.run(&res).await;
                }
            });
    });

    group.bench_function("exponential_retry_config_tasks", |b| {
        let tasks: Vec<ConfigurableTask> = (0..task_count)
            .map(|i| {
                ConfigurableTask::new(
                    TestState::Task(i),
                    TaskConfig::new().with_exponential_retry(3),
                    false,
                )
            })
            .collect();

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let res = Resources::new().insert("store", MemoryStore::new());
                for task in &tasks {
                    let _result = task.run(&res).await;
                }
            });
    });

    group.finish();
}

/// States used exclusively by the run_bare delegation benchmark.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BareState {
    Done,
}

/// Task that overrides `run()` directly — the baseline with no extra call hop.
struct DirectRunTask;

#[async_trait]
impl Task<BareState> for DirectRunTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<BareState>, CanoError> {
        Ok(TaskResult::Single(BareState::Done))
    }
}

/// Task that overrides only `run_bare()` — exercises the default `run()` delegation path.
struct ViaRunBareTask;

#[async_trait]
impl Task<BareState> for ViaRunBareTask {
    async fn run_bare(&self) -> Result<TaskResult<BareState>, CanoError> {
        Ok(TaskResult::Single(BareState::Done))
    }
}

/// Benchmark comparing `run()` called directly versus via the `run_bare()` delegation default.
///
/// Both tasks perform identical trivial work so any timing difference reflects only the
/// extra async call hop introduced by the default `run()` → `run_bare()` delegation.
fn bench_task_run_bare(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_run_bare");

    let iteration_counts = vec![1, 10, 100, 1000];

    for &count in &iteration_counts {
        group.bench_with_input(
            BenchmarkId::new("direct_run", count),
            &count,
            |b, &count| {
                let task = DirectRunTask;

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = Resources::new();
                        for _i in 0..count {
                            let _result = task.run(&res).await;
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("via_run_bare", count),
            &count,
            |b, &count| {
                let task = ViaRunBareTask;

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let res = Resources::new();
                        for _i in 0..count {
                            let _result = task.run(&res).await;
                        }
                    });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_task_creation,
    bench_task_execution,
    bench_task_memory_patterns,
    bench_task_cloning,
    bench_concurrent_task_execution,
    bench_task_dispatch,
    bench_task_config_scenarios,
    bench_task_run_bare
);
criterion_main!(benches);
