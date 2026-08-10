+++
title = "RouterTask"
description = "RouterTask in Cano - side-effect-free branching that leaves no recovery footprint."
template = "section.html"
+++

<div class="content-wrapper">
<h1>RouterTask</h1>
<p class="subtitle">Side-effect-free branching for your workflows.</p>

<p>
A <code>RouterTask</code> is a processing model for <em>pure routing</em>: it reads
<a href="../resources/">Resources</a> and returns the next <code>TaskResult&lt;TState&gt;</code> —
and nothing else. No store mutations, no external I/O, no side effects. Because re-running a router
is free, the workflow engine records <strong>no checkpoint row</strong> for it — see
<a href="../recovery/">Crash Recovery</a> for what that means. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../poll-task/">PollTask</a>, <a href="../timer-task/">TimerTask</a>,
<a href="../batch-task/">BatchTask</a>, <a href="../stepped-task/">SteppedTask</a>, and
<a href="../stream-task/">StreamTask</a>.
</p>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
A router's job is to <em>decide</em>, not to <em>do</em>. Keep <code>route</code> free of side
effects and the engine treats branching as cost-free on resume — there is no recovery row to write,
nothing to replay. If your branching logic also needs to write something, reach for a plain
<a href="../task/">Task</a> instead.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#quick-start">Quick Start with <code>#[task::router]</code></a></li>
<li><a href="#registering">Registering a Router</a></li>
<li><a href="#explicit">Explicit Trait-Impl Form</a></li>
<li><a href="#object-safe">Type-Erased Aliases</a></li>
<li><a href="#when-to-use">When to Use RouterTask</a></li>
</ol>
</nav>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start with <code>#[task::router]</code></h2>
<p>
The required method is <code>async fn route(&amp;self, res: &amp;Resources) -&gt; Result&lt;TaskResult&lt;TState&gt;, CanoError&gt;</code>.
Everything else has a default: <code>fn config(&amp;self) -&gt; TaskConfig</code> (defaults to
<code>TaskConfig::default()</code>) and <code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code>
(defaults to the type name). The recommended form attaches <code>#[task::router(state = MyState)]</code>
to an inherent <code>impl</code> block — the macro synthesises the
<code>impl RouterTask&lt;MyState&gt; for MyRouter</code> header and emits a companion
<code>impl Task&lt;MyState&gt; for MyRouter</code> so the same struct can also be passed to
<code>register</code> if you ever want the checkpoint-recording behaviour back.
</p>

<div class="diagram-frame">
<p class="diagram-label">route() reads, decides, and returns &mdash; nothing else</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 330" role="img">
<title>A router dispatch: the Classify state calls route(&amp;Resources), which reads the config out of Resources without writing anything and returns TaskResult::Single — Step::FastPath when the flag is set, Step::SlowPath otherwise; because the decision has no side effects the engine writes no CheckpointRow for the state.</title>
<defs><marker id="route-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M393,70 V112" marker-end="url(#route-ah)"/>
<text class="t-mut ta-e" x="383" y="93">reads, never writes</text>
<path class="e" d="M156,143 H296" marker-end="url(#route-ah)"/>
<text class="t-code t-mut" x="226" y="127">register_router</text>
<path class="e" d="M486,143 H536 Q544,143 544,135 V75 Q544,67 552,67 H586" marker-end="url(#route-ah)"/>
<text class="t-code t-mut ta-s" x="554" y="110">Single(Step::FastPath)</text>
<path class="e" d="M486,143 H536 Q544,143 544,151 V211 Q544,219 552,219 H586" marker-end="url(#route-ah)"/>
<text class="t-code t-mut ta-s" x="554" y="176">Single(Step::SlowPath)</text>
<path class="e e-dash e-dim" d="M393,170 V248" marker-end="url(#route-ah)"/>
<text class="t-mut ta-s" x="403" y="205">no side effects</text>
<rect class="n-cop" x="298" y="16" width="190" height="54" rx="10"/>
<text class="t-strong" x="393" y="34">Resources</text>
<text class="t-code" x="393" y="53">config.use_fast_path</text>
<rect class="n" x="16" y="120" width="140" height="46" rx="10"/>
<text class="t-code" x="86" y="144">Step::Classify</text>
<rect class="n-hot" x="300" y="116" width="186" height="54" rx="10"/>
<text class="t-code t-strong" x="393" y="134">route(&amp;Resources)</text>
<text class="t-code t-mut" x="393" y="153">&#8594; TaskResult&lt;Step&gt;</text>
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
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form — <code>#[task::router(state = ...)]</code> on an inherent impl</span>

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
    async fn route(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let config = res.get::<Config, _>("config")?;
        if config.use_fast_path {
            Ok(TaskResult::Single(Step::FastPath))
        } else {
            Ok(TaskResult::Single(Step::SlowPath))
        }
    }
}
```
</div>

<div class="callout callout-warning">
<div class="callout-label">Heads-up</div>
<p>
<code>route</code> must return <code>TaskResult::Single</code> — a router state is dispatched through
the single-task path, and returning <code>TaskResult::Split</code> from it fails at run time with
<code>CanoError::Workflow</code> (&ldquo;use <code>register_split()</code> for split tasks&rdquo;). To fan
out into parallel states, route <em>to</em> a state registered with
<a href="../split-join/"><code>register_split()</code></a> instead.
</p>
</div>

<!-- Section: Registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a Router</h2>
<p>
Register a router with <code>Workflow::register_router(state, task)</code> — <strong>not</strong>
<code>register</code>. The engine dispatches it exactly like an ordinary single-task state, but with
one difference: it writes <strong>no <code>CheckpointRow</code></strong> for the router state and
<strong>consumes no checkpoint sequence number</strong>. A router has no side effects, so re-running
it on resume costs nothing — there is nothing to recover, so there is nothing to record.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a router into a workflow</span>

```rust
use cano::prelude::*;

