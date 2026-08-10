+++
title = "Testing"
description = "Batteries-included fixtures for testing Cano workflows — a recording observer, an in-memory checkpoint store, a resources builder, a panic task factory, and a saga assertion."
template = "section.html"
+++

<div class="content-wrapper">
<h1>Testing</h1>
<p class="subtitle">Drop a pile of bespoke test fixtures for a one-line <code>use cano::testing::*;</code>.</p>
<p class="feature-tag">Everything on this page is behind the <code>testing</code> feature gate (<code>features = ["testing"]</code>) — typically enabled under <code>[dev-dependencies]</code>.</p>

<p>
The <code>cano::testing</code> module wraps existing public surface
(<a href="../observers/"><code>WorkflowObserver</code></a>,
<a href="../recovery/"><code>CheckpointStore</code></a>,
<a href="../resources/"><code>Resources</code></a>,
<a href="../store/"><code>MemoryStore</code></a>,
<a href="../task/"><code>Task</code></a>) into ready-made fixtures. It adds <strong>no</strong> new
traits and changes <strong>no</strong> existing public items — it just saves you from
re-writing the same recording observer and in-memory store in every test crate.
</p>

<p>
It is deliberately <strong>not</strong> in the <a href="../"><code>prelude</code></a>: import it
explicitly with <code>use cano::testing::*;</code> so production code never picks it up by
accident.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#enabling">Enabling the feature</a></li>
<li><a href="#recording-observer">RecordingObserver</a></li>
<li><a href="#in-memory-store">InMemoryCheckpointStore</a></li>
<li><a href="#test-resources">TestResources</a></li>
<li><a href="#panic-on-attempt">panic_on_attempt</a></li>
<li><a href="#assert-compensation">assert_compensation_ran</a></li>
</ol>
</nav>
<hr class="section-divider">

<h2 id="enabling">Enabling the feature</h2>

<p>
Add <code>cano</code> with the <code>testing</code> feature to your <code>[dev-dependencies]</code>
so the helpers compile only for tests and never ship in your release build:
</p>

```toml
[dev-dependencies]
{{ cano_dep(features=["testing"]) }}
```

<p>The <code>testing</code> feature pulls in no extra dependencies and is zero-cost when off.</p>

<h2 id="recording-observer">RecordingObserver</h2>

<p>
A <a href="../observers/"><code>WorkflowObserver</code></a> that records every lifecycle event
into an inspectable <code>Vec&lt;RecordedEvent&gt;</code>. Attach it, drive the workflow, then
assert against the recorded path — or use the <code>assert_path</code> /
<code>assert_completed_with</code> convenience checks.
</p>

<div class="diagram-frame">
<p class="diagram-label">Attach the observer, run the workflow, assert on what it recorded</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 272" role="img">
<title>A RecordingObserver is attached with with_observer, the workflow under test is orchestrated, every observer hook lands in a Vec of RecordedEvent, and the test asserts against that recording.</title>
<defs><marker id="harness-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M270,68 H466" marker-end="url(#harness-ah)"/>
<text class="t-code t-mut" x="368" y="56">.with_observer(obs)</text>
<path class="e" d="M595,92 V162" marker-end="url(#harness-ah)"/>
<text class="t-code ta-e" x="583" y="118">orchestrate(Start, &hellip;)</text>
<text class="t-mut ta-e" x="583" y="136">every hook recorded</text>
<path class="e" d="M466,190 H316" marker-end="url(#harness-ah)"/>
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
<text class="t-mut" x="390" y="254">Fixtures plug into the normal builders; the recording is what the test asserts against.</text>
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
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        Ok(TaskResult::Single(S::Done))
    }
}

