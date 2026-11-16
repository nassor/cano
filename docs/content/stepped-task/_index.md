+++
title = "SteppedTask"
description = "SteppedTask in Cano - resumable iterative work that checkpoints a cursor so a crashed run resumes mid-loop."
template = "section.html"
+++

<div class="content-wrapper">
<h1>SteppedTask</h1>
<p class="subtitle">Resumable iterative work: checkpoint a cursor, resume mid-loop after a crash.</p>

<p>
A <code>SteppedTask</code> does work in discrete steps. Each step takes the previous step's cursor and
returns either "more work, here is the new cursor" or "done, here is the next state". When a
<a href="../recovery/">checkpoint store</a> is attached, the engine persists the cursor after every
step, so a crash mid-loop resumes from the last cursor instead of step zero. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>,
<a href="../timer-task/">TimerTask</a>, <a href="../batch-task/">BatchTask</a> and
<a href="../stream-task/">StreamTask</a>, and it reads typed dependencies from
<a href="../resources/">Resources</a>. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance: each <code>step</code> returns <code>More(cursor)</code> or <code>Done(state)</code></span>

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
            return Ok(StepOutcome::Done(Stage::Done));
        }
        // ...process page c.page...
        Ok(StepOutcome::More(Cursor { page: c.page + 1 }))   // cursor persisted here when a store is attached
    }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">Granularity</div>
<p>
A <code>SteppedTask</code> resumes at a finer grain than a workflow state. A long job restarts from its
last persisted cursor (a page number, an offset, a continuation token) instead of from the top. The
cost is idempotency: the step at and after the resume point may run again.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#how-it-works">How the step loop works</a></li>
<li><a href="#quick-start">Quick start with <code>#[task::stepped]</code></a></li>
<li><a href="#registering">Registering a stepped task</a></li>
<li><a href="#persistence">Cursor persistence and resume</a></li>
<li><a href="#idempotency">The idempotency contract</a></li>
<li><a href="#explicit">Explicit trait-impl form</a></li>
<li><a href="#when-to-use">When to use SteppedTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the step loop works</h2>

<div class="diagram-frame">
<p class="diagram-label">The step loop, and a crash resumed from the last cursor</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 748 226" role="img" aria-labelledby="stepped-loop-title stepped-loop-desc">
<title id="stepped-loop-title">The step loop and crash resume</title>
<desc id="stepped-loop-desc">The engine calls step with no cursor. Each step that returns More(cursor) has its cursor persisted to the checkpoint store, and the engine calls step again with that cursor. A step that returns Done(result) leaves the loop for the next state. After a crash, resume_from reads the last persisted cursor, c2, and re-enters the loop at step(Some c2).</desc>
<defs><marker id="stepped-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M68,90 V110 Q68,118 76,118 H120 Q128,118 128,126 V141" marker-end="url(#stepped-ah)"/>
<text class="t-code" x="108" y="104">More(c1)</text>
<path class="e" d="M188,146 V126 Q188,118 196,118 H237 Q245,118 245,110 V95" marker-end="url(#stepped-ah)"/>
<path class="e" d="M273,90 V110 Q273,118 281,118 H322 Q330,118 330,126 V141" marker-end="url(#stepped-ah)"/>
<text class="t-code" x="312" y="104">More(c2)</text>
<path class="e" d="M390,146 V126 Q390,118 398,118 H439 Q447,118 447,110 V95" marker-end="url(#stepped-ah)"/>
<path class="e" d="M520,67 H620" marker-end="url(#stepped-ah)"/>
<text class="t-code" x="572" y="50">Done(result)</text>
<path class="e e-dash" d="M444,168 H516" marker-end="url(#stepped-ah)"/>
<text class="t-mut t-err" x="480" y="154">crash</text>
<path class="e e-hot e-dash" d="M566,146 V120 Q566,112 558,112 H513 Q505,112 505,104 V95" marker-end="url(#stepped-ah)"/>
<text class="t-code t-hot ta-s" x="578" y="130">step(Some c2)</text>
<text class="t-mut" x="461" y="26">repeats until Done</text>
<rect class="n" x="20" y="44" width="96" height="46" rx="10"/>
<text class="t-code" x="68" y="68">step(None)</text>
<rect class="n" x="200" y="44" width="118" height="46" rx="10"/>
<text class="t-code" x="259" y="68">step(Some c1)</text>
<rect class="n" x="402" y="44" width="118" height="46" rx="10"/>
<text class="t-code" x="461" y="68">step(Some c2)</text>
<rect class="n-ok" x="624" y="44" width="104" height="46" rx="10"/>
<text x="676" y="68">Next state</text>
<rect class="n-cop" x="74" y="146" width="168" height="44" rx="10"/>
<text x="158" y="169">persist cursor c1</text>
<rect class="n-cop" x="276" y="146" width="168" height="44" rx="10"/>
<text x="360" y="169">persist cursor c2</text>
<rect class="n-hot" x="520" y="146" width="118" height="44" rx="10"/>
<text class="t-code t-strong" x="579" y="169">resume_from</text>
<text class="t-mut" x="259" y="206">written to the checkpoint store</text>
</svg>
</div>
</div>

