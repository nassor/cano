+++
title = "Validating & Handling Errors in Workflows"
description = "Validate a Cano workflow with validate() and validate_initial_state(), and handle the CanoError variants that orchestrate() can return at runtime."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Validating &amp; Handling Errors</h1>
<p class="subtitle">Check a workflow is wired correctly, and handle what <code>orchestrate()</code> returns.</p>

<p>
See <a href="../">Workflows</a> for defining states and building a workflow. This page covers
validation and runtime error handling.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#validation">Workflow Validation</a></li>
<li class="toc-sub"><a href="#validate-method">validate()</a></li>
<li class="toc-sub"><a href="#validate-initial-state">validate_initial_state()</a></li>
<li><a href="#error-handling">Error Handling</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="validation"><a href="#validation" class="anchor-link" aria-hidden="true">#</a>Workflow Validation</h2>
<p>
Before orchestrating a workflow, you can validate its configuration to catch common mistakes early.
Cano provides two validation methods that check for different categories of problems.
</p>

<div class="diagram-frame">
<p class="diagram-label">The gate every <code>orchestrate()</code> call passes through</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 300" role="img">
<title>orchestrate() runs validate() once per workflow and validate_initial_state() on every call, before any resource setup; either check failing returns CanoError::Configuration before a task runs.</title>
<defs><marker id="valgate-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n-hot" x="25" y="40" width="200" height="58" rx="10"/>
<text class="t-code" x="125" y="65">validate()</text>
<text class="t-mut" x="125" y="85">cached — runs once</text>
<rect class="n" x="285" y="40" width="215" height="58" rx="10"/>
<text class="t-code" x="392" y="65">validate_initial_state</text>
<text class="t-mut" x="392" y="85">every call · O(1)</text>
<rect class="n-ok" x="560" y="40" width="195" height="58" rx="10"/>
<text class="t-code" x="657" y="65">setup_all() → FSM</text>
<text class="t-mut" x="657" y="85">the run begins</text>
<path class="e" d="M229,69 H281" marker-end="url(#valgate-ah)"/>
<text class="t-mut t-ok" x="255" y="57">Ok</text>
<path class="e" d="M504,69 H556" marker-end="url(#valgate-ah)"/>
<text class="t-mut t-ok" x="530" y="57">Ok</text>
<path class="e" d="M125,98 V231 Q125,239 133,239 H186" marker-end="url(#valgate-ah)"/>
<text class="t-mut t-err ta-s" x="136" y="124">no registered state handlers</text>
<text class="t-mut t-err ta-s" x="136" y="142">no exit states defined</text>
<text class="t-mut t-err ta-s" x="136" y="160">a split join_state that is neither</text>
<text class="t-mut t-err ta-s" x="136" y="178">registered nor an exit state</text>
<path class="e" d="M392,98 V206" marker-end="url(#valgate-ah)"/>
<text class="t-mut t-err ta-s" x="402" y="124">initial state neither</text>
<text class="t-mut t-err ta-s" x="402" y="142">registered nor an exit state</text>
<rect class="n-err" x="190" y="210" width="400" height="58" rx="10"/>
<text class="t-code" x="390" y="235">CanoError::Configuration</text>
<text class="t-mut" x="390" y="255">returned before setup_all() or any task runs</text>
<text class="t-mut" x="390" y="288">Calling validate() yourself runs the same checks earlier — at build time, not on the first run.</text>
</svg>
</div>
</div>

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
    let _result = workflow.orchestrate(State::Start, CancellationToken::disabled()).await?;
    Ok(())
}
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_validation</code> — a well-formed workflow that
passes, plus the failure cases (a missing transition target, an unregistered initial state) and the
exact errors <code>validate()</code> / <code>validate_initial_state()</code> return.</p>
</div>
<hr class="section-divider">

<h2 id="error-handling"><a href="#error-handling" class="anchor-link" aria-hidden="true">#</a>Error Handling</h2>
<p>
The <code>orchestrate()</code> method can return several error variants depending on what goes wrong
during execution. Understanding these errors helps you build robust error recovery logic.
</p>

