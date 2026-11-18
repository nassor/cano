+++
title = "WebAssembly (WASI P3)"
description = "Run Cano workflows inside a WebAssembly / WASI Preview 3 component with zero Tokio runtime or native OS dependencies."
template = "section.html"
+++

<div class="content-wrapper">
<h1>WebAssembly (WASI P3)</h1>
<p class="subtitle">Compile Cano into a lean <code>wasm32-wasip3</code> guest component powered by WASI Preview 3 clocks and host-supplied capabilities.</p>
<div class="meta-tags">
<span class="meta-tag">since 0.17.0</span>
</div>
<p class="feature-tag">Enable with <code>default-features = false, features = ["wasm"]</code> on target <code>wasm32-wasip3</code>.</p>

<p>
Cano separates its async engine from native Tokio and OS-specific crates. By disabling default
features and enabling the <code>wasm</code> feature, you can embed Cano inside a WebAssembly
Component Model guest library targeting WASI Preview 3 (<code>wasm32-wasip3</code>) without pulling
in Tokio time/rt drivers, <code>parking_lot</code>, <code>redb</code>, sockets, or filesystem code.
</p>

<!-- Table of Contents -->
<nav class="page-toc" aria-label="Table of contents">
<div class="page-toc-title">On this page</div>
<ol>
<li><a href="#setup">Target &amp; Cargo setup</a></li>
<li><a href="#feature-matrix">Feature &amp; capability matrix</a></li>
<li><a href="#guest-example">Guest component workflow example</a></li>
<li><a href="#runtime-details">How WASI P3 execution differs</a></li>
<li><a href="#host-integration">Host storage &amp; observability</a></li>
</ol>
</nav>

<hr class="section-divider">
<h2 id="setup"><a href="#setup" class="anchor-link" aria-hidden="true">#</a>Target &amp; Cargo setup</h2>

<p>
WASI Preview 3 async bindings in Rust require the <code>wasm32-wasip3</code> target (Rust
1.100.0 or later):
</p>

<div class="code-block">
<span class="code-block-label">Install the WASI Preview 3 target</span>

```sh
rustup target add wasm32-wasip3
```
</div>

<p>
In your guest crate's <code>Cargo.toml</code>, opt out of default features and select
<code>wasm</code> (plus optional pure-Wasm extras like <code>tracing</code>, <code>metrics</code>,
or <code>testing</code>):
</p>

