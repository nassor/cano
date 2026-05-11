+++
title = "Nodes"
description = "Learn how to use Nodes in Cano - structured, resilient processing units with a three-phase lifecycle."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Nodes</h1>
<p class="subtitle">Structured, resilient processing units with a three-phase lifecycle.</p>

<p>
A <code>Node</code> implements a structured three-phase lifecycle with built-in retry capabilities.
Nodes are ideal for complex operations where separating data loading, execution, and result handling improves clarity and maintainability.
</p>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
Nodes separate <em>what data to load</em> (prep), <em>how to process it</em> (exec), and <em>where to store results</em> (post).
On any phase failure the entire <code>prep</code> &rarr; <code>exec</code> &rarr; <code>post</code> pipeline is retried from scratch,
so all three phases must be idempotent. <code>prep</code> and <code>post</code> both receive
<code>&amp;Resources</code> — see <a href="../resources/">Resources</a> for how to register and retrieve typed dependencies.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#three-phases">The Three Phases</a></li>
<li><a href="#quick-start">Quick Start with Nodes</a></li>
<li><a href="#implementing">Implementing a Node (Explicit Form)</a></li>
<li><a href="#nodes-vs-tasks">Nodes vs Tasks</a></li>
<li><a href="#patterns">Real-World Node Patterns</a></li>
<li><a href="#config">Configuration Best Practices</a></li>
</ol>
</nav>

<!-- Section: The Three Phases -->
<hr class="section-divider">
<h2 id="three-phases"><a href="#three-phases" class="anchor-link" aria-hidden="true">#</a>The Three Phases</h2>

<div class="mermaid">
graph LR
A[Prep] -->|Load Data| B[Exec]
B -->|Process| C[Post]
C -->|Save Result| D[Next State]
</div>

<!-- Visual lifecycle cards with step numbers and arrows -->
<div class="lifecycle-flow">
<div class="lifecycle-phase phase-prep">
<div class="phase-step">
<span class="phase-number">1</span>
<h3>Prep</h3>
</div>
<p>Load data from the store, validate inputs, and setup resources. Returns <code>PrepResult</code>.</p>
<span class="phase-tag">Runs once</span>
</div>

<div class="lifecycle-arrow">
<svg viewBox="0 0 24 24"><path d="M5 12h14M13 6l6 6-6 6"/></svg>
</div>

<div class="lifecycle-phase phase-exec">
<div class="phase-step">
<span class="phase-number">2</span>
<h3>Exec</h3>
</div>
<p>Core processing logic. No store access — receives <code>PrepResult</code> and returns <code>ExecResult</code>. Must be idempotent.</p>
<span class="phase-tag">Must be idempotent</span>
</div>

<div class="lifecycle-arrow">
<svg viewBox="0 0 24 24"><path d="M5 12h14M13 6l6 6-6 6"/></svg>
</div>

<div class="lifecycle-phase phase-post">
<div class="phase-step">
<span class="phase-number">3</span>
<h3>Post</h3>
</div>
<p>Store results, cleanup resources, and determine the next workflow state based on execution outcome.</p>
<span class="phase-tag">Runs once</span>
</div>
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
Keep IO operations (database reads, file access) in <code>prep</code> and <code>post</code>.
The <code>exec</code> phase has no store access by design — treat it as a pure computation.
Because the entire pipeline restarts on any failure, making all three phases idempotent
is the safest approach.
</p>
</div>

<!-- Section: Quick Start with #[task::node] inference -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start with <code>#[task::node(state = ...)]</code></h2>
<p>
The recommended form attaches <code>#[task::node(state = MyState)]</code> to an inherent
<code>impl MyNode { ... }</code> block. The macro:
</p>

<ul>
<li>Builds the <code>impl Node&lt;MyState&gt; for MyNode</code> trait header from the attribute</li>
<li>Enforces that <code>prep</code>, <code>exec</code>, and <code>post</code> are all present (compile-time error otherwise)</li>
<li>Infers <code>type PrepResult</code> from the return type of <code>prep</code> (peels <code>Result&lt;T, _&gt;</code>)</li>
<li>Infers <code>type ExecResult</code> from the return type of <code>exec</code> (no peel — <code>exec</code> is infallible)</li>
<li>Injects <code>fn config(&amp;self) -&gt; TaskConfig { TaskConfig::default() }</code> if absent</li>
</ul>

