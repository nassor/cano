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
<code>.with_timeout(duration)</code> caps how long the join waits (for every strategy except
<code>PartialTimeout</code> an expired timeout is a <code>CanoError::Workflow</code> error), and
<code>.with_bulkhead(n)</code> caps how many split tasks run concurrently. Like every other builder
method, <code>register_split()</code> consumes <code>self</code> and returns a new <code>Workflow</code>;
capture the return value. <code>tasks</code> accepts any iterator, so a <code>Vec</code> or a mapped range
both work, and <code>validate()</code> checks that the join state is registered or an exit state.
</p>

<div class="diagram-frame">
<p class="diagram-label">Split / Join Pattern</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 440" role="img">
<title>register_split fans one Process State out to three tasks that run concurrently; the join strategy gate collects their results and advances the workflow to the aggregate state once satisfied, or to the error state on failure or timeout.</title>
<defs><marker id="split-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="280" y="20" width="160" height="46" rx="10"/>
<text class="t-strong" x="360" y="44">Process State</text>
<path class="e" d="M360,66 V88 Q360,96 352,96 H148 Q140,96 140,104 V122" marker-end="url(#split-ah)"/>
<path class="e" d="M360,66 V122" marker-end="url(#split-ah)"/>
<path class="e" d="M360,66 V88 Q360,96 368,96 H572 Q580,96 580,104 V122" marker-end="url(#split-ah)"/>
<text class="t-mut ta-s" x="371" y="80">Split</text>
<rect class="n" x="80" y="126" width="120" height="46" rx="10"/>
<text x="140" y="150">Task 1</text>
<rect class="n" x="300" y="126" width="120" height="46" rx="10"/>
<text x="360" y="150">Task 2</text>
<rect class="n" x="520" y="126" width="120" height="46" rx="10"/>
<text x="580" y="150">Task 3</text>
<path class="e" d="M140,172 V196 Q140,204 148,204 H292 Q300,204 300,212 V245" marker-end="url(#split-ah)"/>
<path class="e" d="M360,172 V228" marker-end="url(#split-ah)"/>
<path class="e" d="M580,172 V196 Q580,204 572,204 H428 Q420,204 420,212 V245" marker-end="url(#split-ah)"/>
<path class="n-hot" d="M360,232 L490,270 L360,308 L230,270 Z"/>
<text class="t-strong" x="360" y="271">Join Strategy</text>
<path class="e e-hot" d="M230,270 H198 Q190,270 190,278 V358" marker-end="url(#split-ah)"/>
<text class="t-mut t-hot ta-e" x="181" y="316">Satisfied</text>
<path class="e" d="M490,270 H522 Q530,270 530,278 V358" marker-end="url(#split-ah)"/>
<text class="t-mut ta-s" x="539" y="316">Failed / Timeout</text>
<rect class="n-ok" x="105" y="364" width="170" height="46" rx="10"/>
<text x="190" y="388">Aggregate State</text>
<rect class="n-err" x="460" y="364" width="140" height="46" rx="10"/>
<text x="530" y="388">Error State</text>
</svg>
</div>
</div>

<div class="code-block">
<span class="code-block-label">register_split + JoinConfig</span>

```rust
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_timeout(Duration::from_secs(5));

Workflow::new(Resources::new().insert(store))
    .register(State::Start, Loader)
    .register_split(State::Fanout, vec![Worker { id: 1 }, Worker { id: 2 }, Worker { id: 3 }], join_config)
    .register(State::Aggregate, Aggregator)
    .add_exit_state(State::Complete)
```
</div>

<hr class="section-divider">

<h2 id="join-strategies"><a href="#join-strategies" class="anchor-link" aria-hidden="true">#</a>Join Strategies</h2>
<p>The <code>JoinStrategy</code> enum controls when a split is considered done and the workflow may advance.
Only <code>Any</code> and <code>PartialResults</code> stop early and cancel the tasks still running; the
other strategies wait for every task to finish (or the timeout to expire) and then check the outcome.</p>

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
<p>Succeed when at least <strong>n</strong> tasks succeeded; every task still runs to completion.</p>

```rust
JoinStrategy::Quorum(2)
```
</div>
<div class="strategy-card">
<p class="strategy-name">Percentage(p)</p>
<p>Succeed when at least <strong>p%</strong> of tasks succeeded (value in <code>(0.0, 1.0]</code>); every task still runs to completion.</p>

```rust
JoinStrategy::Percentage(0.5)
```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialResults(n)</p>
<p>Proceed once <strong>n</strong> tasks have <em>succeeded</em>; the rest are cancelled and failures are tolerated.</p>

```rust
JoinStrategy::PartialResults(3)
```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialTimeout</p>
<p>Accept whatever completes (successes <em>or</em> failures) before the <strong>timeout</strong> expires. Requires <code>.with_timeout()</code>.</p>

```rust
JoinStrategy::PartialTimeout
```
</div>
</div>

<div class="callout callout-warning">
<span class="callout-label">PartialTimeout needs a timeout</span>
<p>
A <code>JoinConfig</code> using <code>PartialTimeout</code> without a configured timeout is rejected by
<code>validate()</code> with <code>CanoError::Configuration</code>, before the workflow runs. Always pair it with <code>.with_timeout(duration)</code>.
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
<code>with_bulkhead(0)</code> is rejected by <code>validate()</code> with <code>CanoError::Configuration</code>,
as is a <code>Percentage</code> outside <code>(0.0, 1.0]</code>. <code>validate()</code> runs before every
<code>orchestrate</code> / <code>resume_from</code>, so these never surface mid-run. Leave the bulkhead unset
(<code>None</code>) for unbounded concurrency. Bulkheads compose with
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
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        store.put("input_data", vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        Ok(DataState::ParallelProcessing)
    }
}

#[derive(Clone)]
struct ProcessorTask { task_id: usize }
impl ProcessorTask { fn new(task_id: usize) -> Self { Self { task_id } } }

#[task(state = DataState)]
impl ProcessorTask {
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let data: Vec<i32> = store.get("input_data")?;
        tokio::time::sleep(Duration::from_millis(100 * self.task_id as u64)).await;
        let result: i32 = data.iter().map(|&x| x * self.task_id as i32).sum();
        store.put(&format!("result_{}", self.task_id), result);
        Ok(DataState::Aggregate)
    }
}

#[derive(Clone)]
struct Aggregator;

#[task(state = DataState)]
impl Aggregator {
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let total: i32 = (1..=3)
            .filter_map(|i| store.get::<i32>(&format!("result_{}", i)).ok())
            .sum();
        store.put("final_result", total);
        Ok(DataState::Complete)
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

    let workflow = Workflow::new(Resources::new().insert(store.clone()))
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

<h2 id="continue">Continue reading</h2>
<p>This page covers splitting a state, the strategy options, the bulkhead, and a complete example. Two companion pages go deeper:</p>
<ul>
<li><a href="join-strategies/">Join Strategies Reference &amp; Examples</a> — each of the six strategies wired up in code, plus a side-by-side comparison table.</li>
<li><a href="parallel-patterns/">Common Parallel Patterns</a> — queue consumers, dynamic fan-out, resource-limited processing, and scheduled parallel batches.</li>
</ul>
</div>
