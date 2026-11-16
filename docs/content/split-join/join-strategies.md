+++
title = "Join strategies"
description = "Cano join strategies: All, Any, Quorum, Percentage, PartialResults and PartialTimeout, each wired into a split, with a comparison table."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Join strategies</h1>
<p class="subtitle">Pick the completion condition for a split.</p>

<p>
A split advances when its <code>JoinStrategy</code> is satisfied. This page is a reference for the six
strategies, each with a timeline and the <code>JoinConfig</code> wiring. <a href="../">Split and join</a>
covers registering a split and the bulkhead.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#join-strategy-examples">Join strategy examples</a></li>
<li class="toc-sub"><a href="#strategy-all">All</a></li>
<li class="toc-sub"><a href="#strategy-any">Any</a></li>
<li class="toc-sub"><a href="#strategy-quorum">Quorum</a></li>
<li class="toc-sub"><a href="#strategy-percentage">Percentage</a></li>
<li class="toc-sub"><a href="#strategy-partial-results">PartialResults</a></li>
<li class="toc-sub"><a href="#strategy-partial-timeout">PartialTimeout</a></li>
<li><a href="#comparison-table">Comparison table</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="join-strategy-examples"><a href="#join-strategy-examples" class="anchor-link" aria-hidden="true">#</a>Join strategy examples</h2>
<p>
A strategy counts the tasks that succeeded. <code>Any</code> and <code>PartialResults</code> abort the
tasks still running as soon as they are satisfied, and an aborted task is dropped at its next
<code>.await</code>. <code>PartialTimeout</code> aborts them when the timeout expires. The other
strategies wait for every task. For all of them except <code>PartialTimeout</code>, an expired
<code>.with_timeout()</code> fails the join with a <code>CanoError::Workflow</code> error.
</p>
<p>
Every example below builds its workflow with this shared setup. A split branch's returned state is
ignored: the workflow always continues at the join state.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Fanout, Aggregate, Complete }

struct Prepare;

#[task(state = State)]
impl Prepare {
    async fn run(&self) -> Result<State, CanoError> {
        Ok(State::Fanout)
    }
}

struct Worker { id: u32 }

#[task(state = State)]
impl Worker {
    async fn run(&self) -> Result<State, CanoError> {
        println!("worker {} finished", self.id);
        Ok(State::Aggregate)
    }
}

struct Summarize;

#[task(state = State)]
impl Summarize {
    async fn run(&self) -> Result<State, CanoError> {
        Ok(State::Complete)
    }
}

// Splits `Fanout` into `workers` tasks and joins with `join_config`.
fn build(join_config: JoinConfig<State>, workers: u32) -> Workflow<State> {
    Workflow::new(Resources::new())
        .register(State::Start, Prepare)
        .register_split(State::Fanout, (1..=workers).map(|id| Worker { id }), join_config)
        .register(State::Aggregate, Summarize)
        .add_exit_state(State::Complete)
}
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example join_strategies</code> runs one split four times with
<code>Any</code>, <code>Quorum</code>, <code>Percentage</code> and <code>PartialResults</code>, using
staggered task delays so you can see when each one returns.</p>
</div>

<h3 id="strategy-all"><a href="#strategy-all" class="anchor-link" aria-hidden="true">#</a>All: wait for every task</h3>
<p>
Joins after every task has succeeded. If any task fails, the join fails with a
<code>CanoError::Workflow</code> error once all tasks have finished. Use it when every result is
required.
</p>

<div class="diagram-frame">
<p class="diagram-label">All: the join waits for the slowest task</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 238" role="img" aria-labelledby="joins-all-title joins-all-desc">
<title id="joins-all-title">All strategy timeline</title>
<desc id="joins-all-desc">Three tasks start together and finish at different times, and each one succeeds. The join fires when the slowest task finishes, and the workflow advances to the join state. If any task had failed, the join would fail after the last task finished.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<rect class="n-ok" x="130" y="54" width="170" height="28" rx="6"/>
<text class="t-mut t-ok" x="215" y="68">succeeded</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<rect class="n-ok" x="130" y="94" width="290" height="28" rx="6"/>
<text class="t-mut t-ok" x="275" y="108">succeeded</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<rect class="n-ok" x="130" y="134" width="390" height="28" rx="6"/>
<text class="t-mut t-ok" x="325" y="148">succeeded</text>
<line class="axis" x1="130" y1="176" x2="700" y2="176"/>
<text class="t-mut ta-e" x="700" y="190">time</text>
<line class="e e-hot" x1="520" y1="44" x2="520" y2="176"/>
<text class="t-mut t-hot" x="520" y="30">join</text>
<rect class="n-hot" x="130" y="194" width="570" height="34" rx="10"/>
<text class="t-strong" x="415" y="211">All 3 succeeded: advance to the join state</text>
</svg>
</div>
</div>

