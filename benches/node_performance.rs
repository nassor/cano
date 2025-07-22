use async_trait::async_trait;
use cano::store::StoreTrait;
use cano::{CanoError, DefaultParams, MemoryStore, Node};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

/// Simple do-nothing node for benchmarking
#[derive(Clone)]
struct DoNothingNode {
    next_state: TestState,
}

impl DoNothingNode {
    fn new(next_state: TestState) -> Self {
        Self { next_state }
    }
}

/// Test states for node benchmarking
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TestState {
    #[allow(dead_code)]
    Start,
    Node(usize),
    Complete,
}

#[async_trait]
impl Node<TestState> for DoNothingNode {
    type Params = DefaultParams;
    type Storage = MemoryStore;
    type PrepResult = ();
    type ExecResult = ();

    fn set_params(&mut self, _params: Self::Params) {}

    async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
        // Do nothing - minimal overhead
        ()
    }

    async fn post(
        &self,
        _store: &Self::Storage,
        _exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        Ok(self.next_state.clone())
    }
}

/// CPU-intensive node for benchmarking computational overhead
#[derive(Clone)]
struct CpuIntensiveNode {
    next_state: TestState,
    iterations: usize,
}

impl CpuIntensiveNode {
    fn new(next_state: TestState, iterations: usize) -> Self {
        Self {
            next_state,
            iterations,
        }
    }
}

#[async_trait]
impl Node<TestState> for CpuIntensiveNode {
    type Params = DefaultParams;
    type Storage = MemoryStore;
    type PrepResult = Vec<u64>;
    type ExecResult = u64;

    fn set_params(&mut self, _params: Self::Params) {}

    async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
        // Generate some data to process
        Ok((0..self.iterations as u64).collect())
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        // Perform some CPU-intensive work
        prep_res.iter().map(|&x| x * x).sum()
    }

    async fn post(
        &self,
        store: &Self::Storage,
        exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        // Store the result
        store.put("cpu_result", exec_res)?;
        Ok(self.next_state.clone())
    }
}

/// I/O simulation node for benchmarking async overhead
#[derive(Clone)]
struct IoSimulationNode {
    next_state: TestState,
    delay_ms: u64,
}

impl IoSimulationNode {
    fn new(next_state: TestState, delay_ms: u64) -> Self {
        Self {
            next_state,
            delay_ms,
        }
    }
}

#[async_trait]
impl Node<TestState> for IoSimulationNode {
    type Params = DefaultParams;
    type Storage = MemoryStore;
    type PrepResult = String;
    type ExecResult = String;

    fn set_params(&mut self, _params: Self::Params) {}

    async fn prep(&self, _store: &Self::Storage) -> Result<Self::PrepResult, CanoError> {
        // Simulate I/O delay
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        Ok("prepared_data".to_string())
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        // Simulate processing
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        format!("processed_{}", prep_res)
    }

    async fn post(
        &self,
        store: &Self::Storage,
        exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        // Simulate I/O delay
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        store.put("io_result", exec_res)?;
        Ok(self.next_state.clone())
    }
}

