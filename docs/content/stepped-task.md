+++
title = "SteppedTask"
description = "SteppedTask in Cano - resumable iterative work that checkpoints a cursor so a crashed run resumes mid-loop."
template = "page.html"
+++

<div class="content-wrapper">
<h1>SteppedTask</h1>
<p class="subtitle">Resumable iterative work — checkpoint a cursor, resume mid-loop after a crash.</p>

<p>
A <code>SteppedTask</code> does work in discrete steps. Each step takes the previous step's cursor and
returns either "more work, here's the new cursor" or "done, here's the next state". When a
<a href="../recovery/">checkpoint store</a> is attached, the engine persists that cursor after every
step — so a crash mid-loop resumes from where it left off, not from step zero. It is one of the
<a href="../task/">Task</a> family of processing models, alongside <a href="../nodes/">Node</a>,
<a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>, and
<a href="../batch-task/">BatchTask</a>, and it reads typed dependencies from
<a href="../resources/">Resources</a> like the rest. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first; for the
cursor-persistence half, <a href="../recovery/">Recovery</a>.

</p>

<div class="code-block">
<span class="code-block-label">At a glance — each <code>step</code> returns <code>More(cursor)</code> or <code>Done(state)</code></span>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Scan, Done }

#[derive(Serialize, Deserialize, Clone)]
struct Cursor { page: u32 }

struct ScanPages;

