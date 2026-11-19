+++
title = "Blackboard"
description = "Blackboard in Cano: a thread-safe, in-memory typed key/value map for passing data between workflow stages."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Blackboard</h1>
<p class="subtitle">In-memory shared state for passing data between workflow stages.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.17.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>

<p>
The blackboard is the shared scratch space between workflow stages. Tasks write values as they run and read the values
produced upstream. <code>Blackboard</code> is an <code>Arc&lt;RwLock&lt;HashMap&gt;&gt;</code>
that is safe to clone and share across async tasks. A custom variant is a typed <code>Resource</code> read by its concrete type.
</p>

<p>
<code>Blackboard</code> is inserted into a <a href="../resources/"><code>Resources</code></a> map and read in tasks with
<code>res.get::&lt;Blackboard&gt;()?</code>. It is one typed dependency among others, such as database pools and HTTP clients.
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
<li><a href="#workflow-usage">Blackboard in a workflow</a></li>
<li><a href="#custom-backends">Custom blackboard resources</a></li>
<li><a href="#error-handling">Error handling</a></li>
</ol>
</nav>

<!-- Section: Thread Safety Model -->
<hr class="section-divider">
<h2 id="thread-safety"><a href="#thread-safety" class="anchor-link" aria-hidden="true">#</a>Thread safety model</h2>

<div class="diagram-frame">
<p class="diagram-label">One shared map, many readers and writers</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 24 770 263" role="img" aria-labelledby="blackboard-lock-title blackboard-lock-desc">
<title id="blackboard-lock-title">One shared Blackboard, three tasks</title>
<desc id="blackboard-lock-desc">One Blackboard, an Arc around an RwLock around a HashMap, is shared by three tasks. Task A and Task B read through get() and hold the shared read lock at the same time. Task C writes through put() and takes the exclusive write lock, so readers wait.</desc>
<defs><marker id="blackboard-lock-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M242,124 H330 Q342,124 342,112 V88 Q342,76 354,76 H494" marker-end="url(#blackboard-lock-ah)"/>
<text class="t-code t-mut" x="424" y="59">.get()</text>
<path class="e" d="M242,132 H494" marker-end="url(#blackboard-lock-ah)"/>
<text class="t-code t-mut" x="424" y="115">.get()</text>
<path class="e" d="M242,140 H330 Q342,140 342,152 V210 Q342,222 354,222 H494" marker-end="url(#blackboard-lock-ah)"/>
<text class="t-code t-mut" x="424" y="205">.put()</text>
<rect class="n-ghost" x="468" y="40" width="252" height="128" rx="12"/>
<rect class="n-hot" x="28" y="96" width="214" height="72" rx="10"/>
<text class="t-strong" x="135" y="121">Blackboard</text>
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
<code>Blackboard</code> wraps its map in <code>Arc&lt;RwLock&lt;_&gt;&gt;</code> (<code>parking_lot</code> under <code>runtime-tokio</code>, <code>std::sync</code> under <code>wasm</code>).
Readers share the lock and writers get exclusive access. Cloning the blackboard gives a second handle to the
same underlying data, not a copy.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128274;</span> Clone shares the same data</span>

```rust
let board = Blackboard::new();
let board2 = board.clone(); // same backing map, not a copy

board.put("key", 42i32);
let val: i32 = board2.get("key")?; // returns 42
assert_eq!(val, 42);
```
</div>

<!-- Section: API Reference -->
<hr class="section-divider">
<h2 id="api-reference"><a href="#api-reference" class="anchor-link" aria-hidden="true">#</a>API reference</h2>

<p>
Every public method on <code>Blackboard</code>. A facet marks a method as infallible; the others return
<code>BlackboardResult&lt;T&gt;</code>, which is <code>Result&lt;T, BlackboardError&gt;</code>.
</p>
<dl class="key-list">
<dt><code>get&lt;T: 'static + Clone&gt;(&amp;self, key: &amp;str) -&gt; BlackboardResult&lt;T&gt;</code></dt>
<dd>Returns a clone of the value. <code>KeyNotFound</code> for a missing key, <code>TypeMismatch</code> for a value of another type.</dd>

<dt><code>get_shared&lt;T: 'static + Send + Sync&gt;(&amp;self, key: &amp;str) -&gt; BlackboardResult&lt;Arc&lt;T&gt;&gt;</code></dt>
<dd>Zero-copy read: returns a clone of the stored <code>Arc</code>. Same errors as <code>get</code>.</dd>

<dt><code>put&lt;T: 'static + Send + Sync&gt;(&amp;self, key: &amp;str, value: T)</code> <small>infallible</small></dt>
<dd>Inserts or replaces the value. It does not need to implement <code>Clone</code>.</dd>

<dt><code>remove(&amp;self, key: &amp;str)</code> <small>infallible</small></dt>
<dd>Removes the key. A missing key is a no-op.</dd>

