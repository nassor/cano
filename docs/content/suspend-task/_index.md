+++
title = "SuspendTask"
description = "SuspendTask in Cano - park a run at a state until an outside party delivers a typed input, then continue it with Workflow::resume_with."
template = "section.html"
+++

<div class="content-wrapper">
<h1>SuspendTask</h1>
<p class="subtitle">Durable pause and resume: park a run until a person or another service delivers a typed input.</p>

<p>
A <code>SuspendTask</code> parks a run at its state. When the engine enters the state it runs
<code>suspend</code> (typically to notify someone), records a checkpoint row and returns
<code>Ok(state)</code> from <code>orchestrate</code>. Later, <code>Workflow::resume_with(id, &amp;input)</code>
delivers a typed input, the engine runs <code>resume</code> with it and the workflow continues. The
pause can last days and span process restarts, because only the
<a href="../recovery/">checkpoint store</a> and your own storage carry state across it. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>,
<a href="../timer-task/">TimerTask</a>, <a href="../batch-task/">BatchTask</a>,
<a href="../stepped-task/">SteppedTask</a> and <a href="../stream-task/">StreamTask</a>, and it reads
typed dependencies from <a href="../resources/">Resources</a>. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance: <code>suspend</code> notifies, <code>resume</code> receives the input and returns the next state</span>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Review, Approved, Rejected }

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Decision { Approve, Reject }

struct HumanReview;

#[task::suspend(state = Step)]
impl HumanReview {
    // Resource-free methods omit `res: &Resources`.
    async fn suspend(&self) -> Result<(), CanoError> {
        // Notify the reviewer; writes RowKind::Suspended and parks orchestrate().
        Ok(())
    }

