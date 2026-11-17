+++
title = "Testing"
description = "Test fixtures for Cano workflows: a recording observer, an in-memory checkpoint store, a panicking task factory, and a saga assertion."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Testing</h1>
<p class="subtitle">Ready-made fixtures for workflow tests, behind <code>use cano::testing::*;</code>.</p>
<p class="feature-tag">Everything on this page is behind the <code>testing</code> feature gate (<code>features = ["testing"]</code>), normally enabled under <code>[dev-dependencies]</code>.</p>

<p>
This page is a reference for the <code>cano::testing</code> module. The module wraps public surface
(<a href="../observers/"><code>WorkflowObserver</code></a>,
<a href="../recovery/"><code>CheckpointStore</code></a>,
<a href="../task/"><code>Task</code></a>) into fixtures, so each test crate does not rewrite the same
recording observer and in-memory store. It adds no traits and changes no existing public items.
</p>

<p>
The module is not in the <a href="../"><code>prelude</code></a>. Import it explicitly with
<code>use cano::testing::*;</code> so production code never picks it up by accident.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#enabling">Enabling the feature</a></li>
<li><a href="#recording-observer">RecordingObserver</a></li>
<li><a href="#in-memory-store">InMemoryCheckpointStore</a></li>
<li><a href="#store-contract">assert_checkpoint_store_contract</a></li>
<li><a href="#panic-on-attempt">panic_on_attempt</a></li>
<li><a href="#assert-compensation">assert_compensation_ran</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="enabling"><a href="#enabling" class="anchor-link" aria-hidden="true">#</a>Enabling the feature</h2>

<p>
Add <code>cano</code> with the <code>testing</code> feature to <code>[dev-dependencies]</code>, so the
helpers compile only for tests and never ship in a release build:
</p>

```toml
[dev-dependencies]
cano = { version = "0.16", features = ["testing"] }
```

<p>The <code>testing</code> feature pulls in no extra dependencies.</p>

<h2 id="recording-observer"><a href="#recording-observer" class="anchor-link" aria-hidden="true">#</a>RecordingObserver</h2>

<p>
A <a href="../observers/"><code>WorkflowObserver</code></a> that records every lifecycle event into an
inspectable <code>Vec&lt;RecordedEvent&gt;</code>. Attach it, drive the workflow, then assert against
the recorded path with <code>assert_path</code> and <code>assert_completed_with</code>, or read the
events directly.
</p>

<div class="diagram-frame">
<p class="diagram-label">Attach the observer, run the workflow, assert on what it recorded</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 28 780 216" role="img" aria-labelledby="testing-record-title testing-record-desc">
<title id="testing-record-title">Recording a workflow run and asserting on it</title>
<desc id="testing-record-desc">A RecordingObserver is attached to the workflow under test with with_observer. The test calls orchestrate on the workflow, and every observer hook lands in a Vec of RecordedEvent. The test then reads observer.events and checks them with assert_path, assert_completed_with and assert_registered_states_entered.</desc>
<defs><marker id="testing-record-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M270,68 H466" marker-end="url(#testing-record-ah)"/>
<text class="t-code t-mut" x="368" y="56">.with_observer(obs)</text>
<path class="e" d="M595,92 V162" marker-end="url(#testing-record-ah)"/>
<text class="t-code ta-e" x="583" y="118">orchestrate(Start)</text>
<text class="t-mut ta-e" x="583" y="136">every hook recorded</text>
<path class="e" d="M466,190 H316" marker-end="url(#testing-record-ah)"/>
<text class="t-code t-mut" x="388" y="178">observer.events()</text>
<rect class="n-hot" x="40" y="44" width="230" height="48" rx="10"/>
<text class="t-code" x="155" y="69">RecordingObserver</text>
<rect class="n" x="470" y="44" width="250" height="48" rx="10"/>
<text class="t-strong" x="595" y="69">Workflow under test</text>
<rect class="n-cop" x="470" y="166" width="250" height="48" rx="10"/>
<text class="t-code" x="595" y="191">Vec&lt;RecordedEvent&gt;</text>
<rect class="n-ok" x="20" y="152" width="290" height="76" rx="10"/>
<text class="t-code" x="165" y="170">assert_path(&amp;[&hellip;])</text>
<text class="t-code" x="165" y="190">assert_completed_with</text>
<text class="t-code" x="165" y="210">assert_registered_states_entered</text>
</svg>
</div>
</div>

