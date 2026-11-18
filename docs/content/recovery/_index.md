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
<li><a href="#trait">The <code>CheckpointStore</code> trait</a></li>
<li><a href="#redb">Built in: <code>RedbStore</code></a></li>
<li><a href="#postgres">Built in: <code>PostgresStore</code></a></li>
<li><a href="#nats">Built in: <code>NatsStore</code></a></li>
<li><a href="#network-failures">Timeouts and retries in the network stores</a></li>
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

<h2 id="trait"><a href="#trait" class="anchor-link" aria-hidden="true">#</a>The <code>CheckpointStore</code> trait</h2>
<p>
A store is a storage backend in <code>cano::store</code> that implements <code>CheckpointStore</code>.
To write your own, implement the trait over any storage; it has three methods and no feature flag:
</p>

```rust
use cano::checkpoint::{CheckpointRow, CheckpointStore};
use cano::CanoError;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MapStore(Mutex<HashMap<String, Vec<CheckpointRow>>>);

// `#[cano::checkpoint_store]` on an inherent `impl` builds the `impl CheckpointStore for ...`
// header for you and rewrites the `async fn`s. You can also write the header yourself:
// `#[cano::checkpoint_store] impl CheckpointStore for MapStore { ... }`.
#[cano::checkpoint_store]
impl MapStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        self.0.lock().unwrap().entry(workflow_id.to_string()).or_default().push(row);
        Ok(())
    }
    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let mut rows = self.0.lock().unwrap().get(workflow_id).cloned().unwrap_or_default();
        rows.sort_by_key(|r| r.sequence);   // load_run must return rows in ascending sequence
        Ok(rows)
    }
    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.0.lock().unwrap().remove(workflow_id);
        Ok(())
    }
}
```

<p>The contract for an implementation:</p>
<dl class="key-list">
<dt><code>append(workflow_id, row)</code></dt>
<dd>Durably persists the row with its <code>kind</code>, <code>output_blob</code> and <code>workflow_version</code>. It must reject a duplicate <code>(workflow_id, sequence)</code> with an <code>Err</code> instead of overwriting: a collision means two runs share a workflow id. <code>RedbStore</code> enforces this with its composite key, <code>PostgresStore</code> with its primary key and <code>NatsStore</code> with an expected-revision write. The sample above skips the check for brevity; <code>custom_checkpoint_store</code> includes it.</dd>

<dt><code>load_run(workflow_id)</code></dt>
<dd>Returns every row ever appended for the id, sorted ascending by <code>sequence</code>, or an empty <code>Vec</code> for an unknown id.</dd>

<dt><code>clear(workflow_id)</code></dt>
<dd>Removes the run's rows and touches no other id. Clearing an unknown id is a no-op.</dd>
</dl>
<p>
A store that maps rows to columns needs one for <code>kind</code> and one for
<code>workflow_version</code>; the sample stores whole rows, so it has both. Implementations must be
<code>Send + Sync + 'static</code> so one store can be shared, typically as
<code>Arc&lt;dyn CheckpointStore&gt;</code>, across concurrent workflows.
</p>
<p>
With the <code>testing</code> feature,
<a href="../testing/#store-contract"><code>assert_checkpoint_store_contract</code></a> runs this
contract, plus isolation, concurrency and large-blob cases, against your implementation.
</p>
<p>
The engine calls <code>clear</code> when a run reaches an exit state, and after a fully successful
<a href="../saga/">compensation rollback</a>, so a finished run leaves no log behind. A failed run
with nothing to compensate keeps its log, so it can be resumed. A <code>clear</code> failure is not
fatal: the engine fires <code>on_checkpoint_clear_failed</code> and returns the run's own result.
</p>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example custom_checkpoint_store</code>. A complete in-memory
<code>CheckpointStore</code> built with <code>#[cano::checkpoint_store]</code>, including the
duplicate <code>(workflow_id, sequence)</code> rejection, wired into a workflow with a
<code>resume_from</code> walk-through. It needs no feature flag.</p>
</div>
<hr class="section-divider">

<h2 id="redb"><a href="#redb" class="anchor-link" aria-hidden="true">#</a>Built in: <code>RedbStore</code></h2>
<p class="feature-tag">Behind the <code>store-redb</code> feature, which is on by default. <code>default-features = false</code> removes redb from the build.</p>

<p>
<code>RedbStore</code> is an embedded, ACID, pure-Rust key-value store
(<a href="https://docs.rs/redb">redb</a>) with no background daemon, which suits a recovery log that
lives in the same process as the engine. <code>RedbStore::new(path)</code> creates the
database file if absent and the checkpoint table. Clones share one handle behind an
<code>Arc</code>.
</p>

```rust
use cano::RedbStore;
use std::sync::Arc;

