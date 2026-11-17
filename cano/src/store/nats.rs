//! [`NatsStore`]: a [`CheckpointStore`] and, with the `scheduler` feature, a
//! `SchedulerStore`, both backed by NATS JetStream Key/Value buckets.
//!
//! Compiled with the `store-nats` feature. [`async_nats`] is re-exported here so you can
//! build [`ConnectOptions`](async_nats::ConnectOptions), a client or a JetStream context
//! without adding it as a dependency.
//!
//! ## Bucket layout
//!
//! The checkpoint bucket, `cano_checkpoints` by default (its stream is `KV_cano_checkpoints`), holds
//! every run. Each row is one key, `{workflow_id}.{sequence}` with the sequence in decimal,
//! whose value is the same tagged [postcard](https://docs.rs/postcard) encoding
//! `RedbStore` writes. Workflow id bytes outside `[A-Za-z0-9_-]` are written as
//! `=` and two uppercase hex digits, so `order-1` stays `order-1` and `order.2` becomes
//! `order=2E2`; the empty id is `=`. Keep ids to a few hundred bytes: a key is a NATS
//! subject, and the server limits subject length.
//!
//! - `append` writes with an expected last revision of `0`, so a second write at the same
//!   key fails; the store reports it as a conflict.
//! - `load_run` reads the latest message of every key of the run through an ordered
//!   consumer, skipping delete markers.
//! - `clear` purges the run's subjects from the stream in one request.
//!
//! By default the store creates each bucket on connect with `history: 1`. Use
//! [`with_checkpoint_bucket_config`](NatsStoreBuilder::with_checkpoint_bucket_config) (and
//! `with_scheduler_bucket_config`) to set storage, replicas, placement, compression or a
//! mirror, and [`with_create_bucket(false)`](NatsStoreBuilder::with_create_bucket) when an
//! operator provisions the buckets.
//!
//! ## Scheduler bucket
//!
//! With the `scheduler` feature the store also implements `SchedulerStore`. Its records
//! live in a second bucket, `cano_scheduler` by default (stream `KV_cano_scheduler`), one
//! key per flow, named by the flow id under the same encoding as above. Schedulers that
//! share one bucket form one cluster.
//!
//! - A record's version is its key's revision. JetStream numbers revisions from one
//!   stream-wide counter, so a version never repeats for one flow, even after `remove`.
//! - `compare_and_swap` with `None` writes with an expected revision of `0`, or over the
//!   delete or purge marker left by an earlier `remove`; with `Some(version)` it writes
//!   with that expected revision. A mismatch is a conflict.
//! - `remove` purges the key, so it can be created again.
//!
//! ## Failures
//!
//! Every call is bounded by the operation timeout (10 s by default). With
//! [`with_retry`](NatsStoreBuilder::with_retry) a call that fails on a
//! transient error (a timeout, a lost connection) is retried; a conflict or a malformed row
//! is never retried.
//!
//! An `Err` from `append` after a timeout does not prove the row is absent: the server may
//! have stored it after the client gave up, and `async-nats` sends a buffered publish once
//! it reconnects. A retried `append` treats a byte-identical row already stored at its key
//! as its own earlier write and returns `Ok`; a different row there is a conflict.
//!
//! A retried `compare_and_swap` that meets a conflict reads the key; stored bytes equal to
//! the bytes it sent count as its own earlier write and return `Written` with the stored
//! revision.
//!
//! ## Example
//!
//! ```rust,no_run
//! use cano::store::nats::NatsStore;
//! use cano::store::nats::async_nats::ConnectOptions;
//! use cano::{CanoError, RetryMode};
//! use std::time::Duration;
//!
//! # async fn demo() -> Result<(), CanoError> {
//! let store = NatsStore::builder()
//!     .with_checkpoint_bucket("orders_checkpoints")
//!     .with_retry(RetryMode::fixed(3, Duration::from_millis(200)))
//!     .connect_with_options(
//!         "nats://nats.internal:4222",
//!         ConnectOptions::with_user_and_password("cano".into(), "secret".into()).name("orders"),
//!     )
//!     .await?;
//! # let _ = store;
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::time::Duration;

