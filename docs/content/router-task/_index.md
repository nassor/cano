+++
title = "RouterTask"
description = "RouterTask in Cano - side-effect-free branching that leaves no recovery footprint."
template = "section.html"
+++

<div class="content-wrapper">
<h1>RouterTask</h1>
<p class="subtitle">Side-effect-free branching for your workflows.</p>

<p>
A <code>RouterTask</code> reads <a href="../resources/">Resources</a> and returns the next state
(<code>Result&lt;TState, CanoError&gt;</code>). It writes to no store and calls no external system.
Because re-running a router costs nothing, the engine records no checkpoint row for it (see
<a href="../recovery/">Crash recovery</a>). It is one of the <a href="../task/">Task</a> family of
processing models, alongside <a href="../poll-task/">PollTask</a>, <a href="../timer-task/">TimerTask</a>,
<a href="../batch-task/">BatchTask</a>, <a href="../stepped-task/">SteppedTask</a>,
<a href="../stream-task/">StreamTask</a> and <a href="../suspend-task/">SuspendTask</a>.
</p>

<div class="callout callout-info">
<div class="callout-label">Router or task?</div>
<p>
A router decides and does nothing else. If the branching logic also writes something, use a plain
<a href="../task/">Task</a> and return the next state from <code>run</code>.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#quick-start">Quick start with <code>#[task::router]</code></a></li>
<li><a href="#registering">Registering a router</a></li>
<li><a href="#explicit">Explicit trait-impl form</a></li>
<li><a href="#when-to-use">When to use RouterTask</a></li>
</ol>
</nav>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start with <code>#[task::router]</code></h2>
<p>
Attach <code>#[task::router(state = MyState)]</code> to an inherent <code>impl</code> block and write
<code>route</code>. The macro generates the <code>impl RouterTask&lt;MyState&gt;</code> header, a
<code>Registrable</code> impl that selects the no-checkpoint path, and a companion
<code>impl Task&lt;MyState&gt;</code> so the struct can also be a <code>register_split</code> branch.
<code>route</code> may omit the <code>res</code> parameter when the decision needs no resources.
</p>
<p>The trait has one required method and two optional ones:</p>
<dl class="key-list">
<dt><code>async fn route(&amp;self, res: &amp;Resources) -&gt; Result&lt;TState, CanoError&gt;</code> <small>required</small></dt>
<dd>Reads resources and returns the next state.</dd>
<dt><code>fn config(&amp;self) -&gt; TaskConfig</code> <small>default <code>TaskConfig::new()</code>, one attempt</small></dt>
<dd>Retry and timeout settings for the dispatch.</dd>
<dt><code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code> <small>default: the type name</small></dt>
<dd>Identifies the router in logs and observers.</dd>
</dl>

