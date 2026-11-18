//! [`PostgresStore`]: a [`CheckpointStore`] and, with the `scheduler` feature, a
//! `SchedulerStore`, backed by PostgreSQL tables.
//!
//! Compiled with the `store-postgres` feature. Connections come from a
//! [`deadpool_postgres`] pool over [`tokio_postgres`]; both crates are re-exported here so
//! you can build a TLS connector, a [`tokio_postgres::Config`] or your own pool without
//! adding them as dependencies.
//!
//! ## Table layout
//!
//! Every run of every workflow lives in one table, `cano_checkpoints` by default:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS "cano_checkpoints" (
//!     sequence         bigint      NOT NULL,
//!     workflow_version bigint      NOT NULL,
//!     created_at       timestamptz NOT NULL DEFAULT now(),
//!     kind             smallint    NOT NULL,
//!     workflow_id      text COLLATE "C" NOT NULL,
//!     state            text        NOT NULL,
//!     task_id          text        NOT NULL,
//!     output_blob      bytea,
//!     PRIMARY KEY (workflow_id, sequence)
//! );
//! ```
//!
//! The primary key is the only index. It rejects a duplicate `(workflow_id, sequence)`, and
//! `load_run` and `clear` each read one range of it.
//! `created_at` is for operators and is not indexed. `kind` stores [`RowKind`] as
//! `StateEntry = 0`, `CompensationCompletion = 1`, `StepCursor = 2`, `Suspended = 3`,
//! `ResumeInput = 4`.
//!
//! By default the store creates the table on connect, under an advisory lock so that
//! processes starting together do not race. Turn that off with
//! [`with_create_table(false)`](PostgresStoreBuilder::with_create_table) and run
//! [`schema_sql`](PostgresStoreBuilder::schema_sql) yourself. The store never
//! creates a schema.
//!
//! ## Scheduler table
//!
//! With the `scheduler` feature the store also implements `SchedulerStore`, the shared
//! record that lets several scheduler instances run one set of flows, each tick on exactly
//! one instance. It lives in a second table, `cano_scheduler` by default, beside a
//! sequence named `<table>_version_seq` that hands out record versions:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS "cano_scheduler" (
//!     flow_id    text        PRIMARY KEY,
//!     version    bigint      NOT NULL,
//!     record     bytea       NOT NULL,
//!     updated_at timestamptz NOT NULL DEFAULT now()
//! );
//! CREATE SEQUENCE IF NOT EXISTS "cano_scheduler_version_seq";
//! ```
//!
//! `record` is the scheduler's JSON document as UTF-8 bytes; read it with
//! `SELECT flow_id, convert_from(record, 'UTF8')::jsonb FROM cano_scheduler`. Every
//! write takes its `version` from the sequence, so a version never repeats for one flow,
//! even after the flow is removed and created again. Instances that share a scheduler
//! table form one cluster. `with_scheduler_table` renames the table; the migration, the
//! operation timeout and the retry policy apply to it as they do to the checkpoint table.
//!
//! ## Failures
//!
//! Every call is bounded by the operation timeout (10 s by default). With
//! [`with_retry`](PostgresStoreBuilder::with_retry) a call that fails on a
//! transient error (a dropped connection, a timeout, a serialization failure) is retried;
//! a conflict or a malformed row is never retried. A connection whose call timed out or
//! was cancelled is closed, never returned to the pool.
//!
//! An `Err` from `append` after a timeout does not prove the row is absent: the insert may
//! have committed after the client gave up. A retried `append` treats a byte-identical row
//! already stored at its sequence as its own earlier write and returns `Ok`; a different
//! row there is a conflict.
//!
//! A retried `compare_and_swap` likewise accepts a stored record whose bytes equal the
//! ones it sent: the scheduler's records carry the writer's identity and a timestamp, so
//! equal bytes mean an earlier attempt landed, and the call returns `Written` with the
//! stored version.
//!
//! ## Example
//!
//! ```rust,no_run
//! use cano::store::postgres::PostgresStore;
//! use cano::{CanoError, RetryMode};
//! use std::time::Duration;
//!
//! # async fn demo() -> Result<(), CanoError> {
//! let store = PostgresStore::builder()
//!     .with_schema("workflows")
//!     .with_max_pool_size(8)
//!     .with_operation_timeout(Duration::from_secs(5))
//!     .with_retry(RetryMode::fixed(3, Duration::from_millis(200)))
//!     .connect("postgres://cano:secret@db.internal/app")
//!     .await?;
//! # let _ = store;
//! # Ok(())
//! # }
//! ```

use std::error::Error as _;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use deadpool_postgres::{
    Manager, ManagerConfig, Object, Pool, PoolError, RecyclingMethod, Runtime, Timeouts,
};
use tokio_postgres::Socket;
use tokio_postgres::tls::{MakeTlsConnect, TlsConnect};

use super::conflict_error;
use super::retry::{self, Failure};
use crate::RetryMode;
use crate::checkpoint::{CheckpointRow, CheckpointStore, RowKind};
use crate::error::CanoError;
#[cfg(feature = "scheduler")]
use crate::scheduler::{CasOutcome, SchedulerStore, VersionedRecord};
use cano_macros::checkpoint_store;
#[cfg(feature = "scheduler")]
use cano_macros::scheduler_store;

pub use deadpool_postgres;
pub use tokio_postgres;

