+++
title = "Rate limiting"
description = "Cano rate limiting: RateLimiterPolicy, try_acquire and acquire, token bucket versus fixed window, weighted cost, and multi-tier limiting."
template = "page.html"
weight = 2
+++

<div class="content-wrapper">

<h1>Rate limiting</h1>
<p class="subtitle">Pace or shed calls to a rate-sensitive dependency.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.14.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>

<p>
This page is a how-to and reference for the rate limiters. See <a href="../">Resilience</a> for how
they nest with the other primitives, and <a href="../circuit-breakers/">Circuit breakers</a> for the
case where the dependency is down.
</p>

<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#rl-policy"><code>RateLimiterPolicy</code></a></li>
<li><a href="#rl-acquire">Acquiring: <code>try_acquire</code> and <code>acquire</code></a></li>
<li><a href="#rl-windowed">Token bucket and fixed window</a></li>
<li><a href="#rl-weighted">Weighted cost</a></li>
<li><a href="#rl-multi">Multi-tier limiting</a></li>
</ol>
</nav>
<hr class="section-divider">

<p>
A <a href="../circuit-breakers/">circuit breaker</a> stops calls to a dependency that is down. A
<code>RateLimiter</code> paces calls to a dependency that is up but rate-sensitive: a third-party API
with a per-second quota, a database, an LLM endpoint billed per request. It turns bursty traffic into
a steady rate the downstream can absorb.
</p>
<p>
It is a token bucket. The bucket holds fractional tokens, each acquisition spends one, and tokens
refill at a fixed rate up to a capacity. Refill is lazy: no background task runs. Each acquire reads
one <code>Instant</code>, adds <code>elapsed × refill_per_sec</code> tokens (capped at capacity), then
decides. The bucket starts full, so a fresh limiter admits a burst of up to <code>capacity</code>
calls at once before traffic settles to the refill rate.
</p>
<div class="diagram-frame">
<p class="diagram-label">A full bucket admits an instant burst, then the refill paces the rest</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 0 780 288" role="img" aria-labelledby="ratelimit-burst-title ratelimit-burst-desc">
<title id="ratelimit-burst-title">Nine calls against a bucket of five tokens per second</title>
<desc id="ratelimit-burst-desc">Nine calls arrive at once against a limiter built with RateLimiterPolicy::per_second(5). The first five are admitted instantly because the bucket starts full. Calls six to nine park on acquire and are admitted one by one as the lazy refill hands out a token every 200 milliseconds, at 200, 400, 600 and 800 milliseconds. try_acquire would return None for calls six to nine instead of parking.</desc>
<text class="t-code t-mut ta-s" x="16" y="20">RateLimiterPolicy::per_second(5)</text>
<text class="t-strong ta-s" x="16" y="72">admitted</text>
<text class="t-mut t-hot" x="147" y="46">#1 to #5</text>
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
<text class="t-mut" x="436" y="264">try_acquire() would return None for #6 to #9 instead of parking</text>
</svg>
</div>
</div>
<p>
A limiter is cheap to clone, and clones share one bucket. Give every task that draws on the same
quota a clone of one limiter, so the budget is enforced globally, including across tasks running in
parallel inside a <a href="../../split-join/">split/join</a> state. The bucket sits behind a
a mutex (<code>parking_lot::Mutex</code> under <code>runtime-tokio</code>, <code>std::sync::Mutex</code> under <code>wasm</code>) that is never held across an await.
</p>

<h3 id="rl-policy"><a href="#rl-policy" class="anchor-link" aria-hidden="true">#</a><code>RateLimiterPolicy</code></h3>
<p>
Build a policy with <code>RateLimiterPolicy::per_second(n)</code>, or <code>::new(tokens, period)</code>
for another period. Tune it with <code>with_max_tokens</code> and <code>with_burst</code>. Total bucket
capacity is <code>max_tokens + burst</code>. The fields are listed with their type and default.
</p>
<dl class="key-list">
<dt><code>max_tokens: u32</code> <small>default <code>tokens</code></small></dt>
<dd>Steady-state bucket ceiling, and the size of the instant burst a fresh limiter admits. The default is one period's worth of tokens.</dd>

