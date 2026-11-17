+++
title = "StreamTask"
description = "StreamTask in Cano - consume an unbounded stream continuously, flush per tumbling window, run until cancelled or exhausted, and resume from a persisted cursor."
template = "section.html"
+++

<div class="content-wrapper">
<h1>StreamTask</h1>
<p class="subtitle">Consume an unbounded stream: flush per window, run until cancelled or exhausted, resume from a cursor.</p>

<p>
A <code>StreamTask</code> consumes an <code>impl Stream</code> continuously. It pulls one item at a
time, processes each into an output, and flushes per tumbling <a href="#windowing">window</a>, so
memory stays bounded and downstream sees progress before the source ends. It runs until the stream is
exhausted, a window asks to stop, or the run is <a href="#cancellation">cancelled</a>. Each flushed
window commits a <a href="#cursor">cursor</a>, so a crashed or cancelled run resumes where it left
off. It is one of the <a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>,
<a href="../timer-task/">TimerTask</a>, <a href="../batch-task/">BatchTask</a>,
<a href="../stepped-task/">SteppedTask</a> and <a href="../suspend-task/">SuspendTask</a>, and it reads typed dependencies from
<a href="../resources/">Resources</a>. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance: <code>open</code> a stream, <code>process_item</code> per item, <code>flush_window</code> per window</span>

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
        // On a fresh run `cursor` is `None`; on resume it is the last committed offset.
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
        // Commit side effects for this window here.
        println!("flushed window of {} outputs", outputs.len());
        Ok(WindowSignal::Continue)
    }

    async fn on_close(&self, _res: &Resources, reason: CloseReason)
        -> Result<Step, CanoError>
    {
        println!("stream ended ({reason:?})");
        Ok(Step::Done)
    }
}

let workflow = Workflow::bare()
    .register(Step::Consume, ConsumeEvents)
    .add_exit_state(Step::Done);
