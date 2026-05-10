+++
title = "Split & Join"
description = "Run tasks concurrently with register_split and JoinConfig; All / Any / Quorum / Percentage / PartialResults / PartialTimeout strategies; the bulkhead."
template = "page.html"
+++

<div class="content-wrapper">

<h1>Split &amp; Join</h1>
<p class="subtitle">Fan work out across many tasks, then join the results back into the FSM.</p>

<p>
A <a href="../workflows/">workflow</a> normally runs one task per state. When you need parallelism —
scatter-gather queries, redundant API calls, batch processing, hedged requests against an SLA —
register a <strong>split</strong> for that state: a list of tasks that run concurrently, plus a
<code>JoinConfig</code> that decides when the workflow may advance and which next state it lands on.
The join strategy controls the termination condition; an optional <strong>bulkhead</strong> bounds how
many of those tasks run at once.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#split-join">Splitting a State</a></li>
<li><a href="#join-strategies">Join Strategies</a></li>
<li><a href="#bulkhead">Bounding Concurrency with a Bulkhead</a></li>
<li><a href="#complete-example">Complete Example</a></li>
<li><a href="#join-strategy-examples">Join Strategy Examples</a></li>
<li class="toc-sub"><a href="#strategy-all">All</a></li>
<li class="toc-sub"><a href="#strategy-any">Any</a></li>
<li class="toc-sub"><a href="#strategy-quorum">Quorum</a></li>
<li class="toc-sub"><a href="#strategy-percentage">Percentage</a></li>
<li class="toc-sub"><a href="#strategy-partial-results">PartialResults</a></li>
<li class="toc-sub"><a href="#strategy-partial-timeout">PartialTimeout</a></li>
<li><a href="#comparison-table">Comparison Table</a></li>
<li><a href="#parallel-patterns">Common Parallel Patterns</a></li>
<li class="toc-sub"><a href="#pattern-queue">Queue Consumer</a></li>
<li class="toc-sub"><a href="#pattern-dynamic">Dynamic Task Generation</a></li>
<li class="toc-sub"><a href="#pattern-resource">Resource-Limited Processing</a></li>
<li class="toc-sub"><a href="#pattern-continuous">Continuous Workflow</a></li>
<li><a href="#workflow-on-request">Workflow per HTTP Request</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="split-join"><a href="#split-join" class="anchor-link" aria-hidden="true">#</a>Splitting a State</h2>
<p>
Use <code>register_split(state, tasks, join_config)</code> in place of <code>register()</code> to map a
state to a set of tasks that run in parallel. <code>JoinConfig::new(strategy, next_state)</code> defines
the termination condition and the state the workflow transitions to once the strategy is satisfied;
<code>.with_timeout(duration)</code> caps how long the join waits, and <code>.with_bulkhead(n)</code>
caps how many split tasks run concurrently. Like every other builder method, <code>register_split()</code>
consumes <code>self</code> and returns a new <code>Workflow</code> — capture the return value.
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

<div class="code-block">
<span class="code-block-label">register_split + JoinConfig</span>

```rust
# use cano::prelude::*;
# use std::time::Duration;
# #[derive(Debug, Clone, PartialEq, Eq, Hash)]
# enum State { Start, Fanout, Aggregate, Complete }
# #[derive(Clone)] struct Loader;
# #[derive(Clone)] struct Worker { id: u32 }
# #[derive(Clone)] struct Aggregator;
# #[task(state = State)]
# impl Loader { async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> { Ok(TaskResult::Single(State::Fanout)) } }
# #[task(state = State)]
# impl Worker { async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> { Ok(TaskResult::Single(State::Aggregate)) } }
# #[task(state = State)]
# impl Aggregator { async fn run_bare(&self) -> Result<TaskResult<State>, CanoError> { Ok(TaskResult::Single(State::Complete)) } }
# fn build(store: MemoryStore) -> Workflow<State> {
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_timeout(Duration::from_secs(5));

Workflow::new(Resources::new().insert("store", store))
    .register(State::Start, Loader)
    .register_split(State::Fanout, vec![Worker { id: 1 }, Worker { id: 2 }, Worker { id: 3 }], join_config)
    .register(State::Aggregate, Aggregator)
    .add_exit_state(State::Complete)
# }
```
</div>