```rust
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_timeout(Duration::from_secs(10));
let workflow = build(join_config, 3);
```

<h3 id="strategy-any"><a href="#strategy-any" class="anchor-link" aria-hidden="true">#</a>Any: first success</h3>
<p>
Joins on the first success and aborts the rest. If every task fails, the join fails with a
<code>CanoError::Workflow</code> error. Use it for redundant calls where the fastest answer wins.
</p>

<div class="diagram-frame">
<p class="diagram-label">Any: the first success joins and aborts the rest</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 238" role="img" aria-labelledby="joins-any-title joins-any-desc">
<title id="joins-any-title">Any strategy timeline</title>
<desc id="joins-any-desc">Three redundant tasks start together. Task 2 succeeds first, so the join fires at that moment and the workflow advances. Tasks 1 and 3 are still running and are aborted.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<rect class="band" x="130" y="54" width="120" height="28" rx="6"/>
<text class="t-mut" x="190" y="68">running</text>
<rect class="n-ghost" x="250" y="54" width="170" height="28" rx="6"/>
<text class="t-mut t-warn" x="335" y="68">aborted</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<rect class="n-ok" x="130" y="94" width="120" height="28" rx="6"/>
<text class="t-mut t-ok" x="190" y="108">succeeded</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<rect class="band" x="130" y="134" width="120" height="28" rx="6"/>
<text class="t-mut" x="190" y="148">running</text>
<rect class="n-ghost" x="250" y="134" width="270" height="28" rx="6"/>
<text class="t-mut t-warn" x="385" y="148">aborted</text>
<line class="axis" x1="130" y1="176" x2="700" y2="176"/>
<text class="t-mut ta-e" x="700" y="190">time</text>
<line class="e e-hot" x1="250" y1="44" x2="250" y2="176"/>
<text class="t-mut t-hot" x="250" y="30">first success</text>
<rect class="n-hot" x="130" y="194" width="570" height="34" rx="10"/>
<text class="t-strong" x="415" y="211">First success: advance. The other tasks are aborted</text>
</svg>
</div>
</div>

```rust
// Three redundant providers: the first to succeed wins.
let join_config = JoinConfig::new(JoinStrategy::Any, State::Aggregate);
let workflow = build(join_config, 3);
```

<h3 id="strategy-quorum"><a href="#strategy-quorum" class="anchor-link" aria-hidden="true">#</a>Quorum(n): at least n successes</h3>
<p>
Waits for every task to finish, then joins if at least <code>n</code> succeeded. Otherwise the join
fails with a <code>CanoError::Workflow</code> error. Use it for majority writes where the stragglers
are still worth waiting for.
</p>

<div class="diagram-frame">
<p class="diagram-label">Quorum(3): the join waits for every task, then counts</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 278" role="img" aria-labelledby="joins-quorum-title joins-quorum-desc">
<title id="joins-quorum-title">Quorum strategy timeline</title>
<desc id="joins-quorum-desc">Four tasks start together. Three succeed and one fails, finishing at different times. The join fires when the last task finishes, counts three successes against a quorum of three, and the workflow advances.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<rect class="n-ok" x="130" y="54" width="120" height="28" rx="6"/>
<text class="t-mut t-ok" x="190" y="68">succeeded</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<rect class="n-ok" x="130" y="94" width="200" height="28" rx="6"/>
<text class="t-mut t-ok" x="230" y="108">succeeded</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<rect class="n-err" x="130" y="134" width="270" height="28" rx="6"/>
<text class="t-mut t-err" x="265" y="148">failed</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<rect class="n-ok" x="130" y="174" width="390" height="28" rx="6"/>
<text class="t-mut t-ok" x="325" y="188">succeeded</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<line class="e e-hot" x1="520" y1="44" x2="520" y2="216"/>
<text class="t-mut t-hot" x="520" y="30">join</text>
<rect class="n-hot" x="130" y="234" width="570" height="34" rx="10"/>
<text class="t-strong" x="415" y="251">3 of 4 succeeded, quorum of 3 met: advance</text>
</svg>
</div>
</div>

