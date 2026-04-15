+++
title = "Resources"
description = "Learn how to use Resources in Cano - lifecycle-managed, typed dependencies for async workflows in Rust."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Resources</h1>
<p class="subtitle">Lifecycle-managed, typed dependency injection for Cano workflows.</p>

<p>
<code>Resources&lt;TResourceKey&gt;</code> is the typed dictionary that every workflow carries.
Before execution, the engine calls <code>setup()</code> on each registered resource in insertion order.
After execution (or on failure), it calls <code>teardown()</code> in reverse order. Tasks and nodes
retrieve typed handles via <code>res.get::&lt;R, Q&gt;(key)?</code> and receive an
<code>Arc&lt;R&gt;</code> they can use independently in parallel splits.
</p>

<p>
This replaces the old <code>TStore</code> + <code>TParams</code> dual-generic approach.
Database pools, HTTP clients, configuration structs, <a href="../store/">MemoryStore</a>, and plain
parameter objects are all resources — registered once, injected everywhere.
</p>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
Resources are not passed to tasks directly. They are injected by the workflow engine as a
shared <code>&amp;Resources&lt;TResourceKey&gt;</code>. Each call to
<code>res.get::&lt;R, Q&gt;(key)?</code> returns an <code>Arc&lt;R&gt;</code> — cheap to
clone and safe to share across concurrent split tasks. See <a href="../workflows/">Workflows</a>
for how the engine uses resources, and <a href="../task/">Tasks</a> / <a href="../nodes/">Nodes</a>
for the <code>run</code> / <code>prep</code> / <code>post</code> signatures that receive
<code>&amp;Resources</code>.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#derive-macros">Derive Macros: <code>#[derive(Resource)]</code> and <code>#[derive(FromResources)]</code></a></li>
<li><a href="#resource-trait">The Resource Trait</a></li>
<li><a href="#resources-container">Resources Container</a></li>
<li><a href="#key-types">Key Types: String vs Enum</a></li>
<li><a href="#lifecycle">Lifecycle Guarantees</a></li>
<li><a href="#params-as-resources">Params as Resources</a></li>
<li><a href="#concurrency">Concurrency and Interior Mutability</a></li>
<li><a href="#orchestrate-lifecycle">Standalone vs Scheduler Lifecycle</a></li>
<li><a href="#no-resources">Workflows Without Resources</a></li>
<li><a href="#api-reference">API Reference</a></li>
</ol>
</nav>

<!-- Section: Derive Macros -->
<hr class="section-divider">
<h2 id="derive-macros"><a href="#derive-macros" class="anchor-link" aria-hidden="true">#</a>Derive Macros: <code>#[derive(Resource)]</code> and <code>#[derive(FromResources)]</code></h2>
<p>
Two derive macros eliminate the most common boilerplate when working with resources.
</p>

<div class="card-stack">
<div class="card">
<h3><code>#[derive(Resource)]</code> — stateless resources</h3>
<p>
Generates an empty <code>impl Resource for T {}</code>, using the trait's no-op <code>setup</code>
and <code>teardown</code> defaults. Use it on any struct that does not need lifecycle hooks —
configuration types, parameter bags, read-only data.
</p>
<div class="code-block">
<span class="code-block-label">Before and after</span>
<pre><code class="language-rust">// Before: three lines of boilerplate
#[resource]
impl Resource for WorkflowParams {}

// After: one attribute
#[derive(Resource)]
struct WorkflowParams {
    batch_size: usize,
    timeout_ms: u64,
}</code></pre>
</div>
</div>
<div class="card">
<h3><code>#[derive(FromResources)]</code> — declarative dependency loading</h3>
<p>
Generates a <code>from_resources(res: &amp;Resources&lt;K&gt;) -&gt; CanoResult&lt;Self&gt;</code>
constructor. Each field must be <code>Arc&lt;T&gt;</code>. The field-level <code>#[res("key")]</code>
attribute (string literal) or <code>#[res(Enum::Variant)]</code> (enum path) declares the
lookup key. Container-level <code>#[from_resources(key = MyKeyType)]</code> overrides the
key type when all fields use the same key type.
</p>
<div class="code-block">
<span class="code-block-label">Deps struct with string keys</span>
<pre><code class="language-rust">use cano::prelude::*;
use std::sync::Arc;
<!--blank-->
#[derive(FromResources)]
struct MyDeps {
    #[res("store")]
    store: Arc&lt;MemoryStore&gt;,
    #[res("config")]
    config: Arc&lt;AppConfig&gt;,
}
<!--blank-->
// In prep / run / post:
let MyDeps { store, config } = MyDeps::from_resources(res)?;</code></pre>
</div>
</div>
</div>

