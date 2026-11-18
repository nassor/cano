+++
title = "Split and join"
description = "Run tasks concurrently with register_split and JoinConfig: the six join strategies and the bulkhead."
template = "section.html"
+++

<div class="content-wrapper">

<h1>Split and join</h1>
<p class="subtitle">Run several tasks concurrently in one state, then join the results back into the workflow.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.7.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>

<p>
A <a href="../workflows/">workflow</a> normally runs one task per state. A split registers a list of
tasks for one state. They run concurrently, and a <code>JoinConfig</code> decides when the workflow
advances and which state it enters next. Use it for scatter-gather queries, redundant API calls and
hedged requests against an SLA. An optional bulkhead caps how many of the tasks run at once.
</p>
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#split-join">Splitting a state</a></li>
<li><a href="#join-strategies">Join strategies</a></li>
<li><a href="#bulkhead">Bounding concurrency with a bulkhead</a></li>
<li><a href="#complete-example">Complete example</a></li>
<li><a href="#continue">Continue reading</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="split-join"><a href="#split-join" class="anchor-link" aria-hidden="true">#</a>Splitting a state</h2>
<p>
<code>register_split(state, tasks, join_config)</code> takes the place of <code>register()</code> for a
state. <code>tasks</code> is any iterator of one task type, such as a <code>Vec</code> or a mapped range.
Each task keeps its own <code>config()</code>, so retries and attempt timeouts apply per task. A task's
returned state is discarded: the workflow always continues at the join state.
</p>
<p>
<code>validate()</code> checks that the join state is registered or an exit state. <code>JoinConfig</code>
has one constructor and two optional settings:
</p>

<dl class="key-list">
<dt><code>JoinConfig::new(strategy, join_state)</code></dt>
<dd>Sets the <a href="#join-strategies">join strategy</a> and the state the workflow enters once it is satisfied.</dd>

<dt><code>.with_timeout(duration)</code> <small>default none</small></dt>
<dd>Caps how long the join waits, counted from when the tasks are spawned, so time queued behind a bulkhead counts. Expiry fails the join with a <code>CanoError::Workflow</code> error, except for <code>PartialTimeout</code>, which advances with what finished.</dd>

<dt><code>.with_bulkhead(n)</code> <small>default unbounded</small></dt>
<dd>At most <code>n</code> tasks run at once. See <a href="#bulkhead">the bulkhead</a>.</dd>
</dl>

<div class="diagram-frame">
<p class="diagram-label">Split and join</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 4 720 422" role="img" aria-labelledby="splitjoin-title splitjoin-desc">
<title id="splitjoin-title">A split state fans out to three concurrent tasks that feed a join strategy</title>
<desc id="splitjoin-desc">The state registered with register_split starts three tasks at once. Their results flow into the join strategy. When the strategy is satisfied, the workflow advances to the join state. When it is not satisfied, or the timeout expires, orchestrate returns Err with CanoError::Workflow.</desc>
<defs><marker id="splitjoin-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="280" y="20" width="160" height="46" rx="10"/>
<text class="t-strong" x="360" y="44">Split state</text>
<path class="e" d="M360,66 V88 Q360,96 352,96 H148 Q140,96 140,104 V122" marker-end="url(#splitjoin-ah)"/>
<path class="e" d="M360,66 V122" marker-end="url(#splitjoin-ah)"/>
<path class="e" d="M360,66 V88 Q360,96 368,96 H572 Q580,96 580,104 V122" marker-end="url(#splitjoin-ah)"/>
<text class="t-mut ta-s" x="371" y="80">run concurrently</text>
<rect class="n" x="80" y="126" width="120" height="46" rx="10"/>
<text x="140" y="150">Task 1</text>
<rect class="n" x="300" y="126" width="120" height="46" rx="10"/>
<text x="360" y="150">Task 2</text>
<rect class="n" x="520" y="126" width="120" height="46" rx="10"/>
<text x="580" y="150">Task 3</text>
<path class="e" d="M140,172 V196 Q140,204 148,204 H292 Q300,204 300,212 V249" marker-end="url(#splitjoin-ah)"/>
<path class="e" d="M360,172 V232" marker-end="url(#splitjoin-ah)"/>
<path class="e" d="M580,172 V196 Q580,204 572,204 H428 Q420,204 420,212 V249" marker-end="url(#splitjoin-ah)"/>
<path class="n-hot" d="M360,232 L490,270 L360,308 L230,270 Z"/>
<text class="t-strong" x="360" y="271">Join strategy</text>
<path class="e e-hot" d="M230,270 H198 Q190,270 190,278 V358" marker-end="url(#splitjoin-ah)"/>
<text class="t-mut t-hot ta-e" x="181" y="316">satisfied</text>
<path class="e" d="M490,270 H522 Q530,270 530,278 V358" marker-end="url(#splitjoin-ah)"/>
<text class="t-mut ta-s" x="539" y="316">not met, or timeout</text>
<rect class="n-ok" x="105" y="364" width="170" height="46" rx="10"/>
<text x="190" y="388">Join state</text>
<rect class="n-err" x="425" y="364" width="210" height="46" rx="10"/>
<text class="t-code" x="530" y="388">Err(CanoError::Workflow)</text>
</svg>
</div>
</div>

