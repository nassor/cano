+++
title = "Validation and errors"
description = "Validate a Cano workflow with validate() and validate_initial_state(), and handle the CanoError variants that orchestrate() can return at runtime."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Validation and errors</h1>
<p class="subtitle">Check a workflow is wired correctly, and handle what <code>orchestrate()</code> returns.</p>

<p>
This page is a reference for validation and runtime errors. <a href="../">Workflows</a> covers defining states and building a workflow.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#validation">Workflow validation</a></li>
<li class="toc-sub"><a href="#validate-method">validate()</a></li>
<li class="toc-sub"><a href="#validate-initial-state">validate_initial_state()</a></li>
<li><a href="#error-handling">Error handling</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="validation"><a href="#validation" class="anchor-link" aria-hidden="true">#</a>Workflow validation</h2>
<p>
Two methods check a workflow for wiring mistakes: <code>validate()</code> checks its structure, and
<code>validate_initial_state()</code> checks where a run starts.
<code>orchestrate</code>, <code>resume_from</code> and their <code>_with_token</code> variants run both
before any resource is set up, so a mistake never reaches a task.
</p>

<div class="diagram-frame">
<p class="diagram-label">The gate every <code>orchestrate()</code> call passes through</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 24 780 331" role="img" aria-labelledby="val-gate-title val-gate-desc">
<title id="val-gate-title">The validation gate before every run</title>
<desc id="val-gate-desc">orchestrate() first calls validate(), which is cached and runs once per workflow. If it passes, it calls validate_initial_state(), which runs on every call. If that passes, resources are set up and the state machine starts. validate() fails on a missing state handler, a missing exit state, two compensatable states with one name, an invalid split JoinConfig, or a split join_state that is neither registered nor an exit state. validate_initial_state() fails when the initial state is neither registered nor an exit state. Either failure returns CanoError::Configuration before setup or any task runs.</desc>
<defs><marker id="val-gate-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n-hot" x="25" y="40" width="200" height="58" rx="10"/>
<text class="t-code" x="125" y="65">validate()</text>
<text class="t-mut" x="125" y="85">cached, runs once</text>
<rect class="n" x="285" y="40" width="215" height="58" rx="10"/>
<text class="t-code" x="392" y="65">validate_initial_state</text>
<text class="t-mut" x="392" y="85">every call, O(1)</text>
<rect class="n-ok" x="560" y="40" width="195" height="58" rx="10"/>
<text class="t-code" x="657" y="65">setup_all(), then FSM</text>
<text class="t-mut" x="657" y="85">the run begins</text>
<path class="e" d="M229,69 H281" marker-end="url(#val-gate-ah)"/>
<text class="t-mut t-ok" x="255" y="57">Ok</text>
<path class="e" d="M504,69 H556" marker-end="url(#val-gate-ah)"/>
<text class="t-mut t-ok" x="530" y="57">Ok</text>
<path class="e" d="M125,98 V271 Q125,279 133,279 H186" marker-end="url(#val-gate-ah)"/>
<text class="t-mut t-err ta-s" x="136" y="124">no registered state handlers</text>
<text class="t-mut t-err ta-s" x="136" y="142">no exit states defined</text>
<text class="t-mut t-err ta-s" x="136" y="160">two compensatable states share a name</text>
<text class="t-mut t-err ta-s" x="136" y="178">an invalid split JoinConfig</text>
<text class="t-mut t-err ta-s" x="136" y="196">a split join_state that is neither</text>
<text class="t-mut t-err ta-s" x="136" y="214">registered nor an exit state</text>
<path class="e" d="M392,98 V246" marker-end="url(#val-gate-ah)"/>
<text class="t-mut t-err ta-s" x="402" y="124">initial state neither</text>
<text class="t-mut t-err ta-s" x="402" y="142">registered nor an exit state</text>
<rect class="n-err" x="190" y="250" width="400" height="58" rx="10"/>
<text class="t-code" x="390" y="275">CanoError::Configuration</text>
<text class="t-mut" x="390" y="295">returned before setup_all() or any task runs</text>
<text class="t-mut" x="390" y="332">Calling validate() yourself runs the same checks earlier, at build time instead of on the first run.</text>
</svg>
</div>
</div>

<h3 id="validate-method"><a href="#validate-method" class="anchor-link" aria-hidden="true">#</a>validate()</h3>
<p>
Checks the workflow's structure and returns <code>CanoError::Configuration</code> when it finds a problem.
The engine calls it before every run and caches the result, so a workflow is checked once however many
times it runs. Calling it yourself runs the checks immediately.
</p>
<div class="card-stack">
<div class="card">
<h3>Checks performed</h3>
<p>The workflow has at least one registered state handler.</p>
<p>The workflow has at least one exit state.</p>
<p>No two compensatable states share a task <code>name()</code>. Override <code>name()</code> on one of them.</p>
<p>Each split's <code>JoinConfig</code> is valid: <code>PartialTimeout</code> has a timeout, a <code>Percentage</code> lies in <code>(0.0, 1.0]</code>, and a bulkhead is not 0.</p>
<p>Each split's join state is registered or an exit state.</p>
</div>
</div>

