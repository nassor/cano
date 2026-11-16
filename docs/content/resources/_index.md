+++
title = "Resources"
description = "Lifecycle-managed, typed dependency injection for Cano workflows in Rust."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Resources</h1>
<p class="subtitle">Lifecycle-managed, typed dependency injection for Cano workflows.</p>

<p>
<code>Resources</code> is the typed dictionary every workflow carries. Database
pools, HTTP clients, config structs, <a href="../store/">MemoryStore</a>, and per-run parameters
are all resources: registered once, injected everywhere. A resource is looked up by its
type, so <code>res.get::&lt;R&gt;()?</code> returns a <code>&amp;R</code> with no key to mistype.
A second instance of the same type is registered under a name and read with
<code>get_named</code>. <code>get_arc::&lt;R&gt;()</code> hands out an <code>Arc&lt;R&gt;</code>,
cheap to clone and safe to share across concurrent split tasks.
</p>

<p>
The engine calls <code>setup()</code> on each registered resource in insertion order before
the FSM runs, and <code>teardown()</code> in reverse order after, even on failure. Tasks
never receive resources directly; they retrieve them from the injected
<code>&amp;Resources</code> via <code>res.get::&lt;R&gt;()?</code>.
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
<li><a href="#named">Named Instances</a></li>
<li><a href="#lifecycle-guide">Lifecycle &amp; Concurrency</a></li>
<li><a href="#no-resources">Workflows Without Resources</a></li>
<li><a href="#api-reference">API Reference</a></li>
</ol>
</nav>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start</h2>

<p>
Define a resource, register it, retrieve it by type in any task. The engine wires
<code>setup</code> / <code>teardown</code> automatically.
</p>

<div class="diagram-frame">
<p class="diagram-label">insert(value) at build time &rarr; res.get::&lt;R&gt;() inside tasks</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 316" role="img">
<title>Resources is a typed map: every build-time insert stores one value under its type, and inside a task res.get::&lt;R&gt; names the resource type to borrow the value back out, so one entry can be read by several tasks.</title>
<defs><marker id="res-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-strong" x="107" y="24">Build time</text>
<text class="t-code t-mut" x="107" y="43">.insert(value)</text>
<text class="t-strong" x="386" y="24">Typed map</text>
<text class="t-code t-mut" x="386" y="43">Resources::new()</text>
<text class="t-strong" x="669" y="24">In tasks</text>
<text class="t-code t-mut" x="669" y="43">res.get::&lt;R&gt;()?</text>
<path class="e" d="M200,96 H262" marker-end="url(#res-ah)"/>
<path class="e" d="M200,166 H262" marker-end="url(#res-ah)"/>
<path class="e" d="M200,236 H262" marker-end="url(#res-ah)"/>
<path class="e e-hot" d="M506,96 H572" marker-end="url(#res-ah)"/>
<path class="e e-hot" d="M506,108 H524 Q536,108 536,120 V133 Q536,145 548,145 H572" marker-end="url(#res-ah)"/>
<path class="e" d="M506,166 H572" marker-end="url(#res-ah)"/>
<path class="e" d="M506,236 H572" marker-end="url(#res-ah)"/>
<rect class="n" x="14" y="72" width="186" height="48" rx="10"/>
<text class="t-code" x="107" y="87">insert(store)</text>
<text class="t-code t-mut" x="107" y="107">MemoryStore::new()</text>
<rect class="n" x="14" y="142" width="186" height="48" rx="10"/>
<text class="t-code" x="107" y="157">insert(config)</text>
<text class="t-code t-mut" x="107" y="177">AppConfig { .. }</text>
<rect class="n" x="14" y="212" width="186" height="48" rx="10"/>
<text class="t-code" x="107" y="227">insert(params)</text>
<text class="t-code t-mut" x="107" y="247">WorkflowParams { .. }</text>
<rect class="n-cop" x="266" y="56" width="240" height="216" rx="12"/>
<rect class="n-hot" x="282" y="76" width="208" height="40" rx="6"/>
<text class="t-code" x="386" y="97">MemoryStore &rarr; value</text>
<rect class="n" x="282" y="146" width="208" height="40" rx="6"/>
<text class="t-code" x="386" y="167">AppConfig &rarr; value</text>
<rect class="n" x="282" y="216" width="208" height="40" rx="6"/>
<text class="t-code" x="386" y="237">WorkflowParams &rarr; value</text>
<rect class="n" x="576" y="69" width="186" height="54" rx="10"/>
<text class="t-strong" x="669" y="86">InitTask</text>
<text class="t-code t-mut" x="669" y="108">&amp;MemoryStore</text>
<rect class="n" x="576" y="133" width="186" height="66" rx="10"/>
<text class="t-strong" x="669" y="149">ProcessTask</text>
<text class="t-code t-mut" x="669" y="169">&amp;MemoryStore</text>
<text class="t-code t-mut" x="669" y="186">&amp;AppConfig</text>
<rect class="n" x="576" y="209" width="186" height="54" rx="10"/>
<text class="t-strong" x="669" y="226">ReportTask</text>
<text class="t-code t-mut" x="669" y="248">&amp;WorkflowParams</text>
<text class="t-mut" x="390" y="296">One entry, many readers: get matches on the resource type and borrows the stored value.</text>
</svg>
</div>
</div>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> End-to-end example</span>

