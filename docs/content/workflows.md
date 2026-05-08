+++
title = "Workflows"
description = "Build state-machine workflows in Cano with split/join, parallel processing, and pluggable join strategies."
template = "page.html"
+++

<div class="content-wrapper">

<h1>Workflows</h1>
<p class="subtitle">Orchestrate complex processes with state machine semantics.</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#defining-states">Defining States</a></li>
<li><a href="#building-a-workflow">Building a Workflow</a></li>
<li><a href="#builder-pattern">Builder Pattern</a></li>
<li><a href="#validation">Workflow Validation</a></li>
<li><a href="#error-handling">Error Handling</a></li>
<li><a href="#split-join">Split/Join Workflows</a></li>
<li class="toc-sub"><a href="#join-strategies">Join Strategies</a></li>
<li class="toc-sub"><a href="#complete-example">Complete Example</a></li>
<li><a href="#join-strategy-examples">Join Strategy Examples</a></li>
<li><a href="#comparison-table">Comparison Table</a></li>
<li><a href="#parallel-patterns">Common Parallel Patterns</a></li>
<li><a href="#workflow-on-request">Common: Workflow per HTTP Request</a></li>
<li><a href="#ad-exchange">Advanced: Ad Exchange</a></li>
</ol>
</nav>

<p>
Workflows in Cano are state machines. You define a set of states (usually an enum) and register a <code>Task</code> or <code>Node</code> for each state. 
The workflow engine manages the transitions between these states until an exit state is reached.
</p>
<hr class="section-divider">

<h2 id="defining-states"><a href="#defining-states" class="anchor-link" aria-hidden="true">#</a>Defining States</h2>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OrderState {
    Start,
    Validate,
    Process,
    Complete,
    Failed,
}

```
<hr class="section-divider">

<h2 id="building-a-workflow"><a href="#building-a-workflow" class="anchor-link" aria-hidden="true">#</a>Building a Workflow</h2>
<p>
A workflow is constructed with a <a href="../resources/"><code>Resources</code></a> dictionary
that its tasks can look up during execution.
Use <code>Workflow::new(resources)</code> when tasks need shared state (such as a <code>MemoryStore</code>,
configuration, or clients), or <code>Workflow::bare()</code> when every task is self-contained and uses
<code>run_bare()</code>. <code>Workflow::bare()</code> is equivalent to
<code>Workflow::new(Resources::empty())</code>.
</p>
<div class="diagram-frame">
<p class="diagram-label">Workflow State Transitions</p>
<div class="mermaid">
graph TD
A[Start] --> B[Validate]
B -->|Valid| C[Process]
B -->|Invalid| D[Failed]
C --> E[Complete]
</div>
</div>

<h3 id="linear-example"><a href="#linear-example" class="anchor-link" aria-hidden="true">#</a>Linear Workflow Example</h3>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OrderState {
    Start,
    Validate,
    Process,
    Complete,
    Failed,
}

// Define simple tasks (full impls covered on the Tasks page)
#[derive(Clone)]
struct StartTask;
#[derive(Clone)]
struct ValidateTask;
#[derive(Clone)]
struct ProcessTask;

#[task(state = OrderState)]
impl StartTask {
    async fn run_bare(&self) -> Result<TaskResult<OrderState>, CanoError> {
        Ok(TaskResult::Single(OrderState::Validate))
    }
}

#[task(state = OrderState)]
impl ValidateTask {
    async fn run_bare(&self) -> Result<TaskResult<OrderState>, CanoError> {
        Ok(TaskResult::Single(OrderState::Process))
    }
}

#[task(state = OrderState)]
impl ProcessTask {
    async fn run_bare(&self) -> Result<TaskResult<OrderState>, CanoError> {
        Ok(TaskResult::Single(OrderState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Build workflow using the builder pattern — each method consumes self
    // and returns a new Workflow, so you must capture the return value.
    // For workflows that need no shared resources at all, use `Workflow::bare()`.
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        // 1. Register Tasks for each state
        .register(OrderState::Start, StartTask)
        .register(OrderState::Validate, ValidateTask)
        .register(OrderState::Process, ProcessTask)
        // 2. Define Exit States (Workflow stops here)
        .add_exit_states(vec![OrderState::Complete, OrderState::Failed]);

    // 3. Execute
    let result = workflow.orchestrate(OrderState::Start).await?;

    println!("Final State: {:?}", result);
    Ok(())
}

```
<hr class="section-divider">

<h2 id="builder-pattern"><a href="#builder-pattern" class="anchor-link" aria-hidden="true">#</a>Builder Pattern and #[must_use]</h2>
<p>
Workflow uses a builder pattern where <code>register()</code>, <code>register_split()</code>, and
<code>add_exit_state()</code> all consume <code>self</code> and return a new <code>Workflow</code>.
The <code>#[must_use]</code> attribute on <code>Workflow</code> and <code>JoinConfig</code> means the compiler
will warn you if you discard the return value. If you forget to capture it, the registration is silently lost.
</p>

<div class="callout callout-warning">
<div class="callout-label">Warning: Do not discard the return value</div>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[derive(Clone)]
struct MyTask;

#[task(state = State)]
impl MyTask {
    async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> {
        Ok(TaskResult::Single(State::Complete))
    }
}

fn examples(store: MemoryStore) {
    let my_task = MyTask;

    // WRONG — registration is lost!
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()));
    workflow.register(State::Start, my_task.clone());  // returns a new Workflow, but it is discarded

    // CORRECT — capture the returned workflow
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()));
    let _workflow = workflow.register(State::Start, my_task.clone());

    // BEST — chain calls in a single expression
    let _workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, my_task)
        .add_exit_state(State::Complete);
}
```
</div>
<hr class="section-divider">

<h2 id="validation"><a href="#validation" class="anchor-link" aria-hidden="true">#</a>Workflow Validation</h2>
<p>
Before orchestrating a workflow, you can validate its configuration to catch common mistakes early.
Cano provides two validation methods that check for different categories of problems.
</p>

<h3 id="validate-method"><a href="#validate-method" class="anchor-link" aria-hidden="true">#</a>validate()</h3>
<p>
Checks the overall workflow structure. Returns <code>CanoError::Configuration</code> if problems are found.
</p>
<div class="card-stack">
<div class="card">
<h3>Checks performed</h3>
<p>No handlers registered — the workflow has no states mapped to tasks.</p>
<p>No exit states defined — the workflow has no way to terminate.</p>
</div>
</div>

<h3 id="validate-initial-state"><a href="#validate-initial-state" class="anchor-link" aria-hidden="true">#</a>validate_initial_state()</h3>
<p>
Checks that a specific initial state has a handler registered. Returns <code>CanoError::Configuration</code>
if the given state has no registered task or split handler.
</p>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Process, Complete }

#[derive(Clone)]
struct MyTask;
#[derive(Clone)]
struct ProcessTask;

#[task(state = State)]
impl MyTask {
    async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> {
        Ok(TaskResult::Single(State::Process))
    }
}

#[task(state = State)]
impl ProcessTask {
    async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> {
        Ok(TaskResult::Single(State::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(State::Start, MyTask)
        .register(State::Process, ProcessTask)
        .add_exit_state(State::Complete);

    // Validate structure: ensures handlers and exit states exist
    workflow.validate()?;

    // Validate that the initial state has a handler
    workflow.validate_initial_state(&State::Start)?;

    // Safe to orchestrate
    let _result = workflow.orchestrate(State::Start).await?;
    Ok(())
}
```
<hr class="section-divider">

<h2 id="error-handling"><a href="#error-handling" class="anchor-link" aria-hidden="true">#</a>Error Handling</h2>
<p>
The <code>orchestrate()</code> method can return several error variants depending on what goes wrong
during execution. Understanding these errors helps you build robust error recovery logic.
</p>

<table class="styled-table">
<thead>
<tr>
<th>Error Variant</th>
<th>Condition</th>
<th>How to Fix</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>CanoError::Workflow</code></td>
<td>No handler registered for current state</td>
<td>Register a task for every reachable state with <code>register()</code></td>
</tr>
<tr>
<td><code>CanoError::Workflow</code></td>
<td>Single task returned <code>TaskResult::Split</code></td>
<td>Use <code>register_split()</code> instead of <code>register()</code> for parallel tasks</td>
</tr>
<tr>
<td><code>CanoError::Workflow</code></td>
<td>Workflow timeout exceeded</td>
<td>Increase <code>with_timeout()</code> or optimize task execution time</td>
</tr>
<tr>
<td><code>CanoError::Configuration</code></td>
<td><code>PartialTimeout</code> strategy used without timeout configured</td>
<td>Add <code>.with_timeout(duration)</code> to <code>JoinConfig</code></td>
</tr>
<tr>
<td><code>CanoError::Timeout</code></td>
<td>Per-attempt timeout from <code>TaskConfig::attempt_timeout</code> elapsed</td>
<td>Increase <code>with_attempt_timeout()</code> or speed up the task; combine with a <code>RetryMode</code> if transient</td>
</tr>
<tr>
<td><code>CanoError::RetryExhausted</code></td>
<td>All retry attempts exhausted by a Node</td>
<td>Increase retry count or fix the underlying transient failure</td>
</tr>
<tr>
<td><code>CanoError::CircuitOpen</code></td>
<td>Call rejected by an open <code>CircuitBreaker</code> attached to <code>TaskConfig</code></td>
<td>Wait for the breaker's <code>reset_timeout</code> or fix the upstream dependency; the retry loop short-circuits — no attempts are consumed</td>
</tr>
<tr>
<td><code>CanoError::TaskExecution</code></td>
<td>Single task panicked (message is prefixed with <code>"panic:"</code>)</td>
<td>Inspect the panic payload in the message; fix the underlying invariant in the task body</td>
</tr>
<tr>
<td><code>CanoError::*</code></td>
<td>Any error propagated from task execution</td>
<td>Check the specific task logic — <code>NodeExecution</code>, <code>Preparation</code>, <code>Store</code>, etc.</td>
</tr>
</tbody>
</table>

