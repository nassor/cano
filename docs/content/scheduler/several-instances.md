+++
title = "Several scheduler instances"
description = "Run one set of Cano scheduler flows on several instances with a shared SchedulerStore: each tick runs on exactly one instance, leases cover crashes, and backoff and trip state survive restarts."
template = "page.html"
weight = 2
+++

<div class="content-wrapper">

<h1>Several scheduler instances</h1>
<p class="subtitle">Run the same flows on several processes, with each tick executed once across all of them.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.17.0</span>
<span class="meta-tag meta-tag-native">not wasm-supported</span>
</div>
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

// Opens the Postgres pool and creates cano_scheduler (and cano_checkpoints) if absent.
let store = PostgresStore::connect("postgres://cano:secret@db.internal/app").await?;

let running = Scheduler::new()
    .every("fetch", workflow, State::Start, Duration::from_secs(60))
    // Enables cross-instance CAS coordination over the shared table.
    .with_store(Arc::new(store))
    // Lease duration before a peer considers a silent run crashed (renewed every ttl / 3).
    .with_lease_ttl(Duration::from_secs(30))
    // Owner label recorded in leases and Status::RunningElsewhere.
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

// Connects to NATS and opens/creates the JetStream KV buckets (uses default 30 s lease TTL).
let store = NatsStore::connect("nats://nats.internal:4222").await?;

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
<dd>Runs at a fixed rate across the cluster. A tick is due once one interval has passed since the last claimed run started (the claim records that start) and no backoff window is open. A run that takes longer than the interval is followed by the next one at once.</dd>

<dt><code>cron</code> <small>expression</small></dt>
<dd>Runs once per firing, in UTC. The first instance to claim a firing records it, and every other instance skips that firing. A firing inside a backoff window is skipped, and firings missed during an outage are skipped rather than replayed. When a store error stops the claim of a due firing, the loop retries every <code>ttl / 3</code> (at least 50 ms, at most 5 s) until the next firing.</dd>

<dt><code>manual</code> <small><code>trigger(id).await</code></small></dt>
<dd>Claims the flow in the store before it spawns the run. The call returns an error while another instance holds the flow's lease, and the message names that instance. A trigger ignores a backoff window and is refused on a tripped flow, as on a single instance. The claim counts as a run in the record.</dd>
</dl>