use async_nats::jetstream::consumer::push::OrderedConfig;
use async_nats::jetstream::consumer::{DeliverPolicy, ReplayPolicy};
use async_nats::jetstream::kv::{self, UpdateErrorKind};
use bytes::Bytes;
use futures_util::StreamExt;

use super::retry::{self, Failure};
use super::{codec, conflict_error};
use crate::RetryMode;
use crate::checkpoint::{CheckpointRow, CheckpointStore};
use crate::error::CanoError;
#[cfg(feature = "scheduler")]
use crate::scheduler::{CasOutcome, SchedulerStore, VersionedRecord};
use cano_macros::checkpoint_store;
#[cfg(feature = "scheduler")]
use cano_macros::scheduler_store;

pub use async_nats;

/// The bucket name used unless [`with_checkpoint_bucket`](NatsStoreBuilder::with_checkpoint_bucket)
/// sets another.
pub const DEFAULT_CHECKPOINT_BUCKET: &str = "cano_checkpoints";

/// The scheduler bucket name used unless
/// [`with_scheduler_bucket`](NatsStoreBuilder::with_scheduler_bucket) sets another.
/// Instances that share a bucket form one scheduler cluster.
#[cfg(feature = "scheduler")]
pub const DEFAULT_SCHEDULER_BUCKET: &str = "cano_scheduler";

/// The client connection name set by [`NatsStoreBuilder::connect`].
pub const DEFAULT_CONNECTION_NAME: &str = "cano";

/// The bound on each store call unless
/// [`with_operation_timeout`](NatsStoreBuilder::with_operation_timeout) sets
/// another.
pub const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// A [`CheckpointStore`], and with the `scheduler` feature a `SchedulerStore`, that keeps
/// its data in NATS JetStream Key/Value buckets.
///
/// Cheap to clone: clones share the client. Build one with [`builder`](Self::builder), or
/// [`connect`](Self::connect) for the defaults. See the [module docs](self) for the key
/// layout and the failure semantics.
#[derive(Clone)]
pub struct NatsStore {
    client: async_nats::Client,
    kv: kv::Store,
    #[cfg(feature = "scheduler")]
    scheduler_kv: kv::Store,
    operation_timeout: Duration,
    retry: RetryMode,
}