<div class="callout callout-info">
<div class="callout-label">Panic safety</div>
<p>
Single-task execution is wrapped in <code>catch_unwind</code>: a panicking task surfaces as
<code>CanoError::TaskExecution("panic: …")</code> rather than aborting the workflow. Split tasks are
already isolated by <code>tokio::task::JoinSet</code>, so panics there propagate as task failures
through the join strategy.
</p>
</div>

```rust
match workflow.orchestrate(State::Start).await {
    Ok(final_state) => println!("Completed: {:?}", final_state),
    Err(CanoError::Workflow(msg)) => eprintln!("Workflow error: {}", msg),
    Err(CanoError::Configuration(msg)) => eprintln!("Config error: {}", msg),
    Err(CanoError::Timeout(msg)) => eprintln!("Attempt timed out: {}", msg),
    Err(CanoError::RetryExhausted(msg)) => eprintln!("Retries exhausted: {}", msg),
    Err(e) => eprintln!("Task error: {}", e),
}

```
<hr class="section-divider">

<h2 id="split-join"><a href="#split-join" class="anchor-link" aria-hidden="true">#</a>Split/Join Workflows</h2>
<p>
Execute multiple tasks in parallel and control how they proceed using flexible join strategies. 
This is essential for scatter-gather patterns, redundant API calls, and performance optimization.
</p>

<div class="diagram-frame">
<p class="diagram-label">Split / Join Pattern</p>
<div class="mermaid">
graph TD
A[Process State] -->|Split| B[Task 1]
A -->|Split| C[Task 2]
A -->|Split| D[Task 3]
B --> E{Join Strategy}
C --> E
D --> E
E -->|Satisfied| F[Aggregate State]
E -->|Failed/Timeout| G[Error State]
</div>
</div>

<h3 id="join-strategies"><a href="#join-strategies" class="anchor-link" aria-hidden="true">#</a>Join Strategies</h3>
<p>Cano provides several strategies to control how parallel tasks are aggregated.</p>

<div class="strategy-grid">
<div class="strategy-card">
<p class="strategy-name">All</p>
<p>Wait for <strong>all</strong> tasks to complete successfully.</p>

```rust
JoinStrategy::All

```
</div>
<div class="strategy-card">
<p class="strategy-name">Any</p>
<p>Proceed after the <strong>first</strong> task completes successfully.</p>

```rust
JoinStrategy::Any

```
</div>
<div class="strategy-card">
<p class="strategy-name">Quorum(n)</p>
<p>Wait for <strong>n</strong> tasks to complete successfully.</p>

```rust
JoinStrategy::Quorum(2)

```
</div>
<div class="strategy-card">
<p class="strategy-name">Percentage(p)</p>
<p>Wait for <strong>p%</strong> of tasks to complete successfully.</p>

```rust
JoinStrategy::Percentage(0.5)

```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialResults(n)</p>
<p>Proceed after <strong>n</strong> tasks complete successfully.</p>

```rust
JoinStrategy::PartialResults(3)

```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialTimeout</p>
<p>Accept whatever completes before <strong>timeout</strong> expires. Requires <code>.with_timeout()</code>.</p>

```rust
JoinStrategy::PartialTimeout

```
</div>
</div>

<h3 id="bulkhead"><a href="#bulkhead" class="anchor-link" aria-hidden="true">#</a>Bounding Concurrency with a Bulkhead</h3>
<p>
A split fans out as many tasks as the runtime can schedule. When you need to cap concurrent in-flight
work — to bound resource use, protect a downstream service, or stabilise tail latency — set a
<strong>bulkhead</strong> on the <code>JoinConfig</code>. Internally this gates each spawned task body on
a shared <code>tokio::sync::Semaphore</code>; excess tasks queue until a permit is free, but the join
strategy still applies once results come in.
</p>

<div class="code-block">
<span class="code-block-label">Bulkhead config</span>

```rust
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_bulkhead(4); // at most 4 split tasks run at once

```
</div>

<div class="callout callout-warning">
<div class="callout-label">Validation</div>
<p>
<code>with_bulkhead(0)</code> is rejected at execution time with <code>CanoError::Configuration</code>.
Leave the bulkhead unset (<code>None</code>) for unbounded concurrency. Bulkheads compose with
<code>PartialTimeout</code> and any other join strategy.
</p>
</div>

<h3 id="complete-example"><a href="#complete-example" class="anchor-link" aria-hidden="true">#</a>Complete Example</h3>
<p>Here is a complete, runnable example demonstrating how to use Split/Join with different strategies.</p>

```rust
use cano::prelude::*;
use std::time::Duration;

// 1. Define Workflow State
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState {
    Start,
    LoadData,
    ParallelProcessing,
    Aggregate,
    Complete,
}

// 2. Task to load initial data
#[derive(Clone)]
struct DataLoader;

#[task(state = DataState)]
impl DataLoader {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Loading initial data...");

        // Load some data to process
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        store.put("input_data", data)?;

        println!("Data loaded: 10 numbers");
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

// 3. Parallel processing task
#[derive(Clone)]
struct ProcessorTask {
    task_id: usize,
}

impl ProcessorTask {
    fn new(task_id: usize) -> Self {
        Self { task_id }
    }
}

#[task(state = DataState)]
impl ProcessorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Processor {} starting...", self.task_id);

        // Get input data
        let data: Vec<i32> = store.get("input_data")?;

        // Simulate processing time
        tokio::time::sleep(Duration::from_millis(100 * self.task_id as u64)).await;

        // Process data (simple example: multiply by task_id)
        let result: i32 = data.iter().map(|&x| x * self.task_id as i32).sum();

        // Store individual result
        store.put(&format!("result_{}", self.task_id), result)?;

        println!("Processor {} completed with result: {}", self.task_id, result);
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

// 4. Aggregator task
#[derive(Clone)]
struct Aggregator;

#[task(state = DataState)]
impl Aggregator {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Aggregating results...");

        // Collect all results
        let mut total = 0;
        let mut count = 0;

        for i in 1..=3 {
            if let Ok(result) = store.get::<i32>(&format!("result_{}", i)) {
                total += result;
                count += 1;
            }
        }

        store.put("final_result", total)?;
        store.put("processor_count", count)?;

        println!("Aggregation complete: {} processors, total: {}", count, total);
        Ok(TaskResult::Single(DataState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Define tasks to run in parallel
    let processors = vec![
        ProcessorTask::new(1),
        ProcessorTask::new(2),
        ProcessorTask::new(3),
    ];

    // Configure Join Strategy: Wait for ALL tasks
    let join_config = JoinConfig::new(
        JoinStrategy::All,
        DataState::Aggregate
    ).with_timeout(Duration::from_secs(5));

    // Build Workflow
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(DataState::Start, DataLoader)
        .register_split(
            DataState::ParallelProcessing,
            processors,
            join_config
        )
        .register(DataState::Aggregate, Aggregator)
        .add_exit_state(DataState::Complete);

    // Run Workflow
    let result = workflow.orchestrate(DataState::Start).await?;

    let final_result: i32 = store.get("final_result")?;
    println!("Workflow completed: {:?}", result);
    println!("Final result: {}", final_result);

    Ok(())
}

```
<hr class="section-divider">

<h2 id="join-strategy-examples"><a href="#join-strategy-examples" class="anchor-link" aria-hidden="true">#</a>Join Strategy Examples</h2>
<p>Each strategy handles parallel task completion differently. Here are detailed examples for each strategy.</p>

<h3 id="strategy-all"><a href="#strategy-all" class="anchor-link" aria-hidden="true">#</a>1. All Strategy - Wait for All Tasks</h3>
<p>Waits for all tasks to complete successfully. Fails if any task fails.</p>

<div class="diagram-frame">
<p class="diagram-label">All Strategy</p>
<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant T1 as Task 1
participant T2 as Task 2
participant T3 as Task 3
W->>T1: Start
W->>T2: Start
W->>T3: Start
T1-->>W: Complete ✓
T2-->>W: Complete ✓
T3-->>W: Complete ✓
Note over W: All Complete → Proceed
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Aggregate, Complete }

