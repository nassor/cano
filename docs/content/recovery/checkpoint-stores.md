+++
title = "Checkpoint stores"
description = "Pick one of Cano's built-in checkpoint stores (redb, PostgreSQL, NATS JetStream) or write your own CheckpointStore and test it against the contract."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Checkpoint stores</h1>
<p class="subtitle">Where the recovery log lives: the three built-in stores, and how to write your own.</p>
<p class="feature-tag">The <code>CheckpointStore</code> trait is always available. <code>RedbStore</code> is behind <code>store-redb</code>, which is on by default. <code>PostgresStore</code> is behind <code>store-postgres</code> and <code>NatsStore</code> behind <code>store-nats</code>.</p>

<p>
See <a href="../">Crash recovery</a> for what the engine records and how a run resumes. This page covers
the store behind it: a type that implements <code>CheckpointStore</code> and keeps the rows somewhere that
outlives the process.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#choosing">Choosing a store</a></li>
<li><a href="#trait">The <code>CheckpointStore</code> trait</a></li>
<li><a href="#redb">Built in: <code>RedbStore</code></a></li>
<li><a href="#postgres">Built in: <code>PostgresStore</code></a></li>
<li><a href="#nats">Built in: <code>NatsStore</code></a></li>
<li><a href="#custom">Writing your own store</a></li>
<li class="toc-sub"><a href="#custom-layout">Lay out the rows</a></li>
<li class="toc-sub"><a href="#custom-impl">Implement the trait</a></li>
<li class="toc-sub"><a href="#custom-network">Network backends: timeouts and lost acknowledgements</a></li>
<li class="toc-sub"><a href="#custom-test">Test it against the contract</a></li>
<li class="toc-sub"><a href="#custom-attach">Attach it to a workflow</a></li>
<li><a href="#network-failures">Connection failures, retries and outages</a></li>
<li class="toc-sub"><a href="#reconnection">Automatic reconnection</a></li>
<li class="toc-sub"><a href="#retries-and-lost-acks">Per-call timeouts, retries and lost acknowledgements</a></li>
<li class="toc-sub"><a href="#prolonged-outages">When the connection does not come back</a></li>
<li class="toc-sub"><a href="#handling-outages">Handling store outages in code</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="choosing"><a href="#choosing" class="anchor-link" aria-hidden="true">#</a>Choosing a store</h2>
<table>
<thead><tr><th>Store</th><th>Feature</th><th>Where the rows live</th><th>Who can resume a run</th></tr></thead>
<tbody>
<tr><td><a href="#redb"><code>RedbStore</code></a></td><td><code>store-redb</code> (default)</td><td>One embedded redb file</td><td>The process that holds the file</td></tr>
<tr><td><a href="#postgres"><code>PostgresStore</code></a></td><td><code>store-postgres</code></td><td>One PostgreSQL table</td><td>Any process that reaches the database</td></tr>
<tr><td><a href="#nats"><code>NatsStore</code></a></td><td><code>store-nats</code></td><td>One NATS JetStream Key/Value bucket</td><td>Any process that reaches the JetStream</td></tr>
<tr><td><a href="#custom">Your own</a></td><td>none</td><td>Any storage you run</td><td>Whatever your storage allows</td></tr>
</tbody>
</table>
<p>
With the <code>scheduler</code> feature each built-in store also implements <code>SchedulerStore</code>, the
shared record that lets <a href="../../scheduler/several-instances/">several scheduler instances</a> run
one set of flows. A custom checkpoint store does not need to.
</p>
<hr class="section-divider">

<h2 id="trait"><a href="#trait" class="anchor-link" aria-hidden="true">#</a>The <code>CheckpointStore</code> trait</h2>
<p>
The trait has three async methods. The engine only ever appends rows, reads a whole run back, and
deletes a whole run:
</p>

```rust
pub trait CheckpointStore: Send + Sync + 'static {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError>;
    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError>;
    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError>;
}
```

<p>
The trait is object safe: a workflow holds its store as <code>Arc&lt;dyn CheckpointStore&gt;</code>, and
one store serves any number of concurrent workflows. The engine calls each method at a fixed point:
</p>
<table>
<thead><tr><th>Method</th><th>Called when</th><th>An <code>Err</code> means</th></tr></thead>
<tbody>
<tr><td><code>append</code></td><td>Before each state's task runs, and after a compensatable task, a step or a suspend (see <a href="../#what-is-recorded">what gets recorded</a>)</td><td>The task does not run; the run fails with <code>CanoError::CheckpointStore</code></td></tr>
<tr><td><code>load_run</code></td><td><code>resume_from</code>, <code>resume_with</code>, and the scheduler before each scheduled run</td><td>The resume fails before any task runs</td></tr>
<tr><td><code>clear</code></td><td>The run reached an exit state, or a saga rollback finished cleanly</td><td>Not fatal: <code>on_checkpoint_clear_failed</code> fires and the run keeps its own result</td></tr>
</tbody>
</table>
<p>The contract every implementation must keep:</p>
<dl class="key-list">
<dt><code>append(workflow_id, row)</code></dt>
<dd>Persists the row durably before returning <code>Ok</code>, with every field: <code>sequence</code>, <code>state</code>, <code>task_id</code>, <code>output_blob</code>, <code>kind</code> and <code>workflow_version</code>. It rejects a second row at the same <code>(workflow_id, sequence)</code> with an <code>Err</code> and keeps the first one. The engine numbers rows densely from 0, so a collision means two runs share a workflow id, and overwriting would corrupt the first run's log.</dd>

