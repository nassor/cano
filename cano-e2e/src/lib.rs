//! Building blocks for the Docker-based end-to-end hardening tests:
//!
//! - [`connect`]: cano's [`PostgresStore`] plus a plain client for the side-effect
//!   tables. A crashed app container restarted against the *same* Postgres resumes from
//!   the store.
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
use cano::store::postgres::PostgresStore;
use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, NoTls};

/// How many units a `Reserve` step holds (and a `Charge`-style refund would return).
pub const UNITS: i64 = 5;

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Connect to `dsn`: create the `ledger` / `events` side-effect tables, then open cano's
/// [`PostgresStore`], which creates `cano_checkpoints`. Returns the store and a
/// client the workflow tasks write their side effects through.
pub async fn connect(dsn: &str) -> anyhow::Result<(PostgresStore, Arc<Client>)> {
    let client = connect_raw(dsn).await?;
    migrate(&client).await?;
    let store = PostgresStore::connect(dsn)
        .await
        .context("opening the checkpoint store")?;
    Ok((store, Arc::new(client)))
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
    // One statement per table, so a failure names the table it hit.
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS ledger (
                  workflow_id text   NOT NULL,
                  account     text   NOT NULL,
                  tag         text   NOT NULL,
                  amount      bigint NOT NULL,
                  PRIMARY KEY (workflow_id, account, tag)
              );",
        )
        .await
        .context("creating ledger table")?;
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS events (
                  id          bigserial PRIMARY KEY,
                  workflow_id text NOT NULL,
                  label       text NOT NULL
              );",
        )
        .await
        .context("creating events table")?;
    Ok(())
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
    /// Idempotent: applying the same `(account, tag)` again is a no-op because
    /// `ON CONFLICT DO NOTHING` silently discards duplicate inserts — the *first*
    /// write's `amount` wins and retries produce a conflict. This is intentional:
    /// saga steps must only write once, and compensation reverts via `ledger_revert`.
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

    fn name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Reserve")
    }
    async fn run(&self, res: &Resources) -> Result<(Phase, i64), CanoError> {
        let db = res.get::<Db>()?;
        db.record_event(&self.wf, "run:Reserve").await?;
        maybe_pause(&self.faults, Phase::Reserve, db, &self.wf).await;
        if self.faults.fail_at == Some(Phase::Reserve) {
            return Err(CanoError::task_execution("Reserve failed"));
        }
        db.ledger_apply(&self.wf, "inventory", "reserve", -UNITS)
            .await?;
        Ok((Phase::Ship, UNITS))
    }
    async fn compensate(&self, res: &Resources, _output: i64) -> Result<(), CanoError> {
        let db = res.get::<Db>()?;
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
    fn name(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Ship")
    }
    async fn run(&self, res: &Resources) -> Result<Phase, CanoError> {
        let db = res.get::<Db>()?;
        db.record_event(&self.wf, "run:Ship").await?;
        maybe_pause(&self.faults, Phase::Ship, db, &self.wf).await;
        if self.faults.fail_at == Some(Phase::Ship) {
            return Err(CanoError::task_execution("Ship failed"));
        }
        Ok(Phase::Done)
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

/// Build the saga workflow for `workflow_id`, checkpointing to `store` and writing side
/// effects through `db`, with `faults`.
pub fn build_workflow(
    store: PostgresStore,
    db: Arc<Client>,
    workflow_id: &str,
    faults: Faults,
) -> Workflow<Phase> {
    let resources = Resources::new().insert(Db(db));
    Workflow::new(resources)
        .register(
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