<div class="callout callout-info">
<span class="callout-label">Two ways to fan out</span>
<p>
A state can be a split because you <em>registered</em> it with <code>register_split()</code> (the tasks
and join strategy are fixed at build time), or because a single task <em>returned</em>
<code>TaskResult::Split(...)</code> at runtime to spawn a dynamic set of follow-on states. A single task
that returns <code>TaskResult::Split</code> for a state registered with plain <code>register()</code>
fails with <code>CanoError::Workflow</code> — use <code>register_split()</code> for that state.
</p>
</div>
<hr class="section-divider">

<h2 id="join-strategies"><a href="#join-strategies" class="anchor-link" aria-hidden="true">#</a>Join Strategies</h2>
<p>The <code>JoinStrategy</code> enum controls when a split is considered done and the workflow may advance.</p>

<div class="strategy-grid">
<div class="strategy-card">
<p class="strategy-name">All</p>
<p>Wait for <strong>all</strong> tasks to complete successfully.</p>

```rust
# use cano::prelude::*;
JoinStrategy::All
```
</div>
<div class="strategy-card">
<p class="strategy-name">Any</p>
<p>Proceed after the <strong>first</strong> task completes successfully.</p>

```rust
# use cano::prelude::*;
JoinStrategy::Any
```
</div>
<div class="strategy-card">
<p class="strategy-name">Quorum(n)</p>
<p>Wait for <strong>n</strong> tasks to complete successfully.</p>

```rust
# use cano::prelude::*;
JoinStrategy::Quorum(2)
```
</div>
<div class="strategy-card">
<p class="strategy-name">Percentage(p)</p>
<p>Wait for <strong>p%</strong> of tasks to complete successfully (value in <code>(0.0, 1.0]</code>).</p>

```rust
# use cano::prelude::*;
JoinStrategy::Percentage(0.5)
```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialResults(n)</p>
<p>Proceed once <strong>n</strong> tasks complete (successes <em>or</em> failures).</p>

```rust
# use cano::prelude::*;
JoinStrategy::PartialResults(3)
```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialTimeout</p>
<p>Accept whatever completes before <strong>timeout</strong> expires. Requires <code>.with_timeout()</code>.</p>

```rust
# use cano::prelude::*;
JoinStrategy::PartialTimeout
```
</div>
</div>

<div class="callout callout-warning">
<span class="callout-label">PartialTimeout needs a timeout</span>
<p>
A <code>JoinConfig</code> using <code>PartialTimeout</code> without a configured timeout fails at execution
time with <code>CanoError::Configuration</code>. Always pair it with <code>.with_timeout(duration)</code>.
</p>
</div>
<hr class="section-divider">

<h2 id="bulkhead"><a href="#bulkhead" class="anchor-link" aria-hidden="true">#</a>Bounding Concurrency with a Bulkhead</h2>
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
# use cano::prelude::*;
# use std::time::Duration;
# #[derive(Debug, Clone, PartialEq, Eq, Hash)]
# enum State { Aggregate }
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_bulkhead(4); // at most 4 split tasks run at once
```
</div>

<div class="callout callout-warning">
<span class="callout-label">Validation</span>
<p>
<code>with_bulkhead(0)</code> is rejected at execution time with <code>CanoError::Configuration</code>.
Leave the bulkhead unset (<code>None</code>) for unbounded concurrency. Bulkheads compose with
<code>PartialTimeout</code> and any other join strategy.
</p>
</div>
<hr class="section-divider">

<h2 id="complete-example"><a href="#complete-example" class="anchor-link" aria-hidden="true">#</a>Complete Example</h2>
<p>
A runnable end-to-end split/join: a loader writes input data into the store, three processor tasks
fan out in parallel, the <code>All</code> strategy waits for every one of them, and an aggregator reads
the per-task results back out. Run the full version with <code>cargo run --example workflow_split_join</code>.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Aggregate, Complete }

#[derive(Clone)]
struct DataLoader;

#[task(state = DataState)]
impl DataLoader {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("input_data", vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10])?;
        Ok(TaskResult::Single(DataState::ParallelProcessing))
    }
}

#[derive(Clone)]
struct ProcessorTask { task_id: usize }
impl ProcessorTask { fn new(task_id: usize) -> Self { Self { task_id } } }

#[task(state = DataState)]
impl ProcessorTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let data: Vec<i32> = store.get("input_data")?;
        tokio::time::sleep(Duration::from_millis(100 * self.task_id as u64)).await;
        let result: i32 = data.iter().map(|&x| x * self.task_id as i32).sum();
        store.put(&format!("result_{}", self.task_id), result)?;
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

#[derive(Clone)]
struct Aggregator;

#[task(state = DataState)]
impl Aggregator {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let total: i32 = (1..=3)
            .filter_map(|i| store.get::<i32>(&format!("result_{}", i)).ok())
            .sum();
        store.put("final_result", total)?;
        Ok(TaskResult::Single(DataState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    let processors = vec![
        ProcessorTask::new(1),
        ProcessorTask::new(2),
        ProcessorTask::new(3),
    ];

    // Wait for ALL processors, give up after 5 seconds.
    let join_config = JoinConfig::new(JoinStrategy::All, DataState::Aggregate)
        .with_timeout(Duration::from_secs(5));

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(DataState::Start, DataLoader)
        .register_split(DataState::ParallelProcessing, processors, join_config)
        .register(DataState::Aggregate, Aggregator)
        .add_exit_state(DataState::Complete);

    let result = workflow.orchestrate(DataState::Start).await?;
    let final_result: i32 = store.get("final_result")?;
    println!("Workflow completed: {:?} — total {}", result, final_result);
    Ok(())
}
```
<hr class="section-divider">