```rust
use cano::prelude::*;
use cano::testing::*;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum S { Start, Done }

struct OkTask;
#[task]
impl Task<S> for OkTask {
    async fn run(&self) -> Result<S, CanoError> {
        Ok(S::Done)
    }
}

#[tokio::test]
async fn recording_observer_captures_the_path() {
    let observer = Arc::new(RecordingObserver::new());
    let wf = Workflow::bare()
        .register(S::Start, OkTask)
        .add_exit_state(S::Done)
        .with_observer(observer.clone());

    assert_eq!(wf.orchestrate(S::Start).await.unwrap(), S::Done);

    // Assert the whole path, or inspect events directly.
    observer.assert_path(&["Start", "Done"]);
    observer.assert_completed_with("Done");
    assert!(observer.events().iter().any(|e| matches!(e, RecordedEvent::TaskSucceeded { .. })));
}
```

<p>
<code>RecordedEvent</code> has one variant per observer hook: <code>StateEntered</code>,
<code>TaskStarted</code>, <code>TaskSucceeded</code>, <code>TaskFailed</code>, <code>Retry</code>,
<code>CircuitOpen</code>, <code>Checkpoint</code>, <code>Resume</code>, <code>Cancelled</code>,
<code>WorkflowTimeout</code>, <code>CheckpointClearFailed</code> and <code>UnknownResumeState</code>.
It is <code>#[non_exhaustive]</code>, so match it with a wildcard arm.
</p>

<h3 id="state-coverage"><a href="#state-coverage" class="anchor-link" aria-hidden="true">#</a>State-coverage assertions</h3>

<p>
Two helpers turn the recorded <code>on_state_enter</code> events into CI guardrails that catch dead
states (a state you registered that nothing routes to) and routing regressions. Both return
<code>Result&lt;(), Vec&lt;String&gt;&gt;</code>: <code>Ok(())</code> when every expected state was
entered, or <code>Err</code> with the missing state labels in input order, deduplicated. Unlike
<code>assert_path</code> they return instead of panicking, so you can inspect the gap with
<code>?</code>, <code>.expect(..)</code> or <code>.unwrap_err()</code>.
</p>

<ul>
<li><code>assert_all_states_entered(&amp;[..])</code> checks an explicit list of states, compared by their <code>Debug</code> rendering.</li>
<li><code>assert_registered_states_entered(&amp;workflow)</code> checks every state the workflow registered a handler for, through <a href="../workflows/"><code>Workflow::registered_states</code></a>. Exit-only states added with <code>add_exit_state</code> and no handler are not required.</li>
</ul>