/// The table name used unless [`with_checkpoint_table`](PostgresStoreBuilder::with_checkpoint_table)
/// sets another.
pub const DEFAULT_CHECKPOINT_TABLE: &str = "cano_checkpoints";

/// The scheduler table name used unless
/// [`with_scheduler_table`](PostgresStoreBuilder::with_scheduler_table) sets another.
#[cfg(feature = "scheduler")]
pub const DEFAULT_SCHEDULER_TABLE: &str = "cano_scheduler";

/// The `application_name` reported to the server when the connection config sets none.
pub const DEFAULT_APPLICATION_NAME: &str = "cano";

/// The bound on each store call unless
/// [`with_operation_timeout`](PostgresStoreBuilder::with_operation_timeout) sets
/// another.
pub const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// `0x63616e6f636b7074`, `"canockpt"`: serializes concurrent first connects in the migration.
const MIGRATION_LOCK: i64 = 7161126307184210036;

/// Postgres cuts identifiers at 63 bytes. The version sequence is `<table>_version_seq`, so
/// the table name's part of it is cut short enough that the suffix survives and the
/// sequence never truncates onto the table's own name.
#[cfg(feature = "scheduler")]
const SEQUENCE_STEM_MAX: usize = 63 - "_version_seq".len();

/// A [`CheckpointStore`] that keeps every run in one PostgreSQL table and, with the
/// `scheduler` feature, a `SchedulerStore` that keeps every flow's record in a second one.
///
/// The checkpoint table is `cano_checkpoints` and the scheduler table is `cano_scheduler`
/// by default. The scheduler record is the scheduler's JSON in a `bytea` column; query it
/// with `convert_from(record, 'UTF8')::jsonb`.
///
/// Cheap to clone: clones share the connection pool. Build one with
/// [`builder`](Self::builder), or [`connect`](Self::connect) for the defaults. See the
/// [module docs](self) for the table layouts and the failure semantics.
#[derive(Clone)]
pub struct PostgresStore {
    pool: Pool,
    sql: Arc<Statements>,
    #[cfg(feature = "scheduler")]
    scheduler: Arc<SchedulerStatements>,
    operation_timeout: Duration,
    retry: RetryMode,
}

impl fmt::Debug for PostgresStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("PostgresStore");
        debug.field("table", &self.sql.table);
        #[cfg(feature = "scheduler")]
        debug.field("scheduler_table", &self.scheduler.table);
        debug
            .field("operation_timeout", &self.operation_timeout)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl PostgresStore {
    /// A builder with every setting at its default.
    #[must_use]
    pub fn builder() -> PostgresStoreBuilder {
        PostgresStoreBuilder::default()
    }

    /// Connect with the default settings, creating `cano_checkpoints` (and, with the
    /// `scheduler` feature, `cano_scheduler`) if it is missing.
    ///
    /// `config` is a libpq-style connection string, as a URL
    /// (`postgres://user:pass@host/db`) or as `key=value` pairs.
    pub async fn connect(config: &str) -> Result<Self, CanoError> {
        Self::builder().connect(config).await
    }

    /// The connection pool the store draws from.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    async fn append_once(
        &self,
        workflow_id: &str,
        row: &CheckpointRow,
        attempt: usize,
    ) -> Result<(), Failure> {
        let sequence = i64::try_from(row.sequence).map_err(|_| {
            Failure::fatal(CanoError::checkpoint_store(format!(
                "postgres append: sequence {} exceeds the Postgres bigint range",
                row.sequence
            )))
        })?;
        let client = Checkout::get(&self.pool, "append").await?;
        let result = self
            .insert_or_verify(&client, workflow_id, sequence, row, attempt)
            .await;
        client.release();
        result
    }

    async fn insert_or_verify(
        &self,
        client: &Object,
        workflow_id: &str,
        sequence: i64,
        row: &CheckpointRow,
        attempt: usize,
    ) -> Result<(), Failure> {
        let insert = client
            .prepare_cached(&self.sql.insert)
            .await
            .map_err(|e| statement_failure("append", &e))?;
        let inserted = client
            .execute(
                &insert,
                &[
                    &workflow_id,
                    &sequence,
                    &row.state.as_str(),
                    &row.task_id.as_str(),
                    &kind_to_db(row.kind),
                    &i64::from(row.workflow_version),
                    &row.output_blob.as_deref(),
                ],
            )
            .await
            .map_err(|e| statement_failure("append", &e))?;
        if inserted == 1 {
            return Ok(());
        }
        if attempt > 0 {
            // An earlier attempt may have committed before its reply was lost.
            let select = client
                .prepare_cached(&self.sql.select_row)
                .await
                .map_err(|e| statement_failure("append", &e))?;
            let stored = client
                .query_opt(&select, &[&workflow_id, &sequence])
                .await
                .map_err(|e| statement_failure("append", &e))?;
            if stored.is_some_and(|stored| same_row(&stored, row)) {
                return Ok(());
            }
        }
        Err(Failure::fatal(conflict_error(workflow_id, row.sequence)))
    }

    async fn load_once(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, Failure> {
        let client = Checkout::get(&self.pool, "load_run").await?;
        let result = async {
            let select = client.prepare_cached(&self.sql.select_run).await?;
            client.query(&select, &[&workflow_id]).await
        }
        .await;
        client.release();
        let rows = result.map_err(|e| statement_failure("load_run", &e))?;
        rows.iter()
            .map(|stored| decode_row(workflow_id, stored))
            .collect()
    }

    async fn clear_once(&self, workflow_id: &str) -> Result<(), Failure> {
        let client = Checkout::get(&self.pool, "clear").await?;
        let result = async {
            let delete = client.prepare_cached(&self.sql.delete).await?;
            client.execute(&delete, &[&workflow_id]).await
        }
        .await;
        client.release();
        result.map(drop).map_err(|e| statement_failure("clear", &e))
    }
}

