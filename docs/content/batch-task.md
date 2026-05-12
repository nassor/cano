+++
title = "BatchTask"
description = "BatchTask in Cano - fan out over data items with bounded concurrency and per-item retry, then re-join in one state."
template = "page.html"
+++

<div class="content-wrapper">
<h1>BatchTask</h1>
<p class="subtitle">Fan out over data items — bounded concurrency, per-item retry, then re-join.</p>

<p>
A <code>BatchTask</code> loads a <code>Vec</code> of work items, processes them with bounded
concurrency (each item independently retryable), collects the per-item results <strong>in input
order</strong>, and decides the next state from the aggregate — all within one workflow state. It is
one of the <a href="../task/">Task</a> family of processing models, alongside
<a href="../nodes/">Node</a>, <a href="../router-task/">RouterTask</a>,
<a href="../poll-task/">PollTask</a>, and <a href="../stepped-task/">SteppedTask</a>, and it reads
typed dependencies from <a href="../resources/">Resources</a> like the rest. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance — <code>#[task::batch]</code> infers <code>Item</code> / <code>ItemOutput</code> from the signatures</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Crunch, Done }

struct Crunch;

#[task::batch(state = Stage)]
impl Crunch {
    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok((1..=100).collect())
    }
    // Returning Err here doesn't fail the batch — it lands in that slot of `finish`'s `outputs`.
    async fn process_item(&self, n: &u32) -> Result<u64, CanoError> {
        Ok((*n as u64) * (*n as u64))
    }
    async fn finish(&self, _res: &Resources, outputs: Vec<Result<u64, CanoError>>)
        -> Result<TaskResult<Stage>, CanoError>
    {
        let total: u64 = outputs.into_iter().flatten().sum();
        println!("sum of squares = {total}");
        Ok(TaskResult::Single(Stage::Done))
    }
    fn concurrency(&self) -> usize { 8 }                  // up to 8 process_item in flight
    fn item_retry(&self) -> RetryMode { RetryMode::fixed(2, std::time::Duration::from_millis(50)) }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">Not the same as <code>TaskResult::Split</code></div>
<p>
<a href="../split-join/">Split / Join</a> fans out over <em>states</em>: the FSM re-enters with one
workflow branch per state, then a <code>JoinStrategy</code> reconciles them. A <code>BatchTask</code>
fans out over <em>data</em>: many items, all inside <em>one</em> state, re-joined before the
transition. Use Split for "run these N <em>workflows</em> in parallel"; use a <code>BatchTask</code>
for "map this one operation over N <em>items</em>".
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#shape">The Three Methods</a></li>
<li><a href="#quick-start">Quick Start with <code>#[task::batch]</code></a></li>
<li><a href="#registering">Registering a Batch Task</a></li>
<li><a href="#tuning">Concurrency &amp; Retry Tuning</a></li>
<li><a href="#vs-split">BatchTask vs Split &amp; Join</a></li>
<li><a href="#explicit">Explicit Trait-Impl Form</a></li>
<li><a href="#object-safe">Type-Erased Aliases</a></li>
<li><a href="#when-to-use">When to Use BatchTask</a></li>
</ol>
</nav>

<!-- Section: shape -->
<hr class="section-divider">
<h2 id="shape"><a href="#shape" class="anchor-link" aria-hidden="true">#</a>The Three Methods</h2>

<div class="diagram-frame">
<p class="diagram-label">BatchTask: load &rarr; process_item (&times;N) &rarr; finish</p>
<div class="mermaid">
graph LR
A[load] -->|"items: Vec of Item"| B[process_item ×N]
B -->|"per-item Results — input order"| C[finish]
C -->|TaskResult| D[Next State]
</div>
</div>

<p>
A <code>BatchTask</code> has two associated types — <code>type Item: Send + Sync + 'static</code> and
<code>type ItemOutput: Send + 'static</code> (inferred by <code>#[task::batch(state = ...)]</code> from
the method signatures, à la <code>#[task::node]</code>'s <code>PrepResult</code> / <code>ExecResult</code>,
or written explicitly) — and three methods:
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
<td><code>async fn load(&amp;self, res) -&gt; Result&lt;Vec&lt;Self::Item&gt;, CanoError&gt;</code></td>
<td>Produce the items. An <code>Err</code> here fails the batch (and triggers the outer <code>config()</code> retry).</td>
</tr>
<tr>
<td><code>async fn process_item(&amp;self, item: &amp;Self::Item) -&gt; Result&lt;Self::ItemOutput, CanoError&gt;</code></td>
<td>Process one item. Takes <code>&amp;Item</code> because <code>item_retry</code> may re-invoke it. Returning <code>Err</code> does <strong>not</strong> fail the batch — it lands in that slot of <code>finish</code>'s <code>outputs</code>.</td>
</tr>
<tr>
<td><code>async fn finish(&amp;self, res, outputs: Vec&lt;Result&lt;Self::ItemOutput, CanoError&gt;&gt;) -&gt; Result&lt;TaskResult&lt;TState&gt;, CanoError&gt;</code></td>
<td>Aggregate — one slot per input item, <strong>in input order</strong> — and decide the next state. This is where the partial-failure <em>policy</em> lives (e.g. "≥ 50% must succeed"). An <code>Err</code> here fails the batch.</td>
</tr>
</tbody>
</table>