<div class="diagram-frame">
<p class="diagram-label">Dead-state check: registered states minus states entered</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 18 780 259" role="img" aria-labelledby="testing-cover-title testing-cover-desc">
<title id="testing-cover-title">Dead-state check with assert_registered_states_entered</title>
<desc id="testing-cover-desc">The function compares two lists. The first is workflow.registered_states, here Start and Worker, with the exit-only state Done excluded. The second is observer.states_entered, built from the StateEntered events, here Start and Done. If every registered state was entered, the result is Ok. Any registered state that was never entered is a dead state, and the result is Err with its label, here Err containing Worker.</desc>
<defs><marker id="testing-cover-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M280,78 H298 Q306,78 306,86 V110 Q306,118 314,118 H326" marker-end="url(#testing-cover-ah)"/>
<path class="e" d="M280,190 H298 Q306,190 306,182 V148 Q306,140 314,140 H326" marker-end="url(#testing-cover-ah)"/>
<path class="e" d="M550,120 H562 Q570,120 570,112 V80 Q570,72 578,72 H596" marker-end="url(#testing-cover-ah)"/>
<text class="t-mut t-ok ta-e" x="556" y="90">all entered</text>
<path class="e" d="M550,138 H562 Q570,138 570,146 V180 Q570,188 578,188 H596" marker-end="url(#testing-cover-ah)"/>
<text class="t-mut t-err ta-e" x="556" y="176">missing</text>
<rect class="n" x="20" y="34" width="260" height="88" rx="10"/>
<text class="t-code" x="150" y="57">workflow.registered_states()</text>
<text class="t-strong" x="150" y="78">Start &middot; Worker</text>
<text class="t-mut" x="150" y="99">exit-only Done excluded</text>
<rect class="n-cop" x="20" y="146" width="260" height="88" rx="10"/>
<text class="t-code" x="150" y="169">observer.states_entered()</text>
<text class="t-strong" x="150" y="190">Start &middot; Done</text>
<text class="t-mut" x="150" y="211">from StateEntered events</text>
<rect class="n-hot" x="330" y="100" width="220" height="58" rx="10"/>
<text class="t-strong" x="440" y="120">compare</text>
<text class="t-code" x="440" y="138">registered minus entered</text>
<rect class="n-ok" x="600" y="44" width="160" height="56" rx="10"/>
<text class="t-code" x="680" y="63">Ok(())</text>
<text class="t-mut" x="680" y="81">no dead states</text>
<rect class="n-err" x="600" y="160" width="160" height="56" rx="10"/>
<text class="t-code" x="680" y="179">Err(["Worker"])</text>
<text class="t-mut t-err" x="680" y="197">dead state</text>
<text class="t-mut" x="390" y="254">assert_registered_states_entered names every registered state nothing routed to.</text>
</svg>
</div>
</div>

```rust
use cano::prelude::*;
use cano::testing::RecordingObserver;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum S { Start, Worker, Done }

#[derive(Clone)]
struct Go(S);
#[task]
impl Task<S> for Go {
    async fn run(&self) -> Result<S, CanoError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn every_registered_state_is_reached() {
    let observer = Arc::new(RecordingObserver::new());
    let wf = Workflow::bare()
        .register(S::Start, Go(S::Worker))
        .register(S::Worker, Go(S::Done))
        .add_exit_state(S::Done)
        .with_observer(observer.clone());
    wf.orchestrate(S::Start).await.unwrap();

    // Every registered handler was reached: no dead states.
    observer.assert_registered_states_entered(&wf).expect("no dead states");

    // Or assert an explicit set; Err lists what was never entered.
    observer.assert_all_states_entered(&[S::Start, S::Worker, S::Done]).unwrap();
}
```

<h2 id="in-memory-store"><a href="#in-memory-store" class="anchor-link" aria-hidden="true">#</a>InMemoryCheckpointStore</h2>

<p>
A process-local <a href="../recovery/"><code>CheckpointStore</code></a> for resume and recovery tests.
It needs no on-disk file. Share one
<code>Arc&lt;InMemoryCheckpointStore&gt;</code> between an <code>orchestrate</code> run and a later
<code>resume_from</code> to exercise crash recovery entirely in memory. It honors the full trait
contract: a duplicate <code>(workflow_id, sequence)</code> is rejected, <code>load_run</code> returns
rows sorted by sequence, and <code>clear</code> is per workflow id.
</p>

<p>
Two readers show what a run wrote. <code>rows(workflow_id)</code> returns the live rows sorted by
sequence, empty once a successful run has cleared its log. <code>history()</code> returns every row
ever appended with its workflow id, in append order, and <code>clear</code> never touches it.
</p>

