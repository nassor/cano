//! Building blocks for the Docker-based end-to-end hardening tests:
//!
//! - [`PostgresCheckpointStore`] — a `cano::CheckpointStore` backed by Postgres
//!   (`tokio-postgres`). It demonstrates the pluggable storage contract and is what the
//!   e2e workflow uses, so a crashed app container can be restarted against the *same*
//!   Postgres and resume.
//! - A small saga workflow ([`build_workflow`]): `Reserve` (compensatable) → `Ship`
//!   (plain) → `Done`, with `fail_at` / `pause_at` knobs for fault injection. All side
//!   effects go to Postgres tables (`ledger`, `events`) so they survive an app crash and
//!   the test can inspect them.
//! - Query helpers ([`ledger_balance`], [`events`]) for the tests.
//!
//! The "custom software" the tests actually drive is the `cano_workflow_app` binary
//! (`src/bin/cano_workflow_app.rs`), which wires these together behind a CLI.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use cano::prelude::*;
use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, NoTls};

/// How many units a `Reserve` step holds (and a `Charge`-style refund would return).
pub const UNITS: i64 = 5;

// ---------------------------------------------------------------------------
// Postgres-backed CheckpointStore
// ---------------------------------------------------------------------------

/// A [`CheckpointStore`] over a single Postgres table.
///
/// Table layout: `cano_checkpoints(workflow_id text, sequence bigint, state text,
/// task_id text, output_blob bytea, kind smallint NOT NULL DEFAULT 0,
/// PRIMARY KEY(workflow_id, sequence))`. The primary key enforces the
/// no-duplicate-`(workflow_id, sequence)` contract at the database level — a second run
/// that tries to share a workflow id fails its first `append` with a conflict.
///
/// The `kind` column maps [`cano::recovery::RowKind`] to a `SMALLINT`:
/// `0 = StateEntry`, `1 = CompensationCompletion`, `2 = StepCursor`. Pre-existing rows
/// (written before this column existed) read back as `StateEntry` via `DEFAULT 0`.
#[derive(Clone)]
pub struct PostgresCheckpointStore {
    client: Arc<Client>,
}

impl PostgresCheckpointStore {
    /// Connect to `dsn`, drive the connection on a background task, and ensure the schema
    /// (the checkpoint table plus the `ledger` / `events` side-effect tables) exists.
    pub async fn connect(dsn: &str) -> anyhow::Result<Self> {
        let client = connect_raw(dsn).await?;
        migrate(&client).await?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// The shared client, so the workflow tasks can write their side effects through the
    /// same connection.
    pub fn client(&self) -> Arc<Client> {
        Arc::clone(&self.client)
    }
}

async fn connect_raw(dsn: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(dsn, NoTls)
        .await
        .with_context(|| format!("connecting to {dsn}"))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("postgres connection error: {e}");
        }
    });
    Ok(client)
}

async fn migrate(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS cano_checkpoints (
                 workflow_id text     NOT NULL,
                 sequence    bigint   NOT NULL,
                 state       text     NOT NULL,
                 task_id     text     NOT NULL,
                 output_blob bytea,
                 kind        smallint NOT NULL DEFAULT 0,
                 PRIMARY KEY (workflow_id, sequence)
             );
             CREATE TABLE IF NOT EXISTS ledger (
                 workflow_id text   NOT NULL,
                 account     text   NOT NULL,
                 tag         text   NOT NULL,
                 amount      bigint NOT NULL,
                 PRIMARY KEY (workflow_id, account, tag)
             );
             CREATE TABLE IF NOT EXISTS events (
                 id          bigserial PRIMARY KEY,
                 workflow_id text NOT NULL,
                 label       text NOT NULL
             );",
        )
        .await
        .context("running schema migration")?;
    Ok(())
}

/// Map [`cano::recovery::RowKind`] to the integer stored in the `kind` column.
///
/// Encoding: `0 = StateEntry`, `1 = CompensationCompletion`, `2 = StepCursor`.
/// Because `RowKind` is `#[non_exhaustive]`, the match needs a catch-all arm — unknown
/// future variants are stored as `0` (StateEntry), which is harmless for the e2e harness.
fn row_kind_to_db(k: &cano::recovery::RowKind) -> i16 {
    match k {
        cano::recovery::RowKind::StateEntry => 0,
        cano::recovery::RowKind::CompensationCompletion => 1,
        cano::recovery::RowKind::StepCursor => 2,
        // unknown future RowKind variant — store as StateEntry; harmless in the e2e harness
        _ => 0,
    }
}