<div class="callout callout-info">
<div class="callout-label">Prelude</div>
<p>
Both macros are re-exported through <code>cano::prelude::*</code> — no separate
<code>use</code> statement needed. <code>#[derive(Resource)]</code> is available as
<code>ResourceDerive</code> in the prelude type namespace to avoid collision with the
<code>Resource</code> trait; the derive name <code>Resource</code> still works in
<code>#[derive(...)]</code> position.
</p>
</div>

<!-- Section: Resource Trait -->
<hr class="section-divider">
<h2 id="resource-trait"><a href="#resource-trait" class="anchor-link" aria-hidden="true">#</a>The Resource Trait</h2>
<p>
Implement <code>Resource</code> for any type your workflow depends on. Both methods default to
no-ops, so a plain struct with no lifecycle logic needs only an empty impl.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Resource trait definition</span>
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
#[resource]
pub trait Resource: Send + Sync + 'static {
    async fn setup(&self) -> Result&lt;(), CanoError&gt; { Ok(()) }
    async fn teardown(&self) -> Result&lt;(), CanoError&gt; { Ok(()) }
}</code></pre>
</div>

<p>
Both hooks take <code>&amp;self</code>. This is intentional: resources are shared via
<code>Arc</code> across concurrent split tasks, so <code>&amp;mut self</code> teardown is not
possible without a lock. Resources that need to mutate during <code>setup</code> or
<code>teardown</code> (for example, opening a connection and storing its handle, or flushing a
write buffer) must use interior mutability — <code>tokio::sync::Mutex</code>, <code>RwLock</code>,
<code>AtomicBool</code>, etc.
</p>

<div class="card-stack">
<div class="card">
<h3>Plain struct — no lifecycle</h3>
<p>Use <code>#[derive(Resource)]</code> for structs that need no lifecycle hooks. It generates the empty <code>impl Resource for T {}</code> automatically.</p>
<div class="code-block">
<span class="code-block-label">No-op Resource via derive</span>
<pre><code class="language-rust">// #[derive(Resource)] generates an empty impl Resource for WorkflowParams {}
#[derive(Resource)]
struct WorkflowParams {
    batch_size: usize,
    timeout_ms: u64,
}</code></pre>
</div>
</div>
<div class="card">
<h3>Non-trivial lifecycle with interior mutability</h3>
<p>Override <code>setup</code> / <code>teardown</code> for resources that need initialization or cleanup. Use interior mutability to mutate state through <code>&amp;self</code>.</p>
<div class="code-block">
<span class="code-block-label">Resource with setup/teardown</span>
<pre><code class="language-rust">use std::sync::{Arc, Mutex};
<!--blank-->
struct CounterResource {
    setup_count: Arc&lt;Mutex&lt;u32&gt;&gt;,
}
<!--blank-->
#[resource]
impl Resource for CounterResource {
    async fn setup(&self) -> Result&lt;(), CanoError&gt; {
        *self.setup_count.lock().unwrap() += 1;
        println!("CounterResource: setup called");
        Ok(())
    }
<!--blank-->
    async fn teardown(&self) -> Result&lt;(), CanoError&gt; {
        println!("CounterResource: teardown called");
        Ok(())
    }
}</code></pre>
</div>
</div>
</div>

<!-- Section: Resources Container -->
<hr class="section-divider">
<h2 id="resources-container"><a href="#resources-container" class="anchor-link" aria-hidden="true">#</a>Resources Container</h2>
<p>
<code>Resources&lt;TResourceKey&gt;</code> is a typed map built with a builder pattern.
The default key type is <code>Cow&lt;'static, str&gt;</code>: <code>&amp;'static str</code> literal
keys stay borrowed (no allocation), owned <code>String</code> keys are accepted via
<code>Cow::Owned</code>, and lookups use <code>&amp;str</code> through the
<code>Borrow&lt;str&gt;</code> blanket impl.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128204;</span> Building a Resources map</span>
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
let store = MemoryStore::new();
<!--blank-->
// Builder pattern — each insert() consumes self and returns a new Resources
let resources = Resources::new()
    .insert("store", store.clone())
    .insert("params", WorkflowParams { batch_size: 256, timeout_ms: 5000 });