#[checkpoint_store]
impl CheckpointStore for PostgresStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let row = &row;
        retry::run(&self.retry, move |n| {
            retry::bounded(
                "postgres append",
                self.operation_timeout,
                self.append_once(workflow_id, row, n),
            )
        })
        .await
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        retry::run(&self.retry, |_| {
            retry::bounded(
                "postgres load_run",
                self.operation_timeout,
                self.load_once(workflow_id),
            )
        })
        .await
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        retry::run(&self.retry, |_| {
            retry::bounded(
                "postgres clear",
                self.operation_timeout,
                self.clear_once(workflow_id),
            )
        })
        .await
    }
}

#[cfg(feature = "scheduler")]
impl PostgresStore {
    async fn scheduler_load_once(&self, flow_id: &str) -> Result<Option<VersionedRecord>, Failure> {
        let client = Checkout::get(&self.pool, "scheduler load").await?;
        let result = async {
            let select = client.prepare_cached(&self.scheduler.select).await?;
            client.query_opt(&select, &[&flow_id]).await
        }
        .await;
        client.release();
        let stored = result.map_err(|e| statement_failure("scheduler load", &e))?;
        stored
            .as_ref()
            .map(|stored| decode_record(flow_id, stored))
            .transpose()
    }

    async fn scheduler_swap_once(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: &[u8],
        attempt: usize,
    ) -> Result<CasOutcome, Failure> {
        let expected = match expected {
            None => None,
            // Postgres sequence values live in `1..=i64::MAX`, so `0` or anything above
            // `i64::MAX` can never match a stored version.
            Some(0) => return Ok(CasOutcome::Conflict),
            Some(version) => match i64::try_from(version) {
                Ok(version) => Some(version),
                Err(_) => return Ok(CasOutcome::Conflict),
            },
        };
        let client = Checkout::get(&self.pool, "scheduler swap").await?;
        let result = self
            .swap_or_verify(&client, flow_id, expected, value, attempt)
            .await;
        client.release();
        result
    }

    async fn swap_or_verify(
        &self,
        client: &Object,
        flow_id: &str,
        expected: Option<i64>,
        value: &[u8],
        attempt: usize,
    ) -> Result<CasOutcome, Failure> {
        let fail = |e: tokio_postgres::Error| statement_failure("scheduler swap", &e);
        let swapped = match expected {
            None => {
                let insert = client
                    .prepare_cached(&self.scheduler.insert)
                    .await
                    .map_err(fail)?;
                client
                    .query_opt(&insert, &[&flow_id, &value])
                    .await
                    .map_err(fail)?
            }
            Some(version) => {
                let update = client
                    .prepare_cached(&self.scheduler.update)
                    .await
                    .map_err(fail)?;
                client
                    .query_opt(&update, &[&flow_id, &version, &value])
                    .await
                    .map_err(fail)?
            }
        };
        if let Some(written) = swapped {
            return Ok(CasOutcome::Written(version_from_db(flow_id, &written, 0)?));
        }
        if attempt > 0 {
            // An earlier attempt may have committed before its reply was lost.
            let select = client
                .prepare_cached(&self.scheduler.select)
                .await
                .map_err(fail)?;
            let stored = client.query_opt(&select, &[&flow_id]).await.map_err(fail)?;
            if let Some(stored) = stored {
                let record = decode_record(flow_id, &stored)?;
                if record.value == value {
                    return Ok(CasOutcome::Written(record.version));
                }
            }
        }
        Ok(CasOutcome::Conflict)
    }

    async fn scheduler_remove_once(&self, flow_id: &str) -> Result<(), Failure> {
        let client = Checkout::get(&self.pool, "scheduler remove").await?;
        let result = async {
            let delete = client.prepare_cached(&self.scheduler.delete).await?;
            client.execute(&delete, &[&flow_id]).await
        }
        .await;
        client.release();
        result
            .map(drop)
            .map_err(|e| statement_failure("scheduler remove", &e))
    }
}

#[cfg(feature = "scheduler")]
#[scheduler_store]
impl SchedulerStore for PostgresStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        retry::run(&self.retry, |_| {
            retry::bounded(
                "postgres scheduler load",
                self.operation_timeout,
                self.scheduler_load_once(flow_id),
            )
        })
        .await
        .map_err(scheduler_error)
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        let value = value.as_slice();
        retry::run(&self.retry, move |n| {
            retry::bounded(
                "postgres scheduler swap",
                self.operation_timeout,
                self.scheduler_swap_once(flow_id, expected, value, n),
            )
        })
        .await
        .map_err(scheduler_error)
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        retry::run(&self.retry, |_| {
            retry::bounded(
                "postgres scheduler remove",
                self.operation_timeout,
                self.scheduler_remove_once(flow_id),
            )
        })
        .await
        .map_err(scheduler_error)
    }
}

/// Settings for a [`PostgresStore`]; start from
/// [`PostgresStore::builder`].
#[derive(Debug, Clone)]
pub struct PostgresStoreBuilder {
    table: String,
    schema: Option<String>,
    create_table: bool,
    max_pool_size: Option<usize>,
    pool_timeouts: Timeouts,
    recycling_method: RecyclingMethod,
    operation_timeout: Duration,
    retry: RetryMode,
    #[cfg(feature = "scheduler")]
    scheduler_table: String,
}