<div class="code-block">
<span class="code-block-label">Cargo.toml for a WASI Preview 3 component library</span>

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
cano = { version = "@CANO_MINOR@", default-features = false, features = ["wasm"] }
```
</div>

<div class="callout callout-info">
<div class="callout-label">Mutually exclusive runtime backends</div>
<p>
<code>wasm</code> and <code>runtime-tokio</code> cannot be enabled in the same build. Enabling both
(or building <code>wasm</code> for a non-<code>wasm32</code> target) fails at compile time with an
explicit error rather than silently miscompiling timers.
</p>
</div>

<hr class="section-divider">
<h2 id="feature-matrix"><a href="#feature-matrix" class="anchor-link" aria-hidden="true">#</a>Feature &amp; capability matrix</h2>

<p>
Every Cano Cargo feature and core subsystem carries a <code>since</code> version and a Wasm
compatibility status:
</p>

<div class="table-wrap">
<table>
<thead>
<tr>
<th>Feature / Capability</th>
<th>Since</th>
<th>WASI P3 (<code>wasm</code>)</th>
<th>Notes</th>
</tr>
</thead>
<tbody>
<tr>
<td><code>Workflow</code>, FSM routing &amp; validation</td>
<td><code>0.1.0</code></td>
<td>Supported</td>
<td>Core finite-state engine and transition cap</td>
</tr>
<tr>
<td><code>Task</code>, <code>#[task]</code>, <code>TaskConfig</code> &amp; retries</td>
<td><code>0.1.4</code> / <code>0.5.0</code></td>
<td>Supported</td>
<td>Retries &amp; <code>attempt_timeout</code> use WASI monotonic clock</td>
</tr>
<tr>
<td>Parallel <code>register_split</code> &amp; <code>JoinStrategy</code></td>
<td><code>0.7.0</code></td>
<td>Supported</td>
<td>Cooperative concurrency via <code>FuturesUnordered</code></td>
</tr>
<tr>
<td><code>Resources</code>, <code>#[resource]</code>, <code>HealthStatus</code></td>
<td><code>0.10.0</code> / <code>0.12.0</code></td>
<td>Supported</td>
<td>Typed dependency injection for host bindings</td>
</tr>
<tr>
<td><code>CircuitBreaker</code> &amp; bulkhead limits</td>
<td><code>0.11.0</code> / <code>0.12.0</code></td>
<td>Supported</td>
<td>Lock-free atomics and in-memory state</td>
</tr>
<tr>
<td><code>RouterTask</code>, <code>PollTask</code>, <code>BatchTask</code>, <code>SteppedTask</code>, <code>Saga</code></td>
<td><code>0.12.0</code></td>
<td>Supported</td>
<td>Forward execution, cursor resume &amp; reverse compensation</td>
</tr>
<tr>
<td><code>WorkflowObserver</code>, <code>TracingObserver</code> (<code>tracing</code>)</td>
<td><code>0.12.0</code> / <code>0.6.0</code></td>
<td>Supported</td>
<td>Wire a tracing subscriber or forward spans to the host</td>
</tr>
<tr>
<td><code>MetricsObserver</code> (<code>metrics</code>)</td>
<td><code>0.13.0</code></td>
<td>Supported</td>
<td>Works with any <code>metrics</code> recorder installed in the guest</td>
</tr>
<tr>
<td><code>RateLimiter</code>, <code>WindowedRateLimiter</code>, <code>MultiRateLimiter</code></td>
<td><code>0.14.0</code> / <code>0.15.0</code></td>
<td>Supported</td>
<td>Co-operative sleeps via WASI monotonic clock</td>
</tr>
<tr>
<td><code>TimerTask</code>, <code>StreamTask</code>, <code>CancellationToken</code></td>
<td><code>0.15.0</code></td>
<td>Supported</td>
<td>Uses guest timers and cancellation tokens</td>
</tr>
<tr>
<td><code>testing</code> (<code>RecordingObserver</code>, <code>InMemoryCheckpointStore</code>, <code>assert_checkpoint_store_contract</code>)</td>
<td><code>0.15.0</code> / <code>0.17.0</code></td>
<td>Supported</td>
<td>In-memory doubles and store contract tests work on Wasm; scheduler test doubles require <code>scheduler</code></td>
</tr>
<tr>
<td><code>Blackboard</code>, <code>SuspendTask</code>, <code>CheckpointStore</code></td>
<td><code>0.17.0</code> / <code>0.12.0</code></td>
<td>Supported</td>
<td>Custom <code>CheckpointStore</code> can call imported WIT interfaces</td>
</tr>
<tr>
<td><code>wasm</code> feature</td>
<td><code>0.17.0</code></td>
<td>Supported</td>
<td>Selects WASI Preview 3 clocks via <code>wasip3</code> and std sync primitives</td>
</tr>
<tr>
<td><code>runtime-tokio</code> (default)</td>
<td><code>0.17.0</code></td>
<td>Not supported</td>
<td>Native Tokio timers, tasks, and <code>parking_lot</code> locks</td>
</tr>
<tr>
<td><code>scheduler</code> (<code>Scheduler</code>, <code>BackoffPolicy</code>, <code>SchedulerStore</code>)</td>
<td><code>0.6.0</code></td>
<td>Not supported</td>
<td>Requires <code>runtime-tokio</code>; Wasm hosts typically trigger guest exports</td>
</tr>
<tr>
<td><code>store-redb</code> (default), <code>store-postgres</code>, <code>store-nats</code></td>
<td><code>0.17.0</code></td>
<td>Not supported</td>
<td>Require native filesystems, sockets, or Tokio drivers</td>
</tr>
<tr>
<td><code>all</code> meta-feature</td>
<td><code>0.10.0</code></td>
<td>Not supported</td>
<td>Pulls in native stores and the Tokio scheduler</td>
</tr>
</tbody>
</table>
</div>

<hr class="section-divider">
<h2 id="guest-example"><a href="#guest-example" class="anchor-link" aria-hidden="true">#</a>Guest component workflow example</h2>

<p>
A guest component exposes an async function (bridged by your WIT guest bindings or
<code>wasip3</code> export) that builds or invokes a <code>Workflow</code> and returns the terminal state:
</p>

<div class="code-block">
<span class="code-block-label">WASI P3 workflow library module</span>

```rust
use cano::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OrderState {
    Validate,
    Charge,
    Complete,
    Rejected,
}