<h3 id="validate-initial-state"><a href="#validate-initial-state" class="anchor-link" aria-hidden="true">#</a>validate_initial_state()</h3>
<p>
Checks that a state can start a run: it must have a registered task or split handler, or be an exit state.
Otherwise it returns <code>CanoError::Configuration</code>.
</p>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Process, Complete }

struct MyTask;
struct ProcessTask;

#[task(state = State)]
impl MyTask {
    async fn run(&self) -> Result<State, CanoError> {
        Ok(State::Process)
    }
}

#[task(state = State)]
impl ProcessTask {
    async fn run(&self) -> Result<State, CanoError> {
        Ok(State::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let workflow = Workflow::new(Resources::new().insert(MemoryStore::new()))
        .register(State::Start, MyTask)
        .register(State::Process, ProcessTask)
        .add_exit_state(State::Complete);

    // Structure: handlers and exit states exist
    workflow.validate()?;

    // The initial state has a handler
    workflow.validate_initial_state(&State::Start)?;

    let _result = workflow.orchestrate(State::Start).await?;
    Ok(())
}
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_validation</code>. It validates a well-formed
workflow, then shows the failure cases (a missing transition target, an unregistered initial state) and the
exact errors <code>validate()</code> and <code>validate_initial_state()</code> return.</p>
</div>
<hr class="section-divider">

<h2 id="error-handling"><a href="#error-handling" class="anchor-link" aria-hidden="true">#</a>Error handling</h2>
<p>
<code>orchestrate()</code> sets up the workflow's resources, drives the state machine, then tears the resources
down on every exit path. A failing <code>setup</code> returns its error before any task runs. If the run fails,
its error is the one returned; a <code>teardown</code> error is returned only when the run itself succeeded.
</p>

<div class="diagram-frame">
<p class="diagram-label">Where each runtime error comes from</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 332" role="img" aria-labelledby="val-errors-title val-errors-desc">
<title id="val-errors-title">Where orchestrate() runtime errors come from</title>
<desc id="val-errors-desc">The run-wide time budget and cancellation sit above the whole run and abort the in-flight attempt, producing WorkflowTimeout or Cancelled and draining the compensation stack. Each state is dispatched in turn. Dispatch raises CanoError::Workflow when no handler exists for the state or a split join condition is not met. The run attempt, which goes through the circuit breaker, the try and the retry loop, raises CircuitOpen, Timeout, RetryExhausted or TaskExecution, including a task panic. A clean attempt reaches the next state or a registered exit state, and the run returns Ok with the final state after teardown. Every failure raised during the run is wrapped in CanoError::WithStateContext.</desc>
<defs><marker id="val-errors-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n-ghost" x="20" y="16" width="740" height="48" rx="10"/>
<text class="t-strong ta-s" x="38" y="36">Whole-run budget &amp; cancellation</text>
<text class="t-code t-err ta-s" x="38" y="55">WorkflowTimeout · Cancelled</text>
<text class="t-mut ta-e" x="742" y="46">in-flight task aborted, compensation stack drained</text>
<path class="e e-dash e-dim" d="M390,64 V96" marker-end="url(#val-errors-ah)"/>
<text class="t-mut ta-s" x="398" y="84">aborts the attempt</text>
<rect class="n" x="25" y="100" width="200" height="58" rx="10"/>
<text class="t-strong" x="125" y="125">dispatch state</text>
<text class="t-mut" x="125" y="145">find the handler</text>
<rect class="n-hot" x="290" y="100" width="200" height="58" rx="10"/>
<text class="t-strong" x="390" y="125">run attempt</text>
<text class="t-mut" x="390" y="145">breaker, try, retry</text>
<rect class="n-ok" x="555" y="100" width="200" height="58" rx="10"/>
<text class="t-strong" x="655" y="125">next state</text>
<text class="t-mut" x="655" y="145">or a registered exit</text>
<path class="e" d="M229,129 H286" marker-end="url(#val-errors-ah)"/>
<path class="e" d="M494,129 H551" marker-end="url(#val-errors-ah)"/>
<path class="e" d="M125,158 V186" marker-end="url(#val-errors-ah)"/>
<path class="e" d="M390,158 V186" marker-end="url(#val-errors-ah)"/>
<path class="e" d="M655,158 V186" marker-end="url(#val-errors-ah)"/>
<rect class="n-err" x="25" y="190" width="200" height="78" rx="10"/>
<text class="t-code t-err" x="125" y="213">CanoError::Workflow</text>
<text class="t-mut" x="125" y="233">no handler for state</text>
<text class="t-mut" x="125" y="251">join condition not met</text>
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

<p>Each entry below names a variant, what raises it, and how to fix it.</p>
<dl class="key-list">
<dt><code>CanoError::Workflow</code> <small>no handler</small></dt>
<dd>No task is registered for the current state. Register a task for every reachable state with <code>register()</code>.</dd>

<dt><code>CanoError::Workflow</code> <small>join condition</small></dt>
<dd>A split's join condition was not met, for example fewer branches succeeded than <code>JoinStrategy::Quorum</code> requires. The message lists the branch errors. Fix the failing branches or loosen the <code>JoinStrategy</code>; see <a href="../../split-join/">Split and join</a>.</dd>

<dt><code>CanoError::WorkflowTimeout</code></dt>
<dd>The wall-clock budget from <code>Workflow::with_total_timeout()</code> elapsed. The in-flight task is aborted and the compensation stack is drained. It surfaces under <code>CanoError::WithStateContext</code>. Raise the budget or speed up the workflow; see <a href="../../resilience/#workflow-total-timeout">Resilience: workflow total timeout</a>.</dd>

<dt><code>CanoError::Cancelled</code></dt>
<dd>The <code>CancellationToken</code> passed to <code>orchestrate_with_token</code> or <code>resume_from_with_token</code> was cancelled. The in-flight task is aborted and the compensation stack is drained. It surfaces under <code>CanoError::WithStateContext</code>, or under <code>CompensationFailed</code> on a dirty rollback. Test for it with <code>err.is_cancelled()</code>; see <a href="../../resilience/#cancellation">Resilience: cooperative cancellation</a>.</dd>

<dt><code>CanoError::Configuration</code></dt>
<dd>Raised before the run starts. <code>validate()</code> rejects a missing handler or exit state, a <code>PartialTimeout</code> strategy without a timeout, a <code>Percentage</code> outside <code>(0.0, 1.0]</code>, a bulkhead of 0, or two compensatable states with one task <code>name()</code>. Add <code>.with_timeout(duration)</code> to the <code>JoinConfig</code>, correct the value, or override <code>name()</code> on one compensatable task.</dd>

<dt><code>CanoError::Timeout</code></dt>
<dd>The per-attempt timeout from <code>TaskConfig::with_attempt_timeout()</code> elapsed. Raise it or speed up the task, and add a <code>RetryMode</code> if the delay is transient.</dd>

<dt><code>CanoError::RetryExhausted</code></dt>
<dd>A task configured with retries (<code>with_fixed_retry</code> or <code>with_exponential_retry</code>) failed every attempt. A task with the default config runs once and returns its own error unchanged. Raise the retry count or fix the transient failure.</dd>

<dt><code>CanoError::CircuitOpen</code></dt>
<dd>An open <code>CircuitBreaker</code> attached to the <code>TaskConfig</code> rejected the call. The retry loop stops at once and no attempt is consumed. Wait for the breaker's <code>reset_timeout</code> or fix the upstream dependency.</dd>

<dt><code>CanoError::TaskExecution</code> <small>panic</small></dt>
<dd>A single task panicked. The message starts with <code>"panic:"</code> and carries the panic payload. Fix the broken invariant in the task body.</dd>

<dt>Any other <code>CanoError</code> <small>task error</small></dt>
<dd>Whatever the task returned, such as <code>TaskExecution</code> or <code>Store</code>. Check the task's own logic.</dd>
</dl>

<div class="callout callout-info">
<div class="callout-label">Panic safety</div>
<p>
A single task runs under <code>catch_unwind</code>, so a panic surfaces as
<code>CanoError::TaskExecution("panic: ...")</code> and does not abort the workflow. Split branches run in a
<code>tokio::task::JoinSet</code>, so a panic there counts as a failed branch and goes through the join strategy.
</p>
</div>

```rust
match workflow.orchestrate(State::Start).await {
    Ok(final_state) => println!("Completed: {:?}", final_state),
    Err(e) => match e.inner() {
        CanoError::Workflow(msg) => eprintln!("Workflow error: {}", msg),
        CanoError::Configuration(msg) => eprintln!("Config error: {}", msg),
        CanoError::Timeout(msg) => eprintln!("Attempt timed out: {}", msg),
        CanoError::RetryExhausted { .. } => eprintln!("Retries exhausted: {}", e),
        _ => eprintln!("Task error: {}", e),
    },
}
```
</div>
