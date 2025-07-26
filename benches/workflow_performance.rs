//! # Workflow Performance Benchmarks
//!
//! This benchmark suite tests both sequential and concurrent workflow execution performance:
//!
//! ## Sequential Benchmarks
//! - `bench_flow_performance`: Tests workflow execution with different node counts
//! - `bench_flow_vs_direct_execution`: Compares workflow overhead vs direct node execution
//! - `bench_memory_patterns`: Tests memory allocation patterns for different workflow sizes
//!
//! ## Concurrent Benchmarks
//! - `bench_concurrent_workflow_performance`: Tests different concurrent strategies (WaitForever, WaitForQuota, WaitDuration)
//! - `bench_concurrent_vs_sequential`: Direct comparison between concurrent and sequential execution
//! - `bench_concurrent_scalability`: Tests performance scaling with workflow complexity and count
//! - `bench_concurrent_memory_patterns`: Tests memory usage patterns for concurrent workflows
//!
//! ## Usage
//! Run with: `cargo bench --bench workflow_performance`
//! Or specific benchmark: `cargo bench --bench workflow_performance concurrent_vs_sequential`

use async_trait::async_trait;
use cano::{
    CanoError, ConcurrentStrategy, ConcurrentWorkflow, ConcurrentWorkflowInstance, KeyValueStore,
    MemoryStore, Node, Workflow,
};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::time::Duration;

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

/// I/O simulation node for benchmarking concurrent performance
#[derive(Clone)]
struct IoSimulationNode {
    next_state: TestState,
    io_duration: Duration,
}

impl IoSimulationNode {
    fn new(next_state: TestState, io_duration: Duration) -> Self {
        Self {
            next_state,
            io_duration,
        }
    }
}

#[async_trait]
impl Node<TestState> for IoSimulationNode {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        // Simulate I/O preparation (e.g., database connection, file opening)
        tokio::time::sleep(self.io_duration / 4).await;
        Ok("io_prepared".to_string())
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        // Simulate main I/O operation (e.g., network request, file read)
        tokio::time::sleep(self.io_duration).await;
        format!("processed_{}", prep_res)
    }

    async fn post(
        &self,
        _store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        // Simulate I/O cleanup (e.g., connection close, cache write)
        tokio::time::sleep(self.io_duration / 8).await;
        // Store result for potential next nodes
        _store.put("io_result", exec_res)?;
        Ok(self.next_state.clone())
    }
}

/// Test states for workflow benchmarking
/// Using a simple enum with node IDs for scalability
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TestState {
    Start,
    Node(usize),
    Complete,
}

#[async_trait]
impl Node<TestState> for DoNothingNode {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
        // Do nothing - minimal overhead
        ()
    }

    async fn post(
        &self,
        _store: &MemoryStore,
        _exec_res: Self::ExecResult,
    ) -> Result<TestState, CanoError> {
        Ok(self.next_state.clone())
    }
}

/// Create a workflow with a specified number of nodes
fn create_flow(node_count: usize) -> Workflow<TestState> {
    let mut workflow: Workflow<TestState> = Workflow::new(TestState::Start);

    if node_count == 0 {
        workflow.add_exit_state(TestState::Start);
        return workflow;
    }

    // Add the sequential chain of nodes, starting from Start state
    for i in 0..node_count {
        let current_state = if i == 0 {
            TestState::Start
        } else {
            TestState::Node(i - 1)
        };
        let next_state = if i == node_count - 1 {
            TestState::Complete
        } else {
            TestState::Node(i)
        };

        workflow.register_node(current_state, DoNothingNode::new(next_state));
    }

    // Set the Complete state as exit state
    workflow.add_exit_state(TestState::Complete);

    workflow
}

