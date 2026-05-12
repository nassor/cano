+++
title = "MemoryStore"
description = "MemoryStore in Cano - the default thread-safe, in-memory shared state for passing data between workflow stages."
template = "page.html"
+++

<div class="content-wrapper">
<h1>MemoryStore</h1>
<p class="subtitle">The default in-memory shared state for passing data between workflow stages.</p>

<p>
The store is the shared data layer that connects workflow stages. Tasks write
values during their execution and read values produced by upstream stages. The default
implementation, <code>MemoryStore</code>, is an <code>Arc&lt;RwLock&lt;HashMap&gt;&gt;</code>
that is safe to clone and share across async tasks. Custom backends can be registered
as typed <code>Resource</code> values and retrieved by concrete type.
</p>

<p>
<code>MemoryStore</code> is registered inside a <a href="../resources/"><code>Resources</code></a>
dictionary and retrieved in tasks with
<code>res.get::&lt;MemoryStore, _&gt;("store")?</code>. This makes the store one
of many named dependencies a workflow can carry, alongside database pools, HTTP clients,
configuration, or any other type that implements the <code>Resource</code> trait. See
<a href="../resources/">Resources</a> for lifecycle semantics, key-type options, and
the full <code>insert</code> / <code>get</code> API.
</p>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#thread-safety">Thread Safety Model</a></li>
<li><a href="#api-reference">API Reference</a></li>
<li><a href="#basic-usage">Basic Usage</a></li>
<li><a href="#zero-copy">Zero-Copy Reads</a></li>
<li><a href="#workflow-usage">Store in a Workflow</a></li>
<li><a href="#custom-backends">Custom Store Backends</a></li>
<li><a href="#error-handling">Error Handling</a></li>
</ol>
</nav>

<!-- Section: Thread Safety Model -->
<hr class="section-divider">
<h2 id="thread-safety"><a href="#thread-safety" class="anchor-link" aria-hidden="true">#</a>Thread Safety Model</h2>

<div class="diagram-frame">
<p class="diagram-label">One shared map, many readers and writers</p>
<div class="mermaid">
graph LR
S["Arc&lt;RwLock&lt;HashMap&gt;&gt;"]
S --> R1["Task A (read)"]
S --> R2["Task B (read)"]
S --> W1["Task C (write)"]
style S fill:#1e293b,stroke:#38bdf8
</div>
</div>

<p>
<code>MemoryStore</code> uses <code>Arc&lt;RwLock&lt;_&gt;&gt;</code> for interior mutability.
Multiple readers hold shared locks concurrently; writers get exclusive access.
Because <code>MemoryStore</code> wraps its inner map in <code>Arc</code>, cloning the store
produces a second handle to the <em>same</em> underlying data -- not a copy.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128274;</span> Clone shares the same data</span>

```rust
let store = MemoryStore::new();
let store2 = store.clone(); // same backing map — not a copy

store.put("key", 42i32)?;
let val: i32 = store2.get("key")?; // returns 42

```
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
<td><code>get</code></td>
<td><code>get&lt;T: Clone&gt;(&amp;self, key: &amp;str) -&gt; StoreResult&lt;T&gt;</code></td>
<td>Returns a clone. Errors on missing key or type mismatch.</td>
</tr>
<tr>
<td><code>get_shared</code></td>
<td><code>get_shared&lt;T: Send + Sync&gt;(&amp;self, key: &amp;str) -&gt; StoreResult&lt;Arc&lt;T&gt;&gt;</code></td>
<td>Zero-copy read. Returns the stored <code>Arc</code> pointer directly when possible.</td>
</tr>
<tr>
<td><code>put</code></td>
<td><code>put&lt;T: Send + Sync&gt;(&amp;self, key: &amp;str, value: T) -&gt; StoreResult&lt;()&gt;</code></td>
<td>Inserts or replaces. Values do not need to implement <code>Clone</code>.</td>
</tr>
<tr>
<td><code>remove</code></td>
<td><code>remove(&amp;self, key: &amp;str) -&gt; StoreResult&lt;()&gt;</code></td>
<td>Silent no-op on missing keys. Errors only on lock failure.</td>
</tr>
<tr>
<td><code>append</code></td>
<td><code>append&lt;T: Send + Sync + Clone&gt;(&amp;self, key: &amp;str, item: T) -&gt; StoreResult&lt;()&gt;</code></td>
<td>Appends to an existing <code>Vec&lt;T&gt;</code>, or creates one. Type mismatch errors if the key holds a non-Vec.</td>
</tr>
<tr>
<td><code>contains_key</code></td>
<td><code>contains_key(&amp;self, key: &amp;str) -&gt; StoreResult&lt;bool&gt;</code></td>
<td>Returns <code>Ok(true)</code> if the key exists.</td>
</tr>
<tr>
<td><code>keys</code></td>
<td><code>keys(&amp;self) -&gt; StoreResult&lt;Vec&lt;Arc&lt;str&gt;&gt;&gt;</code></td>
<td>Returns all keys. Order is unspecified (HashMap).</td>
</tr>
<tr>
<td><code>len</code></td>
<td><code>len(&amp;self) -&gt; StoreResult&lt;usize&gt;</code></td>
<td>Number of key-value pairs currently stored.</td>
</tr>
<tr>
<td><code>is_empty</code></td>
<td><code>is_empty(&amp;self) -&gt; StoreResult&lt;bool&gt;</code></td>
<td>Default impl delegates to <code>len()</code>.</td>
</tr>
<tr>
<td><code>clear</code></td>
<td><code>clear(&amp;self) -&gt; StoreResult&lt;()&gt;</code></td>
<td>Removes all entries.</td>
</tr>
</tbody>
</table>