```
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#batch-vs-stream">BatchTask vs StreamTask</a></li>
<li><a href="#how-it-works">How the stream loop works</a></li>
<li><a href="#quick-start">Quick start with <code>#[task::stream]</code></a></li>
<li><a href="#windowing">Windowing: <code>StreamWindow</code></a></li>
<li><a href="#error-policy">Per-item error policy</a></li>
<li><a href="#cursor">Cursor persistence and resume</a></li>
<li><a href="#cancellation">The cancellation contract</a></li>
<li><a href="#idempotency">The idempotency contract</a></li>
<li><a href="#register">Registering a stream task</a></li>
<li><a href="#explicit">Explicit trait-impl form</a></li>
<li><a href="#when-to-use">When to use StreamTask</a></li>
</ol>
</nav>

<!-- Section: Batch vs Stream -->
<hr class="section-divider">
<h2 id="batch-vs-stream"><a href="#batch-vs-stream" class="anchor-link" aria-hidden="true">#</a>BatchTask vs StreamTask</h2>
<p>
Both fan an operation over many items. A <a href="../batch-task/">BatchTask</a> loads a bounded
<code>Vec</code>, processes all of it, and aggregates once at the end. A <code>StreamTask</code>
consumes a source that does not end on its own (Kafka, SSE, a tailed file, a WebSocket) and emits per
window.
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
<td>Data</td>
<td>bounded <code>Vec</code>, loaded up front</td>
<td>unbounded <code>impl Stream</code></td>
</tr>
<tr>
<td>Ends when</td>
<td>the items are exhausted</td>
<td>exhausted, a window <code>Stop</code>, or cancelled</td>
</tr>
<tr>
<td>Emits</td>
<td>one aggregate, in <code>finish</code></td>
<td>one flush per window, in <code>flush_window</code></td>
</tr>
<tr>
<td>Memory</td>
<td>the whole batch</td>
<td>one window at a time</td>
</tr>
<tr>
<td>After a crash</td>
<td>re-runs the whole state</td>
<td>resumes from the last committed cursor</td>
</tr>
</tbody>
</table>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the stream loop works</h2>

<div class="diagram-frame">
<p class="diagram-label">open, process_item per item, flush_window per window, commit the cursor, repeat until on_close</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 352" role="img" aria-labelledby="stream-loop-title stream-loop-desc">
<title id="stream-loop-title">The stream loop</title>
<desc id="stream-loop-desc">open resumes the source from a cursor once per run. process_item runs per item and buffers outputs until the window is full. flush_window then emits the window and the engine commits its cursor. On Continue the loop returns to process_item for the next window. On Stop(state) the run moves to the next state. When the source is exhausted or the run is cancelled, the partial window is flushed and on_close runs: Exhausted returns Ok(state) for the next state, and Cancelled ends the run with Err(Cancelled). Each exit also commits the last cursor.</desc>
<defs><marker id="stream-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M292,64 V44 Q292,36 300,36 H362 Q370,36 370,44 V61" marker-end="url(#stream-ah)"/>
<text class="t-mut" x="331" y="18">buffer until window full</text>
<path class="e" d="M158,87 H248" marker-end="url(#stream-ah)"/>
<text class="t-mut" x="203" y="70">once per run</text>
<path class="e e-hot" d="M410,87 H526" marker-end="url(#stream-ah)"/>
<text class="t-code t-hot" x="468" y="70">window full</text>
<path class="e" d="M609,110 V186" marker-end="url(#stream-ah)"/>
<text class="t-mut ta-e" x="598" y="154">flushed</text>
<path class="e" d="M530,213 H352 Q340,213 340,201 V114" marker-end="url(#stream-ah)"/>
<text class="t-code" x="440" y="196">Continue: next window</text>
<path class="e" d="M609,236 V272" marker-end="url(#stream-ah)"/>
<text class="t-code ta-e" x="598" y="258">Stop(state)</text>
<path class="e" d="M292,110 V272" marker-end="url(#stream-ah)"/>
<text class="t-mut ta-e" x="282" y="160">source ends or run cancelled</text>
<path class="e" d="M372,299 H526" marker-end="url(#stream-ah)"/>
<text class="t-code" x="449" y="282">Ok(state)</text>
<path class="e e-dim" d="M212,299 H140" marker-end="url(#stream-ah)"/>
<text class="t-mut" x="176" y="282">Cancelled</text>
<rect class="n" x="16" y="64" width="142" height="46" rx="10"/>
<text class="t-code" x="87" y="88">open(cursor)</text>
<rect class="n" x="252" y="64" width="158" height="46" rx="10"/>
<text class="t-code" x="331" y="88">process_item</text>
<rect class="n-hot" x="530" y="64" width="158" height="46" rx="10"/>
<text class="t-code t-strong" x="609" y="88">flush_window</text>
<rect class="n-cop" x="530" y="190" width="158" height="46" rx="10"/>
<text x="609" y="209">commit cursor</text>
<text class="t-mut" x="609" y="227">StepCursor row</text>
<rect class="n" x="212" y="276" width="160" height="46" rx="10"/>
<text class="t-code" x="292" y="300">on_close(reason)</text>
<rect class="n-ok" x="530" y="276" width="158" height="46" rx="10"/>
<text x="609" y="300">Next state</text>
<rect class="n-err" x="16" y="276" width="124" height="46" rx="10"/>
<text class="t-code" x="78" y="300">Err(Cancelled)</text>
<text class="t-mut" x="390" y="344">Every exit commits the last cursor when a store and workflow id are attached.</text>
</svg>
</div>
</div>

<p>
A <code>StreamTask</code> has three associated types: <code>type Item</code> (one element pulled from
the source), <code>type Output</code> (the per-item result collected into a window) and
<code>type Cursor</code> (the resumable position, <code>Serialize + DeserializeOwned + Send + Sync +
'static</code>). Four methods are required:
</p>

<dl class="key-list">
<dt><code>open(res, cursor: Option&lt;Cursor&gt;) -&gt; Pin&lt;Box&lt;dyn Stream&gt;&gt;</code></dt>
<dd>Opens or resumes the source. <code>cursor</code> is <code>None</code> on a fresh run and the last committed position on resume.</dd>
<dt><code>process_item(res, item: Item) -&gt; (Output, Cursor)</code></dt>
<dd>Processes one item. The returned cursor is the position reached by consuming it.</dd>
<dt><code>flush_window(res, outputs: Vec&lt;Output&gt;) -&gt; WindowSignal&lt;TState&gt;</code></dt>
<dd>Flushes one window: commit side effects, then return <code>WindowSignal::Continue</code> or <code>WindowSignal::Stop(state)</code>. The engine commits the window's cursor after this returns.</dd>
<dt><code>on_close(res, reason: CloseReason) -&gt; TState</code></dt>
<dd>Runs after the partial window is flushed, with <code>CloseReason::Exhausted</code> or <code>CloseReason::Cancelled</code>. For <code>Exhausted</code> the returned state is the next state. It does not run when the loop ends in an item error or a failed flush, except on cancel.</dd>
</dl>

<p>Four methods are optional, each shown with its default:</p>

<dl class="key-list">
<dt><code>fn window(&amp;self) -&gt; StreamWindow</code> <small>default <code>StreamWindow::Count(1)</code>, flush per item</small></dt>
<dd>The tumbling-window trigger.</dd>
<dt><code>fn on_item_error(&amp;self) -&gt; StreamErrorPolicy</code> <small>default <code>StreamErrorPolicy::FailFast</code></small></dt>
<dd>What the loop does when <code>process_item</code> returns <code>Err</code>.</dd>
<dt><code>fn config(&amp;self) -&gt; TaskConfig</code> <small>default <code>TaskConfig::new()</code></small></dt>
<dd>Only <code>attempt_timeout</code> applies, as a bound on each <code>process_item</code> call. A timeout is an item error handled by <code>on_item_error</code>. It also bounds cancel latency when <code>process_item</code> can hang, because the loop observes cancellation between items. There is no outer retry: it would call <code>open()</code> again and re-consume the stream.</dd>
<dt><code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code> <small>default: the type name</small></dt>
<dd>Identifies the task in logs and observers.</dd>
</dl>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start with <code>#[task::stream]</code></h2>
<p>
Put <code>#[task::stream(state = MyState)]</code> on an inherent <code>impl</code> block. The macro
infers <code>Item</code> from <code>process_item</code>'s owned <code>item</code> parameter and
<code>Output</code> and <code>Cursor</code> from the <code>Ok</code> tuple of its return type, and
writes the <code>impl StreamTask&lt;MyState&gt; for ConsumeEvents</code> header. It also emits a
companion <code>impl Task&lt;MyState&gt; for ConsumeEvents</code> whose <code>run</code> drives an
in-memory loop, which is what a <code>register_split</code> branch uses. <code>open</code>,
<code>process_item</code>, <code>flush_window</code> and <code>on_close</code> may omit the
<code>res</code> parameter when they do not read resources.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form: <code>#[task::stream(state = ...)]</code> on an inherent impl</span>

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
        -> Result<Step, CanoError>
    {
        println!("stream ended ({reason:?})");
        Ok(Step::Done)
    }
}
```
</div>

