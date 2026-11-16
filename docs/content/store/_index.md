+++
title = "MemoryStore"
description = "MemoryStore in Cano: the default thread-safe, in-memory shared state for passing data between workflow stages."
template = "section.html"
+++

<div class="content-wrapper">
<h1>MemoryStore</h1>
<p class="subtitle">The default in-memory shared state for passing data between workflow stages.</p>

<p>
The store is the shared data layer between workflow stages. Tasks write values as they run and read the values
produced upstream. The default implementation, <code>MemoryStore</code>, is an <code>Arc&lt;RwLock&lt;HashMap&gt;&gt;</code>
that is safe to clone and share across async tasks. A custom backend is a typed <code>Resource</code> read by its concrete type.
</p>

<p>
<code>MemoryStore</code> is inserted into a <a href="../resources/"><code>Resources</code></a> map and read in tasks with
<code>res.get::&lt;MemoryStore&gt;()?</code>. It is one typed dependency among others, such as database pools and HTTP clients.
<a href="../resources/">Resources</a> covers the lifecycle, named instances and the full <code>insert</code> and <code>get</code> API.
</p>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#thread-safety">Thread safety model</a></li>
<li><a href="#api-reference">API reference</a></li>
<li><a href="#basic-usage">Basic usage</a></li>
<li><a href="#zero-copy">Zero-copy reads</a></li>
<li><a href="#workflow-usage">Store in a workflow</a></li>
<li><a href="#custom-backends">Custom store resources</a></li>
<li><a href="#error-handling">Error handling</a></li>
</ol>
</nav>

<!-- Section: Thread Safety Model -->
<hr class="section-divider">
<h2 id="thread-safety"><a href="#thread-safety" class="anchor-link" aria-hidden="true">#</a>Thread safety model</h2>

<div class="diagram-frame">
<p class="diagram-label">One shared map, many readers and writers</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 24 770 263" role="img" aria-labelledby="store-lock-title store-lock-desc">
<title id="store-lock-title">One shared MemoryStore, three tasks</title>
<desc id="store-lock-desc">One MemoryStore, an Arc around an RwLock around a HashMap, is shared by three tasks. Task A and Task B read through get() and hold the shared read lock at the same time. Task C writes through put() and takes the exclusive write lock, so readers wait.</desc>
<defs><marker id="store-lock-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M242,124 H330 Q342,124 342,112 V88 Q342,76 354,76 H494" marker-end="url(#store-lock-ah)"/>
<text class="t-code t-mut" x="424" y="59">.get()</text>
<path class="e" d="M242,132 H494" marker-end="url(#store-lock-ah)"/>
<text class="t-code t-mut" x="424" y="115">.get()</text>
<path class="e" d="M242,140 H330 Q342,140 342,152 V210 Q342,222 354,222 H494" marker-end="url(#store-lock-ah)"/>
<text class="t-code t-mut" x="424" y="205">.put()</text>
<rect class="n-ghost" x="468" y="40" width="252" height="128" rx="12"/>
<rect class="n-hot" x="28" y="96" width="214" height="72" rx="10"/>
<text class="t-strong" x="135" y="121">MemoryStore</text>
<text class="t-code t-mut" x="135" y="145">Arc&lt;RwLock&lt;HashMap&gt;&gt;</text>
<rect class="n" x="498" y="54" width="192" height="44" rx="10"/>
<text x="594" y="77">Task A (read)</text>
<rect class="n" x="498" y="110" width="192" height="44" rx="10"/>
<text x="594" y="133">Task B (read)</text>
<text class="t-mut" x="594" y="186">shared read lock, concurrent</text>
<rect class="n" x="498" y="200" width="192" height="44" rx="10"/>
<text x="594" y="223">Task C (write)</text>
<text class="t-mut" x="594" y="264">exclusive write lock, readers wait</text>
</svg>
</div>
</div>

<p>
<code>MemoryStore</code> wraps its map in <code>Arc&lt;RwLock&lt;_&gt;&gt;</code> (a <code>parking_lot</code> lock).
Readers share the lock and writers get exclusive access. Cloning the store gives a second handle to the
same underlying data, not a copy.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128274;</span> Clone shares the same data</span>

