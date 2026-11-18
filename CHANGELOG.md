# Changelog

## Unreleased

### Added

- `SuspendTask` and `#[task::suspend]`: a task model that parks a run at its state.
  `suspend` runs on entry, the engine writes a `RowKind::Suspended` row, and `orchestrate`
  returns `Ok(parked_state)` with the checkpoint log and compensation stack kept.
  `Workflow::resume_with(id, &input)` (and `resume_with_token`) records the input as a
  `RowKind::ResumeInput` row and continues the run from the state `resume` returns.
  `resume_from` on a parked run returns the parked state without running anything, and
  re-delivers a recorded input whose `resume` did not finish. A suspend task requires a
  checkpoint store (`validate` rejects it otherwise). See the `suspend_task` example.
- `RowKind::Suspended` and `RowKind::ResumeInput`, with `CheckpointRow::suspended` and
  `CheckpointRow::with_resume_input`. Custom `CheckpointStore` backends that map `RowKind` to
  their own encoding need the two new variants.
- `cano_workflow_runs_total` and `cano_workflow_duration_seconds` use the outcome label
  `suspended` for a run that parks at a suspend state and `cancelled` for a run stopped via a
  `CancellationToken`; `cano_scheduler_flow_runs_total` uses `cancelled` for a cancelled
  scheduled run.
- `cano::store::postgres::PostgresStore` (feature `store-postgres`): a `CheckpointStore` in one
  PostgreSQL table, `cano_checkpoints` by default, over a `deadpool-postgres` pool. A builder sets
  the schema, table (`with_checkpoint_table`), pool size and timeouts, TLS connector or your own
  pool, and whether to create the table; `schema_sql()` returns the DDL. The primary key
  `(workflow_id, sequence)` is the only index. Re-exported as `cano::PostgresStore`.
- `cano::store::nats::NatsStore` (feature `store-nats`): a `CheckpointStore` in a NATS JetStream
  Key/Value bucket, `cano_checkpoints` by default, one key per row. A builder sets the bucket
  name (`with_checkpoint_bucket`) and config (`with_checkpoint_bucket_config`), connect options,
  JetStream domain or API prefix, and whether to create the bucket. Re-exported as
  `cano::NatsStore`.
- Both network stores bound every call with an operation timeout (10 s by default) and retry
  transient failures with `with_retry(RetryMode)`, off by default. A retried append that finds
  its own byte-identical row reports `Ok`.
- `cano::testing::assert_checkpoint_store_contract`: runs the `CheckpointStore` contract
  (round trip of every field, ordering, duplicate rejection, id isolation, concurrency, large
  blobs) against any implementation.
- `RedbStore::open_default()` opens `cano.redb` (`RedbStore::DEFAULT_FILE_NAME`) in a persistent
  directory, creating it when missing; `RedbStore::default_path()` returns that file. The
  directory is `CANO_DATA_DIR` (`RedbStore::DATA_DIR_ENV`) when set; else, on Linux and other
  Unix systems except macOS, systemd's `STATE_DIRECTORY` plus `cano`; else a per-user directory
  plus `cano/<executable name>`: `$XDG_STATE_HOME` when it is an absolute path, else
  `~/.local/state`, on Linux and other Unix systems; `~/Library/Application Support` on macOS;
  `%LOCALAPPDATA%` on Windows.
- Multi-instance scheduling (feature `scheduler`). `SchedulerStore` (also `cano::scheduler::SchedulerStore`
  and in the prelude) is a versioned compare-and-swap register keyed by flow id: `load`,
  `compare_and_swap` (returns `CasOutcome::Written(version)` or `Conflict`) and `remove`, with
  `VersionedRecord` (both `CasOutcome` and `VersionedRecord` are in `cano::prelude`). Implement it
  with `#[cano::scheduler_store]` on a trait impl or inherent `impl` block. Errors use
  `CanoError::SchedulerStore` (constructor `CanoError::scheduler_store`, category `scheduler_store`).
- `Scheduler::with_store(Arc<dyn SchedulerStore>)`, `with_lease_ttl(Duration)` (30 s by default, zero is a
  configuration error) and `with_instance_id(&str)` (a random `cano-<16 hex digits>` by default, empty is a
  configuration error), and `RunningScheduler::instance_id()`. Schedulers that share a store run each
  `every` interval and each `cron` firing once across the cluster. A run holds a lease renewed every third
  of the TTL, a lost lease cancels the local run, and a crashed instance's lease expires into one counted
  failure. Backoff, trip state, `run_count` and `last_run` persist, so a restarted cluster does not re-run an
  `every` flow within its interval. A store call that takes longer than one TTL fails, so a store that
  stops answering cannot hold a flow in `Running`. Without `with_store` nothing changes.
