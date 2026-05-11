+++
title = "PollTask"
description = "PollTask in Cano - wait-until loops without blocking a thread, with adaptive backoff and a per-poll error policy."
template = "page.html"
+++

<div class="content-wrapper">
<h1>PollTask</h1>
<p class="subtitle">"Wait-until" loops without blocking a thread.</p>

<p>
A <code>PollTask</code> repeatedly calls its <code>poll</code> method until the work it's waiting on
is ready. Each call returns either "ready, here's the next state" or "not yet, try again in
<em>n</em> milliseconds" — an async sleep, not a blocked thread. It is one of the
<a href="../task/">Task</a> family of processing models, alongside <a href="../nodes/">Node</a>,
<a href="../router-task/">RouterTask</a>, <a href="../batch-task/">BatchTask</a>, and
<a href="../stepped-task/">SteppedTask</a>. A <code>PollTask</code> reads typed dependencies from
<a href="../resources/">Resources</a> the same way every other model does.
</p>

<div class="callout callout-info">
<div class="callout-label">Key concept</div>
<p>
The poll loop <em>is</em> the resilience mechanism. That's why <code>config()</code> defaults to
<code>TaskConfig::minimal()</code> — <strong>no retries</strong>: wrapping a poll loop in an outer
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
<li><a href="#object-safe">Type-Erased Aliases</a></li>
<li><a href="#when-to-use">When to Use PollTask</a></li>
</ol>
</nav>

<!-- Section: how it works -->
<hr class="section-divider">
<h2 id="how-it-works"><a href="#how-it-works" class="anchor-link" aria-hidden="true">#</a>How the Poll Loop Works</h2>

<div class="mermaid">
graph LR
A[poll] -->|"Pending { delay_ms }"| B[sleep delay_ms]
B --> A
A -->|"Ready(result)"| C[Next State]
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
<td><code>Ready(result)</code></td>
<td>ends the loop and forwards <code>result</code> (a <code>TaskResult&lt;TState&gt;</code>) to the FSM</td>
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
<code>poll</code>; the macro injects default bodies for any of <code>config</code>, <code>name</code>,
or <code>on_poll_error</code> you don't write, synthesises the
<code>impl PollTask&lt;MyState&gt; for MyPoller</code> header, and emits a companion
<code>impl Task&lt;MyState&gt; for MyPoller</code> whose <code>run</code> drives the loop (via
<code>cano::task::poll::run_poll_loop</code>) — so a poll task is just an ordinary single-task state
whose <code>run</code> happens to loop. No engine changes.
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
        TaskConfig::minimal().with_attempt_timeout(Duration::from_secs(30))
    }

    async fn poll(&self, res: &Resources) -> Result<PollOutcome<Step>, CanoError> {
        let job = res.get::<Job, _>("job")?;
        let done = job.ticks.load(Ordering::Relaxed);
        if done >= job.total {
            Ok(PollOutcome::Ready(TaskResult::Single(Step::Process)))
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
Register a poll task with plain <code>Workflow::register</code> — there is no special builder method,
because to the FSM it's an ordinary <code>Single</code> state. The companion
<code>impl Task</code> the macro generated runs the loop internally.
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
        let job = res.get::<Job, _>("job")?;
        let status = job.probe().await?;     // a flaky probe — may Err
        Ok(match status {
            JobStatus::Finished => PollOutcome::Ready(TaskResult::Single(Step::Process)),
            JobStatus::Running  => PollOutcome::Pending { delay_ms: 200 },
        })
    }
}
```
</div>

<!-- Section: caps -->
<hr class="section-divider">
<h2 id="caps"><a href="#caps" class="anchor-link" aria-hidden="true">#</a>Bounding the Loop</h2>
<p>
A <code>PollTask</code> has <strong>no built-in iteration or time cap</strong>. With nothing set it
polls forever — which is legitimate ("wait for shutdown"). For a wall-clock bound, return
<code>TaskConfig::minimal().with_attempt_timeout(dur)</code> from <code>config()</code>: since a poll
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
        let job = res.get::<Job, _>("job")?;
        if job.is_done() {
            Ok(PollOutcome::Ready(TaskResult::Single(Step::Process)))
        } else {
            Ok(PollOutcome::Pending { delay_ms: 250 })
        }
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
<td><code>DynPollTask&lt;TState, TResourceKey&gt;</code></td>
<td><code>dyn PollTask&lt;TState, TResourceKey&gt;</code></td>
</tr>
<tr>
<td><code>PollTaskObject&lt;TState, TResourceKey&gt;</code></td>
<td><code>Arc&lt;dyn PollTask&lt;TState, TResourceKey&gt;&gt;</code></td>
</tr>
</tbody>
</table>

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