<h2 id="join-strategy-examples"><a href="#join-strategy-examples" class="anchor-link" aria-hidden="true">#</a>Join Strategy Examples</h2>
<p>Each strategy handles parallel task completion differently. The examples below isolate the
<code>JoinConfig</code> wiring for each one.</p>

<h3 id="strategy-all"><a href="#strategy-all" class="anchor-link" aria-hidden="true">#</a>All — Wait for Every Task</h3>
<p>Waits for all tasks to complete successfully. Fails if any task fails. Use it when every result is required.</p>

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
    // All Strategy: best for workflows requiring complete data
    let join_config = JoinConfig::new(JoinStrategy::All, DataState::Aggregate)
        .with_timeout(Duration::from_secs(10));

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

<h3 id="strategy-any"><a href="#strategy-any" class="anchor-link" aria-hidden="true">#</a>Any — First to Complete</h3>
<p>Proceeds as soon as the first task completes successfully; the rest are cancelled. Ideal for redundant
calls where the fastest response wins.</p>

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
impl ApiCallTask { fn new(provider: &str) -> Self { Self { provider: provider.into() } } }

#[task(state = DataState)]
impl ApiCallTask {
    async fn run_bare(&self) -> Result<TaskResult<DataState>, CanoError> {
        Ok(TaskResult::Single(DataState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<DataState> {
    // Any Strategy: best for redundant API calls or fastest-wins scenarios
    let join_config = JoinConfig::new(JoinStrategy::Any, DataState::Complete);

    // Call 3 different data sources, use whoever responds first
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

<h3 id="strategy-quorum"><a href="#strategy-quorum" class="anchor-link" aria-hidden="true">#</a>Quorum(n) — Wait for N Tasks</h3>
<p>Proceeds once a specific number of tasks complete successfully; the rest are cancelled. Useful for
distributed consensus and majority-vote writes.</p>

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
    // Quorum Strategy: write to 5 replicas, succeed when 3 confirm
    let join_config = JoinConfig::new(JoinStrategy::Quorum(3), DataState::Aggregate)
        .with_timeout(Duration::from_secs(5));

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

<h3 id="strategy-percentage"><a href="#strategy-percentage" class="anchor-link" aria-hidden="true">#</a>Percentage(p) — Wait for a Fraction of Tasks</h3>
<p>Proceeds once a percentage of tasks complete successfully. Scales with the batch size, so it works
well when the number of split tasks varies.</p>

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
    // Percentage Strategy: process 100 records, proceed when 75 complete
    let join_config = JoinConfig::new(JoinStrategy::Percentage(0.75), DataState::Aggregate)
        .with_timeout(Duration::from_secs(10));

    let tasks: Vec<RecordProcessor> = (0..100).map(RecordProcessor::new).collect();

    Workflow::new(Resources::new().insert("store", store))
        .register(DataState::Start, LoadRecords)
        .register_split(DataState::ParallelProcessing, tasks, join_config)
        .register(DataState::Aggregate, SummarizeResults)
        .add_exit_state(DataState::Complete)
}
```

<h3 id="strategy-partial-results"><a href="#strategy-partial-results" class="anchor-link" aria-hidden="true">#</a>PartialResults(n) — Accept Partial Completion</h3>
<p>
Proceeds once <code>n</code> tasks have <em>completed</em> — successes or failures both count — and
cancels the rest. All outcomes are tracked, so a downstream task can inspect how many succeeded versus
failed. Good for latency-bounded fan-out where some failures are tolerable. A fuller, runnable
walk-through lives in <code>cargo run --example workflow_partial_results</code>.
</p>

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
Note over W: 3 completions → Proceed
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
    // PartialResults Strategy: proceed after any 3 of 4 service calls finish
    let join_config = JoinConfig::new(JoinStrategy::PartialResults(3), DataState::Aggregate)
        .with_timeout(Duration::from_secs(5));

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

<h3 id="strategy-partial-timeout"><a href="#strategy-partial-timeout" class="anchor-link" aria-hidden="true">#</a>PartialTimeout — Deadline-Based Completion</h3>
<p>Accepts whatever has completed when the timeout expires and proceeds with those results — the
remaining tasks are cancelled. The go-to strategy for hard SLAs. Requires <code>.with_timeout()</code>.</p>

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
    // PartialTimeout Strategy: real-time recommendations with a 500ms SLA
    let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, DataState::Aggregate)
        .with_timeout(Duration::from_millis(500));

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
<td>N tasks complete (success or failure)</td>
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

<h2 id="parallel-patterns"><a href="#parallel-patterns" class="anchor-link" aria-hidden="true">#</a>Common Parallel Patterns</h2>
<p>
Split/Join handles complex parallel processing within a single workflow. Below are real-world
patterns that fan out work across many tasks and join results back into the FSM. Use the
<strong>Queue Consumer</strong> pattern for external queues (SQS, Redis, Kafka), <strong>Dynamic Task
Generation</strong> when the task count depends on runtime data, <strong>Resource-Limited Processing</strong>
to cap concurrent operations, and the <strong>Continuous Workflow</strong> pattern for scheduled parallel
batches. They all give you the parallelism of running many concurrent workflow instances, with a simpler
mental model, better resource control, and type-safe state management.
</p>

<h3 id="pattern-queue"><a href="#pattern-queue" class="anchor-link" aria-hidden="true">#</a>Pattern 1: Queue Consumer with Batch Processing</h3>
<p>
Process items from a queue in parallel batches. Instead of running multiple workflow instances concurrently,
use a single workflow that pulls a batch, splits it into per-item processors via a task that returns
<code>TaskResult::Split</code>, and loops until the queue drains.
</p>

```rust
use cano::prelude::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum QueueState { PullBatch, ProcessBatch, Complete }

// Simulated queue (in production, use Redis, SQS, etc.)
type SharedQueue = Arc<Mutex<VecDeque<String>>>;

#[derive(Clone)]
struct QueuePuller { queue: SharedQueue, batch_size: usize }

#[task(state = QueueState)]
impl QueuePuller {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let mut queue = self.queue.lock().await;

        let mut batch = Vec::new();
        for _ in 0..self.batch_size {
            match queue.pop_front() {
                Some(item) => batch.push(item),
                None => break,
            }
        }

        if batch.is_empty() {
            tokio::time::sleep(Duration::from_secs(1)).await;
            return Ok(TaskResult::Single(QueueState::PullBatch));
        }

        store.put("current_batch", batch)?;
        Ok(TaskResult::Single(QueueState::ProcessBatch))
    }
}

#[derive(Clone)]
struct ItemProcessor { item_id: String }

#[task(state = QueueState)]
impl ItemProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        store.put(&format!("result_{}", self.item_id), "completed")?;
        Ok(TaskResult::Single(QueueState::Complete))
    }
}

#[derive(Clone)]
struct BatchSplitter;

#[task(state = QueueState)]
impl BatchSplitter {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let batch: Vec<String> = store.get("current_batch")?;
        if batch.is_empty() {
            return Ok(TaskResult::Single(QueueState::PullBatch));
        }
        // One follow-on Complete state per item — processed in parallel.
        Ok(TaskResult::Split(batch.iter().map(|_| QueueState::Complete).collect()))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
    let queue = Arc::new(Mutex::new(VecDeque::from(vec![
        "order1".into(), "order2".into(), "order3".into(), "order4".into(), "order5".into(),
    ])));

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(QueueState::PullBatch, QueuePuller { queue: queue.clone(), batch_size: 10 })
        .register(QueueState::ProcessBatch, BatchSplitter)
        .add_exit_state(QueueState::Complete);

