+++
title = "Cano - Async Data & AI Workflows in Rust"
description = "Cano is a high-performance async workflow orchestration engine for Rust using Finite State Machines for type-safe processing pipelines."
template = "index.html"
+++

<section class="hero">
<h1 class="animate-in">Cano</h1>
<p class="subtitle animate-in">Type-safe async workflow engine with built-in scheduling, retry logic, and state machine semantics.</p>

<p class="prerelease-notice animate-in"><em>Cano still far from 1.0 release. The API is subject to changes and breaking changes.</em></p>

<div class="badges animate-in">
<a href="https://crates.io/crates/cano" title="Crates.io">
<img src="https://img.shields.io/crates/v/cano.svg" alt="Crates.io">
</a>
<a href="https://docs.rs/cano" title="API Documentation">
<img src="https://docs.rs/cano/badge.svg" alt="Documentation">
</a>
<a href="https://crates.io/crates/cano" title="Download Statistics">
<img src="https://img.shields.io/crates/d/cano.svg" alt="Downloads">
</a>
<a href="https://github.com/nassor/cano/blob/main/LICENSE" title="MIT License">
<img src="https://img.shields.io/crates/l/cano.svg" alt="License">
</a>
</div>

<p class="animate-in">
Cano is a high-performance orchestration engine designed for building resilient, self-healing systems in Rust.
Unlike simple task queues, Cano uses <strong>Finite State Machines (FSM)</strong> to define strict, type-safe transitions between processing steps.
</p>

<p class="animate-in">
It excels at managing complex lifecycles where state transitions matter:
</p>
<ul class="animate-in">
<li><strong>Data Pipelines</strong>: ETL jobs with parallel processing (Split/Join) and aggregation.</li>
<li><strong>AI Agents</strong>: Multi-step inference chains with shared context and memory.</li>
<li><strong>Background Systems</strong>: Scheduled maintenance, periodic reporting, and distributed cron jobs.</li>
</ul>
</section>

<h2>Features</h2>
<div class="feature-grid">
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9881;</div>
<h3>Tasks & Nodes</h3>
<p>Single <code>Task</code> trait for simple logic, or <code>Node</code> trait for structured three-phase lifecycle.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#9670;</div>
<h3>State Machines</h3>
<p>Type-safe enum-driven state transitions with compile-time checking.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon accent" aria-hidden="true">&#8635;</div>
<h3>Retry Strategies</h3>
<p>Fixed delays, exponential backoff with jitter, and custom strategies.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon" aria-hidden="true">&#9202;</div>
<h3>Scheduling</h3>
<p>Built-in scheduler with intervals, cron schedules, and manual triggers.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon secondary" aria-hidden="true">&#9881;</div>
<h3>Concurrency</h3>
<p>Execute multiple workflow instances in parallel with timeout strategies.</p>
</div>
<div class="feature-card animate-in">
<div class="feature-icon accent" aria-hidden="true">&#9673;</div>
<h3>Observability</h3>
<p>Comprehensive tracing and observability for workflow execution.</p>
</div>
</div>

<h2>Getting Started</h2>
<p>Add Cano to your <code>Cargo.toml</code>:</p>

<div class="getting-started-code">
<pre><code class="language-toml">[dependencies]
cano = { version = "0.8", features = ["all"] }
tokio = { version = "1", features = ["full"] }</code></pre>
</div>

<h3>Basic Example</h3>
<div class="getting-started-code">
<pre><code class="language-rust">use async_trait::async_trait;
use cano::prelude::*;
<!--blank-->
// Define your workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    Start,
    Process,
    Complete,
}
<!--blank-->
// Simple Task implementation
#[derive(Clone)]
struct SimpleTask;
<!--blank-->
#[async_trait]
impl Task<WorkflowState> for SimpleTask {
    async fn run_bare(&self) -> Result<TaskResult<WorkflowState>, CanoError> {
        println!("Processing task...");
        // Return the next state wrapped in TaskResult
        Ok(TaskResult::Single(WorkflowState::Process))
    }
}
<!--blank-->
struct DoneTask;
<!--blank-->
#[async_trait]
impl Task<WorkflowState> for DoneTask {
    async fn run_bare(&self) -> Result<TaskResult<WorkflowState>, CanoError> {
        println!("Done!");
        Ok(TaskResult::Single(WorkflowState::Complete))
    }
}
<!--blank-->
#[tokio::main]
async fn main() -> Result<(), CanoError> {
    // No resources needed — use Workflow::bare()
    let workflow = Workflow::bare()
        .register(WorkflowState::Start, SimpleTask)
        .register(WorkflowState::Process, DoneTask)
        .add_exit_state(WorkflowState::Complete);
<!--blank-->
    // Run workflow starting at 'Start' state
    workflow.orchestrate(WorkflowState::Start).await?;
<!--blank-->
    Ok(())
}</code></pre>
</div>