impl fmt::Debug for NatsStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("NatsStore");
        debug.field("bucket", &self.kv.name);
        #[cfg(feature = "scheduler")]
        debug.field("scheduler_bucket", &self.scheduler_kv.name);
        debug
            .field("operation_timeout", &self.operation_timeout)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl NatsStore {
    /// A builder with every setting at its default.
    #[must_use]
    pub fn builder() -> NatsStoreBuilder {
        NatsStoreBuilder::default()
    }

    /// Connect with the default settings, creating `cano_checkpoints` if it is missing.
    pub async fn connect(addrs: impl async_nats::ToServerAddrs) -> Result<Self, CanoError> {
        Self::builder().connect(addrs).await
    }

    /// The bucket name.
    pub fn bucket(&self) -> &str {
        &self.kv.name
    }

    /// The Key/Value bucket handle.
    pub fn kv(&self) -> &kv::Store {
        &self.kv
    }

    /// The Key/Value bucket handle of the scheduler records.
    #[cfg(feature = "scheduler")]
    pub fn scheduler_kv(&self) -> &kv::Store {
        &self.scheduler_kv
    }

    /// The NATS client.
    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    async fn append_once(
        &self,
        workflow_id: &str,
        sequence: u64,
        key: &str,
        payload: &Bytes,
        attempt: usize,
    ) -> Result<(), Failure> {
        // Revision 0 means "the subject has no message". `kv.create` is not used: it
        // re-reads the key after any failed publish and so reports its own lost-ack write
        // as `AlreadyExists`.
        let error = match self.kv.update(key, payload.clone(), 0).await {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        match error.kind() {
            UpdateErrorKind::WrongLastRevision => {
                if attempt > 0 {
                    // An earlier attempt may have been stored before its ack was lost.
                    match self.kv.get(key).await {
                        Ok(Some(stored)) if stored == *payload => return Ok(()),
                        Ok(_) => {}
                        Err(e) => return Err(transient("append", e)),
                    }
                }
                Err(Failure::fatal(conflict_error(workflow_id, sequence)))
            }
            UpdateErrorKind::InvalidKey => Err(Failure::fatal(CanoError::checkpoint_store(
                format!("nats append: {error}"),
            ))),
            _ => Err(transient("append", error)),
        }
    }

    async fn load_once(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, Failure> {
        let consumer = self
            .kv
            .stream
            .create_consumer(OrderedConfig {
                deliver_subject: self.client.new_inbox(),
                description: Some("cano checkpoint load".into()),
                filter_subject: run_filter(&self.kv.prefix, &encode_id(workflow_id)),
                replay_policy: ReplayPolicy::Instant,
                deliver_policy: DeliverPolicy::LastPerSubject,
                ..Default::default()
            })
            .await
            .map_err(|e| transient("load_run", e))?;
        if consumer.cached_info().num_pending == 0 {
            return Ok(Vec::new());
        }
        let mut messages = consumer
            .messages()
            .await
            .map_err(|e| transient("load_run", e))?;
        let mut rows = Vec::new();
        // A concurrent `clear` can leave the pending count stale, so no message with
        // `pending == 0` arrives; the operation timeout bounds that wait.
        loop {
            let message = match messages.next().await {
                Some(message) => message.map_err(|e| transient("load_run", e))?,
                None => {
                    return Err(Failure::transient(CanoError::checkpoint_store(
                        "nats load_run: the consumer ended before delivering every row",
                    )));
                }
            };
            let pending = message
                .info()
                .map_err(|e| transient("load_run", e))?
                .pending;
            if !is_tombstone(&message.message) {
                let subject = message.subject.as_str();
                let sequence = sequence_from_subject(subject).ok_or_else(|| {
                    Failure::fatal(CanoError::checkpoint_store(format!(
                        "nats load_run: malformed key {subject:?} in bucket {:?}",
                        self.kv.name
                    )))
                })?;
                rows.push(
                    codec::decode(workflow_id, sequence, &message.payload)
                        .map_err(Failure::fatal)?,
                );
            }
            if pending == 0 {
                break;
            }
        }
        rows.sort_unstable_by_key(|row| row.sequence);
        Ok(rows)
    }

    async fn clear_once(&self, workflow_id: &str) -> Result<(), Failure> {
        self.kv
            .stream
            .purge()
            .filter(run_filter(&self.kv.prefix, &encode_id(workflow_id)))
            .await
            .map(drop)
            .map_err(|e| transient("clear", e))
    }
}

#[cfg(feature = "scheduler")]
impl NatsStore {
    async fn load_flow_once(&self, key: &str) -> Result<Option<VersionedRecord>, Failure> {
        let entry = self
            .scheduler_kv
            .entry(key)
            .await
            .map_err(|e| scheduler_transient("load", e))?;
        Ok(entry.and_then(|entry| live_record(entry.operation, entry.revision, &entry.value)))
    }

    async fn swap_once(
        &self,
        key: &str,
        expected: Option<u64>,
        value: &Bytes,
        attempt: usize,
    ) -> Result<CasOutcome, Failure> {
        // Revision 0 means "the subject has no message". `kv.create` is not used for the
        // same reason as in `append_once`. A deleted or purged key has a marker message,
        // whose revision then stands in for "absent".
        let mut revision = expected.unwrap_or(0);
        loop {
            match self.scheduler_kv.update(key, value.clone(), revision).await {
                Ok(written) => return Ok(CasOutcome::Written(written)),
                Err(error) => match error.kind() {
                    UpdateErrorKind::WrongLastRevision => {}
                    UpdateErrorKind::InvalidKey => {
                        return Err(Failure::fatal(CanoError::scheduler_store(format!(
                            "nats swap: {error}"
                        ))));
                    }
                    _ => return Err(scheduler_transient("swap", error)),
                },
            }
            let current = self
                .scheduler_kv
                .entry(key)
                .await
                .map_err(|e| scheduler_transient("swap", e))?;
            match current {
                Some(entry) if expected.is_none() && entry.operation != kv::Operation::Put => {
                    revision = entry.revision;
                }
                Some(entry)
                    if attempt > 0
                        && entry.operation == kv::Operation::Put
                        && entry.value == *value =>
                {
                    // An earlier attempt may have been stored before its ack was lost.
                    return Ok(CasOutcome::Written(entry.revision));
                }
                _ => return Ok(CasOutcome::Conflict),
            }
        }
    }

    async fn remove_once(&self, key: &str) -> Result<(), Failure> {
        self.scheduler_kv
            .purge(key)
            .await
            .map_err(|e| scheduler_transient("remove", e))
    }
}

#[checkpoint_store]
impl CheckpointStore for NatsStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let sequence = row.sequence;
        let key = format!("{}.{sequence}", encode_id(workflow_id));
        let payload = Bytes::from(codec::encode(row)?);
        let (key, payload) = (key.as_str(), &payload);
        retry::run(&self.retry, move |n| {
            retry::bounded(
                "nats append",
                self.operation_timeout,
                self.append_once(workflow_id, sequence, key, payload, n),
            )
        })
        .await
    }

    async fn load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        retry::run(&self.retry, |_| {
            retry::bounded(
                "nats load_run",
                self.operation_timeout,
                self.load_once(workflow_id),
            )
        })
        .await
    }

    async fn clear(&self, workflow_id: &str) -> Result<(), CanoError> {
        retry::run(&self.retry, |_| {
            retry::bounded(
                "nats clear",
                self.operation_timeout,
                self.clear_once(workflow_id),
            )
        })
        .await
    }
}

