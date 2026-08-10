+++
title = "Join Strategies"
description = "Cano join strategies in depth: All, Any, Quorum, Percentage, PartialResults, and PartialTimeout, each wired up in code, with a comparison table."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Join Strategies</h1>
<p class="subtitle">Pick a completion condition for a split, and see each strategy wired up in code.</p>

<p>
A split advances when its <code>JoinStrategy</code> is satisfied. This page works through all six
strategies with runnable <code>JoinConfig</code> wiring, then compares them side by side. See
<a href="../">Split &amp; Join</a> for how splitting a state and the bulkhead work.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#join-strategy-examples">Join Strategy Examples</a></li>
<li class="toc-sub"><a href="#strategy-all">All</a></li>
<li class="toc-sub"><a href="#strategy-any">Any</a></li>
<li class="toc-sub"><a href="#strategy-quorum">Quorum</a></li>
<li class="toc-sub"><a href="#strategy-percentage">Percentage</a></li>
<li class="toc-sub"><a href="#strategy-partial-results">PartialResults</a></li>
<li class="toc-sub"><a href="#strategy-partial-timeout">PartialTimeout</a></li>
<li><a href="#comparison-table">Comparison Table</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="join-strategy-examples"><a href="#join-strategy-examples" class="anchor-link" aria-hidden="true">#</a>Join Strategy Examples</h2>
<p>Each strategy handles parallel task completion differently. The examples below isolate the
<code>JoinConfig</code> wiring for each one.</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example join_strategies</code> — runs the same parallel split four
times with <code>Any</code>, <code>Quorum</code>, <code>Percentage</code>, and <code>PartialResults</code>,
with staggered task delays so you can see exactly when each one returns.</p>
</div>

<h3 id="strategy-all"><a href="#strategy-all" class="anchor-link" aria-hidden="true">#</a>All — Wait for Every Task</h3>
<p>Waits for all tasks to complete successfully. Fails if any task fails. Use it when every result is required.</p>