<dt><code>load_run(workflow_id)</code></dt>
<dd>Returns every row appended for the id, sorted ascending by <code>sequence</code> whatever order they were written in, or an empty <code>Vec</code> for an unknown id.</dd>

<dt><code>clear(workflow_id)</code></dt>
<dd>Removes every row of that id and nothing else, so <code>a</code> must not clear <code>a.b</code> or <code>ab</code>. Clearing an unknown id returns <code>Ok</code>. After a clear the id accepts sequence 0 again.</dd>
</dl>
<p>
All three may run concurrently, for one id or for different ids. Workflow ids are arbitrary UTF-8:
expect spaces, <code>/</code>, <code>.</code>, <code>*</code>, non-ASCII text and ids of 200 characters or
more. <code>output_blob</code> can be large: a stepped task's cursor or a compensatable task's output is
stored whole.
</p>
<hr class="section-divider">

<h2 id="redb"><a href="#redb" class="anchor-link" aria-hidden="true">#</a>Built in: <code>RedbStore</code></h2>
<p class="feature-tag">Behind the <code>store-redb</code> feature, which is on by default. <code>default-features = false</code> removes redb from the build.</p>

<p>
<code>RedbStore</code> is an embedded, ACID, pure-Rust key-value store
(<a href="https://docs.rs/redb">redb</a>) with no background daemon, which suits a recovery log that
lives in the same process as the engine. <code>RedbStore::new(path)</code> creates the
database file if absent and the checkpoint table. Clones share one handle behind an
<code>Arc</code>.
</p>

```rust
use cano::RedbStore;
use std::sync::Arc;

let checkpoint_store: Arc<RedbStore> = Arc::new(RedbStore::new("workflow.redb")?);
// pass it to the workflow: .with_checkpoint_store(checkpoint_store)
```

<p>
<code>new(path)</code> needs the file's directory to exist. <code>RedbStore::open_default()</code>
opens <code>cano.redb</code> in a persistent directory, the first of the rules below that
applies, and creates missing directories. <code>RedbStore::default_path()</code> returns
that file without opening it. In the paths, <code>app</code> is the executable's file name without
its extension.
</p>
<dl class="key-list">
<dt><code>CANO_DATA_DIR</code> <small>any platform, when set</small></dt>
<dd><code>$CANO_DATA_DIR/cano.redb</code></dd>

<dt><code>STATE_DIRECTORY</code> <small>a systemd unit with <code>StateDirectory=</code></small></dt>
<dd><code>$STATE_DIRECTORY/cano/cano.redb</code>, from the first entry when the variable lists several</dd>

<dt>Linux <small>and other Unix systems except macOS</small></dt>
<dd><code>$XDG_STATE_HOME/cano/app/cano.redb</code>, or <code>~/.local/state/cano/app/cano.redb</code> when <code>XDG_STATE_HOME</code> is unset or not an absolute path</dd>

<dt>macOS</dt>
<dd><code>~/Library/Application Support/cano/app/cano.redb</code></dd>

<dt>Windows</dt>
<dd><code>%LOCALAPPDATA%\cano\app\cano.redb</code>, usually under <code>C:\Users\name\AppData\Local</code></dd>
</dl>
<p>
redb locks the file, so one process holds it at a time: a second open fails while the first store
is alive. Renaming the executable changes <code>app</code> and starts a new file, so a service sets
<code>CANO_DATA_DIR</code> or runs under systemd with <code>StateDirectory=</code>.
</p>

<p>
One table maps <code>(workflow_id, sequence)</code> to a payload holding the other
<code>CheckpointRow</code> fields, including <code>kind</code>: a two-byte <code>CV</code> tag followed by
the <a href="https://docs.rs/postcard"><code>postcard</code></a> encoding. Rows written without the tag
still decode.
redb orders composite keys element by element, so a workflow's rows are range-scanned in ascending
<code>sequence</code>. The sequence lives in the key, not the value.
</p>
<p>
With the <code>scheduler</code> feature <code>RedbStore</code> also implements
<code>SchedulerStore</code> in the tables <code>cano_scheduler</code> and <code>cano_scheduler_version</code> of the
same file. Because one process holds the file, it keeps scheduler state across restarts and coordinates schedulers
inside that process, not across processes. See <a href="../../scheduler/several-instances/">Several scheduler instances</a>.
</p>
<hr class="section-divider">