fn bench_flow_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow_performance");

    // Test different workflow sizes
    let node_counts = vec![10, 100, 1000, 10000];

    for &node_count in &node_counts {
        group.bench_with_input(
            BenchmarkId::new("sequential_execution", node_count),
            &node_count,
            |b, &node_count| {
                let workflow = create_flow(node_count);

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let result = workflow.orchestrate(&store).await;
                        assert!(result.is_ok());
                        assert_eq!(result.unwrap(), TestState::Complete);
                    });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("flow_creation", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    let workflow = create_flow(node_count);
                    // Just to ensure the workflow is created properly
                    assert_eq!(workflow.state_nodes.len(), node_count);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark workflow execution overhead vs direct node execution
fn bench_flow_vs_direct_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow_vs_direct");

    let node_count = 100;

    group.bench_function("flow_execution_100_nodes", |b| {
        let workflow = create_flow(node_count);

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let store = MemoryStore::new();
                let result = workflow.orchestrate(&store).await;
                assert!(result.is_ok());
            });
    });

    group.bench_function("direct_node_execution_100_nodes", |b| {
        let nodes: Vec<DoNothingNode> = (0..node_count)
            .map(|_i| DoNothingNode::new(TestState::Complete))
            .collect();

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let store = MemoryStore::new();
                for node in &nodes {
                    let result = node.run(&store).await;
                    assert!(result.is_ok());
                }
            });
    });

    group.finish();
}

/// Benchmark memory allocation patterns for different workflow sizes
fn bench_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    for &node_count in &[10, 100, 1000] {
        group.bench_with_input(
            BenchmarkId::new("heap_allocation", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    // Create workflow on heap
                    let workflow = Box::new(create_flow(node_count));
                    assert_eq!(workflow.state_nodes.len(), node_count);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("stack_creation", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    // Create workflow on stack (smaller sizes only)
                    if node_count <= 100 {
                        let workflow = create_flow(node_count);
                        assert_eq!(workflow.state_nodes.len(), node_count);
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark concurrent workflow execution with different strategies
fn bench_concurrent_workflow_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_workflow_performance");

    // Test different numbers of concurrent workflows
    let workflow_counts = vec![2, 5, 10, 20, 50];
    let node_count_per_workflow = 10;

    for &workflow_count in &workflow_counts {
        // Benchmark WaitForever strategy
        group.bench_with_input(
            BenchmarkId::new("wait_forever", workflow_count),
            &workflow_count,
            |b, &workflow_count| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..workflow_count)
                            .map(|i| {
                                ConcurrentWorkflowInstance::new(
                                    format!("workflow_{i}"),
                                    create_flow(node_count_per_workflow),
                                )
                            })
                            .collect();

                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        let results = result.unwrap();
                        assert_eq!(results.completed, workflow_count);
                    });
            },
        );

        // Benchmark WaitForQuota strategy (wait for half)
        let quota = (workflow_count / 2).max(1);
        group.bench_with_input(
            BenchmarkId::new("wait_for_quota", format!("{workflow_count}_quota_{quota}")),
            &workflow_count,
            |b, &workflow_count| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..workflow_count)
                            .map(|i| {
                                ConcurrentWorkflowInstance::new(
                                    format!("workflow_{i}"),
                                    create_flow(node_count_per_workflow),
                                )
                            })
                            .collect();

                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitForQuota(quota));
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        let results = result.unwrap();
                        assert_eq!(results.completed, quota);
                    });
            },
        );

        // Benchmark WaitDuration strategy (generous timeout)
        group.bench_with_input(
            BenchmarkId::new("wait_duration", workflow_count),
            &workflow_count,
            |b, &workflow_count| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..workflow_count)
                            .map(|i| {
                                ConcurrentWorkflowInstance::new(
                                    format!("workflow_{i}"),
                                    create_flow(node_count_per_workflow),
                                )
                            })
                            .collect();

                        let timeout = Duration::from_secs(10); // Generous timeout for benchmarking
                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitDuration(timeout));
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        // All should complete within the generous timeout
                    });
            },
        );
    }

    group.finish();
}