<h2 id="leases"><a href="#leases" class="anchor-link" aria-hidden="true">#</a>Leases and failures</h2>
<p>
Claiming a run writes a lease into the flow's record: the instance id, a random session token drawn
when the scheduler starts, a lease number unique to the claim, and an expiry one TTL ahead. The running
instance renews the lease every third of the TTL. When the run ends, one compare-and-swap from the
version it last wrote records the outcome and clears the lease. On a conflict or a store error it reloads
the record and writes only if the lease there is still its own.
</p>
<div class="diagram-frame">
<p class="diagram-label">Lease lifecycle of one run</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 496" role="img" aria-labelledby="sched-lease-title sched-lease-desc">
<title id="sched-lease-title">Lease lifecycle of one run</title>
<desc id="sched-lease-desc">Sequence between a scheduler instance and the SchedulerStore. When the flow is due, the instance loads the record at version v1 and claims it with a compare-and-swap from v1 that writes a lease with its owner id, session, lease number and an expiry one TTL ahead; the store answers Written v2. While the workflow runs, the instance renews the lease every third of the TTL with a compare-and-swap from the version it holds, moving the record to v3 and v4. When the run ends, a compare-and-swap from v4 records the outcome and clears the lease at v5.</desc>
<defs><marker id="sched-lease-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="200" y1="52" x2="200" y2="430"/>
<line class="lifeline" x1="600" y1="52" x2="600" y2="430"/>
<rect class="n" x="100" y="10" width="200" height="42" rx="8"/>
<text class="t-strong" x="200" y="25">Scheduler instance</text>
<text class="t-mut t-code" x="200" y="42">every, cron or trigger()</text>
<rect class="n-cop" x="500" y="10" width="200" height="42" rx="8"/>
<text class="t-strong" x="600" y="25">SchedulerStore</text>
<text class="t-mut" x="600" y="42">Postgres, NATS or redb</text>
<g class="cd-loop"><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.93;0.99;1" values="1;1;0;0"/>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.04;0.08;1" values="0;0;1;1"/>
<path class="e" d="M200,90 H596" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-code" x="400" y="78">due: load(flow_id)</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.105;0.145;1" values="0;0;1;1"/>
<path class="e e-dash" d="M600,122 H204" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut" x="400" y="110">record at version v1</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.17;0.21;1" values="0;0;1;1"/>
<path class="e e-hot" d="M200,166 H596" marker-end="url(#sched-lease-ah)"/>
<text class="t-code" x="400" y="154">CAS(v1): lease = owner, session, id, now + ttl</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.235;0.275;1" values="0;0;1;1"/>
<path class="e e-dash" d="M600,198 H209" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-ok t-code" x="400" y="186">Written(v2)</text>
</g>
<rect class="band-hot" x="195" y="198" width="10" height="188" rx="5"><animate attributeName="height" dur="10s" repeatCount="indefinite" keyTimes="0;0.235;0.625;1" values="0;0;188;188"/></rect>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.27;0.31;1" values="0;0;1;1"/>
<text class="t-strong ta-e" x="184" y="284">workflow runs</text>
<text class="t-mut ta-e" x="184" y="302">lease held</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.3;0.34;1" values="0;0;1;1"/>
<path class="e" d="M205,244 H596" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-code" x="400" y="232">after ttl / 3: renew CAS(v2)</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.365;0.405;1" values="0;0;1;1"/>
<path class="e e-dash" d="M600,270 H209" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-ok t-code" x="400" y="258">Written(v3): expiry moved out</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.43;0.47;1" values="0;0;1;1"/>
<path class="e" d="M205,314 H596" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-code" x="400" y="302">after ttl / 3: renew CAS(v3)</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.495;0.535;1" values="0;0;1;1"/>
<path class="e e-dash" d="M600,340 H209" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-ok t-code" x="400" y="328">Written(v4)</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.56;0.6;1" values="0;0;1;1"/>
<path class="e e-hot" d="M205,386 H596" marker-end="url(#sched-lease-ah)"/>
<text class="t-code" x="400" y="374">release CAS(v4): outcome, lease cleared</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.625;0.665;1" values="0;0;1;1"/>
<path class="e e-dash" d="M600,412 H204" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-ok t-code" x="400" y="400">Written(v5)</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.7;0.76;1" values="0;0;1;1"/>
<rect class="n-hot" x="20" y="446" width="740" height="34" rx="10"/>
<text class="t-strong" x="390" y="463">Every write names the version it last saw, so only the lease holder can renew or release</text>
</g>
</g>
</svg>
</div>
</div>
<dl class="key-list">
<dt>Fencing <small>record version, owner, session, lease number</small></dt>
<dd>Every write is a compare-and-swap on the version the instance last read, except the very first claim of a flow, which creates the record. A renewal or a release also checks that the stored lease carries the instance's owner id, session and lease number. The session is drawn again on every <code>start</code>, so a process restarted with the same <code>with_instance_id</code> does not mistake its pre-crash lease for its own. The lease number changes on every claim, so a late renewal or release from an earlier run cannot touch a newer run on the same instance. Each record also carries the writer, its session, a timestamp and a write counter, so no two writes produce identical bytes.</dd>

<dt>Lease lost <small>no renewal for a whole TTL, or another instance took the flow over</small></dt>
<dd>The holder measures its lease on a monotonic clock from the moment it sent the claim or renewal, so a backward wall-clock step cannot extend it. When the lease runs out, or a renewal finds another owner, the local run is cancelled through its token and a saga drains its compensation stack; the run is awaited, never dropped. Its outcome is not written, because the record no longer belongs to this instance.</dd>

<dt>Crash <small>the process dies mid-run</small></dt>
<dd>The lease expires within one TTL. The next instance that looks at the flow counts the lost run as one failure with the error <code>lease expired: ...</code>, dated at the lease's expiry rather than at the moment it noticed. A flow checked rarely, such as a daily cron, therefore does not lose its next firing to a backoff window that starts late. That failure feeds the flow's <a href="../backoff-and-trip/">backoff and trip</a> state like any other, so the flow runs again once the backoff delay has passed.</dd>

<dt>Unstarted claim <small>lost acknowledgement, timeout, dropped <code>trigger()</code></small></dt>
<dd>A claim advances <code>run_count</code>, <code>last_run</code> and <code>last_tick</code> and keeps their previous values on the lease. If the claim's acknowledgement is lost, the claim times out, or the <code>trigger()</code> future is dropped before the run starts, the instance writes those values back and clears the lease; nothing counts as a run or a failure. If the store is unreachable at that moment, the instance remembers the claim and undoes it on its next claim of the flow.</dd>

