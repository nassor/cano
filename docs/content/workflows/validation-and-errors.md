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
<td><code>CanoError::Workflow</code></td>
<td>Legacy <code>with_timeout()</code> outer <code>tokio::time::timeout</code> elapsed (no graceful compensation)</td>
<td>Prefer <code>with_total_timeout()</code> for new code; otherwise increase <code>with_timeout()</code> or optimize task execution time</td>
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
match workflow.orchestrate(State::Start).await {
    Ok(final_state) => println!("Completed: {:?}", final_state),
    Err(CanoError::Workflow(msg)) => eprintln!("Workflow error: {}", msg),
    Err(CanoError::Configuration(msg)) => eprintln!("Config error: {}", msg),
    Err(CanoError::Timeout(msg)) => eprintln!("Attempt timed out: {}", msg),
    Err(CanoError::RetryExhausted(msg)) => eprintln!("Retries exhausted: {}", msg),
    Err(e) => eprintln!("Task error: {}", e),
}

```
</div>