    // Invoked when Workflow::resume_with delivers Decision (writes RowKind::ResumeInput).
    async fn resume(&self, decision: Decision) -> Result<Step, CanoError> {
        Ok(match decision {
            Decision::Approve => Step::Approved,
            Decision::Reject => Step::Rejected,
        })
    }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">Parked, not failed</div>
<p>
A parked run returns <code>Ok</code>. Its checkpoint log and its compensation stack are kept, so the
run can continue under the same workflow id whenever the input arrives. Returning an error from
<code>suspend</code> is a failure like any other task error.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#how-it-works">How a suspend state runs</a></li>
<li><a href="#quick-start">Quick start with <code>#[task::suspend]</code></a></li>
<li><a href="#registering">Registering a suspend task</a></li>
<li><a href="#resuming">Delivering input and resuming</a></li>
<li><a href="#contract">The at-least-once contract</a></li>
<li><a href="#deadlines">Deadlines are inputs</a></li>
<li><a href="#rules-review">Example: a human review gate</a></li>
<li><a href="#explicit">Explicit trait-impl form</a></li>
<li><a href="#when-to-use">When to use SuspendTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How a suspend state runs</h2>

<div class="diagram-frame">
<p class="diagram-label">A run parks at a suspend state, then continues when an input arrives</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 748 236" role="img" aria-labelledby="suspend-flow-title suspend-flow-desc">
<title id="suspend-flow-title">Parking at a suspend state and resuming with an input</title>
<desc id="suspend-flow-desc">First call: orchestrate enters the suspend state and runs suspend. The engine writes a Suspended checkpoint row and orchestrate returns Ok with the parked state. Later call, possibly from another process: resume_with delivers an input. The engine writes a ResumeInput checkpoint row, runs resume with the input, and the workflow continues at the next state.</desc>
<defs><marker id="suspend-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-mut ta-s" x="20" y="28">first call</text>
<path class="e" d="M138,67 H178" marker-end="url(#suspend-ah)"/>
<path class="e" d="M288,67 H328" marker-end="url(#suspend-ah)"/>
<path class="e" d="M478,67 H518" marker-end="url(#suspend-ah)"/>
<path class="e e-hot e-dash" d="M623,90 V112 Q623,120 615,120 H103 Q95,120 95,128 V145" marker-end="url(#suspend-ah)"/>
<text class="t-mut" x="360" y="110">input arrives, possibly after a restart</text>
<path class="e" d="M170,173 H210" marker-end="url(#suspend-ah)"/>
<path class="e" d="M360,173 H400" marker-end="url(#suspend-ah)"/>
<path class="e" d="M530,173 H570" marker-end="url(#suspend-ah)"/>
<rect class="n" x="20" y="44" width="118" height="46" rx="10"/>
<text class="t-code" x="79" y="68">orchestrate</text>
<rect class="n" x="178" y="44" width="110" height="46" rx="10"/>
<text class="t-code" x="233" y="68">suspend()</text>
<rect class="n-cop" x="328" y="44" width="150" height="46" rx="10"/>
<text x="403" y="68">Suspended row</text>
<rect class="n-warn" x="518" y="44" width="210" height="46" rx="10"/>
<text x="623" y="68">parked: Ok(state)</text>
<rect class="n-hot" x="20" y="150" width="150" height="46" rx="10"/>
<text class="t-code" x="95" y="174">resume_with(input)</text>
<rect class="n-cop" x="210" y="150" width="150" height="46" rx="10"/>
<text x="285" y="174">ResumeInput row</text>
<rect class="n" x="400" y="150" width="130" height="46" rx="10"/>
<text class="t-code" x="465" y="174">resume(input)</text>
<rect class="n-ok" x="570" y="150" width="158" height="46" rx="10"/>
<text x="649" y="174">Next state</text>
<text class="t-mut ta-s" x="20" y="224">later call</text>
</svg>
</div>
</div>

<p>
A <code>SuspendTask</code> has one associated type, <code>type Input: serde::Serialize +
serde::de::DeserializeOwned + Clone + Send + Sync + 'static</code>, and two required methods. The
<code>#[task::suspend(state = ...)]</code> macro infers <code>Input</code> from the <code>resume</code>
signature, or you write it explicitly.
</p>

```rust
async fn suspend(&self, res: &Resources) -> Result<(), CanoError>;

async fn resume(&self, res: &Resources, input: Self::Input) -> Result<TState, CanoError>;
```

<p>The checkpoint log for one visit to a suspend state <code>Review</code> holds these rows:</p>
<dl class="key-list">
<dt><code>StateEntry</code> <small>written when the loop enters the state</small></dt>
<dd>The engine runs <code>suspend</code> next.</dd>

<dt><code>Suspended</code> <small>written after <code>suspend</code> returns <code>Ok</code></small></dt>
<dd>The run is parked. <code>orchestrate</code> returns <code>Ok(Review)</code>, the log stays and the compensation stack is not drained.</dd>

<dt><code>ResumeInput</code> <small>written by <code>resume_with</code></small></dt>
<dd>The input, encoded with <code>serde_json</code>. The state is re-entered without a second <code>StateEntry</code> row, <code>resume</code> runs, and the next state's <code>StateEntry</code> row follows.</dd>
</dl>

<p>
<code>fn config(&amp;self) -&gt; TaskConfig</code> defaults to <code>TaskConfig::new()</code>: one
attempt, no retries. The engine applies it to <code>suspend</code> and to <code>resume</code>
separately, so <code>TaskConfig::new().with_exponential_retry(3)</code> retries a failing call of
either. <code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code> defaults to the type name and
is stored in the checkpoint rows. <a href="../observers/">Observers</a> see each call as a task:
<code>on_task_start</code>, then <code>on_task_success</code> or <code>on_task_failure</code>.
</p>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start with <code>#[task::suspend]</code></h2>
<p>
Put <code>#[task::suspend(state = MyState)]</code> on an inherent <code>impl</code> block. The macro
infers <code>Input</code> from the type of the <code>input</code> parameter of <code>resume</code>, as
written, and writes the <code>impl SuspendTask&lt;MyState&gt; for ...</code> header. The
<code>res</code> parameter may be omitted from <code>suspend</code> and from <code>resume</code> when
they do not read resources.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9203;</span> Inference form: <code>#[task::suspend(state = ...)]</code> on an inherent impl</span>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Review, Approved, Rejected }

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Decision { Approve, Reject }

struct Review;

#[task::suspend(state = Step)]
impl Review {
    async fn suspend(&self) -> Result<(), CanoError> {
        // Notify the reviewer here.
        Ok(())
    }

