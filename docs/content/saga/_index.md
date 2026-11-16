+++
title = "Sagas and compensation"
description = "Pair a Cano workflow step with a compensating action; if a later step fails, roll back the work already done."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Sagas and compensation</h1>
<p class="subtitle">Undo successful steps in reverse when a later step fails.</p>

<p>
Once a workflow step has charged a card or reserved inventory, the FSM cannot roll it back by itself.
Only an explicit refund or release can. The saga pattern pairs each irreversible forward step with a
compensating action and replays those compensations in reverse order when a later step fails.
</p>
<p>
In Cano you implement <code>CompensatableTask</code> for a state's task and register it with
<code>Workflow::register</code>. Its <code>run</code> returns the next state and an
<code>Output</code> that describes what it did. Its <code>compensate</code> takes that
<code>Output</code> back and undoes it. The engine keeps a per-run compensation stack, and when a later
state fails it drains the stack last in, first out (LIFO) and runs each <code>compensate</code>. With a
<a href="../recovery/">checkpoint store</a> attached the outputs are persisted, so a resumed run can
still compensate work done by an earlier process.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#writing">Writing a compensatable task</a></li>
<li><a href="#registering">Registering it</a></li>
<li><a href="#stack">The compensation stack</a></li>
<li><a href="#result">What <code>orchestrate</code> returns</a></li>
<li><a href="#idempotency">The idempotency contract</a></li>
<li><a href="#recovery">Composing with recovery</a></li>
<li><a href="#observers">Observer events</a></li>
<li><a href="#full-example">Full example</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="writing"><a href="#writing" class="anchor-link" aria-hidden="true">#</a>Writing a compensatable task</h2>
<p>
<code>CompensatableTask</code> is a standalone trait, not an extension of <a href="../task/">Task</a>:
its <code>run</code> returns <code>(next_state, Output)</code>, and <code>Task::run</code> has no slot
for the output. Put <code>#[saga::task(state = ...)]</code> on an inherent <code>impl</code> block and
the macro writes the <code>impl CompensatableTask&lt;...&gt; for ...</code> header. You write
<code>type Output</code>, <code>run</code> and <code>compensate</code>, plus <code>config</code> and
<code>name</code> if you want them. <code>Output</code> must be <code>serde</code>-serializable. It is
the only value carried from <code>run</code> to <code>compensate</code>, and the two may run in
different processes after a resume, so make it self-contained.
</p>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Reserve, Ship, Done }

#[derive(Serialize, Deserialize)]
struct Reservation { sku: String, qty: u32 }

struct ReserveInventory;

#[saga::task(state = Step)]
impl ReserveInventory {
    type Output = Reservation;

    async fn run(&self, _res: &Resources) -> Result<(Step, Reservation), CanoError> {
        // ... actually reserve the stock ...
        Ok((Step::Ship, Reservation { sku: "WIDGET-7".into(), qty: 3 }))
    }

    async fn compensate(&self, _res: &Resources, output: Reservation) -> Result<(), CanoError> {
        // ... release `output.qty` of `output.sku`; must be idempotent ...
        let _ = output;
        Ok(())
    }
}
```

<p>
A bare <code>#[saga::task]</code> on a hand-written
<code>impl CompensatableTask&lt;Step&gt; for ReserveInventory { ... }</code> works too, for example for
a generic impl. <code>run</code> and <code>compensate</code> may omit the <code>res</code> parameter
when they do not read resources.
</p>

<p>
<code>config()</code> and <code>name()</code> have the same defaults as <code>Task</code>:
<code>TaskConfig::new()</code> (one attempt, no retries) and the type name. Override
<code>config()</code> to add a retry or timeout policy for the forward run, for example
<code>TaskConfig::new().with_exponential_retry(3)</code>. Override <code>name()</code> to give the task
a stable id. The name is the compensation-stack key, so two compensatable states that share a name make
<code>validate()</code> return a configuration error; override <code>name()</code> on one of them.
<code>Output</code> can be <code>()</code> when a task needs no data to undo itself.
</p>

<div class="callout callout-warning">
<span class="callout-label">Commit the side effect last</span>
<p>
Do not commit a side effect before <code>run</code> returns <code>Ok((next_state, output))</code>. A
panic, or a <code>?</code> that propagates after the commit, leaves the engine with no stack entry, so
a later failure cannot roll the side effect back. Prepare and validate first, then commit immediately
before the final <code>Ok</code>.
</p>
</div>
<hr class="section-divider">

<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering it</h2>
<p>
<code>Workflow::register(state, task)</code> registers a compensatable task, the same call every task
model uses. Compensation covers single-task states only. A split state contributes nothing to the
stack.
</p>

```rust
use cano::prelude::*;

