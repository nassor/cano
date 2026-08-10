+++
title = "Workflows"
description = "Build state-machine workflows in Cano: defining states, the builder pattern, validation, and error handling."
template = "section.html"
+++

<div class="content-wrapper">

<h1>Workflows</h1>
<p class="subtitle">Orchestrate complex processes with state machine semantics.</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#defining-states">Defining States</a></li>
<li><a href="#building-a-workflow">Building a Workflow</a></li>
<li><a href="#cancellation">Cancelling a Run</a></li>
<li><a href="#builder-pattern">Builder Pattern</a></li>
<li><a href="#validation-guide">Validation &amp; Errors</a></li>
<li><a href="#split-join">Parallel Tasks: Split &amp; Join</a></li>
<li><a href="#workflow-on-request">Workflow per HTTP Request</a></li>
<li><a href="#ad-exchange">Worked Example: Ad Exchange</a></li>
</ol>
</nav>

<p>
Workflows in Cano are state machines. You define a set of states (usually an enum) and register a <code>Task</code> for each state.
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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 660 368" role="img">
<title>Workflow state transitions: Start moves to Validate, which branches to Process when the input is valid or to Failed when it is not; Process finishes at Complete.</title>
<defs><marker id="wfstate-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M150,68 V106" marker-end="url(#wfstate-ah)"/>
<path class="e" d="M150,156 V194" marker-end="url(#wfstate-ah)"/>
<path class="e" d="M215,133 H497 Q505,133 505,141 V194" marker-end="url(#wfstate-ah)"/>
<path class="e" d="M150,244 V282" marker-end="url(#wfstate-ah)"/>
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
<text class="t-mut" x="330" y="348">Each task returns the next state; the workflow stops at an exit state.</text>
</svg>
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
    let result = workflow.orchestrate(OrderState::Start, CancellationToken::disabled()).await?;

    println!("Final State: {:?}", result);
    Ok(())
}

```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_simple</code> — a linear three-state workflow
like the one above.</p>
</div>
<hr class="section-divider">

<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>Cancelling a Run</h2>
<p>
<code>orchestrate</code> always takes a <code>CancellationToken</code> as its second argument. The
example above passes <code>CancellationToken::disabled()</code> — a token that never fires, opting
the run out of cancellation at zero cost. To stop a run <em>early</em> on a signal you control — a
shutdown handler, a user "stop" button, a parent task giving up — pass a live token from
<code>CancellationToken::new()</code> instead and keep its paired <code>CancellationHandle</code>.
Firing the handle aborts the in-flight task at its next <code>.await</code>, drains the
<a href="../saga/">saga compensation stack</a>, and returns <code>CanoError::Cancelled</code>.
</p>

```rust
use cano::prelude::*;

let (handle, token) = CancellationToken::new();

// Cancel from anywhere — the handle is Clone and cancel() is idempotent:
tokio::spawn(async move {
    shutdown_signal().await;
    handle.cancel();
});

