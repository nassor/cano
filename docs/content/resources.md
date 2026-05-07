+++
title = "Resources"
description = "Lifecycle-managed, typed dependency injection for Cano workflows in Rust."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Resources</h1>
<p class="subtitle">Lifecycle-managed, typed dependency injection for Cano workflows.</p>

<p>
<code>Resources&lt;TResourceKey&gt;</code> is the typed dictionary every workflow carries. Database
pools, HTTP clients, config structs, <a href="../store/">MemoryStore</a>, and per-run parameters
are all resources — registered once, injected everywhere. Each lookup returns an
<code>Arc&lt;R&gt;</code>, cheap to clone and safe to share across concurrent split tasks.
</p>

<p>
The engine calls <code>setup()</code> on each registered resource in insertion order before
the FSM runs, and <code>teardown()</code> in reverse order after — even on failure. Tasks
never receive resources directly; they retrieve typed handles from the injected
<code>&amp;Resources&lt;TResourceKey&gt;</code> via <code>res.get::&lt;R, _&gt;(key)?</code>.
</p>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#quick-start">Quick Start</a></li>
<li><a href="#defining">Defining a Resource</a></li>
<li><a href="#building">Building a Resources Map</a></li>
<li><a href="#retrieving">Retrieving Resources in Tasks</a></li>
<li><a href="#from-resources">Declarative Dependency Loading</a></li>
<li><a href="#key-types">Key Types: String vs Enum</a></li>
<li><a href="#lifecycle">Lifecycle Guarantees</a></li>
<li><a href="#concurrency">Concurrency and Interior Mutability</a></li>
<li><a href="#orchestrate-lifecycle">Standalone vs Scheduler Lifecycle</a></li>
<li><a href="#no-resources">Workflows Without Resources</a></li>
<li><a href="#api-reference">API Reference</a></li>
</ol>
</nav>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start</h2>

<p>
Define a resource, register it, retrieve it by typed key in any task. The engine wires
<code>setup</code> / <code>teardown</code> automatically.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> End-to-end example</span>
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
#[derive(Debug, Hash, Eq, PartialEq)]
enum Key { Store, Config }
<!--blank-->
// Stateless resource — derive Resource for a no-op setup/teardown impl
#[derive(Resource)]
struct AppConfig { multiplier: u32 }
<!--blank-->
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Init, Done }
<!--blank-->
struct InitTask;
<!--blank-->
#[task(state = Step, key = Key)]
impl InitTask {
    async fn run(&self, res: &Resources&lt;Key&gt;) -&gt; Result&lt;TaskResult&lt;Step&gt;, CanoError&gt; {
        let store  = res.get::&lt;MemoryStore, _&gt;(&amp;Key::Store)?;
        let config = res.get::&lt;AppConfig, _&gt;(&amp;Key::Config)?;
<!--blank-->
        store.put("value", 10u32 * config.multiplier)?;
        Ok(TaskResult::Single(Step::Done))
    }
}
<!--blank-->
#[tokio::main]
async fn main() -&gt; Result&lt;(), CanoError&gt; {
    let resources = Resources::&lt;Key&gt;::new()
        .insert(Key::Store,  MemoryStore::new())
        .insert(Key::Config, AppConfig { multiplier: 3 });
<!--blank-->
    let workflow = Workflow::new(resources)
        .register(Step::Init, InitTask)
        .add_exit_state(Step::Done);
<!--blank-->
    workflow.orchestrate(Step::Init).await?;
    Ok(())
}</code></pre>
</div>

<!-- Section: Defining a Resource -->
<hr class="section-divider">
<h2 id="defining"><a href="#defining" class="anchor-link" aria-hidden="true">#</a>Defining a Resource</h2>

<p>
The <code>Resource</code> trait gives every dependency two lifecycle hooks. Both default to
no-ops, so most resources need no manual impl.
</p>

<div class="code-block">
<span class="code-block-label">Resource trait</span>
<pre><code class="language-rust">pub trait Resource: Send + Sync + 'static {
    async fn setup(&amp;self) -&gt; Result&lt;(), CanoError&gt; { Ok(()) }
    async fn teardown(&amp;self) -&gt; Result&lt;(), CanoError&gt; { Ok(()) }
}</code></pre>
</div>

<h3>Stateless — <code>#[derive(Resource)]</code></h3>
<p>
Generates an empty <code>impl Resource for T {}</code>. Use it for config, parameter bags,
read-only data — anything without lifecycle work.
</p>

<div class="code-block">
<span class="code-block-label">Stateless resource via derive</span>
<pre><code class="language-rust">#[derive(Resource)]
struct WorkflowParams {
    batch_size: usize,
    timeout_ms: u64,
}</code></pre>
</div>

<h3>Stateful — <code>#[resource]</code> on the impl block</h3>
<p>
Override <code>setup</code> / <code>teardown</code> for resources that open connections,
allocate buffers, or flush state. Both hooks take <code>&amp;self</code>: resources are shared
via <code>Arc</code>, so mutation requires interior mutability —
<code>tokio::sync::Mutex</code>, <code>RwLock</code>, atomics, etc.
</p>