#[derive(Clone)]
struct DataLoader;
#[derive(Clone)]
struct ProcessorTask { id: u32 }
impl ProcessorTask { fn new(id: u32) -> Self { Self { id } } }
#[derive(Clone)]
struct Aggregator;

#[task(state = DataState)]
impl DataLoader {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

#[task(state = DataState)]
impl ProcessorTask {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

#[task(state = DataState)]
impl Aggregator {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<DataState> {
    // All Strategy: Best for workflows requiring complete data
    let join_config = JoinConfig::new(
        JoinStrategy::All,
        DataState::Aggregate,
    ).with_timeout(Duration::from_secs(10));

    Workflow::new(Resources::new().insert("store", store))
        .register(DataState::Start, DataLoader)
        .register_split(
            DataState::ParallelProcessing,
            vec![ProcessorTask::new(1), ProcessorTask::new(2), ProcessorTask::new(3)],
            join_config,
        )
        .register(DataState::Aggregate, Aggregator)
        .add_exit_state(DataState::Complete)
}

```

<h3 id="strategy-any"><a href="#strategy-any" class="anchor-link" aria-hidden="true">#</a>2. Any Strategy - First to Complete</h3>
<p>Proceeds as soon as the first task completes successfully. Other tasks are cancelled.</p>

<div class="diagram-frame">
<p class="diagram-label">Any Strategy</p>
<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant T1 as Task 1 (slow)
participant T2 as Task 2 (fast)
participant T3 as Task 3 (slow)
W->>T1: Start
W->>T2: Start
W->>T3: Start
T2-->>W: Complete ✓
Note over W: First Complete → Proceed
W->>T1: Cancel
W->>T3: Cancel
</div>
</div>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Complete }

#[derive(Clone)]
struct DataLoader;

#[task(state = DataState)]
impl DataLoader {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

#[derive(Clone)]
struct ApiCallTask { provider: String }

impl ApiCallTask {
    fn new(provider: &str) -> Self { Self { provider: provider.into() } }
}

#[task(state = DataState)]
impl ApiCallTask {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<DataState> {
    // Any Strategy: Best for redundant API calls or fastest-wins scenarios
    let join_config = JoinConfig::new(
        JoinStrategy::Any,
        DataState::Complete  // Skip aggregation, proceed directly
    );

    // Example: Call 3 different data sources, use whoever responds first
    Workflow::new(Resources::new().insert("store", store))
        .register(DataState::Start, DataLoader)
        .register_split(
            DataState::ParallelProcessing,
            vec![
                ApiCallTask::new("provider1"),
                ApiCallTask::new("provider2"),
                ApiCallTask::new("provider3"),
            ],
            join_config,
        )
        .add_exit_state(DataState::Complete)
}

```

<h3 id="strategy-quorum"><a href="#strategy-quorum" class="anchor-link" aria-hidden="true">#</a>3. Quorum Strategy - Wait for N Tasks</h3>
<p>Proceeds after a specific number of tasks complete successfully. Useful for consensus systems.</p>

<div class="diagram-frame">
<p class="diagram-label">Quorum Strategy</p>
<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant T1 as Task 1
participant T2 as Task 2
participant T3 as Task 3
participant T4 as Task 4
W->>T1: Start
W->>T2: Start
W->>T3: Start
W->>T4: Start
T1-->>W: Complete ✓
T3-->>W: Complete ✓
T2-->>W: Complete ✓
Note over W: Quorum (3/4) Met → Proceed
W->>T4: Cancel
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Aggregate, Complete }

#[derive(Clone)]
struct PrepareData;
#[derive(Clone)]
struct WriteReplica { id: u32 }
impl WriteReplica { fn new(id: u32) -> Self { Self { id } } }
#[derive(Clone)]
struct ConfirmWrite;

#[task(state = DataState)]
impl PrepareData {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

#[task(state = DataState)]
impl WriteReplica {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

#[task(state = DataState)]
impl ConfirmWrite {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<DataState> {
    // Quorum Strategy: Best for distributed consensus or majority voting
    let join_config = JoinConfig::new(
        JoinStrategy::Quorum(3),  // Need 3 out of 5 to succeed
        DataState::Aggregate,
    ).with_timeout(Duration::from_secs(5));

    // Example: Write to 5 replicas, succeed when 3 confirm
    Workflow::new(Resources::new().insert("store", store))
        .register(DataState::Start, PrepareData)
        .register_split(
            DataState::ParallelProcessing,
            vec![
                WriteReplica::new(1),
                WriteReplica::new(2),
                WriteReplica::new(3),
                WriteReplica::new(4),
                WriteReplica::new(5),
            ],
            join_config,
        )
        .register(DataState::Aggregate, ConfirmWrite)
        .add_exit_state(DataState::Complete)
}
```

<h3 id="strategy-percentage"><a href="#strategy-percentage" class="anchor-link" aria-hidden="true">#</a>4. Percentage Strategy - Wait for % of Tasks</h3>
<p>Proceeds after a percentage of tasks complete successfully. Flexible for varying batch sizes.</p>

<div class="diagram-frame">
<p class="diagram-label">Percentage Strategy</p>
<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant T1 as Task 1
participant T2 as Task 2
participant T3 as Task 3
participant T4 as Task 4
W->>T1: Start
W->>T2: Start
W->>T3: Start
W->>T4: Start
T1-->>W: Complete ✓
T2-->>W: Complete ✓
T4-->>W: Complete ✓
Note over W: 75% (3/4) Met → Proceed
W->>T3: Cancel
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Aggregate, Complete }

#[derive(Clone)]
struct LoadRecords;
#[derive(Clone)]
struct RecordProcessor { idx: usize }
impl RecordProcessor { fn new(idx: usize) -> Self { Self { idx } } }
#[derive(Clone)]
struct SummarizeResults;

#[task(state = DataState)]
impl LoadRecords {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

#[task(state = DataState)]
impl RecordProcessor {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

#[task(state = DataState)]
impl SummarizeResults {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<DataState> {
    // Percentage Strategy: Best for batch processing with acceptable partial results
    let join_config = JoinConfig::new(
        JoinStrategy::Percentage(0.75),  // Need 75% to succeed
        DataState::Aggregate,
    ).with_timeout(Duration::from_secs(10));

    // Example: Process 100 records, proceed when 75 complete
    let mut tasks = Vec::new();
    for i in 0..100 {
        tasks.push(RecordProcessor::new(i));
    }

    Workflow::new(Resources::new().insert("store", store))
        .register(DataState::Start, LoadRecords)
        .register_split(
            DataState::ParallelProcessing,
            tasks,
            join_config,
        )
        .register(DataState::Aggregate, SummarizeResults)
        .add_exit_state(DataState::Complete)
}

```

<h3 id="strategy-partial-results"><a href="#strategy-partial-results" class="anchor-link" aria-hidden="true">#</a>5. PartialResults Strategy - Accept Partial Completion</h3>
<p>Proceeds after N tasks complete successfully, cancels remaining tasks. Tracks all outcomes.</p>

<div class="diagram-frame">
<p class="diagram-label">PartialResults Strategy</p>
<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant T1 as Task 1
participant T2 as Task 2
participant T3 as Task 3
participant T4 as Task 4
W->>T1: Start
W->>T2: Start
W->>T3: Start
W->>T4: Start
T1-->>W: Complete ✓
T2-->>W: Failed ✗
T3-->>W: Complete ✓
Note over W: 2 Successes → Proceed
W->>T4: Cancel
Note over W: Track: 2 success, 1 error, 1 cancelled
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Aggregate, Complete }

#[derive(Clone)]
struct PrepareRequest;
#[derive(Clone)]
struct ServiceCall { name: String }
impl ServiceCall { fn new(name: &str) -> Self { Self { name: name.into() } } }
#[derive(Clone)]
struct MergePartialResults;

#[task(state = DataState)]
impl PrepareRequest {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

#[task(state = DataState)]
impl ServiceCall {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

#[task(state = DataState)]
impl MergePartialResults {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<DataState> {
    // PartialResults Strategy: Best for fault-tolerant systems with latency optimization
    let join_config = JoinConfig::new(
        JoinStrategy::PartialResults(2),  // Proceed after any 2 succeed
        DataState::Aggregate,
    )
    .with_timeout(Duration::from_secs(5));