- `Status::RunningElsewhere { owner, lease_expires }`: another instance holds the flow's lease.
  `trigger` is refused while one does, and the error names the owner.
- `SchedulerStore` for the built-in stores, each behind `scheduler` plus its store feature:
  `RedbStore` (tables `cano_scheduler` and `cano_scheduler_version` in the same file; one process holds
  the file, so it coordinates schedulers inside one process), `PostgresStore` (table `cano_scheduler`,
  `postgres::DEFAULT_SCHEDULER_TABLE`, `with_scheduler_table`) and `NatsStore` (bucket `cano_scheduler`,
  `nats::DEFAULT_SCHEDULER_BUCKET`, `with_scheduler_bucket`, `with_scheduler_bucket_config`,
  `scheduler_kv()`).
- `cano::testing::InMemorySchedulerStore` and `cano::testing::assert_scheduler_store_contract`.
- The `scheduler_cluster` example runs two schedulers over one `RedbStore`, and the docs gain a
  "Several scheduler instances" page.
- Scheduled `Workflow` + `CheckpointStore` automatic resume: `Scheduler` defaults `workflow_id`
  to `flow_id` when unset, clears stale matching-version exit logs before starting a fresh run,
  and automatically resumes unfinished runs (rehydrating saga compensation stacks and stepped or
  stream cursors) without repeating resource setup or teardown.
- Expanded store contract assertions: `assert_checkpoint_store_contract` tests concurrent
  identical seq-0 `append` races, and `assert_scheduler_store_contract` checks 256 KiB payloads,
  post-`remove` equal-byte create races, stale pre-remove version rejection after recreate,
  matching payloads on race winners, and concurrent `load` / `compare_and_swap` / `remove`
  linearizability.
- Animated SMIL diagrams across the scheduler documentation, plus updated resilience, scheduled
  checkpoint auto-resume, store contract, and network store documentation.

### Changed

- **Breaking:** the `recovery` feature is replaced by `store-redb`, which is on by default.
  `default-features = false` removes redb from the build.
- **Breaking:** `RedbCheckpointStore` is `cano::store::RedbStore`, re-exported as `cano::RedbStore`
  and in the prelude. Its default file is `cano.redb`.
- **Breaking:** the module `cano::recovery` is split: `cano::checkpoint` holds `CheckpointStore`,
  `CheckpointRow` and `RowKind`, and `cano::store` holds the backends.
- **Breaking:** `MemoryStore` is `cano::blackboard::Blackboard`. `StoreError`, `StoreResult` and
  `CanoError::Store` are `BlackboardError`, `BlackboardResult` and `CanoError::Blackboard`, with
  the error category `blackboard` and the constructor `CanoError::blackboard`.
- The examples `store_custom_backend` and `workflow_stack_store` are `blackboard_namespaced` and
  `workflow_blackboard`; the benches `store_performance` and `store_shared_performance` are
  `blackboard_performance` and `blackboard_shared_performance`.
- **Breaking:** `RunningScheduler::trigger` and `RunningScheduler::reset_flow` are `async` for every
  scheduler, so a call needs `.await`.

### Fixed

- `Workflow::resume_from` and `resume_with` clear rehydrated `SteppedTask` cursors whenever the FSM
  transitions to a different state, so a completed visit in a looping workflow cannot leak its
  cursor into a later visit, and cap rehydrated `prior_transitions` at 64 entries.
- `Workflow::resume_from_with_token` on a log whose last row is already an exit state returns
  `Ok(exit_state)` and clears the log without rehydrating or draining the saga compensation stack.
- `Workflow::validate` checks duplicate compensator names across current state registrations (so
  replacing a duplicate state clears the error and preserves remaining compensators) and rejects
  colliding `Debug` state labels when a checkpoint store is attached.
- `Scheduler::start` rejects workflows containing a `SuspendTask`, attaches `Workflow::with_tracing_span`
  on scheduled runs, tags `execute_flow` spans with `flow_id`, preserves `Status::Backoff` when a
  manual trigger is cancelled during an active backoff window, and stamps per-process session and
  per-claim lease ids on distributed leases so fast restarts with a static `instance_id` and delayed
  zombie tasks cannot steal or release another run's lease.