<p>
You can still write any of these explicitly — explicit always wins. For workflows that
use a custom resource-key type, pass it via <code>#[task::node(state = S, key = K)]</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form — no <code>type PrepResult</code>, <code>type ExecResult</code>, or <code>fn config</code> needed</span>

```rust
use cano::prelude::*;
use rand::Rng;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowAction { Generate, Count, Complete, Error }

#[derive(Clone)]
struct GeneratorNode;

#[task::node(state = WorkflowAction)]
impl GeneratorNode {
    // prep: return type Vec<u32> is inferred as PrepResult
    async fn prep(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        let mut rng = rand::rng();
        let size = rng.random_range(25..=150);
        let numbers: Vec<u32> = (0..size).map(|_| rng.random_range(1..=1000)).collect();
        println!("Generated {} random numbers", numbers.len());
        Ok(numbers)
    }

    // exec: return type Vec<u32> is inferred as ExecResult (infallible — no Result wrapper)
    async fn exec(&self, prep_res: Vec<u32>) -> Vec<u32> {
        let even_numbers: Vec<u32> = prep_res.into_iter().filter(|&n| n % 2 == 0).collect();
        println!("Filtered to {} even numbers", even_numbers.len());
        even_numbers
    }

    // post: receives Vec<u32> (the ExecResult) and returns the next state
    async fn post(
        &self,
        res: &Resources,
        exec_res: Vec<u32>,
    ) -> Result<WorkflowAction, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("filtered_numbers", exec_res)?;
        println!("Generator node completed");
        Ok(WorkflowAction::Count)
    }
}

```
</div>

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
Use <code>#[derive(FromResources)]</code> to pull multiple resources out of the map in a single
destructure. See the <a href="../resources/">Resources</a> page for the full <code>FromResources</code>
reference.
</p>
</div>

<!-- Section: Implementing a Node -->
<hr class="section-divider">
<h2 id="implementing"><a href="#implementing" class="anchor-link" aria-hidden="true">#</a>Implementing a Node (Explicit Form)</h2>
<p>
When you need custom retry config or prefer explicit associated types, you can write any or all
of <code>type PrepResult</code>, <code>type ExecResult</code>, and <code>fn config</code> yourself
inside the inherent <code>impl</code> block. The macro still synthesises the trait header from
<code>state = ...</code>; explicit declarations take precedence over inferred ones.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form — custom retry config and explicit associated types</span>

```rust
use cano::prelude::*;
use rand::Rng;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowAction { Generate, Count, Complete, Error }

#[derive(Clone)]
struct GeneratorNode;

#[task::node(state = WorkflowAction)]
impl GeneratorNode {
    // Explicit associated types override inference
    type PrepResult = Vec<u32>;
    type ExecResult = Vec<u32>;

    // Explicit config overrides the injected default
    fn config(&self) -> TaskConfig {
        TaskConfig::default().with_fixed_retry(3, Duration::from_secs(1))
    }

    // Phase 1: Preparation
    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let mut rng = rand::rng();
        let size = rng.random_range(25..=150);
        let numbers: Vec<u32> = (0..size).map(|_| rng.random_range(1..=1000)).collect();

        println!("Generated {} random numbers", numbers.len());
        Ok(numbers)
    }

    // Phase 2: Execution — infallible, returns ExecResult directly
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        let even_numbers: Vec<u32> = prep_res.into_iter().filter(|&n| n % 2 == 0).collect();
        println!("Filtered to {} even numbers", even_numbers.len());
        even_numbers
    }

    // Phase 3: Post-processing
    async fn post(
        &self,
        res: &Resources,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("filtered_numbers", exec_res)?;

        println!("Generator node completed");
        Ok(WorkflowAction::Count)
    }
}

```
</div>

<!-- Section: Nodes vs Tasks -->
<hr class="section-divider">
<h2 id="nodes-vs-tasks"><a href="#nodes-vs-tasks" class="anchor-link" aria-hidden="true">#</a>Nodes vs Tasks</h2>
<p>
Every <code>Node</code> automatically implements <code>Task</code>, so you can use them interchangeably.
</p>

