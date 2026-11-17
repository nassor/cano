+++
title = "Workflows"
description = "Build state-machine workflows in Cano: defining states, the builder pattern, validation, and error handling."
template = "section.html"
+++

<div class="content-wrapper">

<h1>Workflows</h1>
<p class="subtitle">Register a task for each state and run the machine to an exit state.</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#defining-states">Defining states</a></li>
<li><a href="#building-a-workflow">Building a workflow</a></li>
<li><a href="#cancellation">Cancelling a run</a></li>
<li><a href="#builder-pattern">Builder methods and #[must_use]</a></li>
<li><a href="#validation-guide">Validation and errors</a></li>
<li><a href="#split-join">Parallel tasks: split and join</a></li>
<li><a href="#workflow-on-request">Workflow per HTTP request</a></li>
<li><a href="#ad-exchange">Worked example: ad exchange</a></li>
</ol>
</nav>

<p>
A workflow is a state machine. You define the states, usually as an enum, and register a task for each one.
Each task returns the next state, and the engine keeps dispatching until it reaches an exit state.
</p>
<hr class="section-divider">

<h2 id="defining-states"><a href="#defining-states" class="anchor-link" aria-hidden="true">#</a>Defining states</h2>
<p>
A state type implements <code>Debug</code>, <code>Clone</code>, <code>PartialEq</code>, <code>Eq</code> and <code>Hash</code>.
</p>

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

<h2 id="building-a-workflow"><a href="#building-a-workflow" class="anchor-link" aria-hidden="true">#</a>Building a workflow</h2>
<p>
<code>Workflow::new(resources)</code> takes a <a href="../resources/"><code>Resources</code></a> map that tasks
look up by type while they run. Put shared state there, such as a <code>Blackboard</code>, configuration or clients.
<code>Workflow::bare()</code> is <code>Workflow::new(Resources::new())</code>, for tasks that need nothing.
A task that needs no resources can leave the <code>res</code> parameter out of its signature
(<code>async fn run(&amp;self)</code>); the task macros add it.
</p>
<div class="diagram-frame">
<p class="diagram-label">Workflow state transitions</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 660 368" role="img" aria-labelledby="wf-states-title wf-states-desc">
<title id="wf-states-title">Workflow state transitions</title>
<desc id="wf-states-desc">Start moves to Validate. Validate returns Process when the input is valid, and Process moves to Complete. Validate returns Failed when the input is invalid. Complete and Failed are exit states, where the run stops.</desc>
<defs><marker id="wf-states-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M150,68 V106" marker-end="url(#wf-states-ah)"/>
<path class="e" d="M150,156 V194" marker-end="url(#wf-states-ah)"/>
<path class="e" d="M215,133 H497 Q505,133 505,141 V194" marker-end="url(#wf-states-ah)"/>
<path class="e" d="M150,244 V282" marker-end="url(#wf-states-ah)"/>
<text class="t-mut t-ok ta-s" x="162" y="175">Valid</text>
<text class="t-mut t-err" x="356" y="121">Invalid</text>
<rect class="n" x="85" y="22" width="130" height="46" rx="10"/>
<text x="150" y="46">Start</text>
<rect class="n-hot" x="85" y="110" width="130" height="46" rx="10"/>
<text class="t-strong" x="150" y="134">Validate</text>
<rect class="n" x="85" y="198" width="130" height="46" rx="10"/>
<text x="150" y="222">Process</text>
<rect class="n-ok" x="85" y="286" width="130" height="46" rx="10"/>
<text x="150" y="310">Complete</text>
<rect class="n-err" x="440" y="198" width="130" height="46" rx="10"/>
<text x="505" y="222">Failed</text>
<text class="t-mut" x="505" y="262">exit state</text>
<text class="t-mut ta-s" x="228" y="309">exit state</text>
<text class="t-mut" x="330" y="348">Each task returns the next state. The run stops at an exit state.</text>
</svg>
</div>
</div>

<h3 id="linear-example"><a href="#linear-example" class="anchor-link" aria-hidden="true">#</a>A linear workflow</h3>

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

struct StartTask;
struct ValidateTask;
struct ProcessTask;

#[task(state = OrderState)]
impl StartTask {
    async fn run(&self) -> Result<OrderState, CanoError> {
        Ok(OrderState::Validate)
    }
}

#[task(state = OrderState)]
impl ValidateTask {
    async fn run(&self) -> Result<OrderState, CanoError> {
        Ok(OrderState::Process)
    }
}