/// Benchmark individual node creation performance
fn bench_node_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_creation");

    let node_counts = vec![1, 10, 100, 1000, 10000];

    for &node_count in &node_counts {
        group.bench_with_input(
            BenchmarkId::new("do_nothing_single", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    for i in 0..node_count {
                        let _node = DoNothingNode::new(TestState::Node(i));
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("do_nothing_batch", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    let _nodes: Vec<DoNothingNode> = (0..node_count)
                        .map(|i| DoNothingNode::new(TestState::Node(i)))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("cpu_intensive_creation", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    let _nodes: Vec<CpuIntensiveNode> = (0..node_count)
                        .map(|i| CpuIntensiveNode::new(TestState::Node(i), 100))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("io_simulation_creation", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    let _nodes: Vec<IoSimulationNode> = (0..node_count)
                        .map(|i| IoSimulationNode::new(TestState::Node(i), 1))
                        .collect();
                });
            },
        );
    }

    group.finish();
}

/// Benchmark individual node execution performance
fn bench_node_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_execution");

    let node_counts = vec![1, 10, 100, 1000];

    for &node_count in &node_counts {
        group.bench_with_input(
            BenchmarkId::new("do_nothing_sequential", node_count),
            &node_count,
            |b, &node_count| {
                let nodes: Vec<DoNothingNode> = (0..node_count)
                    .map(|i| DoNothingNode::new(TestState::Node(i)))
                    .collect();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let storage = MemoryStore::new();
                        for node in &nodes {
                            let _result = node.run(&storage).await;
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("single_node_repeated", node_count),
            &node_count,
            |b, &node_count| {
                let node = DoNothingNode::new(TestState::Complete);

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let storage = MemoryStore::new();
                        for _i in 0..node_count {
                            let _result = node.run(&storage).await;
                        }
                    });
            },
        );

        // CPU intensive execution (smaller counts for performance)
        if node_count <= 100 {
            group.bench_with_input(
                BenchmarkId::new("cpu_intensive_execution", node_count),
                &node_count,
                |b, &node_count| {
                    let nodes: Vec<CpuIntensiveNode> = (0..node_count)
                        .map(|i| CpuIntensiveNode::new(TestState::Node(i), 100))
                        .collect();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let storage = MemoryStore::new();
                            for node in &nodes {
                                let _result = node.run(&storage).await;
                            }
                        });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark individual node phases
fn bench_node_phases(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_phases");

    let iteration_counts = vec![100, 1000, 10000];

    for &iterations in &iteration_counts {
        group.bench_with_input(
            BenchmarkId::new("prep_phase_only", iterations),
            &iterations,
            |b, &iterations| {
                let node = DoNothingNode::new(TestState::Complete);

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let storage = MemoryStore::new();
                        for _i in 0..iterations {
                            let _result = node.prep(&storage).await;
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("exec_phase_only", iterations),
            &iterations,
            |b, &iterations| {
                let node = DoNothingNode::new(TestState::Complete);

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        for _i in 0..iterations {
                            let _result = node.exec(()).await;
                        }
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("post_phase_only", iterations),
            &iterations,
            |b, &iterations| {
                let node = DoNothingNode::new(TestState::Complete);

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let storage = MemoryStore::new();
                        for _i in 0..iterations {
                            let _result = node.post(&storage, ()).await;
                        }
                    });
            },
        );

        // CPU intensive phases (smaller iterations)
        if iterations <= 1000 {
            group.bench_with_input(
                BenchmarkId::new("cpu_prep_phase", iterations),
                &iterations,
                |b, &iterations| {
                    let node = CpuIntensiveNode::new(TestState::Complete, 50);

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let storage = MemoryStore::new();
                            for _i in 0..iterations {
                                let _result = node.prep(&storage).await;
                            }
                        });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("cpu_exec_phase", iterations),
                &iterations,
                |b, &iterations| {
                    let node = CpuIntensiveNode::new(TestState::Complete, 50);
                    let prep_data: Vec<u64> = (0..50).collect();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            for _i in 0..iterations {
                                let _result = node.exec(prep_data.clone()).await;
                            }
                        });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark node memory allocation patterns
fn bench_node_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_memory_patterns");

    let sizes = vec![10, 100, 1000, 10000];

    for &size in &sizes {
        group.bench_with_input(
            BenchmarkId::new("stack_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _nodes: Vec<DoNothingNode> = (0..size)
                        .map(|i| DoNothingNode::new(TestState::Node(i)))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("heap_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _nodes: Vec<Box<DoNothingNode>> = (0..size)
                        .map(|i| Box::new(DoNothingNode::new(TestState::Node(i))))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("arc_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let _nodes: Vec<std::sync::Arc<DoNothingNode>> = (0..size)
                        .map(|i| std::sync::Arc::new(DoNothingNode::new(TestState::Node(i))))
                        .collect();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("pre_allocated_capacity", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut nodes = Vec::with_capacity(size);
                    for i in 0..size {
                        nodes.push(DoNothingNode::new(TestState::Node(i)));
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("dynamic_allocation", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut nodes = Vec::new();
                    for i in 0..size {
                        nodes.push(DoNothingNode::new(TestState::Node(i)));
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark node cloning performance
fn bench_node_cloning(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_cloning");

    let sizes = vec![1, 10, 100, 1000];

    for &size in &sizes {
        group.bench_with_input(
            BenchmarkId::new("clone_single_node", size),
            &size,
            |b, &size| {
                let original_node = DoNothingNode::new(TestState::Complete);

                b.iter(|| {
                    for _i in 0..size {
                        let _cloned = original_node.clone();
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("clone_node_vector", size),
            &size,
            |b, &size| {
                let original_nodes: Vec<DoNothingNode> = (0..size)
                    .map(|i| DoNothingNode::new(TestState::Node(i)))
                    .collect();

                b.iter(|| {
                    let _cloned_nodes = original_nodes.clone();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("deep_clone_individual", size),
            &size,
            |b, &size| {
                let original_nodes: Vec<DoNothingNode> = (0..size)
                    .map(|i| DoNothingNode::new(TestState::Node(i)))
                    .collect();

                b.iter(|| {
                    let _cloned_nodes: Vec<DoNothingNode> = original_nodes.to_vec();
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("clone_cpu_intensive", size),
            &size,
            |b, &size| {
                let original_node = CpuIntensiveNode::new(TestState::Complete, 100);

                b.iter(|| {
                    for _i in 0..size {
                        let _cloned = original_node.clone();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark concurrent node execution
fn bench_concurrent_node_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_node_execution");

    let concurrency_levels = vec![1, 2, 4, 8, 16];
    let node_count = 100;

    for &concurrency in &concurrency_levels {
        group.bench_with_input(
            BenchmarkId::new("parallel_do_nothing", concurrency),
            &concurrency,
            |b, &concurrency| {
                let nodes: Vec<std::sync::Arc<DoNothingNode>> = (0..node_count)
                    .map(|i| std::sync::Arc::new(DoNothingNode::new(TestState::Node(i))))
                    .collect();

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let storage = std::sync::Arc::new(MemoryStore::new());
                        let chunk_size = std::cmp::max(1, node_count / concurrency);

                        let tasks: Vec<_> = nodes
                            .chunks(chunk_size)
                            .map(|chunk| {
                                let chunk = chunk.to_vec();
                                let storage = storage.clone();
                                tokio::spawn(async move {
                                    for node in chunk {
                                        let _result = node.run(&*storage).await;
                                    }
                                })
                            })
                            .collect();

                        for task in tasks {
                            let _ = task.await;
                        }
                    });
            },
        );

        // CPU intensive parallel execution (smaller scale)
        if concurrency <= 8 && node_count <= 50 {
            group.bench_with_input(
                BenchmarkId::new("parallel_cpu_intensive", concurrency),
                &concurrency,
                |b, &concurrency| {
                    let small_count = 20; // Smaller for CPU intensive
                    let nodes: Vec<std::sync::Arc<CpuIntensiveNode>> = (0..small_count)
                        .map(|i| std::sync::Arc::new(CpuIntensiveNode::new(TestState::Node(i), 50)))
                        .collect();

                    b.to_async(tokio::runtime::Runtime::new().unwrap())
                        .iter(|| async {
                            let storage = std::sync::Arc::new(MemoryStore::new());
                            let chunk_size = std::cmp::max(1, small_count / concurrency);

                            let tasks: Vec<_> = nodes
                                .chunks(chunk_size)
                                .map(|chunk| {
                                    let chunk = chunk.to_vec();
                                    let storage = storage.clone();
                                    tokio::spawn(async move {
                                        for node in chunk {
                                            let _result = node.run(&*storage).await;
                                        }
                                    })
                                })
                                .collect();

                            for task in tasks {
                                let _ = task.await;
                            }
                        });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark node configuration and parameter setting
fn bench_node_configuration(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_configuration");

    let sizes = vec![1, 10, 100, 1000];

    for &size in &sizes {
        group.bench_with_input(
            BenchmarkId::new("parameter_setting", size),
            &size,
            |b, &size| {
                let mut nodes: Vec<DoNothingNode> = (0..size)
                    .map(|i| DoNothingNode::new(TestState::Node(i)))
                    .collect();

                let params = DefaultParams::new();

                b.iter(|| {
                    for node in &mut nodes {
                        node.set_params(params.clone());
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("node_config_creation", size),
            &size,
            |b, &size| {
                let nodes: Vec<DoNothingNode> = (0..size)
                    .map(|i| DoNothingNode::new(TestState::Node(i)))
                    .collect();

                b.iter(|| {
                    for node in &nodes {
                        let _config = node.config();
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_node_creation,
    bench_node_execution,
    bench_node_phases,
    bench_node_memory_patterns,
    bench_node_cloning,
    bench_concurrent_node_execution,
    bench_node_configuration
);
criterion_main!(benches);