impl Default for PostgresStoreBuilder {
    fn default() -> Self {
        Self {
            table: DEFAULT_CHECKPOINT_TABLE.to_owned(),
            schema: None,
            create_table: true,
            max_pool_size: None,
            pool_timeouts: Timeouts::default(),
            recycling_method: RecyclingMethod::Fast,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            retry: RetryMode::None,
            #[cfg(feature = "scheduler")]
            scheduler_table: DEFAULT_SCHEDULER_TABLE.to_owned(),
        }
    }
}

impl PostgresStoreBuilder {
    /// The table name. Default [`DEFAULT_CHECKPOINT_TABLE`]. Must be 1 to 63 characters of
    /// `[a-z0-9_]`, not starting with a digit.
    #[must_use]
    pub fn with_checkpoint_table(mut self, table: impl Into<String>) -> Self {
        self.table = table.into();
        self
    }

    /// The scheduler table name. Default [`DEFAULT_SCHEDULER_TABLE`]. Same naming rule and
    /// schema as [`with_checkpoint_table`](Self::with_checkpoint_table). The version sequence
    /// is created beside it as `<name>_version_seq`. Instances that share a table form one
    /// scheduler cluster.
    #[cfg(feature = "scheduler")]
    #[must_use]
    pub fn with_scheduler_table(mut self, table: impl Into<String>) -> Self {
        self.scheduler_table = table.into();
        self
    }

    /// The schema that holds the table. Default: none, so the server's `search_path`
    /// resolves the table name. The schema must already exist. Same naming rule as
    /// [`with_checkpoint_table`](Self::with_checkpoint_table).
    #[must_use]
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    /// Whether to create the table on connect when it is missing. Default `true`. With
    /// `false` the builder does no I/O and the operator runs [`schema_sql`](Self::schema_sql).
    #[must_use]
    pub fn with_create_table(mut self, create_table: bool) -> Self {
        self.create_table = create_table;
        self
    }

    /// The most connections the pool opens. Default: deadpool's, twice the logical core
    /// count. Ignored by [`build_with_pool`](Self::build_with_pool).
    #[must_use]
    pub fn with_max_pool_size(mut self, max_pool_size: usize) -> Self {
        self.max_pool_size = Some(max_pool_size);
        self
    }

