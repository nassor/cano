+++
title = "Parallel patterns"
description = "Real-world split/join fan-out patterns in Cano: queue consumer, dynamic task generation, resource-limited processing, and continuous scheduled batches."
template = "page.html"
weight = 2
+++

<div class="content-wrapper">

<h1>Parallel patterns</h1>
<p class="subtitle">Real-world fan-out recipes built on split/join.</p>

<p>
Recipes for common fan-out problems. Each one applies <a href="../">split/join</a>, or a batch task,
to a single situation.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#parallel-patterns">Common parallel patterns</a></li>
<li class="toc-sub"><a href="#pattern-queue">Queue consumer</a></li>
<li class="toc-sub"><a href="#pattern-dynamic">Dynamic task generation</a></li>
<li class="toc-sub"><a href="#pattern-resource">Resource-limited processing</a></li>
<li class="toc-sub"><a href="#pattern-continuous">Continuous workflow</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="parallel-patterns"><a href="#parallel-patterns" class="anchor-link" aria-hidden="true">#</a>Common parallel patterns</h2>
<p>Pick the pattern by what drives the fan-out:</p>
<ul>
<li>Items arrive from an external queue such as SQS, Redis or Kafka: <a href="#pattern-queue">queue consumer</a>.</li>
<li>The number of tasks depends on runtime data: <a href="#pattern-dynamic">dynamic task generation</a>.</li>
<li>A scarce downstream resource limits concurrency: <a href="#pattern-resource">resource-limited processing</a>.</li>
<li>The fan-out repeats on a schedule: <a href="#pattern-continuous">continuous workflow</a>.</li>
</ul>

<h3 id="pattern-queue"><a href="#pattern-queue" class="anchor-link" aria-hidden="true">#</a>Pattern 1: queue consumer with batch processing</h3>
<p>
Consume a queue in parallel batches with one workflow whose state is a
<a href="../../batch-task/">batch task</a>. <code>load</code> pops up to <code>batch_size</code> items,
<code>process_item</code> runs once per item with bounded concurrency, and <code>finish</code> returns
to the same state until the queue drains.
</p>

<div class="diagram-frame">
<p class="diagram-label">Queue consumer: load, process, finish</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 720 440" role="img" aria-labelledby="patterns-queue-title patterns-queue-desc">
<title id="patterns-queue-title">Queue consumer built on a batch task</title>
<desc id="patterns-queue-desc">The load method pops up to batch_size items from the external queue. The process_item method runs once per item, up to 5 at a time, and each call succeeds or fails on its own. The finish method receives every result and returns the next state: Consume again while items remain in the queue, or Done when the queue is empty.</desc>
<defs><marker id="patterns-queue-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-dim" d="M280,396 H40 Q32,396 32,388 V160 Q32,152 40,152 H302 Q310,152 310,144" marker-end="url(#patterns-queue-ah)"/>
<text class="t-mut ta-s" x="44" y="414">Consume while items remain</text>
<rect class="n-cop" x="280" y="16" width="160" height="46" rx="10"/>
<text class="t-strong" x="360" y="40">Queue</text>
<text class="t-mut ta-s" x="452" y="40">SQS, Redis, Kafka</text>
<path class="e" d="M360,62 V90" marker-end="url(#patterns-queue-ah)"/>
<text class="t-mut ta-s" x="372" y="78">pop up to batch_size items</text>
<rect class="n" x="280" y="94" width="160" height="46" rx="10"/>
<text class="t-strong" x="360" y="118">load</text>
<text class="t-code t-mut ta-s" x="452" y="118">QueueConsumer</text>
<path class="e" d="M360,140 V168" marker-end="url(#patterns-queue-ah)"/>
<text class="t-code t-mut ta-s" x="372" y="158">Vec&lt;String&gt; of items</text>
<rect class="n-hot" x="260" y="172" width="200" height="56" rx="10"/>
<text class="t-strong" x="360" y="194">process_item</text>
<text class="t-code t-mut" x="360" y="214">concurrency() = 5</text>
<path class="e" d="M360,228 V248"/>
<path class="e" d="M130,248 H590"/>
<path class="e" d="M130,248 V286" marker-end="url(#patterns-queue-ah)"/>
<path class="e" d="M360,248 V286" marker-end="url(#patterns-queue-ah)"/>
<path class="e" d="M590,248 V286" marker-end="url(#patterns-queue-ah)"/>
<text class="t-mut" x="245" y="266">one call per item</text>
<rect class="n-ok" x="60" y="290" width="140" height="52" rx="10"/>
<text class="t-strong" x="130" y="311">item 1</text>
<text class="t-code t-mut" x="130" y="331">Ok<animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.32;0.3467;0.9467;1" values="0;0;1;1;0"/></text>
<rect class="n-ok" x="290" y="290" width="140" height="52" rx="10"/>
<text class="t-strong" x="360" y="311">item 2</text>
<text class="t-code t-mut" x="360" y="331">Ok<animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.3067;0.3333;0.9467;1" values="0;0;1;1;0"/></text>
<rect class="n-err" x="520" y="290" width="140" height="52" rx="10"/>
<text class="t-strong" x="590" y="311">item n</text>
<text class="t-code t-mut" x="590" y="331">Err<animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.36;0.3867;0.9467;1" values="0;0;1;1;0"/></text>
<path class="e" d="M130,342 V356 Q130,364 138,364 H360"/>
<path class="e" d="M590,342 V356 Q590,364 582,364 H360"/>
<path class="e" d="M360,342 V376" marker-end="url(#patterns-queue-ah)"/>
<rect class="n" x="280" y="380" width="160" height="40" rx="10"/>
<text class="t-strong" x="360" y="400">finish</text>
<text class="t-mut ta-s" x="452" y="400">Done when the queue is empty</text>
<g class="cd-loop"><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;1" values="1;1"/>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M360,62 V94" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.04;0.1067;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.0267;0.04;0.14;0.1533;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M360,140 V172" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.1333;0.1867;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.12;0.1333;0.22;0.2333;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M360,228 V248 H130 V290" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.2133;0.32;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.2;0.2133;0.3533;0.3667;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M360,228 V290" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.2333;0.3067;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.22;0.2333;0.34;0.3533;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M360,228 V248 H590 V290" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.2533;0.36;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.24;0.2533;0.3933;0.4067;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M130,342 V364 H360 V380" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.4133;0.5067;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.4;0.4133;0.54;0.5533;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M360,342 V380" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.4267;0.4933;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.4133;0.4267;0.5267;0.54;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M590,342 V364 H360 V380" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.44;0.5333;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.4267;0.44;0.5667;0.58;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="7.5s" repeatCount="indefinite" path="M280,396 H40 Q32,396 32,388 V160 Q32,152 40,152 H302 Q310,152 310,144" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.5867;0.8;1"/><animate attributeName="opacity" dur="7.5s" repeatCount="indefinite" keyTimes="0;0.5733;0.5867;0.8333;0.8467;1" values="0;0;1;1;0;0"/></circle>
</g>
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
enum QueueState { Consume, Done }

