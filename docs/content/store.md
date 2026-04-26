+++
title = "Store"
description = "Learn how to use the Store in Cano - thread-safe shared state for pipeline data passing between workflow stages."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Store</h1>
<p class="subtitle">Thread-safe shared state for pipeline data passing between workflow stages.</p>

<p>
The store is the shared data layer that connects workflow stages. Tasks and nodes write
values during their execution and read values produced by upstream stages. The default
implementation, <code>MemoryStore</code>, is an <code>Arc&lt;RwLock&lt;HashMap&gt;&gt;</code>
that is safe to clone and share across async tasks. Custom backends implement the
<code>KeyValueStore</code> trait.
</p>

<p>
<code>MemoryStore</code> is registered inside a <a href="../resources/"><code>Resources</code></a>
dictionary and retrieved in tasks and nodes with
<code>res.get::&lt;MemoryStore, str&gt;("store")?</code>. This makes the store one
of many named dependencies a workflow can carry, alongside database pools, HTTP clients,
configuration, or any other type that implements the <code>Resource</code> trait. See
<a href="../resources/">Resources</a> for lifecycle semantics, key-type options, and
the full <code>insert</code> / <code>get</code> API.
</p>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
The store is the glue between workflow stages. Each task reads input from the store and writes
its output back, creating a natural data pipeline without tight coupling between stages.
</p>
</div>

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

<div class="mermaid">
graph LR
S["Arc&lt;RwLock&lt;HashMap&gt;&gt;"]
S --> R1["Task A (read)"]
S --> R2["Task B (read)"]
S --> W1["Task C (write)"]
style S fill:#1e293b,stroke:#38bdf8
</div>

<p>
<code>MemoryStore</code> uses <code>Arc&lt;RwLock&lt;_&gt;&gt;</code> for interior mutability.
Multiple readers hold shared locks concurrently; writers get exclusive access.
Because <code>MemoryStore</code> wraps its inner map in <code>Arc</code>, cloning the store
produces a second handle to the <em>same</em> underlying data -- not a copy.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128274;</span> Clone shares the same data</span>
<pre><code class="language-rust">let store = MemoryStore::new();
let store2 = store.clone(); // same backing map — not a copy
<!--blank-->
store.put("key", 42i32)?;
let val: i32 = store2.get("key")?; // returns 42</code></pre>
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
<td><code>get_shared&lt;T: Send + Sync + Clone&gt;(&amp;self, key: &amp;str) -&gt; StoreResult&lt;Arc&lt;T&gt;&gt;</code></td>
<td>Zero-copy read. Returns the stored <code>Arc</code> pointer directly when possible.</td>
</tr>
<tr>
<td><code>put</code></td>
<td><code>put&lt;T: Send + Sync + Clone&gt;(&amp;self, key: &amp;str, value: T) -&gt; StoreResult&lt;()&gt;</code></td>
<td>Inserts or replaces. Accepts any type.</td>
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
<td><code>keys(&amp;self) -&gt; StoreResult&lt;Vec&lt;String&gt;&gt;</code></td>
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
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
let store = MemoryStore::new();
<!--blank-->
// Store primitive and composite types
store.put("count", 42u32)?;
store.put("labels", vec!["a".to_string(), "b".to_string()])?;
<!--blank-->
// Retrieve with explicit type annotation
let count: u32 = store.get("count")?;
let labels: Vec<String> = store.get("labels")?;
<!--blank-->
// Append to a Vec (creates if absent)
store.append("log", "step 1 complete".to_string())?;
store.append("log", "step 2 complete".to_string())?;
let log: Vec<String> = store.get("log")?;
<!--blank-->
// Check existence before reading optional keys
if store.contains_key("result")? {
    let result: String = store.get("result")?;
}
<!--blank-->
// Remove a key — silent no-op if absent
store.remove("scratch")?;</code></pre>
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
<pre><code class="language-rust">use cano::prelude::*;
use std::sync::Arc;
<!--blank-->
let store = MemoryStore::new();
store.put("dataset", vec![0u8; 1_000_000])?; // 1 MB
<!--blank-->
// Both handles point to the same allocation — no copy
let handle_a: Arc<Vec<u8>> = store.get_shared("dataset")?;
let handle_b: Arc<Vec<u8>> = store.get_shared("dataset")?;
<!--blank-->
assert!(Arc::ptr_eq(&handle_a, &handle_b));</code></pre>
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
<code>res.get::&lt;MemoryStore, str&gt;("store")?</code>, so the same handle is shared
across all registered tasks for the duration of a single <code>orchestrate()</code> call.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9881;</span> End-to-end workflow with store</span>
<pre><code class="language-rust">use async_trait::async_trait;
use cano::prelude::*;
<!--blank-->
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Ingest, Transform, Complete }
<!--blank-->
#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
<!--blank-->
    // Pre-populate before the workflow starts
    store.put("batch_size", 256usize)?;