/// Map the integer from the `kind` column back to [`cano::recovery::RowKind`].
///
/// Unknown integers (from a future schema version read by an older binary) fall back to
/// `StateEntry`, which is harmless for the e2e harness.
fn row_kind_from_db(i: i16) -> cano::recovery::RowKind {
    match i {
        0 => cano::recovery::RowKind::StateEntry,
        1 => cano::recovery::RowKind::CompensationCompletion,
        2 => cano::recovery::RowKind::StepCursor,
        // unknown integer from a future schema version — treat as StateEntry; harmless
        _ => cano::recovery::RowKind::StateEntry,
    }
}

#[cano::checkpoint_store]
impl CheckpointStore for PostgresCheckpointStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let seq = row.sequence as i64;
        let kind = row_kind_to_db(&row.kind);
        let res = self
            .client
            .execute(
                "INSERT INTO cano_checkpoints \
                     (workflow_id, sequence, state, task_id, output_blob, kind) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &workflow_id,
                    &seq,
                    &row.state,
                    &row.task_id,
                    &row.output_blob,
                    &kind,
                ],
            )
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => Err(CanoError::checkpoint_store(format!(
                "checkpoint conflict: workflow {workflow_id:?} already has a row at sequence \
                 {}; resume the existing run or clear it first",
                row.sequence
            ))),
            Err(e) => Err(CanoError::checkpoint_store(format!("append: {e}"))),
        }
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        let rows = self
            .client
            .query(
                "SELECT sequence, state, task_id, output_blob, kind FROM cano_checkpoints
                 WHERE workflow_id = $1 ORDER BY sequence ASC",
                &[&workflow_id],
            )
            .await
            .map_err(|e| CanoError::checkpoint_store(format!("load_run: {e}")))?;
        Ok(rows
            .iter()
            .map(|r| {
                let seq: i64 = r.get(0);
                let kind_int: i16 = r.get(4);
                CheckpointRow {
                    sequence: seq as u64,
                    state: r.get(1),
                    task_id: r.get(2),
                    output_blob: r.get(3),
                    kind: row_kind_from_db(kind_int),
                }
            })
            .collect())
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        self.client
            .execute(
                "DELETE FROM cano_checkpoints WHERE workflow_id = $1",
                &[&workflow_id],
            )
            .await
            .map_err(|e| CanoError::checkpoint_store(format!("clear: {e}")))?;
        Ok(())
    }
}

