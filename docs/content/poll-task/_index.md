+++
title = "PollTask"
description = "PollTask in Cano - wait-until loops without blocking a thread, with adaptive backoff and a per-poll error policy."
template = "section.html"
+++

<div class="content-wrapper">
<h1>PollTask</h1>
<p class="subtitle">"Wait-until" loops without blocking a thread.</p>

<p>
A <code>PollTask</code> repeatedly calls its <code>poll</code> method until the work it's waiting on
is ready. Each call returns either "ready, here's the next state" or "not yet, try again in
<em>n</em> milliseconds" — an async sleep, not a blocked thread. It is one of the
<a href="../task/">Task</a> family of processing models, alongside
<a href="../router-task/">RouterTask</a>, <a href="../timer-task/">TimerTask</a>,
<a href="../batch-task/">BatchTask</a>, <a href="../stepped-task/">SteppedTask</a>, and
<a href="../stream-task/">StreamTask</a>. A
<code>PollTask</code> reads typed dependencies from
<a href="../resources/">Resources</a> the same way every other model does. New to Cano? Read
<a href="../workflows/">Workflows</a> and <a href="../resources/">Resources</a> first.
</p>

<div class="code-block">
<span class="code-block-label">At a glance — <code>poll</code> returns <code>Ready</code> or <code>Pending { delay_ms }</code></span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Stage { AwaitJob, Done }

struct AwaitJob;

#[task::poll(state = Stage)]
impl AwaitJob {
    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Stage>, CanoError> {
        let store = res.get::<MemoryStore>()?;
        match store.get::<bool>("job_done") {
            Ok(true) => Ok(PollOutcome::Ready(Stage::Done)),
            _        => Ok(PollOutcome::Pending { delay_ms: 250 }), // async sleep, then poll again
        }
    }
}
```
</div>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
The poll loop <em>is</em> the resilience mechanism. Like every model, a poll task defaults to
<code>TaskConfig::new()</code> — <strong>no retries</strong> — and wrapping a poll loop in an outer
retry rarely makes sense, since the loop already keeps trying. For tolerating transient errors
<em>inside</em> the loop, use the per-poll <code>on_poll_error</code> policy instead.
</p>
</div>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#how-it-works">How the Poll Loop Works</a></li>
<li><a href="#quick-start">Quick Start with <code>#[task::poll]</code></a></li>
<li><a href="#registering">Registering a Poll Task</a></li>
<li><a href="#error-policy">The Poll Error Policy</a></li>
<li><a href="#caps">Bounding the Loop</a></li>
<li><a href="#explicit">Explicit Trait-Impl Form</a></li>
<li><a href="#when-to-use">When to Use PollTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the Poll Loop Works</h2>

<div class="diagram-frame">
<p class="diagram-label">The poll loop: poll &rarr; sleep &rarr; poll &hellip; until Ready</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 720 196" role="img">
<title>The poll loop: poll returns Pending with a delay, the engine sleeps that long and polls again; Ready(state) moves the workflow to the next state.</title>
<defs><marker id="poll-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<path class="e e-hot" d="M100,58 V32 Q100,24 108,24 H617 Q625,24 625,32 V53" marker-end="url(#poll-ah)"/>
<text class="t-code t-hot" x="362" y="41">Ready(state)</text>
<rect class="n-hot" x="40" y="58" width="120" height="46" rx="10"/>
<text class="t-strong" x="100" y="82">poll</text>
<rect class="n" x="330" y="58" width="170" height="46" rx="10"/>
<text x="415" y="82">sleep delay_ms</text>
<rect class="n-ok" x="560" y="58" width="130" height="46" rx="10"/>
<text x="625" y="82">Next State</text>
<path class="e" d="M160,81 H326" marker-end="url(#poll-ah)"/>
<text class="t-code t-mut" x="243" y="64">Pending { delay_ms }</text>
<path class="e e-dim" d="M415,104 V138 Q415,146 407,146 H108 Q100,146 100,138 V107" marker-end="url(#poll-ah)"/>
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
<th><code>PollOutcome&lt;TState&gt;</code> variant</th>
<th>Effect</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>Ready(state)</code></td>
<td>ends the loop and forwards <code>state</code> (the next <code>TState</code>) to the FSM</td>
</tr>
<tr>
<td><code>Pending { delay_ms }</code></td>
<td>sleeps <code>delay_ms</code> milliseconds (per-poll adaptive backoff — use <code>0</code> for no delay), then polls again</td>
</tr>
</tbody>
</table>

<!-- Section: Quick Start -->
<hr class="section-divider">
<h2 id="quick-start"><a href="#quick-start" class="anchor-link" aria-hidden="true">#</a>Quick Start with <code>#[task::poll]</code></h2>
<p>
Attach <code>#[task::poll(state = MyState)]</code> to an inherent <code>impl</code> block. You write
<code>poll</code>; <code>config</code>, <code>name</code> and <code>on_poll_error</code> fall back to the
trait defaults if you don't write them. The macro synthesises the
<code>impl PollTask&lt;MyState&gt; for MyPoller</code> header, implements
<code>cano::workflow::Registrable</code> so the struct can go straight into <code>register</code>, and emits a
companion <code>impl Task&lt;MyState&gt; for MyPoller</code> whose <code>run</code> drives the loop (via
<code>cano::task::poll::run_poll_loop</code>) — so a poll task is just an ordinary single-task state
whose <code>run</code> happens to loop. <code>poll</code> may also be written without the <code>res</code>
parameter when it needs no resources.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9889;</span> Inference form — <code>#[task::poll(state = ...)]</code> on an inherent impl</span>

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
<h2 id="registering"><a href="#registering" class="anchor-link" aria-hidden="true">#</a>Registering a Poll Task</h2>
<p>
Register a poll task with <code>Workflow::register</code>, the same call every other model uses. To the
FSM it's an ordinary single-task state: the companion <code>impl Task</code> the macro generated
runs the loop internally.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Wiring a poll task into a workflow</span>

```rust
use cano::prelude::*;

