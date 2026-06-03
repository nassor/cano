+++
title = "Rate Limiting"
description = "Cano rate limiting: RateLimiterPolicy, try_acquire/acquire, token bucket vs fixed window, weighted cost, and multi-level limiting."
template = "page.html"
weight = 2
+++

<div class="content-wrapper">

<h1>Rate Limiting</h1>
<p class="subtitle">Pace or shed calls to a rate-sensitive dependency.</p>

<p>
This page covers the rate limiter in depth. See <a href="../">Resilience</a> for how it composes with
the other primitives, and <a href="../circuit-breakers/">Circuit Breakers</a> for the related
"dependency is down" case.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#rl-policy"><code>RateLimiterPolicy</code></a></li>
<li><a href="#rl-acquire">Acquiring: <code>try_acquire</code> / <code>acquire</code></a></li>
<li><a href="#rl-windowed">Token bucket vs fixed window</a></li>
<li><a href="#rl-weighted">Weighted cost</a></li>
<li><a href="#rl-multi">Multi-level limiting</a></li>
</ol>
</nav>
<hr class="section-divider">

<p>
A <a href="../circuit-breakers/">circuit breaker</a> stops calls to a dependency that's <em>down</em>.
A <code>RateLimiter</code> paces calls to a dependency that's <em>up but rate-sensitive</em> — a
third-party API with a per-second quota, a database, an LLM endpoint billed per request. It smooths
bursty traffic into a steady rate the downstream can absorb.
</p>
<p>
It's a <strong>token bucket</strong>: a bucket holds fractional <code>tokens</code>; each acquisition
spends one; tokens replenish at a fixed rate up to a capacity. Refill is <strong>lazy</strong> — there's
no background task. Every acquire reads a single <code>Instant</code>, adds
<code>elapsed × refill_per_sec</code> tokens (capped at capacity), then decides. A workflow that never
builds a limiter pays nothing. The bucket starts <strong>full</strong>, so a burst of up to
<code>capacity</code> calls is admitted instantly before sustained traffic settles to the refill rate.
</p>
<p>
Like a breaker, a limiter is cheap to clone (it's an <code>Arc</code> inside) — <strong>share one
<code>Arc&lt;RateLimiter&gt;</code> across every task that draws on the same quota</strong> so the budget
is enforced globally, including across tasks running in parallel inside a
<a href="../../split-join/">split/join</a> state. Internally it's a synchronous
<code>parking_lot::Mutex</code> with no awaits held across the critical section.
</p>

<h3 id="rl-policy"><a href="#rl-policy" class="anchor-link" aria-hidden="true">#</a><code>RateLimiterPolicy</code></h3>
<p>
Build a policy with <code>RateLimiterPolicy::per_second(n)</code> (or <code>::new(tokens, period)</code>
for an arbitrary window) and tune it with the <code>with_max_tokens</code> / <code>with_burst</code>
builders. Total bucket capacity is <code>max_tokens + burst</code>.
</p>
<table class="styled-table">
<thead><tr><th>Field</th><th>Type</th><th>Meaning</th></tr></thead>
<tbody>
<tr><td><code>max_tokens</code></td><td><code>u32</code></td><td>Steady-state bucket ceiling — and the size of the instantaneous burst a fresh limiter admits, since the bucket starts full. Defaults to <code>tokens</code> (one period's worth).</td></tr>
<tr><td><code>tokens_per_period</code></td><td><code>u32</code></td><td>Tokens added per <code>refill_period</code>.</td></tr>
<tr><td><code>refill_period</code></td><td><code>Duration</code></td><td>How long it takes to add <code>tokens_per_period</code> tokens. <code>per_second(n)</code> sets this to one second.</td></tr>
<tr><td><code>burst</code></td><td><code>u32</code></td><td>Extra capacity above <code>max_tokens</code> for short spikes. Defaults to <code>0</code>.</td></tr>
</tbody>
</table>
<p>
<code>RateLimiter::new</code> <strong>panics</strong> on a misconfigured policy at construction:
<code>max_tokens == 0</code> (a zero-capacity bucket could never admit a call) or a zero refill rate
(<code>tokens_per_period == 0</code> or a zero <code>refill_period</code> — the bucket would never
replenish). Both are programmer errors, caught before any task runs.
</p>

<h3 id="rl-acquire"><a href="#rl-acquire" class="anchor-link" aria-hidden="true">#</a>Acquiring: <code>try_acquire</code> / <code>acquire</code></h3>
<ul>
<li><code>try_acquire() -&gt; Option&lt;Permit&gt;</code> — non-blocking. <code>Some</code> if a token was
available (and consumes it), <code>None</code> if the bucket is empty. Use it to <em>shed</em> load.</li>
<li><code>acquire().await -&gt; Permit</code> — if the bucket is empty it computes exactly how long until
the next token refills, <code>tokio::time::sleep</code>s that long, and retries. Use it to <em>pace</em>
work.</li>
</ul>
<p>
The returned <code>Permit</code> is a lightweight RAII marker for the call's scope. Unlike a
<a href="../circuit-breakers/#cb-permits">circuit-breaker permit</a> (which records a success/failure outcome) or a semaphore
permit (which returns capacity on drop), a token-bucket permit's <strong>drop is a no-op</strong> — the
token was already spent at acquisition and the bucket refills on the clock, not on release.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Call, Done }

#[derive(Clone)]
struct CallUpstream { limiter: Arc<RateLimiter> }

#[task(state = Step)]
impl CallUpstream {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        // Park until the shared budget admits this call, then proceed.
        let _permit = self.limiter.acquire().await;
        // ... call the rate-sensitive dependency ...
        Ok(TaskResult::Single(Step::Done))
    }
}