<table class="styled-table">
<thead>
<tr>
<th>Feature</th>
<th>Task</th>
<th>Node</th>
</tr>
</thead>
<tbody>
<tr>
<td>Structure</td>
<td>Single <code>run</code> method</td>
<td>3 phases: Prep, Exec, Post</td>
</tr>
<tr>
<td>Retry scope</td>
<td>Entire <code>run()</code> call</td>
<td>Entire <code>prep</code> &rarr; <code>exec</code> &rarr; <code>post</code> pipeline</td>
</tr>
<tr>
<td>Complexity</td>
<td>Low</td>
<td>Medium</td>
</tr>
<tr>
<td>Use Case</td>
<td>Simple logic, prototypes</td>
<td>Production logic, complex flows</td>
</tr>
</tbody>
</table>

<div class="callout callout-info">
<div class="callout-label">Blanket impl</div>
<p>
Because every <code>Node</code> automatically implements <code>Task</code>,
you can freely mix both types when calling <code>Workflow::register()</code>.
See the <a href="../task/">Tasks</a> page for the simpler interface.
</p>
</div>

<!-- Section: Real-World Patterns -->
<hr class="section-divider">
<h2 id="patterns"><a href="#patterns" class="anchor-link" aria-hidden="true">#</a>Real-World Node Patterns</h2>
<p>Nodes provide structure for complex workflows. Here are proven patterns from production systems.</p>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">1</span>
<h3>ETL (Extract, Transform, Load) Pattern</h3>
</div>
<p>The three-phase lifecycle naturally maps to ETL operations.</p>

<div class="mermaid">
graph LR
A[Prep: Extract] -->|Load from source| B[Exec: Transform]
B -->|Process data| C[Post: Load]
C -->|Save to destination| D[Next State]
</div>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128230;</span> ETL Node (illustrative — Record, ProcessedRecord, and database helpers are application-defined)</span>

```rust
use cano::prelude::*;

// Application-defined types and helpers — stubbed here so the example compiles
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Extract, Transform, Load, Complete }

#[derive(Clone)]
struct Record;

#[derive(Clone)]
struct ProcessedRecord;

async fn load_from_database(_src: &str) -> Result<Vec<Record>, CanoError> {
    Ok(vec![Record])
}

async fn save_to_database(_dst: &str, _data: &[ProcessedRecord]) -> Result<(), CanoError> {
    Ok(())
}

fn process_record(_r: Record) -> ProcessedRecord {
    ProcessedRecord
}

#[derive(Clone)]
struct ETLNode {
    source: String,
    destination: String,
}

#[task::node(state = State)]
impl ETLNode {
    type PrepResult = Vec<Record>;
    type ExecResult = Vec<ProcessedRecord>;

    fn config(&self) -> TaskConfig {
        TaskConfig::default()
            .with_exponential_retry(3) // Retry failures
    }

    // Extract: Load data from source
    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        println!("📥 Extracting from: {}", self.source);

        let records = load_from_database(&self.source).await?;
        println!("Loaded {} records", records.len());

        Ok(records)
    }

    // Transform: Process the data
    async fn exec(&self, records: Self::PrepResult) -> Self::ExecResult {
        println!("⚙️  Transforming {} records...", records.len());

        records.into_iter()
            .map(|r| process_record(r))
            .collect()
    }

    // Load: Save to destination
    async fn post(&self, res: &Resources, processed: Self::ExecResult)
        -> Result<State, CanoError> {
        println!("📤 Loading to: {}", self.destination);

        save_to_database(&self.destination, &processed).await?;
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("processed_count", processed.len())?;

        Ok(State::Complete)
    }
}

```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">2</span>
<h3>Negotiation/Iterative Pattern</h3>
</div>
<p>Nodes can maintain state across iterations for negotiation workflows.</p>

<div class="mermaid">
sequenceDiagram
participant W as Workflow
participant S as SellerNode
participant B as BuyerNode
W->>S: Round 1
S-->>B: Offer $10,000
B-->>S: Counter: Too high
W->>S: Round 2
S-->>B: Offer $8,000
B-->>S: Accept ✓
</div>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128176;</span> Negotiation Node</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum NegotiationState { SellerOffer, BuyerEvaluate, Complete }

