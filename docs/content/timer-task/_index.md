+++
title = "TimerTask"
description = "TimerTask in Cano - wait-then-transition with a single scheduled sleep for a relative duration or a monotonic instant."
template = "section.html"
+++

<div class="content-wrapper">
<h1>TimerTask</h1>
<p class="subtitle">Wait once, then transition — a single scheduled sleep, not a poll loop.</p>

<p>
A <code>TimerTask</code> decides <em>how long to wait</em>, the engine sleeps exactly once, and then
the task decides <em>where to go next</em>. There is no loop and no condition re-check: a 30-minute
timer schedules one <code>tokio::time::sleep</code> and wakes a single time. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../poll-task/">PollTask</a>,
<a href="../batch-task/">BatchTask</a>, <a href="../stepped-task/">SteppedTask</a>, and
<a href="../stream-task/">StreamTask</a>. A
<code>TimerTask</code> reads typed dependencies from <a href="../resources/">Resources</a> the same
way every other model does. New to Cano? Read <a href="../workflows/">Workflows</a> and
<a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance — <code>wait</code> returns a <code>TimerOutcome</code>, then <code>after_wait</code> routes</span>

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
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Stage>, CanoError> {
        Ok(TaskResult::Single(Stage::Done))
    }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">TimerTask vs PollTask</div>
<p>
Reach for a <code>TimerTask</code> when you just need to <strong>pause, then continue</strong> — a
cool-down, a debounce, or waking after a known delay. Reach for a
<a href="../poll-task/">PollTask</a> when you need to <strong>re-check a condition</strong> while
waiting. A timer wakes once; a poller loops.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#how-it-works">How the Timer Works</a></li>
<li><a href="#quick-start">Quick Start with <code>#[task::timer]</code></a></li>
<li><a href="#registering">Registering a Timer Task</a></li>
<li><a href="#outcomes">Duration vs Until</a></li>
<li><a href="#caps">Bounding the Wait</a></li>
<li><a href="#explicit">Explicit Trait-Impl Form</a></li>
<li><a href="#object-safe">Type-Erased Aliases</a></li>
<li><a href="#when-to-use">When to Use TimerTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the Timer Works</h2>

<div class="diagram-frame">
<p class="diagram-label">wait &rarr; sleep once &rarr; after_wait</p>
<div class="mermaid">
graph LR
A[wait] -->|"TimerOutcome"| B[sleep once]
B --> C[after_wait]
C --> D[Next State]
</div>
</div>

<p>
A timer implements two required methods. <code>async fn wait(&amp;self, res: &amp;Resources) -&gt; Result&lt;TimerOutcome, CanoError&gt;</code>
runs first and decides the delay; the engine then sleeps for that <code>TimerOutcome</code>; finally
<code>async fn after_wait(&amp;self, res: &amp;Resources) -&gt; Result&lt;TaskResult&lt;TState&gt;, CanoError&gt;</code>
runs and returns the next state.
</p>
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
<td>sleep for the relative duration <code>d</code>, then call <code>after_wait</code> (<code>Duration::ZERO</code> transitions immediately)</td>
</tr>
<tr>
<td><code>Until(instant)</code></td>
<td>sleep until the monotonic <code>instant</code>, then call <code>after_wait</code> (a past instant fires immediately)</td>
</tr>
</tbody>
</table>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start with <code>#[task::timer]</code></h2>
<p>
Attach <code>#[task::timer(state = MyState)]</code> to an inherent <code>impl</code> block. You write
<code>wait</code> and <code>after_wait</code>; the macro injects default bodies for <code>config</code>
and <code>name</code> if you don't, synthesises the
<code>impl TimerTask&lt;MyState&gt; for MyTimer</code> header, and emits a companion
<code>impl Task&lt;MyState&gt; for MyTimer</code> whose <code>run</code> schedules the sleep (via
<code>cano::task::timer::run_timer</code>) — so a timer is just an ordinary single-task state whose
<code>run</code> happens to sleep first. No engine changes.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form — <code>#[task::timer(state = ...)]</code> on an inherent impl</span>

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
        TaskConfig::minimal().with_attempt_timeout(Duration::from_secs(5))
    }

    async fn wait(&self, _res: &Resources) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(self.delay))
    }

    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Process))
    }
}
```
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a Timer Task</h2>
<p>
Register a timer with plain <code>Workflow::register</code> — to the FSM a timer is an ordinary
<code>Single</code> state, so there is no special builder method (the companion <code>impl Task</code>
the macro generated does the wait internally).
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a timer into a workflow</span>

```rust
use cano::prelude::*;