    loop {
        let result = workflow.orchestrate(QueueState::PullBatch).await?;
        if result == QueueState::Complete && queue.lock().await.is_empty() {
            break;
        }
    }

    println!("All items processed");
    Ok(())
}
```

<h3 id="pattern-dynamic"><a href="#pattern-dynamic" class="anchor-link" aria-hidden="true">#</a>Pattern 2: Dynamic Task Generation</h3>
<p>
Build the list of parallel tasks from runtime data before constructing the workflow. A loader task
writes the dataset into the store; each processor reads its slice by index; an aggregator runs once
after the split joins.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { LoadRecords, ProcessBatch, Aggregate, Complete }

#[derive(Clone)]
struct RecordLoader;

#[task(state = DataState)]
impl RecordLoader {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("records", (1..=100).collect::<Vec<i32>>())?;
        Ok(TaskResult::Single(DataState::ProcessBatch))
    }
}

#[derive(Clone)]
struct RecordProcessor { index: usize }

#[task(state = DataState)]
impl RecordProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let records: Vec<i32> = store.get("records")?;
        tokio::time::sleep(Duration::from_millis(10)).await;
        store.put(&format!("result_{}", self.index), records[self.index] * 2)?;
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

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

    // Build the processor tasks before constructing the workflow.
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
Cap parallelism when a downstream resource (API keys, connections) is scarce. The
<a href="#bulkhead">bulkhead</a> on <code>JoinConfig</code> is the built-in way to do this; the example
below shows the manual alternative — a shared <code>tokio::sync::Semaphore</code> acquired inside each
task — for cases where you need the limit to span more than one split.
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
struct RateLimitedApiTask { api_id: usize, semaphore: Arc<Semaphore> }

#[task(state = ApiState)]
impl RateLimitedApiTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ApiState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let _permit = self.semaphore.acquire().await
            .map_err(|e| CanoError::task_execution(e.to_string()))?;
        let result = make_api_call(self.api_id).await?;
        store.put(&format!("api_result_{}", self.api_id), result)?;
        Ok(TaskResult::Single(ApiState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<ApiState> {
    // 20 tasks, at most 5 in flight at once.
    let semaphore = Arc::new(Semaphore::new(5));
    let tasks: Vec<RateLimitedApiTask> = (0..20)
        .map(|i| RateLimitedApiTask { api_id: i, semaphore: semaphore.clone() })
        .collect();
    let join_config = JoinConfig::new(JoinStrategy::All, ApiState::Complete);

    Workflow::new(Resources::new().insert("store", store))
        .register_split(ApiState::Start, tasks, join_config)
        .add_exit_state(ApiState::Complete)
}
```

<div class="callout callout-tip">
<span class="callout-label">Tip</span>
<p>
For most cases, <code>JoinConfig::with_bulkhead(n)</code> is simpler than a hand-rolled semaphore —
it gates the split's task bodies on a semaphore for you and still applies the join strategy normally.
Reach for the manual approach only when the limit must be shared across multiple splits or workflows.
</p>
</div>

<h3 id="pattern-continuous"><a href="#pattern-continuous" class="anchor-link" aria-hidden="true">#</a>Pattern 4: Continuous Workflow with Split/Join</h3>
<p>
Combine the <a href="../scheduler/">scheduler</a> with split/join for continuous parallel processing.
Because the split tasks are registered statically, the batch size is fixed per workflow instance —
size it for your throughput and let the scheduler re-run it on an interval.
</p>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProcessState { Start, ProcessBatch, Complete }

async fn fetch_pending_work() -> Result<Vec<String>, CanoError> {
    Ok(vec!["job-1".to_string(), "job-2".to_string()])
}

#[derive(Clone)]
struct WorkProcessor { item_index: usize }

#[task(state = ProcessState)]
impl WorkProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ProcessState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let items: Vec<String> = store.get("work_items")?;
        if let Some(item) = items.get(self.item_index) {
            println!("Processing item: {}", item);
        }
        Ok(TaskResult::Single(ProcessState::Complete))
    }
}