<!--blank-->
// Pass to Workflow::new()
let workflow = Workflow::new(resources)
    .register(State::Start, MyTask)
    .add_exit_state(State::Done);</code></pre>
</div>

<h3>Two ways to insert: <code>insert</code> vs <code>try_insert</code></h3>
<p>
<code>Resources</code> exposes two insertion methods. They differ only in how they handle a
duplicate key — pick whichever matches whether duplicates are a programmer bug or a runtime
condition you want to handle.
</p>

<div class="comparison-grid comparison-grid-stacked">
<div class="comparison-col">
<h3><code>insert</code> — infallible, panics on duplicate</h3>
<p>
The default builder method. Returns <code>Self</code>, so chaining stays clean. A duplicate
key is treated as a programmer error and triggers a panic at startup, before any task runs.
Use this when keys are statically known and a duplicate would mean the wiring code is wrong.
</p>
<pre><code class="language-rust">let resources = Resources::new()
    .insert("store", MemoryStore::new())
    .insert("config", AppConfig::default());
// Panics if "store" or "config" was already inserted.</code></pre>
</div>
<div class="comparison-col">
<h3><code>try_insert</code> — returns <code>Result</code></h3>
<p>
Same body as <code>insert</code> but returns
<code>Result&lt;Self, CanoError&gt;</code>; on duplicate it yields
<code>CanoError::ResourceDuplicateKey</code>. Use this when keys come from dynamic input
(config files, user data, plugins) and you want to handle collisions explicitly.
</p>
<pre><code class="language-rust">let resources = Resources::new()
    .insert("store", MemoryStore::new())
    .try_insert(user_supplied_key, plugin)?;
// Returns Err(ResourceDuplicateKey) instead of panicking.</code></pre>
</div>
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
The two methods can be mixed in the same chain — <code>insert</code> takes <code>Self</code>
and <code>try_insert</code> takes <code>Self</code>, so call <code>try_insert</code> only on
the steps where dynamic input is involved.
</p>
</div>

<h3>Retrieving resources inside a task</h3>
<p>
<code>get::&lt;R, Q&gt;(key)</code> returns <code>Result&lt;Arc&lt;R&gt;, CanoError&gt;</code>.
The two-parameter form explicitly names the resource type <code>R</code> and the key query type
<code>Q</code>. For <code>String</code>-keyed resources, use <code>str</code> as the query type
so a <code>&amp;str</code> literal is accepted.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128274;</span> get() call forms</span>
<pre><code class="language-rust">// String-keyed resources — use str as Q so &str literals work
let store = res.get::&lt;MemoryStore, str&gt;("store")?;
let params = res.get::&lt;WorkflowParams, str&gt;("params")?;
<!--blank-->
// Enum-keyed resources — use the enum type as Q
let store = res.get::&lt;MemoryStore, Key&gt;(&amp;Key::Store)?;
let params = res.get::&lt;WorkflowParams, Key&gt;(&amp;Key::Params)?;</code></pre>
</div>

<h3>Error variants</h3>
<div class="card-grid error-cards">
<div class="card">
<h3>ResourceNotFound</h3>
<p>No entry exists for the given key. Check that the resource was inserted before building the workflow.</p>
</div>
<div class="card">
<h3>ResourceTypeMismatch</h3>
<p>An entry exists under the key but was stored as a different type. Ensure the type annotation in <code>get::&lt;R, Q&gt;</code> matches what was passed to <code>insert()</code>.</p>
</div>
<div class="card">
<h3>ResourceDuplicateKey</h3>
<p>Returned by <code>try_insert()</code> when the key is already present. The infallible <code>insert()</code> panics on the same condition; switch to <code>try_insert()</code> to handle collisions as data.</p>
</div>
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
These two errors are distinct, not both <code>None</code>. If you get <code>ResourceNotFound</code>,
the key is missing. If you get <code>ResourceTypeMismatch</code>, the key is present but the type
annotation is wrong. The error messages include enough context to distinguish them in logs.
</p>
</div>