```rust
let store = MemoryStore::new();
let store2 = store.clone(); // same backing map, not a copy

store.put("key", 42i32);
let val: i32 = store2.get("key")?; // returns 42

```
</div>

<!-- Section: API Reference -->
<hr class="section-divider">
<h2 id="api-reference"><a href="#api-reference" class="anchor-link" aria-hidden="true">#</a>API reference</h2>

<p>
Every public method on <code>MemoryStore</code>. A facet marks a method as infallible; the others return
<code>StoreResult&lt;T&gt;</code>, which is <code>Result&lt;T, StoreError&gt;</code>.
</p>
<dl class="key-list">
<dt><code>get&lt;T: 'static + Clone&gt;(&amp;self, key: &amp;str) -&gt; StoreResult&lt;T&gt;</code></dt>
<dd>Returns a clone of the value. <code>KeyNotFound</code> for a missing key, <code>TypeMismatch</code> for a value of another type.</dd>

<dt><code>get_shared&lt;T: 'static + Send + Sync&gt;(&amp;self, key: &amp;str) -&gt; StoreResult&lt;Arc&lt;T&gt;&gt;</code></dt>
<dd>Zero-copy read: returns a clone of the stored <code>Arc</code>. Same errors as <code>get</code>.</dd>

<dt><code>put&lt;T: 'static + Send + Sync&gt;(&amp;self, key: &amp;str, value: T)</code> <small>infallible</small></dt>
<dd>Inserts or replaces the value. It does not need to implement <code>Clone</code>.</dd>

<dt><code>remove(&amp;self, key: &amp;str)</code> <small>infallible</small></dt>
<dd>Removes the key. A missing key is a no-op.</dd>

<dt><code>append&lt;T: 'static + Send + Sync + Clone&gt;(&amp;self, key: &amp;str, item: T) -&gt; StoreResult&lt;()&gt;</code></dt>
<dd>Pushes onto the <code>Vec&lt;T&gt;</code> at the key, creating it when the key is absent. <code>AppendTypeMismatch</code> when the key holds anything else.</dd>

<dt><code>contains_key(&amp;self, key: &amp;str) -&gt; bool</code> <small>infallible</small></dt>
<dd><code>true</code> when the key exists.</dd>

<dt><code>keys(&amp;self) -&gt; Vec&lt;Arc&lt;str&gt;&gt;</code> <small>infallible</small></dt>
<dd>All keys, in unspecified order.</dd>

<dt><code>len(&amp;self) -&gt; usize</code> <small>infallible</small></dt>
<dd>The number of entries.</dd>

<dt><code>is_empty(&amp;self) -&gt; bool</code> <small>infallible</small></dt>
<dd><code>true</code> when the store holds no entries.</dd>

<dt><code>clear(&amp;self)</code> <small>infallible</small></dt>
<dd>Removes every entry.</dd>
</dl>

<!-- Section: Basic Usage -->
<hr class="section-divider">
<h2 id="basic-usage"><a href="#basic-usage" class="anchor-link" aria-hidden="true">#</a>Basic usage</h2>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128204;</span> Common store operations</span>

```rust
use cano::prelude::*;

fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Store primitive and composite types
    store.put("count", 42u32);
    store.put("labels", vec!["a".to_string(), "b".to_string()]);

    // Retrieve with explicit type annotation
    let count: u32 = store.get("count")?;
    let labels: Vec<String> = store.get("labels")?;

    // Append to a Vec (creates if absent)
    store.append("log", "step 1 complete".to_string())?;
    store.append("log", "step 2 complete".to_string())?;
    let log: Vec<String> = store.get("log")?;

    // Check existence before reading optional keys
    if store.contains_key("result") {
        let result: String = store.get("result")?;
    }

    // Remove a key; a missing key is a no-op
    store.remove("scratch");
    Ok(())
}

```
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
Use <code>contains_key()</code> before <code>get()</code> for optional keys, or handle the
<code>KeyNotFound</code> error directly. <code>append()</code> builds up a collection across stages without
overwriting earlier entries.
</p>
</div>