<h2 id="postgres"><a href="#postgres" class="anchor-link" aria-hidden="true">#</a>Built in: <code>PostgresStore</code></h2>
<p class="feature-tag">Behind the <code>store-postgres</code> feature gate (<code>features = ["store-postgres"]</code>).</p>

<p>
<code>PostgresStore</code> keeps every run in one table, <code>cano_checkpoints</code> by
default (<code>DEFAULT_CHECKPOINT_TABLE</code>), and draws connections from a <a href="https://docs.rs/deadpool-postgres">deadpool-postgres</a>
pool over <a href="https://docs.rs/tokio-postgres">tokio-postgres</a>. The module
<code>cano::store::postgres</code> re-exports both crates. Clones share the pool.
</p>
<p>
With the <code>scheduler</code> feature <code>PostgresStore</code> also implements <code>SchedulerStore</code> in a
second table, <code>cano_scheduler</code> by default (<code>DEFAULT_SCHEDULER_TABLE</code>), renamed with
<code>with_scheduler_table</code>. See <a href="../../scheduler/several-instances/">Several scheduler instances</a>.
</p>

```rust
use cano::PostgresStore;
use cano::RetryMode;
use std::sync::Arc;
use std::time::Duration;

let checkpoint_store = PostgresStore::builder()
    .with_schema("workflows")
    .with_max_pool_size(8)
    .with_operation_timeout(Duration::from_secs(5))
    .with_retry(RetryMode::fixed(3, Duration::from_millis(200)))
    .connect("postgres://cano:secret@db.internal/app")
    .await?;
// pass it to the workflow: .with_checkpoint_store(Arc::new(checkpoint_store))
```

<p>The builder settings, with their defaults in small type:</p>
<dl class="key-list">
<dt><code>with_checkpoint_table(name)</code> <small><code>cano_checkpoints</code></small></dt>
<dd>The table name: 1 to 63 characters of <code>[a-z0-9_]</code>, not starting with a digit.</dd>

<dt><code>with_schema(name)</code> <small>none</small></dt>
<dd>The schema holding the table. Without one, the server's <code>search_path</code> resolves the name. The store never creates a schema.</dd>

<dt><code>with_create_table(bool)</code> <small><code>true</code></small></dt>
<dd>Create the table on connect when it is missing. With <code>false</code>, run the DDL from <code>schema_sql()</code> yourself.</dd>

<dt><code>with_max_pool_size(n)</code> <small>twice the core count</small></dt>
<dd>The most connections the pool opens.</dd>

<dt><code>with_pool_timeouts(Timeouts)</code> <small><code>wait</code> = <code>operation_timeout</code> (10 s)</small></dt>
<dd>deadpool's waits for a free slot, a new connection and a recycle check. By default <code>wait</code> is bounded by <code>operation_timeout</code>, while <code>create</code> and <code>recycle</code> are unset.</dd>

<dt><code>with_recycling_method(RecyclingMethod)</code> <small><code>Fast</code></small></dt>
<dd>How the pool checks a connection before reuse.</dd>

<dt><code>with_operation_timeout(Duration)</code> <small>10 s</small></dt>
<dd>The bound on each attempt of both <code>CheckpointStore</code> (<code>append</code>, <code>load_run</code>, <code>clear</code>) and <code>SchedulerStore</code> (<code>load</code>, <code>compare_and_swap</code>, <code>remove</code>), connection checkout and migration included.</dd>

<dt><code>with_retry(RetryMode)</code> <small><code>RetryMode::None</code></small></dt>
<dd>Jittered retries for transient failures across both <code>CheckpointStore</code> and <code>SchedulerStore</code>. See <a href="#network-failures">connection failures, retries and outages</a>.</dd>
</dl>

<p>
<code>connect(dsn)</code> takes a libpq connection string and connects without TLS.
<code>connect_with_tls(config, tls)</code> takes a <code>tokio_postgres::Config</code> and any TLS
connector, and <code>build_with_pool(pool)</code> uses a pool you built, ignoring the pool settings
above. The store sets <code>application_name</code> to <code>cano</code> unless your config sets one.
</p>

<p>
The table, created under an advisory lock so that processes starting together do not race:
</p>

```sql
CREATE TABLE IF NOT EXISTS "cano_checkpoints" (
    sequence         bigint      NOT NULL,
    workflow_version bigint      NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    kind             smallint    NOT NULL,
    workflow_id      text COLLATE "C" NOT NULL,
    state            text        NOT NULL,
    task_id          text        NOT NULL,
    output_blob      bytea,
    write_id         bytea,
    PRIMARY KEY (workflow_id, sequence)
);
```

<p>
The primary key is the only index. It rejects a duplicate sequence, and <code>load_run</code> and
<code>clear</code> each read one range of it, so no statement the store runs scans the whole table.
<code>output_blob</code> holds the row's bytes exactly as written, and <code>write_id</code> stores a
per-append 16-byte token so retried appends after a lost ACK match only their own write.
<code>kind</code> stores <code>StateEntry</code> as 0, <code>CompensationCompletion</code> as 1,
<code>StepCursor</code> as 2, <code>Suspended</code> as 3 and <code>ResumeInput</code> as 4.
<code>created_at</code> is for operators and is not indexed.
</p>
<hr class="section-divider">