#[cfg(feature = "scheduler")]
#[scheduler_store]
impl SchedulerStore for NatsStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        let key = encode_id(flow_id);
        let key = key.as_str();
        retry::run(&self.retry, |_| {
            scheduler_bounded(
                "nats scheduler load",
                self.operation_timeout,
                self.load_flow_once(key),
            )
        })
        .await
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        let key = encode_id(flow_id);
        let (key, value) = (key.as_str(), &Bytes::from(value));
        retry::run(&self.retry, move |n| {
            scheduler_bounded(
                "nats scheduler swap",
                self.operation_timeout,
                self.swap_once(key, expected, value, n),
            )
        })
        .await
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        let key = encode_id(flow_id);
        let key = key.as_str();
        retry::run(&self.retry, |_| {
            scheduler_bounded(
                "nats scheduler remove",
                self.operation_timeout,
                self.remove_once(key),
            )
        })
        .await
    }
}

/// Settings for a [`NatsStore`]; start from [`NatsStore::builder`].
#[derive(Debug, Clone)]
pub struct NatsStoreBuilder {
    config: kv::Config,
    create_bucket: bool,
    domain: Option<String>,
    api_prefix: Option<String>,
    operation_timeout: Duration,
    retry: RetryMode,
    #[cfg(feature = "scheduler")]
    scheduler_config: kv::Config,
}

impl Default for NatsStoreBuilder {
    fn default() -> Self {
        Self {
            config: kv::Config {
                bucket: DEFAULT_CHECKPOINT_BUCKET.to_owned(),
                description: "cano workflow checkpoints".to_owned(),
                history: 1,
                ..Default::default()
            },
            create_bucket: true,
            domain: None,
            api_prefix: None,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            retry: RetryMode::None,
            #[cfg(feature = "scheduler")]
            scheduler_config: kv::Config {
                bucket: DEFAULT_SCHEDULER_BUCKET.to_owned(),
                description: "cano scheduler flow records".to_owned(),
                history: 1,
                ..Default::default()
            },
        }
    }
}