<!-- Section: Key Types -->
<hr class="section-divider">
<h2 id="key-types"><a href="#key-types" class="anchor-link" aria-hidden="true">#</a>Key Types: String vs Enum</h2>
<p>
<code>TResourceKey</code> defaults to <code>String</code> but can be any type satisfying
<code>Hash + Eq + Send + Sync + 'static</code>. An enum is the recommended choice for
non-trivial workflows: typos in key strings are runtime panics; typos in enum variants are
compile errors.
</p>

<div class="comparison-grid comparison-grid-stacked">
<div class="comparison-col">
<h3>String keys (default)</h3>
<p><strong>Best for:</strong> Quick prototypes, examples, workflows with few resources.</p>
<ul>
<li><code>&amp;str</code> literals work via <code>Borrow&lt;str&gt;</code></li>
<li>Keys only checked at runtime</li>
<li>Works with <code>Resources::new()</code> (no type annotation needed)</li>
</ul>
<div class="code-block">
<span class="code-block-label">String keys</span>
<pre><code class="language-rust">let resources = Resources::new()
    .insert("store", MemoryStore::new())
    .insert("config", AppConfig::default());
<!--blank-->
// In a task:
let store = res.get::&lt;MemoryStore, str&gt;("store")?;</code></pre>
</div>
</div>
<div class="comparison-col">
<h3>Enum keys (recommended)</h3>
<p><strong>Best for:</strong> Production code, larger workflows, shared resource registries.</p>
<ul>
<li>Typos caught at compile time</li>
<li>No string allocation on lookup</li>
<li>Requires <code>Resources::&lt;Key&gt;::new()</code> or type annotation</li>
</ul>
<div class="code-block">
<span class="code-block-label">Enum keys</span>
<pre><code class="language-rust">#[derive(Hash, Eq, PartialEq)]
enum Key { Store, Config, Params }
<!--blank-->
let resources = Resources::&lt;Key&gt;::new()
    .insert(Key::Store, MemoryStore::new())
    .insert(Key::Config, AppConfig::default());
<!--blank-->
// In a task:
let store = res.get::&lt;MemoryStore, Key&gt;(&amp;Key::Store)?;</code></pre>
</div>
</div>
</div>

<!-- Section: Lifecycle Guarantees -->
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
<tr>
<th>Guarantee</th>
<th>Detail</th>
</tr>
</thead>
<tbody>
<tr>
<td>Setup order</td>
<td>FIFO — resources are set up in insertion order. Insert dependencies before dependents.</td>
</tr>
<tr>
<td>Teardown order</td>
<td>LIFO — reverse of setup order, so each teardown can safely assume its dependencies are still live.</td>
</tr>
<tr>
<td>Sequentiality</td>
<td>Both <code>setup</code> and <code>teardown</code> calls are sequential, never concurrent. This avoids ordering races between resources that depend on each other.</td>
</tr>
<tr>
<td>Partial rollback on setup failure</td>
<td>If <code>setup</code> fails at position N, <code>teardown</code> is called LIFO from position N−1 down to 0. Resources at positions ≥ N never ran <code>setup</code> and are never torn down.</td>
</tr>
<tr>
<td>Teardown scope</td>
<td><code>teardown</code> is only called on resources whose <code>setup</code> returned <code>Ok</code>. A resource whose <code>setup</code> returned <code>Err</code> is considered uninitialized and is skipped.</td>
</tr>
<tr>
<td>Teardown errors</td>
<td>Errors from <code>teardown</code> are logged (or printed to stderr) but never abort the teardown sequence. All remaining resources continue to be torn down even if one fails.</td>
</tr>
</tbody>
</table>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Partial rollback — only A and B get teardown; C failed setup, D was never reached</span>
<pre><code class="language-rust">let resources = Resources::new()
    .insert("a", ServiceA::new())  // position 0 — setup OK
    .insert("b", ServiceB::new())  // position 1 — setup OK
    .insert("c", ServiceC::new())  // position 2 — setup FAILS
    .insert("d", ServiceD::new()); // position 3 — never reached
<!--blank-->
// setup_all() returns Err from C
// teardown is called for B (position 1), then A (position 0) — LIFO
// C and D are never torn down
let result = resources.setup_all().await;
assert!(result.is_err());</code></pre>
</div>

