+++
title = "RouterTask"
description = "RouterTask in Cano - side-effect-free branching that leaves no recovery footprint."
template = "page.html"
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
<a href="../task/">Task</a> family of processing models, alongside <a href="../nodes/">Node</a>,
<a href="../poll-task/">PollTask</a>, <a href="../batch-task/">BatchTask</a>, and
<a href="../stepped-task/">SteppedTask</a>.
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

<div class="callout callout-tip">
<div class="callout-label">Tip</div>
<p>
<code>route</code> may return <code>TaskResult::Split</code> as well as <code>TaskResult::Single</code>,
so a router can fan a workflow out into parallel states. See <a href="../split-join/">Split &amp; Join</a>
for what happens next.
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
