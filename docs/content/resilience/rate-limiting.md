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
<div class="diagram-frame">
<p class="diagram-label">A full bucket admits an instant burst, then the refill paces the rest</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 288" role="img">
<title>Nine calls arrive at once against a per_second(5) bucket: the first five are admitted instantly because the bucket starts full, and calls six through nine park on acquire until the lazy refill hands them a token every 200 milliseconds.</title>
<text class="t-code t-mut ta-s" x="16" y="20">RateLimiterPolicy::per_second(5)</text>
<text class="t-strong ta-s" x="16" y="72">admitted</text>
<text class="t-mut t-hot" x="147" y="46">#1–#5</text>
<text class="t-mut" x="260" y="46">#6</text>
<text class="t-mut" x="380" y="46">#7</text>
<text class="t-mut" x="500" y="46">#8</text>
<text class="t-mut" x="620" y="46">#9</text>
<rect class="band-hot" x="132" y="58" width="30" height="26" rx="6"/>
<rect class="band-hot" x="253" y="58" width="14" height="26" rx="4"/>
<rect class="band-hot" x="373" y="58" width="14" height="26" rx="4"/>
<rect class="band-hot" x="493" y="58" width="14" height="26" rx="4"/>
<rect class="band-hot" x="613" y="58" width="14" height="26" rx="4"/>
<text class="t-strong ta-s" x="16" y="126">parked</text>
<text class="t-code t-mut ta-s" x="16" y="146">acquire()</text>
<rect class="band" x="132" y="98" width="121" height="14" rx="4"/>
<rect class="band" x="132" y="118" width="241" height="14" rx="4"/>
<rect class="band" x="132" y="138" width="361" height="14" rx="4"/>
<rect class="band" x="132" y="158" width="481" height="14" rx="4"/>
<text class="t-mut ta-s" x="261" y="109">#6 · 200ms</text>
<text class="t-mut ta-s" x="381" y="129">#7 · 400ms</text>
<text class="t-mut ta-s" x="501" y="149">#8 · 600ms</text>
<text class="t-mut ta-s" x="621" y="169">#9 · 800ms</text>
<line class="axis" x1="132" y1="196" x2="740" y2="196"/>
<line class="tick" x1="132" y1="196" x2="132" y2="202"/>
<line class="tick" x1="260" y1="196" x2="260" y2="202"/>
<line class="tick" x1="380" y1="196" x2="380" y2="202"/>
<line class="tick" x1="500" y1="196" x2="500" y2="202"/>
<line class="tick" x1="620" y1="196" x2="620" y2="202"/>
<line class="tick" x1="740" y1="196" x2="740" y2="202"/>
<text class="t-mut" x="132" y="218">0</text>
<text class="t-mut" x="260" y="218">200ms</text>
<text class="t-mut" x="380" y="218">400ms</text>
<text class="t-mut" x="500" y="218">600ms</text>
<text class="t-mut" x="620" y="218">800ms</text>
<text class="t-mut" x="740" y="218">1s</text>
<text class="t-mut" x="436" y="246">the bucket starts full: 5 calls admitted instantly, then one token every 200ms</text>
<text class="t-mut" x="436" y="264">try_acquire() would return None for #6–#9 instead of parking</text>
</svg>
</div>
</div>
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
<div class="diagram-frame">
<p class="diagram-label">One acquisition: lazy refill, then shed or park</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 294" role="img">
<title>Every acquisition first refills the bucket lazily from the clock, then checks whether it holds enough tokens: if so it debits them and returns a Permit; if not, try_acquire returns None to shed the call while acquire sleeps for exactly the computed refill time and re-checks.</title>
<defs><marker id="acq-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<circle class="n" cx="10" cy="71" r="5"/>
<path class="e" d="M17,71 H40" marker-end="url(#acq-ah)"/>
<path class="e" d="M264,71 H294" marker-end="url(#acq-ah)"/>
<path class="e e-hot" d="M468,71 H556" marker-end="url(#acq-ah)"/>
<text class="t-mut t-hot" x="512" y="58">yes</text>
<path class="e" d="M383,98 V128 Q383,136 375,136 H278 Q270,136 270,144 V166" marker-end="url(#acq-ah)"/>
<path class="e" d="M383,98 V128 Q383,136 391,136 H556 Q564,136 564,144 V166" marker-end="url(#acq-ah)"/>
<text class="t-mut ta-s" x="395" y="116">no — bucket short</text>
<path class="e e-dim" d="M564,224 V248 Q564,256 556,256 H162 Q154,256 154,248 V102" marker-end="url(#acq-ah)"/>
<text class="t-mut" x="359" y="274">recompute the wait, re-check — best-effort wakeups, no queue</text>
<rect class="n-hot" x="44" y="44" width="220" height="54" rx="10"/>
<text class="t-strong" x="154" y="65">lazy refill</text>
<text class="t-code t-mut" x="154" y="85">elapsed × refill_per_sec</text>
<rect class="n" x="298" y="44" width="170" height="54" rx="10"/>
<text class="t-strong" x="383" y="65">enough tokens?</text>
<text class="t-code t-mut" x="383" y="85">tokens &gt;= cost</text>
<rect class="n-ok" x="560" y="44" width="190" height="54" rx="10"/>
<text class="t-strong" x="655" y="65">Permit</text>
<text class="t-code t-mut" x="655" y="85">tokens -= cost</text>
<rect class="n-warn" x="170" y="170" width="200" height="54" rx="10"/>
<text class="t-code" x="270" y="191">try_acquire() → None</text>
<text class="t-mut" x="270" y="211">shed the call</text>
<rect class="n" x="440" y="170" width="248" height="54" rx="10"/>
<text class="t-code" x="564" y="191">acquire(): sleep(time_until)</text>
<text class="t-mut" x="564" y="211">park until a token refills</text>
</svg>
</div>
</div>
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
