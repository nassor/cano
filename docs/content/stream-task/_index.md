+++
title = "StreamTask"
description = "StreamTask in Cano - consume an unbounded stream continuously, flush per tumbling window, run until cancelled or exhausted, and resume from a persisted cursor."
template = "section.html"
+++

<div class="content-wrapper">
<h1>StreamTask</h1>
<p class="subtitle">Consume an unbounded stream — flush per window, run until cancelled or exhausted, resume from a cursor.</p>

<p>
A <code>StreamTask</code> consumes an <code>impl Stream</code> <strong>continuously</strong>: it pulls
one item at a time, processes each into an output, and flushes per tumbling
<a href="#windowing">window</a> — so memory stays bounded and downstream sees progress before the
source ends. It runs until the stream is exhausted, until a window asks to stop, or until the run is
<a href="#cancellation">cancelled</a>; each flushed window commits a <a href="#cursor">cursor</a> so a
crashed or cancelled run resumes where it left off. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>,
<a href="../timer-task/">TimerTask</a>, <a href="../batch-task/">BatchTask</a>, and
<a href="../stepped-task/">SteppedTask</a>, and it reads typed dependencies from
<a href="../resources/">Resources</a> like the rest. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first; for the
cursor-persistence half, <a href="../recovery/">Recovery</a>.
</p>

<div class="code-block">
<span class="code-block-label">At a glance — <code>open</code> a stream, <code>process_item</code> per item, <code>flush_window</code> per window</span>

```rust
use cano::prelude::*;
use futures_util::{Stream, stream};
use std::pin::Pin;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Consume, Done }

struct ConsumeEvents;

#[task::stream(state = Step)]
impl ConsumeEvents {
    fn window(&self) -> StreamWindow {
        StreamWindow::Count(3) // flush every 3 processed items
    }

    async fn open(&self, _res: &Resources, cursor: Option<u64>)
        -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError>
    {
        // On a fresh run `cursor` is `None`; on resume it's the last committed offset.
        let start = cursor.map(|c| c + 1).unwrap_or(0);
        Ok(Box::pin(stream::iter(start..10)) as Pin<Box<dyn Stream<Item = u64> + Send>>)
    }

    async fn process_item(&self, _res: &Resources, item: u64)
        -> Result<(u64, u64), CanoError>
    {
        Ok((item * 2, item)) // (output, cursor reached by consuming this item)
    }

    async fn flush_window(&self, _res: &Resources, outputs: Vec<u64>)
        -> Result<WindowSignal<Step>, CanoError>
    {
        // Commit side effects / offsets for this window here.
        println!("flushed window of {} outputs", outputs.len());
        Ok(WindowSignal::Continue)
    }

    async fn on_close(&self, _res: &Resources, reason: CloseReason)
        -> Result<TaskResult<Step>, CanoError>
    {
        println!("stream ended ({reason:?})");
        Ok(TaskResult::Single(Step::Done))
    }
}

let workflow = Workflow::bare()
    .register_stream(Step::Consume, ConsumeEvents)
    .add_exit_state(Step::Done);
```
</div>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
A <code>StreamTask</code> is for sources that <em>don't end on their own</em> — Kafka, SSE, a
file-tail, a WebSocket. Instead of buffering everything and aggregating once, it emits
<strong>per window</strong> and keeps memory bounded, runs until you stop it, and resumes from a
persisted cursor after a crash. If your data is a <em>bounded</em> <code>Vec</code> you want to map
over and aggregate once, reach for a <a href="../batch-task/">BatchTask</a> instead.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#batch-vs-stream">BatchTask vs StreamTask</a></li>
<li><a href="#how-it-works">How the Stream Loop Works</a></li>
<li><a href="#quick-start">Quick Start with <code>#[task::stream]</code></a></li>
<li><a href="#windowing">Windowing: <code>StreamWindow</code></a></li>
<li><a href="#error-policy">Per-Item Error Policy</a></li>
<li><a href="#cursor">Cursor Persistence &amp; Resume</a></li>
<li><a href="#cancellation">The Cancellation Contract</a></li>
<li><a href="#idempotency">The Idempotency Contract</a></li>
<li><a href="#register">register vs register_stream</a></li>
<li><a href="#explicit">Explicit Trait-Impl Form</a></li>
<li><a href="#when-to-use">When to Use StreamTask</a></li>
</ol>
</nav>