let workflow = Workflow::new(resources)
    .register_router(Step::Classify, Classifier)   // router state — leaves no checkpoint row
    .register(Step::FastPath, FastProcessor)
    .register(Step::SlowPath, SlowProcessor)
    .add_exit_state(Step::Done);
```
</div>

<div class="callout callout-info">
<div class="callout-label">Recovery interplay</div>
<p>
On a <a href="../recovery/">checkpointed</a> workflow, the recovery log skips router states entirely:
a <code>Start → Classify (router) → FastPath → Done</code> run records rows for <code>Start</code>,
<code>FastPath</code>, and <code>Done</code> — but not <code>Classify</code>. If a crash happens
inside <code>FastProcessor</code>, <code>resume_from</code> re-enters at <code>FastPath</code>, having
never needed to "remember" the routing decision — it just runs the router again on the way through if
the resume point happens to land before it.
</p>
</div>

<div class="diagram-frame">
<p class="diagram-label">Recovery footprint of a run that passes through a router</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 236" role="img">
<title>A Start to Classify to FastPath to Done run: the checkpoint log holds one row per state entered — sequence 0 for Start, 1 for FastPath, 2 for Done — while the Classify router state writes no row and consumes no sequence number.</title>
<defs><marker id="rrec-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M178,63 H214" marker-end="url(#rrec-ah)"/>
<path class="e" d="M370,63 H406" marker-end="url(#rrec-ah)"/>
<path class="e" d="M562,63 H598" marker-end="url(#rrec-ah)"/>
<path class="e e-dash" d="M102,86 V142" marker-end="url(#rrec-ah)"/>
<path class="e e-dash e-dim" d="M294,86 V142" marker-end="url(#rrec-ah)"/>
<path class="e e-dash" d="M486,86 V142" marker-end="url(#rrec-ah)"/>
<path class="e e-dash" d="M678,86 V142" marker-end="url(#rrec-ah)"/>
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
<text class="t-mut ta-s" x="26" y="212">Checkpoint log: one row per state entered &#8212; Classify writes none and burns no sequence number.</text>
</svg>
</div>
</div>

<!-- Section: Explicit form -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit Trait-Impl Form</h2>
<p>
If you prefer to write the trait header yourself — e.g. for a generic impl, or a custom resource-key
type — drop the <code>state = ...</code> argument and put a bare <code>#[task::router]</code> on a
<code>impl RouterTask&lt;...&gt; for ...</code> block. Both forms emit the companion
<code>impl Task&lt;...&gt; for T</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form — <code>#[task::router]</code> on a trait impl</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Classify, FastPath, SlowPath, Done }

struct Classifier;

#[task::router]
impl RouterTask<Step> for Classifier {
    fn name(&self) -> std::borrow::Cow<'static, str> {
        "classifier".into()
    }

    async fn route(&self, res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        let config = res.get::<Config, _>("config")?;
        Ok(TaskResult::Single(if config.use_fast_path {
            Step::FastPath
        } else {
            Step::SlowPath
        }))
    }
}
```
</div>

<!-- Section: object-safe aliases -->
<hr class="section-divider">
<h2 id="object-safe"><a href="#object-safe" class="anchor-link" aria-hidden="true">#</a>Type-Erased Aliases</h2>
<p>
For dynamic dispatch — keeping a heterogeneous collection of routers, building one at runtime — Cano
exports two aliases mirroring <code>DynTask</code> / <code>TaskObject</code>:
</p>
<table class="styled-table">
<thead>
<tr>
<th>Alias</th>
<th>Expands to</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>DynRouterTask&lt;TState, TResourceKey&gt;</code></td>
<td><code>dyn RouterTask&lt;TState, TResourceKey&gt;</code></td>
</tr>
<tr>
<td><code>RouterTaskObject&lt;TState, TResourceKey&gt;</code></td>
<td><code>Arc&lt;dyn RouterTask&lt;TState, TResourceKey&gt;&gt;</code></td>
</tr>
</tbody>
</table>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use RouterTask</h2>
<p>Reach for a <code>RouterTask</code> when:</p>
<ul>
<li>you need conditional branching and the decision has <strong>no side effects</strong> — routing
on a config flag, on the shape of already-loaded data, on a feature toggle;</li>
<li>you want the workflow to leave <strong>no recovery footprint</strong> for the branch (no
checkpoint row, no sequence number burned).</li>
</ul>
<p>
If your branching logic <em>also</em> does work — writing to the store, calling an external system —
use a plain <a href="../task/">Task</a> and <code>match</code>-and-return the next state from
<code>run</code>; that's the "Conditional Routing Task" pattern documented on the
<a href="../task/#patterns">Tasks</a> page, and those side effects <em>do</em> need a checkpoint, so a
plain task is the right tool.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
The crate ships a complete example — run it with <code>cargo run --example router_task</code>.
</p>
</div>
</div>
