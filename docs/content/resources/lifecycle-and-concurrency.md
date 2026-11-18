+++
title = "Resource lifecycle and concurrency"
description = "When Cano resource setup()/teardown() run, how resources behave across concurrent splits, and the difference between standalone and scheduler lifecycles."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Resource lifecycle and concurrency</h1>
<p class="subtitle">When setup/teardown fire, and how resources behave under concurrency.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.10.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>

<p>
This page explains when resources are set up and torn down and how they behave under concurrency.
<a href="../">Resources</a> covers defining and retrieving them.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#lifecycle">Lifecycle guarantees</a></li>
<li><a href="#concurrency">Concurrency and interior mutability</a></li>
<li><a href="#orchestrate-lifecycle">Standalone and scheduler lifecycles</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="lifecycle"><a href="#lifecycle" class="anchor-link" aria-hidden="true">#</a>Lifecycle guarantees</h2>

<div class="diagram-frame">
<p class="diagram-label">setup_all() in insertion order, teardown in reverse order</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 750 496" role="img" aria-labelledby="life-order-title life-order-desc">
<title id="life-order-title">Setup in insertion order, teardown in reverse order</title>
<desc id="life-order-desc">Three resources A, B and C, inserted in that order. The engine's setup_all() calls setup on A, then B, then C. It then runs the state machine. teardown_all() calls teardown on C, then B, then A. Each resource's live span nests inside the spans of the resources set up before it.</desc>
<defs><marker id="life-order-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
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
<text class="t-strong" x="110" y="89">insertion order</text>
<path class="e" d="M124,126 H293" marker-end="url(#life-order-ah)"/>
<text class="t-code ta-s" x="142" y="113">setup()</text>
<circle class="n" cx="110" cy="126" r="10"/>
<text class="t-mut" x="110" y="127">1</text>
<path class="e" d="M124,170 H473" marker-end="url(#life-order-ah)"/>
<text class="t-code ta-s" x="142" y="157">setup()</text>
<circle class="n" cx="110" cy="170" r="10"/>
<text class="t-mut" x="110" y="171">2</text>
<path class="e" d="M124,214 H653" marker-end="url(#life-order-ah)"/>
<text class="t-code ta-s" x="142" y="201">setup()</text>
<circle class="n" cx="110" cy="214" r="10"/>
<text class="t-mut" x="110" y="215">3</text>
<rect class="n" x="22" y="238" width="176" height="34" rx="10"/>
<text class="t-code" x="110" y="256">run the FSM</text>
<rect class="n-hot" x="22" y="286" width="176" height="44" rx="10"/>
<text class="t-code" x="110" y="300">teardown_all()</text>
<text class="t-strong t-hot" x="110" y="317">reverse order</text>
<path class="e" d="M124,354 H653" marker-end="url(#life-order-ah)"/>
<text class="t-code ta-s" x="142" y="341">teardown()</text>
<circle class="n" cx="110" cy="354" r="10"/>
<text class="t-mut" x="110" y="355">4</text>
<path class="e" d="M124,398 H473" marker-end="url(#life-order-ah)"/>
<text class="t-code ta-s" x="142" y="385">teardown()</text>
<circle class="n" cx="110" cy="398" r="10"/>
<text class="t-mut" x="110" y="399">5</text>
<path class="e" d="M124,442 H293" marker-end="url(#life-order-ah)"/>
<text class="t-code ta-s" x="142" y="429">teardown()</text>
<circle class="n" cx="110" cy="442" r="10"/>
<text class="t-mut" x="110" y="443">6</text>
<text class="t-mut" x="375" y="478">Bars mark each resource's live span. They nest, so teardown mirrors setup exactly.</text>
</svg>
</div>
</div>

<table class="styled-table">
<thead>
<tr><th>Guarantee</th><th>Detail</th></tr>
</thead>
<tbody>
<tr><td>Setup order</td><td>Insertion order. Insert dependencies before dependents.</td></tr>
<tr><td>Teardown order</td><td>Reverse insertion order, so each teardown can rely on its dependencies still being live.</td></tr>
<tr><td>Sequentiality</td><td>Setup and teardown calls are sequential, never concurrent.</td></tr>
<tr><td>Partial rollback</td><td>If <code>setup</code> fails at position N, teardown runs in reverse from N&minus;1 to 0. Resources at positions N and later are never set up and never torn down.</td></tr>
<tr><td>Teardown errors</td><td>Logged. Every remaining resource is still torn down. <code>orchestrate</code> and <code>resume_from</code> return the first teardown error only when the run itself succeeded; a run error always wins.</td></tr>
</tbody>
</table>

<div class="code-block">
<span class="code-block-label">Partial rollback: only A and B are torn down</span>