<h2 id="nats"><a href="#nats" class="anchor-link" aria-hidden="true">#</a>Built in: <code>NatsStore</code></h2>
<p class="feature-tag">Behind the <code>store-nats</code> feature gate (<code>features = ["store-nats"]</code>).</p>

<p>
<code>NatsStore</code> keeps every run in one JetStream Key/Value bucket,
<code>cano_checkpoints</code> by default (<code>DEFAULT_CHECKPOINT_BUCKET</code>, stream <code>KV_cano_checkpoints</code>). The module
<code>cano::store::nats</code> re-exports <a href="https://docs.rs/async-nats">async-nats</a>.
Clones share the client.
</p>
<p>
With the <code>scheduler</code> feature <code>NatsStore</code> also implements <code>SchedulerStore</code> in a
second bucket, <code>cano_scheduler</code> by default (<code>DEFAULT_SCHEDULER_BUCKET</code>), set with
<code>with_scheduler_bucket</code> and <code>with_scheduler_bucket_config</code>. <code>scheduler_kv()</code> returns
its handle. See <a href="../../scheduler/several-instances/">Several scheduler instances</a>.
</p>

```rust
use cano::NatsStore;
use cano::store::nats::async_nats::ConnectOptions;
use std::sync::Arc;

let checkpoint_store = NatsStore::builder()
    .with_checkpoint_bucket("orders_checkpoints")
    .connect_with_options(
        "nats://nats.internal:4222",
        ConnectOptions::with_user_and_password("cano".into(), "secret".into()),
    )
    .await?;
// pass it to the workflow: .with_checkpoint_store(Arc::new(checkpoint_store))
```

<p>The builder settings, with their defaults in small type:</p>
<dl class="key-list">
<dt><code>with_checkpoint_bucket(name)</code> <small><code>cano_checkpoints</code></small></dt>
<dd>The bucket name: one or more of <code>[A-Za-z0-9_-]</code>.</dd>

<dt><code>with_checkpoint_bucket_config(kv::Config)</code> <small><code>history: 1</code></small></dt>
<dd>The configuration used when the store creates the bucket: storage, replicas, placement, compression, mirror. Setting a non-zero <code>max_age</code> is rejected with <code>CanoError::Configuration</code> so checkpoint and scheduler state cannot silently expire.</dd>

<dt><code>with_create_bucket(bool)</code> <small><code>true</code></small></dt>
<dd>Create the bucket on connect when it is missing.</dd>

<dt><code>with_domain(name)</code>, <code>with_api_prefix(prefix)</code> <small>none</small></dt>
<dd>Reach JetStream in another domain or through an imported API prefix. Set at most one.</dd>

<dt><code>with_operation_timeout(Duration)</code> <small>10 s</small></dt>
<dd>The bound on each attempt of both <code>CheckpointStore</code> (<code>append</code>, <code>load_run</code>, <code>clear</code>) and <code>SchedulerStore</code> (<code>load</code>, <code>compare_and_swap</code>, <code>remove</code>), and on opening a bucket.</dd>

<dt><code>with_retry(RetryMode)</code> <small><code>RetryMode::None</code></small></dt>
<dd>Jittered retries for transient failures across both <code>CheckpointStore</code> and <code>SchedulerStore</code>, plus bucket opening. See <a href="#network-failures">connection failures, retries and outages</a>.</dd>
</dl>

<p>
<code>connect(addrs)</code> connects with the connection name <code>cano</code>.
<code>connect_with_options(addrs, options)</code> uses your <code>ConnectOptions</code> as given
(credentials, TLS, reconnect policy), and <code>build_with_client</code> and
<code>build_with_context</code> reuse a client or JetStream context you already have.
</p>

<p>
Each row is one key, <code>{workflow_id}.{sequence}</code>. Its value is a two-byte <code>CW</code> tag,
a random 16-byte write id drawn for that append, and the postcard encoding; rows with the
<code>RedbStore</code> layout (<code>CV</code>, no write id) still decode. Workflow id bytes outside
<code>[A-Za-z0-9_-]</code> are written as <code>=</code> plus two hex digits, so <code>order-1</code>
stays <code>order-1</code> and <code>order.2</code> becomes <code>order=2E2</code>. <code>append</code>
writes with an expected revision of 0, so a second write to a key fails. <code>load_run</code> reads
through a short-lived consumer that it deletes afterwards, <code>clear</code> purges the run's keys from
the stream in one request, and the scheduler's <code>remove</code> purges a key only at the revision it
just read.
</p>
<hr class="section-divider">

<h2 id="custom"><a href="#custom" class="anchor-link" aria-hidden="true">#</a>Writing your own store</h2>
<p>
Write your own store to keep the log in storage the built-in ones do not cover: SQLite, DynamoDB,
Redis, an object store, or an internal service. It needs no feature flag.
</p>