<dt><code>tokens_per_period: u32</code> <small>set by <code>new</code></small></dt>
<dd>Tokens added per <code>refill_period</code>.</dd>

<dt><code>refill_period: Duration</code> <small>set by <code>new</code>; <code>per_second</code> uses one second</small></dt>
<dd>How long it takes to add <code>tokens_per_period</code> tokens.</dd>

<dt><code>burst: u32</code> <small>default <code>0</code></small></dt>
<dd>Extra capacity above <code>max_tokens</code> for short spikes.</dd>
</dl>
<p>
<code>RateLimiter::new</code> panics on a policy that could never work: <code>max_tokens == 0</code>
(the bucket could never admit a call), or a zero refill rate (<code>tokens_per_period == 0</code> or a
zero <code>refill_period</code>, so the bucket would never refill). These are programmer errors,
caught before any task runs.
</p>

<h3 id="rl-acquire"><a href="#rl-acquire" class="anchor-link" aria-hidden="true">#</a>Acquiring: <code>try_acquire</code> and <code>acquire</code></h3>
<dl class="key-list">
<dt><code>try_acquire(&amp;self) -&gt; Option&lt;Permit&gt;</code> <small>non-blocking</small></dt>
<dd>Returns <code>Some</code> and spends a token if one is available, <code>None</code> if the bucket is empty. Use it to shed load.</dd>

<dt><code>acquire(&amp;self) -&gt; Permit</code> <small>async</small></dt>
<dd>If the bucket is empty, computes how long until the next token refills, sleeps that long via Cano's runtime sleep (<code>tokio::time::sleep</code> on native, WASI Preview 3 monotonic clock on <code>wasm</code>), and tries again. Use it to pace work.</dd>
</dl>
<div class="diagram-frame">
<p class="diagram-label">One acquisition: lazy refill, then shed or park</p>
<div class="cd-wrap">
<svg class="cd" viewBox="0 28 780 269" role="img" aria-labelledby="ratelimit-acquire-title ratelimit-acquire-desc">
<title id="ratelimit-acquire-title">One acquisition against a token bucket</title>
<desc id="ratelimit-acquire-desc">Every acquisition first refills the bucket lazily from the clock, then checks whether it holds at least the requested number of tokens. If it does, it debits them and returns a Permit. If it does not, try_acquire returns None so the caller can shed the call, while acquire sleeps for the computed time until enough tokens refill, then goes back to the refill step and checks again.</desc>
<defs><marker id="ratelimit-acquire-ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="context-stroke"/></marker></defs>
<circle class="n" cx="10" cy="71" r="5"/>
<path class="e" d="M17,71 H40" marker-end="url(#ratelimit-acquire-ah)"/>
<path class="e" d="M264,71 H294" marker-end="url(#ratelimit-acquire-ah)"/>
<path class="e e-hot" d="M468,71 H556" marker-end="url(#ratelimit-acquire-ah)"/>
<text class="t-mut t-hot" x="512" y="58">yes</text>
<path class="e" d="M383,98 V128 Q383,136 375,136 H278 Q270,136 270,144 V166" marker-end="url(#ratelimit-acquire-ah)"/>
<path class="e" d="M383,98 V128 Q383,136 391,136 H556 Q564,136 564,144 V166" marker-end="url(#ratelimit-acquire-ah)"/>
<text class="t-mut ta-s" x="395" y="116">no: bucket short</text>
<path class="e e-dim" d="M564,224 V248 Q564,256 556,256 H162 Q154,256 154,248 V102" marker-end="url(#ratelimit-acquire-ah)"/>
<text class="t-mut" x="359" y="274">recompute the wait and re-check: best-effort wakeups, no queue</text>
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
Wakeups are best effort, not queued. When many tasks park on one empty bucket, a refill can wake
several and only one wins; the rest recompute and park again. The configured rate is never exceeded.
</p>
<p>
The returned <code>Permit</code> is a marker for the call's scope. A circuit-breaker permit records an
outcome on drop, and a semaphore permit returns capacity on drop. A token-bucket permit does nothing on
drop: the token was spent at acquisition, and the bucket refills on the clock.
</p>

