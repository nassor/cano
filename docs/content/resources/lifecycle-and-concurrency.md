+++
title = "Resource Lifecycle and Concurrency"
description = "When Cano resource setup()/teardown() run, how resources behave across concurrent splits, and the difference between standalone and scheduler lifecycles."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Resource Lifecycle and Concurrency</h1>
<p class="subtitle">When setup/teardown fire, and how resources behave under concurrency.</p>

<p>
See <a href="../">Resources</a> for defining and retrieving resources. This page covers their lifecycle
and concurrency guarantees.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#lifecycle">Lifecycle Guarantees</a></li>
<li><a href="#concurrency">Concurrency and Interior Mutability</a></li>
<li><a href="#orchestrate-lifecycle">Standalone vs Scheduler Lifecycle</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="lifecycle"><a href="#lifecycle" class="anchor-link" aria-hidden="true">#</a>Lifecycle Guarantees</h2>

<div class="diagram-frame">
<p class="diagram-label">setup_all() in FIFO order; teardown in LIFO order</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 750 496" role="img">
<title>The engine calls setup() on resources in insertion order (FIFO) and teardown() in exactly the reverse order (LIFO), so each resource's live span nests inside the spans of the resources set up before it.</title>
<defs><marker id="life-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="110" y1="46" x2="110" y2="458"/>
<line class="lifeline" x1="300" y1="46" x2="300" y2="458"/>
<line class="lifeline" x1="480" y1="46" x2="480" y2="458"/>
<line class="lifeline" x1="660" y1="46" x2="660" y2="458"/>
<rect class="band" x="295" y="126" width="10" height="316" rx="5"/>
<rect class="band" x="475" y="170" width="10" height="228" rx="5"/>
<rect class="band" x="655" y="214" width="10" height="140" rx="5"/>
<rect class="n" x="50" y="8" width="120" height="38" rx="8"/>
<text class="t-strong" x="110" y="28">Engine</text>
<rect class="n" x="235" y="8" width="130" height="38" rx="8"/>
<text class="t-strong" x="300" y="28">Resource A</text>
<rect class="n" x="415" y="8" width="130" height="38" rx="8"/>
<text class="t-strong" x="480" y="28">Resource B</text>
<rect class="n" x="595" y="8" width="130" height="38" rx="8"/>
<text class="t-strong" x="660" y="28">Resource C</text>
<rect class="n" x="22" y="58" width="176" height="44" rx="10"/>
<text class="t-code" x="110" y="72">setup_all()</text>
<text class="t-strong" x="110" y="89">FIFO order</text>
<path class="e" d="M124,126 H293" marker-end="url(#life-ah)"/>
<text class="t-code ta-s" x="142" y="113">setup()</text>
<circle class="n" cx="110" cy="126" r="10"/>
<text class="t-mut" x="110" y="127">1</text>
<path class="e" d="M124,170 H473" marker-end="url(#life-ah)"/>
<text class="t-code ta-s" x="142" y="157">setup()</text>
<circle class="n" cx="110" cy="170" r="10"/>
<text class="t-mut" x="110" y="171">2</text>
<path class="e" d="M124,214 H653" marker-end="url(#life-ah)"/>
<text class="t-code ta-s" x="142" y="201">setup()</text>
<circle class="n" cx="110" cy="214" r="10"/>
<text class="t-mut" x="110" y="215">3</text>
<rect class="n" x="22" y="238" width="176" height="34" rx="10"/>
<text class="t-code" x="110" y="256">execute_workflow()</text>
<rect class="n-hot" x="22" y="286" width="176" height="44" rx="10"/>
<text class="t-code" x="110" y="300">teardown_range(all)</text>
<text class="t-strong t-hot" x="110" y="317">LIFO order</text>
<path class="e" d="M124,354 H653" marker-end="url(#life-ah)"/>
<text class="t-code ta-s" x="142" y="341">teardown()</text>
<circle class="n" cx="110" cy="354" r="10"/>
<text class="t-mut" x="110" y="355">4</text>
<path class="e" d="M124,398 H473" marker-end="url(#life-ah)"/>
<text class="t-code ta-s" x="142" y="385">teardown()</text>
<circle class="n" cx="110" cy="398" r="10"/>
<text class="t-mut" x="110" y="399">5</text>
<path class="e" d="M124,442 H293" marker-end="url(#life-ah)"/>
<text class="t-code ta-s" x="142" y="429">teardown()</text>
<circle class="n" cx="110" cy="442" r="10"/>
<text class="t-mut" x="110" y="443">6</text>
<text class="t-mut" x="375" y="478">Bars mark each resource's live span — they nest, so teardown mirrors setup exactly.</text>
</svg>
</div>
</div>

