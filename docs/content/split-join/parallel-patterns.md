+++
title = "Common Parallel Patterns"
description = "Real-world split/join fan-out patterns in Cano: queue consumer, dynamic task generation, resource-limited processing, and continuous scheduled batches."
template = "page.html"
weight = 2
+++

<div class="content-wrapper">

<h1>Parallel Patterns</h1>
<p class="subtitle">Real-world fan-out recipes built on split/join.</p>

<p>
These patterns apply <a href="../">split/join</a> to common scenarios — consuming external queues,
generating tasks from runtime data, capping concurrent operations, and running scheduled parallel
batches.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#parallel-patterns">Common Parallel Patterns</a></li>
<li class="toc-sub"><a href="#pattern-queue">Queue Consumer</a></li>
<li class="toc-sub"><a href="#pattern-dynamic">Dynamic Task Generation</a></li>
<li class="toc-sub"><a href="#pattern-resource">Resource-Limited Processing</a></li>
<li class="toc-sub"><a href="#pattern-continuous">Continuous Workflow</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="parallel-patterns"><a href="#parallel-patterns" class="anchor-link" aria-hidden="true">#</a>Common Parallel Patterns</h2>
<p>
Split/Join handles complex parallel processing within a single workflow. Below are real-world
patterns that fan out work across many tasks and join results back into the FSM. Use the
<strong>Queue Consumer</strong> pattern for external queues (SQS, Redis, Kafka), <strong>Dynamic Task
Generation</strong> when the task count depends on runtime data, <strong>Resource-Limited Processing</strong>
to cap concurrent operations, and the <strong>Continuous Workflow</strong> pattern for scheduled parallel
batches. They all give you the parallelism of running many concurrent workflow instances, with a simpler
mental model, better resource control, and type-safe state management.
</p>

<h3 id="pattern-queue"><a href="#pattern-queue" class="anchor-link" aria-hidden="true">#</a>Pattern 1: Queue Consumer with Batch Processing</h3>
<p>
Process items from a queue in parallel batches. Instead of running multiple workflow instances concurrently,
use a single workflow that pulls a batch, splits it into per-item processors via a task that returns
<code>TaskResult::Split</code>, and loops until the queue drains.
</p>

```rust
use cano::prelude::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum QueueState { PullBatch, ProcessBatch, Complete }

// Simulated queue (in production, use Redis, SQS, etc.)
type SharedQueue = Arc<Mutex<VecDeque<String>>>;

#[derive(Clone)]
struct QueuePuller { queue: SharedQueue, batch_size: usize }

#[task(state = QueueState)]
impl QueuePuller {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let mut queue = self.queue.lock().await;

        let mut batch = Vec::new();
        for _ in 0..self.batch_size {
            match queue.pop_front() {
                Some(item) => batch.push(item),
                None => break,
            }
        }

        if batch.is_empty() {
            tokio::time::sleep(Duration::from_secs(1)).await;
            return Ok(TaskResult::Single(QueueState::PullBatch));
        }

        store.put("current_batch", batch)?;
        Ok(TaskResult::Single(QueueState::ProcessBatch))
    }
}

#[derive(Clone)]
struct ItemProcessor { item_id: String }

#[task(state = QueueState)]
impl ItemProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        store.put(&format!("result_{}", self.item_id), "completed")?;
        Ok(TaskResult::Single(QueueState::Complete))
    }
}

#[derive(Clone)]
struct BatchSplitter;

#[task(state = QueueState)]
impl BatchSplitter {
    async fn run(&self, res: &Resources) -> Result<TaskResult<QueueState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let batch: Vec<String> = store.get("current_batch")?;
        if batch.is_empty() {
            return Ok(TaskResult::Single(QueueState::PullBatch));
        }
        // One follow-on Complete state per item — processed in parallel.
        Ok(TaskResult::Split(batch.iter().map(|_| QueueState::Complete).collect()))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
    let queue = Arc::new(Mutex::new(VecDeque::from(vec![
        "order1".into(), "order2".into(), "order3".into(), "order4".into(), "order5".into(),
    ])));

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(QueueState::PullBatch, QueuePuller { queue: queue.clone(), batch_size: 10 })
        .register(QueueState::ProcessBatch, BatchSplitter)
        .add_exit_state(QueueState::Complete);

    loop {
        let result = workflow.orchestrate(QueueState::PullBatch).await?;
        if result == QueueState::Complete && queue.lock().await.is_empty() {
            break;
        }
    }

    println!("All items processed");
    Ok(())
}
```

<h3 id="pattern-dynamic"><a href="#pattern-dynamic" class="anchor-link" aria-hidden="true">#</a>Pattern 2: Dynamic Task Generation</h3>
<p>
Build the list of parallel tasks from runtime data before constructing the workflow. A loader task
writes the dataset into the store; each processor reads its slice by index; an aggregator runs once
after the split joins.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DataState { LoadRecords, ProcessBatch, Aggregate, Complete }

#[derive(Clone)]
struct RecordLoader;

#[task(state = DataState)]
impl RecordLoader {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        store.put("records", (1..=100).collect::<Vec<i32>>())?;
        Ok(TaskResult::Single(DataState::ProcessBatch))
    }
}

#[derive(Clone)]
struct RecordProcessor { index: usize }

#[task(state = DataState)]
impl RecordProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let records: Vec<i32> = store.get("records")?;
        tokio::time::sleep(Duration::from_millis(10)).await;
        store.put(&format!("result_{}", self.index), records[self.index] * 2)?;
        Ok(TaskResult::Single(DataState::Aggregate))
    }
}