#[task(state = OrderState)]
impl ProcessTask {
    async fn run(&self) -> Result<OrderState, CanoError> {
        Ok(OrderState::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // Each builder method consumes the workflow and returns it, so chain the calls.
    let workflow = Workflow::new(Resources::new().insert(Blackboard::new()))
        // 1. Register a task for each state
        .register(OrderState::Start, StartTask)
        .register(OrderState::Validate, ValidateTask)
        .register(OrderState::Process, ProcessTask)
        // 2. Declare the exit states, where the run stops
        .add_exit_states([OrderState::Complete, OrderState::Failed]);

    // 3. Run from the initial state
    let result = workflow.orchestrate(OrderState::Start).await?;

    println!("Final state: {:?}", result);
    Ok(())
}
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_simple</code>, a linear three-state workflow
like the one above.</p>
</div>
<hr class="section-divider">

<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>Cancelling a run</h2>
<p>
<code>orchestrate</code> runs to an exit state and cannot be stopped from outside; only
<code>with_total_timeout</code> bounds it. To stop a run on a signal you control, create a
<code>CancellationToken</code> (a re-export of <code>tokio_util::sync::CancellationToken</code>) and pass a
reference to <code>orchestrate_with_token</code>. Calling <code>cancel()</code> on the token or any clone of it
drops the in-flight task at its next <code>.await</code>, drains the
<a href="../saga/">saga compensation stack</a>, and returns <code>CanoError::Cancelled</code>.
<code>resume_from_with_token</code> does the same for a resumed run.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

let token = CancellationToken::new();

// Clones share the token, and cancel() is idempotent.
let canceller = token.clone();
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(5)).await;
    canceller.cancel();
});

let result = workflow.orchestrate_with_token(OrderState::Start, &token).await;
assert!(matches!(result, Err(e) if e.is_cancelled()));
```

<p>
Cancellation is cooperative: a task is interrupted only at an <code>.await</code>. A
<a href="../saga/">compensatable task</a> is never interrupted mid-run, and the compensation drain always
runs to completion. The Resilience page covers
<a href="../resilience/#cancellation">cooperative cancellation</a> in full: the <code>on_cancelled</code>
observer hook and the precedence against <code>with_total_timeout</code>.
</p>
<hr class="section-divider">

<h2 id="builder-pattern"><a href="#builder-pattern" class="anchor-link" aria-hidden="true">#</a>Builder methods and #[must_use]</h2>
<p>
<code>register</code>, <code>register_split</code> and <code>add_exit_state</code> each consume
<code>self</code> and return the workflow. <code>Workflow</code> and <code>JoinConfig</code> are
<code>#[must_use]</code>, so the compiler warns when you drop the return value; the registration it carried is lost.
</p>

<p>Two methods put work on a state:</p>
<dl class="key-list">
<dt><code>register(state, task)</code></dt>
<dd>Runs one task of any model: <a href="../task/">Task</a>, <a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>, <a href="../timer-task/">TimerTask</a>, <a href="../batch-task/">BatchTask</a>, <a href="../stepped-task/">SteppedTask</a>, <a href="../stream-task/">StreamTask</a>, <a href="../suspend-task/">SuspendTask</a>, or a <a href="../saga/">CompensatableTask</a>. The model's macro tells the engine how to run it.</dd>

<dt><code>register_split(state, tasks, join_config)</code></dt>
<dd>Runs many tasks in parallel. A <code>JoinConfig</code> picks the <a href="../split-join/">join strategy</a> and the next state. Any task model except <code>SuspendTask</code> can be a branch; stepped and stream branches run their loops in memory, without cursor persistence.</dd>
</dl>

<p>The engine treats each model registered with <code>register</code> differently:</p>
<dl class="key-list">
<dt><code>Task</code>, <code>PollTask</code>, <code>TimerTask</code>, <code>BatchTask</code></dt>
<dd>Dispatched as a single task and checkpointed like any state.</dd>

<dt><code>RouterTask</code></dt>
<dd>Dispatched the same way, but writes no checkpoint row: it only routes, so there is nothing to recover.</dd>

<dt><code>SteppedTask</code></dt>
<dd>The engine owns the step loop and retries each step per the task's config. With a checkpoint store and a workflow id attached, it persists the cursor after every step.</dd>

<dt><code>StreamTask</code></dt>
<dd>The engine drives the windows and stops when the run's token is cancelled. With a checkpoint store and a workflow id attached, it persists a cursor row per flushed window.</dd>

<dt><code>SuspendTask</code></dt>
<dd>Runs <code>suspend</code>, writes a <code>Suspended</code> row and returns the parked state from <code>orchestrate</code>. <code>resume_with</code> later delivers a typed input to <code>resume</code>. It requires a checkpoint store and a workflow id.</dd>

<dt><code>CompensatableTask</code> <small><code>#[saga::task]</code></small></dt>
<dd>Pushes onto the compensation stack on success, and runs only in single-task states. Two compensatable states that share a task <code>name()</code> make <code>validate()</code> fail with a configuration error.</dd>
</dl>

<p>
The same builder carries the cross-cutting settings: <code>with_checkpoint_store</code>,
<code>with_workflow_id</code>, <code>with_workflow_version</code> and <code>resume_from</code> for
<a href="../recovery/">crash recovery</a>; <code>with_observer</code> for <a href="../observers/">observers</a>;
and a per-task <code>TaskConfig</code> for opt-in retries, timeouts and <a href="../resilience/">circuit breakers</a>.
A task runs once unless its config asks for retries.
</p>

<div class="callout callout-warning">
<div class="callout-label">Do not discard the return value</div>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Complete }

#[derive(Clone)]
struct MyTask;

#[task(state = State)]
impl MyTask {
    async fn run(&self) -> Result<State, CanoError> {
        Ok(State::Complete)
    }
}

fn examples() {
    let my_task = MyTask;

    // Wrong: register returns a new workflow, and it is discarded.
    let workflow = Workflow::bare();
    workflow.register(State::Start, my_task.clone());

    // Correct: capture the returned workflow.
    let workflow = Workflow::bare();
    let _workflow = workflow.register(State::Start, my_task.clone());

    // Best: chain the calls in one expression.
    let _workflow = Workflow::bare()
        .register(State::Start, my_task)
        .add_exit_state(State::Complete);
}
```
</div>
<hr class="section-divider">

