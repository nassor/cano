+++
title = "Crash Recovery"
description = "Checkpoint a Cano workflow's FSM progress and resume a crashed run from where it left off."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Crash Recovery</h1>
<p class="subtitle">Persist every state transition; resume a crashed run from the last checkpoint.</p>

<p>
Attach a <strong><code>CheckpointStore</code></strong> to a <a href="../workflows/">Workflow</a>
and the FSM records a <strong><code>CheckpointRow</code></strong> for the states it enters —
written <em>before</em> that state's task runs. After a crash, <code>Workflow::resume_from</code>
reloads the run, re-enters the FSM at the last checkpointed state, and continues forward.
</p>

<div class="callout callout-warning">
<span class="callout-label">Breaking change in 0.12.0</span>
<p>
<code>CheckpointRow</code> gained a <code>kind: RowKind</code> field. <strong>Any custom
<code>CheckpointStore</code> implementation must add <code>kind</code> to its row mapping</strong>
(persist it, return it on load). The built-in <code>RedbCheckpointStore</code> handles this, with a
legacy fallback so pre-0.12 redb files still load (old rows decode as <code>RowKind::StateEntry</code>).
See <a href="#what-is-recorded">What Gets Recorded</a> and <a href="#trait">The <code>CheckpointStore</code> Trait</a> below.
</p>
</div>
<p>
The <code>CheckpointStore</code> trait is <strong>backend-agnostic and always available</strong> —
implement it over Postgres, an HTTP service, a file, anything. With the <code>recovery</code>
feature, <code>RedbCheckpointStore</code> is a batteries-included embedded, ACID, daemon-free
implementation. A workflow with no checkpoint store keeps its zero-overhead behavior.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#attaching">Attaching a Checkpoint Store</a></li>
<li><a href="#what-is-recorded">What Gets Recorded</a></li>
<li><a href="#resuming">Resuming a Run</a></li>
<li><a href="#idempotency">The Idempotency Contract</a></li>
<li><a href="#trait">The <code>CheckpointStore</code> Trait</a></li>
<li><a href="#redb">Built-in: <code>RedbCheckpointStore</code></a></li>
<li><a href="#observer-events">Observer Events</a></li>
<li><a href="#full-example">Full Example</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="attaching"><a href="#attaching" class="anchor-link" aria-hidden="true">#</a>Attaching a Checkpoint Store</h2>
<p>
Two builders opt a workflow into checkpointing:
</p>
<ul>
<li><code>Workflow::with_checkpoint_store(Arc&lt;dyn CheckpointStore&gt;)</code> — the store to write to.</li>
<li><code>Workflow::with_workflow_id(id)</code> — the identifier this run's rows are recorded under.
Required for the forward (<code>orchestrate</code>) direction; <code>resume_from</code> takes the id
explicitly. A store attached without an id errors on the first checkpoint write.</li>
</ul>

```rust
use cano::prelude::*;
use cano::RedbCheckpointStore;            // behind the `recovery` feature
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Work, Done }

#[derive(Clone)] struct StartTask;
#[derive(Clone)] struct WorkTask;

#[task(state = Step)]
impl StartTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Work))
    }
}
#[task(state = Step)]
impl WorkTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

let store = Arc::new(RedbCheckpointStore::new("workflow.redb")?);

let workflow = Workflow::bare()
    .register(Step::Start, StartTask)
    .register(Step::Work, WorkTask)
    .add_exit_state(Step::Done)
    .with_checkpoint_store(store)
    .with_workflow_id("run-42");

workflow.orchestrate(Step::Start).await?;
```
<hr class="section-divider">

<h2 id="what-is-recorded"><a href="#what-is-recorded" class="anchor-link" aria-hidden="true">#</a>What Gets Recorded</h2>
<p>
Each loop iteration of the FSM, <em>before</em> dispatching the state's task, appends a row (with a
couple of exceptions noted below):
</p>