<!-- Section: Batch vs Stream -->
<hr class="section-divider">
<h2 id="batch-vs-stream"><a href="#batch-vs-stream" class="anchor-link" aria-hidden="true">#</a>BatchTask vs StreamTask</h2>
<p>
The two look similar — both fan a sub-operation over many items — but they solve opposite problems. A
<a href="../batch-task/">BatchTask</a> loads a <strong>bounded</strong> <code>Vec</code>, processes
all of it, and aggregates <strong>once</strong> at the end: O(N) memory, one emission, and it
<em>requires the data to end</em>. A <code>StreamTask</code> is for <strong>unbounded</strong> /
continuous sources: it emits incrementally per window, keeps memory bounded, runs until
cancelled/exhausted, and resumes from a cursor.
</p>

<table class="styled-table">
<thead>
<tr>
<th></th>
<th><a href="../batch-task/"><code>BatchTask</code></a></th>
<th><code>StreamTask</code></th>
</tr>
</thead>
<tbody>
<tr>
<td><strong>Data</strong></td>
<td>bounded — a <code>Vec</code> loaded up front</td>
<td>unbounded — a continuous <code>impl Stream</code></td>
</tr>
<tr>
<td><strong>Termination</strong></td>
<td>when the loaded items are exhausted</td>
<td>exhausted, a window <code>Stop</code>, or cancelled</td>
</tr>
<tr>
<td><strong>Emission</strong></td>
<td>one aggregate, in <code>finish</code></td>
<td>per-window, in <code>flush_window</code></td>
</tr>
<tr>
<td><strong>Memory</strong></td>
<td>O(N) — the whole batch is held</td>
<td>bounded — one window at a time</td>
</tr>
<tr>
<td><strong>Recovery</strong></td>
<td>re-run the whole state on resume</td>
<td>resume from the last committed cursor</td>
</tr>
</tbody>
</table>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the Stream Loop Works</h2>

<div class="diagram-frame">
<p class="diagram-label">open &rarr; process_item (×window) &rarr; flush_window &rarr; commit cursor &rarr; … &rarr; on_close</p>
<div class="mermaid">
graph LR
A["open(cursor)"] --> B[process_item]
B -->|"buffer until window full"| B
B -->|"window full"| F[flush_window]
F -->|"Continue"| P[commit cursor]
P --> B
F -->|"Stop(result)"| D[Next State]
B -->|"source exhausted"| C[on_close]
C --> D
</div>
</div>