fn is_unique_violation(e: &tokio_postgres::Error) -> bool {
    e.as_db_error()
        .map(|db| db.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Side-effect helpers (used by the workflow tasks and the tests)
// ---------------------------------------------------------------------------

/// A clone-shareable handle to the Postgres connection for the workflow tasks.
#[derive(Clone)]
pub struct Db(pub Arc<Client>);
impl Resource for Db {}

impl Db {
    async fn record_event(&self, wf: &str, label: &str) -> Result<(), CanoError> {
        self.0
            .execute(
                "INSERT INTO events (workflow_id, label) VALUES ($1, $2)",
                &[&wf, &label],
            )
            .await
            .map_err(|e| CanoError::generic(format!("record event: {e}")))?;
        Ok(())
    }
    /// Idempotent: applying the same `(account, tag)` again is a no-op.
    async fn ledger_apply(
        &self,
        wf: &str,
        account: &str,
        tag: &str,
        amount: i64,
    ) -> Result<(), CanoError> {
        self.0
            .execute(
                "INSERT INTO ledger (workflow_id, account, tag, amount) VALUES ($1,$2,$3,$4)
                 ON CONFLICT (workflow_id, account, tag) DO NOTHING",
                &[&wf, &account, &tag, &amount],
            )
            .await
            .map_err(|e| CanoError::generic(format!("ledger apply: {e}")))?;
        Ok(())
    }
    /// Idempotent: removing a `(account, tag)` that isn't there is a no-op.
    async fn ledger_revert(&self, wf: &str, account: &str, tag: &str) -> Result<(), CanoError> {
        self.0
            .execute(
                "DELETE FROM ledger WHERE workflow_id=$1 AND account=$2 AND tag=$3",
                &[&wf, &account, &tag],
            )
            .await
            .map_err(|e| CanoError::generic(format!("ledger revert: {e}")))?;
        Ok(())
    }
}

/// Net balance of `account` for `workflow_id` (sum of ledger rows).
pub async fn ledger_balance(client: &Client, workflow_id: &str, account: &str) -> i64 {
    client
        .query_one(
            "SELECT COALESCE(SUM(amount),0)::bigint FROM ledger WHERE workflow_id=$1 AND account=$2",
            &[&workflow_id, &account],
        )
        .await
        .map(|r| r.get::<_, i64>(0))
        .unwrap_or(0)
}

/// The ordered event log for `workflow_id` (`"run:Reserve"`, `"compensate:Reserve"`, …).
pub async fn events(client: &Client, workflow_id: &str) -> Vec<String> {
    client
        .query(
            "SELECT label FROM events WHERE workflow_id=$1 ORDER BY id ASC",
            &[&workflow_id],
        )
        .await
        .map(|rows| rows.iter().map(|r| r.get::<_, String>(0)).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The saga workflow
// ---------------------------------------------------------------------------

/// Saga phases. `Reserve` is compensatable; `Ship` is a plain task; `Done` is the exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
    Reserve,
    Ship,
    Done,
}

impl std::str::FromStr for Phase {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "reserve" => Phase::Reserve,
            "ship" => Phase::Ship,
            "done" => Phase::Done,
            other => anyhow::bail!("unknown phase {other:?}"),
        })
    }
}

/// Fault-injection knobs for one run.
#[derive(Clone, Default)]
pub struct Faults {
    /// This phase's `run` fails (after recording `run:<phase>`, before its real effect).
    pub fail_at: Option<Phase>,
    /// This phase's `run` records `run:<phase>` then parks forever (so a test can kill the
    /// process mid-flight, with that phase's *entry* checkpoint already durable).
    pub pause_at: Option<Phase>,
}

/// `Reserve` — compensatable. Holds [`UNITS`] of `"inventory"`; releases them on rollback.
#[derive(Clone)]
struct Reserve {
    wf: String,
    faults: Faults,
}
#[cano::saga::task(state = Phase)]
impl Reserve {
    type Output = i64;
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    fn name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Reserve")
    }
    async fn run(&self, res: &Resources) -> Result<(TaskResult<Phase>, i64), CanoError> {
        let db = res.get::<Db, _>("db")?;
        db.record_event(&self.wf, "run:Reserve").await?;
        maybe_pause(&self.faults, Phase::Reserve, &db, &self.wf).await;
        if self.faults.fail_at == Some(Phase::Reserve) {
            return Err(CanoError::task_execution("Reserve failed"));
        }
        db.ledger_apply(&self.wf, "inventory", "reserve", -UNITS)
            .await?;
        Ok((TaskResult::Single(Phase::Ship), UNITS))
    }
    async fn compensate(&self, res: &Resources, _output: i64) -> Result<(), CanoError> {
        let db = res.get::<Db, _>("db")?;
        db.record_event(&self.wf, "compensate:Reserve").await?;
        db.ledger_revert(&self.wf, "inventory", "reserve").await
    }
}

/// `Ship` — a plain task. Logs `run:Ship`, then either fails, pauses, or advances to `Done`.
#[derive(Clone)]
struct Ship {
    wf: String,
    faults: Faults,
}
#[cano::task(state = Phase)]
impl Ship {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    fn name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Ship")
    }
    async fn run(&self, res: &Resources) -> Result<TaskResult<Phase>, CanoError> {
        let db = res.get::<Db, _>("db")?;
        db.record_event(&self.wf, "run:Ship").await?;
        maybe_pause(&self.faults, Phase::Ship, &db, &self.wf).await;
        if self.faults.fail_at == Some(Phase::Ship) {
            return Err(CanoError::task_execution("Ship failed"));
        }
        Ok(TaskResult::Single(Phase::Done))
    }
}