<div class="diagram-frame">
<p class="diagram-label">Where each runtime error comes from</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 332" role="img">
<title>Map of orchestrate() failures: the run-wide budget and cancellation abort the in-flight attempt, state dispatch raises CanoError::Workflow, the attempt itself raises CircuitOpen, Timeout, RetryExhausted or TaskExecution, and a clean run returns Ok(final_state).</title>
<defs><marker id="errmap-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n-ghost" x="20" y="16" width="740" height="48" rx="10"/>
<text class="t-strong ta-s" x="38" y="36">Whole-run budget &amp; cancellation</text>
<text class="t-code t-err ta-s" x="38" y="55">WorkflowTimeout · Cancelled</text>
<text class="t-mut ta-e" x="742" y="46">in-flight task aborted → compensation stack drained</text>
<path class="e e-dash e-dim" d="M390,64 V96" marker-end="url(#errmap-ah)"/>
<text class="t-mut ta-s" x="398" y="84">aborts the attempt</text>
<rect class="n" x="25" y="100" width="200" height="58" rx="10"/>
<text class="t-strong" x="125" y="125">dispatch state</text>
<text class="t-mut" x="125" y="145">find the handler</text>
<rect class="n-hot" x="290" y="100" width="200" height="58" rx="10"/>
<text class="t-strong" x="390" y="125">run attempt</text>
<text class="t-mut" x="390" y="145">breaker → try → retry</text>
<rect class="n-ok" x="555" y="100" width="200" height="58" rx="10"/>
<text class="t-strong" x="655" y="125">next state</text>
<text class="t-mut" x="655" y="145">or a registered exit</text>
<path class="e" d="M229,129 H286" marker-end="url(#errmap-ah)"/>
<path class="e" d="M494,129 H551" marker-end="url(#errmap-ah)"/>
<path class="e" d="M125,158 V186" marker-end="url(#errmap-ah)"/>
<path class="e" d="M390,158 V186" marker-end="url(#errmap-ah)"/>
<path class="e" d="M655,158 V186" marker-end="url(#errmap-ah)"/>
<rect class="n-err" x="25" y="190" width="200" height="78" rx="10"/>
<text class="t-code t-err" x="125" y="213">CanoError::Workflow</text>
<text class="t-mut" x="125" y="233">no handler for state</text>
<text class="t-mut" x="125" y="251">single task → Split</text>
<rect class="n-err" x="290" y="190" width="200" height="78" rx="10"/>
<text class="t-code t-err" x="390" y="213">CircuitOpen · Timeout</text>
<text class="t-code t-err" x="390" y="233">RetryExhausted</text>
<text class="t-code t-err" x="390" y="251">TaskExecution · panic</text>
<rect class="n-ok" x="555" y="190" width="200" height="78" rx="10"/>
<text class="t-code t-ok" x="655" y="213">Ok(final_state)</text>
<text class="t-mut" x="655" y="233">exit state reached</text>
<text class="t-mut" x="655" y="251">teardown, then return</text>
<rect class="n-hot" x="25" y="286" width="730" height="34" rx="10"/>
<text class="t-strong" x="390" y="308">Every failure raised during the run is wrapped in CanoError::WithStateContext</text>
</svg>
</div>
</div>

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
<td><code>CanoError::WorkflowTimeout</code></td>
<td>Wall-clock budget set via <code>Workflow::with_total_timeout()</code> elapsed; in-flight task aborted, compensation stack drained. Surfaced under <code>CanoError::WithStateContext</code>.</td>
<td>Increase <code>with_total_timeout()</code> or speed up the workflow; see <a href="../../resilience/#workflow-total-timeout">Resilience → Workflow Total Timeout</a></td>
</tr>
<tr>
<td><code>CanoError::Cancelled</code></td>
<td>Run cancelled via a live <code>CancellationToken</code> passed to <code>orchestrate</code> / <code>resume_from</code>; in-flight task aborted, compensation stack drained. Surfaced under <code>CanoError::WithStateContext</code> (or <code>CompensationFailed</code> on a dirty rollback).</td>
<td>Expected when you cancel deliberately; see <a href="../../resilience/#cancellation">Resilience → Cooperative Cancellation</a></td>
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
<td>All retry attempts exhausted by a Task</td>
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
<td>Check the specific task logic — <code>TaskExecution</code>, <code>Store</code>, etc.</td>
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
match workflow.orchestrate(State::Start, CancellationToken::disabled()).await {
    Ok(final_state) => println!("Completed: {:?}", final_state),
    Err(CanoError::Workflow(msg)) => eprintln!("Workflow error: {}", msg),
    Err(CanoError::Configuration(msg)) => eprintln!("Config error: {}", msg),
    Err(CanoError::Timeout(msg)) => eprintln!("Attempt timed out: {}", msg),
    Err(CanoError::RetryExhausted(msg)) => eprintln!("Retries exhausted: {}", msg),
    Err(e) => eprintln!("Task error: {}", e),
}

```
</div>