<h2 id="validation-guide"><a href="#validation-guide" class="anchor-link" aria-hidden="true">#</a>Validation and errors</h2>
<p>
<a href="validation-and-errors/">Validating and handling errors in workflows</a> covers checking that a workflow
is wired correctly and the errors <code>orchestrate()</code> can return at runtime.
</p>
<hr class="section-divider">

<h2 id="split-join"><a href="#split-join" class="anchor-link" aria-hidden="true">#</a>Parallel tasks: split and join</h2>
<p>
A state normally runs one task. For scatter-gather queries, redundant API calls, batch processing or hedged
requests, register the state with <code>register_split()</code> instead: a list of tasks that run concurrently,
plus a <code>JoinConfig</code>. The <code>JoinConfig</code> picks a <code>JoinStrategy</code> (<code>All</code>,
<code>Any</code>, <code>Quorum(n)</code>, <code>Percentage(p)</code>, <code>PartialResults(n)</code> or
<code>PartialTimeout</code>) and the state to move to once the strategy is satisfied.
<code>.with_bulkhead(n)</code> caps how many of the tasks run at once.
</p>

<div class="code-block">
<span class="code-block-label">A split state</span>

```rust
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_timeout(Duration::from_secs(5));

Workflow::new(Resources::new().insert(board))
    .register_split(State::Fanout, vec![Worker { id: 1 }, Worker { id: 2 }], join_config)
    .register(State::Aggregate, Aggregator)
    .add_exit_state(State::Complete)
```
</div>

<p>
The <a href="../split-join/">Split and join</a> guide covers the join strategies, the
<a href="../split-join/#bulkhead">bulkhead</a>, and the parallel-processing patterns: queue consumer, dynamic
task generation, resource-limited fan-out and scheduled batches.
</p>
<hr class="section-divider">

<h2 id="workflow-on-request"><a href="#workflow-on-request" class="anchor-link" aria-hidden="true">#</a>Workflow per HTTP request</h2>
<p>
A common pattern is to run a workflow for every incoming HTTP request and return its results in the response.
Workflows are cheap to build, so one per request is fine, and a <a href="../split-join/">split</a> inside it gives
each request its own parallelism.
</p>

<div class="callout callout-warning">
<span class="callout-label">Blackboard isolation</span>
<p>
A <code>Blackboard</code> is <code>Arc</code>-wrapped, so a clone shares the same data. Create a fresh blackboard
for every request, or request data leaks between concurrent requests.
</p>
</div>