#[tokio::test]
async fn recording_observer_captures_the_path() {
    let observer = Arc::new(RecordingObserver::new());
    let wf = Workflow::bare()
        .register(S::Start, OkTask)
        .add_exit_state(S::Done)
        .with_observer(observer.clone());

    assert_eq!(wf.orchestrate(S::Start, CancellationToken::disabled()).await.unwrap(), S::Done);

    // Assert the whole path, or inspect events directly.
    observer.assert_path(&["Start", "Done"]);
    observer.assert_completed_with("Done");
    assert!(observer.events().iter().any(|e| matches!(e, RecordedEvent::TaskSucceeded { .. })));
}
```

<p>
<code>RecordedEvent</code> mirrors the observer hooks (<code>StateEntered</code>,
<code>TaskStarted</code>, <code>TaskSucceeded</code>, <code>TaskFailed</code>, <code>Retry</code>,
<code>CircuitOpen</code>, <code>Checkpoint</code>, <code>Resume</code>). It is
<code>#[non_exhaustive]</code> — match with a wildcard arm.
</p>

<h3 id="state-coverage">State-coverage assertions</h3>

<p>
Two helpers turn the recorded <code>on_state_enter</code> events into CI guardrails that catch
<strong>dead states</strong> (a state you registered but nothing ever routes to) and routing
regressions. Both return <code>Result&lt;(), Vec&lt;String&gt;&gt;</code> — <code>Ok(())</code>
when every expected state was entered, or <code>Err</code> with the missing state labels (in
input order, deduplicated). Unlike <code>assert_path</code>, they return rather than panic, so
you can inspect the gap (<code>?</code>-propagate, <code>.expect(..)</code>, or
<code>.unwrap_err()</code>).
</p>

<ul>
<li><code>assert_all_states_entered(&amp;[..])</code> — check an explicit list of states (compared by their <code>Debug</code> rendering).</li>
<li><code>assert_registered_states_entered(&amp;workflow)</code> — check <em>every state the workflow registered a handler for</em> (via <a href="../workflows/"><code>Workflow::registered_states</code></a>). Exit-only states added with <code>add_exit_state</code> and no handler are not required.</li>
</ul>

<div class="diagram-frame">
<p class="diagram-label">Dead-state check: registered states minus states entered</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 272" role="img">
<title>assert_registered_states_entered compares the states the workflow registered a handler for against the states the RecordingObserver saw entered; an empty difference is Ok, and anything left over is a dead state reported as Err.</title>
<defs><marker id="cover-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M280,78 H298 Q306,78 306,86 V110 Q306,118 314,118 H326" marker-end="url(#cover-ah)"/>
<path class="e" d="M280,190 H298 Q306,190 306,182 V148 Q306,140 314,140 H326" marker-end="url(#cover-ah)"/>
<path class="e" d="M550,120 H562 Q570,120 570,112 V80 Q570,72 578,72 H596" marker-end="url(#cover-ah)"/>
<text class="t-mut t-ok ta-e" x="556" y="90">all entered</text>
<path class="e" d="M550,138 H562 Q570,138 570,146 V180 Q570,188 578,188 H596" marker-end="url(#cover-ah)"/>
<text class="t-mut t-err ta-e" x="556" y="176">missing</text>
<rect class="n" x="20" y="34" width="260" height="88" rx="10"/>
<text class="t-code" x="150" y="57">workflow.registered_states()</text>
<text class="t-strong" x="150" y="78">Start · Worker</text>
<text class="t-mut" x="150" y="99">exit-only Done excluded</text>
<rect class="n-cop" x="20" y="146" width="260" height="88" rx="10"/>
<text class="t-code" x="150" y="169">observer.states_entered()</text>
<text class="t-strong" x="150" y="190">Start · Done</text>
<text class="t-mut" x="150" y="211">from StateEntered events</text>
<rect class="n-hot" x="330" y="100" width="220" height="58" rx="10"/>
<text class="t-strong" x="440" y="120">compare</text>
<text class="t-code" x="440" y="138">registered &minus; entered</text>
<rect class="n-ok" x="600" y="44" width="160" height="56" rx="10"/>
<text class="t-code" x="680" y="63">Ok(())</text>
<text class="t-mut" x="680" y="81">no dead states</text>
<rect class="n-err" x="600" y="160" width="160" height="56" rx="10"/>
<text class="t-code" x="680" y="179">Err(["Worker"])</text>
<text class="t-mut t-err" x="680" y="197">dead state</text>
<text class="t-mut" x="390" y="254">assert_registered_states_entered names every registered state nothing ever routed to.</text>
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
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        Ok(TaskResult::Single(self.0.clone()))
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
    wf.orchestrate(S::Start, CancellationToken::disabled()).await.unwrap();

    // Every registered handler was actually reached — no dead states.
    observer.assert_registered_states_entered(&wf).expect("no dead states");

    // Or assert an explicit set; Err lists what was never entered.
    observer.assert_all_states_entered(&[S::Start, S::Worker, S::Done]).unwrap();
}
```