let workflow = Workflow::new(resources)
    .register(Step::AwaitJob, AwaitJob)   // ordinary register — it just loops internally
    .register(Step::Process, ProcessResults)
    .add_exit_state(Step::Done);
```
</div>

<!-- Section: error policy -->
<hr class="section-divider">
<h2 id="error-policy"><a href="#error-policy" class="anchor-link" aria-hidden="true">#</a>The Poll Error Policy</h2>
<p>
Override <code>fn on_poll_error(&amp;self) -&gt; PollErrorPolicy</code> to decide what happens when a
<code>poll</code> call returns <code>Err</code>. The enum is <code>#[non_exhaustive]</code> with
<code>Default = FailFast</code>:
</p>
<div class="card-stack">
<div class="card">
<h3><code>PollErrorPolicy::FailFast</code></h3>
<p>The default. The first <code>Err</code> from <code>poll</code> propagates immediately and ends
the task.</p>
</div>
<div class="card">
<h3><code>PollErrorPolicy::RetryOnError { max_errors }</code></h3>
<p>Tolerates up to <code>max_errors</code> <em>consecutive</em> errors before failing. A successful
<code>Pending</code> resets the consecutive-error counter, so transient flakiness mid-loop is
absorbed.</p>
</div>
</div>

<div class="code-block">
<span class="code-block-label">Tolerating a few transient errors mid-loop</span>

```rust
#[task::poll(state = Step)]
impl AwaitJob {
    fn on_poll_error(&self) -> PollErrorPolicy {
        // up to 3 consecutive Errs from `poll` are absorbed; a
        // successful Pending resets the streak. The 4th in a row fails.
        PollErrorPolicy::RetryOnError { max_errors: 3 }
    }

    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let job = res.get::<Job>()?;
        let status = job.probe().await?;     // a flaky probe — may Err
        Ok(match status {
            JobStatus::Finished => PollOutcome::Ready(Step::Process),
            JobStatus::Running  => PollOutcome::Pending { delay_ms: 200 },
        })
    }
}
```
</div>

<div class="callout callout-tip">
<p>Runnable example: <code>cargo run --example poll_retry_on_error</code> — a <code>poll</code> that
intermittently <code>Err</code>s under <code>RetryOnError { max_errors: 3 }</code>: it absorbs short
error streaks (and a <code>Pending</code> resets the counter), and fails once a streak exceeds the cap.</p>
</div>

<!-- Section: caps -->
<hr class="section-divider">
<h2 id="caps"><a href="#caps" class="anchor-link" aria-hidden="true">#</a>Bounding the Loop</h2>
<p>
A <code>PollTask</code> has <strong>no built-in iteration or time cap</strong>. With nothing set it
polls forever — which is legitimate ("wait for shutdown"). For a wall-clock bound, return
<code>TaskConfig::new().with_attempt_timeout(dur)</code> from <code>config()</code>: since a poll
task runs as a single dispatch attempt, that timeout caps the <em>whole loop</em>. The workflow
engine enforces it, producing <code>CanoError::Timeout</code> on expiry.
</p>

<div class="callout callout-warning">
<span class="callout-label">Important</span>
<p>
<code>with_attempt_timeout</code> bounds the entire poll loop, not a single <code>poll</code> call —
because the loop is one dispatch attempt. There's no per-iteration deadline; if a single
<code>poll</code> call can itself hang, guard <em>that</em> with <code>tokio::time::timeout</code>
inside your <code>poll</code> body.
</p>
</div>

<!-- Section: explicit -->
<hr class="section-divider">
<h2 id="explicit"><a href="#explicit" class="anchor-link" aria-hidden="true">#</a>Explicit Trait-Impl Form</h2>
<p>
Prefer writing the trait header yourself? Put a bare <code>#[task::poll]</code> on an
<code>impl PollTask&lt;...&gt; for ...</code> block. The companion <code>impl Task</code> is still
emitted; explicit method definitions always win.
</p>

<div class="code-block">
<span class="code-block-label"><span class="label-icon">&#9998;</span> Explicit form — <code>#[task::poll]</code> on a trait impl</span>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { AwaitJob, Process, Done }

struct AwaitJob;

#[task::poll]
impl PollTask<Step> for AwaitJob {
    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let job = res.get::<Job>()?;
        if job.is_done() {
            Ok(PollOutcome::Ready(Step::Process))
        } else {
            Ok(PollOutcome::Pending { delay_ms: 250 })
        }
    }
}
```
</div>

<!-- Section: When to use -->
<hr class="section-divider">
<h2 id="when-to-use"><a href="#when-to-use" class="anchor-link" aria-hidden="true">#</a>When to Use PollTask</h2>
<p>Reach for a <code>PollTask</code> when:</p>
<ul>
<li>you're <strong>waiting on an external job</strong> — a queue to drain, a batch to finish, an API
to report "done";</li>
<li>you're <strong>polling for a state change</strong> — a database row to appear, a flag to flip;</li>
<li>you want an <strong>adaptive backoff loop</strong> with a clean ready/pending contract instead of
hand-rolled <code>tokio::time::sleep</code> bookkeeping.</li>
</ul>

<div class="callout callout-tip">
<div class="callout-label">Runnable example</div>
<p>
The crate ships a complete example — run it with <code>cargo run --example poll_task</code>.
</p>
</div>
</div>