// Simulated queue (in production, use Redis, SQS, etc.)
type SharedQueue = Arc<Mutex<VecDeque<String>>>;

struct QueueConsumer { queue: SharedQueue, batch_size: usize }

#[task::batch(state = QueueState)]
impl QueueConsumer {
    // At most 5 process_item calls run at once.
    fn concurrency(&self) -> usize { 5 }

    // Pop up to batch_size items off the queue.
    async fn load(&self) -> Result<Vec<String>, CanoError> {
        let mut queue = self.queue.lock().await;
        let n = self.batch_size.min(queue.len());
        Ok(queue.drain(..n).collect())
    }

    // Runs once per item, in parallel.
    async fn process_item(&self, item: &String) -> Result<String, CanoError> {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(item.clone())
    }

    async fn finish(
        &self,
        res: &Resources,
        outputs: Vec<Result<String, CanoError>>,
    ) -> Result<QueueState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        // Failed items are skipped here; return Err to fail the whole batch instead.
        for item in outputs.into_iter().flatten() {
            store.put(&format!("result_{item}"), "completed");
        }
        if self.queue.lock().await.is_empty() {
            Ok(QueueState::Done)
        } else {
            Ok(QueueState::Consume)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();
    let queue: SharedQueue = Arc::new(Mutex::new(VecDeque::from(vec![
        "order1".to_string(), "order2".to_string(), "order3".to_string(),
        "order4".to_string(), "order5".to_string(),
    ])));

    let workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(QueueState::Consume, QueueConsumer { queue: queue.clone(), batch_size: 10 })
        .add_exit_state(QueueState::Done);

    workflow.orchestrate(QueueState::Consume).await?;

    println!("All items processed");
    Ok(())
}
```

<p>
Item failures reach <code>finish</code> as <code>Err</code> entries. A batch task makes no retries unless
<code>item_retry()</code> or <code>config()</code> says so. A queue that never drains runs under the
<a href="../../scheduler/">scheduler</a> instead of looping on the <code>Done</code> exit; Pattern 4 shows
the wiring.
</p>

<h3 id="pattern-dynamic"><a href="#pattern-dynamic" class="anchor-link" aria-hidden="true">#</a>Pattern 2: dynamic task generation</h3>
<p>
Build the task list with an iterator when the number of tasks is not a compile-time constant. The count
is fixed when the workflow is built. A loader writes the dataset into the store, each processor reads
its own item by index, and an aggregator runs once after the split joins.
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
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        store.put("records", (1..=100).collect::<Vec<i32>>());
        Ok(DataState::ProcessBatch)
    }
}

#[derive(Clone)]
struct RecordProcessor { index: usize }

#[task(state = DataState)]
impl RecordProcessor {
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let records: Vec<i32> = store.get("records")?;
        tokio::time::sleep(Duration::from_millis(10)).await;
        store.put(&format!("result_{}", self.index), records[self.index] * 2);
        Ok(DataState::Aggregate)
    }
}

#[derive(Clone)]
struct FinishAggregate;

#[task(state = DataState)]
impl FinishAggregate {
    async fn run(&self, res: &Resources) -> Result<DataState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let total: i32 = (0..100)
            .filter_map(|i| store.get::<i32>(&format!("result_{}", i)).ok())
            .sum();
        println!("Aggregated total: {}", total);
        Ok(DataState::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    // Build the processor tasks before constructing the workflow.
    let processors: Vec<RecordProcessor> = (0..100).map(|i| RecordProcessor { index: i }).collect();
    let join_config = JoinConfig::new(JoinStrategy::All, DataState::Aggregate);

    let workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(DataState::LoadRecords, RecordLoader)
        .register_split(DataState::ProcessBatch, processors, join_config)
        .register(DataState::Aggregate, FinishAggregate)
        .add_exit_state(DataState::Complete);

    workflow.orchestrate(DataState::LoadRecords).await?;
    Ok(())
}
```

<h3 id="pattern-resource"><a href="#pattern-resource" class="anchor-link" aria-hidden="true">#</a>Pattern 3: resource-limited parallel processing</h3>
<p>
Cap parallelism when a downstream resource, such as API keys or connections, is scarce. The
<a href="../#bulkhead">bulkhead</a> on <code>JoinConfig</code> does this for one split. A shared
<code>tokio::sync::Semaphore</code> acquired inside each task is the alternative when one limit must
span several splits or workflows.
</p>

<div class="diagram-frame">
<p class="diagram-label">Resource-limited fan-out: 20 tasks, 5 permits</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 296" role="img" aria-labelledby="patterns-bulkhead-title patterns-bulkhead-desc">
<title id="patterns-bulkhead-title">Resource-limited fan-out: 20 tasks, 5 permits</title>
<desc id="patterns-bulkhead-desc">Twenty split tasks share 5 permits, so at most 5 run at once. Each permit runs four tasks one after another, which gives four waves of five tasks: tasks 0 to 4, 5 to 9, 10 to 14 and 15 to 19. The time axis shows each wave taking one task duration T, so the whole fan-out takes 4T.</desc>
<defs><marker id="patterns-bulkhead-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-code t-hot ta-s" x="16" y="30">Semaphore::new(5)</text>
<text class="t-mut ta-s" x="16" y="50">= with_bulkhead(5)</text>
<text class="t-strong" x="188" y="30">wave 1<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.0811;1" values="0.3;0.3;1;1"/></text>
<text class="t-strong" x="343" y="30">wave 2<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.1892;0.2162;1" values="0.3;0.3;1;1"/></text>
<text class="t-strong" x="498" y="30">wave 3<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.3243;0.3514;1" values="0.3;0.3;1;1"/></text>
<text class="t-strong" x="653" y="30">wave 4<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.4595;0.4865;1" values="0.3;0.3;1;1"/></text>
<text class="t-mut" x="188" y="50">tasks 0 to 4<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.0811;1" values="0.3;0.3;1;1"/></text>
<text class="t-mut" x="343" y="50">tasks 5 to 9<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.1892;0.2162;1" values="0.3;0.3;1;1"/></text>
<text class="t-mut" x="498" y="50">tasks 10 to 14<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.3243;0.3514;1" values="0.3;0.3;1;1"/></text>
<text class="t-mut" x="653" y="50">tasks 15 to 19<animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.4595;0.4865;1" values="0.3;0.3;1;1"/></text>
<text class="t-mut ta-e" x="100" y="84">permit 1</text>
<text class="t-mut ta-e" x="100" y="114">permit 2</text>
<text class="t-mut ta-e" x="100" y="144">permit 3</text>
<text class="t-mut ta-e" x="100" y="174">permit 4</text>
<text class="t-mut ta-e" x="100" y="204">permit 5</text>
<rect class="band-hot" x="113" y="68" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.184;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="268" y="68" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.1892;0.3191;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="423" y="68" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.3243;0.4542;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="578" y="68" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.4595;0.5894;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="113" y="98" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.184;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="268" y="98" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.1892;0.3191;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="423" y="98" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.3243;0.4542;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="578" y="98" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.4595;0.5894;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="113" y="128" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.184;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="268" y="128" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.1892;0.3191;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="423" y="128" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.3243;0.4542;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="578" y="128" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.4595;0.5894;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="113" y="158" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.184;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="268" y="158" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.1892;0.3191;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="423" y="158" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.3243;0.4542;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="578" y="158" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.4595;0.5894;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="113" y="188" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.184;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="268" y="188" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.1892;0.3191;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="423" y="188" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.3243;0.4542;1" values="0;0;149;149"/></rect>
<rect class="band-hot" x="578" y="188" width="149" height="22" rx="5"><animate attributeName="width" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.4595;0.5894;1" values="0;0;149;149"/></rect>
<line class="axis" x1="110" y1="224" x2="730" y2="224"/>
<path class="e e-dim" d="M730,224 H752" marker-end="url(#patterns-bulkhead-ah)"/>
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
<text class="t-mut" x="420" y="276">T = one task duration. 5 permits gate 20 tasks, so the fan-out completes in 4 waves of 5</text>
<g class="cd-loop"><animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;1" values="1;1"/>
<line class="e e-dim e-dash" x1="110" y1="60" x2="110" y2="224" opacity="0"><animate attributeName="x1" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.5946;1" values="110;110;730;730"/><animate attributeName="x2" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0541;0.5946;1" values="110;110;730;730"/><animate attributeName="opacity" dur="7.4s" repeatCount="indefinite" keyTimes="0;0.0473;0.0541;0.5946;0.6216;1" values="0;0;1;1;0;0"/></line>
</g>
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
    async fn run(&self, res: &Resources) -> Result<ApiState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let _permit = self.semaphore.acquire().await
            .map_err(|e| CanoError::task_execution(e.to_string()))?;
        let result = make_api_call(self.api_id).await?;
        store.put(&format!("api_result_{}", self.api_id), result);
        Ok(ApiState::Complete)
    }
}