// ReserveInventory, ChargeCard and ShipOrder each implement CompensatableTask<Step>.
let workflow = Workflow::bare()
    .register(Step::Reserve, ReserveInventory)
    .register(Step::Charge, ChargeCard)
    .register(Step::Ship, ShipOrder)
    .add_exit_state(Step::Done);
```

<p>
Compensatable and plain tasks mix freely, and only the compensatable ones add to the stack. A plain
task that fails still triggers the rollback of every compensatable step that ran before it.
</p>
<hr class="section-divider">

<h2 id="stack"><a href="#stack" class="anchor-link" aria-hidden="true">#</a>The compensation stack</h2>

<div class="diagram-frame">
<p class="diagram-label">The forward run pushes entries; a terminal failure drains the stack LIFO</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 24 780 300" role="img" aria-labelledby="saga-stack-title saga-stack-desc">
<title id="saga-stack-title">The compensation stack drained in reverse</title>
<desc id="saga-stack-desc">Reserve and Charge are compensatable tasks. Each succeeds and pushes its task name and serialized output onto the compensation stack: Reserve pushes entry 1 with its Reservation, Charge pushes entry 2 with its Charge output. Ship is a plain task and it fails. The engine drains the stack last in, first out. Charge compensate runs first, popping entry 2 to refund, then Reserve compensate runs, popping entry 1 to release the hold. Each compensate receives the output its own run produced. After a clean rollback, orchestrate returns the original error.</desc>
<defs><marker id="saga-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-hot" d="M670,98 V144 Q670,152 662,152 H438 Q430,152 430,160 V190" marker-end="url(#saga-ah)"/>
<text class="t-mut t-err" x="550" y="126">terminal failure</text>
<text class="t-strong t-hot" x="550" y="143">drain stack LIFO</text>
<path class="e e-dash" d="M70,98 V190" marker-end="url(#saga-ah)"/>
<text class="t-code t-mut ta-s" x="80" y="132">Reservation</text>
<path class="e e-dash" d="M350,98 V190" marker-end="url(#saga-ah)"/>
<text class="t-code t-mut ta-s" x="360" y="132">Charge</text>
<rect class="n" x="25" y="40" width="170" height="58" rx="10"/>
<text class="t-strong" x="110" y="65">Reserve</text>
<text class="t-mut" x="110" y="85">push 1: Reservation</text>
<rect class="n" x="305" y="40" width="170" height="58" rx="10"/>
<text class="t-strong" x="390" y="65">Charge</text>
<text class="t-mut" x="390" y="85">push 2: Charge</text>
<rect class="n-err" x="585" y="40" width="170" height="58" rx="10"/>
<text class="t-strong" x="670" y="65">Ship</text>
<text class="t-mut" x="670" y="85">plain task, fails</text>
<path class="e" d="M199,69 H301" marker-end="url(#saga-ah)"/>
<text class="t-mut t-ok" x="250" y="57">success</text>
<path class="e" d="M479,69 H581" marker-end="url(#saga-ah)"/>
<text class="t-mut t-ok" x="530" y="57">success</text>
<rect class="n" x="25" y="196" width="170" height="58" rx="10"/>
<text class="t-code" x="110" y="221">compensate()</text>
<text class="t-mut" x="110" y="241">pop 1: release hold</text>
<rect class="n" x="305" y="196" width="170" height="58" rx="10"/>
<text class="t-code" x="390" y="221">compensate()</text>
<text class="t-mut" x="390" y="241">pop 2: refund</text>
<path class="e e-hot" d="M301,225 H199" marker-end="url(#saga-ah)"/>
<text class="t-mut" x="250" y="213">next entry</text>
<rect class="n-hot" x="25" y="274" width="730" height="34" rx="10"/>
<text class="t-strong" x="390" y="296">After a clean rollback, orchestrate returns the original error</text>
</svg>
</div>
</div>
<ul>
<li>Each successful compensatable task pushes <code>(task name, serialized output)</code> onto the
run's stack.</li>
<li>On any terminal failure, such as a task error, a missing handler for a state, a total timeout or a
checkpoint-append failure, the engine drains the stack LIFO. For each entry it finds the compensator
by task name, deserializes the output and runs <code>compensate</code>.</li>
<li>The drain never stops early. A <code>compensate</code> error, timeout or panic is recorded and the
remaining entries are still compensated. A <code>compensate</code> is bounded by the task's
<code>attempt_timeout</code> when one is set, and is never retried.</li>
<li>A compensatable task whose forward <code>run</code> fails produced no output, so it is not on the
stack and nothing is undone for it.</li>
<li>When the workflow sets <a href="../resilience/#workflow-total-timeout"><code>Workflow::with_total_timeout</code></a>,
every drain has a deadline: half the configured total timeout, capped at 30 s, or the value set by
<code>Workflow::with_compensation_timeout</code>, which has no effect without a total timeout. Without one, only each task's <code>attempt_timeout</code> bounds a <code>compensate</code>.
A <code>compensate</code> that exceeds the deadline becomes one recorded error. The entries not yet run
are recorded as <code>OrphanedCompensation</code> errors that carry their serialized output for manual
undo.</li>
</ul>
<p>
When a run is cancelled with <code>orchestrate_with_token</code>, a compensatable task is never
interrupted mid-run. It completes, and the cancel is honoured at the next state boundary, so a
committed side effect always reaches the stack. The compensation drain itself is uncancellable. The
result is <code>CanoError::Cancelled</code> (<code>is_cancelled()</code> is true) after a clean
rollback, or <code>CompensationFailed</code> whose <code>errors[0]</code> is the cancel when a
<code>compensate</code> failed.
</p>
<hr class="section-divider">

<h2 id="result"><a href="#result" class="anchor-link" aria-hidden="true">#</a>What <code>orchestrate</code> returns</h2>
<p>A run ends one of four ways. The three failures differ in what the rollback managed to do.</p>
<div class="card-grid outcome-cards">
<div class="card outcome-ok">
<h3>Reaches an exit state</h3>
<p>The FSM ran all the way to a registered exit state.</p>
<p><strong class="outcome-key">Returns</strong><code>Ok(final_state)</code></p>
<p><strong class="outcome-key">Checkpoint log</strong><span class="outcome-pill cleared">cleared</span></p>
</div>
<div class="card outcome-warn">
<h3>Fails, nothing to compensate</h3>
<p>No compensatable step had completed yet, so the rollback stack is empty.</p>
<p><strong class="outcome-key">Returns</strong>the original error</p>
<p><strong class="outcome-key">Checkpoint log</strong><span class="outcome-pill kept">kept</span></p>
</div>
<div class="card outcome-warn">
<h3>Fails, rollback succeeds</h3>
<p>The stack drained LIFO and every <code>compensate</code> ran cleanly.</p>
<p><strong class="outcome-key">Returns</strong>the original error, unchanged</p>
<p><strong class="outcome-key">Checkpoint log</strong><span class="outcome-pill cleared">cleared</span></p>
</div>
<div class="card outcome-danger">
<h3>Fails, rollback incomplete</h3>
<p>At least one <code>compensate</code> errored, or a stack entry had no registered compensator.</p>
<p><strong class="outcome-key">Returns</strong><code>CanoError::CompensationFailed { errors }</code></p>
<p><strong class="outcome-key">Checkpoint log</strong><span class="outcome-pill kept">kept for manual recovery</span></p>
</div>
</div>
<p>
<code>CompensationFailed.errors[0]</code> is the original failure and <code>errors[1..]</code> are the
compensation errors in drain order. A stack entry whose compensator is not registered, for example
when resuming against a changed workflow definition, counts as one error. Clearing the checkpoint log
is best effort: a failed <code>clear</code> is logged and never fatal.
</p>
<hr class="section-divider">

<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The idempotency contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Make <code>compensate</code> idempotent</span>
<p><code>compensate</code> can run more than once for the same logical step. This happens most often
when a <a href="../recovery/">resume</a> re-runs a compensatable task whose result was already
recorded, or when a re-run lands between writing the task's completion checkpoint and entering the
next state. Use refunds keyed by transaction id, conditional releases, "if still reserved" checks:
anything that is safe to apply twice. The forward <code>run</code> at and after a resume point must be
idempotent as well.</p>
</div>
<hr class="section-divider">

<h2 id="recovery"><a href="#recovery" class="anchor-link" aria-hidden="true">#</a>Composing with recovery</h2>
<p>
Attach a <a href="../recovery/">checkpoint store</a> with <code>with_checkpoint_store</code> and a
workflow id with <code>with_workflow_id</code>, and the compensation stack survives a crash:
</p>

<div class="diagram-frame">
<p class="diagram-label">The compensation stack survives a crash inside the checkpoint log</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 358" role="img" aria-labelledby="saga-resume-title saga-resume-desc">
<title id="saga-resume-title">The compensation stack rebuilt after a crash</title>
<desc id="saga-resume-desc">Process 1 runs Reserve and Charge. Each success appends a completion row with its serialized output to the checkpoint store. Process 1 then crashes and its in-memory stack is lost. Process 2 calls Workflow::resume_from. The checkpoint store returns the RowKind::CompensationCompletion rows, and Process 2 rebuilds the LIFO compensation stack from them in sequence order. A failure after the resume point still rolls back what the crashed process did.</desc>
<defs><marker id="sagaresume-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<line class="lifeline" x1="110" y1="46" x2="110" y2="160"/>
<line class="lifeline" x1="390" y1="46" x2="390" y2="300"/>
<line class="lifeline" x1="670" y1="46" x2="670" y2="300"/>
<rect class="band" x="385" y="94" width="10" height="186" rx="5"/>
<rect class="n" x="20" y="8" width="180" height="38" rx="8"/>
<text class="t-strong" x="110" y="28">Process 1</text>
<rect class="n-cop" x="300" y="8" width="180" height="38" rx="8"/>
<text class="t-strong" x="390" y="28">Checkpoint store</text>
<rect class="n" x="580" y="8" width="180" height="38" rx="8"/>
<text class="t-strong" x="670" y="28">Process 2</text>
<path class="e" d="M110,94 H386" marker-end="url(#sagaresume-ah)"/>
<text class="t-mut" x="248" y="82">Reserve succeeds: append completion row</text>
<path class="e" d="M110,138 H386" marker-end="url(#sagaresume-ah)"/>
<text class="t-mut" x="248" y="126">Charge succeeds: append completion row</text>
<rect class="n-ghost" x="20" y="160" width="180" height="40" rx="10"/>
<text class="t-strong t-warn" x="110" y="178">crash</text>
<text class="t-mut t-warn" x="110" y="194">in-memory stack lost</text>
<path class="e" d="M670,228 H394" marker-end="url(#sagaresume-ah)"/>
<text class="t-code" x="532" y="216">Workflow::resume_from</text>
<path class="e e-dash" d="M396,280 H666" marker-end="url(#sagaresume-ah)"/>
<text class="t-code t-mut" x="528" y="252">RowKind::CompensationCompletion</text>
<text class="t-mut" x="528" y="268">rows in sequence order rebuild the stack</text>
<rect class="n-hot" x="20" y="312" width="740" height="34" rx="10"/>
<text class="t-strong" x="390" y="334">A failure after the resume point still rolls back what the crashed process did</text>
</svg>
</div>
</div>
<ul>
<li>A successful compensatable state writes a second, completion
<a href="../recovery/"><code>CheckpointRow</code></a> with <code>kind == RowKind::CompensationCompletion</code>
and <code>output_blob</code> set to the serialized output. The state's entry row and completion row
use two sequence numbers.</li>
<li><code>Workflow::resume_from</code> rebuilds the compensation stack from every loaded row of kind
<code>RowKind::CompensationCompletion</code>, in sequence order, except the resume point's own row: the
resumed state runs again and pushes its own entry. A <code>RowKind::StepCursor</code> row also carries
an <code>output_blob</code>, but it holds a <a href="../stepped-task/">SteppedTask</a> or
<a href="../stream-task/">StreamTask</a> cursor, and the <code>kind</code> keeps the two apart.</li>
<li><code>compensate(res, output)</code> may run in a different process, so it must work purely from
<code>(res, output)</code>. The workflow definition (state labels and compensatable
<code>register</code> calls) must match across processes, as resume already requires.</li>
<li>If the resuming workflow's <a href="../recovery/#versioning"><code>workflow_version</code></a>
differs from the version stamped on the persisted rows, <code>resume_from</code> returns
<code>CanoError::WorkflowVersionMismatch</code> before it rebuilds the compensation stack. No rollback
runs in that case.</li>
</ul>

<div class="callout callout-tip">
<p>Run <code>cargo run --example saga_recovery --features recovery</code> for a saga wired to a
<code>RedbCheckpointStore</code>. A later step fails, the compensations drain LIFO, and the clean
rollback clears the checkpoint log.</p>
</div>
<hr class="section-divider">

<h2 id="observers"><a href="#observers" class="anchor-link" aria-hidden="true">#</a>Observer events</h2>
<p>
A compensatable task's forward <code>run</code> fires the usual <a href="../observers/">observer</a>
hooks: <code>on_task_start</code>, <code>on_retry</code>, and <code>on_task_success</code> or
<code>on_task_failure</code>. The completion checkpoint fires <code>on_checkpoint</code>. No hook
reports individual compensations, so have your compensators report their own progress if you need to
observe a rollback.
</p>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full example</h2>
<p>
A <code>Reserve → Validate → Charge → Ship → Done</code> workflow mixes both kinds of step.
<code>Reserve</code> and <code>Charge</code> are compensatable, and <code>Validate</code> and
<code>Ship</code> are plain. <code>Ship</code> fails because the courier is down. A plain task failing
drains the stack just as a compensatable one would: <code>Charge</code> refunds, then
<code>Reserve</code> releases the hold. <code>Validate</code> and the failed <code>Ship</code> are not
on the stack, so they are left alone. The clean rollback returns the original error from
<code>orchestrate</code>. This is the <code>saga_payment</code> example shipped with the crate; run it
with <code>cargo run --example saga_payment</code>.
</p>

```rust
use cano::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Reserve, Validate, Charge, Ship, Done }