```rust
use cano::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step { Call, Done }

#[derive(Clone)]
struct CallUpstream { limiter: RateLimiter }

#[task(state = Step)]
impl CallUpstream {
    async fn run(&self) -> Result<Step, CanoError> {
        // Park until the shared budget admits this call; dropping _permit afterward is a no-op.
        let _permit = self.limiter.acquire().await;
        // Perform the rate-limited downstream call here.
        Ok(Step::Done)
    }
}

// 20 req/s, shared by every task that holds a clone of this limiter.
let limiter = RateLimiter::new(RateLimiterPolicy::per_second(20));
let workflow = Workflow::bare()
    .register(Step::Call, CallUpstream { limiter: limiter.clone() })
    .add_exit_state(Step::Done);
```

<p>
<code>RateLimiter</code> also implements <code>Resource</code> with a no-op lifecycle. To share one quota
without cloning the limiter into each task, insert it once into <a href="../../resources/">Resources</a>
and read it with <code>res.get::&lt;RateLimiter&gt;()?</code> in the task body.
</p>

<h3 id="rl-windowed"><a href="#rl-windowed" class="anchor-link" aria-hidden="true">#</a>Token bucket and fixed window</h3>
<div class="meta-tags">
<span class="meta-tag">since 0.15.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>
<p>
The token bucket keeps a long-run rate and smooths bursts. It cannot model a quota that resets at a
boundary, such as "500 per 5 hours". It refills continuously and has no reset instant to show.
<code>WindowedRateLimiter</code> covers that shape: a fixed-window counter that admits the full quota
at once, resets as a step when the window has elapsed, and reports <code>used()</code>,
<code>remaining()</code> and <code>resets_at()</code>. It resets lazily, is cheap to clone, and
implements <code>Resource</code>.
</p>
<p>
Build its policy with <code>WindowPolicy::new(limit, window)</code>, <code>::per_hours(limit, hours)</code>
or <code>::per_days(limit, days)</code>. <code>WindowedRateLimiter::new</code> panics when
<code>limit == 0</code> or the window is zero.
</p>
<table class="styled-table">
<thead><tr><th></th><th><code>RateLimiter</code></th><th><code>WindowedRateLimiter</code></th></tr></thead>
<tbody>
<tr><td>Model</td><td>token bucket</td><td>fixed window</td></tr>
<tr><td>Replenishment</td><td>continuous drip</td><td>step reset at the boundary</td></tr>
<tr><td>When exhausted</td><td>one more unit every <code>period / quota</code></td><td>zero until the reset, then the full quota</td></tr>
<tr><td>Reset instant</td><td>none</td><td><code>resets_at()</code></td></tr>
<tr><td>Best for</td><td>pacing outbound load</td><td>mirroring a quota with a reset time</td></tr>
</tbody>
</table>

<h3 id="rl-weighted"><a href="#rl-weighted" class="anchor-link" aria-hidden="true">#</a>Weighted cost</h3>
<p>
Both limiters meter weighted units. <code>try_acquire_n(cost)</code> and <code>acquire_n(cost)</code>
consume <code>cost</code> units instead of one, and the plain <code>try_acquire</code> and
<code>acquire</code> are <code>_n(1)</code>. All four take <code>&amp;self</code>. A request-count limit
uses <code>cost = 1</code>, and a usage budget uses the call's cost, for example <code>1500</code>
tokens.
</p>
<p>
A <code>cost</code> of <code>0</code> always succeeds without spending anything. A cost larger than
the capacity (or the window limit) can never be satisfied: <code>try_acquire_n</code> returns
<code>None</code> and <code>acquire_n</code> parks forever. <code>tokens_available()</code> (bucket) and
<code>remaining()</code> (window) report live state, and <code>time_until(cost)</code> gives the
retry-after.
</p>