<dt>Store outage <small>the store errors or times out</small></dt>
<dd>Every store call is bounded by the TTL, and a store implementation that panics fails the call instead of the scheduler. A failed claim skips the tick and the scheduler stays up (with the <code>tracing</code> feature the error is logged). An <code>every</code> loop then retries after <code>min(interval, ttl / 3)</code>; a <code>cron</code> loop retries a due firing every <code>ttl / 3</code>, clamped to 50 ms to 5 s, until the next firing. A renewal that fails while the lease has time left keeps the run going and is tried again a third of the TTL later; a run that cannot renew for a whole TTL is cancelled as a lost lease. A release that keeps failing is retried a few times, then left to expire and counted as one failure, unless the same instance claims the flow again before the lease expires. A stored <code>last_error</code> holds at most 4 KiB, cut on a UTF-8 character boundary.</dd>

<dt>Shutdown abort <small><code>stop()</code> aborts a run after the cancel timeout</small></dt>
<dd>The aborted run leaves its lease to expire and is counted as one failure, the same as a crash. A run that finishes during the drain or cancel phase writes its outcome as usual.</dd>
</dl>
<div class="diagram-frame">
<p class="diagram-label">Failover after a partition</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 606" role="img" aria-labelledby="sched-failover-title sched-failover-desc">
<title id="sched-failover-title">Failover after a partition</title>
<desc id="sched-failover-desc">Sequence between instance A, the SchedulerStore and instance B. A claims the flow with a compare-and-swap from v1 and runs it at v2. A partition cuts A off: its renewal never reaches the store. B loads the record and sees A's lease, valid until expires_at. A goes a whole TTL without a renewal, cancels its run and lets a saga roll back. Once the lease has expired, B's claim from v2 records the lost run as one failure and puts the flow in Backoff at v3. After the backoff delay B claims again from v3 and runs the flow at v4. A late release from A still names v2, so the store answers Conflict and A writes nothing.</desc>
<defs><marker id="sched-failover-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="130" y1="52" x2="130" y2="540"/>
<line class="lifeline" x1="390" y1="52" x2="390" y2="540"/>
<line class="lifeline" x1="650" y1="52" x2="650" y2="540"/>
<rect class="n" x="40" y="10" width="180" height="42" rx="8"/>
<text class="t-strong" x="130" y="25">Instance A</text>
<text class="t-mut" x="130" y="42">first lease holder</text>
<rect class="n-cop" x="300" y="10" width="180" height="42" rx="8"/>
<text class="t-strong" x="390" y="25">SchedulerStore</text>
<text class="t-mut" x="390" y="42">the flow's record</text>
<rect class="n" x="560" y="10" width="180" height="42" rx="8"/>
<text class="t-strong" x="650" y="25">Instance B</text>
<text class="t-mut" x="650" y="42">peer</text>
<g class="cd-loop"><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.93;0.99;1" values="1;1;0;0"/>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.03;0.07;1" values="0;0;1;1"/>
<path class="e e-hot" d="M130,86 H386" marker-end="url(#sched-failover-ah)"/>
<text class="t-code" x="260" y="74">claim CAS(v1): owner A, lease 1</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.09;0.13;1" values="0;0;1;1"/>
<path class="e e-dash" d="M390,112 H139" marker-end="url(#sched-failover-ah)"/>
<text class="t-mut t-ok t-code" x="260" y="100">Written(v2)</text>
</g>
<rect class="band-hot" x="125" y="112" width="10" height="140" rx="5"><animate attributeName="height" dur="12s" repeatCount="indefinite" keyTimes="0;0.11;0.37;1" values="0;0;140;140"/></rect>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.16;0.2;1" values="0;0;1;1"/>
<path class="e e-dash" d="M135,150 H300" marker-end="url(#sched-failover-ah)"/>
<text class="t-mut t-err t-code" x="218" y="138">renew CAS(v2)</text>
<text class="t-mut t-warn" x="218" y="164">partition: no answer</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.22;0.26;1" values="0;0;1;1"/>
<path class="e" d="M650,196 H394" marker-end="url(#sched-failover-ah)"/>
<text class="t-mut t-code" x="520" y="184">load(flow_id)</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.28;0.32;1" values="0;0;1;1"/>
<path class="e e-dash" d="M390,222 H646" marker-end="url(#sched-failover-ah)"/>
<text class="t-mut" x="520" y="210">A holds the lease until expires_at</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.35;0.4;1" values="0;0;1;1"/>
<rect class="n-warn" x="30" y="252" width="200" height="44" rx="8"/>
<text class="t-strong" x="130" y="267">no renewal for a TTL</text>
<text class="t-mut" x="130" y="285">cancel run, roll back saga</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.43;0.47;1" values="0;0;1;1"/>
<path class="e e-hot" d="M650,326 H394" marker-end="url(#sched-failover-ah)"/>
<text class="t-code" x="520" y="314">claim CAS(v2): lease expired</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.49;0.53;1" values="0;0;1;1"/>
<path class="e e-dash" d="M390,352 H646" marker-end="url(#sched-failover-ah)"/>
<text class="t-mut t-err t-code" x="520" y="340">Written(v3): one failure, Backoff</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.55;0.59;1" values="0;0;1;1"/>
<path class="e e-hot" d="M650,392 H394" marker-end="url(#sched-failover-ah)"/>
<text class="t-code" x="520" y="380">after the delay: claim CAS(v3)</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.61;0.65;1" values="0;0;1;1"/>
<path class="e e-dash" d="M390,418 H641" marker-end="url(#sched-failover-ah)"/>
<text class="t-mut t-ok t-code" x="520" y="406">Written(v4): owner B</text>
</g>
<rect class="band-hot" x="645" y="418" width="10" height="100" rx="5"><animate attributeName="height" dur="12s" repeatCount="indefinite" keyTimes="0;0.63;0.85;1" values="0;0;100;100"/></rect>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.67;0.71;1" values="0;0;1;1"/>
<path class="e e-dash" d="M130,452 H386" marker-end="url(#sched-failover-ah)"/>
<text class="t-err t-code" x="260" y="440">late release CAS(v2)</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.73;0.77;1" values="0;0;1;1"/>
<path class="e e-dash" d="M390,478 H134" marker-end="url(#sched-failover-ah)"/>
<text class="t-mut t-err t-code" x="260" y="466">Conflict: the record is at v4</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.79;0.83;1" values="0;0;1;1"/>
<rect class="n-err" x="30" y="496" width="200" height="44" rx="8"/>
<text class="t-strong" x="130" y="511">A stands down</text>
<text class="t-mut" x="130" y="529">its outcome is not written</text>
</g>
<g><animate attributeName="opacity" dur="12s" repeatCount="indefinite" keyTimes="0;0.85;0.9;1" values="0;0;1;1"/>
<rect class="n-hot" x="20" y="556" width="740" height="34" rx="10"/>
<text class="t-strong" x="390" y="573">A late write names a version that is gone, so it cannot overwrite the new owner</text>
</g>
</g>
</svg>
</div>
</div>
<div class="diagram-frame">
<p class="diagram-label">A store outage shorter than the TTL</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 336" role="img" aria-labelledby="sched-outage-title sched-outage-desc">
<title id="sched-outage-title">A store outage shorter than the TTL</title>
<desc id="sched-outage-desc">Timeline in thirds of the lease TTL with a store outage from about 0.4 to 0.95 TTL. Top lane: a running flow claims its lease at 0 and renews at one third of the TTL; the renewal at two thirds fails inside the outage, but the lease still has time left, so the run continues; the renewal at one TTL succeeds and the run later releases its lease. Bottom lane: a flow that falls due during the outage fails to claim twice, retrying a third of the TTL later each time, and claims once the store answers again.</desc>
<defs><marker id="sched-outage-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<text class="t-strong ta-e" x="160" y="105">running flow</text>
<text class="t-mut ta-e" x="160" y="123">renews its lease</text>
<text class="t-strong ta-e" x="160" y="190">due flow</text>
<text class="t-mut ta-e" x="160" y="208">tries to claim</text>
<line class="lifeline" x1="180" y1="198" x2="752" y2="198"/>
<line class="axis" x1="180" y1="250" x2="752" y2="250"/>
<path class="e e-dim" d="M752,250 H766" marker-end="url(#sched-outage-ah)"/>
<line class="tick" x1="180" y1="245" x2="180" y2="255"/>
<line class="tick" x1="320" y1="245" x2="320" y2="255"/>
<line class="tick" x1="460" y1="245" x2="460" y2="255"/>
<line class="tick" x1="600" y1="245" x2="600" y2="255"/>
<line class="tick" x1="740" y1="245" x2="740" y2="255"/>
<text class="t-mut" x="180" y="268">0</text>
<text class="t-mut" x="320" y="268">ttl/3</text>
<text class="t-mut" x="460" y="268">2ttl/3</text>
<text class="t-mut" x="600" y="268">ttl</text>
<text class="t-mut" x="740" y="268">4ttl/3</text>
<g class="cd-loop"><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.93;0.99;1" values="1;1;0;0"/>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.238;0.268;1" values="0;0;1;1"/>
<rect class="n-warn" x="350" y="58" width="222" height="172" rx="8"/>
<text class="t-strong t-warn" x="463" y="44">store outage</text>
</g>
<rect class="band-hot" x="180" y="98" width="560" height="30" rx="6"><animate attributeName="width" dur="10s" repeatCount="indefinite" keyTimes="0;0.04;0.7;1" values="0;0;560;560"/></rect>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.04;0.07;1" values="0;0;1;1"/>
<circle class="n-ok" cx="180" cy="113" r="6"/>
<text class="t-mut t-ok" x="180" y="84">claim</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.205;0.235;1" values="0;0;1;1"/>
<circle class="n-ok" cx="320" cy="113" r="6"/>
<text class="t-mut t-ok" x="320" y="84">renewed</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.37;0.4;1" values="0;0;1;1"/>
<circle class="n-err" cx="460" cy="113" r="6"/>
<text class="t-mut t-err" x="460" y="84">renew fails, lease kept</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.535;0.565;1" values="0;0;1;1"/>
<circle class="n-ok" cx="600" cy="113" r="6"/>
<text class="t-mut t-ok" x="600" y="84">renewed</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.7;0.73;1" values="0;0;1;1"/>
<circle class="n-ok" cx="740" cy="113" r="6"/>
<text class="t-mut t-ok" x="740" y="84">release</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.288;0.318;1" values="0;0;1;1"/>
<circle class="n-err" cx="392" cy="198" r="6"/>
<text class="t-mut t-err" x="392" y="220">store error</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.452;0.482;1" values="0;0;1;1"/>
<text class="t-mut" x="459" y="182">retry in ttl/3</text>
<circle class="n-err" cx="526" cy="198" r="6"/>
<text class="t-mut t-err" x="526" y="220">store error</text>
</g>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.618;0.648;1" values="0;0;1;1"/>
<text class="t-mut ta-s" x="580" y="182">retry in ttl/3</text>
<circle class="n-ok" cx="660" cy="198" r="6"/>
<text class="t-mut t-ok" x="660" y="220">claimed</text>
</g>
<line class="e e-dim e-dash" x1="180" y1="60" x2="180" y2="250" opacity="0"><animate attributeName="x1" dur="10s" repeatCount="indefinite" keyTimes="0;0.04;0.7;1" values="180;180;740;740"/><animate attributeName="x2" dur="10s" repeatCount="indefinite" keyTimes="0;0.04;0.7;1" values="180;180;740;740"/><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.03;0.04;0.7;0.72;1" values="0;0;1;1;0;0"/></line>
<g><animate attributeName="opacity" dur="10s" repeatCount="indefinite" keyTimes="0;0.74;0.8;1" values="0;0;1;1"/>
<rect class="n-hot" x="16" y="284" width="748" height="36" rx="10"/>
<text class="t-strong" x="390" y="302">A run survives an outage shorter than its lease; claims retry until the store answers</text>
</g>
</g>
</svg>
</div>
</div>
<p>
A crash blocks a flow for at most one TTL. Pick a TTL several times longer than the worst store latency
(including any configured store <code>with_operation_timeout</code> and <code>with_retry</code> budget),
because an instance that cannot reach the store for a whole TTL cancels its run.
</p>