<!-- Section: Windowing -->
<hr class="section-divider">
<h2 id="windowing"><a href="#windowing" class="anchor-link" aria-hidden="true">#</a>Windowing: <code>StreamWindow</code></h2>
<p>
<code>window()</code> returns a tumbling-window trigger. It sets how often <code>flush_window</code>
fires and how much the driver buffers. Larger windows spread the flush and checkpoint cost over more
items.
</p>

<div class="card-stack">
<div class="card">
<h3><code>StreamWindow::Count(n)</code></h3>
<p>Flush after every <code>n</code> successfully processed items. <code>n</code> is clamped to a
minimum of 1. The default is <code>Count(1)</code>, a flush per item.</p>
</div>
<div class="card">
<h3><code>StreamWindow::Duration(d)</code></h3>
<p>Flush every <code>d</code> of wall-clock time. <code>d</code> is clamped to a minimum of 1 ms, since
a zero duration would keep the tick permanently ready and never poll the source. An empty window is
skipped: an idle source emits no empty flushes and the deadline advances.</p>
</div>
</div>

<div class="callout callout-warning">
<span class="callout-label">Duration windows can drop an item</span>
<p>
When the deadline wins the race against a slow source, the driver drops the in-flight
<code>stream.next()</code> future. If the source's <code>poll_next</code> future owns the read state,
as in <code>stream::unfold(rx, |rx| async move { rx.recv().await })</code>, dropping it closes the
channel and loses the queued item. Use a source whose <code>poll_next</code> borrows its state, or
choose a window that is long relative to the source's cadence.
</p>
</div>

<!-- Section: Error policy -->
<hr class="section-divider">
<h2 id="error-policy"><a href="#error-policy" class="anchor-link" aria-hidden="true">#</a>Per-item error policy</h2>
<p>
<code>on_item_error()</code> returns a <code>StreamErrorPolicy</code> that decides what the loop does
when <code>process_item</code> returns <code>Err</code>:
</p>

<dl class="key-list">
<dt><code>FailFast</code> <small>default</small></dt>
<dd>Propagates the first item error. The loop stops and the run fails.</dd>
<dt><code>SkipAndContinue</code></dt>
<dd>Drops the bad item and keeps consuming, which suits poison messages. The skipped item's cursor is not committed. The next good item advances it.</dd>
<dt><code>RetryOnError { max_errors: u32 }</code></dt>
<dd>Tolerates up to <code>max_errors</code> consecutive item errors before failing. The counter resets on every successfully processed item.</dd>
</dl>