```rust
// Write to 5 replicas; 3 must confirm.
let join_config = JoinConfig::new(JoinStrategy::Quorum(3), State::Aggregate)
    .with_timeout(Duration::from_secs(5));
let workflow = build(join_config, 5);
```

<h3 id="strategy-percentage"><a href="#strategy-percentage" class="anchor-link" aria-hidden="true">#</a>Percentage(p): a fraction of successes</h3>
<p>
Waits for every task to finish, then joins if at least <code>ceil(total * p)</code> succeeded. The value
must be in <code>(0.0, 1.0]</code>. Because the requirement scales with the task count, it suits splits
whose size varies.
</p>

<div class="diagram-frame">
<p class="diagram-label">Percentage(0.75): the join waits for every task, then counts</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 278" role="img" aria-labelledby="joins-percentage-title joins-percentage-desc">
<title id="joins-percentage-title">Percentage strategy timeline</title>
<desc id="joins-percentage-desc">Four tasks start together. Task 2 fails and the other three succeed. The join fires when the last task finishes. Three of four is 75 percent, which meets the configured fraction, so the workflow advances.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<rect class="n-ok" x="130" y="54" width="200" height="28" rx="6"/>
<text class="t-mut t-ok" x="230" y="68">succeeded</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<rect class="n-err" x="130" y="94" width="120" height="28" rx="6"/>
<text class="t-mut t-err" x="190" y="108">failed</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<rect class="n-ok" x="130" y="134" width="270" height="28" rx="6"/>
<text class="t-mut t-ok" x="265" y="148">succeeded</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<rect class="n-ok" x="130" y="174" width="390" height="28" rx="6"/>
<text class="t-mut t-ok" x="325" y="188">succeeded</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<line class="e e-hot" x1="520" y1="44" x2="520" y2="216"/>
<text class="t-mut t-hot" x="520" y="30">join</text>
<rect class="n-hot" x="130" y="234" width="570" height="34" rx="10"/>
<text class="t-strong" x="415" y="251">3 of 4 succeeded, 75% met: advance</text>
</svg>
</div>
</div>

```rust
// 100 records; at least 75 must succeed.
let join_config = JoinConfig::new(JoinStrategy::Percentage(0.75), State::Aggregate)
    .with_timeout(Duration::from_secs(10));
let workflow = build(join_config, 100);
```

<h3 id="strategy-partial-results"><a href="#strategy-partial-results" class="anchor-link" aria-hidden="true">#</a>PartialResults(n): stop after n successes</h3>
<p>
Joins as soon as <code>n</code> tasks have succeeded and aborts the rest. Failures along the way are
tolerated and reported to observers. If the split ends with fewer than <code>n</code> successes, the join
fails with a <code>CanoError::Workflow</code> error that lists the task errors. Use it
when latency matters and some failures are acceptable. <code>cargo run --example workflow_partial_results</code>
walks through it.
</p>

<div class="diagram-frame">
<p class="diagram-label">PartialResults(2): the second success joins and aborts the rest</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 278" role="img" aria-labelledby="joins-partial-results-title joins-partial-results-desc">
<title id="joins-partial-results-title">PartialResults strategy timeline</title>
<desc id="joins-partial-results-desc">Four tasks start together. Task 1 succeeds, task 2 fails and the failure is tolerated, then task 3 succeeds. The second success fires the join. Task 4 is still running and is aborted. The outcome is two successes, one failure and one aborted task.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<rect class="n-ok" x="130" y="54" width="120" height="28" rx="6"/>
<text class="t-mut t-ok" x="190" y="68">succeeded</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<rect class="n-err" x="130" y="94" width="170" height="28" rx="6"/>
<text class="t-mut t-err" x="215" y="108">failed</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<rect class="n-ok" x="130" y="134" width="250" height="28" rx="6"/>
<text class="t-mut t-ok" x="255" y="148">succeeded</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<rect class="band" x="130" y="174" width="250" height="28" rx="6"/>
<text class="t-mut" x="255" y="188">running</text>
<rect class="n-ghost" x="380" y="174" width="140" height="28" rx="6"/>
<text class="t-mut t-warn" x="450" y="188">aborted</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<line class="e e-hot" x1="380" y1="44" x2="380" y2="216"/>
<text class="t-mut t-hot" x="380" y="30">second success</text>
<rect class="n-hot" x="130" y="234" width="570" height="34" rx="10"/>
<text class="t-strong" x="415" y="251">2 succeeded: advance. 1 failed, 1 aborted</text>
</svg>
</div>
</div>