#[derive(Clone)]
struct BatchLoaderTask;

#[task(state = ProcessState)]
impl BatchLoaderTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ProcessState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let items = fetch_pending_work().await?;
        if items.is_empty() {
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

    let batch_size = 10usize;
    let processors: Vec<WorkProcessor> = (0..batch_size).map(|i| WorkProcessor { item_index: i }).collect();
    let join_config = JoinConfig::new(JoinStrategy::All, ProcessState::Complete);

    let batch_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(ProcessState::Start, BatchLoaderTask)
        .register_split(ProcessState::ProcessBatch, processors, join_config)
        .add_exit_state(ProcessState::Complete);

    scheduler.every_seconds("batch_processor", batch_workflow, ProcessState::Start, 10)?;
    scheduler.start().await?;
    Ok(())
}
```
<hr class="section-divider">

<h2 id="workflow-on-request"><a href="#workflow-on-request" class="anchor-link" aria-hidden="true">#</a>Workflow per HTTP Request</h2>
<p>
A very common pattern is triggering a workflow for every incoming HTTP request, running the FSM to
completion, and returning the results in the response. Cano workflows are cheap to construct — tasks are
small <code>Clone</code> structs wrapped in <code>Arc</code> internally — so building one per request is
fast, and split/join inside the workflow gives you per-request parallelism for free.
</p>

<div class="callout callout-warning">
<span class="callout-label">Store isolation</span>
<p>
The <code>MemoryStore</code> is <code>Arc</code>-wrapped, so cloning a workflow shares the same store. For
request-scoped data, create a <strong>fresh store per request</strong> to avoid data leaking between
concurrent requests.
</p>
</div>

<div class="diagram-frame">
<p class="diagram-label">Workflow per Request</p>
<div class="mermaid">
sequenceDiagram
participant Client
participant Handler as HTTP Handler
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

<p>The shape of the handler:</p>
<ol>
<li>Create a <strong>fresh <code>MemoryStore</code></strong> for the request and write the request data into it with <code>store.put()</code>.</li>
<li>Build the workflow with a <strong>factory function</strong> that takes the store, then call <code>workflow.orchestrate(initial_state)</code> to drive the FSM to an exit state.</li>
<li>Read the results back out of the store and return them in the HTTP response. The exit state returned by <code>orchestrate()</code> tells you which terminal branch ran — use it for success/error response logic.</li>
</ol>

```rust
# use cano::prelude::*;
# use std::time::Duration;
# #[derive(Debug, Clone, PartialEq, Eq, Hash)]
# enum TextPipelineState { Parse, Transform, Done }
# #[derive(Clone)] struct ParseTask;
# #[derive(Clone)] struct TransformTask;
# #[task(state = TextPipelineState)]
# impl ParseTask { async fn run_bare(&self) -> Result<TaskResult<TextPipelineState>, CanoError> { Ok(TaskResult::Single(TextPipelineState::Transform)) } }
# #[task(state = TextPipelineState)]
# impl TransformTask { async fn run_bare(&self) -> Result<TaskResult<TextPipelineState>, CanoError> { Ok(TaskResult::Single(TextPipelineState::Done)) } }
// One factory, called per request with a fresh store.
fn build_workflow(store: MemoryStore) -> Workflow<TextPipelineState> {
    Workflow::new(Resources::new().insert("store", store))
        .register(TextPipelineState::Parse, ParseTask)
        .register(TextPipelineState::Transform, TransformTask)
        .add_exit_state(TextPipelineState::Done)
        .with_timeout(Duration::from_secs(5))
}

# async fn handle(text: String) -> Result<usize, CanoError> {
// Inside an HTTP handler:
let store = MemoryStore::new();              // fresh store — full isolation
store.put("input_text", text)?;
let workflow = build_workflow(store.clone());
let final_state = workflow.orchestrate(TextPipelineState::Parse).await?;
let word_count: usize = store.get("word_count")?;
# let _ = final_state;
# Ok(word_count)
# }
```

<div class="callout callout-tip">
<span class="callout-label">Tip</span>
<p>
Use <code>.with_timeout()</code> on the workflow to keep a hung request from blocking indefinitely. For
read-heavy workloads with shared reference data, pre-populate one store, share it via <code>Arc</code>,
and use per-request keys to avoid collisions. The full Axum version is in
<code>cargo run --example workflow_on_request</code>.
</p>
</div>

<p>
For a larger, production-shaped example that chains several split/join points with mixed strategies —
context gathering with <code>All</code>, bidding with <code>PartialTimeout</code>, scoring and tracking
with <code>All</code>, plus graceful error fan-out — see <code>cargo run --example workflow_ad_exchange</code>.
</p>

</div>
