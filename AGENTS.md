# Repository Guidelines

## Project overview

Cano is an async workflow engine for Rust: a workflow is a finite state machine over a user enum,
each state runs a task, and an optional scheduler runs workflows on intervals, cron expressions or
manual triggers. It is a library published to crates.io as two crates, `cano` and its procedural
macros `cano-macros`. The API is pre-1.0 and changes between minor versions.

## Workspace layout

| Path | Purpose |
| --- | --- |
| `cano/` | The library. `src/workflow/` is the FSM engine (`execution.rs` dispatch loop, `compensation.rs` saga drain and resume), `src/task/` the task models, `src/rt.rs` and `src/sync.rs` the runtime/lock shims (`runtime-tokio` vs `wasm` WASI P3), `src/scheduler/` the scheduler, `src/checkpoint.rs` the `CheckpointStore` trait, `CheckpointRow` and `RowKind`, `src/store/` the built-in stores (`redb.rs`, `postgres.rs`, `nats.rs`, sharing `codec.rs` and `retry.rs`), `src/blackboard.rs` the in-memory `Blackboard`, `src/resource.rs` the type-keyed `Resources`, `src/testing.rs` the `testing` feature's doubles and `assert_checkpoint_store_contract`. Features: `runtime-tokio` (default), `store-redb` (default), `wasm`, `scheduler`, `tracing`, `store-postgres`, `store-nats`, `metrics`, `testing`, `all`. |
| `cano/src/bin/` | `recovery_resume`, `stepped_resume`: subprocess drivers for the SIGKILL recovery tests. Not user binaries. |
| `cano/tests/` | Integration suites (cancellation, scheduler, reliability, recovery, tracing). `tests/support/` holds `MemStore`, the double these suites use because they cannot see `cfg(test)` items. |
| `cano/examples/` | Runnable examples, auto-discovered. Only examples that need a feature have an `[[example]]` block with `required-features`. |
| `cano/benches/` | Criterion benches: `workflow_performance`, `task_performance`, `resource_performance`, `blackboard_performance`, `blackboard_shared_performance`, `circuit_performance`, `rate_limiter_performance`. |
| `cano-macros/` | `#[task]`, `#[task::router]`, `#[task::poll]`, `#[task::timer]`, `#[task::batch]`, `#[task::stepped]`, `#[task::stream]`, `#[task::suspend]`, `#[saga::task]`, `#[resource]`, `#[checkpoint_store]`, `#[scheduler_store]`, `#[derive(Resource)]`, `#[derive(FromResources)]`. `src/model.rs` is the one driver every task-model macro goes through. |
| `cano-e2e/` | Docker tests: Postgres crash recovery (`tests/e2e.rs`) and `PostgresStore` and `NatsStore` under Toxiproxy faults (`tests/postgres_store.rs`, `tests/nats_store.rs`, fixtures in `tests/support/`). A workspace member outside `default-members`; never published. |
| `docs/` | Zola site (`content/`, `templates/`, `static/`, `config.toml`), deployed to GitHub Pages. |

## Architecture rules

- **One registration path.** `Workflow::register` takes any `Registrable`; the task macros
  implement it and choose the engine path (`StateEntry::single`, `router`, `stepped`, `stream`,
  `suspend`, `compensatable`). A new task model adds a `Model` in `cano-macros/src/model.rs` and a
  `StateEntry` constructor; it never adds a `register_*` method.
- **A parked run is `Ok`.** A suspend state writes a `Suspended` row and `orchestrate` returns
  `Ok(state)` without draining or clearing the log; `resume_with` appends `ResumeInput` and
  re-enters the state without a `StateEntry` row. `resume_from` reads run status from the last
  row's kind only. `RowKind` variants are stored by index: append new ones at the end.
- **Macros emit `::cano::…` paths only.** `cano/src/lib.rs` has `extern crate self as cano;`, so
  the same output works inside the crate.
- **Tasks return the next state** (`Result<TState, CanoError>`). There is no split-returning task;
  parallelism is `register_split`.
- **No retries by default.** Every model's default `config()` is `TaskConfig::new()`.
- **Resources are keyed by type.** A second value of one type uses `insert_named` / `get_named`.
- **Cancellation is a `tokio_util::sync::CancellationToken` passed as `Option<&CancellationToken>`**
  internally; `None` is the zero-cost path with no `select!`.
- **The engine borrows.** Dispatch passes `&dyn Task`, `&TaskConfig` and the name cached in the
  `Entry` at registration. Observers reach the retry loop through the generic `RetryHooks`; the
  public `run_with_retries` passes `()` so its future carries no observer state.
- **Saga safety.** A compensatable task is never dropped mid-run by a token or a total timeout; its
  `attempt_timeout` applies to the forward call only (`CompensatableAdapter::run`).
- **Bounded state.** Anything that grows per transition or per run has a cap
  (`MAX_TRANSITION_PATH = 64`).

## Do