<h3 id="custom-layout"><a href="#custom-layout" class="anchor-link" aria-hidden="true">#</a>Lay out the rows</h3>
<p>
Key each row by <code>(workflow_id, sequence)</code> and make that key unique in the storage itself, so the
duplicate check is one atomic write instead of a read followed by a write that two runs can race:
</p>
<dl class="key-list">
<dt>SQL table</dt>
<dd>A primary key on <code>(workflow_id, sequence)</code> and a plain <code>INSERT</code>, mapping the unique violation to an <code>Err</code>. <code>load_run</code> is <code>SELECT ... WHERE workflow_id = $1 ORDER BY sequence</code>, and <code>clear</code> a <code>DELETE</code> on the same predicate. Give <code>kind</code> its own column, stored as the <code>RowKind</code> variant index; see the <a href="#postgres">PostgresStore schema</a>.</dd>

<dt>Key-value store</dt>
<dd>One key per row, such as <code>{workflow_id}/{sequence}</code>, written with create-if-absent (a conditional put, <code>SETNX</code>, an expected revision of 0). Escape the workflow id so the separator cannot appear in it, and zero-pad or encode the sequence so a prefix scan returns rows in order, or sort them in <code>load_run</code>.</dd>

<dt>Document or blob store</dt>
<dd>One document per run whose rows are appended under optimistic concurrency (an ETag or version check). Rewriting the whole run on each append works for short runs; long-running stepped or stream tasks append many rows, so prefer one object per row there.</dd>
</dl>
<p>
<code>CheckpointRow</code> and <code>RowKind</code> implement <code>serde::Serialize</code> and
<code>Deserialize</code>, so a store that keeps whole rows can encode them with any serde format. A store
that maps rows to columns needs a column for each field, <code>kind</code> and
<code>workflow_version</code> included: resume routes rows by <code>kind</code> and rejects rows whose
<code>workflow_version</code> differs from the workflow's.
</p>

<h3 id="custom-impl"><a href="#custom-impl" class="anchor-link" aria-hidden="true">#</a>Implement the trait</h3>
<p>
<code>#[cano::checkpoint_store]</code> on an inherent <code>impl</code> block writes the
<code>impl CheckpointStore for ...</code> header and makes the <code>async fn</code>s fit the trait. You
can also write the header yourself and put the attribute on it:
<code>#[cano::checkpoint_store] impl CheckpointStore for MapStore { ... }</code>. This in-memory store
keeps the full contract; replace the map with your storage calls:
</p>

```rust
use cano::checkpoint::CheckpointRow;
use cano::CanoError;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct MapStore(Mutex<HashMap<String, Vec<CheckpointRow>>>);

#[cano::checkpoint_store]
impl MapStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let mut runs = self.0.lock().unwrap();
        let rows = runs.entry(workflow_id.to_string()).or_default();
        // Check and insert under one lock: the duplicate check must be atomic.
        if rows.iter().any(|r| r.sequence == row.sequence) {
            return Err(CanoError::checkpoint_store(format!(
                "checkpoint conflict: {workflow_id:?} already has sequence {}",
                row.sequence
            )));
        }
        rows.push(row);
        Ok(())
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let mut rows = self.0.lock().unwrap().get(workflow_id).cloned().unwrap_or_default();
        rows.sort_by_key(|r| r.sequence);
        Ok(rows)
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.0.lock().unwrap().remove(workflow_id);
        Ok(())
    }
}
```

<ul>
<li>Return storage failures as <code>CanoError::checkpoint_store(message)</code>. The engine wraps an
<code>append</code> error with the state it was writing, and observers and the <code>metrics</code>
feature classify it as <code>checkpoint_store</code>.</li>
<li>Do not block the async runtime. Use the backend's async client, or move a blocking driver call into
<code>tokio::task::spawn_blocking</code>.</li>
<li>Share connections inside the store (a pool, or an <code>Arc</code> around the client) and wrap the
store itself in an <code>Arc</code> for the workflow. The trait requires
<code>Send + Sync + 'static</code>.</li>
<li>Create tables, buckets or directories in the store's constructor, not on the first
<code>append</code>, so a misconfigured store fails at startup rather than in the middle of a run.</li>
</ul>

<h3 id="custom-network"><a href="#custom-network" class="anchor-link" aria-hidden="true">#</a>Network backends: timeouts and lost acknowledgements</h3>
<p>
A store that talks to a server has two more problems: a call that never returns, and a write that
succeeded on the server while its acknowledgement was lost.
</p>
<ul>
<li><strong>Bound every call.</strong> The engine waits for <code>append</code> before it runs the task,
so a hung call stalls the run. Wrap each attempt in <code>tokio::time::timeout</code> and return an
<code>Err</code> when it fires. Drop a connection whose call timed out instead of reusing it.</li>
<li><strong>Retry only transient errors.</strong> A dropped connection or a timeout can be retried; a
duplicate-key error or a row that does not decode cannot.</li>
<li><strong>Recognize your own earlier write.</strong> When a retried <code>append</code> finds a row at
its sequence, that row may be its own first attempt. Comparing row contents is not enough, because two
fresh runs that share an id write identical rows at sequence 0. Draw a random write id once per
<code>append</code> call, store it with the row, and treat an existing row as your own only when both
the contents and the write id match; anything else is a conflict. <code>PostgresStore</code> and
<code>NatsStore</code> work this way.</li>
</ul>
<p>
A store that cannot tell a lost acknowledgement from a conflict can still return <code>Err</code> for
both: the run fails, and <code>resume_from</code> later continues from the rows that did land.
</p>