<div class="code-block">
<span class="code-block-label">Resource with custom setup/teardown</span>
<pre><code class="language-rust">use std::sync::{Arc, Mutex};
<!--blank-->
struct CounterResource {
    setup_count: Arc&lt;Mutex&lt;u32&gt;&gt;,
}
<!--blank-->
#[resource]
impl Resource for CounterResource {
    async fn setup(&amp;self) -&gt; Result&lt;(), CanoError&gt; {
        *self.setup_count.lock().unwrap() += 1;
        Ok(())
    }
<!--blank-->
    async fn teardown(&amp;self) -&gt; Result&lt;(), CanoError&gt; {
        // flush, close handles, etc.
        Ok(())
    }
}</code></pre>
</div>

<!-- Section: Building -->
<hr class="section-divider">
<h2 id="building"><a href="#building" class="anchor-link" aria-hidden="true">#</a>Building a Resources Map</h2>

<p>
<code>Resources</code> is a typed builder. <code>insert</code> consumes <code>self</code> and
panics on duplicate keys (programmer error). <code>try_insert</code> returns
<code>Result&lt;Self, CanoError&gt;</code> so dynamic input can handle collisions explicitly.
The two compose in a single chain.
</p>

<div class="code-block">
<span class="code-block-label">insert + try_insert</span>
<pre><code class="language-rust">// Static keys — duplicates indicate buggy wiring; let it panic
let resources = Resources::new()
    .insert("store",  MemoryStore::new())
    .insert("config", AppConfig::default())
    // Dynamic key — handle collision as data
    .try_insert(user_supplied_key, plugin)?;</code></pre>
</div>

<!-- Section: Retrieving -->
<hr class="section-divider">
<h2 id="retrieving"><a href="#retrieving" class="anchor-link" aria-hidden="true">#</a>Retrieving Resources in Tasks</h2>

<p>
<code>get::&lt;R, Q&gt;(key)</code> returns <code>Result&lt;Arc&lt;R&gt;, CanoError&gt;</code>.
<code>R</code> names the resource type; <code>Q</code> is the key query type — use
<code>_</code> to let the compiler infer it from the key argument.
</p>

<div class="code-block">
<span class="code-block-label">Recommended form — Q inferred</span>
<pre><code class="language-rust">// String keys
let store  = res.get::&lt;MemoryStore, _&gt;("store")?;
let params = res.get::&lt;WorkflowParams, _&gt;("params")?;
<!--blank-->
// Enum keys
let store  = res.get::&lt;MemoryStore, _&gt;(&amp;Key::Store)?;
let params = res.get::&lt;WorkflowParams, _&gt;(&amp;Key::Params)?;</code></pre>
</div>

<table class="styled-table">
<thead>
<tr><th>Error variant</th><th>Meaning</th></tr>
</thead>
<tbody>
<tr><td><code>ResourceNotFound</code></td><td>No entry for the key. The resource was not inserted.</td></tr>
<tr><td><code>ResourceTypeMismatch</code></td><td>Key present, but stored under a different type than <code>R</code>.</td></tr>
<tr><td><code>ResourceDuplicateKey</code></td><td>Returned by <code>try_insert</code> when the key is already present.</td></tr>
</tbody>
</table>

<!-- Section: FromResources -->
<hr class="section-divider">
<h2 id="from-resources"><a href="#from-resources" class="anchor-link" aria-hidden="true">#</a>Declarative Loading with <code>#[derive(FromResources)]</code></h2>

<p>
A struct of <code>Arc</code> dependencies can declare its lookups instead of calling
<code>res.get</code> field-by-field. The derive generates
<code>fn from_resources(res: &amp;Resources&lt;K&gt;) -&gt; CanoResult&lt;Self&gt;</code>.
</p>

<p>
Field-level <code>#[res(...)]</code> declares the lookup key — string literal for string-keyed
maps, enum path for enum-keyed maps. Container-level
<code>#[from_resources(key = MyKey)]</code> sets the key type when fields use enum keys.
</p>

<div class="code-block">
<span class="code-block-label">Deps struct with enum keys</span>
<pre><code class="language-rust">#[derive(Hash, Eq, PartialEq)]
enum Key { Store, Config }
<!--blank-->
#[derive(FromResources)]
#[from_resources(key = Key)]
struct InitDeps {
    #[res(Key::Store)]
    store: Arc&lt;MemoryStore&gt;,
    #[res(Key::Config)]
    config: Arc&lt;AppConfig&gt;,
}
<!--blank-->
// In a task:
let InitDeps { store, config } = InitDeps::from_resources(res)?;</code></pre>
</div>

<!-- Section: Key Types -->
<hr class="section-divider">
<h2 id="key-types"><a href="#key-types" class="anchor-link" aria-hidden="true">#</a>Key Types: String vs Enum</h2>

<p>
<code>TResourceKey</code> defaults to <code>Cow&lt;'static, str&gt;</code>:
<code>&amp;'static str</code> literals stay borrowed (no allocation), owned <code>String</code>s
are accepted, lookups go through <code>&amp;str</code>. Any type satisfying
<code>Hash + Eq + Send + Sync + 'static</code> works — an enum is recommended for non-trivial
workflows.
</p>