<p>
A <code>SteppedTask</code> has one associated type, <code>type Cursor: serde::Serialize +
serde::de::DeserializeOwned + Clone + Send + Sync + 'static</code>, and one required method. The
<code>#[task::stepped(state = ...)]</code> macro infers <code>Cursor</code> from the <code>step</code>
signature, or you write it explicitly.
</p>

```rust
async fn step(
    &self,
    res: &Resources,
    cursor: Option<Self::Cursor>,
) -> Result<StepOutcome<Self::Cursor, TState>, CanoError>;
```

<ul>
<li><code>cursor</code> is <code>None</code> on the first call of a fresh run. On resume it is the
last persisted cursor.</li>
<li><code>StepOutcome&lt;TCursor, TState&gt;</code> is <code>More(TCursor)</code> or
<code>Done(TState)</code>. <code>More(c)</code> passes <code>c</code> to the next call;
<code>Done(state)</code> ends the loop and moves the workflow to <code>state</code>.</li>
</ul>

<p>
<code>fn config(&amp;self) -&gt; TaskConfig</code> defaults to <code>TaskConfig::new()</code>: one
attempt, no retries. The engine applies it to each <code>step</code> call, so
<code>TaskConfig::new().with_exponential_retry(3)</code> retries a failing step with the same cursor.
<code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code> defaults to the type name.
</p>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start with <code>#[task::stepped]</code></h2>
<p>
Put <code>#[task::stepped(state = MyState)]</code> on an inherent <code>impl</code> block. The macro
infers <code>Cursor</code> from the <code>Option&lt;_&gt;</code> and <code>StepOutcome&lt;_, _&gt;</code>
types and writes the <code>impl SteppedTask&lt;MyState&gt; for Cruncher</code> header. It also emits a
companion <code>impl Task&lt;MyState&gt; for Cruncher</code> whose <code>run</code> executes the loop
in memory, with no cursor rows, so a stepped task can be a <code>register_split</code> branch.
<code>step</code> may omit the <code>res</code> parameter when it does not read resources.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128260;</span> Inference form: <code>#[task::stepped(state = ...)]</code> on an inherent impl</span>

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
            return Ok(StepOutcome::Done(Stage::Report));
        }
        // ... do a chunk of work for batch p.processed ...
        Ok(StepOutcome::More(Progress { processed: p.processed + 1, ..p }))
    }
}
```
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a stepped task</h2>
<p>
Register a stepped task with <code>Workflow::register(state, task)</code>, the same call every task
model uses. The engine drives the step loop itself: each <code>step</code> runs under the task's
<code>config()</code>, and with a checkpoint store and a workflow id attached the cursor is persisted
after every step.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a stepped task into a checkpointed workflow</span>

```rust
use cano::prelude::*;
use cano::RedbCheckpointStore;            // requires the `recovery` feature
use std::sync::Arc;

