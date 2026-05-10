+++
title = "Saga / Compensation"
description = "Pair a Cano workflow step with a compensating action; if a later step fails, roll back the work already done."
template = "page.html"
+++

<div class="content-wrapper">
<h1>Saga / Compensation</h1>
<p class="subtitle">Undo successful steps in reverse when a later step fails.</p>

<p>
Once a workflow step has charged a card or reserved inventory, the FSM can't roll it back by
itself — only an explicit refund or release does. The <strong>saga</strong> pattern handles this:
pair each irreversible forward step with a <strong>compensating action</strong>, and replay those
compensations in reverse order if a later step fails.
</p>
<p>
In Cano you implement <strong><code>CompensatableTask</code></strong> for a state's task — its
<code>run</code> returns the next state <em>and</em> an <code>Output</code> describing what it did;
its <code>compensate</code> takes that <code>Output</code> back and undoes it — and register it with
<code>Workflow::register_with_compensation</code>. The engine keeps a per-run compensation stack;
if a later state's task fails, it drains the stack <strong>LIFO</strong> and runs each
<code>compensate</code>. With a <a href="../recovery/">checkpoint store</a> attached, the outputs are
persisted, so a resumed run can still compensate work done in an earlier process.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#writing">Writing a Compensatable Task</a></li>
<li><a href="#registering">Registering It</a></li>
<li><a href="#stack">The Compensation Stack</a></li>
<li><a href="#result">What <code>orchestrate</code> Returns</a></li>
<li><a href="#idempotency">The Idempotency Contract</a></li>
<li><a href="#recovery">Composing with Recovery</a></li>
<li><a href="#observers">Observer Events</a></li>
<li><a href="#full-example">Full Example</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="writing"><a href="#writing" class="anchor-link" aria-hidden="true">#</a>Writing a Compensatable Task</h2>
<p>
<code>CompensatableTask</code> is a standalone trait (not an extension of <a href="../task/">Task</a>) —
its <code>run</code> returns <code>(next_state, Output)</code>, which <code>Task::run</code> has no slot
for. But you write the <code>impl</code> just like a plain <a href="../task/"><code>#[task]</code></a>
one, with the <code>compensatable</code> flag: <code>#[task(state = …, compensatable)]</code> on an
inherent <code>impl</code> block builds the <code>impl CompensatableTask&lt;…&gt; for …</code> header
for you — you write only <code>type Output</code>, <code>run</code>, and <code>compensate</code> (plus
<code>config</code> / <code>name</code> if you want them). The associated <code>Output</code> must be
<code>serde</code>-serializable; it's the <strong>only</strong> thing carried from <code>run</code> to
<code>compensate</code> (they may run in different processes after a resume), so make it self-contained.
</p>

```rust
use cano::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Reserve, Ship, Done }

#[derive(Serialize, Deserialize)]
struct Reservation { sku: String, qty: u32 }

struct ReserveInventory;

#[task(state = Step, compensatable)]
impl ReserveInventory {
    type Output = Reservation;

    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        // ... actually reserve the stock ...
        Ok((TaskResult::Single(Step::Ship), Reservation { sku: "WIDGET-7".into(), qty: 3 }))
    }

    async fn compensate(&self, _res: &Resources, output: Reservation) -> Result<(), CanoError> {
        // ... release `output.qty` of `output.sku` — must be idempotent ...
        let _ = output;
        Ok(())
    }
}
```