```rust
// Proceed after any 2 of 4 service calls succeed.
let join_config = JoinConfig::new(JoinStrategy::PartialResults(2), State::Aggregate)
    .with_timeout(Duration::from_secs(5));
let workflow = build(join_config, 4);
```

<h3 id="strategy-partial-timeout"><a href="#strategy-partial-timeout" class="anchor-link" aria-hidden="true">#</a>PartialTimeout: whatever finished by the deadline</h3>
<p>
Joins when every task has finished or the timeout expires, whichever comes first, and accepts whatever
completed by then. Successes and failures both count, so a join with only failures still advances.
If no task finished, the join fails with a <code>CanoError::Workflow</code> error. Use it for hard
SLAs.
</p>

<div class="diagram-frame">
<p class="diagram-label">PartialTimeout: the deadline joins with whatever finished</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 278" role="img" aria-labelledby="joins-partial-timeout-title joins-partial-timeout-desc">
<title id="joins-partial-timeout-title">PartialTimeout strategy timeline</title>
<desc id="joins-partial-timeout-desc">Four tasks start together with a timeout set. Tasks 1 and 3 succeed before the deadline. At the deadline tasks 2 and 4 are still running, so they are aborted, and the workflow advances with the two results it has.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<rect class="n-ok" x="130" y="54" width="120" height="28" rx="6"/>
<text class="t-mut t-ok" x="190" y="68">succeeded</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<rect class="band" x="130" y="94" width="270" height="28" rx="6"/>
<text class="t-mut" x="265" y="108">running</text>
<rect class="n-ghost" x="400" y="94" width="160" height="28" rx="6"/>
<text class="t-mut t-warn" x="480" y="108">aborted</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<rect class="n-ok" x="130" y="134" width="200" height="28" rx="6"/>
<text class="t-mut t-ok" x="230" y="148">succeeded</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<rect class="band" x="130" y="174" width="270" height="28" rx="6"/>
<text class="t-mut" x="265" y="188">running</text>
<rect class="n-ghost" x="400" y="174" width="240" height="28" rx="6"/>
<text class="t-mut t-warn" x="520" y="188">aborted</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<line class="e e-hot" x1="400" y1="44" x2="400" y2="216"/>
<text class="t-mut t-hot" x="400" y="30">timeout</text>
<rect class="n-hot" x="130" y="234" width="570" height="34" rx="10"/>
<text class="t-strong" x="415" y="251">Timeout: advance with 2 results. The rest are aborted</text>
</svg>
</div>
</div>

<div class="callout callout-warning">
<span class="callout-label">PartialTimeout needs a timeout</span>
<p>
<code>validate()</code> rejects a <code>PartialTimeout</code> split without <code>.with_timeout()</code>
with <code>CanoError::Configuration</code>, before the workflow runs.
</p>
</div>

```rust
// Real-time recommendations with a 500 ms SLA.
let join_config = JoinConfig::new(JoinStrategy::PartialTimeout, State::Aggregate)
    .with_timeout(Duration::from_millis(500));
let workflow = build(join_config, 4);
```
<hr class="section-divider">

<h2 id="comparison-table"><a href="#comparison-table" class="anchor-link" aria-hidden="true">#</a>Comparison table</h2>
<table class="styled-table">
<thead>
<tr>
<th>Strategy</th>
<th>Joins when</th>
<th>Aborts the rest</th>
<th>Use for</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>All</code></td>
<td>All tasks succeeded</td>
<td>No</td>
<td>Complete data required</td>
</tr>
<tr>
<td><code>Any</code></td>
<td>First success</td>
<td>Yes</td>
<td>Redundant calls</td>
</tr>
<tr>
<td><code>Quorum(n)</code></td>
<td>All finished, at least n succeeded</td>
<td>No</td>
<td>Majority writes</td>
</tr>
<tr>
<td><code>Percentage(p)</code></td>
<td>All finished, at least a fraction p succeeded</td>
<td>No</td>
<td>Batches of varying size</td>
</tr>
<tr>
<td><code>PartialResults(n)</code></td>
<td>n tasks succeeded</td>
<td>Yes</td>
<td>Fan-out under a latency bound</td>
</tr>
<tr>
<td><code>PartialTimeout</code></td>
<td>All finished or timeout, accepting successes and failures</td>
<td>At the timeout</td>
<td>Hard SLAs</td>
</tr>
</tbody>
</table>
</div>