    /// Pool timeouts for waiting on a free slot, creating and recycling a connection.
    /// Default: none; the operation timeout still bounds every call. Ignored by
    /// [`build_with_pool`](Self::build_with_pool).
    #[must_use]
    pub fn with_pool_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.pool_timeouts = timeouts;
        self
    }

    /// How the pool checks a connection before reuse. Default [`RecyclingMethod::Fast`].
    /// Ignored by [`build_with_pool`](Self::build_with_pool).
    #[must_use]
    pub fn with_recycling_method(mut self, recycling_method: RecyclingMethod) -> Self {
        self.recycling_method = recycling_method;
        self
    }

    /// The bound on each attempt of a store call (`append`, `load_run`, `clear`, and the
    /// scheduler `load`, `compare_and_swap` and `remove`), connection checkout
    /// included, and on the table migration. Default [`DEFAULT_OPERATION_TIMEOUT`].
    #[must_use]
    pub fn with_operation_timeout(mut self, timeout: Duration) -> Self {
        self.operation_timeout = timeout;
        self
    }

    /// Retry policy for transient failures. Default [`RetryMode::None`]. Conflicts and
    /// malformed rows are never retried. See the [module docs](self) for how a retried
    /// `append` and a retried `compare_and_swap` handle their own earlier write.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryMode) -> Self {
        self.retry = retry;
        self
    }

    /// The `CREATE TABLE IF NOT EXISTS` statement for the configured schema and table,
    /// for operators who run DDL themselves. With the `scheduler` feature it also holds the
    /// scheduler table and its version sequence.
    pub fn schema_sql(&self) -> Result<String, CanoError> {
        self.ddl()
    }

    /// Parse `config` as a libpq-style connection string and connect without TLS.
    pub async fn connect(self, config: &str) -> Result<PostgresStore, CanoError> {
        let config = config.parse::<tokio_postgres::Config>().map_err(|e| {
            CanoError::configuration(format!("invalid Postgres connection string: {e}"))
        })?;
        self.connect_with_config(config).await
    }

    /// Connect without TLS using a prepared [`tokio_postgres::Config`].
    pub async fn connect_with_config(
        self,
        config: tokio_postgres::Config,
    ) -> Result<PostgresStore, CanoError> {
        self.connect_with_tls(config, tokio_postgres::NoTls).await
    }

    /// Connect with a TLS connector, such as one from `tokio-postgres-rustls` or
    /// `postgres-native-tls`. Sets `application_name` to [`DEFAULT_APPLICATION_NAME`] when
    /// `config` sets none.
    pub async fn connect_with_tls<T>(
        self,
        mut config: tokio_postgres::Config,
        tls: T,
    ) -> Result<PostgresStore, CanoError>
    where
        T: MakeTlsConnect<Socket> + Clone + Sync + Send + 'static,
        T::Stream: Sync + Send,
        T::TlsConnect: Sync + Send,
        <T::TlsConnect as TlsConnect<Socket>>::Future: Send,
    {
        self.ddl()?;
        apply_default_application_name(&mut config);
        let manager = Manager::from_config(
            config,
            tls,
            ManagerConfig {
                recycling_method: self.recycling_method.clone(),
            },
        );
        let mut pool = Pool::builder(manager)
            .runtime(Runtime::Tokio1)
            .timeouts(self.pool_timeouts);
        if let Some(max_size) = self.max_pool_size {
            pool = pool.max_size(max_size);
        }
        let pool = pool
            .build()
            .map_err(|e| CanoError::configuration(format!("build Postgres pool: {e}")))?;
        self.build_with_pool(pool).await
    }

    /// Use a pool you built. The pool settings on this builder do not apply.
    pub async fn build_with_pool(self, pool: Pool) -> Result<PostgresStore, CanoError> {
        let table = self.qualified_table()?;
        #[cfg(feature = "scheduler")]
        let (scheduler_table, scheduler_sequence) = self.scheduler_names()?;
        if self.create_table {
            let migration = format!(
                "SELECT pg_advisory_xact_lock({MIGRATION_LOCK});\n{}",
                self.ddl()?
            );
            retry::bounded("postgres migrate", self.operation_timeout, async {
                let client = Checkout::get(&pool, "migrate").await?;
                let result = client.batch_execute(&migration).await;
                client.release();
                result.map_err(|e| statement_failure("migrate", &e))
            })
            .await
            .map_err(|failure| failure.error)?;
        }
        Ok(PostgresStore {
            pool,
            sql: Arc::new(Statements::new(table)),
            #[cfg(feature = "scheduler")]
            scheduler: Arc::new(SchedulerStatements::new(
                scheduler_table,
                &scheduler_sequence,
            )),
            operation_timeout: self.operation_timeout,
            retry: self.retry,
        })
    }

    /// The DDL for every table the enabled features use.
    fn ddl(&self) -> Result<String, CanoError> {
        let sql = create_table_sql(&self.qualified_table()?);
        #[cfg(feature = "scheduler")]
        let sql = {
            let (table, sequence) = self.scheduler_names()?;
            format!("{sql}\n{}", create_scheduler_sql(&table, &sequence))
        };
        Ok(sql)
    }

    /// `"table"` or `"schema"."table"`, after validating both names.
    fn qualified_table(&self) -> Result<String, CanoError> {
        self.qualify(&self.table)
    }

    /// The qualified scheduler table and its qualified version sequence.
    #[cfg(feature = "scheduler")]
    fn scheduler_names(&self) -> Result<(String, String), CanoError> {
        let table = self.qualify(&self.scheduler_table)?;
        // `qualify` validated the name as ASCII, so cutting at a byte index is safe.
        let stem = &self.scheduler_table[..self.scheduler_table.len().min(SEQUENCE_STEM_MAX)];
        let sequence = self.qualify(&format!("{stem}_version_seq"))?;
        Ok((table, sequence))
    }

    /// `"name"` or `"schema"."name"`, after validating both names.
    fn qualify(&self, name: &str) -> Result<String, CanoError> {
        let name = quote_identifier(name)?;
        Ok(match &self.schema {
            Some(schema) => format!("{}.{name}", quote_identifier(schema)?),
            None => name,
        })
    }
}

fn apply_default_application_name(config: &mut tokio_postgres::Config) {
    if config.get_application_name().is_none() {
        config.application_name(DEFAULT_APPLICATION_NAME);
    }
}

/// Validate `name` against `^[a-z_][a-z0-9_]{0,62}$` and double-quote it.
fn quote_identifier(name: &str) -> Result<String, CanoError> {
    let bytes = name.as_bytes();
    let valid = matches!(bytes.first(), Some(b'a'..=b'z' | b'_'))
        && bytes.len() <= 63
        && bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'));
    if valid {
        Ok(format!("\"{name}\""))
    } else {
        Err(CanoError::configuration(format!(
            "invalid Postgres identifier {name:?}: use 1 to 63 characters of [a-z0-9_], not starting with a digit"
        )))
    }
}

fn create_table_sql(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (
    sequence         bigint      NOT NULL,
    workflow_version bigint      NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    kind             smallint    NOT NULL,
    workflow_id      text COLLATE \"C\" NOT NULL,
    state            text        NOT NULL,
    task_id          text        NOT NULL,
    output_blob      bytea,
    PRIMARY KEY (workflow_id, sequence)
);"
    )
}

#[cfg(feature = "scheduler")]
fn create_scheduler_sql(table: &str, sequence: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (
    flow_id    text        PRIMARY KEY,
    version    bigint      NOT NULL,
    record     bytea       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE SEQUENCE IF NOT EXISTS {sequence};"
    )
}

/// The four statements, rendered once for the configured table.
struct Statements {
    table: String,
    insert: String,
    select_run: String,
    select_row: String,
    delete: String,
}

impl Statements {
    fn new(table: String) -> Self {
        Self {
            insert: format!(
                "INSERT INTO {table} (workflow_id, sequence, state, task_id, kind, workflow_version, output_blob) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (workflow_id, sequence) DO NOTHING"
            ),
            select_run: format!(
                "SELECT sequence, state, task_id, kind, workflow_version, output_blob FROM {table} \
                 WHERE workflow_id = $1 ORDER BY sequence"
            ),
            select_row: format!(
                "SELECT state, task_id, kind, workflow_version, output_blob FROM {table} \
                 WHERE workflow_id = $1 AND sequence = $2"
            ),
            delete: format!("DELETE FROM {table} WHERE workflow_id = $1"),
            table,
        }
    }
}