```rust
use cano::prelude::*;
use std::sync::{Arc, Mutex};

struct TrackedService {
    name: &'static str,
    fail_setup: bool,
    events: Arc<Mutex<Vec<String>>>,
}

#[resource]
impl Resource for TrackedService {
    async fn setup(&self) -> Result<(), CanoError> {
        self.events.lock().unwrap().push(format!("setup:{}", self.name));
        if self.fail_setup {
            // Failing setup stops initialization and skips teardown for this resource.
            return Err(CanoError::configuration(format!("{} failed setup", self.name)));
        }
        Ok(())
    }

    async fn teardown(&self) -> Result<(), CanoError> {
        self.events.lock().unwrap().push(format!("teardown:{}", self.name));
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let events = Arc::new(Mutex::new(Vec::new()));

    // Insert named instances so all four share the same type in insertion order.
    let resources = Resources::new()
        .insert_named("A", TrackedService { name: "A", fail_setup: false, events: events.clone() }) // position 0: setup OK
        .insert_named("B", TrackedService { name: "B", fail_setup: false, events: events.clone() }) // position 1: setup OK
        .insert_named("C", TrackedService { name: "C", fail_setup: true,  events: events.clone() }) // position 2: setup FAILS
        .insert_named("D", TrackedService { name: "D", fail_setup: false, events: events.clone() }); // position 3: never reached

    // Workflow::orchestrate runs setup_all() automatically before starting the FSM.
    // When C fails, setup_all() tears down already-initialized entries in reverse order (B, then A).
    let result = resources.setup_all().await;
    assert!(result.is_err());
    assert_eq!(
        *events.lock().unwrap(),
        ["setup:A", "setup:B", "setup:C", "teardown:B", "teardown:A"]
    );
}
```
</div>

<!-- Section: Concurrency -->
<hr class="section-divider">
<h2 id="concurrency"><a href="#concurrency" class="anchor-link" aria-hidden="true">#</a>Concurrency and interior mutability</h2>

<p>
<code>get()</code> returns a <code>&amp;R</code> borrowed from the <code>Resources</code>, and
<code>get_arc()</code> returns an <code>Arc&lt;R&gt;</code>. Split tasks share one resource instance through the
same <code>&amp;Resources</code>, and moving a handle into a spawned task with <code>get_arc</code> is a refcount
bump, not a data copy. Read-only resources need no synchronization; resources with mutable state need interior mutability.
</p>

<table class="styled-table">
<thead>
<tr><th>Primitive</th><th>When to use</th></tr>
</thead>
<tbody>
<tr><td><code>tokio::sync::RwLock&lt;T&gt;</code></td><td>Many readers, rare writers. Best default.</td></tr>
<tr><td><code>tokio::sync::Mutex&lt;T&gt;</code></td><td>Exclusive access. Serializes split tasks, so watch throughput.</td></tr>
<tr><td><code>std::sync::atomic::*</code></td><td>Counters and flags. Lock-free.</td></tr>
<tr><td><code>DashMap&lt;K, V&gt;</code></td><td>Concurrent map writes. Sharded locking.</td></tr>
</tbody>
</table>

<div class="callout callout-warning">
<div class="callout-label">Coarse locks remove parallelism</div>
<p>
A single <code>Mutex&lt;T&gt;</code> around a whole resource serializes every split task that writes to it.
If all splits hit the same resource, parallelism gives no throughput gain. Partition by task index or use a
concurrent structure.
</p>
</div>

<!-- Section: Orchestrate vs Scheduler -->
<hr class="section-divider">
<h2 id="orchestrate-lifecycle"><a href="#orchestrate-lifecycle" class="anchor-link" aria-hidden="true">#</a>Standalone and scheduler lifecycles</h2>

<p>
Standalone <code>orchestrate()</code> and <code>resume_from()</code> run the full lifecycle on every call:
<code>setup_all()</code> before the machine and <code>teardown_all()</code> after it, even on error.
Resources are scoped to one run, which suits per-request work such as HTTP handlers.
</p>

<p>
The scheduler runs <code>setup_all()</code> for every flow once, in <code>Scheduler::start()</code>, and
<code>teardown_all()</code> once in <code>RunningScheduler::stop()</code>. A second <code>stop()</code> returns the
same result without tearing down again. Each scheduled run executes the machine directly and reuses the same
resource instances, so a DB pool opens once and serves every run.
</p>

<div class="callout callout-warning">
<div class="callout-label">Per-run state under the scheduler</div>
<p>
Resources persist across runs, so anything accumulated in one carries forward: counters, cached state, the contents
of a <code>Blackboard</code>. To reset per run, clear it at the start of the workflow, for example with
<code>board.clear()</code> in the initial task.
</p>
</div>

<!-- Section: Workflows Without Resources -->
</div>