<div class="diagram-frame">
<p class="diagram-label">route() reads, decides, and returns; it writes nothing</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 330" role="img" aria-labelledby="router-flow-title router-flow-desc">
<title id="router-flow-title">A router reads resources and picks the next state</title>
<desc id="router-flow-desc">The engine enters the Classify state and calls route with the Resources. route reads the config from Resources and writes nothing. It returns Ok(Step::FastPath) when the flag is set and Ok(Step::SlowPath) otherwise. Because the decision has no side effects, the engine writes no CheckpointRow for the state.</desc>
<defs><marker id="router-flow-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M393,70 V112" marker-end="url(#router-flow-ah)"/>
<text class="t-mut ta-e" x="383" y="93">reads, never writes</text>
<path class="e" d="M156,143 H296" marker-end="url(#router-flow-ah)"/>
<text class="t-code t-mut" x="226" y="127">dispatch</text>
<path class="e" d="M486,143 H536 Q544,143 544,135 V75 Q544,67 552,67 H586" marker-end="url(#router-flow-ah)"/>
<text class="t-code t-mut ta-s" x="554" y="110">Ok(Step::FastPath)</text>
<path class="e" d="M486,143 H536 Q544,143 544,151 V211 Q544,219 552,219 H586" marker-end="url(#router-flow-ah)"/>
<text class="t-code t-mut ta-s" x="554" y="176">Ok(Step::SlowPath)</text>
<path class="e e-dash e-dim" d="M393,170 V248" marker-end="url(#router-flow-ah)"/>
<text class="t-mut ta-s" x="403" y="205">no side effects</text>
<rect class="n-cop" x="298" y="16" width="190" height="54" rx="10"/>
<text class="t-strong" x="393" y="34">Resources</text>
<text class="t-code" x="393" y="53">config.use_fast_path</text>
<rect class="n" x="16" y="120" width="140" height="46" rx="10"/>
<text class="t-code" x="86" y="144">Step::Classify</text>
<rect class="n-hot" x="300" y="116" width="186" height="54" rx="10"/>
<text class="t-code t-strong" x="393" y="134">route(&amp;Resources)</text>
<text class="t-code t-mut" x="393" y="153">&#8594; Result&lt;Step&gt;</text>
<rect class="n" x="590" y="44" width="170" height="46" rx="10"/>
<text class="t-code" x="675" y="68">Step::FastPath</text>
<rect class="n" x="590" y="196" width="170" height="46" rx="10"/>
<text class="t-code" x="675" y="220">Step::SlowPath</text>
<rect class="n-ghost" x="298" y="252" width="190" height="54" rx="10"/>
<text class="t-code t-warn" x="393" y="270">CheckpointRow</text>
<text class="t-mut t-warn" x="393" y="289">no row, no sequence</text>
</svg>
</div>
</div>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form: <code>#[task::router(state = ...)]</code> on an inherent impl</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Classify, FastPath, SlowPath, Done }

struct Config { use_fast_path: bool }
#[resource]
impl Resource for Config {}

struct Classifier;

#[task::router(state = Step)]
impl Classifier {
    async fn route(&self, res: &Resources) -> Result<Step, CanoError> {
        let config = res.get::<Config>()?;
        if config.use_fast_path {
            Ok(Step::FastPath)
        } else {
            Ok(Step::SlowPath)
        }
    }
}
```
</div>

<!-- Section: Registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a router</h2>
<p>
Register a router with <code>Workflow::register(state, task)</code>, the same call every other model
uses. The engine dispatches it like a single-task state, with one difference: it writes no
<code>CheckpointRow</code> for the router state and consumes no checkpoint sequence number. Observers
still receive <code>on_state_enter</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a router into a workflow</span>

```rust
use cano::prelude::*;

let workflow = Workflow::new(resources)
    .register(Step::Classify, Classifier)         // router state: leaves no checkpoint row
    .register(Step::FastPath, FastProcessor)
    .register(Step::SlowPath, SlowProcessor)
    .add_exit_state(Step::Done);