<div class="code-block">
<span class="code-block-label">register_split + JoinConfig</span>

```rust
// Wait for all 3 branches to succeed within 5 s, then enter State::Aggregate.
// Individual branch return states are ignored; JoinConfig picks the join state.
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_timeout(Duration::from_secs(5));

let workflow = Workflow::new(Resources::new().insert(board))
    .register(State::Start, Loader)
    // Spawn all Worker branches concurrently when entering State::Fanout.
    .register_split(State::Fanout, [Worker { id: 1 }, Worker { id: 2 }, Worker { id: 3 }], join_config)
    .register(State::Aggregate, Aggregator)
    .add_exit_state(State::Complete);
```
</div>

<hr class="section-divider">

<h2 id="join-strategies"><a href="#join-strategies" class="anchor-link" aria-hidden="true">#</a>Join strategies</h2>
<p>
The <code>JoinStrategy</code> decides when a split is done. <code>Any</code> and
<code>PartialResults</code> abort the tasks still running as soon as they are satisfied, and
<code>PartialTimeout</code> aborts them at the timeout. The others wait for every task. Each strategy
has a timeline on the <a href="join-strategies/">Join strategies</a> page.
</p>

<div class="strategy-grid">
<div class="strategy-card">
<p class="strategy-name">All</p>
<p>Joins when every task has succeeded.</p>

```rust
JoinStrategy::All
```
</div>
<div class="strategy-card">
<p class="strategy-name">Any</p>
<p>Joins on the first success.</p>

```rust
JoinStrategy::Any
```
</div>
<div class="strategy-card">
<p class="strategy-name">Quorum(n)</p>
<p>Joins when at least <code>n</code> tasks succeeded, after every task has finished.</p>

```rust
JoinStrategy::Quorum(2)
```
</div>
<div class="strategy-card">
<p class="strategy-name">Percentage(p)</p>
<p>Joins when at least a fraction <code>p</code> of the tasks succeeded, after every task has finished. <code>p</code> is in <code>(0.0, 1.0]</code>.</p>

```rust
JoinStrategy::Percentage(0.5)
```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialResults(n)</p>
<p>Joins on the <code>n</code>th success. Failures are tolerated.</p>

```rust
JoinStrategy::PartialResults(3)
```
</div>
<div class="strategy-card">
<p class="strategy-name">PartialTimeout</p>
<p>Joins when every task has finished or the timeout expires, with whatever completed by then. Requires <code>.with_timeout()</code>.</p>

```rust
JoinStrategy::PartialTimeout
```
</div>
</div>

<div class="callout callout-warning">
<span class="callout-label">PartialTimeout needs a timeout</span>
<p>
<code>validate()</code> rejects a <code>PartialTimeout</code> split without a timeout with
<code>CanoError::Configuration</code>, before the workflow runs.
</p>
</div>
<hr class="section-divider">