impl NatsStoreBuilder {
    /// The bucket name. Default [`DEFAULT_CHECKPOINT_BUCKET`]. Must be one or more of
    /// `[A-Za-z0-9_-]`.
    #[must_use]
    pub fn with_checkpoint_bucket(mut self, bucket: impl Into<String>) -> Self {
        self.config.bucket = bucket.into();
        self
    }

    /// The full bucket configuration used when the store creates the bucket: storage,
    /// replicas, `max_age`, placement, compression, mirror. An empty `bucket` keeps the
    /// name already set ([`DEFAULT_CHECKPOINT_BUCKET`] unless [`with_checkpoint_bucket`](Self::with_checkpoint_bucket) ran).
    /// Default: `history: 1`, a description, everything else at the server's defaults.
    #[must_use]
    pub fn with_checkpoint_bucket_config(mut self, config: kv::Config) -> Self {
        let bucket = std::mem::take(&mut self.config.bucket);
        self.config = config;
        if self.config.bucket.is_empty() {
            self.config.bucket = bucket;
        }
        self
    }

    /// The scheduler bucket name. Default [`DEFAULT_SCHEDULER_BUCKET`]. Must be one or more
    /// of `[A-Za-z0-9_-]` and differ from the checkpoint bucket. Instances that share a
    /// scheduler bucket form one scheduler cluster.
    #[cfg(feature = "scheduler")]
    #[must_use]
    pub fn with_scheduler_bucket(mut self, bucket: impl Into<String>) -> Self {
        self.scheduler_config.bucket = bucket.into();
        self
    }

    /// The full configuration used when the store creates the scheduler bucket: storage,
    /// replicas, `max_age`, placement, compression, mirror. An empty `bucket` keeps the
    /// name already set ([`DEFAULT_SCHEDULER_BUCKET`] unless
    /// [`with_scheduler_bucket`](Self::with_scheduler_bucket) ran). Default: `history: 1`, a
    /// description, everything else at the server's defaults.
    #[cfg(feature = "scheduler")]
    #[must_use]
    pub fn with_scheduler_bucket_config(mut self, config: kv::Config) -> Self {
        let bucket = std::mem::take(&mut self.scheduler_config.bucket);
        self.scheduler_config = config;
        if self.scheduler_config.bucket.is_empty() {
            self.scheduler_config.bucket = bucket;
        }
        self
    }

    /// Whether to create the buckets on connect when they are missing. Default `true`.
    #[must_use]
    pub fn with_create_bucket(mut self, create_bucket: bool) -> Self {
        self.create_bucket = create_bucket;
        self
    }