    // Example: Call multiple services, use fastest 3 responses
    Workflow::new(Resources::new().insert("store", store))
        .register(DataState::Start, PrepareRequest)
        .register_split(
            DataState::ParallelProcessing,
            vec![
                ServiceCall::new("fast-service"),
                ServiceCall::new("medium-service"),
                ServiceCall::new("slow-service"),
                ServiceCall::new("backup-service"),
            ],
            join_config,
        )
        .register(DataState::Aggregate, MergePartialResults)
        .add_exit_state(DataState::Complete)
}

```

<h3 id="strategy-partial-timeout"><a href="#strategy-partial-timeout" class="anchor-link" aria-hidden="true">#</a>6. PartialTimeout Strategy - Deadline-Based Completion</h3>
<p>Accepts whatever completes before timeout expires. Proceeds with available results.</p>

<div class="diagram-frame">
<p class="diagram-label">PartialTimeout Strategy</p>
<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant T1 as Task 1
participant T2 as Task 2
participant T3 as Task 3
participant T4 as Task 4
W->>T1: Start
W->>T2: Start
W->>T3: Start
W->>T4: Start
T1-->>W: Complete ✓
T3-->>W: Complete ✓
Note over W: Timeout Reached
W->>T2: Cancel
W->>T4: Cancel
Note over W: Proceed with 2 results
</div>
</div>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Aggregate, Complete }

#[derive(Clone)]
struct LoadUserContext;
#[derive(Clone)]
struct RecommendationEngine { kind: String }
impl RecommendationEngine { fn new(kind: &str) -> Self { Self { kind: kind.into() } } }
#[derive(Clone)]
struct AggregateWithinSla;

#[task(state = DataState)]
impl LoadUserContext {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

#[task(state = DataState)]
impl RecommendationEngine {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

#[task(state = DataState)]
impl AggregateWithinSla {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<DataState> {
    // PartialTimeout Strategy: Best for real-time systems with strict SLAs
    let join_config = JoinConfig::new(
        JoinStrategy::PartialTimeout,  // Must specify timeout
        DataState::Aggregate,
    )
    .with_timeout(Duration::from_millis(500));  // 500ms deadline

    // Example: Real-time recommendation system with 500ms SLA
    Workflow::new(Resources::new().insert("store", store))
        .register(DataState::Start, LoadUserContext)
        .register_split(
            DataState::ParallelProcessing,
            vec![
                RecommendationEngine::new("collaborative"),
                RecommendationEngine::new("content-based"),
                RecommendationEngine::new("trending"),
                RecommendationEngine::new("personalized"),
            ],
            join_config,
        )
        .register(DataState::Aggregate, AggregateWithinSla)
        .add_exit_state(DataState::Complete)
}
```
<hr class="section-divider">

<h2 id="comparison-table"><a href="#comparison-table" class="anchor-link" aria-hidden="true">#</a>Comparison Table</h2>
<table class="styled-table">
<thead>
<tr>
<th>Strategy</th>
<th>Trigger Condition</th>
<th>Cancels Others</th>
<th>Best Use Case</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>All</code></td>
<td>All tasks succeed</td>
<td>No</td>
<td>Complete data required</td>
</tr>
<tr>
<td><code>Any</code></td>
<td>First success</td>
<td>Yes</td>
<td>Redundant API calls</td>
</tr>
<tr>
<td><code>Quorum(n)</code></td>
<td>N tasks succeed</td>
<td>Yes</td>
<td>Distributed consensus</td>
</tr>
<tr>
<td><code>Percentage(p)</code></td>
<td>P% succeed</td>
<td>Yes</td>
<td>Batch processing</td>
</tr>
<tr>
<td><code>PartialResults(n)</code></td>
<td>N tasks succeed</td>
<td>Yes</td>
<td>Latency optimization</td>
</tr>
<tr>
<td><code>PartialTimeout</code></td>
<td>Timeout reached</td>
<td>Yes</td>
<td>Strict SLA requirements</td>
</tr>
</tbody>
</table>
<hr class="section-divider">

<h2 id="parallel-patterns"><a href="#parallel-patterns" class="anchor-link" aria-hidden="true">#</a>Common Parallel Processing Patterns</h2>
<p>
Split/Join handles complex parallel processing within a single workflow. Below are real-world
patterns that fan out work across many tasks and join results back into the FSM.
</p>

<h3 id="pattern-queue"><a href="#pattern-queue" class="anchor-link" aria-hidden="true">#</a>Pattern 1: Queue Consumer with Batch Processing</h3>
<p>
Process items from a queue in parallel batches. Instead of running multiple workflow instances concurrently,
use a single workflow that pulls batches and processes them in parallel.
</p>

```rust
use cano::prelude::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum QueueState {
    PullBatch,
    ProcessBatch,
    Complete,
    Idle,
}

// Simulated queue (in production, use actual queue like Redis, SQS, etc.)
type SharedQueue = Arc<Mutex<VecDeque<String>>>;

#[derive(Clone)]
struct QueuePuller {
    queue: SharedQueue,
    batch_size: usize,
}

#[task(state = QueueState)]
impl QueuePuller {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let mut queue = self.queue.lock().await;

        // Pull batch from queue
        let mut batch = Vec::new();
        for _ in 0..self.batch_size {
            if let Some(item) = queue.pop_front() {
                batch.push(item);
            } else {
                break;
            }
        }

        if batch.is_empty() {
            println!("Queue empty, waiting...");
            // Wait and retry
            tokio::time::sleep(Duration::from_secs(1)).await;
            return Ok(TaskResult::Single(QueueState::PullBatch));
        }

        println!("Pulled {} items from queue", batch.len());
        store.put("current_batch", batch)?;

        // Split into parallel processing
        Ok(TaskResult::Single(QueueState::ProcessBatch))
    }
}

#[derive(Clone)]
struct ItemProcessor {
    item_id: String,
}

#[task(state = QueueState)]
impl ItemProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("Processing item: {}", self.item_id);

        // Simulate processing
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Store result
        store.put(&format!("result_{}", self.item_id), "completed")?;

        Ok(TaskResult::Single(QueueState::Complete))
    }
}

#[derive(Clone)]
struct BatchSplitter {
    queue: SharedQueue,
    batch_size: usize,
}

#[task(state = QueueState)]
impl BatchSplitter {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let batch: Vec<String> = store.get("current_batch")?;

        if batch.is_empty() {
            return Ok(TaskResult::Single(QueueState::PullBatch));
        }

        // Create processors for each item in parallel
        let processors: Vec<Box<dyn Task<QueueState>>> = batch
            .into_iter()
            .map(|item| Box::new(ItemProcessor { item_id: item }) as Box<dyn Task<QueueState>>)
            .collect();

        println!("Splitting into {} parallel processors", processors.len());

        // Return split to process all items in parallel
        Ok(TaskResult::Split(
            processors.into_iter()
                .map(|_| QueueState::Complete)
                .collect()
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
    let queue = Arc::new(Mutex::new(VecDeque::from(vec![
        "order1".to_string(),
        "order2".to_string(),
        "order3".to_string(),
        "order4".to_string(),
        "order5".to_string(),
    ])));

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(QueueState::PullBatch, QueuePuller { 
            queue: queue.clone(), 
            batch_size: 10 
        })
        .register(QueueState::ProcessBatch, BatchSplitter {
            queue: queue.clone(),
            batch_size: 10,
        })
        .add_exit_state(QueueState::Complete);

    // Process batches continuously until queue empty
    loop {
        let result = workflow.orchestrate(QueueState::PullBatch).await?;
        if result == QueueState::Complete {
            let q = queue.lock().await;
            if q.is_empty() {
                break;
            }
        }
    }

    println!("✅ All items processed!");
    Ok(())
}

```

<h3 id="pattern-dynamic"><a href="#pattern-dynamic" class="anchor-link" aria-hidden="true">#</a>Pattern 2: Dynamic Task Generation</h3>
<p>
Generate parallel tasks dynamically based on runtime data. Perfect for processing variable-size datasets
or handling events that arrive over time.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState {
    LoadRecords,
    ProcessBatch,
    Aggregate,
    Complete,
}

// Load records and store them so the split tasks can read them
#[derive(Clone)]
struct RecordLoader;

#[task(state = DataState)]
impl RecordLoader {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let records: Vec<i32> = (1..=100).collect();
        store.put("records", records)?;
        println!("Loaded 100 records");
        Ok(TaskResult::Single(DataState::ProcessBatch))
    }
}

// Each processor reads from the shared store and handles one record by index
#[derive(Clone)]
struct RecordProcessor {
    index: usize,
}

#[task(state = DataState)]
impl RecordProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let records: Vec<i32> = store.get("records")?;
        let value = records[self.index];
        tokio::time::sleep(Duration::from_millis(10)).await;
        store.put(&format!("result_{}", self.index), value * 2)?;
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

// Aggregator runs once after the split joins
#[derive(Clone)]
struct FinishAggregate;

#[task(state = DataState)]
impl FinishAggregate {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let total: i32 = (0..100)
            .filter_map(|i| store.get::<i32>(&format!("result_{}", i)).ok())
            .sum();
        println!("Aggregated total: {}", total);
        Ok(TaskResult::Single(DataState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Build the processor tasks before constructing the workflow
    let processors: Vec<RecordProcessor> = (0..100).map(|i| RecordProcessor { index: i }).collect();

    let join_config = JoinConfig::new(JoinStrategy::All, DataState::Aggregate);

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(DataState::LoadRecords, RecordLoader)
        .register_split(DataState::ProcessBatch, processors, join_config)
        .register(DataState::Aggregate, FinishAggregate)
        .add_exit_state(DataState::Complete);

    workflow.orchestrate(DataState::LoadRecords).await?;
    Ok(())
}

```

<h3 id="pattern-resource"><a href="#pattern-resource" class="anchor-link" aria-hidden="true">#</a>Pattern 3: Resource-Limited Parallel Processing</h3>
<p>
Control parallelism when you have limited resources (API keys, connections, etc.). 
Use semaphores to limit concurrent executions within a single workflow.
</p>

```rust
use cano::prelude::*;

use tokio::sync::Semaphore;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ApiState { Start, Complete }

async fn make_api_call(_id: usize) -> Result<String, CanoError> {
    Ok("ok".to_string())
}

#[derive(Clone)]
struct RateLimitedApiTask {
    api_id: usize,
    semaphore: Arc<Semaphore>,  // Limit concurrent API calls
}

#[task(state = ApiState)]
impl RateLimitedApiTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ApiState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        // Acquire permit (blocks if limit reached)
        let _permit = self.semaphore.acquire().await
            .map_err(|e| CanoError::task_execution(e.to_string()))?;

        println!("API call {} starting (within rate limit)", self.api_id);

        // Make API call
        let result = make_api_call(self.api_id).await?;
        store.put(&format!("api_result_{}", self.api_id), result)?;

        println!("API call {} completed", self.api_id);

        Ok(TaskResult::Single(ApiState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<ApiState> {
    // Example: Limit to 5 concurrent API calls
    let semaphore = Arc::new(Semaphore::new(5));

    let mut tasks = Vec::new();
    for i in 0..20 {
        tasks.push(RateLimitedApiTask {
            api_id: i,
            semaphore: semaphore.clone(),
        });
    }

    // All 20 tasks will run, but only 5 at a time
    let join_config = JoinConfig::new(JoinStrategy::All, ApiState::Complete);

    Workflow::new(Resources::new().insert("store", store))
        .register_split(ApiState::Start, tasks, join_config)
        .add_exit_state(ApiState::Complete)
}
```

<h3 id="pattern-continuous"><a href="#pattern-continuous" class="anchor-link" aria-hidden="true">#</a>Pattern 4: Continuous Workflow with Split/Join</h3>
<p>
Combine scheduling with split/join for continuous parallel processing within a single
scheduled workflow.
</p>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProcessState { Start, ProcessBatch, Complete }

// Simulated upstream queue lookup
async fn fetch_pending_work() -> Result<Vec<String>, CanoError> {
    Ok(vec!["job-1".to_string(), "job-2".to_string()])
}

// WorkProcessor handles a single item identified by index in the store
#[derive(Clone)]
struct WorkProcessor {
    item_index: usize,
}

#[task(state = ProcessState)]
impl WorkProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ProcessState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let items: Vec<String> = store.get("work_items")?;
        if let Some(item) = items.get(self.item_index) {
            println!("Processing item: {}", item);
            // ... processing logic
        }
        Ok(TaskResult::Single(ProcessState::Complete))
    }
}

// LoaderTask fetches work and registers parallel processors via register_split at workflow build time.
// Because the split tasks are registered statically, the batch size is fixed per workflow instance.
#[derive(Clone)]
struct BatchLoaderTask;

#[task(state = ProcessState)]
impl BatchLoaderTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ProcessState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let items = fetch_pending_work().await?;
        if items.is_empty() {
            println!("No work available");
            return Ok(TaskResult::Single(ProcessState::Complete));
        }
        store.put("work_items", items)?;
        Ok(TaskResult::Single(ProcessState::ProcessBatch))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    // Build worker tasks for a fixed batch size; adjust batch_size as needed.
    let batch_size = 10usize;
    let processors: Vec<WorkProcessor> = (0..batch_size).map(|i| WorkProcessor { item_index: i }).collect();
    let join_config = JoinConfig::new(JoinStrategy::All, ProcessState::Complete);

    let batch_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(ProcessState::Start, BatchLoaderTask)
        .register_split(ProcessState::ProcessBatch, processors, join_config)
        .add_exit_state(ProcessState::Complete);

    // Schedule to run every 10 seconds
    scheduler.every_seconds("batch_processor", batch_workflow, ProcessState::Start, 10)?;

    scheduler.start().await?;
    Ok(())
}

```

<h3 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use These Patterns</h3>
<ul>
<li><strong>Queue Consumer Pattern</strong>: When processing items from external queues (SQS, Redis, Kafka)</li>
<li><strong>Dynamic Task Generation</strong>: When the number of parallel tasks depends on runtime data</li>
<li><strong>Resource-Limited Processing</strong>: When you need to limit concurrent operations (API rate limits, database connections)</li>
<li><strong>Continuous Workflow</strong>: When you need scheduled parallel processing without multiple workflow instances</li>
</ul>

<div class="callout callout-tip">
<p><strong>💡 Key Insight:</strong> These patterns leverage Split/Join within a single workflow to achieve 
the same parallelism as running multiple concurrent workflow instances, but with simpler mental model, 
better resource control, and type-safe state management.</p>
</div>
<hr class="section-divider">

<h2 id="workflow-on-request"><a href="#workflow-on-request" class="anchor-link" aria-hidden="true">#</a>Common Example: Workflow per HTTP Request</h2>
<p>
A very common pattern is triggering a workflow for every incoming HTTP request, running the FSM to completion,
and returning the results in the response. Cano workflows are cheap to construct — tasks are small <code>Clone</code>
structs wrapped in <code>Arc</code> internally — so building one per request is fast.
</p>

<div class="callout callout-warning">
<p><strong>⚠️ Store isolation:</strong> The <code>MemoryStore</code> is <code>Arc</code>-wrapped, so cloning a
workflow shares the same store. For request-scoped data you should create a <strong>fresh store per request</strong>
to avoid data leaking between concurrent requests.</p>
</div>

<h3 id="on-request-pattern"><a href="#on-request-pattern" class="anchor-link" aria-hidden="true">#</a>Pattern Overview</h3>
<div class="diagram-frame">
<p class="diagram-label">Workflow per Request</p>
<div class="mermaid">
sequenceDiagram
participant Client
participant Handler as Axum Handler
participant Store as MemoryStore
participant WF as Workflow FSM
Client->>Handler: POST /process {text}
Handler->>Store: put("input_text", text)
Handler->>WF: orchestrate(Parse)
WF->>Store: get / put during tasks
WF-->>Handler: Ok(Done)
Handler->>Store: get("word_count"), get("uppercased")
Handler-->>Client: 200 JSON response
</div>
</div>

<ol>
<li>Create a <strong>fresh <code>MemoryStore</code></strong> for each request.</li>
<li>Write request data into the store with <code>store.put()</code>.</li>
<li>Build the workflow using a <strong>factory function</strong> that receives the store.</li>
<li>Call <code>workflow.orchestrate(initial_state)</code> — this drives the FSM to an exit state.</li>
<li>Read the results from the store and return them in the HTTP response.</li>
</ol>

<h3 id="on-request-example"><a href="#on-request-example" class="anchor-link" aria-hidden="true">#</a>Full Example (Axum)</h3>

```rust
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use cano::prelude::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ── States ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TextPipelineState {
    Parse,
    Transform,
    Done,
}

// ── Request / Response ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct ProcessRequest { text: String }

#[derive(Serialize)]
struct ProcessResponse {
    original: String,
    word_count: usize,
    uppercased: String,
}

// ── Tasks ───────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ParseTask;

#[task(state = TextPipelineState)]
impl ParseTask {
    async fn run(
        &self,
        res: &Resources,
    ) -> Result<TaskResult<TextPipelineState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let text: String = store.get("input_text")
            .map_err(|e| CanoError::task_execution(format!("missing input: {e}")))?;

        if text.trim().is_empty() {
            return Err(CanoError::task_execution("input text is empty"));
        }
        store.put("validated_text", text)
            .map_err(|e| CanoError::store(format!("{e}")))?;

        Ok(TaskResult::Single(TextPipelineState::Transform))
    }
}

#[derive(Clone)]
struct TransformTask;

#[task(state = TextPipelineState)]
impl TransformTask {
    async fn run(
        &self,
        res: &Resources,
    ) -> Result<TaskResult<TextPipelineState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let text: String = store.get("validated_text")
            .map_err(|e| CanoError::task_execution(format!("{e}")))?;

        store.put("word_count", text.split_whitespace().count())
            .map_err(|e| CanoError::store(format!("{e}")))?;
        store.put("uppercased", text.to_uppercase())
            .map_err(|e| CanoError::store(format!("{e}")))?;

        Ok(TaskResult::Single(TextPipelineState::Done))
    }
}

// ── Workflow factory ────────────────────────────────────────────────────

fn build_workflow(store: MemoryStore) -> Workflow<TextPipelineState> {
    Workflow::new(Resources::new().insert("store", store))
        .register(TextPipelineState::Parse, ParseTask)
        .register(TextPipelineState::Transform, TransformTask)
        .add_exit_state(TextPipelineState::Done)
        .with_timeout(Duration::from_secs(5))
}

// ── Handler ─────────────────────────────────────────────────────────────

async fn process_handler(
    Json(payload): Json<ProcessRequest>,
) -> Result<Json<ProcessResponse>, StatusCode> {
    // Fresh store per request — full isolation
    let store = MemoryStore::new();
    store.put("input_text", payload.text.clone())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let workflow = build_workflow(store.clone());
    workflow.orchestrate(TextPipelineState::Parse).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let word_count: usize = store.get("word_count")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let uppercased: String = store.get("uppercased")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProcessResponse {
        original: payload.text,
        word_count,
        uppercased,
    }))
}

// ── Main ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let app = Router::new().route("/process", post(process_handler));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:4001").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

```

<div class="callout callout-tip">
<p><strong>💡 Tips:</strong></p>
<ul>
<li>Use <code>.with_timeout()</code> on the workflow to prevent hung requests from blocking indefinitely.</li>
<li>The final state returned by <code>orchestrate()</code> tells you which exit state was reached — use it for branching response logic (e.g., success vs. error states).</li>
<li>For read-heavy workloads with shared reference data, you can pre-populate a store and share it via <code>Arc</code>, using per-request keys to avoid collisions.</li>
</ul>
</div>

<p>
Run the full example with <code>cargo run --example workflow_on_request</code>.
</p>
<hr class="section-divider">

<h2 id="ad-exchange"><a href="#ad-exchange" class="anchor-link" aria-hidden="true">#</a>Advanced Example: Real-Time Ad Exchange Workflow</h2>
<p>
This comprehensive example demonstrates a production-grade ad exchange system with multiple split/join points,
diverse join strategies, complex state management, and real-world constraints like timeouts and partial results.
</p>

<h3 id="ad-architecture"><a href="#ad-architecture" class="anchor-link" aria-hidden="true">#</a>System Architecture</h3>
<div class="diagram-frame">
<p class="diagram-label">Ad Exchange Architecture</p>
<div class="mermaid">
graph TB
Start[Ad Request Received] --> Validate[Validate Request]
Validate -->|Valid| Split1[SPLIT: Context Gathering]
Validate -->|Invalid| Invalid[Invalid Response]
Invalid --> Complete
subgraph "Split 1: Context Gathering - All Strategy"
Split1 --> User[Fetch User Profile]
Split1 --> Geo[Fetch Geo Data]
Split1 --> Device[Device Detection]
end
User --> Join1[JOIN: All Required 100ms timeout]
Geo --> Join1
Device --> Join1
Join1 -->|Success| Split2[SPLIT: Bid Requests]
Join1 -.->|Timeout/Error| Split5[SPLIT: Error Tracking]
subgraph "Split 2: Bid Requests - PartialTimeout Strategy"
Split2 --> DSP1[DSP-FastBidder 45ms]
Split2 --> DSP2[DSP-Premium 80ms]
Split2 --> DSP3[DSP-Global 120ms]
Split2 --> DSP4[DSP-Slow 190ms]
Split2 --> DSP5[DSP-TooSlow 250ms]
end
DSP1 --> Join2[JOIN: PartialTimeout 200ms]
DSP2 --> Join2
DSP3 --> Join2
DSP4 --> Join2
DSP5 --> Join2
Join2 --> Split3[SPLIT: Bid Scoring]
subgraph "Split 3: Bid Scoring - All Strategy"
Split3 --> Score1[Score Bid 0]
Split3 --> Score2[Score Bid 1]
Split3 --> Score3[Score Bid 2]
end
Score1 --> Join3[JOIN: All 50ms timeout]
Score2 --> Join3
Score3 --> Join3
Join3 -->|Success| Auction[Run Auction]
Join3 -.->|Timeout/Error| Split5
Auction --> Winner{Has Winner?}
Winner -->|Yes| Split4[SPLIT: Tracking]
Winner -->|No| Split5
subgraph "Split 4: Tracking - All Strategy"
Split4 --> Track1[Log Analytics]
Split4 --> Track2[Update Metrics]
Split4 --> Track3[Notify Winner]
Split4 --> Track4[Store Auction]
end
Track1 --> Join4[JOIN: All 100ms timeout]
Track2 --> Join4
Track3 --> Join4
Track4 --> Join4
Join4 -->|Success| Response[Build Response]
Join4 -.->|Timeout/Error| Split5
Response --> Complete[Complete]
subgraph "Split 5: Error Tracking - All Strategy"
Split5 --> ErrLog[Log Error]
Split5 --> ErrMetrics[Update Error Metrics]
end
ErrLog --> Join5[JOIN: All 50ms timeout]
ErrMetrics --> Join5
Join5 --> NoFill[No Fill Response]
NoFill --> Complete
style Start fill:#4CAF50
style Complete fill:#2196F3
style NoFill fill:#FF5722
style Invalid fill:#FF5722
style Split1 fill:#FF9800
style Split2 fill:#FF9800
style Split3 fill:#FF9800
style Split4 fill:#FF9800
style Split5 fill:#F59E0B
style Join1 fill:#9C27B0
style Join2 fill:#9C27B0
style Join3 fill:#9C27B0
style Join4 fill:#9C27B0
style Join5 fill:#F59E0B
</div>
</div>

<h3 id="ad-implementation"><a href="#ad-implementation" class="anchor-link" aria-hidden="true">#</a>Complete Implementation</h3>

```rust
use cano::prelude::*;
use std::time::Duration;

// ============================================================================
// State Definitions
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AdExchangeState {
    // Entry and validation
    Start,

    // Context gathering (Split 1)
    GatherContext,

    // Bid request phase (Split 2)
    RequestBids,

    // Auction phase (Split 3)
    ScoreBids,
    RunAuction,

    // Tracking phase (Split 4)
    TrackResults,

    // Error tracking phase (Split 5)
    ErrorTracking,

    // Terminal states
    BuildResponse,
    InvalidResponse,
    Complete,
    Rejected,
    NoFill,
}

// ============================================================================
// Data Models
// ============================================================================

#[derive(Debug, Clone)]
struct AdRequest {
    request_id: String,
    placement_id: String,
    floor_price: f64,
}

#[derive(Debug, Clone)]
struct UserContext {}

#[derive(Debug, Clone)]
struct GeoContext {}

#[derive(Debug, Clone)]
struct DeviceContext {}

#[derive(Debug, Clone)]
struct BidResponse {
    partner_id: String,
    price: f64,
    creative_id: String,
    response_time_ms: u64,
}

#[derive(Debug, Clone)]
struct ScoredBid {
    bid: BidResponse,
    score: f64,  // Adjusted price after quality scoring
    rank: usize,
}

#[derive(Debug, Clone)]
struct AuctionResult {
    winner: Option<ScoredBid>,
    total_bids: usize,
    auction_time_ms: u64,
}

// ============================================================================
// Phase 1: Request Validation
// ============================================================================

#[derive(Clone)]
struct ValidateRequestTask;

#[task(state = AdExchangeState)]
impl ValidateRequestTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let request: AdRequest = store.get("ad_request")?;

        println!("🔍 Validating request {}", request.request_id);

        // Validation logic
        if request.placement_id.is_empty() {
            println!("❌ Invalid placement ID");
            return Ok(TaskResult::Single(AdExchangeState::InvalidResponse));
        }

        if request.floor_price < 0.01 {
            println!("❌ Floor price too low");
            return Ok(TaskResult::Single(AdExchangeState::InvalidResponse));
        }

        println!("✅ Request validated");
        Ok(TaskResult::Single(AdExchangeState::GatherContext))
    }
}

// ============================================================================
// Phase 2: Context Gathering (Split 1 - All Strategy)
// ============================================================================

// Wrapper enum for heterogeneous context gathering tasks
#[derive(Clone)]
enum ContextTask {
    FetchUser,
    FetchGeo,
    DetectDevice,
}

#[task(state = AdExchangeState)]
impl ContextTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        match self {
            ContextTask::FetchUser => {
                println!("  👤 Fetching user profile...");
                tokio::time::sleep(Duration::from_millis(50)).await;

                let user = UserContext {};

                store.put("user_context", user)?;
                println!("  ✅ User profile loaded");
                Ok(TaskResult::Single(AdExchangeState::RequestBids))
            }
            ContextTask::FetchGeo => {
                println!("  🌍 Fetching geo data...");
                tokio::time::sleep(Duration::from_millis(30)).await;

                let geo = GeoContext {};

                store.put("geo_context", geo)?;
                println!("  ✅ Geo data loaded");
                Ok(TaskResult::Single(AdExchangeState::RequestBids))
            }
            ContextTask::DetectDevice => {
                println!("  📱 Detecting device...");
                tokio::time::sleep(Duration::from_millis(20)).await;

                let device = DeviceContext {};

                store.put("device_context", device)?;
                println!("  ✅ Device detected");
                Ok(TaskResult::Single(AdExchangeState::RequestBids))
            }
        }
    }
}

// ============================================================================
// Phase 3: Bid Requests (Split 2 - PartialTimeout Strategy)
// ============================================================================

#[derive(Clone)]
struct ContactDSPTask {
    partner_id: String,
    response_delay_ms: u64,  // Simulated network latency
}

#[task(state = AdExchangeState)]
impl ContactDSPTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("  📡 Requesting bid from {}...", self.partner_id);

        // Simulate DSP bid request with varying latency
        tokio::time::sleep(Duration::from_millis(self.response_delay_ms)).await;

        // Some DSPs might not respond in time or may not bid
        if self.response_delay_ms > 180 {
            // Will timeout
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let bid = BidResponse {
            partner_id: self.partner_id.clone(),
            price: 2.50 + (self.response_delay_ms as f64 / 100.0),
            creative_id: format!("creative_{}", self.partner_id),
            response_time_ms: self.response_delay_ms,
        };

        // Store bid
        let mut bids: Vec<BidResponse> = store.get("bids").unwrap_or_default();
        bids.push(bid.clone());
        store.put("bids", bids)?;

        println!("  ✅ {} bid: ${:.2}", self.partner_id, bid.price);
        Ok(TaskResult::Single(AdExchangeState::ScoreBids))
    }
}

// ============================================================================
// Phase 4: Bid Scoring (Split 3 - Percentage Strategy)
// ============================================================================

#[derive(Clone)]
struct ScoreBidTask {
    bid_index: usize,
}

#[task(state = AdExchangeState)]
impl ScoreBidTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let bids: Vec<BidResponse> = store.get("bids")?;

        if self.bid_index >= bids.len() {
            return Err(CanoError::task_execution("Bid index out of range"));
        }

        let bid = &bids[self.bid_index];

        println!("  📊 Scoring bid from {}...", bid.partner_id);

        // Simulate scoring computation
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Quality score based on partner history and response time
        let quality_multiplier = match bid.response_time_ms {
            0..=50 => 1.1,    // Fast response bonus
            51..=100 => 1.0,  // Normal
            101..=150 => 0.95, // Slight penalty
            _ => 0.9,         // Slow response penalty
        };

        let scored_bid = ScoredBid {
            bid: bid.clone(),
            score: bid.price * quality_multiplier,
            rank: 0,  // Will be set during auction
        };

        let score_value = scored_bid.score;

        // Store scored bid
        let mut scored_bids: Vec<ScoredBid> = store.get("scored_bids").unwrap_or_default();
        scored_bids.push(scored_bid);
        store.put("scored_bids", scored_bids)?;

        println!("  ✅ Bid scored: ${:.2}", score_value);
        Ok(TaskResult::Single(AdExchangeState::RunAuction))
    }
}

// ============================================================================
// Phase 5: Auction
// ============================================================================

#[derive(Clone)]
struct RunAuctionTask;

#[task(state = AdExchangeState)]
impl RunAuctionTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("\n  🎯 Running auction...");

        let start = tokio::time::Instant::now();
        let mut scored_bids: Vec<ScoredBid> = store.get("scored_bids")?;
        let request: AdRequest = store.get("ad_request")?;

        // Filter bids above floor price
        scored_bids.retain(|b| b.score >= request.floor_price);

        if scored_bids.is_empty() {
            println!("  ❌ No valid bids above floor price");
            let result = AuctionResult {
                winner: None,
                total_bids: 0,
                auction_time_ms: start.elapsed().as_millis() as u64,
            };
            store.put("auction_result", result)?;
            return Ok(TaskResult::Single(AdExchangeState::ErrorTracking));
        }

        // Sort by score (descending)
        scored_bids.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

        // Set ranks
        for (i, bid) in scored_bids.iter_mut().enumerate() {
            bid.rank = i + 1;
        }

        let winner = scored_bids[0].clone();
        println!("  🏆 Winner: {} at ${:.2}", winner.bid.partner_id, winner.score);

        let result = AuctionResult {
            winner: Some(winner),
            total_bids: scored_bids.len(),
            auction_time_ms: start.elapsed().as_millis() as u64,
        };

        store.put("auction_result", result)?;
        Ok(TaskResult::Single(AdExchangeState::TrackResults))
    }
}

// ============================================================================
// Phase 6: Tracking (Split 4 - Quorum Strategy)
// ============================================================================

// Wrapper enum for heterogeneous tracking tasks
#[derive(Clone)]
enum TrackingTask {
    LogAnalytics,
    UpdateMetrics,
    NotifyWinner,
    StoreAuction,
}

#[task(state = AdExchangeState)]
impl TrackingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        match self {
            TrackingTask::LogAnalytics => {
                println!("  📈 Logging to analytics...");
                tokio::time::sleep(Duration::from_millis(30)).await;

                let result: AuctionResult = store.get("auction_result")?;
                println!("  ✅ Analytics logged: {} bids", result.total_bids);
                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
            TrackingTask::UpdateMetrics => {
                println!("  📊 Updating metrics...");
                tokio::time::sleep(Duration::from_millis(25)).await;

                println!("  ✅ Metrics updated");
                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
            TrackingTask::NotifyWinner => {
                println!("  📬 Notifying winner...");

                let result: AuctionResult = store.get("auction_result")?;
                if let Some(winner) = result.winner {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    println!("  ✅ Winner {} notified", winner.bid.partner_id);
                }

                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
            TrackingTask::StoreAuction => {
                println!("  💾 Storing auction data...");
                tokio::time::sleep(Duration::from_millis(35)).await;

                println!("  ✅ Auction data stored");
                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
        }
    }
}

// ============================================================================
// Phase 7: Response Building
// ============================================================================

#[derive(Clone)]
struct BuildResponseTask;

#[task(state = AdExchangeState)]
impl BuildResponseTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        println!("\n  📦 Building response...");

        let request: AdRequest = store.get("ad_request")?;
        let result: AuctionResult = store.get("auction_result")?;

        println!("\n🎯 Ad Exchange Response Summary:");
        println!("  Request ID: {}", request.request_id);
        println!("  Total Bids: {}", result.total_bids);
        println!("  Auction Time: {}ms", result.auction_time_ms);

        if let Some(winner) = result.winner {
            println!("  Winner: {}", winner.bid.partner_id);
            println!("  Winning Price: ${:.2}", winner.score);
            println!("  Creative: {}", winner.bid.creative_id);
        } else {
            println!("  Result: No Fill");
        }

        Ok(TaskResult::Single(AdExchangeState::Complete))
    }
}

// ============================================================================
// NoFill Handler
// ============================================================================

#[derive(Clone)]
struct NoFillTask;

#[task(state = AdExchangeState)]
impl NoFillTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        println!("\n⚠️  No Fill Response");
        println!("Unable to complete ad request due to timeout or insufficient data.\n");
        Ok(TaskResult::Single(AdExchangeState::Complete))
    }
}

// ============================================================================
// Invalid Response Handler
// ============================================================================

#[derive(Clone)]
struct InvalidResponseTask;

#[task(state = AdExchangeState)]
impl InvalidResponseTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        println!("\n⚠️  Invalid Request");
        println!("Request validation failed.\n");
        Ok(TaskResult::Single(AdExchangeState::Complete))
    }
}

// ============================================================================
// Phase 8: Error Tracking (Split 5 - All Strategy)
// ============================================================================

// Wrapper enum for error tracking tasks
#[derive(Clone)]
enum ErrorTrackingTask {
    LogError,
    UpdateErrorMetrics,
}

#[task(state = AdExchangeState)]
impl ErrorTrackingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        match self {
            ErrorTrackingTask::LogError => {
                println!("  📝 Logging error...");
                tokio::time::sleep(Duration::from_millis(20)).await;

                // Determine error type from store or state
                let error_type = if store.get::<AuctionResult>("auction_result").is_ok() {
                    "NoFill"
                } else {
                    "Rejected"
                };

                println!("  ✅ Error logged: {}", error_type);
                Ok(TaskResult::Single(AdExchangeState::NoFill))
            }
            ErrorTrackingTask::UpdateErrorMetrics => {
                println!("  📊 Updating error metrics...");
                tokio::time::sleep(Duration::from_millis(25)).await;

                println!("  ✅ Error metrics updated");
                Ok(TaskResult::Single(AdExchangeState::NoFill))
            }
        }
    }
}

// ============================================================================
// Main Workflow Construction
// ============================================================================

fn create_ad_exchange_workflow(store: MemoryStore) -> Workflow<AdExchangeState> {
    Workflow::new(Resources::new().insert("store", store.clone()))
        // Phase 1: Validation
        .register(AdExchangeState::Start, ValidateRequestTask)

        // Invalid Response Handler
        .register(AdExchangeState::InvalidResponse, InvalidResponseTask)

        // Phase 2: Context Gathering - SPLIT 1 (All Strategy)
        // All three must succeed to proceed within 100ms timeout
        // If any task fails or timeout is exceeded, workflow will error and transition to NoFill
        .register_split(
            AdExchangeState::GatherContext,
            vec![
                ContextTask::FetchUser,
                ContextTask::FetchGeo,
                ContextTask::DetectDevice,
            ],
            JoinConfig::new(
                JoinStrategy::All,
                AdExchangeState::RequestBids,
            )
            .with_timeout(Duration::from_millis(100)),
        )

        // Phase 3: Bid Requests - SPLIT 2 (PartialTimeout Strategy)
        // Accept whatever bids come back within 200ms
        .register_split(
            AdExchangeState::RequestBids,
            vec![
                ContactDSPTask { partner_id: "DSP-FastBidder".to_string(), response_delay_ms: 45 },
                ContactDSPTask { partner_id: "DSP-Premium".to_string(), response_delay_ms: 80 },
                ContactDSPTask { partner_id: "DSP-Global".to_string(), response_delay_ms: 120 },
                ContactDSPTask { partner_id: "DSP-Slow".to_string(), response_delay_ms: 190 },
                ContactDSPTask { partner_id: "DSP-TooSlow".to_string(), response_delay_ms: 250 },
            ],
            JoinConfig::new(
                JoinStrategy::PartialTimeout,
                AdExchangeState::ScoreBids,
            )
            .with_timeout(Duration::from_millis(200)),
        )

        // Phase 4: Bid Scoring - SPLIT 3 (All Strategy)
        // Score all received bids within 50ms timeout
        // If timeout or any scoring fails, workflow will error and transition to NoFill
        .register_split(
            AdExchangeState::ScoreBids,
            vec![
                ScoreBidTask { bid_index: 0 },
                ScoreBidTask { bid_index: 1 },
                ScoreBidTask { bid_index: 2 },
            ],
            JoinConfig::new(
                JoinStrategy::All,
                AdExchangeState::RunAuction,
            )
            .with_timeout(Duration::from_millis(50)),
        )

        // Phase 5: Auction
        .register(AdExchangeState::RunAuction, RunAuctionTask)

        // Phase 6: Tracking - SPLIT 4 (All Strategy)
        // All tracking tasks must complete within 100ms timeout
        // If timeout or any task fails, workflow will error and transition to NoFill
        .register_split(
            AdExchangeState::TrackResults,
            vec![
                TrackingTask::LogAnalytics,
                TrackingTask::UpdateMetrics,
                TrackingTask::NotifyWinner,
                TrackingTask::StoreAuction,
            ],
            JoinConfig::new(
                JoinStrategy::All,
                AdExchangeState::BuildResponse,
            )
            .with_timeout(Duration::from_millis(100)),
        )

        // Phase 7: Response
        .register(AdExchangeState::BuildResponse, BuildResponseTask)

        // NoFill handler (used when splits timeout or fail)
        .register(AdExchangeState::NoFill, NoFillTask)

        // Phase 8: Error Tracking - SPLIT 5 (All Strategy)
        // Both error logging and metrics must complete within 50ms timeout
        .register_split(
            AdExchangeState::ErrorTracking,
            vec![
                ErrorTrackingTask::LogError,
                ErrorTrackingTask::UpdateErrorMetrics,
            ],
            JoinConfig::new(
                JoinStrategy::All,
                AdExchangeState::NoFill,
            )
            .with_timeout(Duration::from_millis(50)),
        )

        // Terminal states
        .add_exit_states(vec![
            AdExchangeState::Complete,
            AdExchangeState::Rejected,
        ])
}

// ============================================================================
// Example Usage
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🚀 Real-Time Ad Exchange Workflow\n");
    println!("{}", "=".repeat(60));

    let store = MemoryStore::new();

    // Create ad request
    let request = AdRequest {
        request_id: "req_abc123".to_string(),
        placement_id: "placement_728x90_top".to_string(),
        floor_price: 1.50,
    };

    store.put("ad_request", request)?;

    // Build and execute workflow
    let workflow = create_ad_exchange_workflow(store.clone());

    println!("\n🎬 Starting ad exchange workflow...\n");
    let start = tokio::time::Instant::now();

    // Execute workflow - if splits timeout or fail, transition to NoFill
    let result = match workflow.orchestrate(AdExchangeState::Start).await {
        Ok(state) => state,
        Err(e) => {
            // If workflow fails due to split timeout/error, handle as NoFill
            eprintln!("❌ Workflow error: {}", e);
            println!("\n⚠️  Handling as No Fill due to error\n");

            // Execute ErrorTracking state explicitly
            workflow.orchestrate(AdExchangeState::ErrorTracking).await?
        }
    };

    let total_time = start.elapsed();

    println!("\n{}", "=".repeat(60));
    println!("✅ Workflow completed in {:?}", total_time);
    println!("   Final State: {:?}", result);

    Ok(())
}

```

<h3 id="ad-features"><a href="#ad-features" class="anchor-link" aria-hidden="true">#</a>Key Features Demonstrated</h3>
<div class="card-stack">
<div class="card">
<h3>Multiple Split Points</h3>
<p><strong>Split 1:</strong> Context gathering with <code>All</code> strategy - all 3 data sources required (100ms timeout)</p>
<p><strong>Split 2:</strong> Bid requests with <code>PartialTimeout</code> - accept responses within 200ms (3-4 of 5 DSPs)</p>
<p><strong>Split 3:</strong> Bid scoring with <code>All</code> strategy - score all received bids (50ms timeout)</p>
<p><strong>Split 4:</strong> Tracking with <code>All</code> strategy - all 4 tracking tasks must complete (100ms timeout)</p>
<p><strong>Split 5:</strong> Error tracking with <code>All</code> strategy - log errors and update metrics, then transition to NoFill (50ms timeout)</p>
<p><strong>Error Handling:</strong> If any <code>All</code> strategy split times out or fails, workflow gracefully transitions to ErrorTracking, which then calls NoFill before completing</p>
</div>
<div class="card">
<h3>Real-World Constraints</h3>
<p>• 200ms timeout for bid requests (industry standard)</p>
<p>• Floor price enforcement</p>
<p>• Quality scoring with latency penalties</p>
<p>• Strict tracking requirements (all systems must update)</p>
<p>• Graceful NoFill handling with error tracking when timeouts occur</p>
<p>• Comprehensive error logging and metrics on failures</p>
</div>
<div class="card">
<h3>Production Patterns</h3>
<p>• Parallel external service calls</p>
<p>• Graceful degradation (partial results)</p>
<p>• Time-based optimization (fastest wins)</p>
<p>• Complex state management across phases</p>
</div>
</div>

<h3 id="ad-output"><a href="#ad-output" class="anchor-link" aria-hidden="true">#</a>Example Output</h3>

```text
🚀 Real-Time Ad Exchange Workflow

============================================================

🎬 Starting ad exchange workflow...

🔍 Validating request req_abc123
✅ Request validated
  👤 Fetching user profile...
  📱 Detecting device...
  🌍 Fetching geo data...
  ✅ Device detected
  ✅ Geo data loaded
  ✅ User profile loaded
  📡 Requesting bid from DSP-FastBidder...
  📡 Requesting bid from DSP-Premium...
  📡 Requesting bid from DSP-Global...
  📡 Requesting bid from DSP-TooSlow...
  📡 Requesting bid from DSP-Slow...
  ✅ DSP-FastBidder bid: $2.95
  ✅ DSP-Premium bid: $3.30
  ✅ DSP-Global bid: $3.70
  📊 Scoring bid from DSP-FastBidder...
  📊 Scoring bid from DSP-Premium...
  📊 Scoring bid from DSP-Global...
  ✅ Bid scored: $3.52
  ✅ Bid scored: $3.25
  ✅ Bid scored: $3.30

  🎯 Running auction...
  🏆 Winner: DSP-Global at $3.52
  📈 Logging to analytics...
  📊 Updating metrics...
  📬 Notifying winner...
  💾 Storing auction data...
  ✅ Metrics updated
  ✅ Analytics logged: 3 bids
  ✅ Auction data stored
  ✅ Winner DSP-Global notified

  📦 Building response...

🎯 Ad Exchange Response Summary:
  Request ID: req_abc123
  Total Bids: 3
  Auction Time: 0ms
  Winner: DSP-Global
  Winning Price: $3.52
  Creative: creative_DSP-Global

============================================================
✅ Workflow completed in 307.716975ms
   Final State: Complete

```

<div class="callout callout-tip">
<p><strong>💡 Production Insight:</strong> This ad exchange workflow demonstrates how to build real-time 
bidding systems that handle 1000s of requests per second. The combination of <code>All</code> and 
<code>PartialTimeout</code> strategies ensures optimal performance while maintaining quality and reliability. 
The ~308ms total time includes 4 parallel split/join operations, showing how split/join can meet the strict 
latency requirements of programmatic advertising (typically &lt;300ms). Note how slow DSPs (DSP-TooSlow and 
DSP-Slow) are automatically cancelled when they exceed the 200ms timeout, preventing them from delaying the auction.</p>
</div>

</div>