#[derive(Clone)]
struct NegotiationData {
    current_offer: u32,
    budget: u32,
    round: u32,
}

impl NegotiationData {
    fn new(current_offer: u32, budget: u32) -> Self {
        Self { current_offer, budget, round: 1 }
    }
}

#[derive(Clone)]
struct SellerNode;

#[task::node(state = NegotiationState)]
impl SellerNode {
    type PrepResult = NegotiationData;
    type ExecResult = NegotiationData;

    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        // Load negotiation data or initialize
        match store.get::<NegotiationData>("negotiation") {
            Ok(data) => Ok(data),
            Err(_) => Ok(NegotiationData::new(10000, 1000)), // initial price, budget
        }
    }

    async fn exec(&self, mut data: Self::PrepResult) -> Self::ExecResult {
        // Calculate new offer
        if data.round > 1 {
            let reduction = rand::random::<u32>() % 2000 + 500;
            data.current_offer = data.current_offer.saturating_sub(reduction);
            println!("Seller: New offer ${}", data.current_offer);
        }
        data
    }

    async fn post(&self, res: &Resources, data: Self::ExecResult)
        -> Result<NegotiationState, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("negotiation", data.clone())?;
        Ok(NegotiationState::BuyerEvaluate)
    }
}
```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">3</span>
<h3>Download &amp; Analyze Pattern</h3>
</div>
<p>Perfect for workflows that download content and perform analysis.</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#128269;</span> Download and analyze</span>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Download, Complete }

#[derive(Clone)]
struct BookAnalysis {
    word_count: usize,
    preposition_count: usize,
    density: f64,
}

fn count_prepositions(_words: &[&str]) -> usize { 0 }

#[derive(Clone)]
struct BookAnalyzerNode;

#[task::node(state = State)]
impl BookAnalyzerNode {
    type PrepResult = String;  // Book content
    type ExecResult = BookAnalysis;

    fn config(&self) -> TaskConfig {
        TaskConfig::default()
            .with_fixed_retry(2, Duration::from_secs(1))
    }

    // Prep: Download book (replace this with your own HTTP client)
    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let url: String = store.get("book_url").map_err(|e| CanoError::store(e.to_string()))?;
        println!("📥 Downloading book from: {}", url);

        let content = String::new(); // imagine: reqwest::get(&url).await?.text().await?
        println!("Downloaded {} characters", content.len());
        Ok(content)
    }

    // Exec: Analyze content (retried on failure)
    async fn exec(&self, content: Self::PrepResult) -> Self::ExecResult {
        println!("🔍 Analyzing content...");

        let words: Vec<&str> = content.split_whitespace().collect();
        let prepositions = count_prepositions(&words);

        BookAnalysis {
            word_count: words.len(),
            preposition_count: prepositions,
            density: (prepositions as f64 / words.len() as f64) * 100.0,
        }
    }

    // Post: Store results
    async fn post(&self, res: &Resources, analysis: Self::ExecResult)
        -> Result<State, CanoError> {
        println!("📊 Analysis complete: {} words, {} prepositions",
                 analysis.word_count, analysis.preposition_count);

        let store = res.get::<MemoryStore, _>("store")?;
        store.put("analysis", analysis)?;
        Ok(State::Complete)
    }
}

```
</div>
</section>

<section class="pattern-section">
<div class="pattern-header">
<span class="pattern-number">4</span>
<h3>Multi-Step Processing Pattern</h3>
</div>
<p>Chain multiple nodes together for complex data pipelines.</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9881;</span> Chained pipeline nodes</span>

