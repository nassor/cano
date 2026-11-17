+++
title = "TimerTask"
description = "TimerTask in Cano - wait-then-transition with a single scheduled sleep for a relative duration or a monotonic instant."
template = "section.html"
+++

<div class="content-wrapper">
<h1>TimerTask</h1>
<p class="subtitle">Wait once, then transition: a single scheduled sleep, not a poll loop.</p>

<p>
A <code>TimerTask</code> decides how long to wait, the engine sleeps once, and then the task decides
where to go next. There is no loop and no condition re-check: a 30-minute timer schedules one
<code>tokio::time::sleep</code> and wakes a single time. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>,
<a href="../batch-task/">BatchTask</a>, <a href="../stepped-task/">SteppedTask</a>,
<a href="../stream-task/">StreamTask</a> and <a href="../suspend-task/">SuspendTask</a>, and it reads typed dependencies from
<a href="../resources/">Resources</a> like the rest. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance: <code>wait</code> returns a <code>TimerOutcome</code>, then <code>after_wait</code> routes</span>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { CoolDown, Done }

struct CoolDown;

#[task::timer(state = Stage)]
impl CoolDown {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::from_secs(30))) // one scheduled wake-up
    }
    async fn after_wait(&self, _res: &Resources) -> Result<Stage, CanoError> {
        Ok(Stage::Done)
    }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">TimerTask or PollTask?</div>
<p>
Use a <code>TimerTask</code> to pause and then continue: a cool-down, a debounce, a wake-up after a
known delay. Use a <a href="../poll-task/">PollTask</a> to re-check a condition while waiting.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#how-it-works">How the timer works</a></li>
<li><a href="#quick-start">Quick start with <code>#[task::timer]</code></a></li>
<li><a href="#registering">Registering a timer task</a></li>
<li><a href="#outcomes">Duration or Until</a></li>
<li><a href="#caps">Bounding the wait</a></li>
<li><a href="#explicit">Explicit trait-impl form</a></li>
<li><a href="#when-to-use">When to use TimerTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the timer works</h2>

<div class="diagram-frame">
<p class="diagram-label">wait, sleep once, after_wait</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 726 204" role="img" aria-labelledby="timer-flow-title timer-flow-desc">
<title id="timer-flow-title">A TimerTask waits exactly once</title>
<desc id="timer-flow-desc">The task calls wait, which returns a TimerOutcome: either Duration(d) or Until(instant). The engine schedules a single sleep for that duration or until that instant, with one wake-up and no loop. Then after_wait runs, and the state it returns moves the workflow on.</desc>
<defs><marker id="timer-flow-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e" d="M130,51 H276" marker-end="url(#timer-flow-ah)"/>
<text class="t-code" x="205" y="34">TimerOutcome</text>
<path class="e" d="M440,51 H576" marker-end="url(#timer-flow-ah)"/>
<text class="t-mut" x="510" y="34">wakes once</text>
<rect class="n" x="26" y="28" width="104" height="46" rx="10"/>
<text class="t-code" x="78" y="52">wait</text>
<rect class="n-hot" x="280" y="28" width="160" height="46" rx="10"/>
<text class="t-strong" x="360" y="52">sleep once</text>
<text class="t-code t-mut" x="360" y="96">Duration(d) &#183; Until(instant)</text>
<text class="t-mut" x="360" y="116">exactly one wake-up, no loop</text>
<rect class="n" x="580" y="28" width="120" height="46" rx="10"/>
<text class="t-code" x="640" y="52">after_wait</text>
<path class="e" d="M640,74 V129" marker-end="url(#timer-flow-ah)"/>
<text class="t-code t-mut ta-e" x="630" y="102">Ok(state)</text>
<rect class="n-ok" x="582" y="134" width="116" height="46" rx="10"/>
<text x="640" y="158">Next State</text>
</svg>
</div>
</div>

<p>
A timer has two required methods. <code>wait</code> runs first and decides the delay, the engine
sleeps for the returned <code>TimerOutcome</code>, and <code>after_wait</code> runs and returns the
next state. An <code>Err</code> from <code>wait</code> ends the task before any sleep.
</p>
<dl class="key-list">
<dt><code>async fn wait(&amp;self, res: &amp;Resources) -&gt; Result&lt;TimerOutcome, CanoError&gt;</code> <small>required</small></dt>
<dd>Called once, before the sleep. Returns how long to wait.</dd>
<dt><code>async fn after_wait(&amp;self, res: &amp;Resources) -&gt; Result&lt;TState, CanoError&gt;</code> <small>required</small></dt>
<dd>Called once, after the sleep. Returns the next state.</dd>
<dt><code>fn config(&amp;self) -&gt; TaskConfig</code> <small>default <code>TaskConfig::new()</code>, one attempt</small></dt>
<dd>Retry and timeout settings for the dispatch. An <code>attempt_timeout</code> here bounds the wait.</dd>
<dt><code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code> <small>default: the type name</small></dt>
<dd>Identifies the task in logs and observers.</dd>
</dl>