let checkpoint_store: Arc<RedbStore> = Arc::new(RedbStore::new("workflow.redb")?);
// pass it to the workflow: .with_checkpoint_store(checkpoint_store)
```

<p>
<code>new(path)</code> needs the file's directory to exist. <code>RedbStore::open_default()</code>
opens <code>cano.redb</code> in a persistent directory, the first of the rules below that
applies, and creates missing directories. <code>RedbStore::default_path()</code> returns
that file without opening it. In the paths, <code>app</code> is the executable's file name without
its extension.
</p>
<dl class="key-list">
<dt><code>CANO_DATA_DIR</code> <small>any platform, when set</small></dt>
<dd><code>$CANO_DATA_DIR/cano.redb</code></dd>

<dt><code>STATE_DIRECTORY</code> <small>a systemd unit with <code>StateDirectory=</code></small></dt>
<dd><code>$STATE_DIRECTORY/cano/cano.redb</code>, from the first entry when the variable lists several</dd>

<dt>Linux <small>and other Unix systems except macOS</small></dt>
<dd><code>$XDG_STATE_HOME/cano/app/cano.redb</code>, or <code>~/.local/state/cano/app/cano.redb</code> when <code>XDG_STATE_HOME</code> is unset or not an absolute path</dd>

<dt>macOS</dt>
<dd><code>~/Library/Application Support/cano/app/cano.redb</code></dd>

<dt>Windows</dt>
<dd><code>%LOCALAPPDATA%\cano\app\cano.redb</code>, usually under <code>C:\Users\name\AppData\Local</code></dd>
</dl>
<p>
redb locks the file, so one process holds it at a time: a second open fails while the first store
is alive. Renaming the executable changes <code>app</code> and starts a new file, so a service sets
<code>CANO_DATA_DIR</code> or runs under systemd with <code>StateDirectory=</code>.
</p>

<p>
One table maps <code>(workflow_id, sequence)</code> to a <code>ROW_TAG_V1</code> (<code>CV</code>) tagged
<a href="https://docs.rs/postcard"><code>postcard</code></a>-encoded payload holding the other
<code>CheckpointRow</code> fields, including <code>kind</code> (while still decoding untagged legacy rows).
redb orders composite keys element by element, so a workflow's rows are range-scanned in ascending
<code>sequence</code>. The sequence lives in the key, not the value.
</p>
<p>
With the <code>scheduler</code> feature <code>RedbStore</code> also implements
<code>SchedulerStore</code> in the tables <code>cano_scheduler</code> and <code>cano_scheduler_version</code> of the
same file. Because one process holds the file, it keeps scheduler state across restarts and coordinates schedulers
inside that process, not across processes. See <a href="../scheduler/several-instances/">Several scheduler instances</a>.
</p>
<hr class="section-divider">

<h2 id="postgres"><a href="#postgres" class="anchor-link" aria-hidden="true">#</a>Built in: <code>PostgresStore</code></h2>
<p class="feature-tag">Behind the <code>store-postgres</code> feature gate (<code>features = ["store-postgres"]</code>).</p>

<p>
<code>PostgresStore</code> keeps every run in one table, <code>cano_checkpoints</code> by
default (<code>DEFAULT_CHECKPOINT_TABLE</code>), and draws connections from a <a href="https://docs.rs/deadpool-postgres">deadpool-postgres</a>
pool over <a href="https://docs.rs/tokio-postgres">tokio-postgres</a>. The module
<code>cano::store::postgres</code> re-exports both crates. Clones share the pool.
</p>
<p>
With the <code>scheduler</code> feature <code>PostgresStore</code> also implements <code>SchedulerStore</code> in a
second table, <code>cano_scheduler</code> by default (<code>DEFAULT_SCHEDULER_TABLE</code>), renamed with
<code>with_scheduler_table</code>. See <a href="../scheduler/several-instances/">Several scheduler instances</a>.
</p>

```rust
use cano::PostgresStore;
use cano::RetryMode;
use std::sync::Arc;
use std::time::Duration;

let checkpoint_store = PostgresStore::builder()
    .with_schema("workflows")
    .with_max_pool_size(8)
    .with_operation_timeout(Duration::from_secs(5))
    .with_retry(RetryMode::fixed(3, Duration::from_millis(200)))
    .connect("postgres://cano:secret@db.internal/app")
    .await?;