<p>
A <code>StreamTask</code> has three associated types — <code>type Item</code> (one element pulled from
the source), <code>type Output</code> (the per-item result accumulated into a window), and
<code>type Cursor</code> (the resumable position; <code>Serialize + DeserializeOwned + Send + Sync +
'static</code>) — and four required methods:
</p>

<table class="styled-table">
<thead>
<tr>
<th>Method</th>
<th>Role</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>open(res, cursor)</code></td>
<td>Open (or resume) the source stream. <code>cursor</code> is <code>None</code> on a fresh run, or the last committed position on resume.</td>
</tr>
<tr>
<td><code>process_item(res, item)</code></td>
<td>Process one item; return <code>(Output, Cursor)</code> — the cursor is the position reached by consuming this item.</td>
</tr>
<tr>
<td><code>flush_window(res, outputs)</code></td>
<td>Flush one full window: commit side effects, then return <code>WindowSignal::Continue</code> or <code>WindowSignal::Stop(result)</code>. The window's cursor is committed after this returns.</td>
</tr>
<tr>
<td><code>on_close(res, reason)</code></td>
<td>Terminal transition when the stream is <code>Exhausted</code> or the run is <code>Cancelled</code>. The in-flight partial window has already been flushed.</td>
</tr>
</tbody>
</table>

<p>
Optional methods carry defaults: <code>window()</code> (defaults to
<code>StreamWindow::Count(1)</code> — flush per item), <code>on_item_error()</code> (defaults to
<code>StreamErrorPolicy::FailFast</code>), <code>config()</code> (defaults to
<code>TaskConfig::minimal()</code> — <strong>no outer retry</strong>, because an outer retry would
re-invoke <code>open()</code> and re-consume the stream), and <code>name()</code> (defaults to the
type name).
</p>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start with <code>#[task::stream]</code></h2>
<p>
Attach <code>#[task::stream(state = MyState)]</code> to an inherent <code>impl</code> block. The macro
infers <code>Item</code> from <code>process_item</code>'s owned <code>item</code> parameter and
<code>Output</code> / <code>Cursor</code> from the <code>Ok</code> tuple of its return type, injects
default <code>window</code> / <code>on_item_error</code> / <code>config</code> / <code>name</code> if
absent, synthesises the <code>impl StreamTask&lt;MyState&gt; for ConsumeEvents</code> header, and emits
a companion <code>impl Task&lt;MyState&gt; for ConsumeEvents</code> whose <code>run</code> drives the
in-memory loop — useful if you register it with plain <code>register</code> and don't want
persistence.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form — <code>#[task::stream(state = ...)]</code> on an inherent impl</span>

```rust
use cano::prelude::*;
use futures_util::{Stream, stream};
use std::pin::Pin;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Consume, Done }

#[derive(Debug, Clone)]
struct Event { offset: u64, payload: String }

#[derive(Debug)]
struct Processed { offset: u64, bytes: usize }

struct ConsumeEvents;

#[task::stream(state = Step)]
impl ConsumeEvents {
    fn window(&self) -> StreamWindow {
        StreamWindow::Count(3)
    }

    async fn open(&self, _res: &Resources, cursor: Option<u64>)
        -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>, CanoError>
    {
        let start = cursor.map(|c| c + 1).unwrap_or(0);
        let events: Vec<Event> = (start..10)
            .map(|offset| Event { offset, payload: format!("payload-{offset}") })
            .collect();
        Ok(Box::pin(stream::iter(events)) as Pin<Box<dyn Stream<Item = Event> + Send>>)
    }

    async fn process_item(&self, _res: &Resources, item: Event)
        -> Result<(Processed, u64), CanoError>
    {
        Ok((
            Processed { offset: item.offset, bytes: item.payload.len() },
            item.offset, // the cursor reached by consuming this item
        ))
    }

    async fn flush_window(&self, _res: &Resources, outputs: Vec<Processed>)
        -> Result<WindowSignal<Step>, CanoError>
    {
        let bytes: usize = outputs.iter().map(|p| p.bytes).sum();
        println!("flushed {} events ({bytes} bytes)", outputs.len());
        Ok(WindowSignal::Continue)
    }

    async fn on_close(&self, _res: &Resources, reason: CloseReason)
        -> Result<TaskResult<Step>, CanoError>
    {
        println!("stream ended ({reason:?})");
        Ok(TaskResult::Single(Step::Done))
    }
}
```
</div>

<!-- Section: Windowing -->
<hr class="section-divider">
<h2 id="windowing"><a href="#windowing" class="anchor-link" aria-hidden="true">#</a>Windowing: <code>StreamWindow</code></h2>
<p>
The <code>window()</code> method returns a <strong>tumbling-window</strong> trigger that controls how
often <code>flush_window</code> fires and how much the driver buffers. Larger windows amortise the
flush + checkpoint cost.
</p>

<div class="card-stack">
<div class="card">
<h3><code>StreamWindow::Count(n)</code></h3>
<p>Flush after every <code>n</code> successfully processed items (clamped to a minimum of 1). The
default is <code>Count(1)</code> — flush per item.</p>
</div>
<div class="card">
<h3><code>StreamWindow::Duration(d)</code></h3>
<p>Flush every <code>d</code> of wall-clock time, tumbling. <strong>Empty windows are skipped</strong>
— an idle source emits no spurious empty flushes; the deadline simply advances.</p>
</div>
</div>

<!-- Section: Error policy -->
<hr class="section-divider">
<h2 id="error-policy"><a href="#error-policy" class="anchor-link" aria-hidden="true">#</a>Per-Item Error Policy</h2>
<p>
<code>on_item_error()</code> returns a <code>StreamErrorPolicy</code> deciding what the windowed loop
does when <code>process_item</code> returns an <code>Err</code>:
</p>

<table class="styled-table">
<thead>
<tr>
<th><code>StreamErrorPolicy</code> variant</th>
<th>Effect</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>FailFast</code> <em>(default)</em></td>
<td>Propagate the first item error — the loop stops and the run fails.</td>
</tr>
<tr>
<td><code>SkipAndContinue</code></td>
<td>Drop the bad item and keep consuming (poison-message handling). The skipped item's cursor is not committed — the next good item advances it.</td>
</tr>
<tr>
<td><code>RetryOnError { max_errors }</code></td>
<td>Tolerate up to <code>max_errors</code> <strong>consecutive</strong> item errors before failing. The counter resets on every successfully processed item.</td>
</tr>
</tbody>
</table>

<!-- Section: Cursor -->
<hr class="section-divider">
<h2 id="cursor"><a href="#cursor" class="anchor-link" aria-hidden="true">#</a>Cursor Persistence &amp; Resume</h2>
<p>
Register a stream with <code>Workflow::register_stream(state, task)</code> — the durable, cancellable
engine path. When a <a href="../recovery/"><code>CheckpointStore</code></a> plus a workflow id are
attached (<code>with_checkpoint_store</code> + <code>with_workflow_id</code>), the engine persists the
cursor returned by the <strong>last item of each flushed window</strong> as a
<code>CheckpointRow</code> whose <code>kind</code> is <code>RowKind::StepCursor</code> (the cursor is
<a href="https://docs.rs/serde_json"><code>serde_json</code></a>-encoded). On
<code>Workflow::resume_from(workflow_id, token)</code>, the latest persisted cursor is rehydrated and
passed to <code>open(Some(cursor))</code> — so a crashed or cancelled run re-opens the source from the
last committed position instead of starting over. Only <strong>fully flushed</strong> windows commit
a cursor; a window that errors mid-flush commits nothing.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a durable, resumable stream into a checkpointed workflow</span>

```rust
use cano::prelude::*;
use cano::RedbCheckpointStore;            // requires the `recovery` feature
use std::sync::Arc;

let checkpoint_store = RedbCheckpointStore::new("/var/lib/myapp/checkpoints.redb")?;
let workflow = Workflow::new(resources)
    .with_checkpoint_store(Arc::new(checkpoint_store))
    .with_workflow_id("event-consumer")
    .register_stream(Step::Consume, ConsumeEvents)   // cursor persisted per flushed window
    .add_exit_state(Step::Done);

// A crashed or cancelled run resumes from the last committed window:
// let result = workflow.resume_from("event-consumer", token).await?;
// open() is called with Some(cursor) — the position of the last fully-flushed window.
```
</div>

<!-- Section: Cancellation -->
<hr class="section-divider">
<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>The Cancellation Contract</h2>
<p>
Because an unbounded stream never ends on its own, cancellation is how you stop it cleanly. When the
run's <code>CancellationToken</code> fires, the engine-driven driver performs a <strong>cooperative
drain</strong>:
</p>
<ol>
<li>it flushes the in-flight (partial) window via <code>flush_window</code>;</li>
<li>it commits that window's cursor;</li>
<li>it calls <code>on_close(CloseReason::Cancelled)</code> for cleanup — its returned state is
<strong>ignored</strong>;</li>
<li>the run ends as <code>CanoError::Cancelled</code> (category <code>"cancelled"</code>).</li>
</ol>

<div class="callout callout-warning">
<span class="callout-label">Cancel means "stop cleanly + resumable", not "transition onward"</span>
<p>
A cancelled stream does <strong>not</strong> gracefully transition to another state — it surfaces as
<code>CanoError::Cancelled</code>. But because the committed cursor survives, a later
<code>resume_from</code> continues from the last committed window. So cancellation is a clean,
resumable stop, not a hand-off. (An <code>Err</code> returned by <code>on_close</code> during the
drain <em>is</em> propagated.)
</p>
</div>

<!-- Section: idempotency -->
<hr class="section-divider">
<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The Idempotency Contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Important — at-least-once</span>
<p><strong><code>open</code> and <code>process_item</code> must be idempotent.</strong> The FSM writes
the state-entry checkpoint <em>before</em> running the task, so a resumed run re-enters the state and
calls <code>open(Some(cursor))</code> from the last committed cursor. The window <em>after</em> that
cursor may have been partially processed and then replayed — make <code>process_item</code> safe to
re-apply (upserts, dedupe keys, "if not already processed" guards, conditional writes). This is also
why <code>config()</code> defaults to <code>TaskConfig::minimal()</code> (no outer retry): an outer
retry would re-invoke <code>open()</code> and re-consume the stream.</p>
</div>

<div class="callout callout-info">
<div class="callout-label">Limitation — the cursor log only clears at an exit state</div>
<p>
The checkpoint log is append-only and is only cleared when the run reaches an exit state, so a
<em>never-terminating</em> stream accumulates one <code>StepCursor</code> row per flushed window
indefinitely. For bounded streams, or runs that reach an exit state via <code>on_close</code> /
<code>WindowSignal::Stop</code>, this is fine. For genuinely endless sources, prefer bounded windows
plus periodic restarts (which clear the log on the clean exit) until a log-compaction step lands.
</p>
</div>

<!-- Section: register vs register_stream -->
<hr class="section-divider">
<h2 id="register"><a href="#register" class="anchor-link" aria-hidden="true">#</a><code>register</code> vs <code>register_stream</code></h2>
<p>
A <code>StreamTask</code> can be registered two ways, and the difference is durability:
</p>

<table class="styled-table">
<thead>
<tr>
<th>Registration</th>
<th>Cursor persistence</th>
<th>Cancellation</th>
<th>Use for</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>register_stream</code></td>
<td>yes (with a checkpoint store + id)</td>
<td>yes — cooperative drain</td>
<td>real, durable stream consumption</td>
</tr>
<tr>
<td><code>register</code></td>
<td>no</td>
<td>no</td>
<td>convenience / tests only</td>
</tr>
</tbody>
</table>

<p>
Plain <code>register</code> runs the macro-generated companion <code>Task</code>, which drives an
<strong>in-memory</strong> windowed loop with <strong>no</strong> cursor persistence and
<strong>no</strong> cancellation — it always runs to exhaustion. Reach for <code>register_stream</code>
for anything real; it is the path that observes the <code>CancellationToken</code> and persists
cursors.
</p>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit Trait-Impl Form</h2>
<p>
Prefer writing the trait header yourself — e.g. to name the associated types, or for a generic impl?
Put a bare <code>#[task::stream]</code> on an <code>impl StreamTask&lt;...&gt; for ...</code> block and
declare the three <code>type</code> lines (<code>Item</code> / <code>Output</code> / <code>Cursor</code>)
yourself. The companion <code>impl Task</code> is still emitted.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form — <code>#[task::stream]</code> on a trait impl</span>

```rust
use cano::prelude::*;
use futures_util::{Stream, stream};
use std::pin::Pin;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Consume, Done }

struct Collector;

#[task::stream]
impl StreamTask<Step> for Collector {
    type Item = u32;
    type Output = u32;
    type Cursor = u64;

    fn window(&self) -> StreamWindow {
        StreamWindow::Count(2)
    }

    async fn open(&self, _res: &Resources, _cursor: Option<u64>)
        -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError>
    {
        Ok(Box::pin(stream::iter(vec![10u32, 20, 30, 40, 50]))
            as Pin<Box<dyn Stream<Item = u32> + Send>>)
    }

    async fn process_item(&self, _res: &Resources, item: u32)
        -> Result<(u32, u64), CanoError>
    {
        Ok((item * 2, item as u64))
    }

    async fn flush_window(&self, _res: &Resources, _outputs: Vec<u32>)
        -> Result<WindowSignal<Step>, CanoError>
    {
        Ok(WindowSignal::Continue)
    }

    async fn on_close(&self, _res: &Resources, _reason: CloseReason)
        -> Result<TaskResult<Step>, CanoError>
    {
        Ok(TaskResult::Single(Step::Done))
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use StreamTask</h2>
<p>Reach for a <code>StreamTask</code> when:</p>
<ul>
<li>your source is <strong>unbounded or continuous</strong> — a Kafka topic, an SSE feed, a tailed
file, a WebSocket — and never produces a final <code>Vec</code> to aggregate;</li>
<li>you want <strong>incremental, per-window emission</strong> with bounded memory rather than one
end-of-batch aggregate;</li>
<li>you need the consumer to <strong>resume from a committed offset</strong> after a crash or a
cancellation, not start over.</li>
</ul>
<p>
If your data is a <em>bounded</em> collection you want to map over and aggregate once, a
<a href="../batch-task/">BatchTask</a> is simpler. If you have a long iterative job over a finite
range that you want to crash-resume mid-loop, a <a href="../stepped-task/">SteppedTask</a> fits
better.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
The crate ships a complete example — run it with <code>cargo run --example stream_task</code>.
</p>
</div>
</div>