    /// Reach JetStream in another domain, such as a hub from a leaf node. Exclusive with
    /// [`with_api_prefix`](Self::with_api_prefix); ignored by
    /// [`build_with_context`](Self::build_with_context).
    #[must_use]
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Reach JetStream through an API prefix imported from another account. Exclusive with
    /// [`with_domain`](Self::with_domain); ignored by
    /// [`build_with_context`](Self::build_with_context).
    #[must_use]
    pub fn with_api_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.api_prefix = Some(prefix.into());
        self
    }

    /// The bound on each attempt of `append`, `load_run` and `clear` (and, with the
    /// `scheduler` feature, of the scheduler calls), and on opening a bucket. Default
    /// [`DEFAULT_OPERATION_TIMEOUT`].
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

    /// Connect with the connection name [`DEFAULT_CONNECTION_NAME`] and the other
    /// [`ConnectOptions`](async_nats::ConnectOptions) at their defaults.
    pub async fn connect(
        self,
        addrs: impl async_nats::ToServerAddrs,
    ) -> Result<NatsStore, CanoError> {
        let options = async_nats::ConnectOptions::new().name(DEFAULT_CONNECTION_NAME);
        self.connect_with_options(addrs, options).await
    }

    /// Connect with your [`ConnectOptions`](async_nats::ConnectOptions), used as given:
    /// credentials, TLS, timeouts, reconnect policy, connection name.
    pub async fn connect_with_options(
        self,
        addrs: impl async_nats::ToServerAddrs,
        options: async_nats::ConnectOptions,
    ) -> Result<NatsStore, CanoError> {
        self.validate()?;
        let client = options
            .connect(addrs)
            .await
            .map_err(|e| CanoError::checkpoint_store(format!("nats connect: {e}")))?;
        self.build_with_client(client).await
    }

    /// Use a client you connected. The domain or API prefix setting picks the JetStream
    /// context.
    pub async fn build_with_client(
        self,
        client: async_nats::Client,
    ) -> Result<NatsStore, CanoError> {
        self.validate()?;
        let context = match (&self.domain, &self.api_prefix) {
            (Some(domain), None) => async_nats::jetstream::with_domain(client, domain),
            (None, Some(prefix)) => async_nats::jetstream::with_prefix(client, prefix),
            _ => async_nats::jetstream::new(client),
        };
        self.build_with_context(context).await
    }

    /// Use a JetStream context you built. The domain and API prefix settings do not apply.
    pub async fn build_with_context(
        self,
        context: async_nats::jetstream::Context,
    ) -> Result<NatsStore, CanoError> {
        self.validate_buckets()?;
        let kv = open_bucket(
            &context,
            self.config,
            self.create_bucket,
            self.operation_timeout,
            CanoError::checkpoint_store::<String>,
        )
        .await?;
        #[cfg(feature = "scheduler")]
        let scheduler_kv = open_bucket(
            &context,
            self.scheduler_config,
            self.create_bucket,
            self.operation_timeout,
            CanoError::scheduler_store::<String>,
        )
        .await?;
        Ok(NatsStore {
            client: context.client(),
            kv,
            #[cfg(feature = "scheduler")]
            scheduler_kv,
            operation_timeout: self.operation_timeout,
            retry: self.retry,
        })
    }

    fn validate(&self) -> Result<(), CanoError> {
        self.validate_buckets()?;
        if self.domain.is_some() && self.api_prefix.is_some() {
            return Err(CanoError::configuration(
                "set either a JetStream domain or an API prefix, not both",
            ));
        }
        Ok(())
    }

    fn validate_buckets(&self) -> Result<(), CanoError> {
        validate_bucket(&self.config.bucket)?;
        #[cfg(feature = "scheduler")]
        {
            validate_bucket(&self.scheduler_config.bucket)?;
            if self.scheduler_config.bucket == self.config.bucket {
                return Err(CanoError::configuration(format!(
                    "the checkpoint and scheduler buckets must differ, both are {:?}",
                    self.config.bucket
                )));
            }
        }
        Ok(())
    }
}

fn validate_bucket(bucket: &str) -> Result<(), CanoError> {
    let valid = !bucket.is_empty()
        && bucket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(CanoError::configuration(format!(
            "invalid NATS bucket name {bucket:?}"
        )))
    }
}

/// Opens `config`'s bucket, creating it when missing and `create_bucket` is set. `error`
/// builds the store's error for a failure.
async fn open_bucket(
    context: &async_nats::jetstream::Context,
    config: kv::Config,
    create_bucket: bool,
    timeout: Duration,
    error: fn(String) -> CanoError,
) -> Result<kv::Store, CanoError> {
    let bucket = config.bucket.clone();
    retry::bounded("nats open bucket", timeout, async {
        let get_err = match context.get_key_value(bucket.as_str()).await {
            Ok(kv) => return Ok(kv),
            Err(e) => e,
        };
        if !create_bucket {
            return Err(Failure::fatal(error(format!(
                "nats bucket {bucket:?} unavailable and bucket creation is disabled: {get_err}"
            ))));
        }
        context
            .create_key_value(config)
            .await
            .map_err(|create_err| {
                Failure::fatal(error(format!(
                    "nats open bucket {bucket:?}: {get_err}; create: {create_err}"
                )))
            })
    })
    .await
    .map_err(|failure| failure.error)
}

fn transient(op: &str, e: impl fmt::Display) -> Failure {
    Failure::transient(CanoError::checkpoint_store(format!("nats {op}: {e}")))
}

