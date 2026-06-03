+++
title = "Scheduler Backoff, Trip State & Recovery"
description = "How the Cano scheduler backs off and trips a repeatedly-failing flow, overriding the BackoffPolicy, the Status variants, and recovering a tripped flow with reset_flow."
template = "page.html"
weight = 1
+++

<div class="content-wrapper">

<h1>Scheduler Backoff &amp; Trip State</h1>
<p class="subtitle">What happens when a scheduled flow keeps failing — and how to recover it.</p>

<p>
See <a href="../">Scheduler</a> for scheduling strategies and lifecycle. This page covers what the
scheduler does when a flow fails repeatedly.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#backoff-policy">Overriding the Default Policy</a></li>
<li><a href="#status-variants">Status Variants</a></li>
<li><a href="#recovery">Recovery via <code>reset_flow</code></a></li>
</ol>
</nav>
<hr class="section-divider">

<p>
A flow that fails repeatedly shouldn't keep re-firing on its base schedule, so <strong>every flow has a
<code>BackoffPolicy</code></strong>. After a failure the scheduler parks the flow in <code>Status::Backoff</code>
for a growing delay; with a <code>streak_limit</code> set it eventually <code>Tripped</code>s and stops
dispatching until you intervene. Each flow starts with <code>BackoffPolicy::default()</code> — call
<code>set_backoff</code> to use a different one.
</p>

<div class="callout callout-info">
<div class="callout-label">Heads-up: failure delays are at least 1s by default</div>
<p>
Because <code>BackoffPolicy::default()</code> has a <strong>1s</strong> initial delay and is applied to
<em>every</em> flow, a flow that fails waits ~1s before its next attempt (the <code>Every</code> loop sleeps
<code>max(interval, next_eligible - now)</code>) — even if its base interval is shorter. If you run a flow
on a sub-second interval and want fast retries after a failure, lower
<code>BackoffPolicy { initial: … }</code> via <code>set_backoff</code>.
</p>
</div>

<div class="callout callout-warning">
<div class="callout-label">Distinct from CircuitBreaker</div>
<p>
Flow-level <code>Tripped</code> is scoped to the scheduler and is separate from the task-level
<code>CanoError::CircuitOpen</code> emitted by a <a href="../../resilience/circuit-breakers/"><code>CircuitBreaker</code></a>.
The breaker gates a single task's call to a dependency; this policy gates the scheduler from re-firing
an entire flow.
</p>
</div>

<h3 id="backoff-policy"><a href="#backoff-policy" class="anchor-link" aria-hidden="true">#</a>Overriding the Default Policy</h3>
<p>
Register the workflow normally, then call <code>set_backoff</code> <strong>before</strong> <code>start()</code>.
The policy controls the initial delay after the first failure, the multiplier applied per
additional consecutive failure, a hard cap on the computed delay, jitter, and an optional streak limit.
</p>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FlowState { Start, Done }

#[derive(Clone)]
struct NoopTask;

#[task(state = FlowState)]
impl NoopTask {
    async fn run_bare(&self) -> Result<TaskResult<FlowState>, CanoError> {
        Ok(TaskResult::Single(FlowState::Done))
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let mut scheduler: Scheduler<FlowState> = Scheduler::new();

    let workflow = Workflow::new(Resources::new())
        .register(FlowState::Start, NoopTask)
        .add_exit_state(FlowState::Done);

    scheduler.every(
        "flaky",
        workflow,
        FlowState::Start,
        Duration::from_millis(200),
    )?;

    scheduler.set_backoff(
        "flaky",
        BackoffPolicy {
            initial: Duration::from_millis(300),
            multiplier: 2.0,
            max_delay: Duration::from_secs(2),
            jitter: 0.1,
            streak_limit: Some(3),
        },
    )?;

    let running = scheduler.start().await?;
    running.wait().await?;
    Ok(())
}

```

<p>
Computed delay is <code>initial * multiplier^(streak-1)</code>, capped at <code>max_delay</code>, then
multiplied by a random factor in <code>1 ± jitter</code>. The <code>Every</code> loop's sleep extends to
<code>max(interval, next_eligible - now)</code>, and the <code>Cron</code> loop suppresses ticks inside the
backoff window. <code>BackoffPolicy::default()</code> gives 1s initial, 2.0× multiplier, 5min cap,
0.1 jitter, and <strong>no trip limit</strong>. Use <code>BackoffPolicy::with_trip(n)</code> to ask for a
trip after <code>n</code> consecutive failures.
</p>

<h3 id="status-variants"><a href="#status-variants" class="anchor-link" aria-hidden="true">#</a>Status Variants</h3>
<p>
<code>Status</code> is <code>#[non_exhaustive]</code> — external <code>match</code> arms must include
a wildcard. The variants are:
</p>
<ul>
<li><code>Idle</code> — registered, never run or finished cleanly.</li>
<li><code>Running</code> — currently executing.</li>
<li><code>Completed</code> — last run reached an exit state.</li>
<li><code>Backoff { until, streak, last_error }</code> — last run errored; the flow is waiting until
<code>until</code> before its next dispatch, per its <code>BackoffPolicy</code>.</li>
<li><code>Tripped { streak, last_error }</code> — streak reached <code>streak_limit</code>; the scheduler
will not dispatch this flow again until <code>reset_flow</code> is called.</li>
</ul>
<p>
Outcome writes are atomic: a single write decides this run's terminal status (<code>Completed</code> on
success, otherwise <code>Backoff</code> or <code>Tripped</code>), so observers never see a transient
intermediate state. <code>FlowInfo</code> exposes <code>failure_streak</code> and
<code>next_eligible</code> for observability.
</p>

<h3 id="recovery"><a href="#recovery" class="anchor-link" aria-hidden="true">#</a>Recovery via <code>reset_flow</code></h3>
<p>
A <code>Tripped</code> flow stays parked until you clear it. <code>RunningScheduler::reset_flow(id)</code>
clears the failure streak and <code>next_eligible</code>, and (when the flow is not currently running) sets
the status back to <code>Idle</code>. Manual <code>trigger()</code> is rejected on a tripped flow — call
<code>reset_flow</code> first.
</p>

```rust
let snap = running.status("flaky").await.expect("flow exists");
if matches!(snap.status, Status::Tripped { .. }) {
    running.reset_flow("flaky").await?;
}

```

<p>
See the <code>scheduler_backoff</code> example
(<code>cargo run --example scheduler_backoff --features scheduler</code>) for an end-to-end walk-through
that exercises the trip and recovery path.
</p>
</div>