<!-- Section: Params as Resources -->
<hr class="section-divider">
<h2 id="params-as-resources"><a href="#params-as-resources" class="anchor-link" aria-hidden="true">#</a>Params as Resources</h2>
<p>
Workflow configuration and per-run parameters are resources. There is no separate
<code>TParams</code> generic or <code>set_params()</code> call. Insert a plain struct under any
key and retrieve it in tasks using <code>res.get()</code> — the default no-op lifecycle adds no
overhead.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9881;</span> Complete example with enum keys and params as a resource</span>
<pre><code class="language-rust">use cano::prelude::*;
use std::sync::{Arc, Mutex};
<!--blank-->
#[derive(Debug, Hash, Eq, PartialEq)]
enum Key { Store, Counter, Params }
<!--blank-->
// #[derive(Resource)] generates a no-op impl — no lifecycle needed
#[derive(Resource)]
struct WorkflowParams {
    multiplier: u32,
}
<!--blank-->
// Resource with non-trivial lifecycle — use #[resource] on the impl block
struct CounterResource {
    setup_count: Arc&lt;Mutex&lt;u32&gt;&gt;,
}
<!--blank-->
impl CounterResource {
    fn new() -&gt; Self {
        Self { setup_count: Arc::new(Mutex::new(0)) }
    }
<!--blank-->
    fn get_setup_count(&amp;self) -&gt; u32 {
        *self.setup_count.lock().unwrap()
    }
}
<!--blank-->
#[resource]
impl Resource for CounterResource {
    async fn setup(&amp;self) -&gt; Result&lt;(), CanoError&gt; {
        *self.setup_count.lock().unwrap() += 1;
        Ok(())
    }
}
<!--blank-->
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Init, Process, Done }
<!--blank-->
struct InitTask;
<!--blank-->
#[task(state = Step, key = Key)]
impl InitTask {
    async fn run(&amp;self, res: &amp;Resources&lt;Key&gt;) -&gt; Result&lt;TaskResult&lt;Step&gt;, CanoError&gt; {
        let store  = res.get::&lt;MemoryStore, Key&gt;(&amp;Key::Store)?;
        let params = res.get::&lt;WorkflowParams, Key&gt;(&amp;Key::Params)?;
<!--blank-->
        let value = 10u32 * params.multiplier;
        store.put("value", value)?;
        Ok(TaskResult::Single(Step::Process))
    }
}
<!--blank-->
struct ProcessTask;
<!--blank-->
#[task(state = Step, key = Key)]
impl ProcessTask {
    async fn run(&amp;self, res: &amp;Resources&lt;Key&gt;) -&gt; Result&lt;TaskResult&lt;Step&gt;, CanoError&gt; {
        let store   = res.get::&lt;MemoryStore, Key&gt;(&amp;Key::Store)?;
        let counter = res.get::&lt;CounterResource, Key&gt;(&amp;Key::Counter)?;
<!--blank-->
        let value: u32 = store.get("value")?;
        let result = value + counter.get_setup_count();
        store.put("result", result)?;
        Ok(TaskResult::Single(Step::Done))
    }
}
<!--blank-->
#[tokio::main]
async fn main() -&gt; Result&lt;(), CanoError&gt; {
    let store = MemoryStore::new();
<!--blank-->
    let resources = Resources::&lt;Key&gt;::new()
        .insert(Key::Store,   store.clone())
        .insert(Key::Counter, CounterResource::new())
        .insert(Key::Params,  WorkflowParams { multiplier: 3 });
<!--blank-->
    let workflow = Workflow::new(resources)
        .register(Step::Init, InitTask)
        .register(Step::Process, ProcessTask)
        .add_exit_state(Step::Done);
<!--blank-->
    workflow.orchestrate(Step::Init).await?;
<!--blank-->
    let result: u32 = store.get("result")?;
    println!("Result: {result}"); // 31 (10 * 3 + 1 setup call)
    Ok(())
}</code></pre>
</div>

<!-- Section: Concurrency -->
<hr class="section-divider">
<h2 id="concurrency"><a href="#concurrency" class="anchor-link" aria-hidden="true">#</a>Concurrency and Interior Mutability</h2>
<p>
<code>get()</code> returns an <code>Arc&lt;R&gt;</code>. Split tasks that run in parallel each
receive their own <code>Arc</code> clone — a cheap atomic reference-count increment with no
data copy. The underlying resource instance is shared.
</p>