<!-- Section: Cursor -->
<hr class="section-divider">
<h2 id="cursor"><a href="#cursor" class="anchor-link" aria-hidden="true">#</a>Cursor persistence and resume</h2>
<p>
Register a stream with <code>Workflow::register(state, task)</code>. With a
<a href="../recovery/"><code>CheckpointStore</code></a> and a workflow id attached
(<code>with_checkpoint_store</code> and <code>with_workflow_id</code>), the engine persists the cursor
of the last item of each flushed window as a <code>CheckpointRow</code> whose <code>kind</code> is
<code>RowKind::StepCursor</code>, encoded with
<a href="https://docs.rs/serde_json"><code>serde_json</code></a>. On
<code>Workflow::resume_from(workflow_id)</code> the latest persisted cursor is passed to
<code>open(Some(cursor))</code>, so a crashed or cancelled run re-opens the source at the last
committed position. Only fully flushed windows commit a cursor. A window that errors mid-flush commits
nothing.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a resumable stream into a checkpointed workflow</span>

```rust
use cano::prelude::*;
use cano::RedbStore;
use std::sync::Arc;

let checkpoint_store = RedbStore::new("/var/lib/myapp/cano.redb")?;
let workflow = Workflow::new(resources)
    .with_checkpoint_store(Arc::new(checkpoint_store))
    .with_workflow_id("event-consumer")
    .register(Step::Consume, ConsumeEvents)          // cursor persisted per flushed window
    .add_exit_state(Step::Done);

// A crashed or cancelled run resumes from the last committed window:
// let result = workflow.resume_from_with_token("event-consumer", &token).await?;
// open() is called with Some(cursor), the position of the last fully flushed window.
```
</div>

<!-- Section: Cancellation -->
<hr class="section-divider">
<h2 id="cancellation"><a href="#cancellation" class="anchor-link" aria-hidden="true">#</a>The cancellation contract</h2>
<p>
An unbounded stream never ends on its own, so cancellation is how you stop it cleanly. When the run's
<code>CancellationToken</code> fires, the engine performs a cooperative drain:
</p>
<ol>
<li>it flushes the in-flight partial window through <code>flush_window</code>;</li>
<li>it calls <code>on_close(CloseReason::Cancelled)</code> for cleanup, and ignores the state it returns;</li>
<li>it commits the window's cursor;</li>
<li>the run ends as <code>CanoError::Cancelled</code> (<code>is_cancelled()</code> is true).</li>
</ol>

<p>
Pass a token with <code>workflow.orchestrate_with_token(state, &amp;token)</code> or
<code>resume_from_with_token(id, &amp;token)</code>, then call <code>token.cancel()</code> from
anywhere. <code>orchestrate</code> takes no token, so such a run ends only by exhaustion, a
<code>WindowSignal::Stop</code>, an error, or a total timeout.
</p>

<p>
<code>open()</code> races the same token, so a source that hangs while connecting is cancellable too.
No cursor exists yet at that point, so nothing is flushed or committed and the pending
<code>open</code> future is dropped.
</p>

<p>
<a href="../resilience/#workflow-total-timeout"><code>Workflow::with_total_timeout</code></a> applies to
stream states through the same drain. The engine folds the budget into the token the loop already
watches, so a deadline trip also flushes the in-flight window and commits its cursor. The stop trigger
decides the error:
</p>

<dl class="key-list">
<dt>The run's <code>CancellationToken</code> fires <small>surfaces <code>CanoError::Cancelled</code></small></dt>
<dd>The in-flight window is flushed, its cursor committed, and <code>on_close(Cancelled)</code> runs. <code>is_cancelled()</code> is true.</dd>
<dt>The <code>with_total_timeout</code> deadline trips <small>surfaces <code>CanoError::WorkflowTimeout</code>, category <code>"workflow_timeout"</code></small></dt>
<dd>The same drain runs.</dd>
</dl>

<div class="callout callout-warning">
<span class="callout-label">Cancel is a resumable stop</span>
<p>
A cancelled stream does not transition to another state. It surfaces as
<code>CanoError::Cancelled</code>, and a later <code>resume_from</code> continues from the last
committed window. An <code>Err</code> returned by <code>on_close</code> during the drain is
propagated.
</p>
</div>

<div class="callout callout-info">
<div class="callout-label">Under a Scheduler</div>
<p>
<code>RunningScheduler::stop()</code> fires the run token only after the drain timeout (20 s by
default) elapses. A <code>StreamTask</code> ends on its own only by exhaustion or
<code>WindowSignal::Stop</code>, so a scheduled stream holds the drain open for the whole timeout.
A scheduler that hosts streams sets <code>with_drain_timeout(Duration::ZERO)</code> to cancel them
as soon as <code>stop()</code> is called.
</p>
</div>