```rust
use cano::prelude::*;
use cano::testing::*;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum S { Start, Done }

struct Go;
#[task]
impl Task<S> for Go {
    async fn run(&self) -> Result<S, CanoError> {
        Ok(S::Done)
    }
}

#[tokio::test]
async fn checkpoints_run_in_memory() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let wf = Workflow::bare()
        .register(S::Start, Go)
        .add_exit_state(S::Done)
        .with_checkpoint_store(store.clone())
        .with_workflow_id("run-1");
    assert_eq!(wf.orchestrate(S::Start).await.unwrap(), S::Done);

    // A successful run clears its live log; history() keeps every appended row.
    assert!(store.rows("run-1").is_empty());
    assert!(!store.history().is_empty());
}
```

<h2 id="store-contract"><a href="#store-contract" class="anchor-link" aria-hidden="true">#</a>assert_checkpoint_store_contract</h2>

<p>
Runs the <a href="../recovery/#trait"><code>CheckpointStore</code> contract</a> against any store and
panics with the name of the first clause it breaks. Use it on your own backend, or on a built-in one
pointed at a real server. It writes only workflow ids that start with <code>namespace/</code> and
clears them before and after, so it can share a database with other data.
</p>

<p>
It checks that an unknown id loads empty, that every field and <code>RowKind</code> round-trips, that
rows load sorted by sequence, that a duplicate sequence is rejected and changes nothing, that ids
sharing a prefix stay isolated under <code>clear</code>, that awkward ids (spaces, wildcards,
non-ASCII, 200+ characters) work, that concurrent appends of one sequence have exactly one winner, and
that a 256 KiB blob round-trips.
</p>

```rust
use cano::testing::{InMemoryCheckpointStore, assert_checkpoint_store_contract};

#[tokio::test]
async fn my_store_meets_the_contract() {
    let store = InMemoryCheckpointStore::new(); // your store here
    assert_checkpoint_store_contract(&store, "contract-test").await;
}
```

<h2 id="panic-on-attempt"><a href="#panic-on-attempt" class="anchor-link" aria-hidden="true">#</a>panic_on_attempt</h2>

<p>
A <a href="../task/"><code>Task</code></a> that panics on its first <em>N</em> attempts, for testing
<a href="../resilience/">panic safety</a>. The engine converts a panicking task into a
<code>CanoError</code> whose message starts with <code>"panic: "</code> instead of unwinding the
workflow loop.
</p>

<p>
The engine wraps its catch-unwind around the whole retry loop, so a panic fails fast and is never
retried. Only a returned <code>Err</code> is. With <code>panics &gt;= 1</code> the run fails on the
first attempt, and the success branch is reached only with <code>panics == 0</code>.
</p>

```rust
use cano::prelude::*;
use cano::testing::panic_on_attempt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum S { Start, Done }

#[tokio::test]
async fn panicking_task_fails_fast() {
    let wf = Workflow::bare()
        .register(S::Start, panic_on_attempt(1, S::Done))
        .add_exit_state(S::Done);

    let err = wf.orchestrate(S::Start).await.unwrap_err();
    assert!(err.to_string().contains("panic"));
}
```

<h2 id="assert-compensation"><a href="#assert-compensation" class="anchor-link" aria-hidden="true">#</a>assert_compensation_ran</h2>

<p>
A <a href="../saga/">saga</a> assertion that compares the recorded compensation order against an
expected sequence. Have each <code>compensate</code> push its identifier into a shared
<code>Vec</code>, typically held in a <a href="../resources/">resource</a>, then pass that slice as
<code>actual</code>. Compensations drain in reverse of completion order, so list
<code>expected</code> in the order you expect them to undo.
</p>

```rust
use cano::testing::assert_compensation_ran;

// Charge ran last, so it compensates first; then Reserve.
let ran = vec!["charge".to_string(), "reserve".to_string()];
assert_compensation_ran(&ran, &["charge", "reserve"]);
```
<hr class="section-divider">

<p>
A runnable demo of every helper: <code>cargo run --example testing_helpers --features testing</code>.
</p>

</div>