/// What `ReserveInventory::run` produced, handed back to its `compensate` to undo it.
#[derive(Debug, Serialize, Deserialize)]
struct Reservation { order_id: String, sku: String, qty: u32 }

/// What `ChargeCard::run` produced, handed back to its `compensate` (a refund).
#[derive(Debug, Serialize, Deserialize)]
struct Charge { order_id: String, txn_id: String, amount_cents: u64 }

struct ReserveInventory;
struct ValidateOrder;
struct ChargeCard;
struct ShipOrder;

#[saga::task(state = Step)]
impl ReserveInventory {
    type Output = Reservation;
    async fn run(&self, _res: &Resources) -> Result<(Step, Reservation), CanoError> {
        let r = Reservation { order_id: "ord-1001".into(), sku: "WIDGET-7".into(), qty: 3 };
        println!("reserve  : holding {} x {}", r.qty, r.sku);
        Ok((Step::Validate, r))
    }
    async fn compensate(&self, _res: &Resources, r: Reservation) -> Result<(), CanoError> {
        println!("reserve  : releasing {} x {}  (rollback)", r.qty, r.sku);
        Ok(())
    }
}

// A plain task: it has nothing to undo, so it never appears on the compensation stack.
#[task(state = Step)]
impl ValidateOrder {
    async fn run(&self) -> Result<Step, CanoError> {
        println!("validate : order ok");
        Ok(Step::Charge)
    }
}