#[cfg(feature = "scheduler")]
fn scheduler_transient(op: &str, e: impl fmt::Display) -> Failure {
    Failure::transient(CanoError::scheduler_store(format!("nats {op}: {e}")))
}

/// [`retry::bounded`] with the scheduler store's error for an elapsed limit.
#[cfg(feature = "scheduler")]
async fn scheduler_bounded<T>(
    what: &str,
    limit: Duration,
    fut: impl Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(Failure::transient(CanoError::scheduler_store(format!(
            "{what}: timed out after {limit:?}"
        )))),
    }
}

/// The record of a key's latest entry; a delete or purge marker means no record.
#[cfg(feature = "scheduler")]
fn live_record(operation: kv::Operation, revision: u64, value: &Bytes) -> Option<VersionedRecord> {
    (operation == kv::Operation::Put).then(|| VersionedRecord {
        version: revision,
        value: value.to_vec(),
    })
}

/// A delete or purge marker, or a server marker such as a TTL expiry.
fn is_tombstone(message: &async_nats::Message) -> bool {
    message.headers.as_ref().is_some_and(|headers| {
        headers
            .get("KV-Operation")
            .is_some_and(|op| matches!(op.as_str(), "DEL" | "PURGE"))
            || headers.get("Nats-Marker-Reason").is_some()
    })
}

/// One subject token for `workflow_id`: bytes in `[A-Za-z0-9_-]` as is, every other byte as
/// `=` and two uppercase hex digits, the empty id as `=`. Injective, and never contains `.`.
fn encode_id(workflow_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    if workflow_id.is_empty() {
        return "=".to_owned();
    }
    let mut encoded = String::with_capacity(workflow_id.len());
    for b in workflow_id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            encoded.push(char::from(b));
        } else {
            encoded.push('=');
            encoded.push(char::from(HEX[usize::from(b >> 4)]));
            encoded.push(char::from(HEX[usize::from(b & 0x0F)]));
        }
    }
    encoded
}

/// The stream subject filter matching every row of one run.
fn run_filter(prefix: &str, encoded_id: &str) -> String {
    format!("{prefix}{encoded_id}.*")
}