<dt><code>append&lt;T: 'static + Send + Sync + Clone&gt;(&amp;self, key: &amp;str, item: T) -&gt; BlackboardResult&lt;()&gt;</code></dt>
<dd>Pushes onto the <code>Vec&lt;T&gt;</code> at the key, creating it when the key is absent. <code>AppendTypeMismatch</code> when the key holds anything else.</dd>

<dt><code>contains_key(&amp;self, key: &amp;str) -&gt; bool</code> <small>infallible</small></dt>
<dd><code>true</code> when the key exists.</dd>

<dt><code>keys(&amp;self) -&gt; Vec&lt;Arc&lt;str&gt;&gt;</code> <small>infallible</small></dt>
<dd>All keys, in unspecified order.</dd>

<dt><code>len(&amp;self) -&gt; usize</code> <small>infallible</small></dt>
<dd>The number of entries.</dd>

<dt><code>is_empty(&amp;self) -&gt; bool</code> <small>infallible</small></dt>
<dd><code>true</code> when the blackboard holds no entries.</dd>

<dt><code>clear(&amp;self)</code> <small>infallible</small></dt>
<dd>Removes every entry.</dd>
</dl>

<!-- Section: Basic Usage -->
<hr class="section-divider">
<h2 id="basic-usage"><a href="#basic-usage" class="anchor-link" aria-hidden="true">#</a>Basic usage</h2>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128204;</span> Common blackboard operations</span>

```rust
use cano::prelude::*;

fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();

    // Store primitive and composite types (values do not need Clone for put).
    board.put("count", 42u32);
    board.put("labels", vec!["a".to_string(), "b".to_string()]);

    // `?` converts BlackboardError (KeyNotFound / TypeMismatch) into CanoError::Blackboard.
    let count: u32 = board.get("count")?;
    let labels: Vec<String> = board.get("labels")?;
    assert_eq!(count, 42);
    assert_eq!(labels.len(), 2);

    // Append to a Vec in place (creates the entry if absent).
    board.append("log", "step 1 complete".to_string())?;
    board.append("log", "step 2 complete".to_string())?;
    let log: Vec<String> = board.get("log")?;
    assert_eq!(log.len(), 2);

    // Check membership or read optional keys atomically under one lock.
    assert!(board.contains_key("count"));
    if let Ok(result) = board.get::<String>("result") {
        println!("Found optional result: {result}");
    }

    // Remove a key; missing keys are a no-op.
    board.remove("scratch");
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
<code>get_shared()</code> returns an <code>Arc&lt;T&gt;</code> pointing at the stored value. The blackboard keeps every
value in an <code>Arc</code>, so the call is a reference-count bump, not a data copy. It suits large values, such as
model weights or a dataset, that several downstream tasks read.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Zero-copy with Arc</span>

```rust
use cano::prelude::*;
use std::sync::Arc;

fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();
    // put and get_shared only need T: Send + Sync + 'static (no Clone required).
    board.put("dataset", vec![0u8; 1_000_000]); // 1 MB

    // Both handles point to the same allocation, so nothing is copied.
    let handle_a: Arc<Vec<u8>> = board.get_shared("dataset")?;
    let handle_b: Arc<Vec<u8>> = board.get_shared("dataset")?;

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

<!-- Section: Blackboard in a Workflow -->
<hr class="section-divider">
<h2 id="workflow-usage"><a href="#workflow-usage" class="anchor-link" aria-hidden="true">#</a>Blackboard in a workflow</h2>

<p>
Insert the blackboard into a <code>Resources</code> map and pass the map to <code>Workflow::new()</code>.
Each task reads it with <code>res.get::&lt;Blackboard&gt;()?</code>, so every task shares one handle.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9881;</span> End-to-end workflow with a blackboard</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { Ingest, Transform, Complete }

struct IngestTask;

#[task(state = Stage)]
impl IngestTask {
    async fn run(&self, res: &Resources) -> Result<Stage, CanoError> {
        let board = res.get::<Blackboard>()?;
        // Read the seed input inserted before orchestrate() started.
        let batch: usize = board.get("batch_size")?;
        let records: Vec<u32> = (0..batch as u32).collect();
        board.put("records", records);
        Ok(Stage::Transform)
    }
}

struct TransformTask;