let workflow = Workflow::bare()
    .register(Step::CoolDown, CoolDown { delay: Duration::from_millis(50) })
    .register(Step::Process, ProcessResults)
    .add_exit_state(Step::Done);
```
</div>

<!-- Section: outcomes -->
<hr class="section-divider">
<h2 id="outcomes"><a href="#outcomes" class="anchor-link" aria-hidden="true">#</a>Duration vs Until</h2>
<div class="card-stack">
<div class="card">
<h3><code>TimerOutcome::Duration(d)</code></h3>
<p>Wait a <strong>relative</strong> delay measured from now. Best for cool-downs, debounces, and
fixed pacing between phases. <code>Duration::ZERO</code> skips the sleep entirely.</p>
</div>
<div class="card">
<h3><code>TimerOutcome::Until(instant)</code></h3>
<p>Wait until a monotonic <code>std::time::Instant</code> (a process-relative reading, <em>not</em>
a wall-clock/calendar time). Best for "wake after a known delay" hand-offs. An instant already in
the past fires immediately.</p>
</div>
</div>

<!-- Section: caps -->
<hr class="section-divider">
<h2 id="caps"><a href="#caps" class="anchor-link" aria-hidden="true">#</a>Bounding the Wait</h2>
<p>
A timer runs as a single dispatch attempt, so an <code>attempt_timeout</code> from
<code>config()</code> caps the <em>whole wait</em>. If the timer hasn't fired by the deadline, the
workflow engine cancels it and produces <code>CanoError::Timeout</code>.
</p>

<div class="callout callout-tip">
<p>The default <code>config()</code> is <code>TaskConfig::minimal()</code> — <strong>no retries</strong>.
A single scheduled sleep doesn't benefit from outer retry wrapping; attach an
<code>attempt_timeout</code> instead if you need a bound.</p>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit Trait-Impl Form</h2>
<p>
Prefer writing the trait header yourself? Put a bare <code>#[task::timer]</code> on an
<code>impl TimerTask&lt;...&gt; for ...</code> block. The companion <code>impl Task</code> is still
emitted; explicit method definitions always win.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form — <code>#[task::timer]</code> on a trait impl</span>

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
    async fn after_wait(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Process))
    }
}
```
</div>

<!-- Section: object-safe aliases -->
<hr class="section-divider">
<h2 id="object-safe"><a href="#object-safe" class="anchor-link" aria-hidden="true">#</a>Type-Erased Aliases</h2>
<table class="styled-table">
<thead>
<tr>
<th>Alias</th>
<th>Expands to</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>DynTimerTask&lt;TState, TResourceKey&gt;</code></td>
<td><code>dyn TimerTask&lt;TState, TResourceKey&gt;</code></td>
</tr>
<tr>
<td><code>TimerTaskObject&lt;TState, TResourceKey&gt;</code></td>
<td><code>Arc&lt;dyn TimerTask&lt;TState, TResourceKey&gt;&gt;</code></td>
</tr>
</tbody>
</table>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use TimerTask</h2>
<p>Reach for a <code>TimerTask</code> when:</p>
<ul>
<li>you need a <strong>fixed cool-down or debounce</strong> between workflow phases;</li>
<li>you want to <strong>wake after a known delay</strong> via <code>TimerOutcome::Until</code> with a
monotonic instant;</li>
<li>you want a <strong>deliberate pause</strong> that doesn't depend on polling an external
condition — if it does, use a <a href="../poll-task/">PollTask</a> instead.</li>
</ul>

<div class="callout callout-warning">
<span class="callout-label">Recovery</span>
<p>
A <code>TimerTask</code> is a checkpointed single-task state: a
<a href="../recovery/">resumed</a> run re-runs <code>wait()</code> from the start. A
<code>Duration(30 min)</code> timer that crashes at minute 29 sleeps the full 30 minutes again, and
an <code>Until</code> deadline built from <code>Instant::now()</code> is recomputed relative to the
new "now" — so it shifts forward by the downtime. <code>std::time::Instant</code> is monotonic and
process-relative, not a calendar time. If you need a delay that survives a crash, derive it inside
<code>wait()</code> from a timestamp you persist in <a href="../resources/">Resources</a> or a
checkpoint, not from <code>Instant::now()</code>.
</p>
</div>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
The crate ships a complete example — run it with <code>cargo run --example timer_task</code>.
</p>
</div>
</div>