<table class="styled-table">
<thead>
<tr><th>Guarantee</th><th>Detail</th></tr>
</thead>
<tbody>
<tr><td>Setup order</td><td>FIFO — insert dependencies before dependents.</td></tr>
<tr><td>Teardown order</td><td>LIFO — each teardown can rely on its dependencies still being live.</td></tr>
<tr><td>Sequentiality</td><td>Setup and teardown calls are sequential, never concurrent.</td></tr>
<tr><td>Partial rollback</td><td>If <code>setup</code> fails at position N, teardown runs LIFO from N&minus;1 to 0. Resources at positions &ge; N never set up, never torn down.</td></tr>
<tr><td>Teardown errors</td><td>Logged, never aborts the sequence — every remaining resource still gets torn down.</td></tr>
</tbody>
</table>

<div class="code-block">
<span class="code-block-label">Partial rollback — only A and B are torn down</span>

```rust
use cano::prelude::*;

#[derive(Resource)] struct ServiceA;
#[derive(Resource)] struct ServiceB;
#[derive(Resource)] struct ServiceD;

struct ServiceC;

impl ServiceA { fn new() -> Self { Self } }
impl ServiceB { fn new() -> Self { Self } }
impl ServiceC { fn new() -> Self { Self } }
impl ServiceD { fn new() -> Self { Self } }

#[resource]
impl Resource for ServiceC {
    async fn setup(&self) -> Result<(), CanoError> {
        Err(CanoError::Configuration("ServiceC failed".into()))
    }
}

#[tokio::main]
async fn main() {
    let resources: Resources = Resources::new()
        .insert("a", ServiceA::new())  // position 0 — setup OK
        .insert("b", ServiceB::new())  // position 1 — setup OK
        .insert("c", ServiceC::new())  // position 2 — setup FAILS
        .insert("d", ServiceD::new()); // position 3 — never reached

    // setup_all() returns Err from C; teardown runs for B then A (LIFO)
    let result = resources.setup_all().await;
    assert!(result.is_err());
}
```
</div>

<!-- Section: Concurrency -->
<hr class="section-divider">
<h2 id="concurrency"><a href="#concurrency" class="anchor-link" aria-hidden="true">#</a>Concurrency and Interior Mutability</h2>

<p>
<code>get()</code> returns an <code>Arc&lt;R&gt;</code>. Split tasks each get their own clone —
a refcount bump, no data copy. Read-only resources need no synchronization. Resources with
mutable state must use interior mutability.
</p>

<table class="styled-table">
<thead>
<tr><th>Primitive</th><th>When to use</th></tr>
</thead>
<tbody>
<tr><td><code>tokio::sync::RwLock&lt;T&gt;</code></td><td>Many readers, rare writers. Best default.</td></tr>
<tr><td><code>tokio::sync::Mutex&lt;T&gt;</code></td><td>Exclusive access. Serializes split tasks — watch throughput.</td></tr>
<tr><td><code>std::sync::atomic::*</code></td><td>Counters and flags. Lock-free.</td></tr>
<tr><td><code>DashMap&lt;K, V&gt;</code></td><td>Concurrent map writes. Sharded locking.</td></tr>
</tbody>
</table>

<div class="callout callout-warning">
<div class="callout-label">Coarse locks eliminate parallelism</div>
<p>
A single <code>Mutex&lt;T&gt;</code> guarding an entire resource serializes split tasks that
write to it. If your splits all hit the same resource, parallelism gives no throughput gain —
partition by task index or use a concurrent structure.
</p>
</div>

<!-- Section: Orchestrate vs Scheduler -->
<hr class="section-divider">
<h2 id="orchestrate-lifecycle"><a href="#orchestrate-lifecycle" class="anchor-link" aria-hidden="true">#</a>Standalone vs Scheduler Lifecycle</h2>

<p>
<strong>Standalone <code>orchestrate()</code></strong> runs the full lifecycle on every call:
<code>setup_all()</code> before the FSM, <code>teardown_range(all)</code> after, even on
error. Resources are scoped to one workflow run. This is the per-request model — HTTP
handlers, request-bound jobs.
</p>

<p>
<strong>Scheduler</strong> runs <code>setup_all()</code> exactly once on
<code>scheduler.start()</code> and <code>teardown_range(all)</code> once on
<code>scheduler.stop()</code>. Each scheduled firing calls <code>execute_workflow()</code>
directly and reuses the same resource instances — open a DB pool once, reuse across runs.
</p>

<div class="callout callout-warning">
<div class="callout-label">Per-run state under the Scheduler</div>
<p>
Resources persist across runs. Anything accumulated in a resource (counters, cached state,
the contents of a <code>MemoryStore</code>) carries forward. For per-run reset, clear it at
the start of the workflow — for example, <code>store.clear()?</code> in the initial task.
</p>
</div>

<!-- Section: Workflows Without Resources -->
</div>
