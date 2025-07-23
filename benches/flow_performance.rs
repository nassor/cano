use async_trait::async_trait;
use cano::{CanoError, Flow, MemoryStore, Node};
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

/// Test states for workflow benchmarking
/// Using a simple enum with node IDs for scalability
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TestState {
    Start,
    Node(usize),
    Complete,
}

impl TestState {
    /// Generate a state for a given node number
    fn node(n: usize) -> Self {
        if n == 0 { Self::Start } else { Self::Node(n) }
    }
}

#[async_trait]
impl Node<TestState> for DoNothingNode {
    type Storage = MemoryStore;
    type PrepResult = ();
    type ExecResult = ();

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

/// Create a flow with a specified number of nodes
fn create_flow(node_count: usize) -> Flow<TestState, MemoryStore> {
    let mut flow = Flow::new(TestState::Start);

    // Add the sequential chain of nodes
    for i in 0..node_count {
        let current_state = TestState::node(i);
        let next_state = if i == node_count - 1 {
            TestState::Complete
        } else {
            TestState::node(i + 1)
        };

        flow.register_node(current_state, DoNothingNode::new(next_state));
    }

    // Set the Complete state as exit state
    flow.add_exit_state(TestState::Complete);

    flow
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
                let flow = create_flow(node_count);

                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async {
                        let storage = MemoryStore::new();
                        let result = flow.orchestrate(&storage).await;
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
                    let flow = create_flow(node_count);
                    // Just to ensure the flow is created properly
                    assert_eq!(flow.state_nodes.len(), node_count);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark flow execution overhead vs direct node execution
fn bench_flow_vs_direct_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow_vs_direct");

    let node_count = 100;

    group.bench_function("flow_execution_100_nodes", |b| {
        let flow = create_flow(node_count);

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let storage = MemoryStore::new();
                let result = flow.orchestrate(&storage).await;
                assert!(result.is_ok());
            });
    });

    group.bench_function("direct_node_execution_100_nodes", |b| {
        let nodes: Vec<DoNothingNode> = (0..node_count)
            .map(|_i| DoNothingNode::new(TestState::Complete))
            .collect();

        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let storage = MemoryStore::new();
                for node in &nodes {
                    let result = node.run(&storage).await;
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
                    // Create flow on heap
                    let flow = Box::new(create_flow(node_count));
                    assert_eq!(flow.state_nodes.len(), node_count);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("stack_creation", node_count),
            &node_count,
            |b, &node_count| {
                b.iter(|| {
                    // Create flow on stack (smaller sizes only)
                    if node_count <= 100 {
                        let flow = create_flow(node_count);
                        assert_eq!(flow.state_nodes.len(), node_count);
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_flow_performance,
    bench_flow_vs_direct_execution,
    bench_memory_patterns
);
criterion_main!(benches);