- Run the gate below before every commit and push.
- Change the public API in one step across every call site: `cano/src` doctests, examples, tests,
  benches, `cano/src/bin`, `cano-macros/tests`, `cano-e2e`, `README.md` and `docs/content`.
- Add an entry to `CHANGELOG.md` under `## Unreleased` for every user-visible change; mark breaking
  ones `**Breaking:**`.
- Keep every example runnable: `.github/workflows/examples.yml` runs each one to completion.
- Compile non-trivial doc and README samples as a throwaway `cano/examples/zz_*.rs`, then delete it.
- Measure performance claims with a same-session A/B (`cargo bench -- --save-baseline ref` on the
  reference commit in a `git worktree`, then `--baseline-lenient ref` on the change). This machine
  class is noisy: rerun a regression once and bisect by commit before acting on it.
- Write docs in the current state only, following the technical-writer rules: no history, no em
  dashes, no AI filler.
- Keep files LF (`git ls-files --eol | grep crlf` must be empty). When a script writes a file on
  Windows, open it with `newline=''`.
- Use `@CANO_MINOR@` (`X.Y`) or `@CANO_VERSION@` (`X.Y.Z`) for Cano dependency snippets in
  `docs/content` (`docs/templates/base.html` substitutes them from `docs/config.toml`). Link Rust
  doc comments to `https://nassor.github.io/cano/` instead of embedding `Cargo.toml` snippets.

## Don't

- Don't commit `Cargo.lock`. Cano is a library; the file is gitignored and CI resolves fresh
  versions. CI cache keys hash `Cargo.toml`.
- Don't hardcode Cano crate version numbers in `cano/Cargo.toml` (`use cano-macros.workspace = true`),
  `cano/src`, `README.md`, or `docs/content` (`cano`'s `version_sources_stay_in_sync` lib test
  enforces this).
- Don't add `register_*` variants, a resource key generic, a default retry policy, or an
  `Arc<CircuitBreaker>` / `Arc<RateLimiter>` requirement back to the API.
- Don't hold a `tracing` span guard (`Span::enter()`) across `.await`; attach spans with
  `.instrument(..)`.
- Don't wrap a compensatable task's future in a timeout or a cancellation `select!`.
- Don't add an `[[example]]` block without `required-features`; auto-discovery covers the rest.
- Don't run benches while compiling or while other heavy work runs on the machine, and don't
  compare against a baseline saved in an earlier session.
- Don't let a crate-own test depend on a private test double when `cano::testing` already has one
  (`InMemoryCheckpointStore`, `RecordingObserver`).
- Don't push to `main` directly. Work on a branch and open a pull request.
- Don't move or delete a pushed tag, and don't publish from a dirty tree.
- Don't change the Zola version pinned in `.github/workflows/docs.yml` (0.23.4) without building
  and link-checking the site with the new one first.
- Don't write `{{`, `}}` or `{%` anywhere in `docs/content`, code blocks included: Zola runs page
  bodies through Tera and fails the build. Rewrite a Rust sample to avoid `{{` in format strings.

## Gate

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p cano --all-features      # lib, cano/tests/*, doctests
cargo test -p cano                     # default features
cargo test -p cano-macros
cargo check -p cano --examples --all-features
cargo check -p cano --benches --all-features
cargo check -p cano-e2e
cargo audit
```

`cargo test -p cano --all-features` is the only command that runs the cancellation, scheduler and
reliability suites; CI's test job runs `--lib` plus the tracing integration tests. The SIGKILL
recovery tests are Unix only.

Docs site, with the pinned Zola version:

```sh
cd docs
zola build
zola check
python3 search-index.py public   # or python build-local.py for a browsable local copy
```

## CI

- `.github/workflows/ci.yml`: fmt, clippy, builds, examples and benches check, lib and tracing
  tests, doctests, a `cano-e2e` compile check, and `cargo audit`.
- `.github/workflows/examples.yml`: runs every example (skips `ai_workflow_yes_and`, which needs an
  LLM server) and curl-tests `workflow_on_request`.
- `.github/workflows/e2e.yml`: the Docker suites (`cargo test -p cano-e2e --tests -- --test-threads=1`).
- `.github/workflows/docs.yml`: builds and link-checks the site on pull requests touching `docs/`;
  deploys to GitHub Pages on a push to `main` that touches `docs/`, or on `workflow_dispatch`.

## Release process

### Version sources of truth

A normal release edits three files:

- `Cargo.toml`: `[workspace.package] version = "X.Y.Z"` and
  `[workspace.dependencies] cano-macros = { version = "0.N", path = "cano-macros" }`.
  `cano`, `cano-macros`, and `cano-e2e` inherit `version.workspace = true`, and `cano/Cargo.toml`
  uses `cano-macros.workspace = true`.
- `docs/config.toml`: `[extra] version = "X.Y.Z"`, which drives the header badge (`vX.Y.Z`) and
  substitutes `@CANO_MINOR@` (`X.Y`) and `@CANO_VERSION@` (`X.Y.Z`) across `docs/content`.
- `CHANGELOG.md`: `## X.Y.Z - YYYY-MM-DD` (optionally `## X.Y.Z - YYYY-MM-DD - <Theme>` on minor
  releases) above a fresh empty `## Unreleased`.

