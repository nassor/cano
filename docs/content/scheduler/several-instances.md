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
<dd>Runs fixed-rate across the cluster, anchored at <code>last_run + interval</code> (stamped when the lease is claimed). A tick is due when one interval has passed since the last claimed run started and no backoff window is open. When a run takes longer than <code>interval</code>, the next attempt dispatches after the 10 ms minimum poll floor.</dd>

<dt><code>cron</code> <small>expression</small></dt>
<dd>Runs once per firing in UTC. The first instance to claim a firing records it, and every other instance skips that firing. Missed firings during an outage are skipped without a catch-up flood, and a firing inside a backoff window is skipped. A transient store error on a due tick retries every <code>(ttl / 3).clamp(50ms, 5s)</code> while still strictly before the next firing.</dd>

<dt><code>manual</code> <small><code>trigger(id).await</code></small></dt>
<dd>Claims the flow in the store before it spawns the run. The call returns an error while another instance holds the flow's lease, and the message names that instance. A trigger ignores a backoff window and is refused on a tripped flow, as on a single instance. The claim counts as a run in the record.</dd>
</dl>

<h2 id="leases"><a href="#leases" class="anchor-link" aria-hidden="true">#</a>Leases and failures</h2>
<p>
Claiming a run writes a fenced lease into the flow's record: the instance id, a per-process random
<code>session</code>, a monotonic per-claim <code>lease.id</code>, and an expiry one TTL ahead. The
running instance renews the lease every third of the TTL. When the run ends, an optimistic 1-RTT
<code>compare_and_swap</code> from the held version commits the outcome and clears the lease, falling back
to reload-and-verify only on a conflict.
</p>
<div class="diagram-frame">
<p class="diagram-label">Distributed dispatch and store lease lifecycle</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 492" role="img" aria-labelledby="sched-lease-title sched-lease-desc">
<title id="sched-lease-title">Distributed dispatch and store lease lifecycle</title>
<desc id="sched-lease-desc">Three lifelines: Trigger and Loop, Workflow Runner, and SchedulerStore. A due tick or trigger sends a request to Workflow Runner, which loads v1 and sends a fenced compare-and-swap claim carrying owner, session, lease_id and TTL, receiving Written v2. While Workflow::run_once executes concurrently, keep_alive sends renew compare-and-swap heartbeats every ttl divided by 3, advancing the version from v2 to v3 to v4. When run_once completes, an optimistic 1-RTT release compare-and-swap from v4 commits the outcome at v5 and clears the lease.</desc>
<defs><marker id="sched-lease-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="125" y1="52" x2="125" y2="434"/>
<line class="lifeline" x1="390" y1="52" x2="390" y2="434"/>
<line class="lifeline" x1="655" y1="52" x2="655" y2="434"/>
<rect class="n" x="25" y="10" width="200" height="42" rx="8"/>
<text class="t-strong" x="125" y="25">Trigger &amp; Loop</text>
<text class="t-mut t-code" x="125" y="42">every &middot; cron &middot; trigger()</text>
<rect class="n-hot" x="285" y="10" width="210" height="42" rx="8"/>
<text class="t-strong" x="390" y="25">Workflow Runner</text>
<text class="t-mut t-code" x="390" y="42">Coordinator + run_reserved</text>
<rect class="n-cop" x="555" y="10" width="200" height="42" rx="8"/>
<text class="t-strong" x="655" y="25">SchedulerStore</text>
<text class="t-mut t-code" x="655" y="42">CAS &middot; FlowRecord</text>
<path class="e" d="M125,86 H386" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-code" x="255" y="73">due tick / trigger()</text>
<path class="e" d="M390,124 H651" marker-end="url(#sched-lease-ah)"/>
<text class="t-mut t-code" x="522" y="111">load(flow_id) -&gt; v1</text>
<path class="e e-hot" d="M390,162 H651" marker-end="url(#sched-lease-ah)"/>
<text class="t-code" x="522" y="149">CAS(v1: owner, session, id, +TTL)</text>
<g class="cd-loop"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.9368;0.9895;1" values="1;1;0;0"/>
<path class="e e-dash" d="M655,196 H394" marker-end="url(#sched-lease-ah)"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.2526;0.2947;1" values="0;0;1;1"/></path>
<text class="t-mut t-ok t-code" x="522" y="183">Written(v2) &middot; HeldLease<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.2526;0.2947;1" values="0;0;1;1"/></text>
<rect class="band-hot" x="218" y="222" width="158" height="106" rx="8"><animate attributeName="height" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3053;0.6421;1" values="0;0;106;106"/></rect>
<text class="t-strong" x="297" y="266">Workflow::run_once<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3158;0.3579;1" values="0;0;1;1"/></text>
<text class="t-mut" x="297" y="286">running under lease<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3158;0.3579;1" values="0;0;1;1"/></text>
<path class="e" d="M390,248 H651" marker-end="url(#sched-lease-ah)"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3368;0.3684;1" values="0;0;1;1"/></path>
<text class="t-mut t-code" x="522" y="235">ttl / 3: renew CAS(v2 -&gt; v3)<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3368;0.3684;1" values="0;0;1;1"/></text>
<text class="t-mut t-ok t-code" x="522" y="264">v3 (+TTL)<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.4105;0.4526;1" values="0;0;1;1"/></text>
<path class="e" d="M390,298 H651" marker-end="url(#sched-lease-ah)"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.4842;0.5158;1" values="0;0;1;1"/></path>
<text class="t-mut t-code" x="522" y="285">ttl / 3: renew CAS(v3 -&gt; v4)<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.4842;0.5158;1" values="0;0;1;1"/></text>
<text class="t-mut t-ok t-code" x="522" y="314">v4 (+TTL)<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.5579;0.6;1" values="0;0;1;1"/></text>
<path class="e e-hot" d="M390,364 H651" marker-end="url(#sched-lease-ah)"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.6421;0.6737;1" values="0;0;1;1"/></path>
<text class="t-code" x="522" y="351">1-RTT release CAS(v4: Completed)<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.6421;0.6737;1" values="0;0;1;1"/></text>
<path class="e e-dash" d="M655,402 H394" marker-end="url(#sched-lease-ah)"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.7158;0.7684;1" values="0;0;1;1"/></path>
<text class="t-mut t-ok t-code" x="522" y="389">Written(v5) &middot; schedule advances<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.7158;0.7684;1" values="0;0;1;1"/></text>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.5s" repeatCount="indefinite" path="M125,86 H386" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.0421;0.1158;1"/><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.0316;0.0421;0.1158;0.1263;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.5s" repeatCount="indefinite" path="M390,124 H651" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.1263;0.1895;1"/><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.1158;0.1263;0.1895;0.2;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.5s" repeatCount="indefinite" path="M390,162 H651" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.2;0.2632;1"/><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.1895;0.2;0.2632;0.2737;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.5s" repeatCount="indefinite" path="M390,248 H651" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.3474;0.4211;1"/><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3368;0.3474;0.4211;0.4316;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.5s" repeatCount="indefinite" path="M390,298 H651" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.4947;0.5684;1"/><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.4842;0.4947;0.5684;0.5789;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="9.5s" repeatCount="indefinite" path="M390,364 H651" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.6526;0.7263;1"/><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.6421;0.6526;0.7263;0.7368;1" values="0;0;1;1;0;0"/></circle>
<rect class="n-ok" x="25" y="446" width="730" height="34" rx="10"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.7684;0.8211;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="390" y="463">Fenced lease renewed at ttl/3; optimistic 1-RTT release commits v5 from HeldLease<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.7684;0.8211;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>
<dl class="key-list">
<dt>Full fencing <small><code>version</code> + <code>owner</code>, <code>session</code>, <code>lease.id</code>, <code>written_session</code>, <code>write_seq</code></small></dt>
<dd>Every mutating swap carries <code>expected: Some(version)</code> and verifies <code>(owner, session, lease.id)</code>. A per-process random <code>session</code> ensures a process restarted quickly with the same static <code>with_instance_id</code> does not treat its pre-crash lease as its own. A monotonic per-claim <code>lease.id</code> ensures a thawed zombie or dropped claim cleanup can never renew or release a newer run on the same instance. Each stored <code>FlowRecord</code> also stamps <code>written_by</code>, <code>written_session</code>, <code>written_at</code> and <code>write_seq</code> so two writes never produce identical bytes across processes or within one clock tick.</dd>

