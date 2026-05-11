# cano-e2e — Docker + testcontainers end-to-end hardening

End-to-end tests for Cano's **crash recovery** and **saga / compensation** features under
real process death and concurrency. Not part of the default build — it lives in
`default-members` exclusions because it pulls in heavy Docker / Postgres test deps. Run it
explicitly with `-p cano-e2e`.

## What's here

- **`src/lib.rs` — `PostgresCheckpointStore`**: a real `cano::CheckpointStore` over
  Postgres (`tokio-postgres`). Demonstrates the pluggable storage contract; its primary key
  on `(workflow_id, sequence)` enforces the no-duplicate-checkpoint rule at the database
  level. The `kind` column (`SMALLINT NOT NULL DEFAULT 0`) maps `RowKind` to an integer
  (`0=StateEntry`, `1=CompensationCompletion`, `2=StepCursor`); the `DEFAULT 0` ensures
  pre-existing rows read back as `StateEntry`. Also defines the saga workflow
  (`Reserve` → `Ship` → `Done`) with fault-injection knobs, and query helpers.
- **`src/bin/cano_workflow_app.rs`** — the "custom software" the tests drive: a small Cano
  app behind a CLI (`<dsn> <workflow_id> <run|resume> [fail_at=PHASE] [pause_at=PHASE]`).
  Prints stdout markers (`READY`, `CHECKPOINT`, `RESUME`, `PAUSED`, `DONE`, `FAILED`).
- **`Dockerfile`** — packages `cano_workflow_app`. Build context is the workspace root.
- **`tests/e2e.rs`** — `testcontainers` scenarios: a Postgres container is the checkpoint
  store; the workflow runs in its own app container so it can be stopped mid-flight and
  restarted to resume. Scenarios: crash-mid-flight then resume (no lost/duplicated effects);
  many concurrent distinct workflow ids; two runs racing the same id (one wins, one fails
  with a conflict); a later-step failure rolling back a compensatable step.

## Running

Requires a working Docker daemon.

```bash
# CI does this as a separate step; locally the test builds it on demand if missing.
docker build -t cano-workflow-app:e2e -f cano-e2e/Dockerfile .

# Run the e2e suite (serial — each test owns its own containers + network).
cargo test -p cano-e2e --test e2e -- --test-threads=1
```

Smoke-test the app directly against a local Postgres:

```bash
docker run -d --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16
cargo run -p cano-e2e --bin cano_workflow_app -- \
  postgres://postgres:postgres@127.0.0.1:5432/postgres run-1 run
# → READY ... / CHECKPOINT ... / DONE Done
```

CI: `.github/workflows/e2e.yml` runs this on pull requests to `main` and pushes to `main`.