<table class="styled-table">
<thead>
<tr>
<th><code>TimerOutcome</code> variant</th>
<th>Effect</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>Duration(d)</code></td>
<td>Sleeps for the relative duration <code>d</code>, then calls <code>after_wait</code>. <code>Duration::ZERO</code> transitions immediately.</td>
</tr>
<tr>
<td><code>Until(instant)</code></td>
<td>Sleeps until the monotonic <code>instant</code>, then calls <code>after_wait</code>. A past instant fires immediately.</td>
</tr>
</tbody>
</table>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start with <code>#[task::timer]</code></h2>
<p>
Attach <code>#[task::timer(state = MyState)]</code> to an inherent <code>impl</code> block and write
<code>wait</code> and <code>after_wait</code>. The macro generates the
<code>impl TimerTask&lt;MyState&gt;</code> header, a <code>Registrable</code> impl so the struct goes
straight into <code>register</code>, and a companion <code>impl Task&lt;MyState&gt;</code> whose
<code>run</code> sleeps between the two calls. Either method may omit the <code>res</code> parameter
when it needs no resources.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form: <code>#[task::timer(state = ...)]</code> on an inherent impl</span>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { CoolDown, Process, Done }

struct CoolDown { delay: Duration }

#[task::timer(state = Step)]
impl CoolDown {
    fn config(&self) -> TaskConfig {
        // cap the whole wait at 5s
        TaskConfig::new().with_attempt_timeout(Duration::from_secs(5))
    }

    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(self.delay))
    }

    async fn after_wait(&self, _res: &Resources) -> Result<Step, CanoError> {
        Ok(Step::Process)
    }
}
```
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a timer task</h2>
<p>
Register a timer with <code>Workflow::register</code>, the same call every other model uses. To the
workflow it is an ordinary single-task state that sleeps inside <code>run</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a timer into a workflow</span>

```rust
use cano::prelude::*;
use std::time::Duration;

let workflow = Workflow::bare()
    .register(Step::CoolDown, CoolDown { delay: Duration::from_millis(50) })
    .register(Step::Process, ProcessResults)
    .add_exit_state(Step::Done);
```
</div>

<!-- Section: outcomes -->
<hr class="section-divider">
<h2 id="outcomes"><a href="#outcomes" class="anchor-link" aria-hidden="true">#</a>Duration or Until</h2>
<div class="card-stack">
<div class="card">
<h3><code>TimerOutcome::Duration(d)</code></h3>
<p>Waits a relative delay measured from now. Use it for cool-downs, debounces, and fixed pacing
between phases. <code>Duration::ZERO</code> skips the sleep.</p>
</div>
<div class="card">
<h3><code>TimerOutcome::Until(instant)</code></h3>
<p>Waits until a monotonic <code>std::time::Instant</code>. An <code>Instant</code> is a
process-relative clock reading, not a calendar time. An instant already in the past fires
immediately.</p>
</div>
</div>

<!-- Section: caps -->
<hr class="section-divider">
<h2 id="caps"><a href="#caps" class="anchor-link" aria-hidden="true">#</a>Bounding the wait</h2>
<p>
A timer runs as a single dispatch attempt, so an <code>attempt_timeout</code> from
<code>config()</code> caps the whole wait. If the timer has not fired by the deadline, the engine
cancels it and returns <code>CanoError::Timeout</code>.
</p>

<div class="callout callout-tip">
<p>The default <code>config()</code> is <code>TaskConfig::new()</code>: no retries. A single
scheduled sleep gains nothing from an outer retry, so attach an <code>attempt_timeout</code> when
you need a bound.</p>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit trait-impl form</h2>
<p>
To write the trait header yourself, put a bare <code>#[task::timer]</code> on an
<code>impl TimerTask&lt;...&gt; for ...</code> block. The macro still emits the companion
<code>impl Task</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form: <code>#[task::timer]</code> on a trait impl</span>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { CoolDown, Process, Done }

struct CoolDown;

#[task::timer]
impl TimerTask<Step> for CoolDown {
    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::from_secs(30)))
    }
    async fn after_wait(&self, _res: &Resources) -> Result<Step, CanoError> {
        Ok(Step::Process)
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to use TimerTask</h2>
<p>
Use a <code>TimerTask</code> for a fixed cool-down or debounce between workflow phases, for a
wake-up at a known instant with <code>TimerOutcome::Until</code>, or for any deliberate pause that
does not depend on an external condition. If the pause does depend on one, use a
<a href="../poll-task/">PollTask</a>.
</p>

<div class="callout callout-warning">
<span class="callout-label">Recovery</span>
<p>
A <code>TimerTask</code> is a checkpointed single-task state, so a <a href="../recovery/">resumed</a>
run calls <code>wait()</code> again from the start. A <code>Duration</code> of 30 minutes that
crashes at minute 29 sleeps the full 30 minutes again. An <code>Until</code> instant built from
<code>Instant::now()</code> is recomputed against the new "now", so it shifts forward by the
downtime.
</p>
<p>
For a delay that survives a crash, derive it inside <code>wait()</code> from a timestamp you persist
in <a href="../resources/">Resources</a> or a checkpoint.
</p>
</div>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
Run <code>cargo run --example timer_task</code>.
</p>
</div>
</div>