    async fn resume(&self, decision: Decision) -> Result<Step, CanoError> {
        Ok(match decision {
            Decision::Approve => Step::Approved,
            Decision::Reject => Step::Rejected,
        })
    }
}
```
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a suspend task</h2>
<p>
Register a suspend task with <code>Workflow::register(state, task)</code>, the same call every task
model uses. A checkpoint store and a workflow id are required, because the parked run lives in the
checkpoint log. <code>validate()</code> fails with a <code>CanoError::Configuration</code> when a
workflow has a suspend state and no store.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Park a run, then deliver the input from a later call</span>

```rust
use cano::prelude::*;
use std::sync::Arc;

let store = Arc::new(RedbStore::new("/var/lib/myapp/cano.redb")?);
let workflow = Workflow::bare()
    .register(Step::Review, Review)
    .add_exit_states([Step::Approved, Step::Rejected])
    .with_checkpoint_store(store)
    .with_workflow_id("review-1");

// The run parks at Review: suspend() ran and orchestrate returned Ok.
assert_eq!(workflow.orchestrate(Step::Review).await?, Step::Review);

// Later, possibly after a restart, with a workflow built the same way:
let state = workflow.resume_with("review-1", &Decision::Approve).await?;
assert_eq!(state, Step::Approved);
```
</div>

<p>
Where the later call can run depends on the store. A
<a href="../recovery/checkpoint-stores/#redb"><code>RedbStore</code></a> file is held by one process at
a time, so the input is delivered by the process that parked the run, or by its replacement
after a restart. With <a href="../recovery/checkpoint-stores/#postgres"><code>PostgresStore</code></a>
or <a href="../recovery/checkpoint-stores/#nats"><code>NatsStore</code></a>, any process that reaches
the server can call <code>resume_with</code>, such as a web handler on another host that
receives the reviewer's decision.
</p>

<ul>
<li><code>orchestrate</code> returns the parked state. Compare it with the state you registered to tell a parked run from a finished one.</li>
<li>A suspend task has no companion <code>Task</code> impl, so it cannot be a <code>register_split</code> branch.</li>
<li><code>with_total_timeout</code> applies to each <code>orchestrate</code> or <code>resume_*</code> call. Time spent parked is not counted.</li>
<li>The <a href="../scheduler/">scheduler</a> reports a parked run as completed, because the run returned <code>Ok</code>.</li>
<li>With the <code>metrics</code> feature, a parked run increments <code>cano_workflow_runs_total</code> with the outcome label <code>suspended</code>.</li>
</ul>

<!-- Section: resuming -->
<hr class="section-divider">
<h2 id="resuming"><a href="#resuming" class="anchor-link" aria-hidden="true">#</a>Delivering input and resuming</h2>
<p>
<code>Workflow::resume_with(workflow_id, &amp;input)</code> serializes the input to JSON, checks it
against the task's <code>Input</code> type, appends the <code>ResumeInput</code> row, calls
<code>resume</code> and continues the workflow. It returns the exit state, or the next suspend state
the run parks at. <code>resume_with_token(workflow_id, &amp;input, &amp;token)</code> is the same call
cancellable through a <code>CancellationToken</code>.
</p>

<p>
The last checkpoint row decides what a call does. <code>resume_from(workflow_id)</code> and
<code>resume_with(workflow_id, &amp;input)</code> read it as follows:
</p>
<dl class="key-list">
<dt>Last row <code>Suspended</code> <small>the run is parked</small></dt>
<dd><code>resume_from</code> returns the parked state at once. No row is appended, no task runs and <code>on_resume</code> does not fire. <code>resume_with</code> validates the input, appends <code>ResumeInput</code> and runs <code>resume</code>.</dd>

<dt>Last row <code>ResumeInput</code> <small>an input was recorded and <code>resume</code> did not finish</small></dt>
<dd><code>resume_from</code> delivers the stored input to <code>resume</code> again. <code>resume_with</code> fails with a <code>CanoError::Workflow</code> that says the run "is not suspended".</dd>

<dt>Any other last row <small>including a crash inside <code>suspend</code></small></dt>
<dd><code>resume_from</code> resumes as usual, so a crash inside <code>suspend</code> re-runs <code>suspend</code>. <code>resume_with</code> fails with "is not suspended".</dd>
</dl>

<p>These errors come from <code>resume_with</code>:</p>
<dl class="key-list">
<dt><code>CanoError::Workflow</code> <small>the run is not parked</small></dt>
<dd>The message contains "is not suspended".</dd>

<dt><code>CanoError::TaskExecution</code> <small>wrong input type</small></dt>
<dd>The message contains "deserialize input". Nothing is recorded, so a following correct call succeeds.</dd>

<dt><code>CanoError::CheckpointStore</code> <small>concurrent delivery</small></dt>
<dd>Two concurrent calls for one run both append the same sequence number. The store rejects the duplicate, so one input wins and the other call fails. A network store built with <code>with_retry</code> re-sends an append whose acknowledgement was lost and accepts its own row, so that call still returns <code>Ok</code>; a different row from a competing call stays a conflict.</dd>

<dt><code>CanoError::CheckpointStore</code> <small>a network store timed out or lost its connection</small></dt>
<dd>The <code>ResumeInput</code> row may have been written anyway: see <a href="../recovery/checkpoint-stores/#network-failures">timeouts and retries</a>. Call <code>resume_from</code> next. It delivers a recorded input again, and returns the parked state when nothing was recorded. Send a different input only after it returns the parked state.</dd>

<dt>the error from <code>resume</code> <small>the task failed</small></dt>
<dd>With an empty compensation stack the recorded input stays in the log, so <code>resume_from</code> delivers it again. With a non-empty stack the run fails like any other: the stack drains.</dd>
</dl>

<p>
The compensation stack is rebuilt from the log on every resume. A <a href="../saga/">saga</a> task that
ran before the pause is compensated if a later state fails after the input arrives.
</p>

<!-- Section: contract -->
<hr class="section-divider">
<h2 id="contract"><a href="#contract" class="anchor-link" aria-hidden="true">#</a>The at-least-once contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Make <code>suspend</code> and <code>resume</code> idempotent</span>
<p>A crash inside <code>suspend</code> re-runs it on the next <code>resume_from</code>, and a crash
inside <code>resume</code> delivers the recorded input again. Both run at least once per visit.
Notifications need a dedupe key, and state changes need upserts or conditional writes.</p>
</div>

<p>
<a href="../resources/">Resources</a> are not checkpointed. Data that <code>resume</code> or a later
state needs after the pause belongs in a durable store that your application owns, such as a database
row keyed by the workflow id. The checkpoint log holds only the position of the run and the input
that was delivered.
</p>

<!-- Section: deadlines -->
<hr class="section-divider">
<h2 id="deadlines"><a href="#deadlines" class="anchor-link" aria-hidden="true">#</a>Deadlines are inputs</h2>
<p>
A suspend task has no built-in timeout. To expire a review, add a variant to the input type and let a
sweeper job deliver it with <code>resume_with</code> once the deadline passes.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9203;</span> An expiry delivered as an ordinary input</span>

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Decision {
    Approve,
    Reject { reason: String },
    Expired,
}

// In resume():
//   Decision::Expired => Ok(Step::Rejected)

// In a sweeper that runs outside the workflow:
// for id in overdue_review_ids().await? {
//     let _ = workflow.resume_with(id, &Decision::Expired).await;
// }
```
</div>

<p>
If the reviewer answered first, the sweeper's call fails with "is not suspended" or loses the
concurrent append, and the run keeps the reviewer's decision.
</p>

<!-- Section: rules review -->
<hr class="section-divider">
<h2 id="rules-review"><a href="#rules-review" class="anchor-link" aria-hidden="true">#</a>Example: a human review gate</h2>
<p>
The <code>suspend_task</code> example is a rules-review pipeline. An AI agent proposes a discount rule
set, a policy gate checks it, a human reviews it and the approved set is deployed. Cano ships no
"human review" or "rules" task: the pipeline is application code built from <code>Task</code>,
<a href="../router-task/">RouterTask</a>, <code>SuspendTask</code> and, where a step needs undoing,
<a href="../saga/">saga</a> tasks.
</p>

<ul>
<li><code>SuggestRules</code> is a plain <code>Task</code>. It writes a proposal to the application database, with a lower discount cap when the reviewer left feedback.</li>
<li><code>PolicyGate</code> is a <code>RouterTask</code>. It sends proposals within the 30 percent limit to <code>Review</code> and the rest to the exit state <code>GateRejected</code>.</li>
<li><code>HumanReview</code> is the <code>SuspendTask</code>. <code>suspend</code> requests the review and <code>resume</code> maps the reviewer's <code>Decision</code> to the next state.</li>
<li><code>DeployRules</code> is a plain <code>Task</code> that copies the proposal to the deployed slot.</li>
</ul>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> The review task: notify in <code>suspend</code>, act on the decision in <code>resume</code></span>

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Decision {
    Approve,
    Reject { reason: String },
    RequestChanges { feedback: String },
}

struct HumanReview;

#[task::suspend(state = Step)]
impl HumanReview {
    async fn suspend(&self, res: &Resources) -> Result<(), CanoError> {
        let db = res.get::<RulesDb>()?;
        // RulesDb holds an application DB handle (e.g. std::sync::Mutex or a DB client).
        db.0.lock().expect("db lock").review_requests += 1;
        println!("review requested");
        Ok(())
    }

