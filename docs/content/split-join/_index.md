+++
title = "Split & Join"
description = "Run tasks concurrently with register_split and JoinConfig; All / Any / Quorum / Percentage / PartialResults / PartialTimeout strategies; the bulkhead."
template = "section.html"
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
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_timeout(Duration::from_secs(5));

Workflow::new(Resources::new().insert("store", store))
    .register(State::Start, Loader)
    .register_split(State::Fanout, vec![Worker { id: 1 }, Worker { id: 2 }, Worker { id: 3 }], join_config)
    .register(State::Aggregate, Aggregator)
    .add_exit_state(State::Complete)
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
<p>Wait for <strong>p%</strong> of tasks to complete successfully (value in <code>(0.0, 1.0]</code>).</p>

```rust
JoinStrategy::Percentage(0.5)
```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialResults(n)</p>
<p>Proceed once <strong>n</strong> tasks complete (successes <em>or</em> failures).</p>

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

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example split_bulkhead</code> — 8 split tasks behind a
<code>with_bulkhead(2)</code>, with start/end timestamps printed so you can see the 4 waves of 2.</p>
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

    let result = workflow.orchestrate(DataState::Start, CancellationToken::disabled()).await?;
    let final_result: i32 = store.get("final_result")?;
    println!("Workflow completed: {:?} — total {}", result, final_result);
    Ok(())
}
```
<hr class="section-divider">

<h2 id="continue">Continue reading</h2>
<p>This page covers splitting a state, the strategy options, the bulkhead, and a complete example. Two companion pages go deeper:</p>
<ul>
<li><a href="join-strategies/">Join Strategies Reference &amp; Examples</a> — each of the six strategies wired up in code, plus a side-by-side comparison table.</li>
<li><a href="parallel-patterns/">Common Parallel Patterns</a> — queue consumers, dynamic fan-out, resource-limited processing, and scheduled parallel batches.</li>
</ul>
</div>