/// The sequence in the last `.`-separated token of a row's subject.
fn sequence_from_subject(subject: &str) -> Option<u64> {
    subject.rsplit('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_id_examples() {
        assert_eq!(encode_id("order-1"), "order-1");
        assert_eq!(encode_id(""), "=");
        assert_eq!(encode_id("a.b"), "a=2Eb");
        assert_eq!(encode_id("a=b"), "a=3Db");
        assert_eq!(encode_id("é"), "=C3=A9");
        assert_eq!(encode_id("wild*card>"), "wild=2Acard=3E");
    }

    #[test]
    fn encode_id_is_injective_and_yields_one_valid_token() {
        let inputs = ["a", "a.b", "a=2Eb", "", "=", " x", "a/b", "ünï©ode-✓"];
        let outputs: Vec<String> = inputs.iter().map(|id| encode_id(id)).collect();
        for (i, a) in outputs.iter().enumerate() {
            for b in &outputs[i + 1..] {
                assert_ne!(a, b);
            }
            assert!(!a.is_empty());
            assert!(!a.contains('.'), "{a}");
            assert!(
                a.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-/_=.".contains(&b)),
                "{a}"
            );
        }
    }

    #[test]
    fn subjects_and_filters() {
        assert_eq!(
            sequence_from_subject("$KV.cano_checkpoints.order-1.42"),
            Some(42)
        );
        assert_eq!(
            sequence_from_subject("$KV.cano_checkpoints.order-1.x"),
            None
        );
        assert_eq!(
            run_filter("$KV.cano_checkpoints.", "order=2E2"),
            "$KV.cano_checkpoints.order=2E2.*"
        );
    }

    #[test]
    fn bucket_names_are_validated() {
        validate_bucket("cano_checkpoints").unwrap();
        validate_bucket("Orders-1").unwrap();
        for bad in ["", "a.b", "a b", "a*", "ü"] {
            assert_eq!(
                validate_bucket(bad).unwrap_err().category(),
                "configuration",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn bucket_config_keeps_the_name_when_empty() {
        let builder = NatsStore::builder().with_checkpoint_bucket_config(kv::Config {
            history: 5,
            ..Default::default()
        });
        assert_eq!(builder.config.bucket, DEFAULT_CHECKPOINT_BUCKET);
        assert_eq!(builder.config.history, 5);
        let builder = NatsStore::builder()
            .with_checkpoint_bucket("custom")
            .with_checkpoint_bucket_config(kv::Config::default());
        assert_eq!(builder.config.bucket, "custom");
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn scheduler_bucket_defaults_and_config() {
        let builder = NatsStore::builder();
        assert_eq!(builder.scheduler_config.bucket, DEFAULT_SCHEDULER_BUCKET);
        assert_eq!(DEFAULT_SCHEDULER_BUCKET, "cano_scheduler");
        assert_eq!(builder.scheduler_config.history, 1);
        builder.validate().unwrap();

        let builder = NatsStore::builder().with_scheduler_bucket_config(kv::Config {
            history: 3,
            ..Default::default()
        });
        assert_eq!(builder.scheduler_config.bucket, DEFAULT_SCHEDULER_BUCKET);
        assert_eq!(builder.scheduler_config.history, 3);
        let builder = NatsStore::builder()
            .with_scheduler_bucket("custom")
            .with_scheduler_bucket_config(kv::Config::default());
        assert_eq!(builder.scheduler_config.bucket, "custom");
        assert_eq!(builder.config.bucket, DEFAULT_CHECKPOINT_BUCKET);
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn scheduler_buckets_are_validated() {
        for builder in [
            NatsStore::builder().with_scheduler_bucket("a.b"),
            NatsStore::builder().with_scheduler_bucket(""),
            NatsStore::builder().with_scheduler_bucket(DEFAULT_CHECKPOINT_BUCKET),
            NatsStore::builder().with_checkpoint_bucket(DEFAULT_SCHEDULER_BUCKET),
        ] {
            assert_eq!(builder.validate().unwrap_err().category(), "configuration");
        }
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn flow_ids_become_one_subject_token() {
        // The scheduler key is `encode_id` alone: no `.`, so a flow id never forms a
        // wildcard-matchable multi-token subject, and ids that differ stay distinct.
        let ids = ["a", "a.b", "a/b", "ab", "x y", "wild*", "é", ""];
        let keys: Vec<String> = ids.iter().map(|id| encode_id(id)).collect();
        for (i, a) in keys.iter().enumerate() {
            assert!(!a.is_empty() && !a.contains('.'), "{a}");
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(encode_id(&"x".repeat(200)).len(), 200);
    }

    #[cfg(feature = "scheduler")]
    #[test]
    fn markers_are_not_records() {
        let value = Bytes::from_static(b"v");
        assert_eq!(
            live_record(kv::Operation::Put, 7, &value),
            Some(VersionedRecord {
                version: 7,
                value: b"v".to_vec()
            })
        );
        assert_eq!(live_record(kv::Operation::Delete, 8, &value), None);
        assert_eq!(live_record(kv::Operation::Purge, 9, &value), None);
    }

    #[cfg(feature = "scheduler")]
    #[tokio::test]
    async fn scheduler_bucket_name_is_checked_before_connecting() {
        let err = NatsStore::builder()
            .with_scheduler_bucket("a.b")
            .connect("nats://127.0.0.1:1")
            .await
            .unwrap_err();
        assert_eq!(err.category(), "configuration");
    }

    #[tokio::test]
    async fn domain_and_prefix_are_exclusive() {
        let err = NatsStore::builder()
            .with_domain("hub")
            .with_api_prefix("JS.acc")
            .connect("nats://127.0.0.1:1")
            .await
            .unwrap_err();
        assert_eq!(err.category(), "configuration");
    }
}