    async fn resume(&self, res: &Resources, decision: Decision) -> Result<Step, CanoError> {
        Ok(match decision {
            Decision::Approve => Step::Deploy,
            Decision::Reject { .. } => Step::Rejected,
            Decision::RequestChanges { feedback } => {
                res.get::<RulesDb>()?.0.lock().expect("db lock").feedback = Some(feedback);
                Step::Suggest
            }
        })
    }
}
```
</div>

<p>
<code>main</code> rebuilds the workflow before every engine call. That shows that only the checkpoint
store and the application database <code>RulesDb</code> carry state across the pause.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9654;</span> The call sequence in <code>main</code></span>

```rust
let run_id = "rules-42";

// 1. Suggest, gate, request review: the run parks.
let state = build(db.clone(), store.clone(), run_id).orchestrate(Step::Suggest).await?;
assert_eq!(state, Step::Review);                       // review_requests == 1

// 2. No decision yet: nothing runs.
let state = build(db.clone(), store.clone(), run_id).resume_from(run_id).await?;
assert_eq!(state, Step::Review);                       // review_requests still 1

// 3. The reviewer asks for changes: Suggest and PolicyGate run again, then it parks again.
let decision = Decision::RequestChanges { feedback: "cap discounts at 20%".into() };
let state = build(db.clone(), store.clone(), run_id).resume_with(run_id, &decision).await?;
assert_eq!(state, Step::Review);                       // review_requests == 2, revision 2

// 4. The reviewer approves: Deploy runs and the checkpoint log is cleared.
let state = build(db.clone(), store.clone(), run_id).resume_with(run_id, &Decision::Approve).await?;
assert_eq!(state, Step::Deployed);
assert!(store.load_run(run_id).await?.is_empty());
```
</div>

<p>
Run it with <code>cargo run --example suspend_task</code>. The command is the same
on Linux, macOS and Windows.
</p>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit trait-impl form</h2>
<p>
To name the <code>Input</code> type or write a generic impl, put a bare <code>#[task::suspend]</code>
on an <code>impl SuspendTask&lt;...&gt; for ...</code> block and declare <code>type Input</code>
yourself.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form: <code>#[task::suspend]</code> on a trait impl</span>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Review, Approved, Rejected }

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Decision { Approve, Reject }

struct Review;

#[task::suspend]
impl SuspendTask<Step> for Review {
    type Input = Decision;

    fn config(&self) -> TaskConfig {
        // Applied independently to both suspend() and resume().
        TaskConfig::new().with_exponential_retry(3)
    }

    async fn suspend(&self, _res: &Resources) -> Result<(), CanoError> {
        Ok(())
    }

    async fn resume(&self, _res: &Resources, input: Decision) -> Result<Step, CanoError> {
        Ok(match input {
            Decision::Approve => Step::Approved,
            Decision::Reject => Step::Rejected,
        })
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to use SuspendTask</h2>
<p>Use a <code>SuspendTask</code> when a run has to wait for something that arrives from outside the
process and may take longer than a process lives: a human approval, a webhook callback, a message
from another service. The input is typed, recorded before it is acted on and delivered at least once.</p>
<p>
For a wait that ends on its own, use a <a href="../timer-task/">TimerTask</a> or a
<a href="../poll-task/">PollTask</a>. For a short wait inside one process, an ordinary task that awaits a
channel is simpler.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
Run <code>cargo run --example suspend_task</code>. It parks a review, checks that
an idle <code>resume_from</code> runs nothing, sends a request for changes and then an approval, and
asserts the result of each call.
</p>
</div>
</div>