<h3 id="rl-multi"><a href="#rl-multi" class="anchor-link" aria-hidden="true">#</a>Multi-tier limiting</h3>
<div class="meta-tags">
<span class="meta-tag">since 0.15.0</span>
<span class="meta-tag meta-tag-wasm">wasm-supported</span>
</div>
<p>
API limits often stack: a 5-hour cap, a weekly cap, and a separate weekly cap for one model.
<code>MultiRateLimiter</code> enforces them together. A request is admitted only if every applicable
tier has room. Each tier is an <code>Arc&lt;dyn Meter&gt;</code> (a <code>RateLimiter</code> or a
<code>WindowedRateLimiter</code>, mixed freely) with its own <code>cost</code>, so a request-count
tier and a token-budget tier can share one gate.
</p>
<p>
Acquisition is atomic and leaks nothing. It reserves each tier in turn, and if any tier rejects, it
drops the reservations gathered so far, which refunds their units. A call that passes some tiers and
fails another never burns budget on the tiers that admitted it. Only one tier's lock is held at a
time, so there is no lock-ordering deadlock. A rejection reports which tier blocked and the
retry-after, as <code>CanoError::RateLimited { tier, retry_after }</code>.
</p>

```rust
use cano::prelude::*;
use std::sync::Arc;
use std::time::Duration;

// Fixed-window request quotas that reset in a step at each window boundary.
let five_hour: Arc<dyn Meter> =
    Arc::new(WindowedRateLimiter::new(WindowPolicy::per_hours(500, 5)));
let weekly: Arc<dyn Meter> =
    Arc::new(WindowedRateLimiter::new(WindowPolicy::per_days(5_000, 7)));
let opus_weekly: Arc<dyn Meter> =
    Arc::new(WindowedRateLimiter::new(WindowPolicy::per_days(200, 7)));
// Token-budget tier: 1,000,000/min steady rate with a 100,000-token burst cap for smoothing.
let tokens: Arc<dyn Meter> = Arc::new(RateLimiter::new(
    RateLimiterPolicy::new(1_000_000, Duration::from_secs(60)).with_max_tokens(100_000),
));

let limiter = MultiRateLimiter::new()
    .with_tier("5h", five_hour, 1)
    .with_tier("weekly", weekly, 1)
    .with_tier("opus_weekly", opus_weekly, 1)
    .with_tier("tokens", tokens, 1500); // this call costs 1500 tokens

// Shed load: atomic across every tier.
match limiter.try_acquire() {
    Ok(_permit) => { /* every tier had room; proceed */ }
    Err(CanoError::RateLimited { tier, retry_after }) => {
        // Earlier tiers are automatically refunded when any later tier rejects.
        eprintln!("blocked by `{tier}`, retry after {retry_after:?}");
    }
    Err(_) => unreachable!(),
}
```

<p>
For a per-request subset, such as a non-Opus request that skips the model tier, call
<code>try_acquire_for(&amp;["5h", "weekly", "tokens"])</code> or the async <code>acquire_for</code>.
A tier with <code>cost = 0</code> is inert: it never blocks and is never debited. Tier names are
labels, not keys, so giving two tiers the same meter charges that meter once per tier.
</p>
<p>
Under the hood, <code>try_reserve_n</code> returns a <code>Reservation</code> that refunds its units
on drop unless committed. It is the one acquire method that takes <code>self: &amp;Arc&lt;Self&gt;</code>,
because a reservation holds an <code>Arc&lt;dyn Meter&gt;</code>.
</p>

<div class="callout callout-tip">
<p>Runnable examples: <code>cargo run --example rate_limiter</code> has two spawned workers sharing one
<code>5 req/s</code> bucket, with timestamps about 200ms apart. <code>cargo run --example
rate_limiter_multi</code> shows a 5h, weekly, per-model and token-budget gate: shed load, the
blocking-tier report, zero leak on rejection, per-request tier selection and async parking.</p>
</div>
</div>