<p>
<code>#[compensatable_task(state = Step)]</code> is exactly equivalent to
<code>#[task(state = Step, compensatable)]</code> — use whichever name you prefer. And if you'd rather
write the trait header yourself — e.g. for a non-default resource-key type, or a generic impl — a
bare <code>#[compensatable_task] impl CompensatableTask&lt;Step, MyKey&gt; for ReserveInventory { … }</code>
works too (or pass <code>key = MyKey</code> to the inherent form:
<code>#[task(state = Step, key = MyKey, compensatable)]</code>).
</p>

<p>
<code>config()</code> and <code>name()</code> have the same defaults as <code>Task</code> — override
<code>config()</code> to change the forward run's retry/timeout policy (e.g.
<code>TaskConfig::minimal()</code> for fail-fast), and <code>name()</code> to give the task a stable
id (it's the compensation-stack key, so a friendly name helps when reading logs). <code>Output</code>
can be <code>()</code> for a compensatable task that needs no data to undo itself.
</p>
<hr class="section-divider">

<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering It</h2>
<p>
<code>Workflow::register_with_compensation(state, task)</code> registers a compensatable task for a
state. It's <strong>single-task states only</strong> — there is no
<code>register_split_with_compensation</code>.
</p>

```rust
use cano::prelude::*;

// ReserveInventory / ChargeCard / ShipOrder each `impl CompensatableTask<Step>`.
let workflow = Workflow::bare()
    .register_with_compensation(Step::Reserve, ReserveInventory)
    .register_with_compensation(Step::Charge, ChargeCard)
    .register_with_compensation(Step::Ship, ShipOrder)
    .add_exit_state(Step::Done);
```

<p>
You can mix compensatable and plain (<code>register</code>) states freely — only the compensatable
ones contribute to the stack. A plain task that fails still triggers the rollback of every
compensatable step that ran before it.
</p>
<hr class="section-divider">

<h2 id="stack"><a href="#stack" class="anchor-link" aria-hidden="true">#</a>The Compensation Stack</h2>
<ul>
<li>Each <strong>successful</strong> compensatable task pushes <code>(task name, serialized output)</code>
onto the run's stack.</li>
<li>On <strong>any terminal failure</strong> — a task error, a misconfigured split, a missing handler,
or a checkpoint-append failure — the stack is drained <strong>last-in, first-out</strong>: for each
entry the engine finds the compensator by task name, deserializes the output, and runs
<code>compensate</code>.</li>
<li>The drain <strong>never stops early</strong>: a <code>compensate</code> error is recorded and the
remaining entries are still compensated.</li>
<li>A compensatable task that <em>fails</em> its forward <code>run</code> produced no output, so it is
not on the stack — there is nothing to undo for it.</li>
</ul>
<hr class="section-divider">

<h2 id="result"><a href="#result" class="anchor-link" aria-hidden="true">#</a>What <code>orchestrate</code> Returns</h2>
<table>
<thead><tr><th>Outcome</th><th>Returned</th><th>Checkpoint log</th></tr></thead>
<tbody>
<tr><td>Run reaches an exit state</td><td><code>Ok(final_state)</code></td><td>cleared</td></tr>
<tr><td>Failure, nothing to compensate (empty stack)</td><td>the original error</td><td>kept</td></tr>
<tr><td>Failure, every <code>compensate</code> succeeded</td><td>the original error, unchanged</td><td>cleared</td></tr>
<tr><td>Failure, ≥1 <code>compensate</code> failed</td><td><code>CanoError::CompensationFailed { errors }</code></td><td>kept (for manual recovery)</td></tr>
</tbody>
</table>
<p>
A <em>clean</em> rollback returns the <strong>original</strong> error — not <code>CompensationFailed</code>;
the rollback succeeded, the workflow still didn't. <code>CompensationFailed.errors[0]</code> is that
original failure; <code>errors[1..]</code> are the compensation errors, in drain order (a stack entry
whose compensator isn't registered — e.g. resuming against a changed workflow definition — counts as
one). All checkpoint-log clearing is best-effort: a failed <code>clear</code> is logged, never fatal.
</p>
<hr class="section-divider">

<h2 id="idempotency"><a href="#idempotency" class="anchor-link" aria-hidden="true">#</a>The Idempotency Contract</h2>

<div class="callout callout-warning">
<span class="callout-label">Important</span>
<p><strong><code>compensate</code> must be idempotent.</strong> It can run more than once for the same
logical step — most often when a <a href="../recovery/">resume</a> re-runs a compensatable task whose
result was already recorded, or when a re-run lands between writing the task's completion checkpoint
and entering the next state. Use refunds keyed by transaction id, conditional releases, "if still
reserved" checks — anything that's safe to apply twice. The forward <code>run</code> at and after a
resume point must likewise be idempotent.</p>
</div>
<hr class="section-divider">

<h2 id="recovery"><a href="#recovery" class="anchor-link" aria-hidden="true">#</a>Composing with Recovery</h2>
<p>
With a <a href="../recovery/">checkpoint store</a> attached
(<code>with_checkpoint_store</code> + <code>with_workflow_id</code>):
</p>
<ul>
<li>A successful compensatable state writes a second <em>completion</em>
<a href="../recovery/"><code>CheckpointRow</code></a> with <code>output_blob</code> set to the
serialized output (so the state's entry row and completion row consume two sequence numbers).</li>
<li><code>Workflow::resume_from</code> rehydrates the compensation stack from every loaded row that
carries an <code>output_blob</code>, in sequence order — so a failure after the resume point can
still roll back work the original process did before the crash.</li>
<li>Because <code>compensate(res, output)</code> may run in a <strong>different process</strong>, it must
work purely from <code>(res, output)</code>, and the workflow definition (state labels +
<code>register_with_compensation</code> calls) must match across processes — the same constraint that
already applies to resume itself.</li>
</ul>
<hr class="section-divider">

<h2 id="observers"><a href="#observers" class="anchor-link" aria-hidden="true">#</a>Observer Events</h2>
<p>
A compensatable task's forward <code>run</code> fires the usual <a href="../observers/">observer</a>
hooks — <code>on_task_start</code>, <code>on_retry</code>, <code>on_task_success</code> /
<code>on_task_failure</code> — and the completion checkpoint fires <code>on_checkpoint</code>. There is
no dedicated "compensating" hook in this version; if you need to observe rollbacks, have your
compensators report progress themselves.
</p>
<hr class="section-divider">

<h2 id="full-example"><a href="#full-example" class="anchor-link" aria-hidden="true">#</a>Full Example</h2>
<p>
A <code>Reserve → Validate → Charge → Ship → Done</code> workflow that mixes both kinds of step:
<code>Reserve</code> and <code>Charge</code> are compensatable; <code>Validate</code> and
<code>Ship</code> are plain. <code>Ship</code> fails (courier down) — and a plain task failing
drains the compensation stack just like a compensatable one would: <code>Charge</code> refunds,
then <code>Reserve</code> releases the hold. The plain steps (<code>Validate</code>, and the
<code>Ship</code> that failed) aren't on the stack, so they're left alone. A clean rollback like
this returns the original error from <code>orchestrate</code>. This is the <code>saga_payment</code>
example shipped with the crate; run it with <code>cargo run --example saga_payment</code>.
</p>

```rust
use cano::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Reserve, Validate, Charge, Ship, Done }

/// What `ReserveInventory::run` produced — handed back to its `compensate` to undo it.
#[derive(Debug, Serialize, Deserialize)]
struct Reservation { order_id: String, sku: String, qty: u32 }

/// What `ChargeCard::run` produced — handed back to its `compensate` (a refund).
#[derive(Debug, Serialize, Deserialize)]
struct Charge { order_id: String, txn_id: String, amount_cents: u64 }

struct ReserveInventory;
struct ValidateOrder;
struct ChargeCard;
struct ShipOrder;

#[task(state = Step, compensatable)]
impl ReserveInventory {
    type Output = Reservation;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Reservation), CanoError> {
        let r = Reservation { order_id: "ord-1001".into(), sku: "WIDGET-7".into(), qty: 3 };
        println!("reserve  : holding {} × {}", r.qty, r.sku);
        Ok((TaskResult::Single(Step::Validate), r))
    }
    async fn compensate(&self, _res: &Resources, r: Reservation) -> Result<(), CanoError> {
        println!("reserve  : releasing {} × {}  (rollback)", r.qty, r.sku);
        Ok(())
    }
}

// A plain task — `register`, not `register_with_compensation`. Nothing to undo, so it never
// appears on the compensation stack.
#[task(state = Step)]
impl ValidateOrder {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("validate : order ok");
        Ok(TaskResult::Single(Step::Charge))
    }
}

#[task(state = Step, compensatable)]
impl ChargeCard {
    type Output = Charge;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, Charge), CanoError> {
        let charge = Charge { order_id: "ord-1001".into(), txn_id: "tx-7788".into(), amount_cents: 4_200 };
        println!("charge   : $42.00 charged ({})", charge.txn_id);
        Ok((TaskResult::Single(Step::Ship), charge))
    }
    async fn compensate(&self, _res: &Resources, c: Charge) -> Result<(), CanoError> {
        println!("charge   : refunding {} cents (tx {})  (rollback)", c.amount_cents, c.txn_id);
        Ok(())
    }
}

// Another plain task — and this one fails. Its failure rolls back every compensatable step
// that ran before it: `Charge`, then `Reserve`.
#[task(state = Step)]
impl ShipOrder {
    fn config(&self) -> TaskConfig { TaskConfig::minimal() } // fail-fast — surface the original error
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        println!("ship     : dispatching → courier unavailable");
        Err(CanoError::task_execution("courier unavailable"))
    }
}

let workflow = Workflow::bare()
    .register_with_compensation(Step::Reserve, ReserveInventory)
    .register(Step::Validate, ValidateOrder)              // plain — no compensation
    .register_with_compensation(Step::Charge, ChargeCard)
    .register(Step::Ship, ShipOrder)                      // plain — and it fails
    .add_exit_state(Step::Done);

match workflow.orchestrate(Step::Reserve).await {
    Ok(state) => println!("completed at {state:?}"),
    Err(error) => println!("failed, rolled back: {error}"), // "courier unavailable" — the original error
}
```
</div>
