+++
title = "Crash recovery"
description = "Checkpoint a Cano workflow's FSM progress and resume a crashed run from where it left off."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Crash recovery</h1>
<p class="subtitle">Persist every state transition, then resume a crashed run from the last checkpoint.</p>
<p class="feature-tag">The <code>CheckpointStore</code> trait is always available. <code>RedbStore</code> is behind <code>store-redb</code>, which is on by default. <code>PostgresStore</code> is behind <code>store-postgres</code> and <code>NatsStore</code> behind <code>store-nats</code>.</p>

<p>
Attach a <code>CheckpointStore</code> to a <a href="../workflows/">Workflow</a> and the engine
records a <code>CheckpointRow</code> for the states it enters, each written before that state's task
runs. After a crash, <code>Workflow::resume_from</code> reloads the run, re-enters the FSM at the last
checkpointed state and continues forward.
</p>

<p>
The trait is backend agnostic: implement it over any storage you run. Three stores ship
with the crate, redb on by default, the two network stores each behind a feature:
<code>RedbStore</code> (an embedded, ACID, daemon-free file),
<code>PostgresStore</code> (one PostgreSQL table) and <code>NatsStore</code> (a
NATS JetStream Key/Value bucket). A workflow with no checkpoint store pays nothing. With the
<code>scheduler</code> feature each of the three also implements <code>SchedulerStore</code>, the shared
record that lets <a href="../scheduler/several-instances/">several scheduler instances</a> run one set of flows.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#attaching">Attaching a checkpoint store</a></li>
<li><a href="#what-is-recorded">What gets recorded</a></li>
<li><a href="#resuming">Resuming a run</a></li>
<li><a href="#versioning">Workflow versioning</a></li>
<li><a href="#idempotency">The idempotency contract</a></li>
<li><a href="#stores">Checkpoint stores</a></li>
<li><a href="#observer-events">Observer events</a></li>
<li><a href="#full-example">Full example</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="attaching"><a href="#attaching" class="anchor-link" aria-hidden="true">#</a>Attaching a checkpoint store</h2>
<p>
Two builders opt a workflow into checkpointing:
</p>
<dl class="key-list">
<dt><code>with_checkpoint_store(Arc&lt;dyn CheckpointStore&gt;)</code></dt>
<dd>The store the engine writes rows to.</dd>

<dt><code>with_workflow_id(id)</code></dt>
<dd>The identifier the run's rows are recorded under. <code>orchestrate</code> needs it: a store attached without an id fails with <code>CanoError::CheckpointStore</code> on the first checkpoint write. <code>resume_from</code> takes the id as an argument.</dd>
</dl>

```rust
use cano::prelude::*;
use cano::RedbStore;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Work, Done }

#[derive(Clone)] struct StartTask;
#[derive(Clone)] struct WorkTask;

#[task(state = Step)]
impl StartTask {
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Work)
    }
}
#[task(state = Step)]
impl WorkTask {
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let checkpoint_store = Arc::new(RedbStore::open_default()?);

    let workflow = Workflow::bare()
        .register(Step::Start, StartTask)
        .register(Step::Work, WorkTask)
        .add_exit_state(Step::Done)
        .with_checkpoint_store(checkpoint_store)
        .with_workflow_id("run-42");

    workflow.orchestrate(Step::Start).await?;
    Ok(())
}
```
<hr class="section-divider">

<h2 id="what-is-recorded"><a href="#what-is-recorded" class="anchor-link" aria-hidden="true">#</a>What gets recorded</h2>
<p>
On each loop iteration, before dispatching the state's task, the engine appends one row. A few
states differ, as listed after the type definitions.
</p>