```rust
pub struct CheckpointRow {
    pub sequence: u64,            // monotonically increasing within the run
    pub state: String,            // the Debug rendering of the state value
    pub task_id: String,          // Task::name() for a single-task state; "" for split / exit
    pub kind: RowKind,            // what this row records — see RowKind
    pub output_blob: Option<Vec<u8>>,  // serialized payload for compensation / stepped cursor (else None)
}

#[non_exhaustive]
pub enum RowKind {
    StateEntry,              // the engine entered this state (the usual case)
    CompensationCompletion,  // a compensatable task finished; output_blob = its serialized output
    StepCursor,              // a SteppedTask advanced; output_blob = the serde_json-encoded cursor
}
```

<ul>
<li><strong>Ordinary states</strong> get one <code>StateEntry</code> row when entered — including the
initial state and the terminal exit state. A <code>Start → Work → Done</code> workflow records three
<code>StateEntry</code> rows.</li>
<li>A <strong>split state is one state ⇒ one row</strong>, not one row per parallel task.</li>
<li><strong><a href="../router-task/">Router</a> states write <em>no</em> row</strong> (and consume
no sequence number) — a pure router has no side effects, so there's nothing to recover.</li>
<li>A <strong><a href="../saga/">compensatable</a> state that succeeds</strong> writes a second
<code>CompensationCompletion</code> row whose <code>output_blob</code> is the serialized output (so
its entry row and completion row consume two sequence numbers).</li>
<li>A <strong><a href="../stepped-task/">SteppedTask</a> registered with <code>register_stepped</code></strong>
writes a <code>StepCursor</code> row after each step, carrying the <code>serde_json</code>-encoded
cursor.</li>
<li><code>state</code> is <code>format!("{state:?}")</code>; resume maps it back to the matching
registered or exit state, so each state must have a distinct <code>Debug</code> form (true for any
<code>#[derive(Debug)]</code> enum).</li>
</ul>

<p>Rows are <strong>append-only</strong>. Reconstructing a run is "load every row for this id, in
sequence order" — which is exactly what <code>CheckpointStore::load_run</code> returns.</p>
<hr class="section-divider">

<h2 id="resuming"><a href="#resuming" class="anchor-link" aria-hidden="true">#</a>Resuming a Run</h2>
<p>
<code>Workflow::resume_from(workflow_id)</code> loads the run via <code>load_run</code> and routes by
each row's <code>kind</code>:
</p>
<ul>
<li><code>StateEntry</code> rows drive the <strong>state replay</strong> — it takes the highest-<code>sequence</code>
<code>StateEntry</code> row, maps its label back to a state, and re-enters the FSM loop from there,
continuing the same sequence.</li>
<li><code>CompensationCompletion</code> rows rehydrate the <a href="../saga/">compensation stack</a>
(in sequence order) — so a failure after the resume point can still roll back work an earlier process
did. (The resume point's <em>own</em> entry row is excluded — that state re-runs and re-pushes its
entry, so replaying both would compensate it twice.)</li>
<li><code>StepCursor</code> rows feed the resumed <a href="../stepped-task/">SteppedTask</a>: the
latest cursor for the resumed state is decoded and handed to the first <code>step</code> call instead
of <code>None</code>.</li>
</ul>
<p>
Resources' <code>setup</code> / <code>teardown</code> wrap it exactly as in <code>orchestrate</code>.
If the last recorded row is an exit state, the run had already finished, so <code>resume_from</code>
returns it immediately.
</p>

```rust
use cano::prelude::*;
use cano::RedbCheckpointStore;
use std::sync::Arc;

let store = Arc::new(RedbCheckpointStore::new("workflow.redb")?);
let workflow = Workflow::bare()
    .register(Step::Start, StartTask)
    .register(Step::Work, WorkTask)
    .add_exit_state(Step::Done)
    .with_checkpoint_store(store);

// Some earlier process crashed mid-run; pick up where it left off.
let final_state = workflow.resume_from("run-42").await?;
assert_eq!(final_state, Step::Done);
```

<p>
<code>resume_from</code> errors with <code>CanoError::CheckpointStore</code> when the store fails or
there are no rows for the id, and with <code>CanoError::Workflow</code> when the recorded state
label doesn't match any state of <em>this</em> workflow definition.
</p>
<hr class="section-divider">

<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The Idempotency Contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Important</span>
<p><strong>The task at the resumed state re-runs.</strong> The checkpoint for a state is written
<em>before</em> its task — so when you resume, the engine cannot tell whether that task's side
effects already happened. Tasks at and after the resume point must therefore be
<strong>idempotent</strong>: re-running them must be safe (use upserts, dedupe keys, conditional
writes, …). States <em>before</em> the resume point are never re-run.</p>
</div>
<hr class="section-divider">

<h2 id="trait"><a href="#trait" class="anchor-link" aria-hidden="true">#</a>The <code>CheckpointStore</code> Trait</h2>
<p>
The trait is intentionally tiny and has <strong>no feature flag</strong> — implement it over any
storage you like:
</p>

```rust
use cano::recovery::{CheckpointRow, CheckpointStore};
use cano::CanoError;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct InMemoryStore(Mutex<HashMap<String, Vec<CheckpointRow>>>);

// `#[cano::checkpoint_store]` on an inherent `impl` builds the `impl CheckpointStore for …`
// header for you (it also rewrites the `async fn`s). Or write that header yourself:
// `#[cano::checkpoint_store] impl CheckpointStore for InMemoryStore { … }`.
#[cano::checkpoint_store]
impl InMemoryStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        self.0.lock().unwrap().entry(workflow_id.to_string()).or_default().push(row);
        Ok(())
    }
    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let mut rows = self.0.lock().unwrap().get(workflow_id).cloned().unwrap_or_default();
        rows.sort_by_key(|r| r.sequence);   // load_run must return ascending by `sequence`
        Ok(rows)
    }
    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.0.lock().unwrap().remove(workflow_id);
        Ok(())
    }
}
```

<p>
Contract: <code>append</code> durably persists the row — <strong>including its <code>kind</code> and
<code>output_blob</code></strong> — and must <strong>reject a duplicate <code>(workflow_id, sequence)</code></strong>
with an <code>Err</code> rather than overwriting (a collision means two runs share a workflow id;
<code>RedbCheckpointStore</code> enforces this via its composite key, as does the Postgres impl in
<code>cano-e2e</code> via its primary key). <code>load_run</code> returns every row ever appended for
the id, <strong>sorted ascending by <code>sequence</code></strong>, or an empty <code>Vec</code> for
an unknown id; <code>clear</code> removes a run's rows and must not touch any other id (clearing an
unknown id is a no-op). The example above stores the whole <code>CheckpointRow</code>, so it carries
<code>kind</code> for free — a store that maps rows to columns must add a <code>kind</code> column
(this is the <strong>0.12.0 breaking change</strong> for custom impls). The engine calls
<code>clear</code> automatically when a run reaches an exit state — and after a fully successful
<a href="../saga/">compensation rollback</a> — so a finished run leaves no recovery log behind
(it's best-effort: a <code>clear</code> failure is logged, not fatal). Implementations must be
<code>Send + Sync + 'static</code> so one store can be shared (typically as
<code>Arc&lt;dyn CheckpointStore&gt;</code>) across concurrent workflows.
</p>
<hr class="section-divider">

<h2 id="redb"><a href="#redb" class="anchor-link" aria-hidden="true">#</a>Built-in: <code>RedbCheckpointStore</code></h2>
<p class="feature-tag">Behind the <code>recovery</code> feature gate (<code>features = ["recovery"]</code>). The <code>CheckpointStore</code> trait and <code>CheckpointRow</code> are always available.</p>

<p>
<code>RedbCheckpointStore</code> is an embedded, ACID, pure-Rust key-value store
(<a href="https://docs.rs/redb">redb</a>) with no background daemon — a natural fit for a recovery
log that ships in the same process as the engine. Construct one with
<code>RedbCheckpointStore::new(path)</code>; it creates the database file (if absent) and the
checkpoint table. It is cheap to clone — the handle is held behind an <code>Arc</code>.
</p>

```rust
use cano::RedbCheckpointStore;
use std::sync::Arc;

