+++
title = "Several scheduler instances"
description = "Run one set of Cano scheduler flows on several instances with a shared SchedulerStore: each tick runs on exactly one instance, leases cover crashes, and backoff and trip state survive restarts."
template = "page.html"
weight = 2
+++

<div class="content-wrapper">

<h1>Several scheduler instances</h1>
<p class="subtitle">Run the same flows on several processes, with each tick executed once across all of them.</p>
<p class="feature-tag">Behind the <code>scheduler</code> feature, plus the feature of the store you share: <code>store-redb</code> (on by default), <code>store-postgres</code> or <code>store-nats</code>.</p>

<p>
See <a href="../">Scheduler</a> for the strategies and the lifecycle. This page covers what changes when
several schedulers share one <code>SchedulerStore</code>.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#when">When to use it</a></li>
<li><a href="#setup">Setup</a></li>
<li><a href="#one-run-per-tick">One run per tick</a></li>
<li><a href="#leases">Leases and failures</a></li>
<li><a href="#restarts">Restarts</a></li>
<li><a href="#status">Status and cancelling</a></li>
<li><a href="#requirements">Clocks, ids and clusters</a></li>
<li><a href="#stores">Choosing a store</a></li>
<li><a href="#custom-store">Writing your own store</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="when"><a href="#when" class="anchor-link" aria-hidden="true">#</a>When to use it</h2>
<p>
Run a scheduler on several instances when one instance is a single point of failure, or when you deploy
the same service to several hosts and a cron job must still fire once. Without <code>with_store</code> every
scheduler keeps its flows in memory and runs them alone, so two copies of the service would run every flow
twice. With a shared store the copies take turns: whichever instance claims a tick runs it, and the others
skip it.
</p>

<h2 id="setup"><a href="#setup" class="anchor-link" aria-hidden="true">#</a>Setup</h2>
<p>
Every instance runs the same code against the same store. <code>with_store</code> turns coordination on,
<code>with_lease_ttl</code> sets how long a run may go unrenewed before another instance takes the flow
(30 s by default), and <code>with_instance_id</code> names the instance in leases and statuses (a random
<code>cano-</code> plus 16 hex digits by default). A zero TTL or an empty id makes <code>start()</code> return
a configuration error.
</p>

```rust
use cano::PostgresStore;
use cano::prelude::*;
use std::sync::Arc;
use std::time::Duration;

let store = PostgresStore::builder()
    .connect("postgres://cano:secret@db.internal/app")
    .await?;

let running = Scheduler::new()
    .every("fetch", workflow, State::Start, Duration::from_secs(60))
    .with_store(Arc::new(store))
    .with_lease_ttl(Duration::from_secs(30))
    .with_instance_id("worker-1")
    .start()
    .await?;
```

<p>
A NATS cluster differs only in the store. <code>NatsStore</code> keeps the records in a JetStream Key/Value
bucket.
</p>

```rust
use cano::NatsStore;
use cano::prelude::*;
use std::sync::Arc;
use std::time::Duration;

let store = NatsStore::builder()
    .connect("nats://nats.internal:4222")
    .await?;

let running = Scheduler::new()
    .every("fetch", workflow, State::Start, Duration::from_secs(60))
    .with_store(Arc::new(store))
    .with_instance_id("worker-1")
    .start()
    .await?;
```

<p>
The <code>scheduler_cluster</code> example runs two schedulers in one process over one shared
<code>RedbStore</code> and prints how the ticks split between them:
<code>cargo run --example scheduler_cluster --features scheduler</code>.
</p>

<h2 id="one-run-per-tick"><a href="#one-run-per-tick" class="anchor-link" aria-hidden="true">#</a>One run per tick</h2>
<p>
Each flow has one record in the store. A tick runs on the instance that wins a compare-and-swap on that
record. The loser reads the new record and skips the tick.
</p>

<div class="diagram-frame">
<p class="diagram-label">Two instances, one run per tick</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 196" role="img" aria-labelledby="sched-cluster-title sched-cluster-desc">
<title id="sched-cluster-title">Two instances share one every flow</title>
<desc id="sched-cluster-desc">Two scheduler instances, node-a and node-b, run the same every flow against one store. At each of four ticks exactly one instance claims the run and the other skips it. node-a runs the first tick, node-b the second, and node-a the third and fourth.</desc>
<text class="t-strong" x="16" y="70">node-a</text>
<text class="t-strong" x="16" y="120">node-b</text>
<rect class="band-hot" x="120" y="52" width="56" height="26" rx="6"/>
<text class="t-mut" x="132" y="119">skip</text>
<text class="t-mut" x="312" y="69">skip</text>
<rect class="band-hot" x="300" y="102" width="56" height="26" rx="6"/>
<rect class="band-hot" x="480" y="52" width="56" height="26" rx="6"/>
<text class="t-mut" x="492" y="119">skip</text>
<rect class="band-hot" x="660" y="52" width="56" height="26" rx="6"/>
<text class="t-mut" x="672" y="119">skip</text>
<text class="t-mut" x="136" y="69">run</text>
<text class="t-mut" x="316" y="119">run</text>
<text class="t-mut" x="496" y="69">run</text>
<text class="t-mut" x="676" y="69">run</text>
<line class="axis" x1="120" y1="150" x2="740" y2="150"/>
<line class="tick" x1="120" y1="150" x2="120" y2="156"/>
<line class="tick" x1="300" y1="150" x2="300" y2="156"/>
<line class="tick" x1="480" y1="150" x2="480" y2="156"/>
<line class="tick" x1="660" y1="150" x2="660" y2="156"/>
<text class="t-mut" x="120" y="172">tick 1</text>
<text class="t-mut" x="300" y="172">tick 2</text>
<text class="t-mut" x="480" y="172">tick 3</text>
<text class="t-mut" x="660" y="172">tick 4</text>
</svg>
</div>
</div>