let result = workflow.orchestrate(OrderState::Start, token).await;
assert!(matches!(result, Err(e) if e.category() == "cancelled"));
```

<p>
Cancellation is <em>cooperative</em> (a task is interrupted only at an <code>.await</code>) and
<em>saga-safe</em> (a <a href="../saga/">compensatable task</a> is never interrupted mid-run). The
<a href="../resilience/#cancellation">Resilience → Cooperative Cancellation</a> page covers the full
semantics, the <code>on_cancelled</code> observer hook, and precedence against
<code>with_total_timeout</code>.
</p>
<hr class="section-divider">

<h2 id="builder-pattern"><a href="#builder-pattern" class="anchor-link" aria-hidden="true">#</a>Builder Pattern and #[must_use]</h2>
<p>
Workflow uses a builder pattern where the <code>register*</code> methods and
<code>add_exit_state()</code> all consume <code>self</code> and return a new <code>Workflow</code>.
The <code>#[must_use]</code> attribute on <code>Workflow</code> and <code>JoinConfig</code> means the compiler
will warn you if you discard the return value. If you forget to capture it, the registration is silently lost.
</p>

<p>Each <code>register*</code> method maps a state to a kind of <code>StateEntry</code>:</p>
<table class="styled-table">
<thead>
<tr>
<th>Builder method</th>
<th><code>StateEntry</code> kind</th>
<th>What runs at that state</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>register(state, task)</code></td>
<td><code>Single</code></td>
<td>One <a href="../task/">Task</a> (or <a href="../poll-task/">PollTask</a>, <a href="../timer-task/">TimerTask</a>, <a href="../batch-task/">BatchTask</a> — all dispatch as <code>Task</code>).</td>
</tr>
<tr>
<td><code>register_split(state, tasks, join_config)</code></td>
<td><code>Split</code></td>
<td>Many tasks in parallel; a <code>JoinConfig</code> picks the <a href="../split-join/">join strategy</a> and the next state.</td>
</tr>
<tr>
<td><code>register_router(state, task)</code></td>
<td><code>Router</code></td>
<td>A <a href="../router-task/">RouterTask</a> — dispatched like <code>Single</code>, but writes <strong>no checkpoint row</strong> (pure routing, nothing to recover).</td>
</tr>
<tr>
<td><code>register_stepped(state, task)</code></td>
<td><code>Stepped</code></td>
<td>A <a href="../stepped-task/">SteppedTask</a> — the engine owns the step loop and, with a checkpoint store attached, persists the cursor after each step.</td>
</tr>
<tr>
<td><code>register_with_compensation(state, task)</code></td>
<td><code>CompensatableSingle</code></td>
<td>A <a href="../saga/">CompensatableTask</a> — single-task states only; pushes onto the compensation stack on success.</td>
</tr>
</tbody>
</table>

<p>
The same builder also carries the cross-cutting concerns: <code>with_checkpoint_store</code> /
<code>with_workflow_id</code> / <code>with_workflow_version</code> / <code>resume_from</code> for <a href="../recovery/">crash recovery</a>,
<code>register_with_compensation</code> for <a href="../saga/">sagas</a>, <code>with_observer</code> for
<a href="../observers/">observers</a>, and per-task <code>TaskConfig</code> for retries, timeouts, and
<a href="../resilience/">circuit breakers</a>.
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

<h2 id="validation-guide"><a href="#validation-guide" class="anchor-link" aria-hidden="true">#</a>Validation &amp; Error Handling</h2>
<p>Checking a workflow is wired correctly, and the errors <code>orchestrate()</code> can return at runtime, are covered on a dedicated page:</p>
<ul>
<li><a href="validation-and-errors/">Validating &amp; Handling Errors in Workflows</a></li>
</ul>
<hr class="section-divider">

<h2 id="split-join"><a href="#split-join" class="anchor-link" aria-hidden="true">#</a>Parallel Tasks: Split &amp; Join</h2>
<p>
A state normally runs one task. When you need parallelism — scatter-gather queries, redundant API
calls, batch processing, hedged requests — register that state with <code>register_split()</code>
instead of <code>register()</code>: a list of tasks that run concurrently, plus a <code>JoinConfig</code>
that picks a <code>JoinStrategy</code> (<code>All</code>, <code>Any</code>, <code>Quorum(n)</code>,
<code>Percentage(p)</code>, <code>PartialResults(n)</code>, <code>PartialTimeout</code>) and the next
state to transition to once the strategy is satisfied. An optional <code>.with_bulkhead(n)</code> caps
how many of those tasks run at once.
</p>

<div class="code-block">
<span class="code-block-label">A split state</span>

```rust
let join_config = JoinConfig::new(JoinStrategy::All, State::Aggregate)
    .with_timeout(Duration::from_secs(5));

Workflow::new(Resources::new().insert("store", store))
    .register_split(State::Fanout, vec![Worker { id: 1 }, Worker { id: 2 }], join_config)
    .register(State::Aggregate, Aggregator)
    .add_exit_state(State::Complete)