// 20 req/s, shared across every task that constructs from this Arc.
let limiter = Arc::new(RateLimiter::new(RateLimiterPolicy::per_second(20)));
let workflow = Workflow::bare()
    .register(Step::Call, CallUpstream { limiter: Arc::clone(&limiter) })
    .add_exit_state(Step::Done);
```

<p>
<code>RateLimiter</code> also implements <code>Resource</code> (no-op lifecycle), so instead of threading
the <code>Arc</code> into each task you can register it once in <a href="../../resources/">Resources</a> and
look it up by key inside the task body — handy when several tasks share one quota.
</p>

<h3 id="rl-windowed"><a href="#rl-windowed" class="anchor-link" aria-hidden="true">#</a>Token bucket vs fixed window</h3>
<p>
The token bucket is a faithful <strong>governor</strong> — it keeps you under a long-run rate and
smooths bursts — but it is not a faithful <strong>model</strong> of a "resets-at-a-boundary" quota
like a usage quota's "N per 5 hours, resets at 14:00." It drips capacity back continuously and has no
reset instant to display. When you need that shape, use <code>WindowedRateLimiter</code>: a
fixed-window counter that admits the full quota at once, resets as a <em>step</em> at the boundary,
and exposes <code>used()</code> / <code>remaining()</code> / <code>resets_at()</code>. It resets
lazily (no background task) and, like the bucket, is cheap to clone and implements
<code>Resource</code>.
</p>
<table class="styled-table">
<thead><tr><th></th><th><code>RateLimiter</code> (token bucket)</th><th><code>WindowedRateLimiter</code> (fixed window)</th></tr></thead>
<tbody>
<tr><td>Replenishment</td><td>continuous drip at the refill rate</td><td>step reset at the boundary</td></tr>
<tr><td>After exhaustion</td><td>one more unit every <code>period/quota</code></td><td>zero until the reset, then the full quota</td></tr>
<tr><td><code>resets_at</code></td><td>none (boundary-less)</td><td>a displayable instant</td></tr>
<tr><td>Best for</td><td>pacing outbound load under a rate</td><td>mirroring a quota with a reset time</td></tr>
</tbody>
</table>

<h3 id="rl-weighted"><a href="#rl-weighted" class="anchor-link" aria-hidden="true">#</a>Weighted cost</h3>
<p>
Both limiters meter <strong>weighted units</strong>: <code>try_acquire_n(cost)</code> /
<code>acquire_n(cost)</code> consume <code>cost</code> units instead of one (the no-argument
<code>try_acquire</code> / <code>acquire</code> are <code>_n(1)</code>). A request-count limit uses
<code>cost = 1</code>; a usage/token budget uses the call's cost (e.g. <code>1500</code> tokens).
<code>tokens_available()</code> / <code>time_until(cost)</code> expose the live state for
observability and retry-after.
</p>

<h3 id="rl-multi"><a href="#rl-multi" class="anchor-link" aria-hidden="true">#</a>Multi-level limiting (several tiers at once)</h3>
<p>
Real-world API limits often stack: a 5-hour cap <em>and</em> a weekly cap <em>and</em> a separate
weekly cap for a single endpoint. <code>MultiRateLimiter</code> enforces them together — a request is admitted
only if <strong>every</strong> applicable tier has room. Each tier is any <code>Meter</code> (a
<code>RateLimiter</code> or a <code>WindowedRateLimiter</code>, mixed freely) with its own
<code>cost</code>, so a request-count tier and a token-budget tier can share one gate.
</p>
<p>
The acquisition is <strong>atomic with no leak</strong>: it reserves each tier in turn, and if any
tier rejects it drops the reservations gathered so far — <em>refunding</em> their units — so a
partially-passing attempt never burns budget on the tiers that admitted it. (This is why a
<code>Reservation</code>'s drop refunds, unlike a committed <code>Permit</code>.) At most one tier's
lock is held at a time, so there is no deadlock. On rejection it reports <strong>which</strong> tier
blocked and the retry-after, as <code>CanoError::RateLimited { tier, retry_after }</code>.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;
use std::time::Duration;

let five_hour: Arc<dyn Meter> =
    Arc::new(WindowedRateLimiter::new(WindowPolicy::per_hours(500, 5)));
let weekly: Arc<dyn Meter> =
    Arc::new(WindowedRateLimiter::new(WindowPolicy::per_days(5_000, 7)));
let opus_weekly: Arc<dyn Meter> =
    Arc::new(WindowedRateLimiter::new(WindowPolicy::per_days(200, 7)));
// A usage/token budget metered in tokens, smoothed by a bucket.
let tokens: Arc<dyn Meter> = Arc::new(RateLimiter::new(
    RateLimiterPolicy::new(1_000_000, Duration::from_secs(60)).with_max_tokens(1_000_000),
));

let limiter = MultiRateLimiter::new()
    .with_tier("5h", five_hour, 1)
    .with_tier("weekly", weekly, 1)
    .with_tier("opus_weekly", opus_weekly, 1)
    .with_tier("tokens", tokens, 1500); // this call costs 1500 tokens

// Shed-load: which tier blocked, and for how long?
match limiter.try_acquire() {
    Ok(_permit) => { /* all tiers had room; proceed */ }
    Err(CanoError::RateLimited { tier, retry_after }) => {
        eprintln!("blocked by `{tier}`, retry after {retry_after:?}");
    }
    Err(_) => unreachable!(),
}
```

<p>
For a per-request subset — e.g. a non-Opus request that should skip the model-scoped tier — use
<code>try_acquire_for(&amp;["5h", "weekly", "tokens"])</code> (or the async
<code>acquire_for</code>). A tier with <code>cost = 0</code> is inert (never blocks, never debited),
another way to disable one conditionally.
</p>

<div class="callout callout-tip">
<p>Runnable examples: <code>cargo run --example rate_limiter</code> — two spawned workers share one
<code>5 req/s</code> bucket (timestamps land at ~200ms intervals). <code>cargo run --example
rate_limiter_multi</code> — a 5h + weekly + per-model + token-budget gate showing shed-load,
the blocking-tier report, zero-leak on rejection, per-request tier selection, and async parking.</p>
</div>
</div>
