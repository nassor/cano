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
and the FSM records one <strong><code>CheckpointRow</code></strong> for every state it enters —
written <em>before</em> that state's task runs. After a crash, <code>Workflow::resume_from</code>
reloads the run, re-enters the FSM at the last checkpointed state, and continues forward.
</p>
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
Each loop iteration of the FSM, <em>before</em> dispatching the state's task, appends one row:
</p>

```rust
pub struct CheckpointRow {
    pub sequence: u64,            // monotonically increasing within the run
    pub state: String,            // the Debug rendering of the state value
    pub task_id: String,          // Task::name() for a single-task state; "" for split / exit
    pub output_blob: Option<Vec<u8>>,  // serialized output of a compensatable task (else None)
}
```

<ul>
<li>One row per <strong>state entered</strong> — including the initial state and the terminal
exit state. A <code>Start → Work → Done</code> workflow records three rows.</li>
<li>A <strong>split state is one state ⇒ one row</strong>, not one row per parallel task.</li>
<li><code>state</code> is <code>format!("{state:?}")</code>; resume maps it back to the matching
registered or exit state, so each state must have a distinct <code>Debug</code> form (true for any
<code>#[derive(Debug)]</code> enum).</li>
<li><code>output_blob</code> carries a <a href="../saga/">compensatable task</a>'s serialized
output: a successful compensatable state writes a second <em>completion</em> row with it set, and
<code>resume_from</code> rehydrates the compensation stack from those rows. Plain rows leave it
<code>None</code>.</li>
</ul>

<p>Rows are <strong>append-only</strong>. Reconstructing a run is "load every row for this id, in
sequence order" — which is exactly what <code>CheckpointStore::load_run</code> returns.</p>
<hr class="section-divider">

<h2 id="resuming"><a href="#resuming" class="anchor-link" aria-hidden="true">#</a>Resuming a Run</h2>
<p>
<code>Workflow::resume_from(workflow_id)</code> loads the run via <code>load_run</code>, takes the
highest-<code>sequence</code> row, maps its label back to a state, and re-enters the FSM loop from
there — continuing the same sequence. Resources' <code>setup</code> / <code>teardown</code> wrap it
exactly as in <code>orchestrate</code>. If the last recorded row is an exit state, the run had
already finished, so <code>resume_from</code> returns it immediately.
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
Contract: <code>append</code> durably persists the row; <code>load_run</code> returns every row
ever appended for the id, <strong>sorted ascending by <code>sequence</code></strong>, or an empty
<code>Vec</code> for an unknown id; <code>clear</code> removes a run's rows and must not touch any
other id (clearing an unknown id is a no-op). The engine calls <code>clear</code> automatically
when a run reaches an exit state — and after a fully successful
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
<a href="https://docs.rs/bincode"><code>bincode</code></a>-encoded payload. redb orders composite
keys element by element, so a workflow's rows are stored — and range-scanned — in ascending
<code>sequence</code> order; the <code>sequence</code> lives in the key, not the value.
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
<code>cargo run --example workflow_recovery --features recovery</code>.
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

// The append-only log: Start, Process (crash), Process (re-run), Finalize, Done.
for row in store.load_run("demo-run").await? {
    println!("#{} {}  {}", row.sequence, row.state, row.task_id);
}
```
</div>
