//! [`PostgresStore`]: a [`CheckpointStore`] backed by one PostgreSQL table.
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
use cano_macros::checkpoint_store;

pub use deadpool_postgres;
pub use tokio_postgres;

/// The table name used unless [`with_checkpoint_table`](PostgresStoreBuilder::with_checkpoint_table)
/// sets another.
pub const DEFAULT_CHECKPOINT_TABLE: &str = "cano_checkpoints";

/// The `application_name` reported to the server when the connection config sets none.
pub const DEFAULT_APPLICATION_NAME: &str = "cano";

/// The bound on each store call unless
/// [`with_operation_timeout`](PostgresStoreBuilder::with_operation_timeout) sets
/// another.
pub const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// `0x63616e6f636b7074`, `"canockpt"`: serializes concurrent first connects in the migration.
const MIGRATION_LOCK: i64 = 7161126307184210036;

/// A [`CheckpointStore`] that keeps every run in one PostgreSQL table.
///
/// Cheap to clone: clones share the connection pool. Build one with
/// [`builder`](Self::builder), or [`connect`](Self::connect) for the defaults. See the
/// [module docs](self) for the table layout and the failure semantics.
#[derive(Clone)]
pub struct PostgresStore {
    pool: Pool,
    sql: Arc<Statements>,
    operation_timeout: Duration,
    retry: RetryMode,
}

impl fmt::Debug for PostgresStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresStore")
            .field("table", &self.sql.table)
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

    /// Connect with the default settings, creating `cano_checkpoints` if it is missing.
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

    /// The bound on each attempt of `append`, `load_run` and `clear`, connection checkout
    /// included, and on the table migration. Default [`DEFAULT_OPERATION_TIMEOUT`].
    #[must_use]
    pub fn with_operation_timeout(mut self, timeout: Duration) -> Self {
        self.operation_timeout = timeout;
        self
    }

    /// Retry policy for transient failures. Default [`RetryMode::None`]. Conflicts and
    /// malformed rows are never retried. See the [module docs](self) for how a retried
    /// `append` handles its own earlier write.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryMode) -> Self {
        self.retry = retry;
        self
    }

    /// The `CREATE TABLE IF NOT EXISTS` statement for the configured schema and table,
    /// for operators who run DDL themselves.
    pub fn schema_sql(&self) -> Result<String, CanoError> {
        Ok(create_table_sql(&self.qualified_table()?))
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
        self.qualified_table()?;
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
        if self.create_table {
            let migration = format!(
                "SELECT pg_advisory_xact_lock({MIGRATION_LOCK});\n{}",
                create_table_sql(&table)
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
            operation_timeout: self.operation_timeout,
            retry: self.retry,
        })
    }

    /// `"table"` or `"schema"."table"`, after validating both names.
    fn qualified_table(&self) -> Result<String, CanoError> {
        let table = quote_identifier(&self.table)?;
        Ok(match &self.schema {
            Some(schema) => format!("{}.{table}", quote_identifier(schema)?),
            None => table,
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
}