<h2 id="restarts"><a href="#restarts" class="anchor-link" aria-hidden="true">#</a>Restarts</h2>
<p>
The record holds <code>run_count</code>, <code>last_run</code>, <code>last_tick</code>, the failure streak
and the backoff window, so they outlive any instance. A restarted cluster does not run an
<code>every</code> flow again until <code>last_run + interval</code> has passed, and a flow that was
never written runs at once. A <code>Tripped</code> flow stays tripped until
<code>reset_flow(id).await</code>, which clears the streak in the shared record and so applies to every
instance. <code>reset_flow</code> also clears an expired lease (<code>expires_at &lt;= now</code>) while
leaving any live lease in flight intact.
</p>

<h2 id="status"><a href="#status" class="anchor-link" aria-hidden="true">#</a>Status and cancelling</h2>
<p>
A flow whose live lease another instance holds reports
<code>Status::RunningElsewhere { owner, lease_expires }</code>. Once <code>lease_expires</code> has passed,
<code>status()</code> and <code>list()</code> report the stored state instead (<code>Idle</code>,
<code>Completed</code>, <code>Backoff</code> or <code>Tripped</code>), so a crashed owner does not show as
running forever. <code>status()</code> mirrors the store as of this instance's last read. An
<code>every</code> or <code>cron</code> flow refreshes it at <code>start</code>, at every dispatch attempt,
and on <code>trigger</code> and <code>reset_flow</code>; a <code>manual</code> flow has no loop, so only
<code>start</code>, <code>trigger</code> and <code>reset_flow</code> refresh it.
</p>
<p>
<code>cancel_flow(id)</code> only cancels a run on the instance you call it on. On an instance that shows
<code>RunningElsewhere</code> it does nothing, so call it on the instance that owns the lease.
<code>RunningScheduler::instance_id()</code> returns this instance's id.
</p>

