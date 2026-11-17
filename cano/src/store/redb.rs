//! [`RedbStore`]: the default store, an embedded [`redb`] database file.
//!
//! [`redb`](https://docs.rs/redb) is an embedded, ACID, pure-Rust key-value store
//! with no background daemon, a good fit for state that lives in the same process as
//! the workflow engine. Compiled with the `store-redb` feature, on by default.
//! [`RedbStore`] implements [`CheckpointStore`] and, with the `scheduler` feature,
//! `SchedulerStore`.
//!
//! ## Checkpoint layout
//!
//! One table, `cano_checkpoints`, maps `(workflow_id, sequence)` to a
//! version-tagged, [`postcard`]-encoded [`StoredRow`] (the [`CheckpointRow`]
//! fields *other than* `sequence` — which is already the second key component, so
//! there's no need to store it twice). Every row written by this version of the
//! store is prefixed with the two-byte magic [`ROW_TAG_V1`] (`b"CV"`) so
//! `load_run` can tell versioned rows apart from legacy rows written before
//! tagging *without guessing from decode success*: a corrupt versioned row fails
//! loudly instead of being silently misread as a plausible-looking legacy row.
//! Untagged values (pre-tagging databases) are decoded with the old heuristic —
//! current shape first, then the pre-`workflow_version` shape. redb orders
//! composite keys element by element, so within one `workflow_id` the rows are
//! stored — and range-scanned — in ascending `sequence` order, which is exactly
//! what [`CheckpointStore::load_run`] returns.
//!
//! ## Scheduler layout
//!
//! With the `scheduler` feature, two more tables hold the scheduler's flow records.
//! `cano_scheduler` maps a flow id to `(version, value)`. `cano_scheduler_version` has a
//! single row with the last version handed out, so a version is never reused for an id,
//! even after the record is removed and created again. A compare-and-swap is one write
//! transaction: it reads the record, compares its version with the expected one, bumps the
//! counter, writes the record and commits.
//!
//! A redb file is held by one process. A `RedbStore` therefore coordinates schedulers
//! inside one process and keeps their state across restarts, but it cannot coordinate
//! several processes; that needs `PostgresStore` or `NatsStore`.

use std::ffi::{OsStr, OsString};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "scheduler")]
use redb::ReadableTable;
use redb::{Database, ReadableDatabase, TableDefinition};

use super::codec::{self, ROW_TAG_V1, StoredRow};
use super::conflict_error;
use crate::checkpoint::{CheckpointRow, CheckpointStore, RowKind};
use crate::error::CanoError;
#[cfg(feature = "scheduler")]
use crate::scheduler::{CasOutcome, SchedulerStore, VersionedRecord};
use cano_macros::checkpoint_store;
#[cfg(feature = "scheduler")]
use cano_macros::scheduler_store;

/// `(workflow_id, sequence) -> [ROW_TAG_V1][postcard(StoredRow)]` (or an untagged
/// legacy payload for databases written before version tagging).
const CHECKPOINTS: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("cano_checkpoints");

/// `flow_id -> (version, value)`: the scheduler's flow records.
#[cfg(feature = "scheduler")]
const SCHEDULER: TableDefinition<&str, (u64, &[u8])> = TableDefinition::new("cano_scheduler");

/// One row, [`SCHEDULER_VERSION_KEY`] -> the last version handed out. Never reset, so
/// versions stay unique per flow id across remove and re-create.
#[cfg(feature = "scheduler")]
const SCHEDULER_VERSION: TableDefinition<&str, u64> =
    TableDefinition::new("cano_scheduler_version");

/// The key of the single row in [`SCHEDULER_VERSION`].
#[cfg(feature = "scheduler")]
const SCHEDULER_VERSION_KEY: &str = "last";

/// Pre-versioning on-disk shape. Kept solely as a decode fallback so existing
/// redb databases written before `workflow_version` was added still load.
#[derive(Debug, serde::Deserialize)]
struct StoredRowV0 {
    state: String,
    task_id: String,
    output_blob: Option<Vec<u8>>,
    kind: RowKind,
}

/// The key range covering every row for `workflow_id`, in ascending `sequence` order.
fn workflow_range(workflow_id: &str) -> RangeInclusive<(&str, u64)> {
    (workflow_id, u64::MIN)..=(workflow_id, u64::MAX)
}

/// Wrap any `redb` error (they all implement [`Display`](std::fmt::Display)) as a
/// [`CanoError::CheckpointStore`].
fn redb_err(e: impl std::fmt::Display) -> CanoError {
    CanoError::CheckpointStore(format!("redb: {e}"))
}

/// Wrap any `redb` error as a [`CanoError::SchedulerStore`].
#[cfg(feature = "scheduler")]
fn scheduler_err(e: impl std::fmt::Display) -> CanoError {
    CanoError::scheduler_store(format!("redb: {e}"))
}

/// Assemble a [`CheckpointRow`] from the decoded stored fields.
///
/// Every decode path in `load_run` produces the same five fields; only
/// `workflow_version` differs (`0` for pre-versioning rows). Kept as one
/// function so the field-order contract between the on-disk shape and the
/// public row lives in a single place.
fn stored_to_row(
    sequence: u64,
    state: String,
    task_id: String,
    output_blob: Option<Vec<u8>>,
    kind: RowKind,
    workflow_version: u32,
) -> CheckpointRow {
    CheckpointRow {
        sequence,
        state,
        task_id,
        output_blob,
        kind,
        workflow_version,
    }
}