/// Benchmark concurrent vs sequential execution
fn bench_concurrent_vs_sequential(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_vs_sequential");

    let workflow_count = 10;
    let node_count_per_workflow = 20;

    // Sequential execution (one after another)
    group.bench_function("sequential_workflows", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let store = MemoryStore::new();
                for _i in 0..workflow_count {
                    let workflow = create_flow(node_count_per_workflow);
                    let result = workflow.orchestrate(&store).await;
                    assert!(result.is_ok());
                }
            });
    });

    // Concurrent execution (all at once)
    group.bench_function("concurrent_workflows", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let store = MemoryStore::new();
                let instances: Vec<_> = (0..workflow_count)
                    .map(|i| {
                        ConcurrentWorkflowInstance::new(
                            format!("workflow_{i}"),
                            create_flow(node_count_per_workflow),
                        )
                    })
                    .collect();

                let mut concurrent_workflow =
                    ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                concurrent_workflow.add_instances(instances);

                let result = concurrent_workflow.orchestrate(store).await;
                assert!(result.is_ok());
                let results = result.unwrap();
                assert_eq!(results.completed, workflow_count);
            });
    });

    group.finish();
}

/// Benchmark concurrent workflow scalability
fn bench_concurrent_scalability(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_scalability");

    // Test how performance scales with different workflow complexities
    let node_counts = vec![5, 10, 50, 100];
    let concurrent_workflows = 10;

    for &node_count in &node_counts {
        group.bench_with_input(
            BenchmarkId::new(
                "complexity_scaling",
                format!("{node_count}_nodes_per_workflow"),
            ),
            &node_count,
            |b, &node_count| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..concurrent_workflows)
                            .map(|i| {
                                ConcurrentWorkflowInstance::new(
                                    format!("workflow_{i}"),
                                    create_flow(node_count),
                                )
                            })
                            .collect();

                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        let results = result.unwrap();
                        assert_eq!(results.completed, concurrent_workflows);
                    });
            },
        );
    }

    // Test how performance scales with different numbers of concurrent workflows
    let workflow_counts = vec![1, 5, 10, 25, 50, 100];
    let nodes_per_workflow = 10;

    for &workflow_count in &workflow_counts {
        group.bench_with_input(
            BenchmarkId::new(
                "workflow_count_scaling",
                format!("{workflow_count}_concurrent_workflows"),
            ),
            &workflow_count,
            |b, &workflow_count| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..workflow_count)
                            .map(|i| {
                                ConcurrentWorkflowInstance::new(
                                    format!("workflow_{i}"),
                                    create_flow(nodes_per_workflow),
                                )
                            })
                            .collect();

                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        let results = result.unwrap();
                        assert_eq!(results.completed, workflow_count);
                    });
            },
        );
    }

    group.finish();
}