```rust
use cano::prelude::*;

// Stateless resource: derive Resource for a no-op setup/teardown impl
#[derive(Resource)]
struct AppConfig { multiplier: u32 }

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Init, Done }

struct InitTask;

#[task(state = Step)]
impl InitTask {
    async fn run(&self, res: &Resources) -> Result<Step, CanoError> {
        let store  = res.get::<MemoryStore>()?;
        let config = res.get::<AppConfig>()?;

        store.put("value", 10u32 * config.multiplier);
        Ok(Step::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let resources = Resources::new()
        .insert(MemoryStore::new())
        .insert(AppConfig { multiplier: 3 });

    let workflow = Workflow::new(resources)
        .register(Step::Init, InitTask)
        .add_exit_state(Step::Done);

    workflow.orchestrate(Step::Init).await?;
    Ok(())
}

```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_resources</code> — a stateless config resource,
a stateful counter resource (interior mutability), and a resource with real <code>setup</code> /
<code>teardown</code> lifecycle work, all retrieved by type inside tasks.</p>
</div>

<!-- Section: Defining a Resource -->
<hr class="section-divider">
<h2 id="defining"><a href="#defining" class="anchor-link" aria-hidden="true">#</a>Defining a Resource</h2>

<p>
The <code>Resource</code> trait gives every dependency three hooks — two lifecycle, one
observability. All default to no-ops (and <code>health()</code> defaults to
<code>Healthy</code>), so most resources need no manual impl. <code>health()</code> is
opt-in observability — it is never called during normal workflow execution; see
<a href="../observers/#health">Observers &amp; Health Probes</a>.
</p>

<div class="code-block">
<span class="code-block-label">Resource trait</span>

```rust
pub trait Resource: Send + Sync + 'static {
    async fn setup(&self) -> Result<(), CanoError> { Ok(()) }
    async fn teardown(&self) -> Result<(), CanoError> { Ok(()) }
    async fn health(&self) -> HealthStatus { HealthStatus::Healthy }
}

```
</div>

<h3>Stateless — <code>#[derive(Resource)]</code></h3>
<p>
Generates an empty <code>impl Resource for T {}</code>. Use it for config, parameter bags,
read-only data — anything without lifecycle work.
</p>

<div class="code-block">
<span class="code-block-label">Stateless resource via derive</span>

```rust
#[derive(Resource)]
struct WorkflowParams {
    batch_size: usize,
    timeout_ms: u64,
}

```
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

```rust
use cano::prelude::*;

use std::sync::{Arc, Mutex};

struct CounterResource {
    setup_count: Arc<Mutex<u32>>,
}

#[resource]
impl Resource for CounterResource {
    async fn setup(&self) -> Result<(), CanoError> {
        *self.setup_count.lock().unwrap() += 1;
        Ok(())
    }

    async fn teardown(&self) -> Result<(), CanoError> {
        // flush, close handles, etc.
        Ok(())
    }
}

```
</div>

<!-- Section: Building -->
<hr class="section-divider">
<h2 id="building"><a href="#building" class="anchor-link" aria-hidden="true">#</a>Building a Resources Map</h2>

<p>
<code>Resources</code> is a typed builder. <code>insert</code> consumes <code>self</code> and
panics when a resource of the same type is already present (programmer error).
<code>insert_named</code> registers a second instance of a type under a name.
<code>try_insert</code> and <code>try_insert_named</code> return
<code>Result&lt;Self, CanoError&gt;</code> so dynamic input can handle collisions explicitly.
They compose in a single chain.
</p>

<div class="code-block">
<span class="code-block-label">insert + try_insert</span>

```rust
// One resource per type; a duplicate type indicates buggy wiring, so it panics
let resources = Resources::new()
    .insert(MemoryStore::new())
    .insert(AppConfig::default())
    // A second MemoryStore needs a name
    .insert_named("replica", MemoryStore::new())
    // Dynamic input: handle a collision as data
    .try_insert(plugin)?;

```
</div>

