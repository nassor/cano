+++
title = "PollTask"
description = "PollTask in Cano - wait-until loops without blocking a thread, with adaptive backoff and a per-poll error policy."
template = "section.html"
+++

<div class="content-wrapper">
<h1>PollTask</h1>
<p class="subtitle">"Wait-until" loops without blocking a thread.</p>

<p>
A <code>PollTask</code> calls its <code>poll</code> method until the work it waits on is ready. Each
call returns either "ready, here is the next state" or "not yet, try again in <em>n</em>
milliseconds". The wait is an async sleep, so no thread blocks. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../timer-task/">TimerTask</a>,
<a href="../batch-task/">BatchTask</a>, <a href="../stepped-task/">SteppedTask</a>,
<a href="../stream-task/">StreamTask</a> and <a href="../suspend-task/">SuspendTask</a>, and it reads typed dependencies from
<a href="../resources/">Resources</a> like the rest. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance: <code>poll</code> returns <code>Ready</code> or <code>Pending { delay_ms }</code></span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { AwaitJob, Done }

struct AwaitJob;

#[task::poll(state = Stage)]
impl AwaitJob {
    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Stage>, CanoError> {
        let board = res.get::<Blackboard>()?;
        // Missing key or false keeps polling without propagating KeyNotFound.
        match board.get::<bool>("job_done") {
            Ok(true) => Ok(PollOutcome::Ready(Stage::Done)),
            _        => Ok(PollOutcome::Pending { delay_ms: 250 }), // async sleep, then poll again
        }
    }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">Retries</div>
<p>
A poll task defaults to <code>TaskConfig::new()</code>: one attempt, no retries. The loop already
keeps trying, so an outer retry rarely helps. To tolerate transient errors inside the loop, use the
<code>on_poll_error</code> policy.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#how-it-works">How the poll loop works</a></li>
<li><a href="#quick-start">Quick start with <code>#[task::poll]</code></a></li>
<li><a href="#registering">Registering a poll task</a></li>
<li><a href="#error-policy">The poll error policy</a></li>
<li><a href="#caps">Bounding the loop</a></li>
<li><a href="#explicit">Explicit trait-impl form</a></li>
<li><a href="#when-to-use">When to use PollTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the poll loop works</h2>

<div class="diagram-frame">
<p class="diagram-label">The poll loop: poll, sleep, poll again until Ready</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 196" role="img" aria-labelledby="poll-loop-title poll-loop-desc">
<title id="poll-loop-title">The poll loop</title>
<desc id="poll-loop-desc">The task calls poll. When poll returns Pending with a delay in milliseconds, the engine sleeps that long and calls poll again. The loop repeats until poll returns Ready with a state, which moves the workflow to the next state.</desc>
<defs><marker id="poll-loop-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-hot" d="M100,58 V32 Q100,24 108,24 H617 Q625,24 625,32 V53" marker-end="url(#poll-loop-ah)"/>
<text class="t-code t-hot" x="362" y="41">Ready(state)</text>
<rect class="n-hot" x="40" y="58" width="120" height="46" rx="10"/>
<text class="t-strong" x="100" y="82">poll</text>
<rect class="n" x="330" y="58" width="170" height="46" rx="10"/>
<text x="415" y="82">sleep delay_ms</text>
<rect class="n-ok" x="560" y="58" width="130" height="46" rx="10"/>
<text x="625" y="82">Next State</text>
<path class="e" d="M160,81 H326" marker-end="url(#poll-loop-ah)"/>
<text class="t-code t-mut" x="243" y="64">Pending { delay_ms }</text>
<path class="e e-dim" d="M415,104 V138 Q415,146 407,146 H108 Q100,146 100,138 V107" marker-end="url(#poll-loop-ah)"/>
<text class="t-mut" x="257" y="162">repeat until Ready</text>
</svg>
</div>
</div>

<p>
The required method is <code>async fn poll(&amp;self, res: &amp;Resources) -&gt; Result&lt;PollOutcome&lt;TState&gt;, CanoError&gt;</code>.
Its return value drives the loop:
</p>
<table class="styled-table">
<thead>
<tr>
<th>Variant</th>
<th>Effect</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>Ready(state)</code></td>
<td>Ends the loop and forwards <code>state</code> to the workflow.</td>
</tr>
<tr>
<td><code>Pending { delay_ms }</code></td>
<td>Sleeps <code>delay_ms</code> milliseconds, then polls again. With <code>0</code> the loop yields to the executor and polls again.</td>
</tr>
</tbody>
</table>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick start with <code>#[task::poll]</code></h2>
<p>
Attach <code>#[task::poll(state = MyState)]</code> to an inherent <code>impl</code> block and write
<code>poll</code>. The macro generates the <code>impl PollTask&lt;MyState&gt;</code> header, a
<code>Registrable</code> impl so the struct goes straight into <code>register</code>, and a companion
<code>impl Task&lt;MyState&gt;</code> whose <code>run</code> drives the loop. <code>poll</code> may omit
the <code>res</code> parameter when it needs no resources.
</p>
<p>One method is required and three are optional:</p>
<dl class="key-list">
<dt><code>async fn poll(&amp;self, res: &amp;Resources) -&gt; Result&lt;PollOutcome&lt;TState&gt;, CanoError&gt;</code> <small>required</small></dt>
<dd>Called once per iteration.</dd>
<dt><code>fn on_poll_error(&amp;self) -&gt; PollErrorPolicy</code> <small>default <code>PollErrorPolicy::FailFast</code></small></dt>
<dd>What the loop does when <code>poll</code> returns <code>Err</code>.</dd>
<dt><code>fn config(&amp;self) -&gt; TaskConfig</code> <small>default <code>TaskConfig::new()</code>, one attempt</small></dt>
<dd>Retry and timeout settings for the dispatch. An <code>attempt_timeout</code> here bounds the whole loop.</dd>
<dt><code>fn name(&amp;self) -&gt; Cow&lt;'static, str&gt;</code> <small>default: the type name</small></dt>
<dd>Identifies the task in logs and observers.</dd>
</dl>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form: <code>#[task::poll(state = ...)]</code> on an inherent impl</span>

```rust
use cano::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { AwaitJob, Process, Done }

struct Job { ticks: Arc<AtomicU32>, total: u32 }
#[resource]
impl Resource for Job {}

struct AwaitJob;

#[task::poll(state = Step)]
impl AwaitJob {
    fn config(&self) -> TaskConfig {
        // cap the whole wait at 30s
        TaskConfig::new().with_attempt_timeout(Duration::from_secs(30))
    }

    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let job = res.get::<Job>()?;
        let done = job.ticks.load(Ordering::Relaxed);
        if done >= job.total {
            Ok(PollOutcome::Ready(Step::Process))
        } else {
            Ok(PollOutcome::Pending { delay_ms: 200 })
        }
    }
}
```
</div>

<!-- Section: registering -->
<hr class="section-divider">
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a poll task</h2>
<p>
Register a poll task with <code>Workflow::register</code>, the same call every other model uses. To
the workflow it is an ordinary single-task state that loops internally.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a poll task into a workflow</span>

```rust
use cano::prelude::*;

let workflow = Workflow::new(resources)
    .register(Step::AwaitJob, AwaitJob)   // ordinary register; it loops internally
    .register(Step::Process, ProcessResults)
    .add_exit_state(Step::Done);
```
</div>

<!-- Section: error policy -->
<hr class="section-divider">
<h2 id="error-policy"><a href="#error-policy" class="anchor-link" aria-hidden="true">#</a>The poll error policy</h2>
<p>
Override <code>fn on_poll_error(&amp;self) -&gt; PollErrorPolicy</code> to decide what happens when a
<code>poll</code> call returns <code>Err</code>. The default is <code>FailFast</code>.
</p>
<div class="card-stack">
<div class="card">
<h3><code>PollErrorPolicy::FailFast</code></h3>
<p>The first <code>Err</code> from <code>poll</code> ends the task and propagates.</p>
</div>
<div class="card">
<h3><code>PollErrorPolicy::RetryOnError { max_errors }</code></h3>
<p>Tolerates up to <code>max_errors</code> <em>consecutive</em> errors, then fails with the last one.
A <code>Pending</code> result resets the counter, so errors spread across the loop do not add up.</p>
</div>
</div>

<div class="code-block">
<span class="code-block-label">Tolerating a few transient errors mid-loop</span>

```rust
#[task::poll(state = Step)]
impl AwaitJob {
    fn on_poll_error(&self) -> PollErrorPolicy {
        // Up to 3 consecutive Errs from `poll` are absorbed, and a
        // Pending resets the streak. The 4th in a row fails.
        PollErrorPolicy::RetryOnError { max_errors: 3 }
    }

    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let job = res.get::<Job>()?;
        let status = job.probe().await?;     // a flaky probe that may Err
        Ok(match status {
            JobStatus::Finished => PollOutcome::Ready(Step::Process),
            JobStatus::Running  => PollOutcome::Pending { delay_ms: 200 },
        })
    }
}
```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example poll_retry_on_error</code>. It runs a <code>poll</code>
that errs in streaks under <code>RetryOnError { max_errors: 3 }</code>: a streak of 3 survives and a
streak of 4 fails the workflow.</p>
</div>

<!-- Section: caps -->
<hr class="section-divider">
<h2 id="caps"><a href="#caps" class="anchor-link" aria-hidden="true">#</a>Bounding the loop</h2>
<p>
A <code>PollTask</code> has no built-in iteration or time cap, so with nothing set it polls forever.
For a wall-clock bound, return <code>TaskConfig::new().with_attempt_timeout(dur)</code> from
<code>config()</code>. The poll loop is one dispatch attempt, so the timeout caps the whole loop, and
the engine returns <code>CanoError::Timeout</code> when it expires.
</p>

<div class="callout callout-warning">
<span class="callout-label">Cooperative timeout</span>
<p>
The timeout covers the whole loop and drops the task at its next <code>.await</code>, so a
<code>poll</code> that hangs on an await is cancelled too. A <code>poll</code> body that blocks a
thread without awaiting cannot be interrupted.
</p>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit trait-impl form</h2>
<p>
To write the trait header yourself, put a bare <code>#[task::poll]</code> on an
<code>impl PollTask&lt;...&gt; for ...</code> block. The macro still emits the companion
<code>impl Task</code>.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form: <code>#[task::poll]</code> on a trait impl</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { AwaitJob, Process, Done }

#[derive(Resource)]
struct Job {
    url: String,
}

impl Job {
    async fn is_done(&self) -> Result<bool, CanoError> {
        // Probe an external endpoint or database using self.url.
        Ok(!self.url.is_empty())
    }
}

struct AwaitJob;

#[task::poll]
impl PollTask<Step> for AwaitJob {
    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let job = res.get::<Job>()?;
        if job.is_done().await? {
            // Exits the poll loop and enters Step::Process.
            Ok(PollOutcome::Ready(Step::Process))
        } else {
            // Sleeps asynchronously without blocking the executor thread.
            Ok(PollOutcome::Pending { delay_ms: 250 })
        }
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to use PollTask</h2>
<p>
Use a <code>PollTask</code> to wait for an external job to finish, or for a database row to appear or a
flag to flip, with a ready or pending result per check instead of hand-written
<code>tokio::time::sleep</code> bookkeeping. If you only need to pause once for a known delay, use a
<a href="../timer-task/">TimerTask</a>.
</p>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
Run <code>cargo run --example poll_task</code>.
</p>
</div>
</div>