<!-- Section: Zero-Copy Reads -->
<hr class="section-divider">
<h2 id="zero-copy"><a href="#zero-copy" class="anchor-link" aria-hidden="true">#</a>Zero-copy reads with get_shared()</h2>

<p>
<code>get_shared()</code> returns an <code>Arc&lt;T&gt;</code> pointing at the stored value. The store keeps every
value in an <code>Arc</code>, so the call is a reference-count bump, not a data copy. It suits large values, such as
model weights or a dataset, that several downstream tasks read.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Zero-copy with Arc</span>

```rust
use cano::prelude::*;
use std::sync::Arc;

fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
    store.put("dataset", vec![0u8; 1_000_000]); // 1 MB

    // Both handles point to the same allocation, so nothing is copied
    let handle_a: Arc<Vec<u8>> = store.get_shared("dataset")?;
    let handle_b: Arc<Vec<u8>> = store.get_shared("dataset")?;

    assert!(Arc::ptr_eq(&handle_a, &handle_b));
    Ok(())
}

```
</div>

<!-- get vs get_shared comparison panel -->
<div class="method-comparison">
<div class="method-col">
<span class="method-badge method-badge-clone">Clones data</span>
<h4>get()</h4>
<p>
<code>get()</code> always clones the underlying value. Use when you need an owned copy.
</p>
</div>
<div class="method-col">
<span class="method-badge method-badge-zerocopy">Zero-copy</span>
<h4>get_shared()</h4>
<p>
Prefer <code>get_shared()</code> for large or frequently read values.
</p>
</div>
</div>

<!-- Section: Store in a Workflow -->
<hr class="section-divider">
<h2 id="workflow-usage"><a href="#workflow-usage" class="anchor-link" aria-hidden="true">#</a>Store in a workflow</h2>

<p>
Insert the store into a <code>Resources</code> map and pass the map to <code>Workflow::new()</code>.
Each task reads it with <code>res.get::&lt;MemoryStore&gt;()?</code>, so every task shares one handle.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9881;</span> End-to-end workflow with store</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Ingest, Transform, Complete }

struct IngestTask;

#[task(state = Stage)]
impl IngestTask {
    async fn run(&self, res: &Resources) -> Result<Stage, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let batch: usize = store.get("batch_size")?;
        let records: Vec<u32> = (0..batch as u32).collect();
        store.put("records", records);
        Ok(Stage::Transform)
    }
}

struct TransformTask;

#[task(state = Stage)]
impl TransformTask {
    async fn run(&self, res: &Resources) -> Result<Stage, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let records: Vec<u32> = store.get("records")?;
        let transformed: Vec<u32> = records.into_iter().map(|x| x * 2).collect();
        store.put("result", transformed);
        Ok(Stage::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Pre-populate before the workflow starts
    store.put("batch_size", 256usize);

    let workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(Stage::Ingest, IngestTask)
        .register(Stage::Transform, TransformTask)
        .add_exit_state(Stage::Complete);

    workflow.orchestrate(Stage::Ingest).await?;

    // Read results after the workflow completes
    let result: Vec<u32> = store.get("result")?;
    println!("Processed {} records", result.len());

    Ok(())
}

```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_stack_store</code>. It uses a <code>MemoryStore</code>
as a shared stack passed between workflow stages.</p>
</div>

<!-- Section: Custom Store Backends -->
<hr class="section-divider">
<h2 id="custom-backends"><a href="#custom-backends" class="anchor-link" aria-hidden="true">#</a>Custom store resources</h2>

<p>
For custom storage, define a concrete type with the API your workflow needs and implement <code>Resource</code> for it.
Insert it into <code>Resources</code> and read it by concrete type inside tasks. A second instance of the same type goes
under a name with <code>insert_named</code> and is read with <code>get_named</code>. Storage dependencies stay explicit,
with no storage trait in between.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128295;</span> Custom namespaced store resource</span>

```rust
use cano::prelude::*;
use cano::store::StoreResult;
use std::sync::Arc;

/// Example: a store that prefixes all keys with a namespace.
#[derive(Clone)]
pub struct NamespacedStore {
    namespace: String,
    inner: MemoryStore,
}

impl NamespacedStore {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            inner: MemoryStore::new(),
        }
    }

    fn ns_key(&self, key: &str) -> String {
        format!("{}::{}", self.namespace, key)
    }

    pub fn get<T: 'static + Clone>(&self, key: &str) -> StoreResult<T> {
        self.inner.get(&self.ns_key(key))
    }

    pub fn put<T: 'static + Send + Sync>(&self, key: &str, value: T) {
        self.inner.put(&self.ns_key(key), value);
    }

    pub fn remove(&self, key: &str) {
        self.inner.remove(&self.ns_key(key));
    }

    pub fn append<T: 'static + Send + Sync + Clone>(&self, key: &str, item: T) -> StoreResult<()> {
        self.inner.append(&self.ns_key(key), item)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(&self.ns_key(key))
    }

    pub fn keys(&self) -> Vec<String> {
        // Strip the namespace prefix before returning keys to callers
        let prefix = format!("{}::", self.namespace);
        self.inner
            .keys()
            .into_iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
            .collect()
    }

    pub fn len(&self) -> usize {
        // Count only keys in this namespace
        self.keys().len()
    }

    pub fn clear(&self) {
        // Remove only keys in this namespace
        for key in self.keys() {
            self.remove(&key);
        }
    }
}