<p>
Resources that are read-only during workflow execution need no synchronization.
Resources that require mutable state during execution (for example, accumulating results or
maintaining a connection pool counter) must use interior mutability:
</p>

<table class="styled-table">
<thead>
<tr>
<th>Interior mutability primitive</th>
<th>When to use</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>tokio::sync::RwLock&lt;T&gt;</code></td>
<td>Many concurrent readers, rare writers. Best default for resource state that tasks mostly read.</td>
</tr>
<tr>
<td><code>tokio::sync::Mutex&lt;T&gt;</code></td>
<td>Exclusive access needed. Serializes split tasks on that resource — be aware of the throughput impact.</td>
</tr>
<tr>
<td><code>std::sync::atomic::*</code></td>
<td>Single numeric counters or flags. Lock-free, minimal overhead.</td>
</tr>
<tr>
<td><code>DashMap&lt;K, V&gt;</code></td>
<td>Concurrent key-value writes. Sharded locking with lower contention than a <code>Mutex&lt;HashMap&gt;</code>.</td>
</tr>
</tbody>
</table>

<div class="callout callout-warning">
<div class="callout-label">Coarse locks eliminate parallelism</div>
<p>
A single <code>Mutex&lt;T&gt;</code> guarding an entire resource will serialize split tasks
that try to write to it simultaneously. If your split tasks all write to the same resource,
the parallel execution provides no throughput benefit — consider partitioning by task index
or using a concurrent data structure.
</p>
</div>

<!-- Section: Orchestrate vs Scheduler Lifecycle -->
<hr class="section-divider">
<h2 id="orchestrate-lifecycle"><a href="#orchestrate-lifecycle" class="anchor-link" aria-hidden="true">#</a>Standalone vs Scheduler Lifecycle</h2>
<p>
The timing of <code>setup</code> and <code>teardown</code> differs between standalone
<code>orchestrate()</code> calls and workflows registered with the
<a href="../scheduler/">Scheduler</a>.
</p>

<div class="card-stack">
<div class="card">
<h3>Standalone — orchestrate()</h3>
<p>
Each call to <code>orchestrate()</code> runs the full lifecycle: <code>setup_all()</code>
before the FSM loop, <code>teardown_range(all)</code> after it (even if the workflow
returns an error). This is the right model for per-request workflows like HTTP handlers.
</p>
<div class="code-block">
<span class="code-block-label">Per-request lifecycle</span>
<pre><code class="language-rust">// Each request creates fresh resources — setup/teardown per call
let store = MemoryStore::new();
let resources = Resources::new()
    .insert("store", store.clone())
    .insert("request", RequestParams { text: payload.text });
<!--blank-->
let workflow = Workflow::new(resources)
    .register(State::Parse, ParseTask)
    .register(State::Respond, RespondTask)
    .add_exit_state(State::Done);
<!--blank-->
workflow.orchestrate(State::Parse).await?;
<!--blank-->
let result: String = store.get("response")?;</code></pre>
</div>
</div>
<div class="card">
<h3>Scheduler — setup once, run many</h3>
<p>
When a workflow is registered with <code>Scheduler</code>, <code>setup_all()</code> runs
<strong>once</strong> when <code>scheduler.start()</code> is called — not on each
scheduled firing. Teardown runs once when <code>scheduler.stop()</code> is called.
Each scheduled run calls <code>execute_workflow()</code> directly, skipping the
lifecycle.
</p>
<div class="code-block">
<span class="code-block-label">Scheduler — setup once</span>
<pre><code class="language-rust">// Resources are set up once when the scheduler starts,
// shared across all scheduled runs
let resources = Resources::new()
    .insert("store", MemoryStore::new())
    .insert("db", DbPool::connect("postgres://...").await?);
<!--blank-->
let workflow = Workflow::new(resources)
    .register(State::Collect, CollectTask)
    .register(State::Report, ReportTask)
    .add_exit_state(State::Done);
<!--blank-->
let mut scheduler = Scheduler::new();
scheduler.every_seconds("daily_report", workflow, State::Collect, 86400)?;
<!--blank-->
// setup() called on MemoryStore and DbPool here — once
scheduler.start().await?;
<!--blank-->
// ... each scheduled firing runs the FSM, reusing the same resources
<!--blank-->
// teardown() called on DbPool (LIFO), then MemoryStore — once
scheduler.stop().await;</code></pre>
</div>
</div>
</div>

