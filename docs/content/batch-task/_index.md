+++
title = "BatchTask"
description = "BatchTask in Cano - fan out over data items with bounded concurrency and per-item retry, then re-join in one state."
template = "section.html"
+++

<div class="content-wrapper">
<h1>BatchTask</h1>
<p class="subtitle">Fan out over data items with bounded concurrency and per-item retry, then re-join.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.12.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>

<p>
A <code>BatchTask</code> loads a <code>Vec</code> of work items, processes them with bounded
concurrency (each item independently retryable), collects the per-item results in input order, and
decides the next state from the aggregate, all within one workflow state. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>,
<a href="../poll-task/">PollTask</a>, <a href="../timer-task/">TimerTask</a>,
<a href="../stepped-task/">SteppedTask</a>, <a href="../stream-task/">StreamTask</a> and
<a href="../suspend-task/">SuspendTask</a>, and it reads
typed dependencies from <a href="../resources/">Resources</a> like the rest. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance: <code>#[task::batch]</code> infers <code>Item</code> and <code>ItemOutput</code> from the signatures</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Crunch, Done }

struct Crunch;

#[task::batch(state = Stage)]
impl Crunch {
    // Resource-free load and finish omit `res: &Resources`.
    async fn load(&self) -> Result<Vec<u32>, CanoError> {
        Ok((1..=100).collect())
    }
    // Returning Err here does not fail the batch; it lands in that item's slot of `outputs`.
    async fn process_item(&self, n: &u32) -> Result<u64, CanoError> {
        Ok((*n as u64) * (*n as u64))
    }
    // Aggregates per-item results in input order and picks the next FSM state.
    async fn finish(&self, outputs: Vec<Result<u64, CanoError>>) -> Result<Stage, CanoError> {
        let total: u64 = outputs.into_iter().flatten().sum();
        println!("sum of squares = {total}");
        Ok(Stage::Done)
    }
    fn concurrency(&self) -> usize { 8 } // up to 8 process_item futures in flight
    fn item_retry(&self) -> RetryMode { RetryMode::fixed(2, std::time::Duration::from_millis(50)) }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">Batch or Split?</div>
<p>
A <code>BatchTask</code> maps one operation over many data items inside one state.
<a href="../split-join/">Split / Join</a> runs several different tasks in parallel for one state. See
<a href="#vs-split">BatchTask vs Split &amp; Join</a>.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#shape">The three methods</a></li>
<li><a href="#quick-start">Quick start with <code>#[task::batch]</code></a></li>
<li><a href="#registering">Registering a batch task</a></li>
<li><a href="#tuning">Concurrency and retry tuning</a></li>
<li><a href="#vs-split">BatchTask vs Split &amp; Join</a></li>
<li><a href="#explicit">Explicit trait-impl form</a></li>
<li><a href="#when-to-use">When to use BatchTask</a></li>
</ol>
</nav>

<!-- Section: shape -->
<hr class="section-divider">
<h2 id="shape"><a href="#shape" class="anchor-link" aria-hidden="true">#</a>The three methods</h2>

<div class="diagram-frame">
<p class="diagram-label">BatchTask: load, process_item for each item, finish</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 708 246" role="img" aria-labelledby="batch-flow-title batch-flow-desc">
<title id="batch-flow-title">A BatchTask fans out over data inside one state</title>
<desc id="batch-flow-desc">load produces a list of items. process_item runs over all N items with at most concurrency() running at once. finish receives the per-item results in input order and returns the next state. An Err from load or finish fails the state, and re-runs the whole load, process, finish cycle only when config() enables retries.</desc>
<defs><marker id="batch-flow-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M626,64 V44 Q626,36 618,36 H84 Q76,36 76,44 V59" marker-end="url(#batch-flow-ah)"/>
<text class="t-mut" x="351" y="18">Err in load or finish: config() retries the whole cycle, if enabled</text>
<path class="e" d="M128,87 H262" marker-end="url(#batch-flow-ah)"/>
<text class="t-code" x="195" y="70">items: Vec&lt;Item&gt;</text>
<path class="e" d="M444,87 H570" marker-end="url(#batch-flow-ah)"/>
<text class="t-mut" x="507" y="54">per-item Results</text>
<text class="t-mut" x="507" y="70">in input order</text>
<rect class="n" x="24" y="64" width="104" height="46" rx="10"/>
<text class="t-code" x="76" y="88">load</text>
<rect class="n" x="282" y="48" width="158" height="46" rx="10"/>
<rect class="n" x="274" y="56" width="158" height="46" rx="10"/>
<rect class="n-hot" x="266" y="64" width="158" height="46" rx="10"/>
<text class="t-code t-strong" x="345" y="88">process_item</text>
<text class="t-mut" x="345" y="130">&times;N, up to concurrency() at once</text>
<rect class="n" x="574" y="64" width="104" height="46" rx="10"/>
<text class="t-code" x="626" y="88">finish</text>
<path class="e" d="M626,110 V171" marker-end="url(#batch-flow-ah)"/>
<text class="t-code t-mut ta-e" x="616" y="142">Ok(state)</text>
<rect class="n-ok" x="568" y="176" width="116" height="46" rx="10"/>
<text x="626" y="200">Next State</text>
</svg>
</div>
</div>

<p>
A <code>BatchTask</code> has two associated types, <code>type Item: Send + Sync + 'static</code> and
<code>type ItemOutput: Send + 'static</code>. <code>#[task::batch(state = ...)]</code> infers both from
the method signatures, or you write them yourself. It has three required methods:
</p>
<dl class="key-list">
<dt><code>async fn load(&amp;self, res: &amp;Resources) -&gt; Result&lt;Vec&lt;Self::Item&gt;, CanoError&gt;</code></dt>
<dd>Produces the items. An <code>Err</code> fails the batch and triggers the outer <code>config()</code> retry.</dd>
<dt><code>async fn process_item(&amp;self, item: &amp;Self::Item) -&gt; Result&lt;Self::ItemOutput, CanoError&gt;</code></dt>
<dd>Processes one item. It takes <code>&amp;Item</code> because <code>item_retry</code> may call it again. An <code>Err</code> does not fail the batch: it lands in that item's slot of <code>finish</code>'s <code>outputs</code>.</dd>
<dt><code>async fn finish(&amp;self, res: &amp;Resources, outputs: Vec&lt;Result&lt;Self::ItemOutput, CanoError&gt;&gt;) -&gt; Result&lt;TState, CanoError&gt;</code></dt>
<dd>Receives one slot per input item, in input order, and returns the next state. The partial-failure policy lives here, for example "at least 50% must succeed". An <code>Err</code> fails the batch.</dd>
</dl>

<p>Four optional methods, each shown with its default:</p>
<dl class="key-list">
<dt><code>fn concurrency(&amp;self) -&gt; usize</code> <small>default <code>1</code>, sequential</small></dt>
<dd>How many items run at once.</dd>
<dt><code>fn item_retry(&amp;self) -&gt; RetryMode</code> <small>default <code>RetryMode::None</code></small></dt>
<dd>Retry policy for each item, independent of the outer dispatch.</dd>
<dt><code>fn config(&amp;self) -&gt; TaskConfig</code> <small>default <code>TaskConfig::new()</code>, one attempt</small></dt>
<dd>Retry config for the whole <code>load</code>, process, <code>finish</code> cycle. The cycle only fails, and so only retries, when <code>load</code> or <code>finish</code> returns <code>Err</code>. Opt in with <code>TaskConfig::new().with_exponential_retry(3)</code>.</dd>
<dt><code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code> <small>default: the type name</small></dt>
<dd>Identifies the task in logs and observers.</dd>
</dl>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start with <code>#[task::batch]</code></h2>
<p>
Attach <code>#[task::batch(state = MyState)]</code> to an inherent <code>impl</code> block. The macro
infers <code>Item</code> and <code>ItemOutput</code> from the signatures and generates the
<code>impl BatchTask&lt;MyState&gt;</code> header, a <code>Registrable</code> impl, and a companion
<code>impl Task&lt;MyState&gt;</code> whose <code>run</code> drives the bounded-concurrency loop (built
on <code>futures_util::buffer_unordered</code>). The companion also lets a batch serve as a
<code>register_split</code> branch. <code>load</code> and <code>finish</code> may omit the
<code>res</code> parameter when they do not read resources.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#127760;</span> Inference form: fan out over a list of URLs</span>

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
        let board = res.get::<Blackboard>()?;
        let urls: Vec<String> = board.get("urls")?;
        Ok(urls)
    }

    async fn process_item(&self, url: &String) -> Result<usize, CanoError> {
        // Pretend to fetch; return the response byte count.
        Ok(url.len() * 100)
    }

    async fn finish(&self, res: &Resources, outputs: Vec<Result<usize, CanoError>>)
        -> Result<Step, CanoError>
    {
        let ok = outputs.iter().filter(|r| r.is_ok()).count();
        let total: usize = outputs.iter().filter_map(|r| r.as_ref().ok()).sum();
        let board = res.get::<Blackboard>()?;
        board.put("total_bytes", total);
        if ok * 2 >= outputs.len() {                 // partial-failure policy: >= 50% must succeed
            Ok(Step::Summarise)
        } else {
            Err(CanoError::task_execution("too many fetch failures"))
        }
    }
}
```
</div>


<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a batch task</h2>
<p>
Register a batch task with <code>Workflow::register</code>, the same call every task model uses. To the
workflow it is one ordinary state; the fan-out and re-join happen inside the generated <code>run</code>.
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
<h2 id="tuning"><a href="#tuning" class="anchor-link" aria-hidden="true">#</a>Concurrency and retry tuning</h2>
<div class="card-stack">
<div class="card">
<h3><code>concurrency()</code>: width of the fan-out</h3>
<p>Defaults to <code>1</code>, fully sequential. Raise it to run that many <code>process_item</code>
calls at once. Results are still collected in input order, and <code>0</code> is treated as <code>1</code>.</p>
</div>
<div class="card">
<h3><code>item_retry()</code>: per-item resilience</h3>
<p>Defaults to <code>RetryMode::None</code>. Set, for example,
<code>RetryMode::fixed(2, Duration::from_millis(50))</code> to retry an individual item before its
slot becomes an <code>Err</code>. It is independent of the outer <code>config()</code> retry.</p>
</div>
<div class="card">
<h3><code>config()</code>: whole-cycle retry</h3>
<p>Defaults to <code>TaskConfig::new()</code>, one attempt. It retries the entire
<code>load → process → finish</code> cycle, which only fails when <code>load</code> or
<code>finish</code> returns <code>Err</code>. Opt in with <code>TaskConfig::new().with_exponential_retry(3)</code>.</p>
</div>
</div>

<!-- Section: vs split -->
<hr class="section-divider">
<h2 id="vs-split"><a href="#vs-split" class="anchor-link" aria-hidden="true">#</a>BatchTask vs Split &amp; Join</h2>
<div class="comparison-grid">
<div class="comparison-col">
<h3>BatchTask</h3>
<p>Fans out over data items, inside one state.</p>
<ul>
<li>One state, one re-join point (<code>finish</code>)</li>
<li>Per-item retry via <code>item_retry()</code></li>
<li>Item failures are collected, and the policy lives in <code>finish</code></li>
<li>Bounded concurrency via <code>concurrency()</code></li>
</ul>
</div>
<div class="comparison-col">
<h3>Split &amp; Join</h3>
<p>Fans out over tasks, inside one state.</p>
<ul>
<li>One state runs many parallel <code>Task</code>s, reconciled by a join strategy</li>
<li><code>JoinStrategy</code>: <code>All</code>, <code>Any</code>, <code>Quorum</code>, <code>Percentage</code>, <code>PartialResults</code>, <code>PartialTimeout</code></li>
<li>An optional <a href="../split-join/#bulkhead">bulkhead</a> caps concurrency</li>
<li>See the <a href="../split-join/">Split and join</a> guide</li>
</ul>
</div>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit trait-impl form</h2>
<p>
To write the trait header yourself, put a bare <code>#[task::batch]</code> on an
<code>impl BatchTask&lt;...&gt; for ...</code> block and declare <code>type Item</code> and
<code>type ItemOutput</code>. The macro still emits the companion <code>impl Task</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form: <code>#[task::batch]</code> on a trait impl</span>

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
        let board = res.get::<Blackboard>()?;
        let urls: Vec<String> = board.get("urls")?;
        Ok(urls)
    }

    async fn process_item(&self, url: &String) -> Result<usize, CanoError> {
        // Each item runs up to concurrency() at once; errors map into finish's outputs Vec.
        Ok(url.len() * 100)
    }

    async fn finish(&self, _res: &Resources, outputs: Vec<Result<usize, CanoError>>)
        -> Result<Step, CanoError>
    {
        // Enforce partial-failure threshold before advancing.
        let ok = outputs.iter().filter(|r| r.is_ok()).count();
        if ok * 2 >= outputs.len() {
            Ok(Step::Summarise)
        } else {
            Err(CanoError::task_execution("too many fetch failures"))
        }
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to use BatchTask</h2>
<p>
Use a <code>BatchTask</code> to map one operation over a collection, such as fetching <em>N</em> URLs
or validating <em>N</em> records, when you want bounded parallelism, per-item retry, and one re-join
point where you apply the partial-failure policy and choose the transition. If each item is its own
multi-step workflow, use a <a href="../split-join/">Split</a> instead.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
The crate ships a complete example. Run it with <code>cargo run --example batch_task</code>.
</p>
</div>
</div>
