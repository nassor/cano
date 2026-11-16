# Changelog

## Unreleased

### Changed

- **Breaking:** `CancellationToken` is `tokio_util::sync::CancellationToken`; `CancellationHandle`
  and `CancellationToken::disabled()` are removed. `Workflow::orchestrate(state)` and
  `resume_from(id)` take no token; `orchestrate_with_token` and `resume_from_with_token` are the
  cancellable forms. `CanoError::is_cancelled()` replaces `category() == "cancelled"`.
- **Breaking:** one `Workflow::register` takes every task model (router, poll, timer, batch,
  stepped, stream and `#[saga::task]`). The task macros implement `cano::workflow::Registrable`,
  which selects the engine path. `register_router`, `register_stepped`, `register_stream` and
  `register_with_compensation` are removed. `StateEntry` is opaque.
- **Breaking:** tasks return the next state. `TaskResult` and `Task::run_bare` are removed; every
  model macro accepts methods written without the `res: &Resources` parameter.
  `PollOutcome::Ready(S)`, `StepOutcome::Done(S)`, `WindowSignal::Stop(S)`,
  `CompensatableTask::run -> (S, Output)`.
- **Breaking:** tasks do not retry by default. `RetryMode::default()` is `None` and every model's
  default `config()` is `TaskConfig::new()`. `TaskConfig::minimal` is removed.
- **Breaking:** `Resources` is keyed by type: `insert(value)`, `get::<T>() -> &T`, `get_arc`, and
  `insert_named` / `get_named` for a second value of one type. The `TResourceKey` generic is
  removed from `Resources`, `Workflow` and every task trait. `#[derive(FromResources)]` reads a
  field by type, or by name with `#[res("name")]`. `testing::TestResources` is removed.
- **Breaking:** `MemoryStore::put`, `remove` and `clear` cannot fail and return `()`.
  `StoreError::LockError` and `StoreError::Generic` are removed.
- **Breaking:** `Scheduler` is non-generic and built by chaining (`every`, `cron`, `manual`,
  `with_backoff`, `with_drain_timeout`, `with_cancel_timeout`, `start`); one scheduler runs
  workflows with different state enums. `RunningScheduler::stop` drains in-flight runs (default
  20 s), cancels them (default 5 s), aborts the rest with an error naming them, then tears down
  resources. `trigger`, `cancel_flow`, `reset_flow`, `status` and `list` are synchronous;
  `wait_idle` and `cano::scheduler::shutdown_signal` are added. `every_seconds`, `every_minutes`,
  `every_hours`, `set_backoff` and `has_running_flows` are removed.
- **Breaking:** `CircuitBreaker::try_acquire` and the `RateLimiter` / `WindowedRateLimiter`
  acquire methods take `&self`; `TaskConfig::with_circuit_breaker` takes the breaker by value.
- **Breaking:** `SplitResult`, `SplitTaskResult` and `JoinConfig::with_join_state` are removed.
  Invalid join configurations fail `Workflow::validate`.
- **Breaking:** `SteppedTask::Cursor` must implement `Clone`; each step retries under the task's
  config.
- `cano::testing` compiles in the crate's own tests. `InMemoryCheckpointStore` gains `rows` and
  `history`; `RecordedEvent` gains `WorkflowTimeout`, `CheckpointClearFailed` and
  `UnknownResumeState`.
- Dependencies raised to their latest releases: `tokio` 1.54, `parking_lot` 0.13, `tracing` 0.1.44,
  `redb` 4.6. `Cargo.lock` is no longer committed.

### Fixed

- `orchestrate` returns a resource teardown error after a successful run, as `resume_from` does.
- Two compensatable states with the same task name fail `validate()` instead of rolling back
  through the wrong instance.
- `WithStateContext::transitions_so_far` keeps the most recent 64 states, so a cyclic workflow no
  longer grows it without bound.
- An observer that panics in `on_retry` or `on_circuit_open` no longer fails the task.
- `#[task::poll]` on a trait impl yields between zero-delay polls instead of starving other tasks
  on the same runtime.

## 0.15.4 and earlier

See the [git tags](https://github.com/nassor/cano/tags) and the commit history.