<dt>Lease lost and crash failover <small>monotonic holder TTL + zombie fencing</small></dt>
<dd>The lease holder tracks its remaining TTL monotonically from <code>swap_started + ttl</code> via <code>tokio::time::Instant</code>, bounded by <code>wall_remaining</code>, so a backward NTP step cannot stretch its local deadline. When renewals are blocked for a full TTL or a renewal hits its deadline, <code>keep_alive</code> returns <code>Renew::Lost(None)</code>, fires <code>run_token</code>, and drains any saga compensation stack without discarding the running future. On a peer instance, once <code>expires_at &lt;= now</code>, <code>claim</code> records the failure anchored at <code>lease.expires_at</code> (preventing a later wall-clock tick on sparse schedules such as daily crons from opening a fresh backoff window and skipping the next firing) and takes over at a new record version. A thawed zombie attempting a stale <code>CAS(exp=old_version)</code> hits <code>Conflict</code>, reads <code>NotHeld</code>, and discards its outcome.</dd>

<dt>Lost-ACK claim rollback <small><code>abort_claim</code> + pre-claim counters</small></dt>
<dd>Before bumping <code>run_count</code>, <code>last_run</code> or <code>last_tick</code>, <code>claim</code> saves <code>prev_run_count</code>, <code>prev_last_run</code> and <code>prev_last_tick</code> on the lease. If a claim swap drops its ACK, times out, or is cancelled before execution starts (including dropping a <code>trigger()</code> future mid-claim), <code>abort_claim</code> restores those pre-claim counters, marks a deliberate cancellation instead of a failure, and clears the lease. If <code>abort_claim</code> cannot reach the store, the coordinator keeps the unstarted <code>(flow_id, lease_id)</code> in a bounded set and restores the counters on its next successful read. When a retried <code>claim</code> encounters its own <code>(owner, session, lease.id)</code> after a lost ACK and concurrent <code>reset_flow</code>, it resumes ownership instead of returning <code>NotDue</code>.</dd>

