+++
title = "Resources"
description = "Lifecycle-managed, typed dependency injection for Cano workflows in Rust."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Resources</h1>
<p class="subtitle">Lifecycle-managed, typed dependency injection for Cano workflows.</p>

<p>
<code>Resources</code> is the typed map every workflow carries. Database pools, HTTP clients, config structs,
a <a href="../blackboard/">Blackboard</a> and per-run parameters are all resources, registered once and read from any task.
A resource is looked up by its type, so <code>res.get::&lt;R&gt;()?</code> returns a <code>&amp;R</code> with no key to mistype.
A second instance of the same type goes under a name and is read with <code>get_named</code>.
</p>

<p>
<code>get_arc::&lt;R&gt;()</code> hands out an <code>Arc&lt;R&gt;</code>, cheap to clone and safe to share across split tasks.
The engine calls <code>setup()</code> on each resource in insertion order before the machine runs, and
<code>teardown()</code> in reverse order after it, even on failure.
</p>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#quick-start">Quick start</a></li>
<li><a href="#defining">Defining a resource</a></li>
<li><a href="#building">Building a Resources map</a></li>
<li><a href="#retrieving">Retrieving resources in tasks</a></li>
<li><a href="#from-resources">Declarative dependency loading</a></li>
<li><a href="#named">Named instances</a></li>
<li><a href="#lifecycle-guide">Lifecycle and concurrency</a></li>
<li><a href="#no-resources">Workflows without resources</a></li>
<li><a href="#api-reference">API reference</a></li>
</ol>
</nav>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start</h2>

<p>
Define a resource, insert it, and read it by type in any task. The engine runs
<code>setup</code> and <code>teardown</code> for you.
</p>