/// Benchmark memory usage of concurrent workflows
fn bench_concurrent_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_memory_patterns");

    let workflow_counts = vec![5, 10, 20, 50];

    for &workflow_count in &workflow_counts {
        // Benchmark creating concurrent workflow instances
        group.bench_with_input(
            BenchmarkId::new("instance_creation", workflow_count),
            &workflow_count,
            |b, &workflow_count| {
                b.iter(|| {
                    let instances: Vec<_> = (0..workflow_count)
                        .map(|i| {
                            ConcurrentWorkflowInstance::new(
                                format!("workflow_{i}"),
                                create_flow(10),
                            )
                        })
                        .collect();

                    assert_eq!(instances.len(), workflow_count);
                });
            },
        );

        // Benchmark concurrent workflow setup overhead
        group.bench_with_input(
            BenchmarkId::new("setup_overhead", workflow_count),
            &workflow_count,
            |b, &workflow_count| {
                b.iter(|| {
                    let instances: Vec<_> = (0..workflow_count)
                        .map(|i| {
                            ConcurrentWorkflowInstance::new(
                                format!("workflow_{i}"),
                                create_flow(10),
                            )
                        })
                        .collect();

                    let mut concurrent_workflow =
                        ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                    concurrent_workflow.add_instances(instances);

                    // Just setup, no execution
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks I/O-bound vs CPU-bound concurrent workflow performance
fn bench_io_vs_cpu_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("io_vs_cpu_concurrent");

    // Test with different I/O durations
    for io_duration_ms in [1, 5, 10, 25, 50] {
        let io_duration = Duration::from_millis(io_duration_ms);

        group.bench_with_input(
            BenchmarkId::new("cpu_bound", format!("{io_duration_ms}ms")),
            &io_duration_ms,
            |b, _| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..5)
                            .map(|i| {
                                ConcurrentWorkflowInstance::new(
                                    format!("cpu_workflow_{i}"),
                                    create_flow(3), // Simple CPU-bound workflow
                                )
                            })
                            .collect();

                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        let results = result.unwrap();
                        assert_eq!(results.completed, 5);
                    })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("io_bound", format!("{io_duration_ms}ms")),
            &io_duration_ms,
            |b, _| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..5)
                            .map(|i| {
                                let mut workflow = Workflow::new(TestState::Start);
                                workflow.register_node(
                                    TestState::Start,
                                    IoSimulationNode::new(TestState::Complete, io_duration),
                                );
                                workflow.add_exit_state(TestState::Complete);
                                ConcurrentWorkflowInstance::new(
                                    format!("io_workflow_{i}"),
                                    workflow,
                                )
                            })
                            .collect();

                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        let results = result.unwrap();
                        assert_eq!(results.completed, 5);
                    })
            },
        );
    }

    group.finish();
}

/// Benchmarks concurrent I/O scalability vs sequential execution
fn bench_concurrent_io_scalability(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_io_scalability");

    let io_duration = Duration::from_millis(10); // 10ms I/O operations

    // Test with different numbers of concurrent I/O workflows
    for workflow_count in [1, 5, 10, 20, 50] {
        group.bench_with_input(
            BenchmarkId::new("concurrent", workflow_count),
            &workflow_count,
            |b, &count| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let instances: Vec<_> = (0..count)
                            .map(|i| {
                                let mut workflow = Workflow::new(TestState::Start);
                                workflow.register_node(
                                    TestState::Start,
                                    IoSimulationNode::new(TestState::Complete, io_duration),
                                );
                                workflow.add_exit_state(TestState::Complete);
                                ConcurrentWorkflowInstance::new(
                                    format!("io_workflow_{i}"),
                                    workflow,
                                )
                            })
                            .collect();

                        let mut concurrent_workflow =
                            ConcurrentWorkflow::new(ConcurrentStrategy::WaitForever);
                        concurrent_workflow.add_instances(instances);

                        let result = concurrent_workflow.orchestrate(store).await;
                        assert!(result.is_ok());
                        let results = result.unwrap();
                        assert_eq!(results.completed, count);
                    })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("sequential", workflow_count),
            &workflow_count,
            |b, &count| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let store = MemoryStore::new();
                        let mut workflow = Workflow::new(TestState::Start);
                        workflow.register_node(
                            TestState::Start,
                            IoSimulationNode::new(TestState::Complete, io_duration),
                        );
                        workflow.add_exit_state(TestState::Complete);

                        for _i in 0..count {
                            let result = workflow.orchestrate(&store).await;
                            assert!(result.is_ok());
                            assert_eq!(result.unwrap(), TestState::Complete);
                        }
                    })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_flow_performance,
    bench_flow_vs_direct_execution,
    bench_memory_patterns,
    bench_concurrent_workflow_performance,
    bench_concurrent_vs_sequential,
    bench_concurrent_scalability,
    bench_concurrent_memory_patterns,
    bench_io_vs_cpu_concurrent,
    bench_concurrent_io_scalability
);
criterion_main!(benches);