Only when raising MSRV (`rust-version`, which requires a minor bump in `0.x`) also update:
`Cargo.toml`, `.github/workflows/ci.yml`, `.github/workflows/examples.yml`,
`.github/workflows/e2e.yml`, `cano-e2e/Dockerfile`, `README.md`, and `docs/content/_index.md`.

### Steps

1. **Audit changes and pick the version by semver.** Compare `git log v<prev>..HEAD --oneline`
   against `## Unreleased` in `CHANGELOG.md` so no user-visible change or `**Breaking:**` entry
   is missing. In `0.x`, a patch (`0.16.0` to `0.16.1`) is picked up automatically by every
   `cano = "0.16"` user, so any `**Breaking:**` entry or MSRV bump requires a minor bump
   (`0.(N+1).0`).
   - On a minor bump, update `[workspace.dependencies] cano-macros` in `Cargo.toml` to `"0.(N+1)"`.
   - On a routine patch bump, leave `cano-macros = "0.N"` unchanged. If the patch changes macro
     codegen in a way `cano X.Y.Z` requires, tighten `cano-macros` in `Cargo.toml` to `"X.Y.Z"`
     so existing lockfiles cannot pair new `cano` with an older `0.N` `cano-macros`.
2. **Check crates.io and tags.** Confirm the current published versions and ensure `vX.Y.Z` does
   not exist locally or on `origin`:

   ```sh
   curl -sA cano-release https://crates.io/api/v1/crates/cano
   curl -sA cano-release https://crates.io/api/v1/crates/cano-macros
   git ls-remote --tags origin vX.Y.Z
   ```

3. **Isolate dependency drift before bumping.** Branch from `main` (e.g. `chore/release-X.Y.Z`).
   Because `Cargo.lock` is gitignored, run `cargo update` and the full gate before bumping. If
   code untouched since the previous tag fails, fix it in a separate commit/PR first.
4. **Bump the version in one commit:**
   - `Cargo.toml`: update `[workspace.package] version`, and `[workspace.dependencies] cano-macros`
     if this is a minor bump or a macro-codegen patch;
   - `docs/config.toml`: update `[extra] version`;
   - `CHANGELOG.md`: rename `## Unreleased` to `## X.Y.Z - YYYY-MM-DD` (plus `- <Theme>` if
     desired on minor releases) and start a new empty `## Unreleased` above it.
5. **Run the full gate and package checks.**
   - Run the full Cargo gate above (including `version_sources_stay_in_sync`), verify LF line
     endings (`git ls-files --eol | grep crlf` empty), and run the Zola build, link check, and
     search index (`zola build`, `zola check`, `python3 search-index.py public`).
   - Inspect packages: `cargo publish --dry-run -p cano-macros`,
     `cargo package -p cano-macros --list`, and `cargo package -p cano --list` (confirm no stray
     files or `zz_*.rs` examples). On a patch bump where `cano-macros 0.N` is already on crates.io,
     also run `cargo publish --dry-run -p cano`; on a minor bump (`0.N.0`), `cano-macros 0.N` is
     not on crates.io yet so `--dry-run -p cano` cannot resolve it remotely, run
     `cargo package -p cano --no-verify` instead.
6. **Merge and wait for `main` workflows.** Open the pull request, merge into `main`, and wait for
   `CI`, `Examples`, `E2E`, and `Docs` (which deploys the updated `docs/config.toml` version to GitHub
   Pages) to pass on the `main` merge commit before tagging or publishing.
7. **Tag and publish from a clean worktree.** Pull `main` (`git checkout main && git pull --ff-only`),
   verify `HEAD` is the green merge commit, tag and push `vX.Y.Z`, and publish `cano-macros` first
   from a detached worktree because `cano` depends on the published `cano-macros`:

   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   git worktree add ../cano-release vX.Y.Z
   cd ../cano-release
   cargo publish -p cano-macros
   cargo publish -p cano
   cd - && git worktree remove --force ../cano-release
   ```

   If `cargo publish -p cano` fails immediately after `cano-macros` uploads due to crates.io
   sparse-index propagation delay, wait until
   `https://crates.io/api/v1/crates/cano-macros/X.Y.Z` resolves and rerun `cargo publish -p cano`
   in the same worktree.
8. **Verify and cut the GitHub release.** Confirm `yanked: false` on
   `https://crates.io/api/v1/crates/cano-macros/X.Y.Z` and
   `https://crates.io/api/v1/crates/cano/X.Y.Z`, check docs.rs builds for both crates and the
   `vX.Y.Z` badge on `https://nassor.github.io/cano/`, and create the GitHub release from the
   changelog section (`gh release create vX.Y.Z --title "vX.Y.Z" --notes-file <section>`).
   Never move or delete a pushed tag; if a critical defect ships, `cargo yank --version X.Y.Z` and
   release `X.Y.(Z+1)`.