```rust
pub struct CheckpointRow {
    pub sequence: u64,            // increases by one per row within the run
    pub state: String,            // the Debug rendering of the state value
    pub task_id: String,          // Task::name() for a single-task state; "" for split and exit states
    pub output_blob: Option<Vec<u8>>,  // serialized output (compensation), cursor (stepped, stream) or resume input (ResumeInput); else None
    pub kind: RowKind,            // what this row records
    pub workflow_version: u32,    // see Workflow::with_workflow_version (default 0)
}

#[non_exhaustive]
pub enum RowKind {
    StateEntry,              // the engine entered this state (the usual case)
    CompensationCompletion,  // a compensatable task finished; output_blob holds its serialized output
    StepCursor,              // a stepped or stream task advanced; output_blob holds the serde_json-encoded cursor
    Suspended,               // a SuspendTask finished suspend and the run is parked at this state; output_blob is None
    ResumeInput,             // Workflow::resume_with delivered an input; output_blob holds the serde_json-encoded input
}
```

<ul>
<li>An ordinary state gets one <code>StateEntry</code> row when entered, including the initial state and the exit state. A <code>Start</code>, <code>Work</code>, <code>Done</code> workflow records three rows.</li>
<li>A split state is one state, so one row regardless of how many tasks run in parallel.</li>
<li>A <a href="../router-task/">router</a> state writes no row and uses no sequence number. A router has no side effects, so there is nothing to recover.</li>
<li>A <a href="../saga/">compensatable</a> state that succeeds writes a second row, <code>CompensationCompletion</code>, whose <code>output_blob</code> is the serialized output. Its entry row and completion row use two sequence numbers.</li>
<li>A <a href="../stepped-task/">SteppedTask</a> writes a <code>StepCursor</code> row after each step that returns <code>More</code>, carrying the <code>serde_json</code>-encoded cursor. A <a href="../stream-task/">StreamTask</a> writes one per flushed window. Both register with the same <code>register</code> call as any task, and the rows are written only when a checkpoint store and a workflow id are both set. Without them the task runs in memory.</li>
<li>A <a href="../suspend-task/">SuspendTask</a> writes a <code>Suspended</code> row after <code>suspend</code> returns <code>Ok</code>, and <code>orchestrate</code> returns the parked state with the log kept. <code>Workflow::resume_with</code> writes a <code>ResumeInput</code> row carrying the <code>serde_json</code>-encoded input before it calls <code>resume</code>. Both need a checkpoint store and a workflow id.</li>
<li><code>state</code> is <code>format!("{state:?}")</code>, and resume maps it back to a registered or exit state, so each state needs a distinct <code>Debug</code> form. Any <code>#[derive(Debug)]</code> enum has one.</li>
</ul>

<p>Rows are append-only. Reconstructing a run means loading every row for the id in sequence order,
which is what <code>CheckpointStore::load_run</code> returns.</p>
<hr class="section-divider">

<h2 id="resuming"><a href="#resuming" class="anchor-link" aria-hidden="true">#</a>Resuming a run</h2>
<p>
<code>Workflow::resume_from(workflow_id)</code> loads the run with <code>load_run</code> and
re-enters the FSM from the highest-sequence row. <code>resume_from_with_token(workflow_id, &amp;token)</code>
makes the resumed run cancellable. When a workflow runs under the
<a href="../scheduler/#checkpoint-resume">scheduler</a>, the scheduler defaults <code>workflow_id</code> to
the flow id when unset, clears stale completed exit logs, and automatically resumes unfinished runs
on the next tick or failover.
</p>