<div class="diagram-frame">
<p class="diagram-label">One request through a workflow</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 730 384" role="img" aria-labelledby="wf-request-title wf-request-desc">
<title id="wf-request-title">Workflow per HTTP request</title>
<desc id="wf-request-desc">A sequence between four parties. The client posts to the HTTP handler. The handler builds a workflow around a fresh blackboard and calls orchestrate with the initial state. While it runs, the workflow reads and writes the blackboard. When the workflow returns its exit state, the handler reads the results from the blackboard and sends a 200 JSON response to the client.</desc>
<defs><marker id="wf-request-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="70" y1="46" x2="70" y2="322"/>
<line class="lifeline" x1="258" y1="46" x2="258" y2="322"/>
<line class="lifeline" x1="448" y1="46" x2="448" y2="322"/>
<line class="lifeline" x1="638" y1="46" x2="638" y2="322"/>
<rect class="n" x="15" y="8" width="110" height="38" rx="8"/>
<text class="t-strong" x="70" y="28">Client</text>
<rect class="n" x="188" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="258" y="28">HTTP handler</text>
<rect class="n-cop" x="378" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="448" y="28">Blackboard</text>
<rect class="n" x="568" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="638" y="28">Workflow</text>
<path class="e" d="M76,90 H252" marker-end="url(#wf-request-ah)"/>
<text class="t-code" x="164" y="78">POST /process</text>
<path class="e" d="M264,134 H632" marker-end="url(#wf-request-ah)"/>
<text class="t-code" x="548" y="122">orchestrate(Parse)</text>
<path class="e" d="M632,178 H454" marker-end="url(#wf-request-ah)"/>
<text class="t-mut" x="543" y="166">get / put during tasks</text>
<path class="e e-dash" d="M632,222 H264" marker-end="url(#wf-request-ah)"/>
<text class="t-code t-ok" x="548" y="210">Ok(Done)</text>
<path class="e" d="M264,266 H442" marker-end="url(#wf-request-ah)"/>
<text class="t-code" x="353" y="254">get("word_count")</text>
<path class="e e-dash" d="M252,310 H76" marker-end="url(#wf-request-ah)"/>
<text class="t-code t-ok" x="164" y="298">200 JSON response</text>
<rect class="n-hot" x="14" y="334" width="702" height="34" rx="10"/>
<text class="t-strong" x="365" y="352">One fresh blackboard and one workflow per request, so nothing leaks between requests.</text>
</svg>
</div>
</div>

<p>The handler does three things:</p>
<ol>
<li>Creates a fresh <code>Blackboard</code>, puts the request data into the workflow's <code>Resources</code>, and builds the workflow with a factory function.</li>
<li>Calls <code>workflow.orchestrate(initial_state)</code> to drive the machine to an exit state. The returned exit state says which terminal branch ran, so use it to choose the response.</li>
<li>Reads the results back out of the blackboard and returns them.</li>
</ol>

```rust
// One factory, called per request with that request's resources.
fn build_workflow(resources: Resources) -> Workflow<TextPipelineState> {
    Workflow::new(resources)
        .register(TextPipelineState::Parse, ParseTask)
        .register(TextPipelineState::Transform, TransformTask)
        .add_exit_state(TextPipelineState::Done)
        .with_total_timeout(Duration::from_secs(5))
}

// Inside an HTTP handler:
let board = Blackboard::new(); // fresh blackboard, full isolation
let resources = Resources::new()
    .insert(board.clone())
    .insert(RequestParams { text });
let workflow = build_workflow(resources);
let final_state = workflow.orchestrate(TextPipelineState::Parse).await?;
let word_count: usize = board.get("word_count")?;
```

<div class="callout callout-tip">
<span class="callout-label">Tip</span>
<p>
<code>.with_total_timeout()</code> keeps a hung request from blocking forever. For read-heavy workloads with
shared reference data, fill one blackboard, share it, and use per-request keys to avoid collisions. The full Axum
version is <code>cargo run --example workflow_on_request</code>.
</p>
</div>
<hr class="section-divider">

<h2 id="ad-exchange"><a href="#ad-exchange" class="anchor-link" aria-hidden="true">#</a>Worked example: ad exchange</h2>
<p>
<code>cargo run --example workflow_ad_exchange</code> chains several split and join points with mixed strategies.
Context gathering uses <code>All</code> with a 100 ms budget, bid requests go to five DSPs under
<code>PartialTimeout</code> with a 200 ms SLA, and bid scoring and result tracking use <code>All</code>.
A required split that times out fans out to an error path. The <a href="../split-join/">Split and join</a>
guide has the strategy reference it builds on.
</p>

</div>