// pass it to the workflow: .with_checkpoint_store(Arc::new(checkpoint_store))
```

<p>The builder settings, with their defaults in small type:</p>
<dl class="key-list">
<dt><code>with_checkpoint_table(name)</code> <small><code>cano_checkpoints</code></small></dt>
<dd>The table name: 1 to 63 characters of <code>[a-z0-9_]</code>, not starting with a digit.</dd>

<dt><code>with_schema(name)</code> <small>none</small></dt>
<dd>The schema holding the table. Without one, the server's <code>search_path</code> resolves the name. The store never creates a schema.</dd>

<dt><code>with_create_table(bool)</code> <small><code>true</code></small></dt>
<dd>Create the table on connect when it is missing. With <code>false</code>, run the DDL from <code>schema_sql()</code> yourself.</dd>

<dt><code>with_max_pool_size(n)</code> <small>twice the core count</small></dt>
<dd>The most connections the pool opens.</dd>

<dt><code>with_pool_timeouts(Timeouts)</code> <small>none</small></dt>
<dd>deadpool's waits for a free slot, a new connection and a recycle check.</dd>

<dt><code>with_recycling_method(RecyclingMethod)</code> <small><code>Fast</code></small></dt>
<dd>How the pool checks a connection before reuse.</dd>

<dt><code>with_operation_timeout(Duration)</code> <small>10 s</small></dt>
<dd>The bound on each attempt of both <code>CheckpointStore</code> (<code>append</code>, <code>load_run</code>, <code>clear</code>) and <code>SchedulerStore</code> (<code>load</code>, <code>compare_and_swap</code>, <code>remove</code>), connection checkout and migration included.</dd>

<dt><code>with_retry(RetryMode)</code> <small><code>RetryMode::None</code></small></dt>
<dd>Jittered retries for transient failures across both <code>CheckpointStore</code> and <code>SchedulerStore</code>. See <a href="#network-failures">timeouts and retries</a>.</dd>
</dl>

<p>
<code>connect(dsn)</code> takes a libpq connection string and connects without TLS.
<code>connect_with_tls(config, tls)</code> takes a <code>tokio_postgres::Config</code> and any TLS
connector, and <code>build_with_pool(pool)</code> uses a pool you built, ignoring the pool settings
above. The store sets <code>application_name</code> to <code>cano</code> unless your config sets one.
</p>

<p>
The table, created under an advisory lock so that processes starting together do not race:
</p>

```sql
CREATE TABLE IF NOT EXISTS "cano_checkpoints" (
    sequence         bigint      NOT NULL,
    workflow_version bigint      NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    kind             smallint    NOT NULL,
    workflow_id      text COLLATE "C" NOT NULL,
    state            text        NOT NULL,
    task_id          text        NOT NULL,
    output_blob      bytea,
    PRIMARY KEY (workflow_id, sequence)
);
```

<p>
The primary key is the only index. It rejects a duplicate sequence, and <code>load_run</code> and
<code>clear</code> each read one range of it, so no statement the store runs scans the whole table.
<code>output_blob</code> holds the row's bytes exactly as written. <code>kind</code>
stores <code>StateEntry</code> as 0, <code>CompensationCompletion</code> as 1, <code>StepCursor</code>
as 2, <code>Suspended</code> as 3 and <code>ResumeInput</code> as 4. <code>created_at</code> is for
operators and is not indexed.
</p>
<hr class="section-divider">

<h2 id="nats"><a href="#nats" class="anchor-link" aria-hidden="true">#</a>Built in: <code>NatsStore</code></h2>
<p class="feature-tag">Behind the <code>store-nats</code> feature gate (<code>features = ["store-nats"]</code>).</p>

<p>
<code>NatsStore</code> keeps every run in one JetStream Key/Value bucket,
<code>cano_checkpoints</code> by default (<code>DEFAULT_CHECKPOINT_BUCKET</code>, stream <code>KV_cano_checkpoints</code>). The module
<code>cano::store::nats</code> re-exports <a href="https://docs.rs/async-nats">async-nats</a>.
Clones share the client.
</p>
<p>
With the <code>scheduler</code> feature <code>NatsStore</code> also implements <code>SchedulerStore</code> in a
second bucket, <code>cano_scheduler</code> by default (<code>DEFAULT_SCHEDULER_BUCKET</code>), set with
<code>with_scheduler_bucket</code> and <code>with_scheduler_bucket_config</code>. <code>scheduler_kv()</code> returns
its handle. See <a href="../scheduler/several-instances/">Several scheduler instances</a>.
</p>

```rust
use cano::NatsStore;
use cano::store::nats::async_nats::ConnectOptions;
use std::sync::Arc;

let checkpoint_store = NatsStore::builder()
    .with_checkpoint_bucket("orders_checkpoints")
    .connect_with_options(
        "nats://nats.internal:4222",
        ConnectOptions::with_user_and_password("cano".into(), "secret".into()),
    )
    .await?;