let checkpoint_store = RedbCheckpointStore::new("/var/lib/myapp/checkpoints.redb")?;
let workflow = Workflow::new(resources)
    .with_checkpoint_store(Arc::new(checkpoint_store))
    .with_workflow_id("nightly-crunch")
    .register(Stage::Crunch, Cruncher)           // cursor persisted after each step
    .register(Stage::Report, Reporter)
    .add_exit_state(Stage::Done);

// The first run crashes after 600 of 1000 steps. After a restart:
// let result = workflow.resume_from("nightly-crunch").await?;
// step() is first called with Some(Progress { processed: 600, .. }), not None.
```
</div>

<!-- Section: persistence -->
<hr class="section-divider">
<h2 id="persistence"><a href="#persistence" class="anchor-link" aria-hidden="true">#</a>Cursor persistence and resume</h2>
<p>
With a <a href="../recovery/"><code>CheckpointStore</code></a> and a workflow id attached
(<code>with_checkpoint_store</code> and <code>with_workflow_id</code>), the engine persists the cursor
after each <code>StepOutcome::More</code> as a <code>CheckpointRow</code> whose <code>kind</code> is
<code>RowKind::StepCursor</code>, encoded with
<a href="https://docs.rs/serde_json"><code>serde_json</code></a>. <code>Workflow::resume_from(workflow_id)</code>
passes the latest persisted cursor for the state to the first <code>step</code> call instead of
<code>None</code>. A cursor is used once: if the workflow visits the same state again, that visit
starts from <code>None</code>.
</p>

<div class="callout callout-info">
<div class="callout-label">Works without the <code>recovery</code> feature</div>
<p>
Any <code>CheckpointStore</code> receives cursor rows. Only the built-in <code>RedbCheckpointStore</code>
needs the <code>recovery</code> feature. With no store or no workflow id attached, the engine still
drives the step loop and writes no cursor rows.
</p>
</div>

<!-- Section: idempotency -->
<hr class="section-divider">
<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The idempotency contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Make <code>step</code> idempotent</span>
<p>The engine persists the cursor after a step returns <code>More</code>. On resume it cannot tell
whether the side effects of the last step ran before the crash, so the step at and after the resume
point may run again. Make it safe to re-apply: upserts, dedupe keys, "if not already processed"
guards, conditional writes. Steps before the resume point do not run again.</p>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit trait-impl form</h2>
<p>
To name the <code>Cursor</code> type or write a generic impl, put a bare <code>#[task::stepped]</code>
on an <code>impl SteppedTask&lt;...&gt; for ...</code> block and declare <code>type Cursor</code>
yourself. The companion <code>impl Task</code> is still emitted.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form: <code>#[task::stepped]</code> on a trait impl</span>

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
        let store = res.get::<MemoryStore>()?;
        let total: u64 = store.get("row_count")?;
        if c.offset >= total {
            return Ok(StepOutcome::Done(Stage::Report));
        }
        // ... process the page starting at c.offset (idempotently!) ...
        Ok(StepOutcome::More(MyCursor { offset: c.offset + 500 }))
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to use SteppedTask</h2>
<p>Use a <code>SteppedTask</code> for long iterative work where restarting from the top after a crash
would waste hours: scanning a large dataset page by page with a continuation token, or advancing an
offset through a chunked upload or migration.</p>
<p>
If the work is not iterative, or one dispatch is short enough that re-running it on resume is cheap,
a plain <a href="../task/">Task</a> with <a href="../recovery/">state-level checkpointing</a> is
simpler.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example and crash test</div>
<p>
Run the example with <code>cargo run --example stepped_task --features recovery</code>. The
crash-recovery integration test is <code>tests/stepped_resume_e2e.rs</code>: it kills a stepped run
mid-loop and asserts that <code>resume_from</code> picks up from the persisted cursor.
</p>
</div>
</div>