<!-- Section: Retrieving -->
<hr class="section-divider">
<h2 id="retrieving"><a href="#retrieving" class="anchor-link" aria-hidden="true">#</a>Retrieving Resources in Tasks</h2>

<p>
<code>get::&lt;R&gt;()</code> returns <code>Result&lt;&amp;R, CanoError&gt;</code>, a plain
borrow tied to <code>res</code>. <code>get_arc::&lt;R&gt;()</code> returns an
<code>Arc&lt;R&gt;</code> when you need to move the handle into a spawned task or a struct.
<code>get_named</code> and <code>get_named_arc</code> take the name as an extra argument.
</p>

<div class="code-block">
<span class="code-block-label">By type, by name, as Arc</span>

```rust
let store  = res.get::<MemoryStore>()?;          // &MemoryStore
let params = res.get::<WorkflowParams>()?;       // &WorkflowParams

let replica = res.get_named::<MemoryStore>("replica")?;

let shared: Arc<MemoryStore> = res.get_arc::<MemoryStore>()?;
let shared_replica = res.get_named_arc::<MemoryStore>("replica")?;

```
</div>

<table class="styled-table">
<thead>
<tr><th>Error variant</th><th>Meaning</th></tr>
</thead>
<tbody>
<tr><td><code>ResourceNotFound</code></td><td>No resource of that type (or with that name) was inserted.</td></tr>
<tr><td><code>ResourceTypeMismatch</code></td><td>A named resource exists, but it holds a different type than <code>R</code>.</td></tr>
<tr><td><code>ResourceDuplicateKey</code></td><td>Returned by <code>try_insert</code> when the type is already present, and by <code>try_insert_named</code> when the name is.</td></tr>
</tbody>
</table>

<!-- Section: FromResources -->
<hr class="section-divider">
<h2 id="from-resources"><a href="#from-resources" class="anchor-link" aria-hidden="true">#</a>Declarative Loading with <code>#[derive(FromResources)]</code></h2>

<p>
A struct of <code>Arc</code> dependencies can declare its lookups instead of calling
<code>res.get_arc</code> field-by-field. The derive generates
<code>fn from_resources(res: &amp;Resources) -&gt; CanoResult&lt;Self&gt;</code>.
</p>

<p>
Every field must be <code>Arc&lt;T&gt;</code>. A field with no attribute is looked up by type;
<code>#[res("name")]</code> looks up the instance inserted with that name.
</p>

<div class="code-block">
<span class="code-block-label">Deps struct: by type and by name</span>

```rust
use cano::prelude::*;
use std::sync::Arc;

#[derive(Resource)]
struct AppConfig { multiplier: u32 }

#[derive(FromResources)]
struct InitDeps {
    store: Arc<MemoryStore>,
    config: Arc<AppConfig>,
    #[res("replica")]
    replica: Arc<MemoryStore>,
}

fn lookup(res: &Resources) -> Result<(), CanoError> {
    // In a task:
    let InitDeps { store, config, replica } = InitDeps::from_resources(res)?;
    let _ = (store, config, replica);
    Ok(())
}
```
</div>

<!-- Section: Named Instances -->
<hr class="section-divider">
<h2 id="named"><a href="#named" class="anchor-link" aria-hidden="true">#</a>Named Instances</h2>

<p>
By default a <code>Resources</code> holds one value per type. When a workflow needs two
instances of the same type (two <code>MemoryStore</code>s, a primary and a replica pool),
register the extra one with <code>insert_named</code> and read it with <code>get_named</code>.
Names live in their own namespace: a named resource never collides with the by-type entry
of the same type, and unnamed lookups never see named entries.
</p>

<table class="styled-table">
<thead>
<tr><th></th><th>By type</th><th>By name</th></tr>
</thead>
<tbody>
<tr><td>Insert</td><td><code>insert(value)</code></td><td><code>insert_named("name", value)</code></td></tr>
<tr><td>Read</td><td><code>get::&lt;R&gt;()</code></td><td><code>get_named::&lt;R&gt;("name")</code></td></tr>
<tr><td>Typo detection</td><td>The type name is checked at compile time; a missing registration is still <code>ResourceNotFound</code> at runtime</td><td>Runtime: <code>ResourceNotFound</code></td></tr>
<tr><td>Duplicate</td><td>Panics (or <code>try_insert</code> error)</td><td>Panics (or <code>try_insert_named</code> error)</td></tr>
<tr><td>Health label</td><td>Type name</td><td>The name</td></tr>
</tbody>
</table>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example resources_advanced</code>: type-keyed lookups, named
second instances, <code>try_insert</code> returning <code>CanoError::ResourceDuplicateKey</code> on a repeated
type, and the partial-LIFO rollback when a later resource's <code>setup</code> fails.</p>
</div>