<dl class="key-list">
<dt><code>every</code> <small>interval</small></dt>
<dd>Runs once per interval across the cluster. A tick is due when one interval has passed since the last claimed run started and no backoff window is open.</dd>

<dt><code>cron</code> <small>expression</small></dt>
<dd>Runs once per firing. The first instance to claim a firing records it, and every other instance skips that firing. A firing inside a backoff window is skipped.</dd>

<dt><code>manual</code> <small><code>trigger(id).await</code></small></dt>
<dd>Claims the flow in the store before it spawns the run. The call returns an error while another instance holds the flow's lease, and the message names that instance. A trigger ignores a backoff window and is refused on a tripped flow, as on a single instance. The claim counts as a run in the record.</dd>
</dl>

<h2 id="leases"><a href="#leases" class="anchor-link" aria-hidden="true">#</a>Leases and failures</h2>
<p>
Claiming a run writes a lease into the flow's record: the instance id and an expiry one TTL ahead. The
running instance renews the lease every third of the TTL. When the run ends, one write records the outcome
and clears the lease.
</p>
<dl class="key-list">
<dt>Lease lost <small>no renewal for a whole TTL, or another instance took the flow over</small></dt>
<dd>The local run is cancelled through its token, so a saga drains its compensation stack. The outcome is not written, because the record now belongs to the other instance.</dd>

<dt>Crash <small>the process dies mid-run</small></dt>
<dd>The lease expires within one TTL. The next instance that looks at the flow counts the lost run as one failure with the error <code>lease expired: ...</code> and carries on. That failure feeds the flow's <a href="../backoff-and-trip/">backoff and trip</a> state like any other.</dd>

<dt>Store outage <small>the store errors or times out</small></dt>
<dd>A store call that does not answer within one TTL fails like an error. A failed claim skips the tick, and the scheduler stays up (with the <code>tracing</code> feature the error is logged). A run that cannot renew for a whole TTL is cancelled as a lost lease. A release that keeps failing is retried a few times, then left to expire and counted as one failure, unless the same instance claims the flow again before the lease expires.</dd>

<dt>Shutdown abort <small><code>stop()</code> aborts a run after the cancel timeout</small></dt>
<dd>The aborted run leaves its lease to expire and is counted as one failure, the same as a crash. A run that finishes during the drain or cancel phase writes its outcome as usual.</dd>
</dl>
<p>
A crash blocks a flow for at most one TTL. Pick a TTL several times longer than the worst store latency,
because an instance that cannot reach the store for a whole TTL cancels its run.
</p>

<h2 id="restarts"><a href="#restarts" class="anchor-link" aria-hidden="true">#</a>Restarts</h2>
<p>
The record holds <code>run_count</code>, <code>last_run</code>, the failure streak and the backoff window,
so they outlive any instance. A restarted cluster does not run an <code>every</code> flow again until its
interval has passed since the last claimed run, and a flow that was never written runs at once. A
<code>Tripped</code> flow stays tripped until <code>reset_flow(id).await</code>, which clears the streak in
the shared record and so applies to every instance. A reset leaves the lease of a run in flight alone.
</p>

<h2 id="status"><a href="#status" class="anchor-link" aria-hidden="true">#</a>Status and cancelling</h2>
<p>
A flow whose lease another live instance holds reports
<code>Status::RunningElsewhere { owner, lease_expires }</code>. The same status shows for a crashed owner
until the lease expires. <code>status()</code> mirrors the store as of this instance's last read. The
instance refreshes it at <code>start</code>, at every dispatch attempt of an <code>every</code> or
<code>cron</code> flow, and on <code>trigger</code> and <code>reset_flow</code>, so it can lag another
instance's run by up to one interval.
</p>
<p>
<code>cancel_flow(id)</code> only cancels a run on the instance you call it on. On an instance that shows
<code>RunningElsewhere</code> it does nothing, so call it on the instance that owns the lease.
<code>RunningScheduler::instance_id()</code> returns this instance's id.
</p>

```rust
match node.status("fetch").map(|info| info.status) {
    Some(Status::RunningElsewhere { owner, lease_expires }) => {
        println!("fetch runs on {owner} until {lease_expires}");
    }
    Some(other) => println!("fetch: {other:?}"),
    None => println!("unknown flow"),
}
```