#[task(state = Stage)]
impl TransformTask {
    async fn run(&self, res: &Resources) -> Result<Stage, CanoError> {
        let board = res.get::<Blackboard>()?;
        // Consume upstream data and store the final output for main.
        let records: Vec<u32> = board.get("records")?;
        let transformed: Vec<u32> = records.into_iter().map(|x| x * 2).collect();
        board.put("result", transformed);
        Ok(Stage::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let board = Blackboard::new();

    // Pre-populate inputs before the workflow starts.
    board.put("batch_size", 256usize);

    // Pass a clone into Resources so main keeps a handle to the same map.
    let workflow = Workflow::new(Resources::new().insert(board.clone()))
        .register(Stage::Ingest, IngestTask)
        .register(Stage::Transform, TransformTask)
        .add_exit_state(Stage::Complete);

    workflow.orchestrate(Stage::Ingest).await?;

    // Read results after the workflow completes.
    let result: Vec<u32> = board.get("result")?;
    println!("Processed {} records", result.len());

    Ok(())
}
```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example workflow_blackboard</code>. It uses a <code>Blackboard</code>
as a shared stack passed between workflow stages.</p>
</div>

<!-- Section: Custom Blackboard Resources -->
<hr class="section-divider">
<h2 id="custom-backends"><a href="#custom-backends" class="anchor-link" aria-hidden="true">#</a>Custom blackboard resources</h2>

<p>
For custom storage, define a concrete type with the API your workflow needs and implement <code>Resource</code> for it.
Insert it into <code>Resources</code> and read it by concrete type inside tasks. A second instance of the same type goes
under a name with <code>insert_named</code> and is read with <code>get_named</code>. Dependencies stay explicit,
with no storage trait in between.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128295;</span> Custom namespaced blackboard resource</span>

```rust
use cano::blackboard::BlackboardResult;
use cano::prelude::*;

/// Wraps a shared Blackboard and prefixes every key with "{namespace}::".
#[derive(Clone, Resource)]
pub struct NamespacedBoard {
    namespace: String,
    inner: Blackboard,
}

impl NamespacedBoard {
    pub fn new(namespace: impl Into<String>, inner: Blackboard) -> Self {
        Self {
            namespace: namespace.into(),
            inner,
        }
    }

    fn ns_key(&self, key: &str) -> String {
        format!("{}::{key}", self.namespace)
    }

    pub fn get<T: 'static + Clone>(&self, key: &str) -> BlackboardResult<T> {
        self.inner.get(&self.ns_key(key))
    }

    pub fn put<T: 'static + Send + Sync>(&self, key: &str, value: T) {
        self.inner.put(&self.ns_key(key), value);
    }

    pub fn remove(&self, key: &str) {
        self.inner.remove(&self.ns_key(key));
    }

    pub fn append<T: 'static + Send + Sync + Clone>(
        &self,
        key: &str,
        item: T,
    ) -> BlackboardResult<()> {
        self.inner.append(&self.ns_key(key), item)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(&self.ns_key(key))
    }

    pub fn keys(&self) -> Vec<String> {
        // Return unprefixed keys belonging only to this namespace.
        let prefix = format!("{}::", self.namespace);
        self.inner
            .keys()
            .into_iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
            .collect()
    }

    pub fn len(&self) -> usize {
        // Count matching keys without allocating stripped Strings.
        let prefix = format!("{}::", self.namespace);
        self.inner
            .keys()
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .count()
    }

    pub fn clear(&self) {
        // Remove only keys belonging to this namespace.
        for key in self.keys() {
            self.remove(&key);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Done }

struct WriteStageTask;

#[task(state = State)]
impl WriteStageTask {
    async fn run(&self, res: &Resources) -> Result<State, CanoError> {
        // Multiple NamespacedBoard views can share one Blackboard safely.
        let ingest = res.get_named::<NamespacedBoard>("ingest")?;
        let metrics = res.get_named::<NamespacedBoard>("metrics")?;
        ingest.put("status", "complete".to_string());
        metrics.put("status", 200u16);
        Ok(State::Done)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let shared = Blackboard::new();
    let ingest = NamespacedBoard::new("ingest", shared.clone());
    let metrics = NamespacedBoard::new("metrics", shared);

    // Keep an ingest handle so main can inspect keys after orchestrate() completes.
    let workflow = Workflow::new(
        Resources::new()
            .insert_named("ingest", ingest.clone())
            .insert_named("metrics", metrics),
    )
    .register(State::Start, WriteStageTask)
    .add_exit_state(State::Done);

    workflow.orchestrate(State::Start).await?;
    assert_eq!(ingest.get::<String>("status")?, "complete");
    Ok(())
}
```
</div>

<div class="callout callout-tip">
<div class="callout-label">Typed resources keep it simple</div>
<p>
<code>Resources</code> looks dependencies up by concrete type, so a custom blackboard can expose exactly the methods it needs.
For dynamic dispatch, wrap the backend in a concrete resource type that owns an object-safe client or repository trait.
</p>
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example blackboard_namespaced</code>. A small
<code>NamespacedBoard</code> wraps a <code>Blackboard</code> with key prefixing and is registered as a
resource and read by type. A <code>get_shared::&lt;T&gt;()</code> demo uses <code>Arc::ptr_eq</code>
to show a large value shared between tasks without a copy.</p>
</div>

<!-- Section: Error Handling -->
<hr class="section-divider">
<h2 id="error-handling"><a href="#error-handling" class="anchor-link" aria-hidden="true">#</a>Error handling</h2>

<p>
Fallible blackboard operations (<code>get</code>, <code>get_shared</code>, <code>append</code>) return
<code>BlackboardResult&lt;T&gt;</code>, which is
<code>Result&lt;T, BlackboardError&gt;</code>. <code>BlackboardError</code> converts automatically
to <code>CanoError::Blackboard</code> via the <code>From</code> impl, so the <code>?</code>
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