<!-- Section: Lifecycle -->
<hr class="section-divider">

<h2 id="lifecycle-guide"><a href="#lifecycle-guide" class="anchor-link" aria-hidden="true">#</a>Lifecycle &amp; Concurrency</h2>
<p>When <code>setup()</code> and <code>teardown()</code> fire, how resources behave across concurrent splits, and standalone vs scheduler lifecycle are covered on a dedicated page:</p>
<ul>
<li><a href="lifecycle-and-concurrency/">Resource Lifecycle and Concurrency</a></li>
</ul>
<hr class="section-divider">
<h2 id="no-resources"><a href="#no-resources" class="anchor-link" aria-hidden="true">#</a>Workflows Without Resources</h2>

<p>
When every task is self-contained, skip the resources map. Write the method without the
<code>res</code> parameter (<code>async fn run(&amp;self)</code>) and build the workflow with
<code>Workflow::bare()</code> (equivalent to <code>Workflow::new(Resources::new())</code>).
</p>

<div class="code-block">
<span class="code-block-label">Resource-free workflow</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Compute, Done }

#[derive(Clone)]
struct PureTask;

#[task(state = Step)]
impl PureTask {
    async fn run(&self) -> Result<Step, CanoError> {
        let answer = 40 + 2;
        println!("Computed: {answer}");
        Ok(Step::Done)
    }
}

fn build() -> Workflow<Step> {
    Workflow::bare()
        .register(Step::Compute, PureTask)
        .add_exit_state(Step::Done)
}

```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_bare</code> — a workflow with no resources at all.</p>
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
<tr><td><code>insert</code></td><td><code>insert&lt;R: Resource&gt;(self, resource: R) -&gt; Self</code></td><td>Builder; one resource per type; <strong>panics</strong> on a duplicate type.</td></tr>
<tr><td><code>insert_named</code></td><td><code>insert_named&lt;R: Resource&gt;(self, name, resource: R) -&gt; Self</code></td><td>Builder; a named instance. <strong>Panics</strong> on a duplicate name.</td></tr>
<tr><td><code>try_insert</code></td><td><code>try_insert&lt;R&gt;(self, resource: R) -&gt; Result&lt;Self, CanoError&gt;</code></td><td>Returns <code>ResourceDuplicateKey</code> on a duplicate type.</td></tr>
<tr><td><code>try_insert_named</code></td><td><code>try_insert_named&lt;R&gt;(self, name, resource: R) -&gt; Result&lt;Self, CanoError&gt;</code></td><td>Returns <code>ResourceDuplicateKey</code> on a duplicate name.</td></tr>
<tr><td><code>get</code></td><td><code>get&lt;R&gt;(&amp;self) -&gt; Result&lt;&amp;R, CanoError&gt;</code></td><td><code>ResourceNotFound</code> when the type is absent.</td></tr>
<tr><td><code>get_named</code></td><td><code>get_named&lt;R&gt;(&amp;self, name: &amp;str) -&gt; Result&lt;&amp;R, CanoError&gt;</code></td><td><code>ResourceNotFound</code> / <code>ResourceTypeMismatch</code>.</td></tr>
<tr><td><code>get_arc</code></td><td><code>get_arc&lt;R&gt;(&amp;self) -&gt; Result&lt;Arc&lt;R&gt;, CanoError&gt;</code></td><td>Same as <code>get</code>, returning a cloned <code>Arc</code>.</td></tr>
<tr><td><code>get_named_arc</code></td><td><code>get_named_arc&lt;R&gt;(&amp;self, name: &amp;str) -&gt; Result&lt;Arc&lt;R&gt;, CanoError&gt;</code></td><td>Same as <code>get_named</code>, returning a cloned <code>Arc</code>.</td></tr>
<tr><td><code>setup_all()</code></td><td><code>async fn setup_all(&amp;self) -&gt; Result&lt;(), CanoError&gt;</code></td><td>FIFO setup with partial LIFO rollback. Engine calls this.</td></tr>
<tr><td><code>teardown_all()</code></td><td><code>async fn teardown_all(&amp;self)</code></td><td>LIFO teardown. Errors logged, never propagated.</td></tr>
<tr><td><code>check_all_health()</code></td><td><code>async fn check_all_health(&amp;self) -&gt; Vec&lt;(Cow&lt;'static, str&gt;, HealthStatus)&gt;</code></td><td>Insertion order; each entry is labelled by name, or by type name when inserted unnamed.</td></tr>
</tbody>
</table>

</div>