<h2 id="requirements"><a href="#requirements" class="anchor-link" aria-hidden="true">#</a>Clocks, ids and clusters</h2>
<ul>
<li>Lease expiry compares wall clocks across instances. Keep the clocks synchronised, with a skew well
below the TTL.</li>
<li>Every instance needs a distinct <code>with_instance_id</code>. Two instances with one id treat each
other's lease as their own. A stable name such as a pod or host name keeps logs and statuses readable.</li>
<li>Instances that share a table, bucket or file form one cluster. Give every deployment that must not
share flows its own table or bucket, or two unrelated services with a flow called <code>fetch</code> will
coordinate with each other.</li>
</ul>

<h2 id="stores"><a href="#stores" class="anchor-link" aria-hidden="true">#</a>Choosing a store</h2>
<p>
All three built-in stores also implement <code>CheckpointStore</code>, and the scheduler records live
beside the checkpoints. The scheduler needs only the store's compare-and-swap, so any of them can serve.
</p>
<dl class="key-list">
<dt><code>RedbStore</code> <small>table <code>cano_scheduler</code> and <code>cano_scheduler_version</code>, same file</small></dt>
<dd>One process holds a redb file. The state survives restarts and schedulers inside that process coordinate, which suits tests and a single service with several schedulers. It cannot coordinate separate processes.</dd>

<dt><code>PostgresStore</code> <small>table <code>cano_scheduler</code>, <code>with_scheduler_table</code></small></dt>
<dd>Coordinates processes and hosts that reach one database. The record is JSON in a <code>bytea</code> column, and a sequence named <code>cano_scheduler_version_seq</code> hands out versions.</dd>

<dt><code>NatsStore</code> <small>bucket <code>cano_scheduler</code>, <code>with_scheduler_bucket</code>, <code>with_scheduler_bucket_config</code></small></dt>
<dd>Coordinates processes that reach one JetStream. One key per flow, versioned by the key's revision. <code>scheduler_kv()</code> returns the bucket handle.</dd>
</dl>
<p>
The <a href="../../recovery/#postgres">Postgres</a> and <a href="../../recovery/#nats">NATS</a> sections
of the recovery guide list the other builder settings, which apply to the scheduler records too.
</p>

<h2 id="custom-store"><a href="#custom-store" class="anchor-link" aria-hidden="true">#</a>Writing your own store</h2>
<p>
<code>SchedulerStore</code> is a versioned compare-and-swap register keyed by flow id. The scheduler keeps
every decision in the record's bytes, so a backend only stores and swaps opaque values. The contract:
</p>
<ul>
<li><code>load</code> returns <code>None</code> for an id never written or removed.</li>
<li><code>compare_and_swap</code> with <code>expected = None</code> writes only when no record exists, and
with <code>Some(v)</code> only when the current version is <code>v</code>. Otherwise it returns
<code>CasOutcome::Conflict</code> and changes nothing. Of concurrent swaps from one version, exactly one
is written.</li>
<li>A written record gets a version different from every earlier version of that id.</li>
<li>A store that retries a swap whose acknowledgement was lost must report <code>Written</code> when the
stored value equals the value it sent. The scheduler's values carry the writer and a timestamp, so equal
bytes mean the earlier attempt landed.</li>
<li><code>remove</code> deletes one id, and removing an unknown id is <code>Ok</code>.</li>
<li>Ids are arbitrary UTF-8 (spaces, <code>.</code>, <code>/</code>, <code>*</code>, non-ASCII, 200 or more
characters) and stay isolated from each other.</li>
</ul>
<p>
Annotate the impl with <code>#[cano::scheduler_store]</code>. Errors use
<code>CanoError::scheduler_store(message)</code>, which has the category <code>scheduler_store</code>.
</p>

```rust
use cano::prelude::*;
use cano::scheduler::{CasOutcome, VersionedRecord};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MapStore {
    // The last version handed out, and the records.
    inner: Mutex<(u64, HashMap<String, VersionedRecord>)>,
}

#[cano::scheduler_store]
impl SchedulerStore for MapStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        Ok(self.inner.lock().unwrap().1.get(flow_id).cloned())
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        let mut guard = self.inner.lock().unwrap();
        let (last_version, records) = &mut *guard;
        if records.get(flow_id).map(|r| r.version) != expected {
            return Ok(CasOutcome::Conflict);
        }
        *last_version += 1;
        let version = *last_version;
        records.insert(flow_id.to_string(), VersionedRecord { version, value });
        Ok(CasOutcome::Written(version))
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        self.inner.lock().unwrap().1.remove(flow_id);
        Ok(())
    }
}
```

<p>
With the <code>testing</code> feature, <code>assert_scheduler_store_contract</code> runs these clauses
against your store and panics with the name of the first one it breaks. A single process can use
<code>InMemorySchedulerStore</code> to test several schedulers without a database. Both are described on the
<a href="../../testing/#scheduler-store-contract">testing page</a>.
</p>

```rust
use cano::testing::assert_scheduler_store_contract;

#[tokio::test]
async fn map_store_meets_the_contract() {
    let store = MapStore::default();
    assert_scheduler_store_contract(&store, "contract-test").await;
}
```
</div>