<div class="diagram-frame">
<p class="diagram-label">All Strategy</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 840 374" role="img" style="max-width: 860px">
<title>All strategy: the workflow starts three tasks, every one of them reports success, and only when the last result lands does the join fire — nothing is cancelled.</title>
<defs><marker id="all-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="10" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="80" y="28">Workflow</text>
<rect class="n" x="180" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="250" y="28">Task 1</text>
<rect class="n" x="350" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="420" y="28">Task 2</text>
<rect class="n" x="520" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="590" y="28">Task 3</text>
<line class="lifeline" x1="80" y1="46" x2="80" y2="310"/>
<line class="lifeline" x1="250" y1="46" x2="250" y2="310"/>
<line class="lifeline" x1="420" y1="46" x2="420" y2="310"/>
<line class="lifeline" x1="590" y1="46" x2="590" y2="310"/>
<text class="t-mut" x="165" y="66">Start</text>
<path class="e" d="M80,78 H247" marker-end="url(#all-ah)"/>
<text class="t-mut" x="250" y="110">Start</text>
<path class="e" d="M80,122 H417" marker-end="url(#all-ah)"/>
<text class="t-mut" x="335" y="154">Start</text>
<path class="e" d="M80,166 H587" marker-end="url(#all-ah)"/>
<text class="t-mut t-ok" x="165" y="198">Complete ✓</text>
<path class="e e-dash" d="M250,210 H83" marker-end="url(#all-ah)"/>
<text class="t-mut t-ok" x="250" y="242">Complete ✓</text>
<path class="e e-dash" d="M420,254 H83" marker-end="url(#all-ah)"/>
<text class="t-mut t-ok" x="335" y="286">Complete ✓</text>
<path class="e e-dash" d="M590,298 H83" marker-end="url(#all-ah)"/>
<rect class="n-hot" x="16" y="318" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="336">All Complete → Proceed</text>
</svg>
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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 840 374" role="img" style="max-width: 860px">
<title>Any strategy: three redundant tasks start, the fast one reports success first, the join fires on that single result and both slow tasks are cancelled before they finish.</title>
<defs><marker id="any-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="10" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="80" y="28">Workflow</text>
<rect class="n-ghost" x="180" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="250" y="28">Task 1 (slow)</text>
<rect class="n" x="350" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="420" y="28">Task 2 (fast)</text>
<rect class="n-ghost" x="520" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="590" y="28">Task 3 (slow)</text>
<line class="lifeline" x1="80" y1="46" x2="80" y2="352"/>
<line class="n-ghost" x1="250" y1="46" x2="250" y2="352"/>
<line class="lifeline" x1="420" y1="46" x2="420" y2="352"/>
<line class="n-ghost" x1="590" y1="46" x2="590" y2="352"/>
<text class="t-mut" x="165" y="66">Start</text>
<path class="e" d="M80,78 H247" marker-end="url(#any-ah)"/>
<text class="t-mut" x="250" y="110">Start</text>
<path class="e" d="M80,122 H417" marker-end="url(#any-ah)"/>
<text class="t-mut" x="335" y="154">Start</text>
<path class="e" d="M80,166 H587" marker-end="url(#any-ah)"/>
<text class="t-mut t-ok" x="250" y="198">Complete ✓</text>
<path class="e e-dash" d="M420,210 H83" marker-end="url(#any-ah)"/>
<rect class="n-hot" x="16" y="230" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="248">First Complete → Proceed</text>
<text class="t-mut t-warn" x="165" y="284">Cancel ⏱</text>
<path class="e e-dim" d="M80,296 H247" marker-end="url(#any-ah)"/>
<text class="t-mut t-warn" x="335" y="328">Cancel ⏱</text>
<path class="e e-dim" d="M80,340 H587" marker-end="url(#any-ah)"/>
</svg>
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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 840 462" role="img" style="max-width: 860px">
<title>Quorum strategy: four tasks start, the join waits for the third success to land in whatever order they arrive, fires as soon as the quorum of 3 of 4 is met, and cancels the task still running.</title>
<defs><marker id="quorum-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="10" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="80" y="28">Workflow</text>
<rect class="n" x="180" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="250" y="28">Task 1</text>
<rect class="n" x="350" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="420" y="28">Task 2</text>
<rect class="n" x="520" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="590" y="28">Task 3</text>
<rect class="n-ghost" x="690" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="760" y="28">Task 4</text>
<line class="lifeline" x1="80" y1="46" x2="80" y2="440"/>
<line class="lifeline" x1="250" y1="46" x2="250" y2="440"/>
<line class="lifeline" x1="420" y1="46" x2="420" y2="440"/>
<line class="lifeline" x1="590" y1="46" x2="590" y2="440"/>
<line class="n-ghost" x1="760" y1="46" x2="760" y2="440"/>
<text class="t-mut" x="165" y="66">Start</text>
<path class="e" d="M80,78 H247" marker-end="url(#quorum-ah)"/>
<text class="t-mut" x="250" y="110">Start</text>
<path class="e" d="M80,122 H417" marker-end="url(#quorum-ah)"/>
<text class="t-mut" x="335" y="154">Start</text>
<path class="e" d="M80,166 H587" marker-end="url(#quorum-ah)"/>
<text class="t-mut" x="420" y="198">Start</text>
<path class="e" d="M80,210 H757" marker-end="url(#quorum-ah)"/>
<text class="t-mut t-ok" x="165" y="242">Complete ✓</text>
<path class="e e-dash" d="M250,254 H83" marker-end="url(#quorum-ah)"/>
<text class="t-mut t-ok" x="335" y="286">Complete ✓</text>
<path class="e e-dash" d="M590,298 H83" marker-end="url(#quorum-ah)"/>
<text class="t-mut t-ok" x="250" y="330">Complete ✓</text>
<path class="e e-dash" d="M420,342 H83" marker-end="url(#quorum-ah)"/>
<rect class="n-hot" x="16" y="362" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="380">Quorum (3/4) Met → Proceed</text>
<text class="t-mut t-warn" x="420" y="416">Cancel ⏱</text>
<path class="e e-dim" d="M80,428 H757" marker-end="url(#quorum-ah)"/>
</svg>
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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 840 462" role="img" style="max-width: 860px">
<title>Percentage strategy: four tasks start and the join fires the moment the configured fraction of them has succeeded — here 75%, three of four — cancelling the one still in flight.</title>
<defs><marker id="pct-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="10" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="80" y="28">Workflow</text>
<rect class="n" x="180" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="250" y="28">Task 1</text>
<rect class="n" x="350" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="420" y="28">Task 2</text>
<rect class="n-ghost" x="520" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="590" y="28">Task 3</text>
<rect class="n" x="690" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="760" y="28">Task 4</text>
<line class="lifeline" x1="80" y1="46" x2="80" y2="440"/>
<line class="lifeline" x1="250" y1="46" x2="250" y2="440"/>
<line class="lifeline" x1="420" y1="46" x2="420" y2="440"/>
<line class="n-ghost" x1="590" y1="46" x2="590" y2="440"/>
<line class="lifeline" x1="760" y1="46" x2="760" y2="440"/>
<text class="t-mut" x="165" y="66">Start</text>
<path class="e" d="M80,78 H247" marker-end="url(#pct-ah)"/>
<text class="t-mut" x="250" y="110">Start</text>
<path class="e" d="M80,122 H417" marker-end="url(#pct-ah)"/>
<text class="t-mut" x="335" y="154">Start</text>
<path class="e" d="M80,166 H587" marker-end="url(#pct-ah)"/>
<text class="t-mut" x="420" y="198">Start</text>
<path class="e" d="M80,210 H757" marker-end="url(#pct-ah)"/>
<text class="t-mut t-ok" x="165" y="242">Complete ✓</text>
<path class="e e-dash" d="M250,254 H83" marker-end="url(#pct-ah)"/>
<text class="t-mut t-ok" x="250" y="286">Complete ✓</text>
<path class="e e-dash" d="M420,298 H83" marker-end="url(#pct-ah)"/>
<text class="t-mut t-ok" x="420" y="330">Complete ✓</text>
<path class="e e-dash" d="M760,342 H83" marker-end="url(#pct-ah)"/>
<rect class="n-hot" x="16" y="362" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="380">75% (3/4) Met → Proceed</text>
<text class="t-mut t-warn" x="335" y="416">Cancel ⏱</text>
<path class="e e-dim" d="M80,428 H587" marker-end="url(#pct-ah)"/>
</svg>
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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 840 504" role="img" style="max-width: 860px">
<title>PartialResults strategy: four tasks start and the join counts completions rather than successes, so a failure still counts — three outcomes arrive, the join fires, the last task is cancelled and every outcome is tracked for the downstream task.</title>
<defs><marker id="partial-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="10" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="80" y="28">Workflow</text>
<rect class="n" x="180" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="250" y="28">Task 1</text>
<rect class="n" x="350" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="420" y="28">Task 2</text>
<rect class="n" x="520" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="590" y="28">Task 3</text>
<rect class="n-ghost" x="690" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="760" y="28">Task 4</text>
<line class="lifeline" x1="80" y1="46" x2="80" y2="440"/>
<line class="lifeline" x1="250" y1="46" x2="250" y2="440"/>
<line class="lifeline" x1="420" y1="46" x2="420" y2="440"/>
<line class="lifeline" x1="590" y1="46" x2="590" y2="440"/>
<line class="n-ghost" x1="760" y1="46" x2="760" y2="440"/>
<text class="t-mut" x="165" y="66">Start</text>
<path class="e" d="M80,78 H247" marker-end="url(#partial-ah)"/>
<text class="t-mut" x="250" y="110">Start</text>
<path class="e" d="M80,122 H417" marker-end="url(#partial-ah)"/>
<text class="t-mut" x="335" y="154">Start</text>
<path class="e" d="M80,166 H587" marker-end="url(#partial-ah)"/>
<text class="t-mut" x="420" y="198">Start</text>
<path class="e" d="M80,210 H757" marker-end="url(#partial-ah)"/>
<text class="t-mut t-ok" x="165" y="242">Complete ✓</text>
<path class="e e-dash" d="M250,254 H83" marker-end="url(#partial-ah)"/>
<text class="t-mut t-err" x="250" y="286">Failed ✗</text>
<path class="e e-dash" d="M420,298 H83" marker-end="url(#partial-ah)"/>
<text class="t-mut t-ok" x="335" y="330">Complete ✓</text>
<path class="e e-dash" d="M590,342 H83" marker-end="url(#partial-ah)"/>
<rect class="n-hot" x="16" y="362" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="380">3 completions → Proceed</text>
<text class="t-mut t-warn" x="420" y="416">Cancel ⏱</text>
<path class="e e-dim" d="M80,428 H757" marker-end="url(#partial-ah)"/>
<rect class="n-hot" x="16" y="448" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="466">Track: <tspan class="t-ok">2 success</tspan>, <tspan class="t-err">1 error</tspan>, <tspan class="t-warn">1 cancelled</tspan></text>
</svg>
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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 840 504" role="img" style="max-width: 860px">
<title>PartialTimeout strategy: four tasks start, two report success before the deadline, and the timeout itself fires the join — the two tasks still running are cancelled and the workflow proceeds with the results it has.</title>
<defs><marker id="ptimeout-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n" x="10" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="80" y="28">Workflow</text>
<rect class="n" x="180" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="250" y="28">Task 1</text>
<rect class="n-ghost" x="350" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="420" y="28">Task 2</text>
<rect class="n" x="520" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="590" y="28">Task 3</text>
<rect class="n-ghost" x="690" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="760" y="28">Task 4</text>
<line class="lifeline" x1="80" y1="46" x2="80" y2="440"/>
<line class="lifeline" x1="250" y1="46" x2="250" y2="440"/>
<line class="n-ghost" x1="420" y1="46" x2="420" y2="440"/>
<line class="lifeline" x1="590" y1="46" x2="590" y2="440"/>
<line class="n-ghost" x1="760" y1="46" x2="760" y2="440"/>
<text class="t-mut" x="165" y="66">Start</text>
<path class="e" d="M80,78 H247" marker-end="url(#ptimeout-ah)"/>
<text class="t-mut" x="250" y="110">Start</text>
<path class="e" d="M80,122 H417" marker-end="url(#ptimeout-ah)"/>
<text class="t-mut" x="335" y="154">Start</text>
<path class="e" d="M80,166 H587" marker-end="url(#ptimeout-ah)"/>
<text class="t-mut" x="420" y="198">Start</text>
<path class="e" d="M80,210 H757" marker-end="url(#ptimeout-ah)"/>
<text class="t-mut t-ok" x="165" y="242">Complete ✓</text>
<path class="e e-dash" d="M250,254 H83" marker-end="url(#ptimeout-ah)"/>
<text class="t-mut t-ok" x="335" y="286">Complete ✓</text>
<path class="e e-dash" d="M590,298 H83" marker-end="url(#ptimeout-ah)"/>
<rect class="n-hot" x="16" y="318" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="336">Timeout Reached</text>
<text class="t-mut t-warn" x="250" y="372">Cancel ⏱</text>
<path class="e e-dim" d="M80,384 H417" marker-end="url(#ptimeout-ah)"/>
<text class="t-mut t-warn" x="420" y="416">Cancel ⏱</text>
<path class="e e-dim" d="M80,428 H757" marker-end="url(#ptimeout-ah)"/>
<rect class="n-hot" x="16" y="448" width="808" height="34" rx="10"/>
<text class="t-strong" x="420" y="466">Proceed with 2 results</text>
</svg>
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
</div>