```
</div>

<p>
The join strategies, the <a href="../split-join/#bulkhead">bulkhead</a>, and the parallel-processing
patterns (queue consumer, dynamic task generation, resource-limited fan-out, scheduled batches) are all
covered in the <a href="../split-join/">Split &amp; Join</a> guide.
</p>
<hr class="section-divider">

<h2 id="workflow-on-request"><a href="#workflow-on-request" class="anchor-link" aria-hidden="true">#</a>Workflow per HTTP Request</h2>
<p>
A very common pattern is triggering a workflow for every incoming HTTP request, running the FSM to
completion, and returning the results in the response. Cano workflows are cheap to construct — tasks are
small <code>Clone</code> structs wrapped in <code>Arc</code> internally — so building one per request is
fast, and <a href="../split-join/">split/join</a> inside the workflow gives you per-request parallelism for free.
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
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 730 428" role="img">
<title>One HTTP request: the handler writes the request data into a fresh store, orchestrates the workflow to its exit state, reads the results back out of the store, and returns the response.</title>
<defs><marker id="wfreq-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="70" y1="46" x2="70" y2="364"/>
<line class="lifeline" x1="258" y1="46" x2="258" y2="364"/>
<line class="lifeline" x1="448" y1="46" x2="448" y2="364"/>
<line class="lifeline" x1="638" y1="46" x2="638" y2="364"/>
<rect class="n" x="15" y="8" width="110" height="38" rx="8"/>
<text class="t-strong" x="70" y="28">Client</text>
<rect class="n" x="188" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="258" y="28">HTTP Handler</text>
<rect class="n-cop" x="378" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="448" y="28">MemoryStore</text>
<rect class="n" x="568" y="8" width="140" height="38" rx="8"/>
<text class="t-strong" x="638" y="28">Workflow FSM</text>
<path class="e" d="M76,90 H252" marker-end="url(#wfreq-ah)"/>
<text class="t-code" x="164" y="78">POST /process {text}</text>
<path class="e" d="M264,134 H442" marker-end="url(#wfreq-ah)"/>
<text class="t-code" x="353" y="122">put("input_text", text)</text>
<path class="e" d="M264,178 H632" marker-end="url(#wfreq-ah)"/>
<text class="t-code" x="545" y="166">orchestrate(Parse)</text>
<path class="e" d="M632,222 H454" marker-end="url(#wfreq-ah)"/>
<text class="t-mut" x="545" y="210">get / put during tasks</text>
<path class="e e-dash" d="M632,266 H264" marker-end="url(#wfreq-ah)"/>
<text class="t-code t-ok" x="510" y="254">Ok(Done)</text>
<path class="e" d="M264,310 H442" marker-end="url(#wfreq-ah)"/>
<text class="t-code" x="353" y="298">get("word_count"), get("uppercased")</text>
<path class="e e-dash" d="M252,354 H76" marker-end="url(#wfreq-ah)"/>
<text class="t-code t-ok" x="164" y="342">200 JSON response</text>
<rect class="n-hot" x="14" y="374" width="702" height="34" rx="10"/>
<text class="t-strong" x="365" y="392">One fresh store and one workflow per request — no leaks between requests.</text>
</svg>
</div>
</div>

<p>The shape of the handler:</p>
<ol>
<li>Create a <strong>fresh <code>MemoryStore</code></strong> for the request and write the request data into it with <code>store.put()</code>.</li>
<li>Build the workflow with a <strong>factory function</strong> that takes the store, then call <code>workflow.orchestrate(initial_state)</code> to drive the FSM to an exit state.</li>
<li>Read the results back out of the store and return them in the HTTP response. The exit state returned by <code>orchestrate()</code> tells you which terminal branch ran — use it for success/error response logic.</li>
</ol>

```rust
// One factory, called per request with a fresh store.
fn build_workflow(store: MemoryStore) -> Workflow<TextPipelineState> {
    Workflow::new(Resources::new().insert("store", store))
        .register(TextPipelineState::Parse, ParseTask)
        .register(TextPipelineState::Transform, TransformTask)
        .add_exit_state(TextPipelineState::Done)
        .with_total_timeout(Duration::from_secs(5))
}

// Inside an HTTP handler:
let store = MemoryStore::new();              // fresh store — full isolation
store.put("input_text", text)?;
let workflow = build_workflow(store.clone());
let final_state = workflow.orchestrate(TextPipelineState::Parse, CancellationToken::disabled()).await?; // which terminal branch ran
let word_count: usize = store.get("word_count")?;
```

<div class="callout callout-tip">
<span class="callout-label">Tip</span>
<p>
Use <code>.with_total_timeout()</code> on the workflow to keep a hung request from blocking indefinitely. For
read-heavy workloads with shared reference data, pre-populate one store, share it via <code>Arc</code>,
and use per-request keys to avoid collisions. The full Axum version is in
<code>cargo run --example workflow_on_request</code>.
</p>
</div>
<hr class="section-divider">

<h2 id="ad-exchange"><a href="#ad-exchange" class="anchor-link" aria-hidden="true">#</a>Worked Example: Real-Time Ad Exchange</h2>
<p>
The repository ships a production-shaped example that chains several split/join points with mixed
strategies: context gathering with <code>All</code> (100ms budget), bid requests across five DSPs with
<code>PartialTimeout</code> (200ms SLA), bid scoring and result tracking with <code>All</code>, and a
graceful error fan-out when any required split times out. It demonstrates multiple split points in one
FSM, per-split timeouts, partial results, and complex state management across phases. Run it with
<code>cargo run --example workflow_ad_exchange</code>, and see the <a href="../split-join/">Split &amp; Join</a>
guide for the strategy reference it builds on.
</p>

</div>