<p>Optional knobs, all with defaults:</p>
<table class="styled-table">
<thead>
<tr>
<th>Method</th>
<th>Default</th>
<th>Purpose</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>fn concurrency(&amp;self) -&gt; usize</code></td>
<td><code>1</code> (sequential)</td>
<td>How many items run at once.</td>
</tr>
<tr>
<td><code>fn item_retry(&amp;self) -&gt; RetryMode</code></td>
<td><code>RetryMode::None</code></td>
<td>Per-item retry policy, independent of the outer dispatch.</td>
</tr>
<tr>
<td><code>fn config(&amp;self) -&gt; TaskConfig</code></td>
<td><code>TaskConfig::default()</code></td>
<td>Retry config for the <em>whole</em> <code>load → process → finish</code> cycle — the batch only <code>Err</code>s (triggering this) when <code>load</code> or <code>finish</code> <code>Err</code>s.</td>
</tr>
<tr>
<td><code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code></td>
<td>type name</td>
<td>Identifies the task in logs / observers.</td>
</tr>
</tbody>
</table>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start with <code>#[task::batch]</code></h2>
<p>
Attach <code>#[task::batch(state = MyState)]</code> (optionally <code>key = MyKey</code>) to an
inherent <code>impl</code> block. The macro infers <code>Item</code> / <code>ItemOutput</code> from
the signatures, injects default bodies for <code>concurrency</code> / <code>item_retry</code> /
<code>config</code> / <code>name</code> if absent, synthesises the
<code>impl BatchTask&lt;MyState&gt; for Fetcher</code> header, and emits a companion
<code>impl Task&lt;MyState&gt; for Fetcher</code> whose <code>run</code> runs the bounded-concurrency
loop (via <code>cano::task::batch::run_batch</code>, which uses
<code>futures_util::buffer_unordered</code>). No engine changes.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#127760;</span> Inference form — fan out over a list of URLs</span>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Fetch, Summarise, Done }

struct FetchUrls;

#[task::batch(state = Step)]
impl FetchUrls {
    fn concurrency(&self) -> usize { 8 }
    fn item_retry(&self) -> RetryMode { RetryMode::fixed(2, Duration::from_millis(50)) }

    async fn load(&self, res: &Resources) -> Result<Vec<String>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        Ok(store.get("urls")?)
    }

    async fn process_item(&self, url: &String) -> Result<usize, CanoError> {
        // pretend to fetch; return the byte count
        Ok(url.len() * 100)
    }

    async fn finish(&self, res: &Resources, outputs: Vec<Result<usize, CanoError>>)
        -> Result<TaskResult<Step>, CanoError>
    {
        let ok = outputs.iter().filter(|r| r.is_ok()).count();
        let total: usize = outputs.iter().filter_map(|r| r.as_ref().ok()).sum();
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("total_bytes", total)?;
        if ok * 2 >= outputs.len() {                 // partial-failure policy: ≥ 50% must succeed
            Ok(TaskResult::Single(Step::Summarise))
        } else {
            Err(CanoError::task_execution("too many fetch failures"))
        }
    }
}
```
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
<code>process_item</code> returning <code>Err</code> is <em>not</em> a batch failure — that error
lands in the matching slot of <code>finish</code>'s <code>outputs</code> vector. Decide the
partial-failure policy in <code>finish</code>: count the <code>Ok</code>s, set a threshold, and
either transition or <code>Err</code> out (which then triggers the outer <code>config()</code>
retry, re-running the <em>whole</em> <code>load → process → finish</code> cycle).
</p>
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a Batch Task</h2>
<p>
Register a batch task with plain <code>Workflow::register</code> — to the FSM it's an ordinary
<code>Single</code> state; the fan-out and re-join happen inside the generated <code>run</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a batch task into a workflow</span>

```rust
use cano::prelude::*;

let workflow = Workflow::new(resources)
    .register(Step::Fetch, FetchUrls)
    .register(Step::Summarise, Summarise)
    .add_exit_state(Step::Done);