// pass it to the workflow: .with_checkpoint_store(Arc::new(checkpoint_store))
```

<p>The builder settings, with their defaults in small type:</p>
<dl class="key-list">
<dt><code>with_checkpoint_bucket(name)</code> <small><code>cano_checkpoints</code></small></dt>
<dd>The bucket name: one or more of <code>[A-Za-z0-9_-]</code>.</dd>

<dt><code>with_checkpoint_bucket_config(kv::Config)</code> <small><code>history: 1</code></small></dt>
<dd>The configuration used when the store creates the bucket: storage, replicas, placement, compression, mirror. Setting a non-zero <code>max_age</code> is rejected with <code>CanoError::Configuration</code> so checkpoint and scheduler state cannot silently expire.</dd>

<dt><code>with_create_bucket(bool)</code> <small><code>true</code></small></dt>
<dd>Create the bucket on connect when it is missing.</dd>

<dt><code>with_domain(name)</code>, <code>with_api_prefix(prefix)</code> <small>none</small></dt>
<dd>Reach JetStream in another domain or through an imported API prefix. Set at most one.</dd>

<dt><code>with_operation_timeout(Duration)</code> <small>10 s</small></dt>
<dd>The bound on each attempt of both <code>CheckpointStore</code> (<code>append</code>, <code>load_run</code>, <code>clear</code>) and <code>SchedulerStore</code> (<code>load</code>, <code>compare_and_swap</code>, <code>remove</code>), and on opening a bucket.</dd>

<dt><code>with_retry(RetryMode)</code> <small><code>RetryMode::None</code></small></dt>
<dd>Jittered retries for transient failures across both <code>CheckpointStore</code> and <code>SchedulerStore</code>, plus bucket opening. See <a href="#network-failures">timeouts and retries</a>.</dd>
</dl>

<p>
<code>connect(addrs)</code> connects with the connection name <code>cano</code>.
<code>connect_with_options(addrs, options)</code> uses your <code>ConnectOptions</code> as given
(credentials, TLS, reconnect policy), and <code>build_with_client</code> and
<code>build_with_context</code> reuse a client or JetStream context you already have.
</p>

<p>
Each row is one key, <code>{workflow_id}.{sequence}</code>, holding a <code>ROW_TAG_V2</code>
(<code>CW</code>) tagged postcard encoding with a fresh per-append 128-bit <code>write_id</code>
(decoding both <code>ROW_TAG_V1</code> and <code>ROW_TAG_V2</code>). Workflow id bytes outside
<code>[A-Za-z0-9_-]</code> are written as <code>=</code> plus two hex digits, so <code>order-1</code>
stays <code>order-1</code> and <code>order.2</code> becomes <code>order=2E2</code>. <code>append</code>
writes with an expected revision of 0, so a second write to a key fails. <code>load_run</code> cleans up
its ephemeral ordered consumer, <code>clear</code> purges the run's keys from the stream in one
request, and scheduler <code>remove</code> checks the key's revision before purging.
</p>
<hr class="section-divider">

<h2 id="network-failures"><a href="#network-failures" class="anchor-link" aria-hidden="true">#</a>Timeouts and retries in the network stores</h2>
<p>
<code>PostgresStore</code> and <code>NatsStore</code> apply <code>with_operation_timeout</code> and
jittered <code>with_retry</code> across both <code>CheckpointStore</code> (<code>append</code>,
<code>load_run</code>, <code>clear</code>) and <code>SchedulerStore</code> (<code>load</code>,
<code>compare_and_swap</code>, <code>remove</code>). Transient errors (a timeout, a dropped
connection, a serialization failure or a shutting-down server) are retried with randomized jitter to avoid
herd spikes on store recovery. A conflict or a malformed row is never retried. A Postgres connection whose
call timed out is closed, never returned to the pool.
</p>
<p>
An <code>Err</code> from <code>append</code> or <code>compare_and_swap</code> after a timeout does not prove
the write is absent: the mutation may have landed after the client gave up. Each <code>append</code> call
generates a fresh random per-append <code>write_id</code> (stored in a <code>write_id</code> column by
<code>PostgresStore</code> and in the <code>ROW_TAG_V2</code> header by <code>NatsStore</code>), so a
retried <code>append</code> treats a stored row at that sequence as its own earlier write only when both the
row contents and <code>write_id</code> match. Concurrent fresh runs starting at sequence <code>0</code> with
identical initial states therefore never mistake each other's row for their own. Similarly, a retried
<code>SchedulerStore::compare_and_swap</code> accepts a stored record whose bytes match what it sent,
because each <code>FlowRecord</code> stamps <code>written_by</code>, <code>written_session</code>,
<code>written_at</code> and <code>write_seq</code>.
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