```
</div>

<div class="callout callout-info">
<div class="callout-label">Recovery interplay</div>
<p>
On a <a href="../recovery/">checkpointed</a> workflow, a <code>Start</code>, <code>Classify</code>
(router), <code>FastPath</code>, <code>Done</code> run records rows for <code>Start</code>,
<code>FastPath</code> and <code>Done</code>, not <code>Classify</code>. A crash inside
<code>FastProcessor</code> resumes at <code>FastPath</code> without running the router again. A crash in
<code>Start</code> resumes at <code>Start</code>, and the run reaches the router again on the way through.
</p>
<p>
If the initial state is a router and the run crashes before any other state is checkpointed,
<code>resume_from</code> finds no rows and returns an error. Start the workflow again instead.
</p>
</div>

<div class="diagram-frame">
<p class="diagram-label">Recovery footprint of a run that passes through a router</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 236" role="img" aria-labelledby="router-rec-title router-rec-desc">
<title id="router-rec-title">Checkpoint rows for a run through a router</title>
<desc id="router-rec-desc">A run goes Start, Classify, FastPath, Done. The checkpoint log holds one row per state entered: sequence 0 for Start, 1 for FastPath and 2 for Done. The Classify router state writes no row and consumes no sequence number.</desc>
<defs><marker id="router-rec-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M178,63 H214" marker-end="url(#router-rec-ah)"/>
<path class="e" d="M370,63 H406" marker-end="url(#router-rec-ah)"/>
<path class="e" d="M562,63 H598" marker-end="url(#router-rec-ah)"/>
<path class="e e-dash" d="M102,86 V142" marker-end="url(#router-rec-ah)"/>
<path class="e e-dash e-dim" d="M294,86 V142" marker-end="url(#router-rec-ah)"/>
<path class="e e-dash" d="M486,86 V142" marker-end="url(#router-rec-ah)"/>
<path class="e e-dash" d="M678,86 V142" marker-end="url(#router-rec-ah)"/>
<text class="t-mut" x="294" y="18">router state</text>
<rect class="n" x="26" y="40" width="152" height="46" rx="10"/>
<text class="t-strong" x="102" y="64">Start</text>
<rect class="n-hot" x="218" y="40" width="152" height="46" rx="10"/>
<text class="t-strong" x="294" y="64">Classify</text>
<rect class="n" x="410" y="40" width="152" height="46" rx="10"/>
<text class="t-strong" x="486" y="64">FastPath</text>
<rect class="n-ok" x="602" y="40" width="152" height="46" rx="10"/>
<text class="t-strong" x="678" y="64">Done</text>
<rect class="n-cop" x="26" y="146" width="152" height="46" rx="10"/>
<text class="t-code" x="102" y="170">seq 0 &#183; Start</text>
<rect class="n-ghost" x="218" y="146" width="152" height="46" rx="10"/>
<text class="t-mut t-warn" x="294" y="170">no row written</text>
<rect class="n-cop" x="410" y="146" width="152" height="46" rx="10"/>
<text class="t-code" x="486" y="170">seq 1 &#183; FastPath</text>
<rect class="n-cop" x="602" y="146" width="152" height="46" rx="10"/>
<text class="t-code" x="678" y="170">seq 2 &#183; Done</text>
<text class="t-mut ta-s" x="26" y="212">One row per state entered. Classify writes none and uses no sequence number.</text>
</svg>
</div>
</div>

<!-- Section: Explicit form -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit trait-impl form</h2>
<p>
To write the trait header yourself, for example for a generic impl, drop the <code>state = ...</code>
argument and put a bare <code>#[task::router]</code> on an <code>impl RouterTask&lt;...&gt; for ...</code>
block. Both forms emit the companion <code>impl Task&lt;...&gt; for T</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form: <code>#[task::router]</code> on a trait impl</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Classify, FastPath, SlowPath, Done }

struct Config { use_fast_path: bool }
#[resource]
impl Resource for Config {}

struct Classifier;

#[task::router]
impl RouterTask<Step> for Classifier {
    fn name(&self) -> std::borrow::Cow<'static, str> {
        "classifier".into()
    }

    async fn route(&self, res: &Resources) -> Result<Step, CanoError> {
        let config = res.get::<Config>()?;
        Ok(if config.use_fast_path {
            Step::FastPath
        } else {
            Step::SlowPath
        })
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to use RouterTask</h2>
<p>
Use a <code>RouterTask</code> for conditional branching that only reads: a config flag, the shape of
already-loaded data, a feature toggle. The branch then leaves no checkpoint row and burns no sequence
number.
</p>
<p>
If the branching logic also does work, such as writing to the store or calling an external system,
use a plain <a href="../task/">Task</a> and return the next state from <code>run</code>. That is the
"Conditional Routing Task" pattern on the <a href="../task/#patterns">Tasks</a> page, and its side
effects need a checkpoint.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
Run <code>cargo run --example router_task</code>.
</p>
</div>
</div>
