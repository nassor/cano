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

struct Worker {
    id: u32,
}

#[task(state = State)]
impl Worker {
    async fn run(&self) -> Result<State, CanoError> {
        println!("worker {} finished", self.id);
        // Returned state is ignored by split dispatch; JoinConfig picks join_state.
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

// Splits `Fanout` into `workers` concurrent tasks and joins with `join_config`.
fn build(join_config: JoinConfig<State>, workers: u32) -> Workflow<State> {
    Workflow::bare()
        .register(State::Start, Prepare)
        // register_split accepts any IntoIterator of tasks.
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
<svg class="cd cd-anim" viewBox="0 0 720 248" role="img" aria-labelledby="joins-all-title joins-all-desc">
<title id="joins-all-title">All strategy timeline</title>
<desc id="joins-all-desc">Three tasks start together and finish at different times, and each one succeeds. The join fires when the slowest task finishes, and the workflow advances to the join state. If any task had failed, the join would fail after the last task finished.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<line class="axis" x1="130" y1="176" x2="700" y2="176"/>
<text class="t-mut ta-e" x="700" y="190">time</text>
<g class="cd-loop"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.9398;0.988;1" values="1;1;0;0"/>
<rect class="n-ok" x="130" y="54" width="170" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.253;1" values="0;0;170;170"/></rect>
<text class="t-mut t-ok" x="215" y="68">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.253;0.2831;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="94" width="290" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.3976;1" values="0;0;290;290"/></rect>
<text class="t-mut t-ok" x="275" y="108">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.3976;0.4277;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="134" width="390" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="0;0;390;390"/></rect>
<text class="t-mut t-ok" x="325" y="148">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5482;1" values="0;0;1;1"/></text>
<line class="e e-hot" x1="520" y1="44" x2="520" y2="176"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5361;1" values="0;0;1;1"/></line>
<text class="t-mut t-hot" x="520" y="30">join<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5361;1" values="0;0;1;1"/></text>
<line class="e e-dim e-dash" x1="130" y1="44" x2="130" y2="176" opacity="0"><animate attributeName="x1" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="130;130;520;520"/><animate attributeName="x2" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="130;130;520;520"/><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0422;0.0482;0.5181;0.5422;1" values="0;0;1;1;0;0"/></line>
<rect class="n-hot" x="130" y="204" width="570" height="34" rx="10"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.6145;0.6566;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="415" y="221">All 3 succeeded: advance to the join state<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.6145;0.6566;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>

```rust
// All 3 workers must succeed within 10 s before advancing to State::Aggregate.
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
<svg class="cd cd-anim" viewBox="0 0 720 248" role="img" aria-labelledby="joins-any-title joins-any-desc">
<title id="joins-any-title">Any strategy timeline</title>
<desc id="joins-any-desc">Three redundant tasks start together. Task 2 succeeds first, so the join fires at that moment and the workflow advances. Tasks 1 and 3 are still running and are aborted.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<line class="axis" x1="130" y1="176" x2="700" y2="176"/>
<text class="t-mut ta-e" x="700" y="190">time</text>
<g class="cd-loop"><animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.9107;0.9821;1" values="1;1;0;0"/>
<rect class="band" x="130" y="54" width="120" height="28" rx="6"><animate attributeName="width" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.0714;0.2857;1" values="0;0;120;120"/></rect>
<text class="t-mut" x="190" y="68">running<animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.1161;0.1607;1" values="0;0;1;1"/></text>
<rect class="n-ghost" x="250" y="54" width="170" height="28" rx="6"><animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.3482;0.3929;1" values="0;0;1;1"/></rect>
<text class="t-mut t-warn" x="335" y="68">aborted<animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.3482;0.3929;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="94" width="120" height="28" rx="6"><animate attributeName="width" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.0714;0.2857;1" values="0;0;120;120"/></rect>
<text class="t-mut t-ok" x="190" y="108">succeeded<animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.2857;0.3304;1" values="0;0;1;1"/></text>
<rect class="band" x="130" y="134" width="120" height="28" rx="6"><animate attributeName="width" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.0714;0.2857;1" values="0;0;120;120"/></rect>
<text class="t-mut" x="190" y="148">running<animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.1161;0.1607;1" values="0;0;1;1"/></text>
<rect class="n-ghost" x="250" y="134" width="270" height="28" rx="6"><animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.3482;0.3929;1" values="0;0;1;1"/></rect>
<text class="t-mut t-warn" x="385" y="148">aborted<animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.3482;0.3929;1" values="0;0;1;1"/></text>
<line class="e e-hot" x1="250" y1="44" x2="250" y2="176"><animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.2857;0.3125;1" values="0;0;1;1"/></line>
<text class="t-mut t-hot" x="250" y="30">first success<animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.2857;0.3125;1" values="0;0;1;1"/></text>
<line class="e e-dim e-dash" x1="130" y1="44" x2="130" y2="176" opacity="0"><animate attributeName="x1" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.0714;0.2857;1" values="130;130;250;250"/><animate attributeName="x2" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.0714;0.2857;1" values="130;130;250;250"/><animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.0625;0.0714;0.2857;0.3214;1" values="0;0;1;1;0;0"/></line>
<rect class="n-hot" x="130" y="204" width="570" height="34" rx="10"><animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.4286;0.4911;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="415" y="221">First success: advance. The other tasks are aborted<animate attributeName="opacity" dur="5.6s" repeatCount="indefinite" keyTimes="0;0.4286;0.4911;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>

```rust
// Three redundant providers: advance on the first success and abort the remaining two.
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
<svg class="cd cd-anim" viewBox="0 0 720 288" role="img" aria-labelledby="joins-quorum-title joins-quorum-desc">
<title id="joins-quorum-title">Quorum strategy timeline</title>
<desc id="joins-quorum-desc">Four tasks start together. Three succeed and one fails, finishing at different times. The join fires when the last task finishes, counts three successes against a quorum of three, and the workflow advances.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<g class="cd-loop"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.9398;0.988;1" values="1;1;0;0"/>
<rect class="n-ok" x="130" y="54" width="120" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.1928;1" values="0;0;120;120"/></rect>
<text class="t-mut t-ok" x="190" y="68">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.1928;0.2229;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="94" width="200" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.2892;1" values="0;0;200;200"/></rect>
<text class="t-mut t-ok" x="230" y="108">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.2892;0.3193;1" values="0;0;1;1"/></text>
<rect class="n-err" x="130" y="134" width="270" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.3735;1" values="0;0;270;270"/></rect>
<text class="t-mut t-err" x="265" y="148">failed<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.3735;0.4036;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="174" width="390" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="0;0;390;390"/></rect>
<text class="t-mut t-ok" x="325" y="188">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5482;1" values="0;0;1;1"/></text>
<line class="e e-hot" x1="520" y1="44" x2="520" y2="216"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5361;1" values="0;0;1;1"/></line>
<text class="t-mut t-hot" x="520" y="30">join<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5361;1" values="0;0;1;1"/></text>
<line class="e e-dim e-dash" x1="130" y1="44" x2="130" y2="216" opacity="0"><animate attributeName="x1" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="130;130;520;520"/><animate attributeName="x2" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="130;130;520;520"/><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0422;0.0482;0.5181;0.5422;1" values="0;0;1;1;0;0"/></line>
<rect class="n-hot" x="130" y="244" width="570" height="34" rx="10"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.6145;0.6566;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="415" y="261">3 of 4 succeeded, quorum of 3 met: advance<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.6145;0.6566;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>

```rust
// Wait for all 5 replicas to finish, then advance if at least 3 succeeded.
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
<svg class="cd cd-anim" viewBox="0 0 720 288" role="img" aria-labelledby="joins-percentage-title joins-percentage-desc">
<title id="joins-percentage-title">Percentage strategy timeline</title>
<desc id="joins-percentage-desc">Four tasks start together. Task 2 fails and the other three succeed. The join fires when the last task finishes. Three of four is 75 percent, which meets the configured fraction, so the workflow advances.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<g class="cd-loop"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.9398;0.988;1" values="1;1;0;0"/>
<rect class="n-ok" x="130" y="54" width="200" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.2892;1" values="0;0;200;200"/></rect>
<text class="t-mut t-ok" x="230" y="68">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.2892;0.3193;1" values="0;0;1;1"/></text>
<rect class="n-err" x="130" y="94" width="120" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.1928;1" values="0;0;120;120"/></rect>
<text class="t-mut t-err" x="190" y="108">failed<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.1928;0.2229;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="134" width="270" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.3735;1" values="0;0;270;270"/></rect>
<text class="t-mut t-ok" x="265" y="148">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.3735;0.4036;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="174" width="390" height="28" rx="6"><animate attributeName="width" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="0;0;390;390"/></rect>
<text class="t-mut t-ok" x="325" y="188">succeeded<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5482;1" values="0;0;1;1"/></text>
<line class="e e-hot" x1="520" y1="44" x2="520" y2="216"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5361;1" values="0;0;1;1"/></line>
<text class="t-mut t-hot" x="520" y="30">join<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.5181;0.5361;1" values="0;0;1;1"/></text>
<line class="e e-dim e-dash" x1="130" y1="44" x2="130" y2="216" opacity="0"><animate attributeName="x1" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="130;130;520;520"/><animate attributeName="x2" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0482;0.5181;1" values="130;130;520;520"/><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.0422;0.0482;0.5181;0.5422;1" values="0;0;1;1;0;0"/></line>
<rect class="n-hot" x="130" y="244" width="570" height="34" rx="10"><animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.6145;0.6566;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="415" y="261">3 of 4 succeeded, 75% met: advance<animate attributeName="opacity" dur="8.3s" repeatCount="indefinite" keyTimes="0;0.6145;0.6566;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>

```rust
// Wait for all 100 tasks to finish; advance if at least ceil(100 * 0.75) = 75 succeeded.
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
<svg class="cd cd-anim" viewBox="0 0 720 288" role="img" aria-labelledby="joins-partial-results-title joins-partial-results-desc">
<title id="joins-partial-results-title">PartialResults strategy timeline</title>
<desc id="joins-partial-results-desc">Four tasks start together. Task 1 succeeds, task 2 fails and the failure is tolerated, then task 3 succeeds. The second success fires the join. Task 4 is still running and is aborted. The outcome is two successes, one failure and one aborted task.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<g class="cd-loop"><animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.9275;0.9855;1" values="1;1;0;0"/>
<rect class="n-ok" x="130" y="54" width="120" height="28" rx="6"><animate attributeName="width" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.058;0.2319;1" values="0;0;120;120"/></rect>
<text class="t-mut t-ok" x="190" y="68">succeeded<animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.2319;0.2681;1" values="0;0;1;1"/></text>
<rect class="n-err" x="130" y="94" width="170" height="28" rx="6"><animate attributeName="width" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.058;0.3043;1" values="0;0;170;170"/></rect>
<text class="t-mut t-err" x="215" y="108">failed<animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.3043;0.3406;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="134" width="250" height="28" rx="6"><animate attributeName="width" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.058;0.4203;1" values="0;0;250;250"/></rect>
<text class="t-mut t-ok" x="255" y="148">succeeded<animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.4203;0.4565;1" values="0;0;1;1"/></text>
<rect class="band" x="130" y="174" width="250" height="28" rx="6"><animate attributeName="width" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.058;0.4203;1" values="0;0;250;250"/></rect>
<text class="t-mut" x="255" y="188">running<animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.0942;0.1304;1" values="0;0;1;1"/></text>
<rect class="n-ghost" x="380" y="174" width="140" height="28" rx="6"><animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.471;0.5072;1" values="0;0;1;1"/></rect>
<text class="t-mut t-warn" x="450" y="188">aborted<animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.471;0.5072;1" values="0;0;1;1"/></text>
<line class="e e-hot" x1="380" y1="44" x2="380" y2="216"><animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.4203;0.442;1" values="0;0;1;1"/></line>
<text class="t-mut t-hot" x="380" y="30">second success<animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.4203;0.442;1" values="0;0;1;1"/></text>
<line class="e e-dim e-dash" x1="130" y1="44" x2="130" y2="216" opacity="0"><animate attributeName="x1" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.058;0.4203;1" values="130;130;380;380"/><animate attributeName="x2" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.058;0.4203;1" values="130;130;380;380"/><animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.0507;0.058;0.4203;0.4493;1" values="0;0;1;1;0;0"/></line>
<rect class="n-hot" x="130" y="244" width="570" height="34" rx="10"><animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.5362;0.587;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="415" y="261">2 succeeded: advance. 1 failed, 1 aborted<animate attributeName="opacity" dur="6.9s" repeatCount="indefinite" keyTimes="0;0.5362;0.587;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>

```rust
// Advance as soon as 2 of 4 calls succeed and abort the remaining in-flight tasks.
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
<svg class="cd cd-anim" viewBox="0 0 720 288" role="img" aria-labelledby="joins-partial-timeout-title joins-partial-timeout-desc">
<title id="joins-partial-timeout-title">PartialTimeout strategy timeline</title>
<desc id="joins-partial-timeout-desc">Four tasks start together with a timeout set. Tasks 1 and 3 succeed before the deadline. At the deadline tasks 2 and 4 are still running, so they are aborted, and the workflow advances with the two results it has.</desc>
<text class="ta-e" x="110" y="68">Task 1</text>
<text class="ta-e" x="110" y="108">Task 2</text>
<text class="ta-e" x="110" y="148">Task 3</text>
<text class="ta-e" x="110" y="188">Task 4</text>
<line class="axis" x1="130" y1="216" x2="700" y2="216"/>
<text class="t-mut ta-e" x="700" y="230">time</text>
<g class="cd-loop"><animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.9296;0.9859;1" values="1;1;0;0"/>
<rect class="n-ok" x="130" y="54" width="120" height="28" rx="6"><animate attributeName="width" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0563;0.2254;1" values="0;0;120;120"/></rect>
<text class="t-mut t-ok" x="190" y="68">succeeded<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.2254;0.2606;1" values="0;0;1;1"/></text>
<rect class="band" x="130" y="94" width="270" height="28" rx="6"><animate attributeName="width" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0563;0.4366;1" values="0;0;270;270"/></rect>
<text class="t-mut" x="265" y="108">running<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0915;0.1268;1" values="0;0;1;1"/></text>
<rect class="n-ghost" x="400" y="94" width="160" height="28" rx="6"><animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.4859;0.5211;1" values="0;0;1;1"/></rect>
<text class="t-mut t-warn" x="480" y="108">aborted<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.4859;0.5211;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="130" y="134" width="200" height="28" rx="6"><animate attributeName="width" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0563;0.338;1" values="0;0;200;200"/></rect>
<text class="t-mut t-ok" x="230" y="148">succeeded<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.338;0.3732;1" values="0;0;1;1"/></text>
<rect class="band" x="130" y="174" width="270" height="28" rx="6"><animate attributeName="width" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0563;0.4366;1" values="0;0;270;270"/></rect>
<text class="t-mut" x="265" y="188">running<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0915;0.1268;1" values="0;0;1;1"/></text>
<rect class="n-ghost" x="400" y="174" width="240" height="28" rx="6"><animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.4859;0.5211;1" values="0;0;1;1"/></rect>
<text class="t-mut t-warn" x="520" y="188">aborted<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.4859;0.5211;1" values="0;0;1;1"/></text>
<line class="e e-hot" x1="400" y1="44" x2="400" y2="216"><animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.4366;0.4577;1" values="0;0;1;1"/></line>
<text class="t-mut t-hot" x="400" y="30">timeout<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.4366;0.4577;1" values="0;0;1;1"/></text>
<line class="e e-dim e-dash" x1="130" y1="44" x2="130" y2="216" opacity="0"><animate attributeName="x1" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0563;0.4366;1" values="130;130;400;400"/><animate attributeName="x2" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0563;0.4366;1" values="130;130;400;400"/><animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.0493;0.0563;0.4366;0.4648;1" values="0;0;1;1;0;0"/></line>
<rect class="n-hot" x="130" y="244" width="570" height="34" rx="10"><animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.5493;0.5986;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="415" y="261">Timeout: advance with 2 results. The rest are aborted<animate attributeName="opacity" dur="7.1s" repeatCount="indefinite" keyTimes="0;0.5493;0.5986;1" values="0;0;1;1"/></text>
</g>
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
// PartialTimeout requires with_timeout(); aborts unfinished tasks at 500 ms and advances with what finished.
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