<!-- Section: Basic Usage -->
<hr class="section-divider">
<h2 id="basic-usage"><a href="#basic-usage" class="anchor-link" aria-hidden="true">#</a>Basic Usage</h2>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128204;</span> Common store operations</span>

```rust
use cano::prelude::*;

fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Store primitive and composite types
    store.put("count", 42u32)?;
    store.put("labels", vec!["a".to_string(), "b".to_string()])?;

    // Retrieve with explicit type annotation
    let count: u32 = store.get("count")?;
    let labels: Vec<String> = store.get("labels")?;

    // Append to a Vec (creates if absent)
    store.append("log", "step 1 complete".to_string())?;
    store.append("log", "step 2 complete".to_string())?;
    let log: Vec<String> = store.get("log")?;

    // Check existence before reading optional keys
    if store.contains_key("result")? {
        let result: String = store.get("result")?;
    }

    // Remove a key — silent no-op if absent
    store.remove("scratch")?;
    Ok(())
}

```
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
Use <code>contains_key()</code> before <code>get()</code> for optional keys, or handle the
<code>KeyNotFound</code> error directly. The <code>append()</code> method is handy for
building up collections across multiple workflow stages without overwriting previous entries.
</p>
</div>

<!-- Section: Zero-Copy Reads -->
<hr class="section-divider">
<h2 id="zero-copy"><a href="#zero-copy" class="anchor-link" aria-hidden="true">#</a>Zero-Copy Reads with get_shared()</h2>