<div class="diagram-frame">
<p class="diagram-label">insert(value) at build time &rarr; res.get::&lt;R&gt;() inside tasks</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 316" role="img" aria-labelledby="res-map-title res-map-desc">
<title id="res-map-title">Resources as a typed map</title>
<desc id="res-map-desc">Three build-time inserts each store one value under its type: Blackboard, AppConfig and WorkflowParams. Inside tasks, res.get names a resource type and borrows the stored value. InitTask reads Blackboard, ProcessTask reads Blackboard and AppConfig, and ReportTask reads WorkflowParams, so one entry can have several readers.</desc>
<defs><marker id="res-map-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-strong" x="107" y="24">Build time</text>
<text class="t-code t-mut" x="107" y="43">.insert(value)</text>
<text class="t-strong" x="386" y="24">Typed map</text>
<text class="t-code t-mut" x="386" y="43">Resources::new()</text>
<text class="t-strong" x="669" y="24">In tasks</text>
<text class="t-code t-mut" x="669" y="43">res.get::&lt;R&gt;()?</text>
<path class="e" d="M200,96 H262" marker-end="url(#res-map-ah)"/>
<path class="e" d="M200,166 H262" marker-end="url(#res-map-ah)"/>
<path class="e" d="M200,236 H262" marker-end="url(#res-map-ah)"/>
<path class="e e-hot" d="M506,96 H572" marker-end="url(#res-map-ah)"/>
<path class="e e-hot" d="M506,108 H524 Q536,108 536,120 V133 Q536,145 548,145 H572" marker-end="url(#res-map-ah)"/>
<path class="e" d="M506,166 H572" marker-end="url(#res-map-ah)"/>
<path class="e" d="M506,236 H572" marker-end="url(#res-map-ah)"/>
<rect class="n" x="14" y="72" width="186" height="48" rx="10"/>
<text class="t-code" x="107" y="87">insert(board)</text>
<text class="t-code t-mut" x="107" y="107">Blackboard::new()</text>
<rect class="n" x="14" y="142" width="186" height="48" rx="10"/>
<text class="t-code" x="107" y="157">insert(config)</text>
<text class="t-code t-mut" x="107" y="177">AppConfig { .. }</text>
<rect class="n" x="14" y="212" width="186" height="48" rx="10"/>
<text class="t-code" x="107" y="227">insert(params)</text>
<text class="t-code t-mut" x="107" y="247">WorkflowParams { .. }</text>
<rect class="n-ghost" x="266" y="56" width="240" height="216" rx="12"/>
<rect class="n-hot" x="282" y="76" width="208" height="40" rx="6"/>
<text class="t-code" x="386" y="97">Blackboard &rarr; value</text>
<rect class="n" x="282" y="146" width="208" height="40" rx="6"/>
<text class="t-code" x="386" y="167">AppConfig &rarr; value</text>
<rect class="n" x="282" y="216" width="208" height="40" rx="6"/>
<text class="t-code" x="386" y="237">WorkflowParams &rarr; value</text>
<rect class="n" x="576" y="69" width="186" height="54" rx="10"/>
<text class="t-strong" x="669" y="86">InitTask</text>
<text class="t-code t-mut" x="669" y="108">&amp;Blackboard</text>
<rect class="n" x="576" y="133" width="186" height="66" rx="10"/>
<text class="t-strong" x="669" y="149">ProcessTask</text>
<text class="t-code t-mut" x="669" y="169">&amp;Blackboard</text>
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
        let board  = res.get::<Blackboard>()?;
        let config = res.get::<AppConfig>()?;

        board.put("value", 10u32 * config.multiplier);
        Ok(Step::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let resources = Resources::new()
        .insert(Blackboard::new())
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
<p>Runnable example: <code>cargo run --example workflow_resources</code>. It has a stateless config resource,
a stateful counter resource (interior mutability), and a resource with real <code>setup</code> and
<code>teardown</code> work, all read by type inside tasks.</p>
</div>

<!-- Section: Defining a Resource -->
<hr class="section-divider">
<h2 id="defining"><a href="#defining" class="anchor-link" aria-hidden="true">#</a>Defining a resource</h2>

<p>
The <code>Resource</code> trait has three hooks: <code>setup</code> and <code>teardown</code> for the lifecycle, and
<code>health</code> for observability. All default to no-ops (<code>health()</code> returns <code>Healthy</code>),
so most resources need no manual impl. The engine never calls <code>health()</code> during a run; see
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

<h3>Stateless: <code>#[derive(Resource)]</code></h3>
<p>
Generates an empty <code>impl Resource for T {}</code>. Use it for config, parameter bags and read-only data.
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

<h3>Stateful: <code>#[resource]</code> on the impl block</h3>
<p>
Override <code>setup</code> and <code>teardown</code> for resources that open connections, allocate buffers or flush state.
Both take <code>&amp;self</code> and resources are shared through <code>Arc</code>, so mutation needs interior mutability:
<code>tokio::sync::Mutex</code>, <code>RwLock</code> or atomics.
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
<h2 id="building"><a href="#building" class="anchor-link" aria-hidden="true">#</a>Building a Resources map</h2>

<p>
<code>Resources</code> is a typed builder. <code>insert</code> consumes <code>self</code> and panics when a resource of
the same type is already present, which is a programmer error. <code>insert_named</code> registers a second instance
of a type under a name. <code>try_insert</code> and <code>try_insert_named</code> return
<code>Result&lt;Self, CanoError&gt;</code>, so dynamic input can handle a collision as data.
</p>

<div class="code-block">
<span class="code-block-label">insert + try_insert</span>

```rust
// One resource per type; a duplicate type indicates buggy wiring, so it panics
let resources = Resources::new()
    .insert(Blackboard::new())
    .insert(AppConfig::default())
    // A second Blackboard needs a name
    .insert_named("replica", Blackboard::new())
    // Dynamic input: handle a collision as data
    .try_insert(plugin)?;

```
</div>

<!-- Section: Retrieving -->
<hr class="section-divider">
<h2 id="retrieving"><a href="#retrieving" class="anchor-link" aria-hidden="true">#</a>Retrieving resources in tasks</h2>

<p>
<code>get::&lt;R&gt;()</code> returns <code>Result&lt;&amp;R, CanoError&gt;</code>, a plain
borrow tied to <code>res</code>. <code>get_arc::&lt;R&gt;()</code> returns an
<code>Arc&lt;R&gt;</code> when you need to move the handle into a spawned task or a struct.
<code>get_named</code> and <code>get_named_arc</code> take the name as an extra argument.
</p>

<div class="code-block">
<span class="code-block-label">By type, by name, as Arc</span>

```rust
let board  = res.get::<Blackboard>()?;          // &Blackboard
let params = res.get::<WorkflowParams>()?;       // &WorkflowParams

let replica = res.get_named::<Blackboard>("replica")?;

let shared: Arc<Blackboard> = res.get_arc::<Blackboard>()?;
let shared_replica = res.get_named_arc::<Blackboard>("replica")?;

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
<h2 id="from-resources"><a href="#from-resources" class="anchor-link" aria-hidden="true">#</a>Declarative loading with <code>#[derive(FromResources)]</code></h2>

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
    board: Arc<Blackboard>,
    config: Arc<AppConfig>,
    #[res("replica")]
    replica: Arc<Blackboard>,
}

fn lookup(res: &Resources) -> Result<(), CanoError> {
    // In a task:
    let InitDeps { board, config, replica } = InitDeps::from_resources(res)?;
    let _ = (board, config, replica);
    Ok(())
}
```
</div>

<!-- Section: Named Instances -->
<hr class="section-divider">
<h2 id="named"><a href="#named" class="anchor-link" aria-hidden="true">#</a>Named instances</h2>

<p>
A <code>Resources</code> holds one value per type. When a workflow needs two instances of one type, such as two
<code>Blackboard</code>s or a primary and a replica pool, register the extra one with <code>insert_named</code>
and read it with <code>get_named</code>. Names live in their own namespace: a named resource never collides with the
by-type entry of the same type, and <code>get</code> never sees named entries.
A duplicate type or name panics; <code>try_insert</code> and <code>try_insert_named</code> return
<code>ResourceDuplicateKey</code> instead.
</p>

<table class="styled-table">
<thead>
<tr><th></th><th>By type</th><th>By name</th></tr>
</thead>
<tbody>
<tr><td>Insert</td><td><code>insert(value)</code></td><td><code>insert_named("name", value)</code></td></tr>
<tr><td>Read</td><td><code>get::&lt;R&gt;()</code></td><td><code>get_named::&lt;R&gt;("name")</code></td></tr>
<tr><td>Not inserted</td><td><code>ResourceNotFound</code></td><td><code>ResourceNotFound</code></td></tr>
<tr><td>Health label</td><td>the type name</td><td>the name</td></tr>
</tbody>
</table>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example resources_advanced</code>: type-keyed lookups, named
second instances, <code>try_insert</code> returning <code>CanoError::ResourceDuplicateKey</code> on a repeated
type, and the partial-LIFO rollback when a later resource's <code>setup</code> fails.</p>
</div>

<!-- Section: Lifecycle -->
<hr class="section-divider">

<h2 id="lifecycle-guide"><a href="#lifecycle-guide" class="anchor-link" aria-hidden="true">#</a>Lifecycle and concurrency</h2>
<p><a href="lifecycle-and-concurrency/">Resource lifecycle and concurrency</a> covers when <code>setup()</code> and <code>teardown()</code> fire, how resources behave across concurrent splits, and the standalone and scheduler lifecycles.</p>
<hr class="section-divider">
<h2 id="no-resources"><a href="#no-resources" class="anchor-link" aria-hidden="true">#</a>Workflows without resources</h2>

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
<p>Runnable example: <code>cargo run --example workflow_bare</code>, a workflow with no resources at all.</p>
</div>

<!-- Section: API Reference -->
<hr class="section-divider">
<h2 id="api-reference"><a href="#api-reference" class="anchor-link" aria-hidden="true">#</a>API reference</h2>

<p>Every public method on <code>Resources</code>; <code>R</code> is always <code>R: Resource</code>.</p>
<dl class="key-list">
<dt><code>Resources::new() -&gt; Self</code></dt>
<dd>An empty map.</dd>
<dt><code>insert&lt;R&gt;(self, resource: R) -&gt; Self</code></dt>
<dd>Builder. One value per type; panics on a duplicate type.</dd>
<dt><code>insert_named&lt;R&gt;(self, name: impl Into&lt;Cow&lt;'static, str&gt;&gt;, resource: R) -&gt; Self</code></dt>
<dd>Builder for a second value of a type, under a name. Panics on a duplicate name.</dd>
<dt><code>try_insert&lt;R&gt;(self, resource: R) -&gt; Result&lt;Self, CanoError&gt;</code></dt>
<dd><code>ResourceDuplicateKey</code> on a duplicate type.</dd>
<dt><code>try_insert_named&lt;R&gt;(self, name, resource: R) -&gt; Result&lt;Self, CanoError&gt;</code></dt>
<dd><code>ResourceDuplicateKey</code> on a duplicate name.</dd>
<dt><code>get&lt;R&gt;(&amp;self) -&gt; Result&lt;&amp;R, CanoError&gt;</code></dt>
<dd><code>ResourceNotFound</code> when no value of that type was inserted.</dd>
<dt><code>get_named&lt;R&gt;(&amp;self, name: &amp;str) -&gt; Result&lt;&amp;R, CanoError&gt;</code></dt>
<dd><code>ResourceNotFound</code> for an unknown name, <code>ResourceTypeMismatch</code> when the name holds another type.</dd>
<dt><code>get_arc&lt;R&gt;(&amp;self) -&gt; Result&lt;Arc&lt;R&gt;, CanoError&gt;</code> <small>and <code>get_named_arc</code></small></dt>
<dd>As <code>get</code> and <code>get_named</code>, returning a cloned <code>Arc</code> you can move into a spawned task.</dd>
<dt><code>async fn setup_all(&amp;self) -&gt; Result&lt;(), CanoError&gt;</code></dt>
<dd>Setup in insertion order, rolling back the ones already set up if one fails. The engine calls it.</dd>
<dt><code>async fn teardown_all(&amp;self)</code></dt>
<dd>Teardown in reverse order. Errors are logged, never returned.</dd>
<dt><code>async fn check_all_health(&amp;self) -&gt; Vec&lt;(Cow&lt;'static, str&gt;, HealthStatus)&gt;</code></dt>
<dd>One entry per resource in insertion order, labelled by its name, or by its type name when inserted unnamed.</dd>
<dt><code>async fn aggregate_health(&amp;self) -&gt; HealthStatus</code></dt>
<dd>The worst status across all resources; <code>Healthy</code> when there are none.</dd>
</dl>

</div>