<table class="styled-table">
<thead>
<tr><th></th><th>String keys (default)</th><th>Enum keys</th></tr>
</thead>
<tbody>
<tr><td>Construction</td><td><code>Resources::new()</code></td><td><code>Resources::&lt;Key&gt;::new()</code></td></tr>
<tr><td>Typo detection</td><td>Runtime — <code>ResourceNotFound</code></td><td>Compile time</td></tr>
<tr><td>Allocation on lookup</td><td>None for <code>&amp;str</code> literals</td><td>None</td></tr>
<tr><td>Best for</td><td>Prototypes, small workflows</td><td>Production, larger workflows</td></tr>
</tbody>
</table>

<!-- Section: Lifecycle -->
<hr class="section-divider">
<h2 id="lifecycle"><a href="#lifecycle" class="anchor-link" aria-hidden="true">#</a>Lifecycle Guarantees</h2>

<div class="mermaid">
sequenceDiagram
participant E as Engine
participant R0 as Resource A
participant R1 as Resource B
participant R2 as Resource C
Note over E: setup_all() — FIFO
E->>R0: setup()
E->>R1: setup()
E->>R2: setup()
Note over E: execute_workflow()
Note over E: teardown_range(all) — LIFO
E->>R2: teardown()
E->>R1: teardown()
E->>R0: teardown()
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
<pre><code class="language-rust">let resources = Resources::new()
    .insert("a", ServiceA::new())  // position 0 — setup OK
    .insert("b", ServiceB::new())  // position 1 — setup OK
    .insert("c", ServiceC::new())  // position 2 — setup FAILS
    .insert("d", ServiceD::new()); // position 3 — never reached
<!--blank-->
// setup_all() returns Err from C; teardown runs for B then A (LIFO)
let result = resources.setup_all().await;
assert!(result.is_err());</code></pre>
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
<hr class="section-divider">
<h2 id="no-resources"><a href="#no-resources" class="anchor-link" aria-hidden="true">#</a>Workflows Without Resources</h2>

<p>
When every task is self-contained, skip the resources map. Implement <code>run_bare()</code>
instead of <code>run()</code>, and build the workflow with <code>Workflow::bare()</code>
(equivalent to <code>Workflow::new(Resources::empty())</code>).
</p>

<div class="code-block">
<span class="code-block-label">Resource-free workflow</span>
<pre><code class="language-rust">#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Compute, Done }
<!--blank-->
struct PureTask;
<!--blank-->
#[task(state = Step)]
impl PureTask {
    async fn run_bare(&amp;self) -&gt; Result&lt;TaskResult&lt;Step&gt;, CanoError&gt; {
        let answer = 40 + 2;
        println!("Computed: {answer}");
        Ok(TaskResult::Single(Step::Done))
    }
}
<!--blank-->
let workflow = Workflow::bare()
    .register(Step::Compute, PureTask)
    .add_exit_state(Step::Done);</code></pre>
</div>

<!-- Section: API Reference -->
<hr class="section-divider">
<h2 id="api-reference"><a href="#api-reference" class="anchor-link" aria-hidden="true">#</a>API Reference</h2>

<table class="styled-table">
<thead>
<tr><th>Method</th><th>Signature</th><th>Notes</th></tr>
</thead>
<tbody>
<tr><td><code>new()</code></td><td><code>Resources::new() -&gt; Self</code></td><td>Empty map.</td></tr>
<tr><td><code>empty()</code></td><td><code>Resources::empty() -&gt; Self</code></td><td>Alias for <code>new()</code>.</td></tr>
<tr><td><code>with_capacity(n)</code></td><td><code>Resources::with_capacity(n) -&gt; Self</code></td><td>Pre-allocates the underlying <code>HashMap</code>.</td></tr>
<tr><td><code>insert</code></td><td><code>insert&lt;R: Resource&gt;(self, key, resource) -&gt; Self</code></td><td>Builder; <strong>panics</strong> on duplicate.</td></tr>
<tr><td><code>try_insert</code></td><td><code>try_insert&lt;R&gt;(self, key, resource) -&gt; Result&lt;Self, CanoError&gt;</code></td><td>Returns <code>ResourceDuplicateKey</code> on duplicate.</td></tr>
<tr><td><code>get</code></td><td><code>get&lt;R, Q&gt;(&amp;self, key: &amp;Q) -&gt; Result&lt;Arc&lt;R&gt;, CanoError&gt;</code></td><td><code>ResourceNotFound</code> / <code>ResourceTypeMismatch</code>.</td></tr>
<tr><td><code>setup_all()</code></td><td><code>async fn setup_all(&amp;self) -&gt; Result&lt;(), CanoError&gt;</code></td><td>FIFO setup with partial LIFO rollback. Engine calls this.</td></tr>
<tr><td><code>teardown_all()</code></td><td><code>async fn teardown_all(&amp;self)</code></td><td>LIFO teardown. Errors logged, never propagated.</td></tr>
</tbody>
</table>

</div>