#[saga::task(state = Step)]
impl ChargeCard {
    type Output = Charge;
    async fn run(&self, _res: &Resources) -> Result<(Step, Charge), CanoError> {
        let charge = Charge { order_id: "ord-1001".into(), txn_id: "tx-7788".into(), amount_cents: 4_200 };
        println!("charge   : $42.00 charged ({})", charge.txn_id);
        Ok((Step::Ship, charge))
    }
    async fn compensate(&self, _res: &Resources, c: Charge) -> Result<(), CanoError> {
        println!("charge   : refunding {} cents (tx {})  (rollback)", c.amount_cents, c.txn_id);
        Ok(())
    }
}

// Another plain task, and this one fails. Its failure rolls back every compensatable step
// that ran before it: `Charge`, then `Reserve`.
#[task(state = Step)]
impl ShipOrder {
    async fn run(&self) -> Result<Step, CanoError> {
        println!("ship     : dispatching, courier unavailable");
        Err(CanoError::task_execution("courier unavailable"))
    }
}

#[tokio::main]
async fn main() {
    let workflow = Workflow::bare()
        .register(Step::Reserve, ReserveInventory)
        .register(Step::Validate, ValidateOrder)              // plain, no compensation
        .register(Step::Charge, ChargeCard)
        .register(Step::Ship, ShipOrder)                      // plain, and it fails
        .add_exit_state(Step::Done);

    match workflow.orchestrate(Step::Reserve).await {
        Ok(state) => println!("completed at {state:?}"),
        Err(error) => println!("failed, rolled back: {error}"), // "courier unavailable", the original error
    }
}
```
</div>