<h3 id="custom-test"><a href="#custom-test" class="anchor-link" aria-hidden="true">#</a>Test it against the contract</h3>
<p>
The <code>testing</code> feature's
<a href="../../testing/#store-contract"><code>assert_checkpoint_store_contract</code></a> runs every
clause above against a store: field round-trips for each <code>RowKind</code>, sorting, duplicate
rejection, prefix isolation under <code>clear</code>, awkward ids, concurrent appends and a 256 KiB blob.
It panics naming the clause that failed. It only touches ids under the namespace you pass and clears them
before and after, so it can run against a shared database.
</p>

```toml
[dev-dependencies]
cano = { version = "0.16", features = ["testing"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
#[cfg(test)]
mod tests {
    use super::MapStore;
    use cano::testing::assert_checkpoint_store_contract;

    #[tokio::test]
    async fn map_store_keeps_the_contract() {
        let store = MapStore::default();
        assert_checkpoint_store_contract(&store, "contract").await;
    }
}
```

<h3 id="custom-attach"><a href="#custom-attach" class="anchor-link" aria-hidden="true">#</a>Attach it to a workflow</h3>
<p>
A custom store attaches like a built-in one, through <code>with_checkpoint_store</code> and a workflow id:
</p>

```rust
let store = Arc::new(MapStore::default());

let workflow = Workflow::bare()
    .register(Step::Start, StartTask)
    .register(Step::Work, WorkTask)
    .add_exit_state(Step::Done)
    .with_checkpoint_store(store.clone())
    .with_workflow_id("run-42");

workflow.orchestrate(Step::Start).await?;
assert!(store.load_run("run-42").await?.is_empty()); // cleared at the exit state
```

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example custom_checkpoint_store</code>. An in-memory store built
with <code>#[cano::checkpoint_store]</code>, including the duplicate rejection, wired into a workflow
that crashes once and continues with <code>resume_from</code>. It needs no feature flag.</p>
</div>
<hr class="section-divider">

<h2 id="network-failures"><a href="#network-failures" class="anchor-link" aria-hidden="true">#</a>Connection failures, retries and outages</h2>
<p>
Both network stores (<code>PostgresStore</code> and <code>NatsStore</code>) recover from dropped
connections automatically without recreating the store. <code>RedbStore</code> operates on an embedded
local file, so it has no network connection to drop or reconnect.
</p>

<h3 id="reconnection"><a href="#reconnection" class="anchor-link" aria-hidden="true">#</a>Automatic reconnection</h3>
<dl class="key-list">
<dt><code>PostgresStore</code> <small>pool eviction and keepalives</small></dt>
<dd>Connections come from a <code>deadpool-postgres</code> pool. Whenever an operation hits a transient error (a closed socket, I/O error, operation timeout, cancelled caller, or transient SQLSTATE such as <code>08xxx</code> or <code>57Pxx</code>), the broken connection is evicted and closed rather than returned to the pool, so the next checkout opens a fresh connection automatically. TCP keepalives default to 30 seconds idle so dead peers surface promptly. On initial <code>connect</code>, the table migration runs under <code>with_retry</code> as well.</dd>

<dt><code>NatsStore</code> <small>background client reconnect</small></dt>
<dd><code>NatsStore</code> wraps an <code>async_nats::Client</code>, which reconnects in the background according to its <code>ConnectOptions</code> (pass custom reconnect settings, credentials, or ping intervals through <code>connect_with_options</code>). While the client is disconnected, each attempt checks <code>connection_state()</code> and fails at once as a transient error rather than waiting out the operation timeout; once the client reconnects, calls on the same <code>NatsStore</code> succeed again. Initial <code>connect</code> and <code>connect_with_options</code> bound the first TCP handshake by <code>operation_timeout</code> and apply <code>with_retry</code> when opening or creating the KV buckets.</dd>
</dl>