/// The embedded store: one `redb` database file. Implements [`CheckpointStore`] and, with
/// the `scheduler` feature, `SchedulerStore` (tables `cano_checkpoints` and
/// `cano_scheduler`).
///
/// A redb file is held by one process, so the scheduler side coordinates schedulers inside
/// one process and persists their state across restarts; it cannot coordinate several
/// processes. Use `PostgresStore` or `NatsStore` for that.
///
/// Cheap to clone — the database handle is held behind an `Arc`, so every clone
/// shares the same file and write lock. Construct one with [`new`](Self::new) at a path you
/// choose, or with [`open_default`](Self::open_default) at a persistent per-user or
/// per-service location; both create the file (if absent) and its tables so that
/// later reads never trip over a missing table.
#[derive(Clone)]
pub struct RedbStore {
    db: Arc<Database>,
}

impl RedbStore {
    /// File name of the database inside the default directory.
    pub const DEFAULT_FILE_NAME: &str = "cano.redb";

    /// Environment variable that overrides the default directory.
    pub const DATA_DIR_ENV: &str = "CANO_DATA_DIR";

    /// Open (creating if necessary) the `redb` database at `path` and ensure the
    /// checkpoint table (and, with the `scheduler` feature, the scheduler tables) exists.
    /// The parent directory must exist.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, CanoError> {
        let db = Database::create(path).map_err(redb_err)?;
        // Materialize the tables up front so `begin_read` + `open_table` in
        // `load_run` and `load` cannot fail on a freshly created database.
        let tx = db.begin_write().map_err(redb_err)?;
        {
            let _ = tx.open_table(CHECKPOINTS).map_err(redb_err)?;
            #[cfg(feature = "scheduler")]
            {
                let _ = tx.open_table(SCHEDULER).map_err(redb_err)?;
                let _ = tx.open_table(SCHEDULER_VERSION).map_err(redb_err)?;
            }
        }
        tx.commit().map_err(redb_err)?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Open the store at [`default_path`](Self::default_path), creating its directory when
    /// missing.
    ///
    /// The directory is the first of these that applies; an empty variable counts as unset:
    ///
    /// 1. `CANO_DATA_DIR` ([`DATA_DIR_ENV`](Self::DATA_DIR_ENV)), used as is, on every platform.
    /// 2. On Linux and other Unix systems except macOS, `STATE_DIRECTORY`, which systemd sets
    ///    for a unit with `StateDirectory=`: its first entry, then `cano`.
    /// 3. A per-user directory, then `cano/<executable file name>`:
    ///    `$XDG_STATE_HOME` when it is an absolute path, else `~/.local/state`, on Linux and
    ///    other Unix systems; `~/Library/Application Support` on macOS; `%LOCALAPPDATA%`,
    ///    else `~\AppData\Local`, on Windows.
    ///
    /// The database file inside it is [`DEFAULT_FILE_NAME`](Self::DEFAULT_FILE_NAME). The
    /// log survives restarts.
    ///
    /// redb locks the file, so one process holds it at a time: a second open fails while a
    /// store on that file is alive (clones share one handle). The per-user path contains the
    /// executable's file name, so renaming the binary starts a new file. A service sets
    /// `CANO_DATA_DIR`, or runs under systemd with `StateDirectory=`.
    ///
    /// # Errors
    ///
    /// [`CanoError::Configuration`] when rule 3 applies and the home directory or the
    /// executable name is unknown; [`CanoError::CheckpointStore`] when the directory cannot
    /// be created or the database cannot be opened.
    pub fn open_default() -> Result<Self, CanoError> {
        Self::open_in(&current_default_dir()?)
    }

    /// The file [`open_default`](Self::open_default) opens. Reads the environment only; no
    /// file system access.
    ///
    /// # Errors
    ///
    /// [`CanoError::Configuration`] under the same condition as `open_default`.
    pub fn default_path() -> Result<PathBuf, CanoError> {
        Ok(current_default_dir()?.join(Self::DEFAULT_FILE_NAME))
    }

    /// Create `dir` and its parents when missing, then open [`DEFAULT_FILE_NAME`](Self::DEFAULT_FILE_NAME) in it.
    fn open_in(dir: &Path) -> Result<Self, CanoError> {
        std::fs::create_dir_all(dir).map_err(|e| {
            CanoError::CheckpointStore(format!(
                "create checkpoint directory {}: {e}",
                dir.display()
            ))
        })?;
        Self::new(dir.join(Self::DEFAULT_FILE_NAME))
    }
}

/// The operating system family whose conventions pick the per-user directory.
#[derive(Clone, Copy)]
enum Platform {
    Unix,
    MacOs,
    Windows,
}

impl Platform {
    const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else if cfg!(target_os = "macos") {
        Self::MacOs
    } else {
        Self::Unix
    };
}