<dt>Store outage and bounded calls <small><code>guarded(ttl)</code>, <code>last_error</code> cap, backoff self-healing</small></dt>
<dd>Every <code>load</code> and <code>compare_and_swap</code> call runs inside <code>guarded(ttl)</code>, which catches custom store panics and caps each call (as well as overall <code>claim</code> and <code>reset</code> CAS loops) by <code>ttl</code>. While <code>remaining &gt; 0</code>, <code>keep_alive</code> tolerates transient renewal errors (<code>Renew::Held</code>) and self-heals on the next <code>ttl / 3</code> heartbeat without dropping the active run. On a claim outage, <code>every</code> loops back off to <code>interval.min((ttl / 3).max(MIN_POLL))</code> and <code>cron</code> loops retry due ticks every <code>(ttl / 3).clamp(50ms, 5s)</code> while before the next firing. Error strings in <code>FlowRecord::last_error</code> are UTF-8-safely truncated to 4 KiB (<code>4096</code> bytes with <code>...</code>) so oversized errors cannot exceed backend payload limits.</dd>

<dt>Shutdown abort <small><code>stop()</code> aborts a run after the cancel timeout</small></dt>
<dd>The aborted run leaves its lease to expire and is counted as one failure anchored at <code>lease.expires_at</code>, the same as a crash. A run that finishes during the drain or cancel phase releases its lease in 1 RTT as usual.</dd>
</dl>
<div class="diagram-frame">
<p class="diagram-label">Crash or partition failover and zombie fencing</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 560" role="img" aria-labelledby="sched-failover-title sched-failover-desc">
<title id="sched-failover-title">Crash or partition failover and zombie fencing</title>
<desc id="sched-failover-desc">Three lifelines: Instance A (s1), SchedulerStore, and Instance B (s2). Instance A claims v2 and runs while a partition stalls renewals past TTL. Once the monotonic deadline swap_started plus ttl elapses, Instance A cancels its run token and drains any saga compensations. Meanwhile Instance B observes RunningElsewhere until expires_at passes, then records a failure anchored at lease.expires_at and claims v3. When thawed zombie A sends a stale release or renew CAS expecting v2, the store returns Conflict; A sees owner B (NotHeld) and discards its stale outcome.</desc>
<defs><marker id="sched-failover-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="125" y1="52" x2="125" y2="498"/>
<line class="lifeline" x1="390" y1="52" x2="390" y2="498"/>
<line class="lifeline" x1="655" y1="52" x2="655" y2="498"/>
<rect class="n" x="25" y="10" width="200" height="42" rx="8"/>
<text class="t-strong" x="125" y="25">Instance A (s1)</text>
<text class="t-mut t-code" x="125" y="42">initial lease holder</text>
<rect class="n-cop" x="290" y="10" width="200" height="42" rx="8"/>
<text class="t-strong" x="390" y="25">SchedulerStore</text>
<text class="t-mut t-code" x="390" y="42">fenced CAS state</text>
<rect class="n" x="555" y="10" width="200" height="42" rx="8"/>
<text class="t-strong" x="655" y="25">Instance B (s2)</text>
<text class="t-mut t-code" x="655" y="42">failover peer</text>
<g class="cd-loop"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.9429;0.9905;1" values="1;1;0;0"/>
<path class="e e-hot" d="M125,82 H386" marker-end="url(#sched-failover-ah)"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.0381;0.0762;1" values="0;0;1;1"/></path>
<text class="t-code" x="255" y="69">CAS(v1-&gt;v2): owner=A, s1, id=1<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.0381;0.0762;1" values="0;0;1;1"/></text>
<rect class="band-hot" x="16" y="96" width="96" height="88" rx="6"><animate attributeName="height" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.1143;0.3619;1" values="0;0;88;88"/></rect>
<text class="t-strong" x="64" y="130">running<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.1238;0.1619;1" values="0;0;1;1"/></text>
<text class="t-mut t-code" x="64" y="148">v2 held<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.1238;0.1619;1" values="0;0;1;1"/></text>
<rect class="n-warn" x="150" y="106" width="216" height="60" rx="8"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.1905;0.2381;1" values="0;0;1;1"/></rect>
<text class="t-strong t-warn" x="258" y="126">partition / stall &gt; TTL<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.1905;0.2381;1" values="0;0;1;1"/></text>
<text class="t-mut" x="258" y="146">renew(v2) blocked / timed out<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.1905;0.2381;1" values="0;0;1;1"/></text>
<path class="e e-dash" d="M655,138 H394" marker-end="url(#sched-failover-ah)"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.219;0.2667;1" values="0;0;1;1"/></path>
<text class="t-mut t-code" x="524" y="125">load -&gt; RunningElsewhere(A)<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.219;0.2667;1" values="0;0;1;1"/></text>
<rect class="n-warn" x="16" y="192" width="218" height="54" rx="8"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.3619;0.419;1" values="0;0;1;1"/></rect>
<text class="t-code t-warn" x="125" y="210">swap_started + ttl elapsed<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.3619;0.419;1" values="0;0;1;1"/></text>
<text class="t-mut" x="125" y="229">cancel run_token + drain saga<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.3619;0.419;1" values="0;0;1;1"/></text>
<path class="e e-hot" d="M655,282 H394" marker-end="url(#sched-failover-ah)"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.4571;0.4952;1" values="0;0;1;1"/></path>
<text class="t-code" x="524" y="269">expired: fail at lease.expires_at<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.4571;0.4952;1" values="0;0;1;1"/></text>
<path class="e e-dash" d="M390,318 H651" marker-end="url(#sched-failover-ah)"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.5333;0.581;1" values="0;0;1;1"/></path>
<text class="t-mut t-ok t-code" x="524" y="305">Written(v3) &middot; owner=B, s2<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.5333;0.581;1" values="0;0;1;1"/></text>
<rect class="band-hot" x="668" y="324" width="98" height="70" rx="6"><animate attributeName="height" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.5714;0.7143;1" values="0;0;70;70"/></rect>
<text class="t-strong" x="717" y="359">B runs v3<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.5905;0.6286;1" values="0;0;1;1"/></text>
<path class="e e-dash" d="M125,366 H386" marker-end="url(#sched-failover-ah)"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.6286;0.6667;1" values="0;0;1;1"/></path>
<text class="t-err t-code" x="255" y="353">zombie CAS(exp=v2, outcome)<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.6286;0.6667;1" values="0;0;1;1"/></text>
<path class="e e-dash" d="M390,402 H129" marker-end="url(#sched-failover-ah)"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.7048;0.7429;1" values="0;0;1;1"/></path>
<text class="t-err t-code" x="255" y="389">Conflict (stored v3 != v2)<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.7048;0.7429;1" values="0;0;1;1"/></text>
<rect class="n-err" x="20" y="438" width="210" height="46" rx="8"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.7429;0.7905;1" values="0;0;1;1"/></rect>
<text class="t-code t-err" x="125" y="453">load -&gt; owner=B (NotHeld)<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.7429;0.7905;1" values="0;0;1;1"/></text>
<text class="t-mut" x="125" y="470">stale outcome discarded<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.7429;0.7905;1" values="0;0;1;1"/></text>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.5s" repeatCount="indefinite" path="M125,82 H386" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.0381;0.1048;1"/><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.0286;0.0381;0.1048;0.1143;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.5s" repeatCount="indefinite" path="M655,282 H394" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.4571;0.5238;1"/><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.4476;0.4571;0.5238;0.5333;1" values="0;0;1;1;0;0"/></circle>
<circle class="token" r="5" cx="0" cy="0" opacity="0"><animateMotion dur="10.5s" repeatCount="indefinite" path="M125,366 H386" calcMode="linear" keyPoints="0;0;1;1" keyTimes="0;0.6286;0.6952;1"/><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.619;0.6286;0.6952;0.7048;1" values="0;0;1;1;0;0"/></circle>
<rect class="n-hot" x="20" y="508" width="740" height="36" rx="10"><animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.7905;0.8381;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="390" y="526">Monotonic TTL bounds the holder; CAS version + (owner, session, lease_id) fence zombies<animate attributeName="opacity" dur="10.5s" repeatCount="indefinite" keyTimes="0;0.7905;0.8381;1" values="0;0;1;1"/></text>
</g>
</svg>
</div>
</div>
<div class="diagram-frame">
<p class="diagram-label">Store-outage tolerance and backoff self-healing</p>
<div class="cd-wrap">
<svg class="cd cd-anim" viewBox="0 0 780 340" role="img" aria-labelledby="sched-outage-title sched-outage-desc">
<title id="sched-outage-title">Store-outage tolerance and backoff self-healing</title>
<desc id="sched-outage-desc">Two-lane timeline across a transient store outage shorter than the lease TTL. In Lane 1, keep_alive tolerates transient renewal errors while remaining time stays positive, returning Renew::Held so the active run continues and self-heals on the next ttl divided by 3 heartbeat before committing release. In Lane 2, every and cron claim outages back off cleanly (using min of interval and ttl divided by 3, or retrying due cron ticks while before the next firing, with abort_claim restoring last_tick and run_count on a lost ACK) and win claims cleanly once the store recovers.</desc>
<defs><marker id="sched-outage-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<rect class="n-warn" x="320" y="34" width="170" height="212" rx="8"/>
<text class="t-warn t-strong" x="405" y="50">outage (&lt; TTL)</text>
<text class="t-strong ta-e" x="140" y="104">keep_alive</text>
<text class="t-mut ta-e" x="140" y="122">&lt; TTL outage</text>
<text class="t-strong ta-e" x="140" y="188">claim &amp; loop</text>
<text class="t-mut ta-e" x="140" y="206">every / cron</text>
<line class="axis" x1="150" y1="252" x2="732" y2="252"/>
<path class="e e-dim" d="M732,252 H750" marker-end="url(#sched-outage-ah)"/>
<text class="t-mut ta-e" x="748" y="268">time</text>
<g class="cd-loop"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.9368;0.9895;1" values="1;1;0;0"/>
<rect class="band-hot" x="150" y="94" width="560" height="34" rx="6"><animate attributeName="width" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.0421;0.7158;1" values="0;0;560;560"/></rect>
<text class="t-mut t-ok t-code" x="220" y="80">ttl/3: renew Ok<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.1263;0.1684;1" values="0;0;1;1"/></text>
<text class="t-mut t-warn t-code" x="405" y="80">err -&gt; Renew::Held<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.2737;0.3158;1" values="0;0;1;1"/></text>
<text class="t-strong" x="430" y="111">active run preserved<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.2947;0.3368;1" values="0;0;1;1"/></text>
<text class="t-mut t-ok t-code" x="575" y="80">self-healed: Ok<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.5263;0.5684;1" values="0;0;1;1"/></text>
<text class="t-mut t-ok t-code" x="705" y="80">release Ok<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.6947;0.7368;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="150" y="178" width="70" height="30" rx="6"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.0421;0.1263;1" values="0;0;1;1"/></rect>
<text class="t-mut t-ok" x="185" y="193">ok<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.0842;0.1263;1" values="0;0;1;1"/></text>
<rect class="n-err" x="320" y="178" width="60" height="30" rx="6"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.2632;0.3053;1" values="0;0;1;1"/></rect>
<text class="t-mut t-err" x="350" y="193">lost-ACK<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.2632;0.3053;1" values="0;0;1;1"/></text>
<text class="t-mut t-code" x="405" y="226">abort_claim restores last_tick/run_count<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.2842;0.3263;1" values="0;0;1;1"/></text>
<rect class="band" x="386" y="178" width="104" height="30" rx="6"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3368;0.3895;1" values="0;0;1;1"/></rect>
<text class="t-mut" x="438" y="193">backoff wait<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3368;0.3895;1" values="0;0;1;1"/></text>
<text class="t-mut t-code" x="438" y="164">min(interval, ttl/3) / cron retry<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.3368;0.3895;1" values="0;0;1;1"/></text>
<rect class="n-ok" x="504" y="178" width="122" height="30" rx="6"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.5474;0.6105;1" values="0;0;1;1"/></rect>
<text class="t-mut t-ok" x="565" y="193">healed: claim<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.5474;0.6105;1" values="0;0;1;1"/></text>
<line class="e e-dim e-dash" x1="150" y1="64" x2="150" y2="252" opacity="0"><animate attributeName="x1" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.0421;0.7158;1" values="150;150;710;710"/><animate attributeName="x2" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.0421;0.7158;1" values="150;150;710;710"/><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.0316;0.0421;0.7158;0.7368;1" values="0;0;1;1;0;0"/></line>
<rect class="n-hot" x="16" y="286" width="748" height="36" rx="10"><animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.7474;0.8;1" values="0;0;1;1"/></rect>
<text class="t-strong" x="390" y="304">Outages &lt; TTL: active runs self-heal at ttl/3; loops back off and preserve counters<animate attributeName="opacity" dur="9.5s" repeatCount="indefinite" keyTimes="0;0.7474;0.8;1" values="0;0;1;1"/></text>
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
<code>Status::RunningElsewhere { owner, lease_expires }</code>. Once <code>lease_expires &lt;= now</code>,
<code>status()</code> and <code>list()</code> dynamically normalize the snapshot back to the underlying
<code>FlowRecord</code> state (<code>Idle</code>, <code>Completed</code>, <code>Backoff</code>, or
<code>Tripped</code>) instead of reporting an expired <code>RunningElsewhere</code> indefinitely.
<code>status()</code> mirrors the store as of this instance's last read: <code>every</code> and
<code>cron</code> flows refresh on <code>start</code>, every dispatch attempt, <code>trigger</code> and
<code>reset_flow</code>; <code>manual</code>-only flows have no background loop, so their stored snapshot
refreshes on <code>start</code>, <code>trigger</code> and <code>reset_flow</code>.
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
<li>Cross-host lease expiry (<code>expires_at &lt;= now</code>) compares UTC wall clocks across instances, even though the holder bounds its own TTL monotonically via <code>Instant</code>. Keep host clocks synchronized with NTP skew well below <code>ttl / 3</code>.</li>
<li>Every instance needs a distinct <code>with_instance_id</code>. Two concurrent processes with one id treat each other's lease as their own. A stable name such as a pod or host name keeps logs and statuses readable.</li>
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
The <a href="../../recovery/#postgres">Postgres</a> and <a href="../../recovery/#nats">NATS</a> sections
of the recovery guide list the other builder settings (including <code>with_operation_timeout</code> and
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