#[task(state = OrderState)]
impl ValidateOrder {
    async fn run(&self, res: &Resources) -> Result<OrderState, CanoError> {
        let board = res.get::<Blackboard>()?;
        let amount: u64 = board.get("amount").unwrap_or(0);
        if amount == 0 {
            return Ok(OrderState::Rejected);
        }
        Ok(OrderState::Charge)
    }
}

struct ChargeCard;

#[task(state = OrderState)]
impl ChargeCard {
    fn config(&self) -> TaskConfig {
        TaskConfig::new().with_attempt_timeout(Duration::from_secs(2))
    }

    async fn run(&self, res: &Resources) -> Result<OrderState, CanoError> {
        res.get::<Blackboard>()?.put("charged", true);
        Ok(OrderState::Complete)
    }
}

pub async fn run_order(amount: u64) -> Result<OrderState, CanoError> {
    let board = Blackboard::new();
    board.put("amount", amount);

    let wf = Workflow::new(Resources::new().insert(board))
        .register(OrderState::Validate, ValidateOrder)
        .register(OrderState::Charge, ChargeCard)
        .add_exit_states([OrderState::Complete, OrderState::Rejected]);

    wf.orchestrate(OrderState::Validate).await
}
```
</div>

<p>
Notice there are no Tokio macros or <code>tokio::time</code> imports: <code>with_attempt_timeout</code>,
<code>RetryMode</code>, <code>PollTask</code>, <code>TimerTask</code>, <code>BatchTask</code>,
<code>StreamTask</code>, and <code>RateLimiter</code> all dispatch through Cano's internal runtime
layer onto WASI Preview 3 monotonic clocks automatically.
</p>

<hr class="section-divider">
<h2 id="runtime-details"><a href="#runtime-details" class="anchor-link" aria-hidden="true">#</a>How WASI P3 execution differs</h2>

<ul>
<li><strong>Clocks and sleeps</strong>: Cano's runtime layer calls <code>wasip3::clocks::monotonic_clock::now()</code>, <code>wait_for</code>, and <code>wait_until</code> instead of <code>tokio::time</code>.</li>
<li><strong>Fan-out concurrency</strong>: Parallel branches (<code>register_split</code> and <code>BatchTask</code>) drive futures concurrently on the component's single async task via <code>FuturesUnordered</code> rather than spawning multi-threaded OS tasks with <code>tokio::task::JoinSet</code>.</li>
<li><strong>Locks</strong>: <code>Blackboard</code> and internal synchronization use <code>std::sync</code> locks instead of <code>parking_lot</code> under <code>wasm</code>, keeping thread-parking crates out of the binary.</li>
<li><strong>Panic handling</strong>: On <code>wasm32</code> targets compiled with <code>panic = "abort"</code>, panics trap the component instance rather than unwinding; when compiling with unwind support, Cano catches panics at task and split boundaries like native builds.</li>
</ul>

<hr class="section-divider">
<h2 id="host-integration"><a href="#host-integration" class="anchor-link" aria-hidden="true">#</a>Host storage &amp; observability</h2>

<p>
Because built-in database drivers are disabled in Wasm builds, durable persistence and
cross-component telemetry flow through traits:
</p>

<ul>
<li><strong>Checkpoints &amp; suspend/resume</strong>: Implement <a href="../recovery/checkpoint-stores/#custom"><code>CheckpointStore</code></a> backed by imported WIT key-value or SQL host interfaces, and pass it to <code>Workflow::with_checkpoint_store(...)</code>.</li>
<li><strong>Host services via Resources</strong>: Register typed handles wrapping your generated WIT client bindings in <a href="../resources/"><code>Resources</code></a> so tasks stay decoupled from Component Model transport details.</li>
<li><strong>Observers &amp; telemetry</strong>: Implement <a href="../observers/"><code>WorkflowObserver</code></a> (or install a guest <code>tracing</code> subscriber / <code>metrics</code> recorder) to forward workflow events to WASI logging or host telemetry imports.</li>
</ul>

</div>