<h3 id="retries-and-lost-acks"><a href="#retries-and-lost-acks" class="anchor-link" aria-hidden="true">#</a>Per-call timeouts, retries and lost acknowledgements</h3>
<p>
<code>PostgresStore</code> and <code>NatsStore</code> bound every attempt of every call, checkpoint
(<code>append</code>, <code>load_run</code>, <code>clear</code>) and scheduler (<code>load</code>,
<code>compare_and_swap</code>, <code>remove</code>) alike, with <code>operation_timeout</code> (10 s by
default). Per-call retries default to <code>RetryMode::None</code>: without <code>with_retry</code>, a
call during a connection drop fails once, evicts or reconnects the transport, and returns
<code>Err(CanoError::CheckpointStore(...))</code>; later calls succeed once connectivity heals.
</p>
<p>
With <code>with_retry(RetryMode::...)</code>, transient failures (timeouts, dropped connections,
Postgres serialization or deadlock failures, server restarts) are retried with equal jitter in
<code>[50%, 100%]</code> of the configured delay so instances that lost the store together do not retry
in lockstep. Conflicts and malformed rows are fatal and never retried. A Postgres connection whose
call timed out or was cancelled is closed and never returned to the pool.
</p>
<p>
An <code>Err</code> from <code>append</code> or <code>compare_and_swap</code> after a timeout does not prove
the write is absent: it may have landed after the client gave up. Every <code>append</code> stamps a
random 16-byte write id (the <code>write_id</code> column in <code>PostgresStore</code>, the row header in
<code>NatsStore</code>), and a retried <code>append</code> treats an existing row at its sequence as its own
earlier write only when both the contents and the write id match. Two fresh runs that start at sequence
<code>0</code> with the same initial state therefore never mistake each other's row for their own. A
retried <code>compare_and_swap</code> likewise accepts a stored record whose bytes equal what it sent;
the scheduler stamps every record with its writer, session, timestamp and a write counter, so equal bytes
mean the earlier attempt landed.
</p>

<h3 id="prolonged-outages"><a href="#prolonged-outages" class="anchor-link" aria-hidden="true">#</a>When the connection does not come back</h3>
<p>
<code>RetryMode</code> has no unbounded variant, so store calls never hang forever when a backend stays
unreachable past all configured attempts:
</p>
<dl class="key-list">
<dt>Store layer <small>finite exhaustion</small></dt>
<dd>Once <code>RetryMode::max_attempts()</code> is exhausted, the call returns <code>Err(CanoError::CheckpointStore(...))</code> (or <code>CanoError::SchedulerStore(...)</code> on scheduler calls). <code>PostgresStore</code> leaves broken slots evicted and stays idle until another call runs. <code>NatsStore</code>'s underlying <code>async_nats::Client</code> continues background reconnect attempts unless capped via <code>ConnectOptions::max_reconnects</code>.</dd>

<dt><code>append</code> during a run <small><code>orchestrate</code>, <code>resume_from</code>, <code>resume_with</code></small></dt>
<dd>State-entry rows are written before the incoming state's task runs, so that task never executes when the write fails. If a <code>CompensatableTask</code> finished its forward <code>run</code> and its completion-row <code>append</code> fails, Cano still pushes its output onto the in-memory compensation stack before aborting so that step is rolled back too. The engine stops the FSM, drains any in-memory saga compensations in LIFO order (without needing the checkpoint store), runs resource <code>teardown</code>, and returns <code>CanoError::WithStateContext</code> wrapping <code>CanoError::CheckpointStore</code> (or <code>CanoError::CompensationFailed</code> if a compensator also failed). Pre-outage rows remain in the store: if no saga steps rolled back, call <code>resume_from</code> once connectivity returns (or <code>orchestrate</code> if sequence <code>0</code> never landed); if saga steps rolled back in memory while <code>clear</code> could not reach the store, call <code>store.clear(id)</code> before reusing that id rather than calling <code>resume_from</code>.</dd>

<dt><code>load_run</code> on resume <small><code>resume_from</code>, <code>resume_with</code></small></dt>
<dd>Tears down resources and returns <code>Err(CanoError::CheckpointStore(...))</code> before any task runs. Retry the call once the store answers again.</dd>

<dt><code>clear</code> at completion <small>exit state or clean saga rollback</small></dt>
<dd>Final log cleanup is best-effort. A completed run still returns <code>Ok(exit_state)</code> and a cleanly rolled-back saga still returns its original error. Cano logs a <code>tracing</code> warning, records a failed clear metric, and fires <code>WorkflowObserver::on_checkpoint_clear_failed(workflow_id, error)</code>. Because the old rows remain stored, a fresh <code>orchestrate</code> with the same <code>workflow_id</code> later hits a sequence-0 conflict until <code>store.clear(workflow_id)</code> removes them, and calling <code>resume_from</code> on a rolled-back saga's stranded log would compensate its steps again.</dd>

<dt>Under <code>Scheduler</code> <small>backoff, trips and leases</small></dt>
<dd>When only the workflow's <code>CheckpointStore</code> fails, each failed run feeds the flow's <code>BackoffPolicy</code> (<code>Status::Backoff</code>, and <code>Status::Tripped</code> when <code>streak_limit</code> is set). Once the store recovers, the next tick clears a stranded exit log or auto-resumes an unfinished run; see <a href="../../scheduler/#checkpoint-resume">checkpoints and auto-resume</a>. When the same store backs <code>SchedulerStore</code>, a run that cannot renew its lease for a whole <code>lease_ttl</code> is cancelled via its <code>CancellationToken</code> without writing an outcome, and due ticks skip failed claims and retry until the store answers; see <a href="../../scheduler/several-instances/#leases">leases and failures</a>.</dd>
</dl>