let store: Arc<RedbCheckpointStore> = Arc::new(RedbCheckpointStore::new("workflow.redb")?);
// pass it to the workflow: .with_checkpoint_store(store)
```

<p>
Internally a single table maps <code>(workflow_id, sequence)</code> to the
<a href="https://docs.rs/postcard"><code>postcard</code></a>-encoded payload. redb orders composite
keys element by element, so a workflow's rows are stored — and range-scanned — in ascending
<code>sequence</code> order; the <code>sequence</code> lives in the key, not the value. The on-disk
row now carries the <code>kind</code> discriminant; <code>RedbCheckpointStore</code> keeps a legacy
fallback so a pre-0.12 redb file still loads — rows written without a <code>kind</code> field decode
as <code>RowKind::StateEntry</code>.
</p>
<hr class="section-divider">

<h2 id="observer-events"><a href="#observer-events" class="anchor-link" aria-hidden="true">#</a>Observer Events</h2>
<p>
A <a href="../observers/"><code>WorkflowObserver</code></a> sees two recovery hooks:
</p>
<div class="card-stack">
<div class="card">
<h3><code>on_checkpoint(workflow_id: &amp;str, sequence: u64)</code></h3>
<p>Fired after each <code>CheckpointRow</code> is durably appended.</p>
</div>
<div class="card">
<h3><code>on_resume(workflow_id: &amp;str, sequence: u64)</code></h3>
<p>Fired once at the start of <code>resume_from</code> — <code>sequence</code> is the last
persisted row's sequence; execution continues from the state that row recorded.</p>
</div>
</div>
<p>
<code>TracingObserver</code> (behind the <code>tracing</code> feature) re-emits them as
<code>cano::observer</code> events: <code>on_checkpoint</code> at <code>DEBUG</code>,
<code>on_resume</code> at <code>INFO</code>.
</p>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full Example</h2>
<p>
A <code>Start → Process → Finalize → Done</code> workflow whose <code>Process</code> task fails the
first time (standing in for a crash) — so <code>orchestrate</code> returns an error — and then
completes on a follow-up <code>resume_from</code>. This is the <code>workflow_recovery</code> example
shipped with the crate; run it with
<code>cargo run --example workflow_recovery --features recovery</code>. For a <em>real</em> crash
(SIGKILL mid-flight, then restart-and-resume in a fresh process), see the integration tests
<code>tests/recovery_e2e.rs</code> and <code>tests/stepped_resume_e2e.rs</code>, the
<code>examples/stepped_task.rs</code> example, and the Docker/Postgres end-to-end suite in the
<code>cano-e2e</code> workspace member.
</p>

```rust
use cano::prelude::*;
use cano::RedbCheckpointStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Process, Finalize, Done }