/// [`default_dir`] for this process: its environment, home directory and executable.
fn current_default_dir() -> Result<PathBuf, CanoError> {
    default_dir(
        Platform::CURRENT,
        &|key| std::env::var_os(key),
        std::env::home_dir(),
        exe_name().as_deref(),
    )
}

/// The directory rules of [`RedbStore::open_default`], with every input passed in
/// so each platform's rules are testable on any host. Paths are built with `join` and the
/// platform checks avoid `Path::is_absolute` and `split_paths`, whose meaning follows the host.
fn default_dir(
    platform: Platform,
    env: &dyn Fn(&str) -> Option<OsString>,
    home: Option<PathBuf>,
    app: Option<&OsStr>,
) -> Result<PathBuf, CanoError> {
    let var = |key: &str| env(key).filter(|value| !value.is_empty());

    if let Some(dir) = var(RedbStore::DATA_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    if let Platform::Unix = platform
        && let Some(dirs) = var("STATE_DIRECTORY")
    {
        let first = match dirs.to_str() {
            Some(dirs) => PathBuf::from(dirs.split(':').next().unwrap_or_default()),
            // Only a Unix host sets this variable, and there `split_paths` splits on `:`.
            None => std::env::split_paths(&dirs).next().unwrap_or_default(),
        };
        if !first.as_os_str().is_empty() {
            return Ok(first.join("cano"));
        }
    }

    let home = home.filter(|home| !home.as_os_str().is_empty());
    let base = match platform {
        Platform::Unix => var("XDG_STATE_HOME")
            .filter(|dir| dir.as_encoded_bytes().first() == Some(&b'/'))
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join(".local").join("state"))),
        Platform::MacOs => home.map(|home| home.join("Library").join("Application Support")),
        Platform::Windows => var("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join("AppData").join("Local"))),
    };
    match (base, app) {
        (Some(base), Some(app)) => Ok(base.join("cano").join(app)),
        _ => Err(CanoError::configuration(
            "no default checkpoint directory: set CANO_DATA_DIR or pass a path to \
            RedbStore::new",
        )),
    }
}

/// The running executable's file name without its extension.
fn exe_name() -> Option<OsString> {
    Some(std::env::current_exe().ok()?.file_stem()?.to_os_string())
}

#[checkpoint_store]
impl CheckpointStore for RedbStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let sequence = row.sequence;
        // Serialize *before* handing off to the blocking thread so heap
        // allocation does not hold the redb write lock open. On failure the
        // transaction is never begun, avoiding unnecessary lock contention.
        // The payload carries the version tag so `load_run` never has to guess the
        // on-disk shape from decode success — a corrupt tagged row is reported
        // loudly instead of being misread as a legacy row (see `ROW_TAG_V1`).
        let tagged = codec::encode(row)?;

        // redb is a synchronous embedded store: `begin_write` + `insert` + `commit`
        // (a full fsync at the default `Durability::Immediate`) block the calling
        // thread. Running the transaction on a blocking thread keeps the fsync and
        // the single global write lock off the Tokio worker that awaits us from the
        // FSM loop (once per state entry) — otherwise every checkpoint transition
        // stalls a runtime worker and serializes concurrent appends onto it.
        let db = Arc::clone(&self.db);
        let wf_id = workflow_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), CanoError> {
            let tx = db.begin_write().map_err(redb_err)?;
            {
                let mut table = tx.open_table(CHECKPOINTS).map_err(redb_err)?;
                // Reject a duplicate `(workflow_id, sequence)`: `insert` would silently
                // overwrite the existing row. That only happens when two runs share a
                // `workflow_id` (a misuse — `resume_from` the existing run, or `clear` it
                // first), so surface it instead of corrupting the log. The uncommitted write
                // is rolled back when `tx` drops on the early return. (`resume_from` always
                // appends *new* sequences, so a legitimate resume never trips this.)
                if table
                    .insert((wf_id.as_str(), sequence), tagged.as_slice())
                    .map_err(redb_err)?
                    .is_some()
                {
                    return Err(conflict_error(&wf_id, sequence));
                }
            }
            tx.commit().map_err(redb_err)?;
            Ok(())
        })
        .await
        .map_err(|e| CanoError::CheckpointStore(format!("checkpoint append task panicked: {e}")))?
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        // Range scan + decode on a blocking thread — same rationale as
        // `append`: redb's read transaction is synchronous, and a long log
        // would otherwise pin a Tokio worker for the whole scan.
        let db = Arc::clone(&self.db);
        let wf_id = workflow_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<CheckpointRow>, CanoError> {
            let tx = db.begin_read().map_err(redb_err)?;
            let table = tx.open_table(CHECKPOINTS).map_err(redb_err)?;

            // Fixed capacity heuristic: 64 rows covers typical runs (one state-entry row
            // per state plus a handful of cursor rows); longer runs just reallocate.
            let mut rows = Vec::with_capacity(64);
            for entry in table.range(workflow_range(&wf_id)).map_err(redb_err)? {
                let (key, value) = entry.map_err(redb_err)?;
                let sequence = key.value().1;
                let bytes = value.value();
                let row = if bytes.starts_with(&ROW_TAG_V1) {
                    // Explicitly versioned row. A decode failure here is genuine
                    // corruption — surface it instead of falling back to the legacy
                    // shape (the V0 fallback would silently mask a corrupt V1 row
                    // as a plausible-looking legacy one).
                    codec::decode(&wf_id, sequence, bytes)?
                } else {
                    // Untagged row written before version tagging: try the current
                    // shape, then the pre-`workflow_version` shape. Only this legacy
                    // path keeps the heuristic fallback.
                    match postcard::from_bytes::<StoredRow>(bytes) {
                        Ok(payload) => stored_to_row(
                            sequence,
                            payload.state,
                            payload.task_id,
                            payload.output_blob,
                            payload.kind,
                            payload.workflow_version,
                        ),
                        Err(new_err) => match postcard::from_bytes::<StoredRowV0>(bytes) {
                            Ok(legacy) => stored_to_row(
                                sequence,
                                legacy.state,
                                legacy.task_id,
                                legacy.output_blob,
                                legacy.kind,
                                0,
                            ),
                            Err(_) => {
                                // Surface both workflow id and sequence so an operator
                                // can locate the bad row directly via redb CLI or repair
                                // tooling — the postcard error alone leaves no anchor.
                                return Err(CanoError::CheckpointStore(format!(
                                    "decode checkpoint row at workflow_id={wf_id:?} sequence={sequence}: {new_err}"
                                )));
                            }
                        },
                    }
                };
                rows.push(row);
            }
            Ok(rows)
        })
        .await
        .map_err(|e| {
            CanoError::CheckpointStore(format!("checkpoint load task panicked: {e}"))
        })?
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        let db = Arc::clone(&self.db);
        let wf_id = workflow_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), CanoError> {
            let tx = db.begin_write().map_err(redb_err)?;
            {
                let mut table = tx.open_table(CHECKPOINTS).map_err(redb_err)?;
                table
                    .retain_in(workflow_range(&wf_id), |_, _| false)
                    .map_err(redb_err)?;
            }
            tx.commit().map_err(redb_err)?;
            Ok(())
        })
        .await
        .map_err(|e| CanoError::CheckpointStore(format!("checkpoint clear task panicked: {e}")))?
    }
}