<h2 id="in-memory-store">InMemoryCheckpointStore</h2>

<p>
A process-local <a href="../recovery/"><code>CheckpointStore</code></a> for resume / recovery
tests — no <code>recovery</code> feature and no on-disk file needed. Share one
<code>Arc&lt;InMemoryCheckpointStore&gt;</code> across an <code>orchestrate</code> run and a later
<code>resume_from</code> to exercise the crash-recovery path entirely in memory. It honors the
full trait contract: duplicate <code>(workflow_id, sequence)</code> is rejected,
<code>load_run</code> returns rows sorted by sequence, and <code>clear</code> is per-id.
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
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        Ok(TaskResult::Single(S::Done))
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
    assert_eq!(wf.orchestrate(S::Start, CancellationToken::disabled()).await.unwrap(), S::Done);
}
```

<h2 id="test-resources">TestResources</h2>

<p>
A small builder for the <a href="../resources/"><code>Resources</code></a> a workflow needs.
<code>with_store</code> drops in a fresh <a href="../store/"><code>MemoryStore</code></a> at a key;
<code>with_resource</code> takes any <a href="../resources/"><code>Resource</code></a>.
</p>

```rust
use cano::prelude::*;
use cano::testing::TestResources;

let resources = TestResources::new()
    .with_store("store")
    .build();
assert!(resources.get::<MemoryStore, _>("store").is_ok());
```

<h2 id="panic-on-attempt">panic_on_attempt</h2>

<p>
A <a href="../task/"><code>Task</code></a> that panics on its first <em>N</em> attempts, for
exercising <a href="../resilience/">panic safety</a>: the engine converts a panicking task into a
<code>CanoError</code> (message starting <code>"panic: "</code>) instead of unwinding the
workflow loop.
</p>

<p class="feature-tag">
Note: the engine wraps its catch-unwind around the <em>whole</em> retry loop, so a panic
<strong>fails fast — panics are never retried</strong> (only a returned <code>Err</code> is).
With <code>panics &gt;= 1</code> the run fails on the first attempt; the success branch is reached
only with <code>panics == 0</code>.
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

    let err = wf.orchestrate(S::Start, CancellationToken::disabled()).await.unwrap_err();
    assert!(err.to_string().contains("panic"));
}
```

<h2 id="assert-compensation">assert_compensation_ran</h2>

<p>
A <a href="../saga/">saga</a> assertion that compares the recorded compensation order against
an expected sequence. Have each <code>compensate</code> push its identifier into a shared
<code>Vec</code> (typically held in a <a href="../resources/">resource</a>), then pass that slice
as <code>actual</code>. Compensations drain in reverse of completion order, so list
<code>expected</code> in the order you expect them to <em>undo</em>.
</p>

```rust
use cano::testing::assert_compensation_ran;

// Charge ran last, so it compensates first; then Reserve.
let ran = vec!["charge".to_string(), "reserve".to_string()];
assert_compensation_ran(&ran, &["charge", "reserve"]);
```

<hr class="section-divider">

<p>
Runnable end-to-end demo of every helper:
<code>cargo run --example testing_helpers --features testing</code>.
</p>

</div>
