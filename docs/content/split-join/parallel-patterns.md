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

<div class="diagram-frame">
<p class="diagram-label">Queue consumer: pull, split, drain</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 416" role="img">
<title>Queue consumer pattern: PullBatch pops a batch off the external queue, ProcessBatch returns TaskResult::Split with one Complete branch per item, and the outer loop calls orchestrate again until the queue drains.</title>
<defs><marker id="queue-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M360,342 V372 Q360,380 352,380 H40 Q32,380 32,372 V160 Q32,152 40,152 H302 Q310,152 310,144" marker-end="url(#queue-ah)"/>
<text class="t-mut" x="376" y="400">outer loop — orchestrate(PullBatch) again until the queue drains</text>
<rect class="n-cop" x="280" y="16" width="160" height="46" rx="10"/>
<text class="t-strong" x="360" y="40">Queue</text>
<text class="t-mut ta-s" x="452" y="44">SQS · Redis · Kafka</text>
<path class="e" d="M360,62 V90" marker-end="url(#queue-ah)"/>
<text class="t-mut ta-s" x="372" y="82">pop up to batch_size items</text>
<path class="e e-dim" d="M280,102 H232 Q224,102 224,110 V122 Q224,130 232,130 H276" marker-end="url(#queue-ah)"/>
<text class="t-mut ta-e" x="212" y="110">empty batch</text>
<text class="t-mut ta-e" x="212" y="126">sleep 1s, retry</text>
<rect class="n" x="280" y="94" width="160" height="46" rx="10"/>
<text class="t-strong" x="360" y="118">PullBatch</text>
<text class="t-code t-mut ta-s" x="452" y="122">QueuePuller</text>
<path class="e" d="M360,140 V168" marker-end="url(#queue-ah)"/>
<text class="t-code t-mut ta-s" x="372" y="160">store.put("current_batch")</text>
<rect class="n-hot" x="260" y="172" width="200" height="56" rx="10"/>
<text class="t-strong" x="360" y="194">ProcessBatch</text>
<text class="t-code t-mut" x="360" y="214">TaskResult::Split</text>
<text class="t-code t-mut ta-s" x="472" y="204">BatchSplitter</text>
<path class="e" d="M360,228 V248"/>
<path class="e" d="M130,248 H590"/>
<path class="e" d="M130,248 V286" marker-end="url(#queue-ah)"/>
<path class="e" d="M360,248 V286" marker-end="url(#queue-ah)"/>
<path class="e" d="M590,248 V286" marker-end="url(#queue-ah)"/>
<text class="t-mut" x="245" y="266">one Complete branch per item</text>
<rect class="n-ok" x="60" y="290" width="140" height="52" rx="10"/>
<text class="t-strong" x="130" y="311">item 1</text>
<text class="t-code t-mut" x="130" y="331">Complete</text>
<rect class="n-ok" x="290" y="290" width="140" height="52" rx="10"/>
<text class="t-strong" x="360" y="311">item 2</text>
<text class="t-code t-mut" x="360" y="331">Complete</text>
<rect class="n-ok" x="520" y="290" width="140" height="52" rx="10"/>
<text class="t-strong" x="590" y="311">item n</text>
<text class="t-code t-mut" x="590" y="331">Complete</text>
</svg>
</div>
</div>

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
        let result = workflow.orchestrate(QueueState::PullBatch, CancellationToken::disabled()).await?;
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

    workflow.orchestrate(DataState::LoadRecords, CancellationToken::disabled()).await?;
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

<div class="diagram-frame">
<p class="diagram-label">Resource-limited fan-out: 20 tasks, 5 permits</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 296" role="img">
<title>Resource-limited fan-out: 20 split tasks share 5 permits, so at most 5 run concurrently and the fan-out completes in four waves of five.</title>
<defs><marker id="bulk-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-code t-hot ta-s" x="16" y="30">Semaphore::new(5)</text>
<text class="t-mut ta-s" x="16" y="50">= with_bulkhead(5)</text>
<text class="t-strong" x="188" y="30">wave 1</text>
<text class="t-strong" x="343" y="30">wave 2</text>
<text class="t-strong" x="498" y="30">wave 3</text>
<text class="t-strong" x="653" y="30">wave 4</text>
<text class="t-mut" x="188" y="50">tasks 0-4</text>
<text class="t-mut" x="343" y="50">tasks 5-9</text>
<text class="t-mut" x="498" y="50">tasks 10-14</text>
<text class="t-mut" x="653" y="50">tasks 15-19</text>
<text class="t-mut ta-e" x="100" y="84">permit 1</text>
<text class="t-mut ta-e" x="100" y="114">permit 2</text>
<text class="t-mut ta-e" x="100" y="144">permit 3</text>
<text class="t-mut ta-e" x="100" y="174">permit 4</text>
<text class="t-mut ta-e" x="100" y="204">permit 5</text>
<rect class="band-hot" x="113" y="68" width="149" height="22" rx="5"/>
<rect class="band-hot" x="268" y="68" width="149" height="22" rx="5"/>
<rect class="band-hot" x="423" y="68" width="149" height="22" rx="5"/>
<rect class="band-hot" x="578" y="68" width="149" height="22" rx="5"/>
<rect class="band-hot" x="113" y="98" width="149" height="22" rx="5"/>
<rect class="band-hot" x="268" y="98" width="149" height="22" rx="5"/>
<rect class="band-hot" x="423" y="98" width="149" height="22" rx="5"/>
<rect class="band-hot" x="578" y="98" width="149" height="22" rx="5"/>
<rect class="band-hot" x="113" y="128" width="149" height="22" rx="5"/>
<rect class="band-hot" x="268" y="128" width="149" height="22" rx="5"/>
<rect class="band-hot" x="423" y="128" width="149" height="22" rx="5"/>
<rect class="band-hot" x="578" y="128" width="149" height="22" rx="5"/>
<rect class="band-hot" x="113" y="158" width="149" height="22" rx="5"/>
<rect class="band-hot" x="268" y="158" width="149" height="22" rx="5"/>
<rect class="band-hot" x="423" y="158" width="149" height="22" rx="5"/>
<rect class="band-hot" x="578" y="158" width="149" height="22" rx="5"/>
<rect class="band-hot" x="113" y="188" width="149" height="22" rx="5"/>
<rect class="band-hot" x="268" y="188" width="149" height="22" rx="5"/>
<rect class="band-hot" x="423" y="188" width="149" height="22" rx="5"/>
<rect class="band-hot" x="578" y="188" width="149" height="22" rx="5"/>
<line class="axis" x1="110" y1="224" x2="730" y2="224"/>
<path class="e e-dim" d="M730,224 H752" marker-end="url(#bulk-ah)"/>
<line class="tick" x1="110" y1="224" x2="110" y2="230"/>
<line class="tick" x1="265" y1="224" x2="265" y2="230"/>
<line class="tick" x1="420" y1="224" x2="420" y2="230"/>
<line class="tick" x1="575" y1="224" x2="575" y2="230"/>
<line class="tick" x1="730" y1="224" x2="730" y2="230"/>
<text class="t-mut" x="110" y="246">0</text>
<text class="t-mut" x="265" y="246">T</text>
<text class="t-mut" x="420" y="246">2T</text>
<text class="t-mut" x="575" y="246">3T</text>
<text class="t-mut" x="730" y="246">4T</text>
<text class="t-mut" x="420" y="276">T = one task duration — 5 permits gate 20 tasks, so the fan-out completes in 4 waves of 5</text>
</svg>
</div>
</div>

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