#[derive(Clone)]
struct FinishAggregate;

#[task(state = DataState)]
impl FinishAggregate {
    async fn run(&self, res: &Resources) -> Result<TaskResult<DataState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let total: i32 = (0..100)
            .filter_map(|i| store.get::<i32>(&format!("result_{}", i)).ok())
            .sum();
        println!("Aggregated total: {}", total);
        Ok(TaskResult::Single(DataState::Complete))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Build the processor tasks before constructing the workflow.
    let processors: Vec<RecordProcessor> = (0..100).map(|i| RecordProcessor { index: i }).collect();
    let join_config = JoinConfig::new(JoinStrategy::All, DataState::Aggregate);

    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(DataState::LoadRecords, RecordLoader)
        .register_split(DataState::ProcessBatch, processors, join_config)
        .register(DataState::Aggregate, FinishAggregate)
        .add_exit_state(DataState::Complete);

    workflow.orchestrate(DataState::LoadRecords).await?;
    Ok(())
}
```

<h3 id="pattern-resource"><a href="#pattern-resource" class="anchor-link" aria-hidden="true">#</a>Pattern 3: Resource-Limited Parallel Processing</h3>
<p>
Cap parallelism when a downstream resource (API keys, connections) is scarce. The
<a href="../#bulkhead">bulkhead</a> on <code>JoinConfig</code> is the built-in way to do this; the example
below shows the manual alternative — a shared <code>tokio::sync::Semaphore</code> acquired inside each
task — for cases where you need the limit to span more than one split.
</p>

```rust
use cano::prelude::*;
use tokio::sync::Semaphore;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ApiState { Start, Complete }

async fn make_api_call(_id: usize) -> Result<String, CanoError> {
    Ok("ok".to_string())
}

#[derive(Clone)]
struct RateLimitedApiTask { api_id: usize, semaphore: Arc<Semaphore> }

#[task(state = ApiState)]
impl RateLimitedApiTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ApiState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let _permit = self.semaphore.acquire().await
            .map_err(|e| CanoError::task_execution(e.to_string()))?;
        let result = make_api_call(self.api_id).await?;
        store.put(&format!("api_result_{}", self.api_id), result)?;
        Ok(TaskResult::Single(ApiState::Complete))
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<ApiState> {
    // 20 tasks, at most 5 in flight at once.
    let semaphore = Arc::new(Semaphore::new(5));
    let tasks: Vec<RateLimitedApiTask> = (0..20)
        .map(|i| RateLimitedApiTask { api_id: i, semaphore: semaphore.clone() })
        .collect();
    let join_config = JoinConfig::new(JoinStrategy::All, ApiState::Complete);

    Workflow::new(Resources::new().insert("store", store))
        .register_split(ApiState::Start, tasks, join_config)
        .add_exit_state(ApiState::Complete)
}
```

<div class="callout callout-tip">
<span class="callout-label">Tip</span>
<p>
For most cases, <code>JoinConfig::with_bulkhead(n)</code> is simpler than a hand-rolled semaphore —
it gates the split's task bodies on a semaphore for you and still applies the join strategy normally.
Reach for the manual approach only when the limit must be shared across multiple splits or workflows.
</p>
</div>

<h3 id="pattern-continuous"><a href="#pattern-continuous" class="anchor-link" aria-hidden="true">#</a>Pattern 4: Continuous Workflow with Split/Join</h3>
<p>
Combine the <a href="../../scheduler/">scheduler</a> with split/join for continuous parallel processing.
Because the split tasks are registered statically, the batch size is fixed per workflow instance —
size it for your throughput and let the scheduler re-run it on an interval.
</p>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProcessState { Start, ProcessBatch, Complete }

async fn fetch_pending_work() -> Result<Vec<String>, CanoError> {
    Ok(vec!["job-1".to_string(), "job-2".to_string()])
}

#[derive(Clone)]
struct WorkProcessor { item_index: usize }

#[task(state = ProcessState)]
impl WorkProcessor {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ProcessState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let items: Vec<String> = store.get("work_items")?;
        if let Some(item) = items.get(self.item_index) {
            println!("Processing item: {}", item);
        }
        Ok(TaskResult::Single(ProcessState::Complete))
    }
}

#[derive(Clone)]
struct BatchLoaderTask;

#[task(state = ProcessState)]
impl BatchLoaderTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ProcessState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let items = fetch_pending_work().await?;
        if items.is_empty() {
            return Ok(TaskResult::Single(ProcessState::Complete));
        }
        store.put("work_items", items)?;
        Ok(TaskResult::Single(ProcessState::ProcessBatch))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler = Scheduler::new();
    let store = MemoryStore::new();

    let batch_size = 10usize;
    let processors: Vec<WorkProcessor> = (0..batch_size).map(|i| WorkProcessor { item_index: i }).collect();
    let join_config = JoinConfig::new(JoinStrategy::All, ProcessState::Complete);

    let batch_workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(ProcessState::Start, BatchLoaderTask)
        .register_split(ProcessState::ProcessBatch, processors, join_config)
        .add_exit_state(ProcessState::Complete);

    scheduler.every_seconds("batch_processor", batch_workflow, ProcessState::Start, 10)?;
    // Keep the handle alive — dropping the `RunningScheduler` aborts the spawned loops.
    let running = scheduler.start().await?;

    // ...run until shut down (e.g. a Ctrl-C handler), then stop gracefully.
    running.wait().await?;
    Ok(())
}
```
</div>