/// The scheduler statements, rendered once for the configured table and sequence.
#[cfg(feature = "scheduler")]
struct SchedulerStatements {
    table: String,
    select: String,
    insert: String,
    update: String,
    delete: String,
}

#[cfg(feature = "scheduler")]
impl SchedulerStatements {
    fn new(table: String, sequence: &str) -> Self {
        Self {
            select: format!("SELECT version, record FROM {table} WHERE flow_id = $1"),
            insert: format!(
                "INSERT INTO {table} (flow_id, version, record) \
                 VALUES ($1, nextval('{sequence}'), $2) \
                 ON CONFLICT (flow_id) DO NOTHING RETURNING version"
            ),
            update: format!(
                "UPDATE {table} SET version = nextval('{sequence}'), record = $3, updated_at = now() \
                 WHERE flow_id = $1 AND version = $2 RETURNING version"
            ),
            delete: format!("DELETE FROM {table} WHERE flow_id = $1"),
            table,
        }
    }
}

/// A pooled connection that is closed, not returned to the pool, unless
/// [`release`](Self::release) runs. A timeout or a cancelled caller drops an attempt in
/// the middle of a statement, leaving an unread reply on the connection.
struct Checkout(Option<Object>);

impl Checkout {
    async fn get(pool: &Pool, op: &str) -> Result<Self, Failure> {
        match pool.get().await {
            Ok(client) => Ok(Self(Some(client))),
            Err(e) => {
                let transient = match &e {
                    PoolError::Timeout(_) => true,
                    PoolError::Backend(e) => is_transient(e),
                    _ => false,
                };
                let message = match &e {
                    PoolError::Backend(e) => format!("postgres {op}: {}", PgError(e)),
                    _ => format!("postgres {op}: {e}"),
                };
                Err(Failure {
                    error: CanoError::checkpoint_store(message),
                    transient,
                })
            }
        }
    }

    /// Return the connection to the pool: its statements ran to completion.
    fn release(mut self) {
        drop(self.0.take());
    }
}

impl std::ops::Deref for Checkout {
    type Target = Object;

    fn deref(&self) -> &Object {
        self.0
            .as_ref()
            .expect("checkout holds a connection until released")
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if let Some(client) = self.0.take() {
            drop(Object::take(client));
        }
    }
}

/// [`tokio_postgres::Error`]'s `Display` omits the server message; this adds its source.
struct PgError<'a>(&'a tokio_postgres::Error);

impl fmt::Display for PgError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        if let Some(source) = self.0.source() {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}

fn statement_failure(op: &str, e: &tokio_postgres::Error) -> Failure {
    Failure {
        error: CanoError::checkpoint_store(format!("postgres {op}: {}", PgError(e))),
        transient: is_transient(e),
    }
}

/// A failure another attempt may fix: no SQLSTATE (I/O, closed connection, protocol) or a
/// transient one.
fn is_transient(e: &tokio_postgres::Error) -> bool {
    e.code().is_none_or(|code| transient_sqlstate(code.code()))
}

/// Connection exceptions (`08`), insufficient resources (`53`), operator intervention such
/// as a server shutdown (`57P`), serialization failure (`40001`) and deadlock (`40P01`).
fn transient_sqlstate(code: &str) -> bool {
    code.starts_with("08")
        || code.starts_with("53")
        || code.starts_with("57P")
        || code == "40001"
        || code == "40P01"
}

fn kind_to_db(kind: RowKind) -> i16 {
    match kind {
        RowKind::StateEntry => 0,
        RowKind::CompensationCompletion => 1,
        RowKind::StepCursor => 2,
        RowKind::Suspended => 3,
        RowKind::ResumeInput => 4,
    }
}

fn kind_from_db(kind: i16) -> Option<RowKind> {
    Some(match kind {
        0 => RowKind::StateEntry,
        1 => RowKind::CompensationCompletion,
        2 => RowKind::StepCursor,
        3 => RowKind::Suspended,
        4 => RowKind::ResumeInput,
        _ => return None,
    })
}

/// Whether a `select_row` result holds exactly `row`'s payload.
fn same_row(stored: &tokio_postgres::Row, row: &CheckpointRow) -> bool {
    stored.try_get::<_, &str>(0).is_ok_and(|s| s == row.state)
        && stored.try_get::<_, &str>(1).is_ok_and(|s| s == row.task_id)
        && stored
            .try_get::<_, i16>(2)
            .is_ok_and(|k| k == kind_to_db(row.kind))
        && stored
            .try_get::<_, i64>(3)
            .is_ok_and(|v| v == i64::from(row.workflow_version))
        && stored
            .try_get::<_, Option<&[u8]>>(4)
            .is_ok_and(|b| b == row.output_blob.as_deref())
}