<p>
<code>get_shared()</code> returns an <code>Arc&lt;T&gt;</code> pointing to the value
in the store. When <code>MemoryStore</code> stored the value, it wrapped it in an
<code>Arc</code> internally. <code>get_shared()</code> returns a clone of that pointer --
a cheap reference-count bump, not a data copy. This is useful when passing large
values (e.g., a model's weight tensor or a large dataset) to multiple downstream tasks.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Zero-copy with Arc</span>

```rust
use cano::prelude::*;
use std::sync::Arc;

fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
    store.put("dataset", vec![0u8; 1_000_000])?; // 1 MB

    // Both handles point to the same allocation — no copy
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
Prefer <code>get_shared()</code> for large or frequently-read values.
</p>
</div>
</div>

<!-- Section: Store in a Workflow -->
<hr class="section-divider">
<h2 id="workflow-usage"><a href="#workflow-usage" class="anchor-link" aria-hidden="true">#</a>Store in a Workflow</h2>

<p>
Register the store inside a <code>Resources</code> dictionary and pass it to
<code>Workflow::new()</code>. Each task retrieves the store via
<code>res.get::&lt;MemoryStore, _&gt;("store")?</code>, so the same handle is shared
across all registered tasks for the duration of a single <code>orchestrate()</code> call.
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
    async fn run(&self, res: &Resources) -> Result<TaskResult<Stage>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let batch: usize = store.get("batch_size")?;
        let records: Vec<u32> = (0..batch as u32).collect();
        store.put("records", records)?;
        Ok(TaskResult::Single(Stage::Transform))
    }
}

struct TransformTask;

#[task(state = Stage)]
impl TransformTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<Stage>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let records: Vec<u32> = store.get("records")?;
        let transformed: Vec<u32> = records.into_iter().map(|x| x * 2).collect();
        store.put("result", transformed)?;
        Ok(TaskResult::Single(Stage::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Pre-populate before the workflow starts
    store.put("batch_size", 256usize)?;

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
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
<p>Runnable example: <code>cargo run --example workflow_stack_store</code> — uses a <code>MemoryStore</code>
as a shared stack passed between workflow stages.</p>
</div>

<!-- Section: Custom Store Backends -->
<hr class="section-divider">
<h2 id="custom-backends"><a href="#custom-backends" class="anchor-link" aria-hidden="true">#</a>Custom Store Resources</h2>

<p>
For custom storage, define a concrete type with the API your workflow needs and
implement <code>Resource</code> for it. Register that type in <code>Resources</code>, then retrieve it by
concrete type inside tasks. This keeps storage dependencies explicit and avoids
an unnecessary storage trait layer now that <code>Resources</code> provides typed dependency lookup.
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

    pub fn put<T: 'static + Send + Sync>(&self, key: &str, value: T) -> StoreResult<()> {
        self.inner.put(&self.ns_key(key), value)
    }

    pub fn remove(&self, key: &str) -> StoreResult<()> {
        self.inner.remove(&self.ns_key(key))
    }

    pub fn append<T: 'static + Send + Sync + Clone>(&self, key: &str, item: T) -> StoreResult<()> {
        self.inner.append(&self.ns_key(key), item)
    }

    pub fn contains_key(&self, key: &str) -> StoreResult<bool> {
        self.inner.contains_key(&self.ns_key(key))
    }

    pub fn keys(&self) -> StoreResult<Vec<String>> {
        // Strip the namespace prefix before returning keys to callers
        let prefix = format!("{}::", self.namespace);
        let raw_keys = self.inner.keys()?;
        Ok(raw_keys
            .into_iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
            .collect())
    }

    pub fn len(&self) -> StoreResult<usize> {
        // Count only keys in this namespace
        Ok(self.keys()?.len())
    }

    pub fn clear(&self) -> StoreResult<()> {
        // Remove only keys in this namespace
        for key in self.keys()? {
            self.remove(&key)?;
        }
        Ok(())
    }
}

impl Resource for NamespacedStore {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Done }

fn main() -> Result<(), CanoError> {
    // Use it like any other store — register it in Resources with your own key
    let store = NamespacedStore::new("pipeline_v2");
    store.put("status", "running".to_string())?;

    let _workflow: Workflow<State> = Workflow::new(
        Resources::new().insert("namespaced_store", store.clone()),
    )
    // ... register tasks that retrieve it via
    //     res.get::<NamespacedStore, _>("namespaced_store")? ...
    .add_exit_state(State::Done);
    Ok(())
}

```
</div>

<div class="callout callout-tip">
<div class="callout-label">Typed resources keep it simple</div>
<p>
Because <code>Resources</code> retrieves dependencies by concrete type, your custom store can expose
the exact methods it needs. If you need dynamic dispatch, wrap your backend in a concrete
resource type that owns an object-safe client or repository trait.
</p>
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example store_custom_backend</code> — a small
<code>NamespacedStore</code> wrapping a <code>MemoryStore</code> with key prefixing, registered as a
resource and retrieved by type, plus a <code>get_shared::&lt;T&gt;()</code> demo (<code>Arc::ptr_eq</code>
proves the zero-copy share of a large value between tasks).</p>
</div>

<!-- Section: Error Handling -->
<hr class="section-divider">
<h2 id="error-handling"><a href="#error-handling" class="anchor-link" aria-hidden="true">#</a>Error Handling</h2>

<p>
Store operations return <code>StoreResult&lt;T&gt;</code>, which is
<code>Result&lt;T, StoreError&gt;</code>. <code>StoreError</code> converts automatically
to <code>CanoError::Store</code> via the <code>From</code> impl, so the <code>?</code>
operator works transparently in task methods.
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
<h3>LockError</h3>
<p>The internal <code>RwLock</code> is poisoned -- a thread panicked while holding the lock. Typically fatal; log and restart.</p>
</div>
<div class="card">
<h3>AppendTypeMismatch</h3>
<p>The key exists but holds a non-<code>Vec</code> value. Mixing scalar and collection writes to the same key is a logic error.</p>
</div>
</div>
</div>