```rust
match node.status("fetch").map(|info| info.status) {
    // Reported while a peer holds an unexpired lease on the flow.
    Some(Status::RunningElsewhere { owner, lease_expires }) => {
        println!("fetch runs on {owner} until {lease_expires}");
    }
    // Covers Idle, Running, Completed, Backoff, Tripped, and future #[non_exhaustive] variants.
    Some(other) => println!("fetch: {other:?}"),
    None => println!("unknown flow"),
}
```

<h2 id="requirements"><a href="#requirements" class="anchor-link" aria-hidden="true">#</a>Clocks, ids and clusters</h2>
<ul>
<li>Lease expiry compares wall clocks across instances: a peer treats a lease as expired once its own clock
passes <code>expires_at</code>. The holder measures its lease on a monotonic clock. Keep the clocks
synchronised, with a skew well below the TTL.</li>
<li>Give every instance a distinct <code>with_instance_id</code>. The session token drawn at each
<code>start</code> keeps two schedulers that share an id from taking each other's leases, but a distinct,
stable name such as a pod or host name keeps logs and <code>Status::RunningElsewhere { owner, .. }</code>
unambiguous.</li>
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
<dd>Coordinates processes that reach one JetStream. One key per flow, versioned by the key's revision. <code>scheduler_kv()</code> returns the bucket handle. Non-zero <code>max_age</code> in bucket configs is rejected with <code>CanoError::Configuration</code> so <code>Backoff</code>, <code>Tripped</code> or checkpoint state cannot silently expire.</dd>
</dl>
<p>
The <a href="../../recovery/checkpoint-stores/#postgres">Postgres</a> and <a href="../../recovery/checkpoint-stores/#nats">NATS</a> sections
of the checkpoint stores guide list the other builder settings (including <code>with_operation_timeout</code> and
jittered <code>with_retry</code>), which apply to the scheduler records too.
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
<li>A written record gets a version different from every earlier version of that id (even after <code>remove</code> and recreate).</li>
<li>A store that retries a swap whose acknowledgement was lost must report <code>Written</code> when the
stored value equals the value it sent. The scheduler's values carry <code>written_by</code>, <code>written_session</code>,
<code>written_at</code> and <code>write_seq</code>, so equal bytes mean the earlier attempt landed.</li>
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
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MapStore {
    // Store-wide monotonic version counter and per-flow records.
    inner: Mutex<(u64, HashMap<String, VersionedRecord>)>,
}

#[scheduler_store]
impl SchedulerStore for MapStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| CanoError::scheduler_store(e.to_string()))?;
        Ok(guard.1.get(flow_id).cloned())
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| CanoError::scheduler_store(e.to_string()))?;
        let (last_version, records) = &mut *guard;

        // Enforce create-if-absent (expected == None) or versioned swap (Some(v)).
        if records.get(flow_id).map(|r| r.version) != expected {
            return Ok(CasOutcome::Conflict);
        }

        // Monotonic store-wide counter ensures a recreated flow never reuses a removed version.
        *last_version = last_version
            .checked_add(1)
            .ok_or_else(|| CanoError::scheduler_store("version counter overflow"))?;
        let version = *last_version;
        records.insert(flow_id.to_string(), VersionedRecord { version, value });
        Ok(CasOutcome::Written(version))
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| CanoError::scheduler_store(e.to_string()))?;
        guard.1.remove(flow_id);
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
    // Writes only under the "contract-test/<case>" prefix and cleans them up before and after.
    assert_scheduler_store_contract(&store, "contract-test").await;
}
```
</div>