fn build_workflow(store: MemoryStore) -> Workflow<ApiState> {
    // 20 tasks, at most 5 in flight at once.
    let semaphore = Arc::new(Semaphore::new(5));
    let tasks: Vec<RateLimitedApiTask> = (0..20)
        .map(|i| RateLimitedApiTask { api_id: i, semaphore: semaphore.clone() })
        .collect();
    let join_config = JoinConfig::new(JoinStrategy::All, ApiState::Complete);

    Workflow::new(Resources::new().insert(store))
        .register_split(ApiState::Start, tasks, join_config)
        .add_exit_state(ApiState::Complete)
}
```

<h3 id="pattern-continuous"><a href="#pattern-continuous" class="anchor-link" aria-hidden="true">#</a>Pattern 4: continuous workflow with split/join</h3>
<p>
Combine the <a href="../../scheduler/">scheduler</a> with split/join for continuous parallel processing.
The split tasks are registered statically, so the batch size is fixed per workflow. Size it for your
throughput and let the scheduler run the workflow on an interval.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProcessState { Start, ProcessBatch, Complete }

async fn fetch_pending_work() -> Result<Vec<String>, CanoError> {
    Ok(vec!["job-1".to_string(), "job-2".to_string()])
}

#[derive(Clone)]
struct WorkProcessor { item_index: usize }

#[task(state = ProcessState)]
impl WorkProcessor {
    async fn run(&self, res: &Resources) -> Result<ProcessState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let items: Vec<String> = store.get("work_items")?;
        if let Some(item) = items.get(self.item_index) {
            println!("Processing item: {}", item);
        }
        Ok(ProcessState::Complete)
    }
}

#[derive(Clone)]
struct BatchLoaderTask;

#[task(state = ProcessState)]
impl BatchLoaderTask {
    async fn run(&self, res: &Resources) -> Result<ProcessState, CanoError> {
        let store = res.get::<MemoryStore>()?;
        let items = fetch_pending_work().await?;
        if items.is_empty() {
            return Ok(ProcessState::Complete);
        }
        store.put("work_items", items);
        Ok(ProcessState::ProcessBatch)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let store = MemoryStore::new();

    let batch_size = 10usize;
    let processors: Vec<WorkProcessor> = (0..batch_size).map(|i| WorkProcessor { item_index: i }).collect();
    let join_config = JoinConfig::new(JoinStrategy::All, ProcessState::Complete);

    let batch_workflow = Workflow::new(Resources::new().insert(store.clone()))
        .register(ProcessState::Start, BatchLoaderTask)
        .register_split(ProcessState::ProcessBatch, processors, join_config)
        .add_exit_state(ProcessState::Complete);

    // Keep the handle alive: dropping every `RunningScheduler` handle aborts the runs.
    let running = Scheduler::new()
        .every("batch_processor", batch_workflow, ProcessState::Start, Duration::from_secs(10))
        .start()
        .await?;

    // Run until Ctrl-C or SIGTERM, then drain in-flight runs, cancel stragglers and tear down.
    shutdown_signal().await?;
    running.stop().await?;
    Ok(())
}
```
</div>
