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
<li><a href="#implementing">Implementing a Node</a></li>
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

<!-- Section: Implementing a Node -->
<hr class="section-divider">
<h2 id="implementing"><a href="#implementing" class="anchor-link" aria-hidden="true">#</a>Implementing a Node</h2>
<p>
Here is a complete example of a Node that generates random numbers and filters them.
This demonstrates the three-phase lifecycle: <code>prep</code> (generate), <code>exec</code> (filter), and <code>post</code> (store).
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Complete Node implementation</span>
<pre><code class="language-rust">use async_trait::async_trait;
use cano::prelude::*;
use rand::RngExt;
<!--blank-->
#[derive(Clone)]
struct GeneratorNode;
<!--blank-->
#[async_trait]
impl Node<WorkflowAction> for GeneratorNode {
    // Define the types passed between phases
    type PrepResult = Vec<u32>;
    type ExecResult = Vec<u32>;
<!--blank-->
    // Optional: Configure retry behavior
    fn config(&self) -> TaskConfig {
        TaskConfig::default().with_fixed_retry(3, Duration::from_secs(1))
    }
<!--blank-->
    // Phase 1: Preparation
    // Load data, validate inputs, or generate initial state.
    // This runs once and is not retried automatically.
    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let mut rng = rand::rng();
        let size = rng.random_range(25..=150);
        let numbers: Vec<u32> = (0..size).map(|_| rng.random_range(1..=1000)).collect();
<!--blank-->
        println!("Generated {} random numbers", numbers.len());
        Ok(numbers)
    }
<!--blank-->
    // Phase 2: Execution
    // Core logic. Infallible by design — returns ExecResult directly, not Result.
    // If prep or post fail, the entire pipeline restarts; exec itself cannot fail.
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        // Filter out odd numbers
        let even_numbers: Vec<u32> = prep_res.into_iter().filter(|&n| n % 2 == 0).collect();
        println!("Filtered to {} even numbers", even_numbers.len());
        even_numbers
    }
<!--blank-->
    // Phase 3: Post-processing
    // Store results, cleanup, and decide next state.
    // It receives the result from exec().
    async fn post(
        &self,
        res: &Resources,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowAction, CanoError> {
        // Retrieve the store from resources and write results
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("filtered_numbers", exec_res)?;
<!--blank-->
        println!("✓ Generator node completed");
        Ok(WorkflowAction::Count)
    }
}</code></pre>
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
<pre><code class="language-rust">use cano::prelude::*;
<!--blank-->
// Application-defined types and helpers (not part of Cano)
// struct Record { ... }
// struct ProcessedRecord { ... }
// async fn load_from_database(src: &str) -> Result<Vec<Record>, CanoError> { ... }
// async fn save_to_database(dst: &str, data: &[ProcessedRecord]) -> Result<(), CanoError> { ... }
// fn process_record(r: Record) -> ProcessedRecord { ... }
<!--blank-->
#[derive(Clone)]
struct ETLNode {
    source: String,
    destination: String,
}
<!--blank-->
#[async_trait]
impl Node<State> for ETLNode {
    type PrepResult = Vec<Record>;
    type ExecResult = Vec<ProcessedRecord>;
<!--blank-->
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
            .with_exponential_retry(3) // Retry failures
    }
<!--blank-->
    // Extract: Load data from source
    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        println!("📥 Extracting from: {}", self.source);
<!--blank-->
        let records = load_from_database(&self.source).await?;
        println!("Loaded {} records", records.len());
<!--blank-->
        Ok(records)
    }
<!--blank-->
    // Transform: Process the data
    async fn exec(&self, records: Self::PrepResult) -> Self::ExecResult {
        println!("⚙️  Transforming {} records...", records.len());
<!--blank-->
        records.into_iter()
            .map(|r| process_record(r))
            .collect()
    }
<!--blank-->
    // Load: Save to destination
    async fn post(&self, res: &Resources, processed: Self::ExecResult)
        -> Result<State, CanoError> {
        println!("📤 Loading to: {}", self.destination);
<!--blank-->
        save_to_database(&self.destination, &processed).await?;
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("processed_count", processed.len())?;
<!--blank-->
        Ok(State::Complete)
    }
}</code></pre>
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
<pre><code class="language-rust">#[derive(Clone)]
struct SellerNode;
<!--blank-->
#[async_trait]
impl Node<NegotiationState> for SellerNode {
    type PrepResult = NegotiationState;
    type ExecResult = NegotiationState;
<!--blank-->
    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        // Load negotiation state or initialize
        match store.get::<NegotiationState>("negotiation") {
            Ok(state) => Ok(state),
            Err(_) => Ok(NegotiationState::new(10000, 1000)), // initial price, budget
        }
    }
