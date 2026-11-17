# Repository Guidelines

## Project overview

Cano is an async workflow engine for Rust: a workflow is a finite state machine over a user enum,
each state runs a task, and an optional scheduler runs workflows on intervals, cron expressions or
manual triggers. It is a library published to crates.io as two crates, `cano` and its procedural
macros `cano-macros`. The API is pre-1.0 and changes between minor versions.

## Workspace layout

| Path | Purpose |
| --- | --- |
| `cano/` | The library. `src/workflow/` is the FSM engine (`execution.rs` dispatch loop, `compensation.rs` saga drain and resume), `src/task/` the task models, `src/scheduler/` the scheduler, `src/resource.rs` the type-keyed `Resources`, `src/testing.rs` the `testing` feature's doubles. |
| `cano/src/bin/` | `recovery_resume`, `stepped_resume`: subprocess drivers for the SIGKILL recovery tests. Not user binaries. |
| `cano/tests/` | Integration suites (cancellation, scheduler, reliability, recovery, tracing). `tests/support/` holds `MemStore`, the double these suites use because they cannot see `cfg(test)` items. |
| `cano/examples/` | Runnable examples, auto-discovered. Only examples that need a feature have an `[[example]]` block with `required-features`. |
| `cano/benches/` | Criterion benches: `workflow_performance`, `task_performance`, `resource_performance`, `store_performance`, `store_shared_performance`, `circuit_performance`, `rate_limiter_performance`. |
| `cano-macros/` | `#[task]`, `#[task::router]`, `#[task::poll]`, `#[task::timer]`, `#[task::batch]`, `#[task::stepped]`, `#[task::stream]`, `#[task::suspend]`, `#[saga::task]`, `#[resource]`, `#[checkpoint_store]`, `#[derive(Resource)]`, `#[derive(FromResources)]`. `src/model.rs` is the one driver every task-model macro goes through. |
| `cano-e2e/` | Docker and Postgres crash-recovery tests. A workspace member outside `default-members`; never published. |
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

## Don't

- Don't commit `Cargo.lock`. Cano is a library; the file is gitignored and CI resolves fresh
  versions. CI cache keys hash `Cargo.toml`.
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
- `.github/workflows/e2e.yml`: the Docker crash-recovery suite (`cargo test -p cano-e2e --test e2e`).
- `.github/workflows/docs.yml`: builds and link-checks the site on pull requests touching `docs/`;
  deploys to GitHub Pages on a push to `main` that touches `docs/`, or on `workflow_dispatch`.

## Release process

1. **Pick the version by semver.** Read `## Unreleased` in `CHANGELOG.md`. In 0.x a patch
   (0.15.3 to 0.15.4) is picked up automatically by every `cano = "0.15"` user, so any
   `**Breaking:**` entry requires a minor bump (0.16.0).
2. **Check crates.io** for the current version:
   `curl -A cano-release https://crates.io/api/v1/crates/cano` (the API needs a User-Agent).
3. **Bump the version** in one commit on a branch:
   - `version` in `[workspace.package]` of the root `Cargo.toml`;
   - the `cano-macros = { version = "0.N", path = "../cano-macros" }` requirement in
     `cano/Cargo.toml` on a minor bump;
   - `version` under `[extra]` in `docs/config.toml` (the header badge), and every
     `cano = { version = "0.N", ... }` install snippet on a minor bump
     (`rg -n 'version = "0\.' docs/content README.md`);
   - rename `## Unreleased` to `## X.Y.Z - YYYY-MM-DD` in `CHANGELOG.md` and start a new empty
     `## Unreleased`.
4. **Gate**: `cargo update`, then the full gate above, then
   `cargo publish --dry-run -p cano-macros` and `cargo package -p cano --list` (no stray files).
   A failure in code untouched since the last tag is dependency drift: fix it in a separate commit
   first.
5. **Merge** the pull request into `main` and wait for CI, Examples and E2E to pass on `main`.
6. **Tag** the merge commit: `git tag vX.Y.Z && git push origin vX.Y.Z`.
7. **Publish from the tag in a clean worktree**, macros first, because `cano` depends on the
   published `cano-macros`:

   ```sh
   git worktree add ../cano-release vX.Y.Z
   cd ../cano-release
   cargo publish -p cano-macros
   cargo publish -p cano
   cd - && git worktree remove --force ../cano-release
   ```

8. **Verify** `https://crates.io/api/v1/crates/cano/X.Y.Z` shows `yanked: false`, and create the
   GitHub release from the tag with the changelog section as its notes
   (`gh release create vX.Y.Z --notes-file <section>`).