/// Map a `select_run` row; any out-of-range or unknown value is a fatal failure.
fn decode_row(workflow_id: &str, stored: &tokio_postgres::Row) -> Result<CheckpointRow, Failure> {
    let raw_sequence = stored.try_get::<_, i64>(0);
    let invalid = |reason: &dyn fmt::Display| {
        let sequence = raw_sequence
            .as_ref()
            .map_or_else(|_| "?".to_owned(), i64::to_string);
        Failure::fatal(CanoError::checkpoint_store(format!(
            "postgres load_run: invalid row at workflow_id={workflow_id:?} sequence={sequence}: {reason}"
        )))
    };
    let sequence = match &raw_sequence {
        Ok(raw) => u64::try_from(*raw).map_err(|e| invalid(&e))?,
        Err(e) => return Err(invalid(e)),
    };
    let state: String = stored.try_get(1).map_err(|e| invalid(&e))?;
    let task_id: String = stored.try_get(2).map_err(|e| invalid(&e))?;
    let kind_code: i16 = stored.try_get(3).map_err(|e| invalid(&e))?;
    let kind =
        kind_from_db(kind_code).ok_or_else(|| invalid(&format!("unknown kind {kind_code}")))?;
    let version: i64 = stored.try_get(4).map_err(|e| invalid(&e))?;
    let workflow_version = u32::try_from(version).map_err(|e| invalid(&e))?;
    let output_blob: Option<Vec<u8>> = stored.try_get(5).map_err(|e| invalid(&e))?;
    Ok(CheckpointRow {
        sequence,
        state,
        task_id,
        output_blob,
        kind,
        workflow_version,
    })
}

/// Retag a failure the shared helpers built as a checkpoint store error.
#[cfg(feature = "scheduler")]
fn scheduler_error(error: CanoError) -> CanoError {
    match error {
        CanoError::CheckpointStore(message) => CanoError::scheduler_store(message),
        other => other,
    }
}

/// The `bigint` version at column `index`; a negative or missing one is a fatal failure.
#[cfg(feature = "scheduler")]
fn version_from_db(
    flow_id: &str,
    stored: &tokio_postgres::Row,
    index: usize,
) -> Result<u64, Failure> {
    stored
        .try_get::<_, i64>(index)
        .map_err(|e| e.to_string())
        .and_then(|version| u64::try_from(version).map_err(|e| e.to_string()))
        .map_err(|reason| {
            Failure::fatal(CanoError::scheduler_store(format!(
                "postgres scheduler: invalid version at flow_id={flow_id:?}: {reason}"
            )))
        })
}