- Dropping `RunningScheduler::stop()` mid-drain (for example under `tokio::time::timeout`) resets
  aborted flows from `Status::Running` to `Status::Idle` and ensures a subsequent `stop()` reports
  the aborted flows and tears down each flow's resources at most once.
- `WorkflowObserver` `on_task_start` and `on_task_failure` hooks stay balanced when a `CancellationToken`
  fires during `CheckpointStore::append` before task polling begins, and when a split state times out
  under `JoinConfig::with_timeout`.
- `Resources::setup_all` and `teardown_all_result` catch panics inside custom `setup` and `teardown`
  methods so prior resources still roll back and remaining teardowns still run in reverse order.
- `WindowedRateLimiter` avoids `u64` overflow when `limit == u64::MAX`; `PollTask` and `StreamTask`
  avoid `u32` overflow when `max_errors == u32::MAX`; sub-millisecond `RetryMode` delays sleep as
  configured; `BackoffPolicy::compute_delay` and engine deadlines avoid panicking on `Duration::MAX`
  or extreme streaks.
- `PostgresStore` and `NatsStore` only treat a duplicate row or `WrongLastRevision` as an absorbed
  lost acknowledgement when an earlier attempt actually sent the write, evict broken pooled Postgres
  connections on transient failures, reject matching checkpoint and scheduler table names in
  `PostgresStoreBuilder`, bound `NatsStoreBuilder::connect_with_options` by `operation_timeout`, and
  reject trailing bytes in `postcard`-encoded checkpoint payloads.
- `#[task]`, `#[resource]`, `#[checkpoint_store]` and `#[scheduler_store]` preserve explicit
  lifetimes, rewrite nested elided references, order lifetime parameters before type and const
  generics, and reject duplicate or unsupported items with spanned compile errors.
- `Scheduler` lease resilience and recovery:
  - Preserves pre-claim `run_count`, `last_run` and `last_tick` on `Lease`, rolls back unstarted
    claims via `abort_claim` (with a bounded `unstarted_leases` fallback set during store outages)
    on lost ACKs, cancellations, dropped `trigger` futures, and shutdown races, and recovers from a
    lost-ACK claim plus concurrent `reset_flow` by matching the session's `lease.id`.
  - Tracks holder lease TTL monotonically via `tokio::time::Instant` anchored before the claim or
    renewal swap, returns `Renew::Lost(None)` when the renewal budget expires or times out, and
    caps `claim` and `reset` CAS loops by the lease TTL.
  - Anchors expired-lease failure timestamps at `lease.expires_at.min(now)`, retries due cron ticks
    on transient store errors while before the next firing, retries `every` store errors at `ttl / 3`,
    normalizes expired `Status::RunningElsewhere` in `status`, `list` and `wait_idle`, clears
    expired leases in `reset_flow` while leaving live leases intact, uses an optimistic 1-RTT swap
    in `release` with `written_session` disambiguation, and UTF-8-safely truncates `last_error` to
    4096 bytes.
- Store hardening across `NatsStore`, `PostgresStore`, `RedbStore` and `store::retry`:
  - `NatsStore::append` (`ROW_TAG_V2` with 128-bit `write_id`) and `PostgresStore::append`
    (`write_id bytea` column) prevent retried appends from absorbing another writer's identical
    seq-0 row; `RedbStore` decodes both V1 and V2 payloads.
  - `NatsStore::compare_and_swap` returns `CasOutcome::Conflict` in 1 RTT on `WrongLastRevision`
    when no send occurred on the current target revision and resets `sent_target` when advancing
    across a tombstone; `NatsStore::remove` checks the current revision and uses
    `purge_expect_revision`; `NatsStore::load_run` bounds `messages.next()` with `consumer_info`
    polling during concurrent `clear` and deletes ephemeral consumers on return or drop;
    `NatsStoreBuilder` rejects non-zero `max_age`, aligns `request_timeout` and JetStream `Context`
    timeout with `operation_timeout`, retries `open_bucket`, guards connection and `max_payload`
    limits, and classifies permanent JetStream errors as fatal.
  - `PostgresStore` defaults `keepalives_idle` to 30 s, `connect_timeout` and deadpool `wait`
    timeout to `operation_timeout`, and classifies SQLSTATE `40003`, `25006` and `57014` as
    transient; `store::retry` applies equal `[50%, 100%]` jitter; `RedbStore` cancels queued
    blocking writes via `CallerCancelled` drop guards and skips `tx.commit()` on no-op `clear` and
    `remove` calls.

## 0.16.0 - 2026-11-15 - API Simplification

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