<h2 id="bulkhead"><a href="#bulkhead" class="anchor-link" aria-hidden="true">#</a>Bounding concurrency with a bulkhead</h2>
<p>
A split spawns all its tasks at once. A bulkhead caps how many run at the same time: each task acquires
a permit from a shared <code>tokio::sync::Semaphore</code> before it runs, and the excess waits for a
permit. The join strategy still applies as results arrive. Use it to bound resource use or protect a
downstream service.
</p>

<div class="code-block">
<span class="code-block-label">Bulkhead config</span>

```rust
// At most 4 split tasks run at once; excess branches queue on a semaphore permit
// (and any with_timeout() budget includes that queue wait).
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_bulkhead(4);
```
</div>

<div class="callout callout-warning">
<span class="callout-label">Validation</span>
<p>
<code>validate()</code> rejects <code>with_bulkhead(0)</code>, and a <code>Percentage</code> outside
<code>(0.0, 1.0]</code>, with <code>CanoError::Configuration</code>. It runs before every
<code>orchestrate</code> and <code>resume_from</code>, so these never surface mid-run.
</p>
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example split_bulkhead</code> runs 8 tasks behind
<code>with_bulkhead(2)</code> and prints start and end timestamps, so you can see 4 waves of 2.</p>
</div>
<hr class="section-divider">

<h2 id="complete-example"><a href="#complete-example" class="anchor-link" aria-hidden="true">#</a>Complete example</h2>
<p>
A loader writes input data into the blackboard, three processors fan out in parallel, the <code>All</code>
strategy waits for every one of them, and an aggregator reads the per-task results back out. Run the
full version with <code>cargo run --example workflow_split_join</code>.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { Start, ParallelProcessing, Aggregate, Complete }

struct DataLoader;

#[task(state = DataState)]
impl DataLoader {
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let board = res.get::<Blackboard>()?;
        // Publish shared input once before fanning out across parallel branches.
        board.put("input_data", vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        Ok(DataState::ParallelProcessing)
    }
}

struct ProcessorTask {
    task_id: usize,
}

#[task(state = DataState)]
impl ProcessorTask {
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let board = res.get::<Blackboard>()?;
        // Zero-copy Arc<Vec<i32>> read shared across concurrent split branches.
        let data = board.get_shared::<Vec<i32>>("input_data")?;
        tokio::time::sleep(Duration::from_millis(100 * self.task_id as u64)).await;

        let result: i32 = data.iter().map(|&x| x * self.task_id as i32).sum();
        // Write to a branch-specific key so concurrent workers never overwrite each other.
        board.put(&format!("result_{}", self.task_id), result);

        // Return value is ignored; JoinConfig moves the workflow to DataState::Aggregate.
        Ok(DataState::Aggregate)
    }
}

struct Aggregator;

#[task(state = DataState)]
impl Aggregator {
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let board = res.get::<Blackboard>()?;
        // JoinStrategy::All guarantees all 3 branches succeeded before Aggregate runs.
        let mut total = 0i32;
        for id in 1..=3 {
            total += board.get::<i32>(&format!("result_{id}"))?;
        }
        board.put("final_result", total);
        Ok(DataState::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();

    let processors = [
        ProcessorTask { task_id: 1 },
        ProcessorTask { task_id: 2 },
        ProcessorTask { task_id: 3 },
    ];

    // Wait for ALL processors; abort and return CanoError::Workflow if 5 s elapses.
    let join_config = JoinConfig::new(JoinStrategy::All, DataState::Aggregate)
        .with_timeout(Duration::from_secs(5));

    let workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(DataState::Start, DataLoader)
        .register_split(DataState::ParallelProcessing, processors, join_config)
        .register(DataState::Aggregate, Aggregator)
        .add_exit_state(DataState::Complete);

    let result = workflow.orchestrate(DataState::Start).await?;
    let final_result: i32 = board.get("final_result")?;
    println!("Workflow completed: {result:?}, total {final_result}");
    Ok(())
}
```
<hr class="section-divider">

<h2 id="continue">Continue reading</h2>
<ul>
<li><a href="join-strategies/">Join strategies</a>: each of the six strategies with a timeline, its wiring, and a comparison table.</li>
<li><a href="parallel-patterns/">Common Parallel Patterns</a>: queue consumers, dynamic fan-out, resource-limited processing, and scheduled parallel batches.</li>
</ul>
</div>