async fn maybe_pause(faults: &Faults, here: Phase, db: &Db, wf: &str) {
    if faults.pause_at == Some(here) {
        let _ = db.record_event(wf, &format!("paused:{here:?}")).await;
        println!("PAUSED {here:?}");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        std::future::pending::<()>().await;
    }
}

/// Build the saga workflow for `workflow_id`, checkpointing to `store`, with `faults`.
pub fn build_workflow(
    store: PostgresCheckpointStore,
    workflow_id: &str,
    faults: Faults,
) -> Workflow<Phase> {
    let resources = Resources::new().insert("db", Db(store.client()));
    Workflow::new(resources)
        .register_with_compensation(
            Phase::Reserve,
            Reserve {
                wf: workflow_id.to_string(),
                faults: faults.clone(),
            },
        )
        .register(
            Phase::Ship,
            Ship {
                wf: workflow_id.to_string(),
                faults,
            },
        )
        .add_exit_state(Phase::Done)
        .with_checkpoint_store(Arc::new(store))
        .with_workflow_id(workflow_id.to_string())
}

/// Prints `CHECKPOINT <state> <seq>` / `RESUME <id> <seq>` markers so a test harness can
/// follow a run's progress over a process boundary (mirrors the in-repo `recovery_resume`).
pub struct StdoutTracer {
    last_state: std::sync::Mutex<String>,
}
impl StdoutTracer {
    pub fn new() -> Self {
        Self {
            last_state: std::sync::Mutex::new(String::new()),
        }
    }
}
impl Default for StdoutTracer {
    fn default() -> Self {
        Self::new()
    }
}
impl WorkflowObserver for StdoutTracer {
    fn on_state_enter(&self, state: &str) {
        *self.last_state.lock().unwrap() = state.to_string();
    }
    fn on_checkpoint(&self, _workflow_id: &str, sequence: u64) {
        emit(&format!(
            "CHECKPOINT {} {sequence}",
            self.last_state.lock().unwrap()
        ));
    }
    fn on_resume(&self, workflow_id: &str, sequence: u64) {
        emit(&format!("RESUME {workflow_id} {sequence}"));
    }
}

fn emit(line: &str) {
    use std::io::Write as _;
    println!("{line}");
    let _ = std::io::stdout().flush();
}

/// Sleep helper for the app's startup back-off when Postgres isn't ready yet.
pub async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

// ---------------------------------------------------------------------------
// Unit tests for the RowKind ↔ DB integer mapping (no Docker required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{row_kind_from_db, row_kind_to_db};
    use cano::recovery::RowKind;

    #[test]
    fn row_kind_to_db_known_variants() {
        assert_eq!(row_kind_to_db(&RowKind::StateEntry), 0);
        assert_eq!(row_kind_to_db(&RowKind::CompensationCompletion), 1);
        assert_eq!(row_kind_to_db(&RowKind::StepCursor), 2);
    }

    #[test]
    fn row_kind_from_db_known_values() {
        assert_eq!(row_kind_from_db(0), RowKind::StateEntry);
        assert_eq!(row_kind_from_db(1), RowKind::CompensationCompletion);
        assert_eq!(row_kind_from_db(2), RowKind::StepCursor);
    }

    #[test]
    fn row_kind_from_db_unknown_falls_back_to_state_entry() {
        assert_eq!(row_kind_from_db(99), RowKind::StateEntry);
        assert_eq!(row_kind_from_db(-1), RowKind::StateEntry);
    }

    #[test]
    fn row_kind_round_trip_all_variants() {
        for (variant, expected_int) in [
            (RowKind::StateEntry, 0i16),
            (RowKind::CompensationCompletion, 1i16),
            (RowKind::StepCursor, 2i16),
        ] {
            let db_int = row_kind_to_db(&variant);
            assert_eq!(db_int, expected_int, "to_db mismatch for {variant:?}");
            assert_eq!(
                row_kind_from_db(db_int),
                variant,
                "from_db round-trip mismatch for {variant:?}"
            );
        }
    }
}