#[task::stepped(state = Stage)]
impl ScanPages {
    async fn step(&self, _res: &Resources, cursor: Option<Cursor>)
        -> Result<StepOutcome<Cursor, Stage>, CanoError>
    {
        let c = cursor.unwrap_or(Cursor { page: 0 });
        if c.page >= 50 {
            return Ok(StepOutcome::Done(TaskResult::Single(Stage::Done)));
        }
        // ...process page c.page...
        Ok(StepOutcome::More(Cursor { page: c.page + 1 }))   // cursor persisted here when a store is attached
    }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
A <code>SteppedTask</code> gives you crash-resume granularity <em>finer than a workflow state</em>.
A long job that would otherwise restart from the top after a crash instead restarts from its last
persisted cursor — the page number, the offset, the continuation token. The price is idempotency:
the step at and after the resume point may re-run.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#how-it-works">How the Step Loop Works</a></li>
<li><a href="#quick-start">Quick Start with <code>#[task::stepped]</code></a></li>
<li><a href="#registering">Registering a Stepped Task</a></li>
<li><a href="#persistence">Cursor Persistence &amp; Resume</a></li>
<li><a href="#idempotency">The Idempotency Contract</a></li>
<li><a href="#explicit">Explicit Trait-Impl Form</a></li>
<li><a href="#object-safe">Type-Erased Aliases</a></li>
<li><a href="#when-to-use">When to Use SteppedTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the Step Loop Works</h2>

<div class="diagram-frame">
<p class="diagram-label">The step loop, with a crash &amp; resume from the last cursor</p>
<div class="mermaid">
graph LR
A["step(None)"] -->|"More(c1)"| P1[persist cursor c1]
P1 --> B["step(Some c1)"]
B -->|"More(c2)"| P2[persist cursor c2]
P2 --> C[…]
C -->|"Done(result)"| D[Next State]
P2 -. crash .-> R["resume_from"]
R -->|"step(Some c2)"| C
</div>
</div>

<p>
A <code>SteppedTask</code> has one associated type — <code>type Cursor: serde::Serialize +
serde::de::DeserializeOwned + Send + Sync + 'static</code> (inferred by
<code>#[task::stepped(state = ...)]</code> from the <code>step</code> signature, or written explicitly)
— and one required method:
</p>

```rust
async fn step(
    &self,
    res: &Resources,
    cursor: Option<Self::Cursor>,
) -> Result<StepOutcome<Self::Cursor, TState>, CanoError>;
```

<ul>
<li><code>cursor</code> is <code>None</code> on the first call (or a fresh run); on resume it's the
last <em>persisted</em> cursor.</li>
<li>The outcome — <code>enum StepOutcome&lt;TCursor, TState&gt; { More(TCursor), Done(TaskResult&lt;TState&gt;) }</code>:
<code>More(c)</code> continues, threading <code>c</code> into the next call; <code>Done(result)</code>
ends the loop and forwards <code>result</code> to the FSM.</li>
</ul>

<p>
Optional: <code>fn config(&amp;self) -&gt; TaskConfig</code> (defaults to <code>TaskConfig::default()</code>
— guards individual <code>step</code> calls) and <code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code>
(defaults to the type name).
</p>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start with <code>#[task::stepped]</code></h2>
<p>
Attach <code>#[task::stepped(state = MyState)]</code> to an inherent <code>impl</code> block. The macro
infers <code>Cursor</code> from the <code>Option&lt;_&gt;</code> / <code>StepOutcome&lt;_, _&gt;</code>
types, injects default <code>config</code> / <code>name</code> if absent, synthesises the
<code>impl SteppedTask&lt;MyState&gt; for Cruncher</code> header, and emits a companion
<code>impl Task&lt;MyState&gt; for Cruncher</code> whose <code>run</code> runs the loop in memory (via
<code>cano::task::stepped::run_stepped</code>) — useful if you register it with plain
<code>register</code> and don't want persistence.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128260;</span> Inference form — <code>#[task::stepped(state = ...)]</code> on an inherent impl</span>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Crunch, Report, Done }

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Progress { processed: u32, total: u32 }

struct Cruncher;

#[task::stepped(state = Stage)]
impl Cruncher {
    async fn step(&self, _res: &Resources, cursor: Option<Progress>)
        -> Result<StepOutcome<Progress, Stage>, CanoError>
    {
        let p = cursor.unwrap_or(Progress { processed: 0, total: 1000 });
        if p.processed >= p.total {
            return Ok(StepOutcome::Done(TaskResult::Single(Stage::Report)));
        }
        // ... do a chunk of work for batch p.processed ...
        Ok(StepOutcome::More(Progress { processed: p.processed + 1, ..p }))
    }
}
```
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a Stepped Task</h2>
<p>
Register a stepped task with <code>Workflow::register_stepped(state, task)</code> — <strong>not</strong>
<code>register</code>. (You <em>can</em> use plain <code>register</code> — the companion
<code>impl Task</code> the macro emitted runs the loop in memory — but then you lose the cursor
persistence; <code>register_stepped</code> is what hooks the engine-driven step loop up.)
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a stepped task into a checkpointed workflow</span>

```rust
use cano::prelude::*;
use cano::RedbCheckpointStore;            // requires the `recovery` feature
use std::sync::Arc;

let store = RedbCheckpointStore::new("/var/lib/myapp/checkpoints.redb")?;
let workflow = Workflow::new(resources)
    .with_checkpoint_store(Arc::new(store))
    .with_workflow_id("nightly-crunch")
    .register_stepped(Stage::Crunch, Cruncher)   // cursor persisted after each step
    .register(Stage::Report, Reporter)
    .add_exit_state(Stage::Done);

// First run crashes after 600/1000 steps. Restart:
// let result = workflow.resume_from("nightly-crunch").await?;
// step() is first called with Some(Progress { processed: 600, .. }) — not None.
```
</div>

<!-- Section: persistence -->
<hr class="section-divider">
<h2 id="persistence"><a href="#persistence" class="anchor-link" aria-hidden="true">#</a>Cursor Persistence &amp; Resume</h2>
<p>
When a stepped task is registered via <code>register_stepped</code> <em>and</em> a
<a href="../recovery/"><code>CheckpointStore</code></a> plus a workflow id are attached
(<code>with_checkpoint_store</code> + <code>with_workflow_id</code>), the engine drives the step loop
and, after each <code>StepOutcome::More</code>, persists the cursor as a <code>CheckpointRow</code>
whose <code>kind</code> is <code>RowKind::StepCursor</code> (the cursor is
<a href="https://docs.rs/serde_json"><code>serde_json</code></a>-encoded). On
<code>Workflow::resume_from(workflow_id)</code>, the latest persisted cursor for that state is
rehydrated and passed to the first <code>step</code> call instead of <code>None</code> — so a crash
mid-loop resumes from where it left off.
</p>

<div class="callout callout-info">
<div class="callout-label">No <code>recovery</code> feature required</div>
<p>
Cursor persistence works <strong>whenever a checkpoint store is attached</strong> — it's just a
<code>CheckpointRow</code> with a <code>serde_json</code>-encoded blob, so it doesn't depend on the
<code>recovery</code> feature. Only the <em>built-in</em> <code>RedbCheckpointStore</code> lives
behind that feature gate; a custom <code>CheckpointStore</code> impl gets stepped-cursor persistence
for free. With no store attached at all, <code>register_stepped</code> degrades gracefully to a plain
in-memory loop.
</p>
</div>

<!-- Section: idempotency -->
<hr class="section-divider">
<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The Idempotency Contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Important</span>
<p><strong><code>step</code> must be idempotent with respect to observable effects.</strong> The
cursor is persisted <em>after</em> a step returns <code>More</code> — so when you resume, the engine
cannot tell whether that step's side effects already happened before the crash. The step at and after
the resume point may therefore re-run: make it safe to re-apply (upserts, dedupe keys, "if not
already processed" guards, conditional writes). Steps before the resume point are never re-run.</p>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit Trait-Impl Form</h2>
<p>
Prefer the trait header explicit — e.g. to declare a named <code>Cursor</code> type, or for a
generic impl? Put a bare <code>#[task::stepped]</code> on an
<code>impl SteppedTask&lt;...&gt; for ...</code> block and declare <code>type Cursor</code> yourself.
The companion <code>impl Task</code> is still emitted.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form — <code>#[task::stepped]</code> on a trait impl</span>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Crunch, Report, Done }

#[derive(Serialize, Deserialize, Debug, Clone)]
struct MyCursor { offset: u64 }

struct Cruncher;

#[task::stepped]
impl SteppedTask<Stage> for Cruncher {
    type Cursor = MyCursor;

    async fn step(&self, res: &Resources, cursor: Option<MyCursor>)
        -> Result<StepOutcome<MyCursor, Stage>, CanoError>
    {
        let c = cursor.unwrap_or(MyCursor { offset: 0 });
        let store = res.get::<MemoryStore, _>("store")?;
        let total: u64 = store.get("row_count")?;
        if c.offset >= total {
            return Ok(StepOutcome::Done(TaskResult::Single(Stage::Report)));
        }
        // ... process the page starting at c.offset (idempotently!) ...
        Ok(StepOutcome::More(MyCursor { offset: c.offset + 500 }))
    }
}
```
</div>

<!-- Section: object-safe aliases -->
<hr class="section-divider">
<h2 id="object-safe"><a href="#object-safe" class="anchor-link" aria-hidden="true">#</a>Type-Erased Aliases</h2>
<p>
As with <code>DynNode</code>, the object-safe aliases pin the associated type: <code>Cursor</code> to
<code>Vec&lt;u8&gt;</code> (the encoded form).
</p>
<table class="styled-table">
<thead>
<tr>
<th>Alias</th>
<th>Expands to</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>DynSteppedTask&lt;TState, TResourceKey&gt;</code></td>
<td><code>dyn SteppedTask&lt;TState, TResourceKey, Cursor = Vec&lt;u8&gt;&gt;</code></td>
</tr>
<tr>
<td><code>SteppedTaskObject&lt;TState, TResourceKey&gt;</code></td>
<td><code>Arc&lt;dyn SteppedTask&lt;TState, TResourceKey, Cursor = Vec&lt;u8&gt;&gt;&gt;</code></td>
</tr>
</tbody>
</table>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use SteppedTask</h2>
<p>Reach for a <code>SteppedTask</code> when you have long iterative work and want crash-resume
granularity finer than per-workflow-state:</p>
<ul>
<li><strong>large dataset scans</strong> — page-by-page with a continuation token;</li>
<li><strong>chunked uploads / migrations</strong> — an offset cursor that advances as you go;</li>
<li><strong>any long job</strong> where restarting from the top after a crash would waste hours of
already-done work.</li>
</ul>
<p>
If the work isn't iterative — or a single dispatch is short enough that re-running it on resume is
cheap — a plain <a href="../task/">Task</a> with <a href="../recovery/">checkpointing</a> at the
state level is simpler.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example &amp; crash test</div>
<p>
Run the example with <code>cargo run --example stepped_task --features recovery</code>; the
crash-recovery integration test lives at <code>tests/stepped_resume_e2e.rs</code> (it kills a
stepped run mid-loop and asserts <code>resume_from</code> picks up from the persisted cursor).
</p>
</div>
</div>