```rust
use cano::prelude::*;
use rand::Rng;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum State { Start, Filter, Aggregate, Complete }

#[derive(Clone)]
struct Stats { count: usize, sum: u32, avg: f64 }

// Node 1: Data Generator
#[derive(Clone)]
struct GeneratorNode;

#[task::node(state = State)]
impl GeneratorNode {
    type PrepResult = ();
    type ExecResult = Vec<u32>;

    async fn prep(&self, _: &Resources) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _: Self::PrepResult) -> Self::ExecResult {
        let mut rng = rand::rng();
        (0..100).map(|_| rng.random_range(1..=1000)).collect()
    }

    async fn post(&self, res: &Resources, data: Self::ExecResult)
        -> Result<State, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("generated_data", data)?;
        Ok(State::Filter)
    }
}

// Node 2: Data Filter
#[derive(Clone)]
struct FilterNode;

#[task::node(state = State)]
impl FilterNode {
    type PrepResult = Vec<u32>;
    type ExecResult = Vec<u32>;

    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.get("generated_data").map_err(|e| CanoError::store(e.to_string()))
    }

    async fn exec(&self, data: Self::PrepResult) -> Self::ExecResult {
        data.into_iter().filter(|&x| x % 2 == 0).collect()
    }

    async fn post(&self, res: &Resources, filtered: Self::ExecResult)
        -> Result<State, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("filtered_data", filtered)?;
        Ok(State::Aggregate)
    }
}

// Node 3: Aggregator
#[derive(Clone)]
struct AggregatorNode;

#[task::node(state = State)]
impl AggregatorNode {
    type PrepResult = Vec<u32>;
    type ExecResult = Stats;

    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.get("filtered_data").map_err(|e| CanoError::store(e.to_string()))
    }

    async fn exec(&self, data: Self::PrepResult) -> Self::ExecResult {
        Stats {
            count: data.len(),
            sum: data.iter().sum(),
            avg: data.iter().sum::<u32>() as f64 / data.len() as f64,
        }
    }

    async fn post(&self, res: &Resources, stats: Self::ExecResult)
        -> Result<State, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("final_stats", stats)?;
        Ok(State::Complete)
    }
}

// Combine in workflow
fn build_workflow(store: MemoryStore) -> Workflow<State> {
    Workflow::new(Resources::new().insert("store", store))
        .register(State::Start, GeneratorNode)
        .register(State::Filter, FilterNode)
        .register(State::Aggregate, AggregatorNode)
        .add_exit_state(State::Complete)
}

```
</div>
</section>

<!-- Section: Configuration Best Practices -->
<hr class="section-divider">
<h2 id="config"><a href="#config" class="anchor-link" aria-hidden="true">#</a>Node Configuration Best Practices</h2>
<p>Choose the right configuration for your node's reliability requirements.</p>

<table class="styled-table">
<thead>
<tr>
<th>Config</th>
<th>Use Case</th>
<th>Example</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>minimal()</code></td>
<td>Fast, reliable operations</td>
<td>Data transformations</td>
</tr>
<tr>
<td><code>default()</code></td>
<td>Standard operations</td>
<td>File I/O, Database queries</td>
</tr>
<tr>
<td><code>fixed_retry(n)</code></td>
<td>Transient failures</td>
<td>Network operations</td>
</tr>
<tr>
<td><code>exponential_retry(n)</code></td>
<td>Rate-limited APIs</td>
<td>External API calls</td>
</tr>
<tr>
<td><code>with_attempt_timeout(d)</code></td>
<td>Bound each prep&rarr;exec&rarr;post attempt</td>
<td>Slow downstreams, hung connections</td>
</tr>
</tbody>
</table>

<div class="callout callout-warning">
<div class="callout-label">Important</div>
<p>
On any phase failure, the entire <code>prep</code> &rarr; <code>exec</code> &rarr; <code>post</code> pipeline
restarts from the beginning. All three phases must be idempotent — side effects in <code>prep</code> or
<code>exec</code> (e.g. writing to an external system) will be repeated on every retry attempt.
</p>
</div>

<div class="callout callout-info">
<div class="callout-label">Per-attempt timeout</div>
<p>
Nodes share <code>TaskConfig</code> with tasks, so <code>.with_attempt_timeout(d)</code> applies to the
full <code>prep</code> &rarr; <code>exec</code> &rarr; <code>post</code> attempt. If the deadline elapses,
the in-flight phase is dropped, a <code>CanoError::Timeout</code> is fed into the retry loop, and the
configured <code>RetryMode</code> decides whether to restart the pipeline.
</p>
</div>
</div>