```
</div>

<!-- Section: tuning -->
<hr class="section-divider">
<h2 id="tuning"><a href="#tuning" class="anchor-link" aria-hidden="true">#</a>Concurrency &amp; Retry Tuning</h2>
<div class="card-stack">
<div class="card">
<h3><code>concurrency()</code> — width of the fan-out</h3>
<p>Defaults to <code>1</code> (fully sequential). Raise it to run that many <code>process_item</code>
calls at once; results are still collected in input order regardless.</p>
</div>
<div class="card">
<h3><code>item_retry()</code> — per-item resilience</h3>
<p>Defaults to <code>RetryMode::None</code>. Set e.g.
<code>RetryMode::fixed(2, Duration::from_millis(50))</code> to retry an individual item before its
slot becomes an <code>Err</code> — orthogonal to the outer <code>config()</code> retry.</p>
</div>
<div class="card">
<h3><code>config()</code> — whole-cycle retry</h3>
<p>Defaults to <code>TaskConfig::default()</code>. This governs retries of the entire
<code>load → process → finish</code> cycle, which only happens when <code>load</code> or
<code>finish</code> returns <code>Err</code>.</p>
</div>
</div>

<!-- Section: vs split -->
<hr class="section-divider">
<h2 id="vs-split"><a href="#vs-split" class="anchor-link" aria-hidden="true">#</a>BatchTask vs Split &amp; Join</h2>
<div class="comparison-grid">
<div class="comparison-col">
<h3>BatchTask</h3>
<p><strong>Fans out over:</strong> data items, inside one state.</p>
<ul>
<li>One state, one re-join point (<code>finish</code>)</li>
<li>Per-item retry via <code>item_retry()</code></li>
<li>Item failures collected, not fatal — policy lives in <code>finish</code></li>
<li>Bounded concurrency via <code>concurrency()</code></li>
</ul>
</div>
<div class="comparison-col">
<h3>Split &amp; Join</h3>
<p><strong>Fans out over:</strong> workflow states / branches.</p>
<ul>
<li>One state ⇒ many parallel <code>Task</code>s, each transitioning</li>
<li><code>JoinStrategy</code> — <code>All</code>, <code>Any</code>, <code>Quorum</code>, …</li>
<li>Optional <a href="../split-join/#bulkhead">bulkhead</a> caps concurrency</li>
<li>See the <a href="../split-join/">Split &amp; Join</a> guide</li>
</ul>
</div>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit Trait-Impl Form</h2>
<p>
Prefer the trait header explicit? Put a bare <code>#[task::batch]</code> on an
<code>impl BatchTask&lt;...&gt; for ...</code> block and declare <code>type Item</code> /
<code>type ItemOutput</code> yourself. The companion <code>impl Task</code> is still emitted.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form — <code>#[task::batch]</code> on a trait impl</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Fetch, Summarise, Done }

struct FetchUrls;

#[task::batch]
impl BatchTask<Step> for FetchUrls {
    type Item = String;
    type ItemOutput = usize;

    fn concurrency(&self) -> usize { 8 }

    async fn load(&self, res: &Resources) -> Result<Vec<String>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        Ok(store.get("urls")?)
    }

    async fn process_item(&self, url: &String) -> Result<usize, CanoError> {
        Ok(url.len() * 100)
    }

    async fn finish(&self, _res: &Resources, outputs: Vec<Result<usize, CanoError>>)
        -> Result<TaskResult<Step>, CanoError>
    {
        let ok = outputs.iter().filter(|r| r.is_ok()).count();
        if ok * 2 >= outputs.len() {
            Ok(TaskResult::Single(Step::Summarise))
        } else {
            Err(CanoError::task_execution("too many fetch failures"))
        }
    }
}
```
</div>

<!-- Section: object-safe aliases -->
<hr class="section-divider">
<h2 id="object-safe"><a href="#object-safe" class="anchor-link" aria-hidden="true">#</a>Type-Erased Aliases</h2>
<p>
As with <code>DynNode</code>, the object-safe aliases pin the associated types: <code>Item</code> to
<code>Box&lt;dyn Any + Send + Sync&gt;</code> and <code>ItemOutput</code> to
<code>Box&lt;dyn Any + Send&gt;</code>.
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
<td><code>DynBatchTask&lt;TState, TResourceKey&gt;</code></td>
<td><code>dyn BatchTask&lt;TState, TResourceKey, Item = …, ItemOutput = …&gt;</code></td>
</tr>
<tr>
<td><code>BatchTaskObject&lt;TState, TResourceKey&gt;</code></td>
<td><code>Arc&lt;dyn BatchTask&lt;TState, TResourceKey, Item = …, ItemOutput = …&gt;&gt;</code></td>
</tr>
</tbody>
</table>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use BatchTask</h2>
<p>Reach for a <code>BatchTask</code> when you want to map a sub-operation over a collection — fetch
<em>N</em> URLs, process <em>N</em> records, validate <em>N</em> inputs — with:</p>
<ul>
<li><strong>bounded parallelism</strong> (don't open 10 000 sockets at once);</li>
<li><strong>per-item retry</strong> (one flaky record shouldn't sink the rest);</li>
<li><strong>a single re-join point</strong> where you apply the partial-failure policy and decide the
transition;</li>
<li>and you'd rather not spawn <em>N</em> workflow branches to do it.</li>
</ul>
<p>
If instead each item is really its own multi-step workflow, that's a <a href="../split-join/">Split</a>
job, not a batch.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
The crate ships a complete example — run it with <code>cargo run --example batch_task</code>.
</p>
</div>
</div>