<div class="callout callout-warning">
<div class="callout-label">Per-run mutable state with the Scheduler</div>
<p>
Because resources are shared across runs, any state accumulated in a resource carries over
to the next run. For per-run state (for example, a <code>MemoryStore</code> that should start
empty each run), clear it at the beginning of the first task in your workflow, or use
<code>store.clear()?</code> in a dedicated reset task registered for the initial state.
</p>
</div>

<!-- Section: Workflows Without Resources -->
<hr class="section-divider">
<h2 id="no-resources"><a href="#no-resources" class="anchor-link" aria-hidden="true">#</a>Workflows Without Resources</h2>
<p>
When every task is self-contained and needs no shared dependencies, skip resource setup entirely
with <code>Workflow::bare()</code> or <code>Resources::empty()</code>. Tasks implement
<code>run_bare()</code> instead of <code>run()</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Resource-free workflow</span>
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Compute, Done }
<!--blank-->
struct PureTask;
<!--blank-->
#[task(state = Step)]
impl PureTask {
    // run_bare() skips the &Resources parameter entirely
    async fn run_bare(&amp;self) -&gt; Result&lt;TaskResult&lt;Step&gt;, CanoError&gt; {
        let answer = 40 + 2;
        println!("Computed: {answer}");
        Ok(TaskResult::Single(Step::Done))
    }
}
<!--blank-->
// Workflow::bare() is equivalent to Workflow::new(Resources::empty())
let workflow = Workflow::bare()
    .register(Step::Compute, PureTask)
    .add_exit_state(Step::Done);</code></pre>
</div>

<!-- Section: API Reference -->
<hr class="section-divider">
<h2 id="api-reference"><a href="#api-reference" class="anchor-link" aria-hidden="true">#</a>API Reference</h2>

<table class="styled-table">
<thead>
<tr>
<th>Method</th>
<th>Signature</th>
<th>Notes</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>new()</code></td>
<td><code>Resources::new() -&gt; Self</code></td>
<td>Empty map. <code>#[must_use]</code>.</td>
</tr>
<tr>
<td><code>empty()</code></td>
<td><code>Resources::empty() -&gt; Self</code></td>
<td>Alias for <code>new()</code>. Use when you want to be explicit that no dependencies exist.</td>
</tr>
<tr>
<td><code>with_capacity(n)</code></td>
<td><code>Resources::with_capacity(n: usize) -&gt; Self</code></td>
<td>Pre-allocates <code>HashMap</code> capacity to avoid rehashing during bulk inserts. <code>#[must_use]</code>.</td>
</tr>
<tr>
<td><code>insert(key, resource)</code></td>
<td><code>insert&lt;R: Resource&gt;(self, key: impl Into&lt;TResourceKey&gt;, resource: R) -&gt; Self</code></td>
<td>Builder pattern — consumes and returns <code>self</code>. <strong>Panics</strong> on duplicate key (programmer error). <code>#[must_use]</code>.</td>
</tr>
<tr>
<td><code>try_insert(key, resource)</code></td>
<td><code>try_insert&lt;R: Resource&gt;(self, key: impl Into&lt;TResourceKey&gt;, resource: R) -&gt; Result&lt;Self, CanoError&gt;</code></td>
<td>Fallible variant of <code>insert</code>. Returns <code>CanoError::ResourceDuplicateKey</code> on duplicate. Use when keys come from dynamic input.</td>
</tr>
<tr>
<td><code>get(key)</code></td>
<td><code>get&lt;R: Resource, Q: ?Sized&gt;(&amp;self, key: &amp;Q) -&gt; Result&lt;Arc&lt;R&gt;, CanoError&gt;</code></td>
<td>Returns <code>ResourceNotFound</code> if key absent; <code>ResourceTypeMismatch</code> if type wrong.</td>
</tr>
<tr>
<td><code>setup_all()</code></td>
<td><code>async fn setup_all(&amp;self) -&gt; Result&lt;(), CanoError&gt;</code></td>
<td>FIFO setup with partial LIFO rollback on failure. Called by the engine before workflow execution.</td>
</tr>
<tr>
<td><code>teardown_all()</code></td>
<td><code>async fn teardown_all(&amp;self)</code></td>
<td>LIFO teardown of all resources. Errors logged, never propagated.</td>
</tr>
</tbody>
</table>
</div>