<h3 id="handling-outages"><a href="#handling-outages" class="anchor-link" aria-hidden="true">#</a>Handling store outages in code</h3>
<p>
Configure bounded operation timeouts and jittered retries on the builder so short blips recover
transparently while prolonged outages still fail in predictable time:
</p>

```rust
use cano::store::nats::async_nats::ConnectOptions;
use cano::{CanoError, NatsStore, PostgresStore, RetryMode};
use std::sync::Arc;
use std::time::Duration;

async fn postgres_store(dsn: &str) -> Result<Arc<PostgresStore>, CanoError> {
    let store = PostgresStore::builder()
        .with_operation_timeout(Duration::from_secs(3))
        .with_retry(RetryMode::exponential(4))
        .connect(dsn)
        .await?;
    Ok(Arc::new(store))
}

async fn nats_store(url: &str) -> Result<Arc<NatsStore>, CanoError> {
    let timeout = Duration::from_secs(3);
    let options = ConnectOptions::new()
        .name("orders")
        .request_timeout(Some(timeout))
        .ping_interval(Duration::from_secs(10));
    let store = NatsStore::builder()
        .with_operation_timeout(timeout)
        .with_retry(RetryMode::exponential(4))
        .connect_with_options(url, options)
        .await?;
    Ok(Arc::new(store))
}
```

<p>
When <code>orchestrate</code> still fails with <code>CanoError::CheckpointStore</code> after retries are
exhausted, keep the existing store instance and inspect <code>store.load_run(run_id)</code> once the
backend recovers: if no rows landed, start fresh with <code>orchestrate</code>; otherwise continue from the
last durable checkpoint with <code>resume_from</code> (unless saga steps already rolled back in memory, in
which case clear the log before starting fresh):
</p>

```rust
use cano::PostgresStore;
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Start, Work, Done }

async fn run_or_recover(
    store: &PostgresStore,
    workflow: &Workflow<Step>,
    run_id: &str,
) -> Result<Step, CanoError> {
    match workflow.orchestrate(Step::Start).await {
        Ok(state) => return Ok(state),
        Err(err) => match err.inner() {
            CanoError::CheckpointStore(msg) => {
                eprintln!("store unreachable on {run_id}: {msg}");
                // Keep `store` as is: broken pooled connections were evicted.
            }
            CanoError::CompensationFailed { errors } => {
                // `errors[0]` is the wrapped trigger; `errors[1..]` are rollback errors.
                eprintln!("rollback failed ({} error(s)): {err}", errors.len() - 1);
                return Err(err);
            }
            _ => return Err(err),
        },
    }

    // Wait for Postgres to answer again, then check what landed before the drop.
    // (If this workflow ran CompensatableTask steps, they already rolled back in
    // memory; call `store.clear(run_id).await?` before starting a fresh run.)
    let rows = loop {
        match store.load_run(run_id).await {
            Ok(rows) => break rows,
            Err(_) => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    };
    if rows.is_empty() {
        workflow.orchestrate(Step::Start).await
    } else {
        workflow.resume_from(run_id).await
    }
}
```

<p>
Because final <code>clear</code> is best-effort, attach a <code>WorkflowObserver</code> implementing
<code>on_checkpoint_clear_failed</code> to record workflow ids whose logs could not be deleted, then
sweep them once the store is reachable again:
</p>

```rust
use cano::prelude::*;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct StrandedClearTracker {
    pending_ids: Mutex<Vec<String>>,
}

impl StrandedClearTracker {
    async fn sweep(&self, store: &dyn CheckpointStore) {
        let ids = std::mem::take(&mut *self.pending_ids.lock().unwrap());
        for id in ids {
            if let Err(err) = store.clear(&id).await {
                eprintln!("clear still failing for {id}: {err}");
                let mut pending = self.pending_ids.lock().unwrap();
                if !pending.iter().any(|existing| existing == &id) {
                    pending.push(id);
                }
            }
        }
    }
}

impl WorkflowObserver for StrandedClearTracker {
    fn on_checkpoint_clear_failed(&self, workflow_id: &str, error: &CanoError) {
        eprintln!("checkpoint clear failed for {workflow_id}: {error}");
        let mut pending = self.pending_ids.lock().unwrap();
        if !pending.iter().any(|id| id == workflow_id) {
            pending.push(workflow_id.to_owned());
        }
    }
}

fn attach_tracker<S>(
    workflow: Workflow<S>,
    store: Arc<dyn CheckpointStore>,
    tracker: Arc<StrandedClearTracker>,
) -> Workflow<S>
where
    S: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync + 'static,
{
    workflow
        .with_checkpoint_store(store)
        .with_observer(tracker)
}
```
</div>