<div class="callout callout-warning">
<span class="callout-label">A failed drain flush replays its window</span>
<p>
If the drain's <code>flush_window</code> returns <code>Err</code>, <code>on_close(Cancelled)</code>
still runs as best-effort cleanup and its own error is discarded. The window's cursor is not
committed, and the run surfaces the flush error. A later <code>resume_from</code> re-opens at the
previous committed cursor and replays that window, as the
<a href="#idempotency">at-least-once contract</a> allows.
</p>
</div>

<div class="callout callout-info">
<div class="callout-label">What counts as cancelled</div>
<p>
A run is reported as cancelled (the
<a href="../observers/#on-cancelled"><code>on_cancelled</code></a> observer hook, the
<code>"cancelled"</code> metrics label) only when the engine's drain fired and the final result is
still <code>Cancelled</code>. If a later step of an otherwise successful drain fails, most likely the
<code>StepCursor</code> checkpoint append, that error wins and the run is reported as failed. A
<code>StreamTask</code> that returns <code>Err(CanoError::Cancelled)</code> from
<code>process_item</code>, <code>flush_window</code> or <code>on_close</code> never triggered the
drain, so it also stays on the failure path.
</p>
</div>

<!-- Section: idempotency -->
<hr class="section-divider">
<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The idempotency contract</h2>

<div class="callout callout-warning">
<span class="callout-label">At-least-once: make <code>open</code>, <code>process_item</code> and <code>on_close</code> idempotent</span>
<p>The FSM writes the state-entry checkpoint before running the task, so a resumed run re-enters the
state and calls <code>open(Some(cursor))</code> from the last committed cursor. The window after that
cursor may have been partly processed and is replayed. Make <code>process_item</code> safe to
re-apply: upserts, dedupe keys, "if not already processed" guards, conditional writes.
<code>on_close</code> runs once per run but may run again on resume, since a crash between
<code>on_close</code> and the cursor commit replays the boundary window.</p>
</div>

<div class="callout callout-info">
<div class="callout-label">The cursor log clears only at an exit state</div>
<p>
The checkpoint log is append-only and is cleared when the run reaches an exit state. A stream that
never terminates therefore accumulates one <code>StepCursor</code> row per flushed window. Bounded
streams, and runs that reach an exit state through <code>on_close</code> or
<code>WindowSignal::Stop</code>, are unaffected. For an endless source, use bounded windows and
restart periodically, which clears the log on the clean exit.
</p>
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="register"><a href="#register" class="anchor-link" aria-hidden="true">#</a>Registering a stream task</h2>
<p>
Register a stream with <code>Workflow::register(state, task)</code>, the same call every task model
uses. The engine drives the windowed loop itself:
</p>
<ul>
<li>with a checkpoint store and a workflow id, each flushed window's cursor is persisted;</li>
<li>the run's <code>CancellationToken</code> is observed, with the cooperative drain described
above;</li>
<li><code>process_item</code>, <code>flush_window</code> and <code>on_close</code> run under panic
safety, so a panic becomes a <code>CanoError</code> instead of unwinding past the FSM.</li>
</ul>
<p>
The companion <code>Task</code> impl that the macro emits serves <code>register_split</code>
branches. It drives an in-memory windowed loop with no cursor persistence and no cancellation, and it
ends only through source exhaustion, a <code>WindowSignal::Stop</code>, or an error.
</p>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit trait-impl form</h2>
<p>
To name the associated types or write a generic impl, put a bare <code>#[task::stream]</code> on an
<code>impl StreamTask&lt;...&gt; for ...</code> block and declare the three <code>type</code> lines
(<code>Item</code>, <code>Output</code>, <code>Cursor</code>) yourself. The companion
<code>impl Task</code> is still emitted.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form: <code>#[task::stream]</code> on a trait impl</span>

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
        -> Result<Step, CanoError>
    {
        Ok(Step::Done)
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to use StreamTask</h2>
<p>
Use a <code>StreamTask</code> when the source is unbounded or continuous (a Kafka topic, an SSE feed,
a tailed file, a WebSocket) and never produces a final <code>Vec</code> to aggregate. It fits when you
want per-window emission with bounded memory, and a consumer that resumes from a committed offset
after a crash or a cancellation.
</p>
<p>
For a bounded collection that you map over and aggregate once, a
<a href="../batch-task/">BatchTask</a> is simpler. For a long iterative job over a finite range that
resumes mid-loop, a <a href="../stepped-task/">SteppedTask</a> fits better.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
Run the complete example with <code>cargo run --example stream_task</code>.
</p>
</div>
</div>