<!--blank-->
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(Stage::Ingest, |res: &Resources| async move {
            let store = res.get::<MemoryStore, str>("store")?;
            let batch: usize = store.get("batch_size")?;
            let records: Vec<u32> = (0..batch as u32).collect();
            store.put("records", records)?;
            Ok(TaskResult::Single(Stage::Transform))
        })
        .register(Stage::Transform, |res: &Resources| async move {
            let store = res.get::<MemoryStore, str>("store")?;
            let records: Vec<u32> = store.get("records")?;
            let transformed: Vec<u32> = records.into_iter().map(|x| x * 2).collect();
            store.put("result", transformed)?;
            Ok(TaskResult::Single(Stage::Complete))
        })
        .add_exit_state(Stage::Complete);
<!--blank-->
    workflow.orchestrate(Stage::Ingest).await?;
<!--blank-->
    // Read results after the workflow completes
    let result: Vec<u32> = store.get("result")?;
    println!("Processed {} records", result.len());
<!--blank-->
    Ok(())
}</code></pre>
</div>

<!-- Section: Custom Store Backends -->
<hr class="section-divider">
<h2 id="custom-backends"><a href="#custom-backends" class="anchor-link" aria-hidden="true">#</a>Custom Store Backends</h2>

<p>
Implement <code>KeyValueStore</code> to plug in any storage backend: a database,
a distributed cache, a file-backed store, or an in-process channel. The trait
requires <code>Send + Sync</code>, so implementations typically use interior mutability.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128295;</span> Custom namespaced store backend</span>
<pre><code class="language-rust">use cano::store::{KeyValueStore, StoreResult, StoreError};
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
<!--blank-->
/// Example: a store that prefixes all keys with a namespace.
#[derive(Clone)]
pub struct NamespacedStore {
    namespace: String,
    inner: MemoryStore,
}
<!--blank-->
impl NamespacedStore {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            inner: MemoryStore::new(),
        }
    }
<!--blank-->
    fn ns_key(&self, key: &str) -> String {
        format!("{}::{}", self.namespace, key)
    }
}
<!--blank-->
impl KeyValueStore for NamespacedStore {
    fn get&lt;T: 'static + Clone&gt;(&self, key: &str) -> StoreResult&lt;T&gt; {
        self.inner.get(&self.ns_key(key))
    }
<!--blank-->
    fn put&lt;T: 'static + Send + Sync + Clone&gt;(&self, key: &str, value: T) -> StoreResult&lt;()&gt; {
        self.inner.put(&self.ns_key(key), value)
    }
<!--blank-->
    fn remove(&self, key: &str) -> StoreResult&lt;()&gt; {
        self.inner.remove(&self.ns_key(key))
    }
<!--blank-->
    fn append&lt;T: 'static + Send + Sync + Clone&gt;(&self, key: &str, item: T) -> StoreResult&lt;()&gt; {
        self.inner.append(&self.ns_key(key), item)
    }
<!--blank-->
    fn contains_key(&self, key: &str) -> StoreResult&lt;bool&gt; {
        self.inner.contains_key(&self.ns_key(key))
    }
<!--blank-->
    fn keys(&self) -> StoreResult&lt;Vec&lt;String&gt;&gt; {
        // Strip the namespace prefix before returning keys to callers
        let prefix = format!("{}::", self.namespace);
        let raw_keys = self.inner.keys()?;
        Ok(raw_keys
            .into_iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
            .collect())
    }
<!--blank-->
    fn len(&self) -> StoreResult&lt;usize&gt; {
        // Count only keys in this namespace
        Ok(self.keys()?.len())
    }
<!--blank-->
    fn clear(&self) -> StoreResult&lt;()&gt; {
        // Remove only keys in this namespace
        for key in self.keys()? {
            self.remove(&key)?;
        }
        Ok(())
    }
}
<!--blank-->
// Use it like any other store — register it in Resources with your own key
let store = NamespacedStore::new("pipeline_v2");
store.put("status", "running".to_string())?;
<!--blank-->
let workflow = Workflow::new(
        Resources::new().insert("namespaced_store", store.clone()),
    )
    // ... register tasks that retrieve it via
    //     res.get::<NamespacedStore, str>("namespaced_store")? ...
    ;</code></pre>
</div>

<div class="callout callout-warning">
<div class="callout-label">Trait object note</div>
<p>
<code>KeyValueStore</code> is not object-safe due to its generic methods.
You cannot store it as <code>Box&lt;dyn KeyValueStore&gt;</code> directly. Register a
concrete store type in <code>Resources</code> and retrieve it with its concrete type
(<code>res.get::&lt;MyStore, _&gt;("my_store")?</code>), or wrap the store in a newtype
that exposes a non-generic API.
</p>
</div>

<!-- Section: Error Handling -->
<hr class="section-divider">
<h2 id="error-handling"><a href="#error-handling" class="anchor-link" aria-hidden="true">#</a>Error Handling</h2>

<p>
Store operations return <code>StoreResult&lt;T&gt;</code>, which is
<code>Result&lt;T, StoreError&gt;</code>. <code>StoreError</code> converts automatically
to <code>CanoError::Store</code> via the <code>From</code> impl, so the <code>?</code>
operator works transparently in task and node methods.
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