#[cfg(feature = "scheduler")]
#[scheduler_store]
impl SchedulerStore for RedbStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        // Blocking redb read on a blocking thread, as in `load_run`.
        let db = Arc::clone(&self.db);
        let id = flow_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<VersionedRecord>, CanoError> {
            let tx = db.begin_read().map_err(scheduler_err)?;
            let table = tx.open_table(SCHEDULER).map_err(scheduler_err)?;
            let record = table.get(id.as_str()).map_err(scheduler_err)?.map(|guard| {
                let (version, value) = guard.value();
                VersionedRecord {
                    version,
                    value: value.to_vec(),
                }
            });
            Ok(record)
        })
        .await
        .map_err(|e| CanoError::scheduler_store(format!("scheduler load task panicked: {e}")))?
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        // Read, compare, bump the counter, write and commit in one write transaction.
        // redb allows one writer at a time, so of concurrent swaps from one version
        // exactly one sees the expected version.
        let db = Arc::clone(&self.db);
        let id = flow_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<CasOutcome, CanoError> {
            let tx = db.begin_write().map_err(scheduler_err)?;
            let next = {
                let mut records = tx.open_table(SCHEDULER).map_err(scheduler_err)?;
                let current = records
                    .get(id.as_str())
                    .map_err(scheduler_err)?
                    .map(|guard| guard.value().0);
                if current != expected {
                    // Nothing was written; the transaction rolls back when `tx` drops.
                    return Ok(CasOutcome::Conflict);
                }
                let mut counter = tx.open_table(SCHEDULER_VERSION).map_err(scheduler_err)?;
                let last = counter
                    .get(SCHEDULER_VERSION_KEY)
                    .map_err(scheduler_err)?
                    .map_or(0, |guard| guard.value());
                let next = last
                    .checked_add(1)
                    .ok_or_else(|| scheduler_err("version counter exhausted"))?;
                counter
                    .insert(SCHEDULER_VERSION_KEY, next)
                    .map_err(scheduler_err)?;
                records
                    .insert(id.as_str(), (next, value.as_slice()))
                    .map_err(scheduler_err)?;
                next
            };
            tx.commit().map_err(scheduler_err)?;
            Ok(CasOutcome::Written(next))
        })
        .await
        .map_err(|e| CanoError::scheduler_store(format!("scheduler swap task panicked: {e}")))?
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        // Only the record goes; the version counter stays so a re-created record never
        // reuses an earlier version.
        let db = Arc::clone(&self.db);
        let id = flow_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), CanoError> {
            let tx = db.begin_write().map_err(scheduler_err)?;
            {
                let mut table = tx.open_table(SCHEDULER).map_err(scheduler_err)?;
                table.remove(id.as_str()).map_err(scheduler_err)?;
            }
            tx.commit().map_err(scheduler_err)?;
            Ok(())
        })
        .await
        .map_err(|e| CanoError::scheduler_store(format!("scheduler remove task panicked: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn append_load_clear_roundtrip() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();

        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        store
            .append("run", CheckpointRow::new(1, "B", "t1"))
            .await
            .unwrap();
        store
            .append(
                "run",
                CheckpointRow::new(2, "C", "t2").with_output(vec![7, 8, 9]),
            )
            .await
            .unwrap();

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(rows[0].state, "A");
        assert_eq!(rows[2].task_id, "t2");
        assert_eq!(rows[2].output_blob.as_deref(), Some(&[7u8, 8, 9][..]));

        store.clear("run").await.unwrap();
        assert!(store.load_run("run").await.unwrap().is_empty());
        // Clearing an unknown id is a no-op.
        store.clear("missing").await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_row_decode_error_names_workflow_id_and_sequence() {
        // A corrupt row's decode error must carry both the workflow id and
        // the row sequence so an operator can locate it without having to
        // iterate the redb file by hand.
        let dir = tempdir().unwrap();
        let path = dir.path().join("ckpt.redb");
        let store = RedbStore::new(&path).unwrap();

        // Append three valid rows.
        for seq in 0u64..=2 {
            store
                .append("run", CheckpointRow::new(seq, format!("S{seq}"), "t"))
                .await
                .unwrap();
        }

        // Inject a garbage payload at sequence 1 by reaching into the redb
        // table directly. The bytes are neither valid `StoredRow` nor valid
        // `StoredRowV0` so both decode paths fail.
        {
            let tx = store.db.begin_write().unwrap();
            {
                let mut table = tx.open_table(CHECKPOINTS).unwrap();
                let garbage: &[u8] = &[0xFFu8; 8];
                table.insert(("run", 1u64), garbage).unwrap();
            }
            tx.commit().unwrap();
        }

        let err = store.load_run("run").await.unwrap_err();
        let msg = err.message();
        assert!(msg.contains("\"run\""), "missing workflow_id in {msg:?}");
        assert!(msg.contains("sequence=1"), "missing sequence in {msg:?}");
    }

    #[tokio::test]
    async fn legacy_untagged_rows_still_load_with_version_zero() {
        // Databases written before version tagging store raw postcard(StoredRowV0)
        // bytes with no tag byte. They must keep loading, with workflow_version 0.
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();

        // Mirror of the pre-versioning on-disk shape with the same field order
        // and types, so `StoredRowV0` (intentionally Deserialize-only, it exists
        // solely as a decode fallback) stays that way.
        #[derive(serde::Serialize)]
        struct LegacyRow {
            state: String,
            task_id: String,
            output_blob: Option<Vec<u8>>,
            kind: crate::checkpoint::RowKind,
        }
        let legacy = LegacyRow {
            state: "Start".to_string(),
            task_id: "t0".to_string(),
            output_blob: None,
            kind: crate::checkpoint::RowKind::StateEntry,
        };
        let bytes = postcard::to_stdvec(&legacy).unwrap();
        assert!(!bytes.starts_with(&ROW_TAG_V1), "fixture must be untagged");
        {
            let tx = store.db.begin_write().unwrap();
            {
                let mut table = tx.open_table(CHECKPOINTS).unwrap();
                table
                    .insert(("legacy-run", 0u64), bytes.as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }

        let rows = store.load_run("legacy-run").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, "Start");
        assert_eq!(rows[0].workflow_version, 0);
    }

    #[tokio::test]
    async fn corrupt_tagged_row_errors_loudly_instead_of_masking_as_v0() {
        // A row stamped with ROW_TAG_V1 but holding a garbage payload must surface
        // corruption, never silently fall back to the legacy V0 shape — that mask
        // is the exact hazard the version tag exists to close.
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();

        let corrupt: Vec<u8> = ROW_TAG_V1
            .iter()
            .copied()
            .chain([0xDE, 0xAD, 0xBE, 0xEF])
            .collect();
        {
            let tx = store.db.begin_write().unwrap();
            {
                let mut table = tx.open_table(CHECKPOINTS).unwrap();
                table
                    .insert(("corrupt-run", 0u64), corrupt.as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }

        let err = store.load_run("corrupt-run").await.unwrap_err();
        assert!(
            err.message().contains("decode checkpoint row"),
            "expected a loud decode error, got: {err}"
        );
    }

    #[tokio::test]
    async fn rows_ordered_by_sequence_regardless_of_insert_order() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();

        for seq in [5u64, 1, 9, 0, 3] {
            store
                .append("run", CheckpointRow::new(seq, format!("S{seq}"), "t"))
                .await
                .unwrap();
        }

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1, 3, 5, 9]
        );
    }

    #[tokio::test]
    async fn rows_survive_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ckpt.redb");
        {
            let store = RedbStore::new(&path).unwrap();
            store
                .append("run", CheckpointRow::new(0, "A", "t0"))
                .await
                .unwrap();
            store
                .append("run", CheckpointRow::new(1, "B", "t1"))
                .await
                .unwrap();
        }
        // New instance over the same file — committed rows must still be there.
        let reopened = RedbStore::new(&path).unwrap();
        let rows = reopened.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(rows[1].state, "B");
    }

    #[tokio::test]
    async fn clear_isolates_workflows() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();

        store
            .append("a", CheckpointRow::new(0, "A0", "t"))
            .await
            .unwrap();
        store
            .append("a", CheckpointRow::new(1, "A1", "t"))
            .await
            .unwrap();
        store
            .append("b", CheckpointRow::new(0, "B0", "t"))
            .await
            .unwrap();

        store.clear("a").await.unwrap();

        assert!(store.load_run("a").await.unwrap().is_empty());
        let b = store.load_run("b").await.unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].state, "B0");
    }

    #[tokio::test]
    async fn load_run_unknown_id_is_empty() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();
        assert!(store.load_run("nope").await.unwrap().is_empty());
    }

    // -- hardening ---------------------------------------------------------

    #[tokio::test]
    async fn append_rejects_duplicate_sequence() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();

        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        let err = store
            .append("run", CheckpointRow::new(0, "A-again", "t0"))
            .await
            .expect_err("duplicate (workflow_id, sequence) must be rejected");
        assert_eq!(err.category(), "checkpoint_store");
        assert!(
            err.message().contains("conflict"),
            "unexpected message: {err}"
        );

        // The original row survives the rejected write; new sequences still append.
        store
            .append("run", CheckpointRow::new(1, "B", "t1"))
            .await
            .unwrap();
        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.sequence, r.state.clone()))
                .collect::<Vec<_>>(),
            vec![(0, "A".to_string()), (1, "B".to_string())]
        );
        // A different workflow id can reuse sequence 0 freely.
        store
            .append("other", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_distinct_ids_stay_isolated_and_monotonic() {
        let dir = tempdir().unwrap();
        let store = Arc::new(RedbStore::new(dir.path().join("ckpt.redb")).unwrap());

        const RUNS: u64 = 16;
        const ROWS_PER_RUN: u64 = 12;

        let mut handles = Vec::new();
        for r in 0..RUNS {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let id = format!("run-{r}");
                for s in 0..ROWS_PER_RUN {
                    store
                        .append(&id, CheckpointRow::new(s, format!("S{r}-{s}"), "t"))
                        .await
                        .unwrap();
                    // Yield so the redb write lock changes hands between runs.
                    tokio::task::yield_now().await;
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        for r in 0..RUNS {
            let rows = store.load_run(&format!("run-{r}")).await.unwrap();
            assert_eq!(rows.len() as u64, ROWS_PER_RUN, "run {r} row count");
            assert_eq!(
                rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
                (0..ROWS_PER_RUN).collect::<Vec<_>>(),
                "run {r} sequences"
            );
            assert!(
                rows.iter()
                    .enumerate()
                    .all(|(i, row)| row.state == format!("S{r}-{i}")),
                "run {r} rows belong only to that run"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_same_id_distinct_sequences_all_land() {
        let dir = tempdir().unwrap();
        let store = Arc::new(RedbStore::new(dir.path().join("ckpt.redb")).unwrap());

        const N: u64 = 32;
        let mut handles = Vec::new();
        for s in 0..N {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                store
                    .append("run", CheckpointRow::new(s, format!("S{s}"), "t"))
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            (0..N).collect::<Vec<_>>(),
            "every distinct sequence landed exactly once, in order"
        );
    }

    #[tokio::test]
    async fn large_output_blob_roundtrips() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();
        let blob: Vec<u8> = (0..5 * 1024 * 1024usize).map(|i| (i % 251) as u8).collect();
        store
            .append(
                "run",
                CheckpointRow::new(0, "Big", "t").with_output(blob.clone()),
            )
            .await
            .unwrap();
        let rows = store.load_run("run").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].output_blob.as_deref(), Some(blob.as_slice()));
    }

    #[tokio::test]
    async fn corrupted_stored_row_is_a_decode_error_not_a_panic() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();
        store
            .append("run", CheckpointRow::new(0, "A", "t0"))
            .await
            .unwrap();
        // Overwrite sequence 1's slot with bytes that aren't a valid `StoredRow`.
        // (We reach past the public API on purpose to simulate an on-disk corruption.)
        {
            let tx = store.db.begin_write().unwrap();
            {
                let mut table = tx.open_table(CHECKPOINTS).unwrap();
                table
                    .insert(("run", 1u64), [0xFFu8, 0xFF, 0xFF, 0xFF].as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }
        let err = store
            .load_run("run")
            .await
            .expect_err("a corrupted row must surface as an error, not panic");
        assert_eq!(err.category(), "checkpoint_store");
        assert!(
            err.message().contains("decode"),
            "unexpected message: {err}"
        );
    }

    // -- RowKind round-trip ---------------------------------------------------

    /// Every `RowKind` variant must survive an append → load_run cycle with
    /// their kind and output_blob intact.
    #[tokio::test]
    async fn all_row_kinds_roundtrip() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();

        // StateEntry: kind=StateEntry, output_blob=None
        store
            .append("run", CheckpointRow::new(0, "A", "task-a"))
            .await
            .unwrap();

        // CompensationCompletion: kind=CompensationCompletion, output_blob=Some
        store
            .append(
                "run",
                CheckpointRow::new(1, "B", "task-b").with_output(vec![1, 2, 3]),
            )
            .await
            .unwrap();

        // StepCursor: kind=StepCursor, output_blob=Some
        store
            .append(
                "run",
                CheckpointRow::new(2, "C", "task-c").with_cursor(vec![4, 5]),
            )
            .await
            .unwrap();

        // Suspended: kind=Suspended, output_blob=None
        store
            .append("run", CheckpointRow::new(3, "D", "task-d").suspended())
            .await
            .unwrap();

        // ResumeInput: kind=ResumeInput, output_blob=Some
        store
            .append(
                "run",
                CheckpointRow::new(4, "E", "task-e").with_resume_input(vec![6]),
            )
            .await
            .unwrap();

        let rows = store.load_run("run").await.unwrap();
        assert_eq!(rows.len(), 5);

        assert_eq!(rows[0].sequence, 0);
        assert_eq!(rows[0].state, "A");
        assert_eq!(rows[0].kind, RowKind::StateEntry);
        assert_eq!(rows[0].output_blob, None);

        assert_eq!(rows[1].sequence, 1);
        assert_eq!(rows[1].state, "B");
        assert_eq!(rows[1].kind, RowKind::CompensationCompletion);
        assert_eq!(rows[1].output_blob.as_deref(), Some(&[1u8, 2, 3][..]));

        assert_eq!(rows[2].sequence, 2);
        assert_eq!(rows[2].state, "C");
        assert_eq!(rows[2].kind, RowKind::StepCursor);
        assert_eq!(rows[2].output_blob.as_deref(), Some(&[4u8, 5][..]));

        assert_eq!(rows[3].sequence, 3);
        assert_eq!(rows[3].kind, RowKind::Suspended);
        assert_eq!(rows[3].output_blob, None);

        assert_eq!(rows[4].sequence, 4);
        assert_eq!(rows[4].kind, RowKind::ResumeInput);
        assert_eq!(rows[4].output_blob.as_deref(), Some(&[6u8][..]));
    }

    // -- workflow_version persistence ---------------------------------------

    #[tokio::test]
    async fn workflow_version_roundtrips_through_redb_store() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();
        let row = CheckpointRow::new(0, "Start", "task").with_workflow_version(7);
        store.append("wf-roundtrip", row).await.unwrap();
        let loaded = store.load_run("wf-roundtrip").await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].workflow_version, 7);
    }

    #[tokio::test]
    async fn legacy_stored_row_decodes_as_workflow_version_zero() {
        // Mirror of the pre-versioning on-disk shape so the test can *write*
        // a legacy blob. The production `StoredRowV0` is Deserialize-only by
        // design (it only ever has to read old data), so we define a local
        // serializable twin here with the exact same field order.
        #[derive(serde::Serialize)]
        struct LegacyWrite {
            state: String,
            task_id: String,
            output_blob: Option<Vec<u8>>,
            kind: RowKind,
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("ckpt.redb");

        // Write a V0-shape blob (no workflow_version field) directly into the
        // table to simulate a database written before versioning existed.
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(CHECKPOINTS).unwrap();
                let legacy = LegacyWrite {
                    state: "Start".into(),
                    task_id: "task".into(),
                    output_blob: None,
                    kind: RowKind::StateEntry,
                };
                let bytes = postcard::to_stdvec(&legacy).unwrap();
                table.insert(("wf-legacy", 0u64), bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }

        // Now open the store the normal way and confirm the legacy row decodes
        // with workflow_version defaulted to 0.
        let store = RedbStore::new(&path).unwrap();
        let rows = store.load_run("wf-legacy").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sequence, 0);
        assert_eq!(rows[0].state, "Start");
        assert_eq!(rows[0].task_id, "task");
        assert_eq!(rows[0].kind, RowKind::StateEntry);
        assert_eq!(rows[0].workflow_version, 0);
    }

    #[tokio::test]
    async fn meets_the_checkpoint_store_contract() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("ckpt.redb")).unwrap();
        crate::testing::assert_checkpoint_store_contract(&store, "contract").await;
    }

    const PLATFORMS: [Platform; 3] = [Platform::Unix, Platform::MacOs, Platform::Windows];

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    fn dir_for(
        platform: Platform,
        pairs: &'static [(&'static str, &'static str)],
    ) -> Result<PathBuf, CanoError> {
        default_dir(
            platform,
            &env_of(pairs),
            Some(PathBuf::from("/home/u")),
            Some(OsStr::new("svc")),
        )
    }

    #[test]
    fn data_dir_env_wins_on_every_platform() {
        let env = &[
            ("CANO_DATA_DIR", "/srv/ckpt"),
            ("STATE_DIRECTORY", "/var/lib/app"),
            ("XDG_STATE_HOME", "/x"),
            ("LOCALAPPDATA", "/l"),
        ];
        for platform in PLATFORMS {
            assert_eq!(dir_for(platform, env).unwrap(), PathBuf::from("/srv/ckpt"));
        }
    }

    #[test]
    fn systemd_state_directory_uses_the_first_entry() {
        let env = &[("STATE_DIRECTORY", "/var/lib/app:/var/lib/other")];
        assert_eq!(
            dir_for(Platform::Unix, env).unwrap(),
            PathBuf::from("/var/lib/app").join("cano")
        );
        assert_eq!(
            dir_for(Platform::MacOs, env).unwrap(),
            PathBuf::from("/home/u")
                .join("Library")
                .join("Application Support")
                .join("cano")
                .join("svc")
        );
        assert_eq!(
            dir_for(Platform::Windows, env).unwrap(),
            PathBuf::from("/home/u")
                .join("AppData")
                .join("Local")
                .join("cano")
                .join("svc")
        );
    }

    #[test]
    fn unix_uses_xdg_state_home_then_home() {
        assert_eq!(
            dir_for(Platform::Unix, &[("XDG_STATE_HOME", "/x/state")]).unwrap(),
            PathBuf::from("/x/state").join("cano").join("svc")
        );
        let fallback = PathBuf::from("/home/u")
            .join(".local")
            .join("state")
            .join("cano")
            .join("svc");
        assert_eq!(dir_for(Platform::Unix, &[]).unwrap(), fallback);
        assert_eq!(
            dir_for(Platform::Unix, &[("XDG_STATE_HOME", "")]).unwrap(),
            fallback
        );
        assert_eq!(
            dir_for(Platform::Unix, &[("XDG_STATE_HOME", "state")]).unwrap(),
            fallback
        );
    }

    #[test]
    fn macos_uses_application_support() {
        assert_eq!(
            dir_for(Platform::MacOs, &[]).unwrap(),
            PathBuf::from("/home/u")
                .join("Library")
                .join("Application Support")
                .join("cano")
                .join("svc")
        );
    }

    #[test]
    fn windows_uses_localappdata_then_home() {
        assert_eq!(
            dir_for(Platform::Windows, &[("LOCALAPPDATA", "/l")]).unwrap(),
            PathBuf::from("/l").join("cano").join("svc")
        );
        assert_eq!(
            dir_for(Platform::Windows, &[]).unwrap(),
            PathBuf::from("/home/u")
                .join("AppData")
                .join("Local")
                .join("cano")
                .join("svc")
        );
    }

    #[test]
    fn no_base_directory_is_a_configuration_error() {
        let no_env = env_of(&[]);
        for platform in PLATFORMS {
            for (home, app) in [
                (None, Some(OsStr::new("svc"))),
                (Some(PathBuf::from("/home/u")), None),
            ] {
                let err = default_dir(platform, &no_env, home, app).unwrap_err();
                assert_eq!(err.category(), "configuration");
                assert!(err.to_string().contains("CANO_DATA_DIR"), "{err}");
            }
        }
    }

    #[tokio::test]
    async fn open_in_creates_missing_directories_and_survives_reopen() {
        let root = tempdir().unwrap();
        let dir = root.path().join("a").join("b").join("c");
        let store = RedbStore::open_in(&dir).unwrap();
        store
            .append("run", CheckpointRow::new(0, "S", "t"))
            .await
            .unwrap();
        drop(store);

        let store = RedbStore::open_in(&dir).unwrap();
        assert_eq!(
            store.load_run("run").await.unwrap(),
            vec![CheckpointRow::new(0, "S", "t")]
        );
        assert!(dir.join(RedbStore::DEFAULT_FILE_NAME).is_file());
    }

    #[test]
    fn a_held_file_cannot_be_opened_twice() {
        let dir = tempdir().unwrap();
        let _held = RedbStore::open_in(dir.path()).unwrap();
        let err = match RedbStore::open_in(dir.path()) {
            Ok(_) => panic!("a second open of a held file must fail"),
            Err(err) => err,
        };
        assert_eq!(err.category(), "checkpoint_store");
    }

    #[cfg(feature = "scheduler")]
    #[tokio::test]
    async fn scheduler_store_contract() {
        let dir = tempdir().unwrap();
        let store = RedbStore::new(dir.path().join("sched.redb")).unwrap();
        crate::testing::assert_scheduler_store_contract(&store, "redb").await;
    }

    #[cfg(feature = "scheduler")]
    #[tokio::test]
    async fn scheduler_records_survive_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sched.redb");

        let store = RedbStore::new(&path).unwrap();
        let CasOutcome::Written(v1) = store
            .compare_and_swap("flow", None, b"first".to_vec())
            .await
            .unwrap()
        else {
            panic!("first write must succeed");
        };
        drop(store);

        let store = RedbStore::new(&path).unwrap();
        let record = store.load("flow").await.unwrap().unwrap();
        assert_eq!(record.version, v1);
        assert_eq!(record.value, b"first");

        let CasOutcome::Written(v2) = store
            .compare_and_swap("flow", Some(v1), b"second".to_vec())
            .await
            .unwrap()
        else {
            panic!("swap from the persisted version must succeed");
        };
        assert_ne!(v1, v2);
        assert_eq!(
            store
                .compare_and_swap("flow", Some(v1), b"stale".to_vec())
                .await
                .unwrap(),
            CasOutcome::Conflict
        );

        // The counter survives too: remove + re-create never repeats a version.
        store.remove("flow").await.unwrap();
        drop(store);
        let store = RedbStore::new(&path).unwrap();
        assert!(store.load("flow").await.unwrap().is_none());
        let CasOutcome::Written(v3) = store
            .compare_and_swap("flow", None, b"third".to_vec())
            .await
            .unwrap()
        else {
            panic!("re-create must succeed");
        };
        assert!(v3 != v1 && v3 != v2);
    }
}