<!--blank-->
    async fn exec(&self, mut state: Self::PrepResult) -> Self::ExecResult {
        // Calculate new offer
        if state.round > 1 {
            let reduction = rand::random::<u32>() % 2000 + 500;
            state.current_offer = state.current_offer.saturating_sub(reduction);
            println!("Seller: New offer ${}", state.current_offer);
        }
        state
    }
<!--blank-->
    async fn post(&self, res: &Resources, state: Self::ExecResult)
        -> Result<NegotiationState, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("negotiation", state.clone())?;
        Ok(NegotiationState::BuyerEvaluate)
    }
}</code></pre>
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
<pre><code class="language-rust">#[derive(Clone)]
struct BookAnalyzerNode;
<!--blank-->
#[async_trait]
impl Node<State> for BookAnalyzerNode {
    type PrepResult = String;  // Book content
    type ExecResult = BookAnalysis;
<!--blank-->
    fn config(&self) -> TaskConfig {
        TaskConfig::default()
            .with_fixed_retry(2, Duration::from_secs(1))
    }
<!--blank-->
    // Prep: Download book
    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        let url: String = store.get("book_url")?;
        println!("📥 Downloading book from: {}", url);
<!--blank-->
        let client = reqwest::Client::new();
        let content = client.get(&url)
            .send().await?
            .text().await?;
<!--blank-->
        println!("Downloaded {} characters", content.len());
        Ok(content)
    }
<!--blank-->
    // Exec: Analyze content (retried on failure)
    async fn exec(&self, content: Self::PrepResult) -> Self::ExecResult {
        println!("🔍 Analyzing content...");
<!--blank-->
        let words: Vec<&str> = content.split_whitespace().collect();
        let prepositions = count_prepositions(&words);
<!--blank-->
        BookAnalysis {
            word_count: words.len(),
            preposition_count: prepositions,
            density: (prepositions as f64 / words.len() as f64) * 100.0,
        }
    }
<!--blank-->
    // Post: Store results
    async fn post(&self, res: &Resources, analysis: Self::ExecResult)
        -> Result<State, CanoError> {
        println!("📊 Analysis complete: {} words, {} prepositions",
                 analysis.word_count, analysis.preposition_count);
<!--blank-->
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("analysis", analysis)?;
        Ok(State::Complete)
    }
}</code></pre>
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
<pre><code class="language-rust">// Node 1: Data Generator
#[derive(Clone)]
struct GeneratorNode;
<!--blank-->
#[async_trait]
impl Node<State> for GeneratorNode {
    type PrepResult = ();
    type ExecResult = Vec<u32>;
<!--blank-->
    async fn prep(&self, _: &Resources) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }
<!--blank-->
    async fn exec(&self, _: Self::PrepResult) -> Self::ExecResult {
        let mut rng = rand::rng();
        (0..100).map(|_| rng.random_range(1..=1000)).collect()
    }
<!--blank-->
    async fn post(&self, res: &Resources, data: Self::ExecResult)
        -> Result<State, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("generated_data", data)?;
        Ok(State::Filter)
    }
}
<!--blank-->
// Node 2: Data Filter
#[derive(Clone)]
struct FilterNode;
<!--blank-->
#[async_trait]
impl Node<State> for FilterNode {
    type PrepResult = Vec<u32>;
    type ExecResult = Vec<u32>;
<!--blank-->
    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        store.get("generated_data")
    }
<!--blank-->
    async fn exec(&self, data: Self::PrepResult) -> Self::ExecResult {
        data.into_iter().filter(|&x| x % 2 == 0).collect()
    }
<!--blank-->
    async fn post(&self, res: &Resources, filtered: Self::ExecResult)
        -> Result<State, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("filtered_data", filtered)?;
        Ok(State::Aggregate)
    }
}
<!--blank-->
// Node 3: Aggregator
#[derive(Clone)]
struct AggregatorNode;
<!--blank-->
#[async_trait]
impl Node<State> for AggregatorNode {
    type PrepResult = Vec<u32>;
    type ExecResult = Stats;
<!--blank-->
    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        store.get("filtered_data")
    }
<!--blank-->
    async fn exec(&self, data: Self::PrepResult) -> Self::ExecResult {
        Stats {
            count: data.len(),
            sum: data.iter().sum(),
            avg: data.iter().sum::<u32>() as f64 / data.len() as f64,
        }
    }
<!--blank-->
    async fn post(&self, res: &Resources, stats: Self::ExecResult)
        -> Result<State, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("final_stats", stats)?;
        Ok(State::Complete)
    }
}
<!--blank-->
// Combine in workflow
let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
    .register(State::Start, GeneratorNode)
    .register(State::Filter, FilterNode)
    .register(State::Aggregate, AggregatorNode)
    .add_exit_state(State::Complete);</code></pre>
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
</div>

