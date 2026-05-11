+++
title = "Workflows"
description = "Build state-machine workflows in Cano: defining states, the builder pattern, validation, and error handling."
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
<li><a href="#split-join">Parallel Tasks: Split &amp; Join</a></li>
<li><a href="#ad-exchange">Worked Example: Ad Exchange</a></li>
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
<td>One <a href="../task/">Task</a> (or <a href="../nodes/">Node</a>, <a href="../poll-task/">PollTask</a>, <a href="../batch-task/">BatchTask</a> — all dispatch as <code>Task</code>).</td>
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
The join strategies, the <a href="../split-join/#bulkhead">bulkhead</a>, the parallel-processing
patterns (queue consumer, dynamic task generation, resource-limited fan-out, scheduled batches), and the
workflow-per-HTTP-request pattern are all covered in the <a href="../split-join/">Split &amp; Join</a> guide.
</p>
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