<div class="diagram-frame">
<p class="diagram-label">Run 1 appends a row per state and crashes; run 2 resumes from the last row</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 566" role="img" aria-labelledby="recovery-resume-title recovery-resume-desc">
<title id="recovery-resume-title">A crashed run and its resume</title>
<desc id="recovery-resume-desc">Three lifelines: the engine of run 1, the CheckpointStore, and the engine of run 2. Run 1 appends row 0, a StateEntry for Start, then StartTask runs. It appends row 1, a StateEntry for Work, then WorkTask starts and the process crashes. Rows 0 and 1 are durable. Run 2 calls resume_from with the run id, which loads the run from the store and receives rows 0 and 1. The engine re-enters at Work, the last StateEntry row, and appends row 2 for Work. WorkTask runs again, so it must be idempotent. The engine then appends row 3 for Done. Every row is appended before its task runs, so the resumed state runs again.</desc>
<defs><marker id="recovery-resume-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="150" y1="46" x2="150" y2="206"/>
<line class="lifeline" x1="400" y1="46" x2="400" y2="526"/>
<line class="lifeline" x1="650" y1="46" x2="650" y2="526"/>
<rect class="band" x="142" y="94" width="16" height="34" rx="5"/>
<rect class="band" x="142" y="162" width="16" height="32" rx="5"/>
<rect class="band" x="642" y="462" width="16" height="30" rx="5"/>
<rect class="n" x="70" y="8" width="160" height="38" rx="8"/>
<text class="t-strong" x="150" y="28">Engine (run 1)</text>
<rect class="n-cop" x="315" y="8" width="170" height="38" rx="8"/>
<text class="t-strong" x="400" y="28">CheckpointStore</text>
<rect class="n" x="570" y="8" width="160" height="38" rx="8"/>
<text class="t-strong" x="650" y="28">Engine (run 2)</text>
<path class="e" d="M154,82 H396" marker-end="url(#recovery-resume-ah)"/>
<text class="t-code" x="275" y="70">append #0 · StateEntry · Start</text>
<text class="t-mut ta-e" x="134" y="112">StartTask runs</text>
<path class="e" d="M154,150 H396" marker-end="url(#recovery-resume-ah)"/>
<text class="t-code" x="275" y="138">append #1 · StateEntry · Work</text>
<text class="t-mut ta-e" x="134" y="179">WorkTask starts</text>
<text class="t-strong t-err" x="150" y="226">✗ crash</text>
<text class="t-mut" x="150" y="244">process dies mid-WorkTask</text>
<text class="t-mut ta-s" x="414" y="234">rows #0 and #1 are durable</text>
<rect class="n" x="550" y="252" width="200" height="40" rx="10"/>
<text class="t-code" x="650" y="273">resume_from("run-42")</text>
<path class="e" d="M646,322 H404" marker-end="url(#recovery-resume-ah)"/>
<text class="t-code" x="525" y="310">load_run("run-42")</text>
<path class="e e-dash" d="M404,356 H646" marker-end="url(#recovery-resume-ah)"/>
<text class="t-code t-mut" x="525" y="344">[#0 Start, #1 Work]</text>
<rect class="n-hot" x="542" y="374" width="216" height="48" rx="10"/>
<text class="t-strong" x="650" y="390">re-enter at Work</text>
<text class="t-mut" x="650" y="407">last StateEntry row wins</text>
<path class="e" d="M646,452 H404" marker-end="url(#recovery-resume-ah)"/>
<text class="t-code" x="525" y="440">append #2 · StateEntry · Work</text>
<text class="t-mut t-warn ta-e" x="634" y="478">WorkTask re-runs: idempotent</text>
<path class="e" d="M646,512 H404" marker-end="url(#recovery-resume-ah)"/>
<text class="t-code" x="525" y="500">append #3 · StateEntry · Done</text>
<text class="t-mut" x="390" y="546">Every row is appended before its task runs, so the resumed state runs again.</text>
</svg>
</div>
</div>
<p>Resume routes rows by their <code>kind</code>:</p>
<ul>
<li><code>StateEntry</code> rows drive the state replay. The engine takes the highest-sequence row, maps its label back to a state and re-enters the FSM loop there, continuing the same sequence.</li>
<li><code>CompensationCompletion</code> rows rebuild the <a href="../saga/">compensation stack</a> in sequence order, so a failure after the resume point can still roll back work an earlier process did. If the last row is itself a completion row, it is skipped: that state re-runs and pushes its entry again, and keeping both would compensate it twice.</li>
<li><code>StepCursor</code> rows feed a resumed <a href="../stepped-task/">SteppedTask</a> or <a href="../stream-task/">StreamTask</a>. The latest cursor for the resumed state goes to the first <code>step</code> call, or to <code>open</code> for a stream, instead of <code>None</code>.</li>
<li>A last row of <code>Suspended</code> marks a parked run. <code>resume_from</code> returns the parked state and runs nothing, and <code>resume_with</code> records the input and continues. A last row of <code>ResumeInput</code> means <code>resume</code> did not finish, so <code>resume_from</code> delivers the stored input again. Neither kind changes the <a href="../saga/">compensation stack</a> rebuild.</li>
</ul>
<p>
Resource <code>setup</code> and <code>teardown</code> wrap the resumed run as they do in
<code>orchestrate</code>: setup, rehydration and run, then teardown. The run's own error wins, and a
teardown error is returned only when the run succeeded. If the last recorded row is an exit state,
the run had already finished and <code>resume_from</code> returns that state immediately.
</p>

```rust
use cano::prelude::*;
use cano::RedbStore;
use std::sync::Arc;

// Step, StartTask and WorkTask are the ones from the first example.
#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let checkpoint_store = Arc::new(RedbStore::open_default()?);
    let workflow = Workflow::bare()
        .register(Step::Start, StartTask)
        .register(Step::Work, WorkTask)
        .add_exit_state(Step::Done)
        .with_checkpoint_store(checkpoint_store);

    // An earlier process crashed mid-run; pick up where it left off.
    let final_state = workflow.resume_from("run-42").await?;
    assert_eq!(final_state, Step::Done);
    Ok(())
}
```

<p>
<code>resume_from</code> returns these errors:
</p>
<dl class="key-list">
<dt><code>CanoError::Configuration</code></dt>
<dd>No checkpoint store is attached, or the workflow shape is invalid.</dd>

<dt><code>CanoError::CheckpointStore</code></dt>
<dd>The store failed, or it has no rows for the id.</dd>

<dt><code>CanoError::Workflow</code></dt>
<dd>The recorded state label matches no state of this workflow definition.</dd>

<dt><code>CanoError::WorkflowVersionMismatch { stored, expected }</code></dt>
<dd>The latest row's <code>workflow_version</code> differs from the version on this workflow. See <a href="#versioning">Workflow versioning</a>.</dd>
</dl>
<hr class="section-divider">

<h2 id="versioning"><a href="#versioning" class="anchor-link" aria-hidden="true">#</a>Workflow versioning</h2>
<p>
When a deploy changes the FSM shape, a checkpoint written by the old shape can no longer be resumed
safely. States added or removed, a renamed cursor schema or reordered split branches can re-enter a
state that no longer exists, or feed the wrong cursor format to a
<a href="../stepped-task/">SteppedTask</a>. <code>Workflow::with_workflow_version(u32)</code> guards
against that.
</p>

```rust
let workflow = Workflow::new(resources)
    .register(MyState::Start, StartTask)
    .register(MyState::Process, ProcessTask)
    .add_exit_state(MyState::Done)
    .with_checkpoint_store(checkpoint_store)
    .with_workflow_id("orders-v2")
    .with_workflow_version(2);
```

<p>
Every appended row is stamped with the configured version, <code>0</code> when the builder is never
called. On <code>resume_from</code>, the engine compares the highest-sequence row's version with the
workflow's, after it loads the rows and before any state is re-entered. A mismatch returns
<code>CanoError::WorkflowVersionMismatch { stored, expected }</code> and no task re-runs. Older rows
in a mixed log do not fail the resume. With the <code>tracing</code> feature, each logs a warning.
</p>

<div class="diagram-frame">
<p class="diagram-label">The <code>workflow_version</code> gate on <code>resume_from</code></p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 250" role="img" aria-labelledby="recovery-version-title recovery-version-desc">
<title id="recovery-version-title">The workflow_version gate on resume_from</title>
<desc id="recovery-version-desc">On resume_from, the engine loads the rows and compares the latest row's workflow_version with the version configured on this workflow. Equal versions resume the run by re-entering at the last state. Different versions return WorkflowVersionMismatch before any state is re-entered, so no task re-runs.</desc>
<defs><marker id="recovery-version-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M170,113 H246" marker-end="url(#recovery-version-ah)"/>
<text class="t-mut" x="208" y="101">latest row</text>
<path class="e" d="M480,104 H492 Q500,104 500,96 V58 Q500,50 508,50 H516" marker-end="url(#recovery-version-ah)"/>
<text class="t-mut t-ok ta-e" x="492" y="76">equal</text>
<path class="e" d="M480,122 H492 Q500,122 500,130 V168 Q500,176 508,176 H516" marker-end="url(#recovery-version-ah)"/>
<text class="t-mut t-err ta-e" x="492" y="158">differs</text>
<rect class="n" x="20" y="90" width="150" height="46" rx="10"/>
<text class="t-code" x="95" y="114">resume_from(id)</text>
<rect class="n-hot" x="250" y="84" width="230" height="58" rx="10"/>
<text class="t-strong" x="365" y="105">workflow_version gate</text>
<text class="t-code t-mut" x="365" y="123">stored vs expected</text>
<rect class="n-ok" x="520" y="22" width="240" height="56" rx="10"/>
<text class="t-strong" x="640" y="41">resume the run</text>
<text class="t-mut" x="640" y="59">re-enter at the last state</text>
<rect class="n-err" x="520" y="148" width="240" height="56" rx="10"/>
<text class="t-strong" x="640" y="167">refuse the resume</text>
<text class="t-code t-err" x="640" y="185">WorkflowVersionMismatch</text>
<text class="t-mut" x="390" y="232">The check runs before any state is re-entered, so no task re-runs.</text>
</svg>
</div>
</div>

<p>
Bump the version when a deploy changes the FSM in a way that makes an old checkpoint meaningless:
renamed or removed states, a changed <a href="../stepped-task/">SteppedTask</a> cursor format,
reordered split branches, restructured <a href="../saga/">compensation</a> outputs. Leave it alone
for changes that keep behavior: retry-policy tweaks, observability additions, internal refactors, and
new states added after the current exit state.
</p>

<p>
A workflow that never calls <code>with_workflow_version</code> uses <code>0</code> on both sides, so
the check always passes. <code>RedbStore</code> decodes rows stored without a version as
<code>0</code>, so an existing database resumes under a workflow at version 0 with no migration.
</p>
<hr class="section-divider">

<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The idempotency contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Important</span>
<p><strong>The task at the resumed state re-runs.</strong> The checkpoint for a state is written
before its task, so on resume the engine cannot tell whether that task's side effects already
happened. Tasks at and after the resume point must be idempotent: use upserts, dedupe keys and
conditional writes. States before the resume point never re-run.</p>
</div>
<hr class="section-divider">

<h2 id="stores"><a href="#stores" class="anchor-link" aria-hidden="true">#</a>Checkpoint stores</h2>
<p>
The log lives in a <code>CheckpointStore</code>: the built-in <code>RedbStore</code>,
<code>PostgresStore</code> or <code>NatsStore</code>, or your own implementation of the trait's three
methods (<code>append</code>, <code>load_run</code>, <code>clear</code>).
<a href="checkpoint-stores/">Checkpoint stores</a> compares the built-in stores, lists their settings,
and walks through writing and testing your own.
</p>
<hr class="section-divider">

<h2 id="observer-events"><a href="#observer-events" class="anchor-link" aria-hidden="true">#</a>Observer events</h2>
<p>
A <a href="../observers/"><code>WorkflowObserver</code></a> has four recovery hooks:
</p>
<dl class="key-list">
<dt><code>on_checkpoint(workflow_id: &amp;str, sequence: u64)</code></dt>
<dd>Fires after each row is durably appended. <code>TracingObserver</code> logs it at <code>DEBUG</code>.</dd>

<dt><code>on_resume(workflow_id: &amp;str, sequence: u64)</code></dt>
<dd>Fires once at the start of <code>resume_from</code>, after rehydration. <code>sequence</code> is the last persisted row, and execution continues from the state it recorded. <code>TracingObserver</code> logs it at <code>INFO</code>.</dd>

<dt><code>on_checkpoint_clear_failed(workflow_id: &amp;str, error: &amp;CanoError)</code></dt>
<dd>Fires when the final <code>clear</code> fails. The run's result is unaffected, but the stranded log can cause a duplicate-sequence error on a fresh run with the same id.</dd>

<dt><code>on_unknown_resume_state(workflow_id: &amp;str, sequence: u64, unknown_state_label: &amp;str)</code></dt>
<dd>Fires during resume for each <code>StateEntry</code> row whose label matches no registered state. The row is dropped from the rehydrated path and the resume continues.</dd>
</dl>
<p>
<code>TracingObserver</code> (behind the <code>tracing</code> feature) emits the last two as
<code>WARN</code> events under the <code>cano::observer</code> target.
</p>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full example</h2>
<p>
A <code>Start</code>, <code>Process</code>, <code>Finalize</code>, <code>Done</code> workflow whose
<code>Process</code> task fails on its first attempt, standing in for a crash, so <code>orchestrate</code>
returns an error. A follow-up <code>resume_from</code> completes the run. This is the
<code>workflow_recovery</code> example shipped with the crate: run it with
<code>cargo run --example workflow_recovery</code>. For a real crash (SIGKILL
mid-flight, then restart and resume in a fresh process), see the integration tests
<code>tests/recovery_e2e.rs</code> and <code>tests/stepped_resume_e2e.rs</code>, the
<code>stepped_task</code> example, and the Docker and Postgres suite in the <code>cano-e2e</code>
workspace member.
</p>

```rust
use cano::prelude::*;
use cano::RedbStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Process, Finalize, Done }

// Shared across runs through Resources: the first Process attempt errors, later ones succeed.
#[derive(Resource)]
struct CrashOnce { attempts: AtomicU32 }

#[derive(Clone)] struct StartTask;
#[derive(Clone)] struct ProcessTask;
#[derive(Clone)] struct FinalizeTask;

#[task(state = Step)]
impl StartTask {
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Process)
    }
}

#[task(state = Step)]
impl ProcessTask {
    // No retries by default, so the first failure leaves `orchestrate`, like a real crash.
    async fn run(&self, res: &Resources) -> Result<Step, CanoError> {
        let crash = res.get::<CrashOnce>()?;
        if crash.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(CanoError::task_execution("simulated crash"));
        }
        Ok(Step::Finalize)
    }
}

#[task(state = Step)]
impl FinalizeTask {
    async fn run(&self) -> Result<Step, CanoError> {
        Ok(Step::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let checkpoint_store = Arc::new(RedbStore::new("recovery.redb")?);
    let resources = Resources::new().insert(CrashOnce { attempts: AtomicU32::new(0) });
    let workflow = Workflow::new(resources)
        .register(Step::Start, StartTask)
        .register(Step::Process, ProcessTask)
        .register(Step::Finalize, FinalizeTask)
        .add_exit_state(Step::Done)
        .with_checkpoint_store(checkpoint_store.clone())
        .with_workflow_id("demo-run");

    // Run 1 fails inside ProcessTask. The Start and Process rows are already durable.
    let _ = workflow.orchestrate(Step::Start).await;

    // Run 2 resumes: ProcessTask runs again (and now succeeds), then the run finishes at Done.
    let final_state = workflow.resume_from("demo-run").await?;
    assert_eq!(final_state, Step::Done);

    // Reaching Done cleared the log, so nothing is left to print here.
    assert!(checkpoint_store.load_run("demo-run").await?.is_empty());
    Ok(())
}
```
</div>