// Shared across runs via Resources: the first Process attempt errors; later ones succeed.
#[derive(Default)]
struct CrashOnce { attempts: AtomicU32 }
#[resource]
impl Resource for CrashOnce {}

#[derive(Clone)] struct StartTask;
#[derive(Clone)] struct ProcessTask;
#[derive(Clone)] struct FinalizeTask;

#[task(state = Step)]
impl StartTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Process))
    }
}

#[task(state = Step)]
impl ProcessTask {
    // No retries — let the first failure bubble out of `orchestrate`, like a real crash.
    fn config(&self) -> TaskConfig { TaskConfig::minimal() }
    async fn run(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let crash = res.get::<CrashOnce, _>("crash")?;
        if crash.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(CanoError::task_execution("simulated crash"));
        }
        Ok(TaskResult::Single(Step::Finalize))
    }
}

#[task(state = Step)]
impl FinalizeTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

let store = Arc::new(RedbCheckpointStore::new("recovery.redb")?);
let resources = Resources::new().insert("crash", CrashOnce::default());
let workflow = Workflow::new(resources)
    .register(Step::Start, StartTask)
    .register(Step::Process, ProcessTask)
    .register(Step::Finalize, FinalizeTask)
    .add_exit_state(Step::Done)
    .with_checkpoint_store(store.clone())
    .with_workflow_id("demo-run");

// Run 1: crashes inside ProcessTask. The Start and Process rows are already durable.
let _ = workflow.orchestrate(Step::Start).await;

// Run 2: resume — re-runs ProcessTask (now it succeeds) and finishes at Done.
let final_state = workflow.resume_from("demo-run").await?;
assert_eq!(final_state, Step::Done);

// The append-only log: Start, Process (crash), Process (re-run), Finalize, Done —
// all RowKind::StateEntry here, since this workflow has no compensatable or stepped states.
for row in store.load_run("demo-run").await? {
    println!("#{} {:?} {}  {}", row.sequence, row.kind, row.state, row.task_id);
}
```
</div>