/// Map a `select` row of the scheduler table.
#[cfg(feature = "scheduler")]
fn decode_record(flow_id: &str, stored: &tokio_postgres::Row) -> Result<VersionedRecord, Failure> {
    let version = version_from_db(flow_id, stored, 0)?;
    let value: Vec<u8> = stored.try_get(1).map_err(|e| {
        Failure::fatal(CanoError::scheduler_store(format!(
            "postgres scheduler load: invalid record at flow_id={flow_id:?}: {e}"
        )))
    })?;
    Ok(VersionedRecord { version, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_identifiers_are_configuration_errors() {
        for name in ["x; DROP TABLE y", "Cano", "1abc", "", &"a".repeat(64)] {
            let err = PostgresStore::builder()
                .with_checkpoint_table(name)
                .schema_sql()
                .unwrap_err();
            assert_eq!(err.category(), "configuration", "{name:?}");
            let err = PostgresStore::builder()
                .with_schema(name)
                .schema_sql()
                .unwrap_err();
            assert_eq!(err.category(), "configuration", "{name:?}");
        }
        for name in ["cano_checkpoints", "_t", "t9", &"a".repeat(63)] {
            PostgresStore::builder()
                .with_checkpoint_table(name)
                .schema_sql()
                .unwrap();
        }
    }

    #[test]
    fn schema_sql_names_the_configured_table() {
        let default = PostgresStore::builder().schema_sql().unwrap();
        assert!(default.contains("\"cano_checkpoints\""), "{default}");
        assert!(
            default.contains("PRIMARY KEY (workflow_id, sequence)"),
            "{default}"
        );
        let qualified = PostgresStore::builder()
            .with_schema("wf")
            .schema_sql()
            .unwrap();
        assert!(
            qualified.contains("\"wf\".\"cano_checkpoints\""),
            "{qualified}"
        );
    }

    #[tokio::test]
    async fn invalid_names_fail_before_any_io() {
        // Port 1 is never listening: reaching it would be a connect error, not this one.
        let err = PostgresStore::builder()
            .with_checkpoint_table("Bad")
            .connect("postgres://u:p@127.0.0.1:1/db")
            .await
            .unwrap_err();
        assert_eq!(err.category(), "configuration");
        let err = PostgresStore::connect("not a = valid = string")
            .await
            .unwrap_err();
        assert_eq!(err.category(), "configuration");
    }

    #[test]
    fn kinds_round_trip() {
        for kind in [
            RowKind::StateEntry,
            RowKind::CompensationCompletion,
            RowKind::StepCursor,
            RowKind::Suspended,
            RowKind::ResumeInput,
        ] {
            assert_eq!(kind_from_db(kind_to_db(kind)), Some(kind));
        }
        assert_eq!(kind_from_db(99), None);
        assert_eq!(kind_from_db(-1), None);
    }

    #[test]
    fn sqlstate_classification() {
        for code in ["08006", "08001", "57P01", "40001", "40P01", "53300"] {
            assert!(transient_sqlstate(code), "{code}");
        }
        for code in ["23505", "42P01", "28P01", "22021"] {
            assert!(!transient_sqlstate(code), "{code}");
        }
    }

    #[test]
    fn application_name_defaults_only_when_unset() {
        let mut config = tokio_postgres::Config::new();
        apply_default_application_name(&mut config);
        assert_eq!(config.get_application_name(), Some("cano"));

        let mut config = tokio_postgres::Config::new();
        config.application_name("my-app");
        apply_default_application_name(&mut config);
        assert_eq!(config.get_application_name(), Some("my-app"));
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn schema_sql_adds_the_scheduler_table_and_sequence() {
        let default = PostgresStore::builder().schema_sql().unwrap();
        assert!(
            default.contains("CREATE TABLE IF NOT EXISTS \"cano_scheduler\" ("),
            "{default}"
        );
        assert!(
            default.contains("flow_id    text        PRIMARY KEY"),
            "{default}"
        );
        assert!(
            default.contains("record     bytea       NOT NULL"),
            "{default}"
        );
        assert!(
            default.contains("CREATE SEQUENCE IF NOT EXISTS \"cano_scheduler_version_seq\";"),
            "{default}"
        );
        assert!(default.contains("\"cano_checkpoints\""), "{default}");

        let custom = PostgresStore::builder()
            .with_schema("wf")
            .with_scheduler_table("sched")
            .schema_sql()
            .unwrap();
        assert!(
            custom.contains("CREATE TABLE IF NOT EXISTS \"wf\".\"sched\" ("),
            "{custom}"
        );
        assert!(
            custom.contains("CREATE SEQUENCE IF NOT EXISTS \"wf\".\"sched_version_seq\";"),
            "{custom}"
        );
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn scheduler_table_names_are_validated() {
        for name in ["x; DROP TABLE y", "Cano", "1abc", "", &"a".repeat(64)] {
            let err = PostgresStore::builder()
                .with_scheduler_table(name)
                .schema_sql()
                .unwrap_err();
            assert_eq!(err.category(), "configuration", "{name:?}");
        }
        for name in ["cano_scheduler", "_t", "t9", &"a".repeat(63)] {
            PostgresStore::builder()
                .with_scheduler_table(name)
                .schema_sql()
                .unwrap();
        }
    }

    #[cfg(feature = "scheduler")]
    #[tokio::test]
    async fn invalid_scheduler_table_fails_before_any_io() {
        let err = PostgresStore::builder()
            .with_scheduler_table("Bad")
            .connect("postgres://u:p@127.0.0.1:1/db")
            .await
            .unwrap_err();
        assert_eq!(err.category(), "configuration");
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn a_long_table_name_keeps_its_sequence_distinct() {
        let long = "a".repeat(63);
        let builder = PostgresStore::builder().with_scheduler_table(long.clone());
        let (table, sequence) = builder.scheduler_names().unwrap();
        assert_eq!(table, format!("\"{long}\""));
        let bare = sequence.trim_matches('"');
        assert_eq!(bare.len(), 63, "{sequence}");
        assert!(bare.ends_with("_version_seq"), "{sequence}");
        assert_ne!(sequence, table);

        let (_, short) = PostgresStore::builder().scheduler_names().unwrap();
        assert_eq!(short, "\"cano_scheduler_version_seq\"");
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn scheduler_statements_swap_on_the_version_column() {
        let sql = SchedulerStatements::new(
            "\"wf\".\"cano_scheduler\"".to_owned(),
            "\"wf\".\"cano_scheduler_version_seq\"",
        );
        assert_eq!(
            sql.select,
            "SELECT version, record FROM \"wf\".\"cano_scheduler\" WHERE flow_id = $1"
        );
        assert!(
            sql.insert.contains(
                "VALUES ($1, nextval('\"wf\".\"cano_scheduler_version_seq\"'), $2) \
                 ON CONFLICT (flow_id) DO NOTHING RETURNING version"
            ),
            "{}",
            sql.insert
        );
        assert!(
            sql.update.contains("SET version = nextval('\"wf\".\"cano_scheduler_version_seq\"'), record = $3, updated_at = now() WHERE flow_id = $1 AND version = $2 RETURNING version"),
            "{}",
            sql.update
        );
        assert_eq!(
            sql.delete,
            "DELETE FROM \"wf\".\"cano_scheduler\" WHERE flow_id = $1"
        );
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn scheduler_errors_are_retagged() {
        let retagged = scheduler_error(CanoError::checkpoint_store("postgres scheduler load: x"));
        assert_eq!(retagged.category(), "scheduler_store");
        assert_eq!(
            scheduler_error(CanoError::configuration("c")).category(),
            "configuration"
        );
    }

    #[tokio::test]
    async fn out_of_range_sequence_fails_fatally_before_any_io() {
        // `with_create_table(false)` builds the pool lazily; port 1 would error on connect.
        let store = PostgresStore::builder()
            .with_create_table(false)
            .with_retry(RetryMode::fixed(3, Duration::from_secs(60)))
            .connect("postgres://u:p@127.0.0.1:1/db")
            .await
            .unwrap();
        let err = store
            .append("wf", CheckpointRow::new(u64::MAX, "S", "t"))
            .await
            .unwrap_err();
        assert_eq!(err.category(), "checkpoint_store");
        assert!(err.to_string().contains("bigint range"), "{err}");
    }

    #[cfg(feature = "scheduler")]
    #[tokio::test]
    async fn impossible_scheduler_versions_conflict_without_io() {
        let store = PostgresStore::builder()
            .with_create_table(false)
            .connect("postgres://u:p@127.0.0.1:1/db")
            .await
            .unwrap();
        for impossible in [0, (i64::MAX as u64) + 1, u64::MAX] {
            let outcome = store
                .compare_and_swap("f", Some(impossible), b"v".to_vec())
                .await
                .unwrap();
            assert_eq!(outcome, CasOutcome::Conflict, "version {impossible}");
        }
    }
}