impl Resource for NamespacedStore {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Done }

fn main() -> Result<(), CanoError> {
    // Use it like any other store: register it in Resources by type
    let store = NamespacedStore::new("pipeline_v2");
    store.put("status", "running".to_string());

    let _workflow: Workflow<State> = Workflow::new(
        Resources::new().insert(store.clone()),
    )
    // ... register tasks that retrieve it via
    //     res.get::<NamespacedStore>()? ...
    .add_exit_state(State::Done);
    Ok(())
}

```
</div>

<div class="callout callout-tip">
<div class="callout-label">Typed resources keep it simple</div>
<p>
<code>Resources</code> looks dependencies up by concrete type, so a custom store can expose exactly the methods it needs.
For dynamic dispatch, wrap the backend in a concrete resource type that owns an object-safe client or repository trait.
</p>
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example store_custom_backend</code>. A small
<code>NamespacedStore</code> wraps a <code>MemoryStore</code> with key prefixing and is registered as a
resource and read by type. A <code>get_shared::&lt;T&gt;()</code> demo uses <code>Arc::ptr_eq</code>
to show a large value shared between tasks without a copy.</p>
</div>

<!-- Section: Error Handling -->
<hr class="section-divider">
<h2 id="error-handling"><a href="#error-handling" class="anchor-link" aria-hidden="true">#</a>Error handling</h2>

<p>
Fallible store operations (<code>get</code>, <code>get_shared</code>, <code>append</code>) return
<code>StoreResult&lt;T&gt;</code>, which is
<code>Result&lt;T, StoreError&gt;</code>. <code>StoreError</code> converts automatically
to <code>CanoError::Store</code> via the <code>From</code> impl, so the <code>?</code>
operator works transparently in task methods. <code>put</code>, <code>remove</code>,
<code>clear</code>, <code>contains_key</code>, <code>keys</code>, <code>len</code> and
<code>is_empty</code> cannot fail and return plain values.
</p>

<div class="card-grid error-cards">
<div class="card">
<h3>KeyNotFound</h3>
<p>The requested key does not exist. Use <code>contains_key()</code> to guard optional reads, or handle the error with <code>.unwrap_or_default()</code>.</p>
</div>
<div class="card">
<h3>TypeMismatch</h3>
<p>The stored value cannot be downcast to the requested type. Ensure consistent type usage per key across all pipeline stages.</p>
</div>
<div class="card">
<h3>AppendTypeMismatch</h3>
<p>The key exists but holds a non-<code>Vec</code> value. Mixing scalar and collection writes to the same key is a logic error.</p>
</div>
</div>
</div>

