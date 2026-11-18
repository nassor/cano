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
//! whose value is a two-byte `CW` tag, a random 16-byte write id and the
//! [postcard](https://docs.rs/postcard) encoding of the row. Workflow id bytes outside `[A-Za-z0-9_-]` are written as
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
//! The underlying [`async_nats::Client`] reconnects in the background according to its
//! [`ConnectOptions`](async_nats::ConnectOptions) without recreating the store. While the
//! client is disconnected, store attempts fail fast as transient errors (`client is not
//! connected`) rather than waiting out `operation_timeout`, and calls succeed again once it
//! reconnects.
//!
//! Every call is bounded by the operation timeout (10 s by default). With
//! [`with_retry`](NatsStoreBuilder::with_retry) a call that fails on a
//! transient error (a timeout, a lost connection) is retried; a conflict or a malformed row
//! is never retried.
//!
//! [`RetryMode`] is finite (`RetryMode::None` by default): when the connection stays
//! unreachable past [`RetryMode::max_attempts`], the call stops and returns
//! `Err(CanoError::CheckpointStore(..))` (or `Err(CanoError::SchedulerStore(..))` on
//! scheduler calls). Inside a workflow, a failed `append` stops execution before the
//! incoming state's task runs and drains in-memory saga compensations, leaving earlier rows
//! stored for [`Workflow::resume_from`](crate::Workflow::resume_from) once connectivity
//! returns (unless a saga rollback already undid those steps, in which case `clear` the
//! stranded log first). A failed exit-state or post-rollback `clear` keeps the run's
//! outcome and fires
//! [`WorkflowObserver::on_checkpoint_clear_failed`](crate::WorkflowObserver::on_checkpoint_clear_failed).
//!
//! An `Err` from `append` after a timeout does not prove the row is absent: the server may
//! have stored it after the client gave up, and `async-nats` sends a buffered publish once
//! it reconnects. Each `append` stamps a fresh 128-bit `write_id` (`ROW_TAG_V2`), so a
//! retried `append` treats a stored row at its key as its own earlier write only when its
//! full payload — `write_id` included — matches; a rival's identical row is a conflict.
//!
//! A retried `compare_and_swap` that meets a conflict reads the key; stored bytes equal to
//! the bytes it sent count as its own earlier write and return `Written` with the stored
//! revision.
//!
//! `remove` reads the key's current revision and purges with `purge_expect_revision`, so a
//! buffered timed-out purge cannot delete a newly created record after reconnect.
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
#[cfg(feature = "scheduler")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_nats::connection::State as ConnectionState;
use async_nats::jetstream::consumer::push::{OrderedConfig, OrderedErrorKind};
use async_nats::jetstream::consumer::{Consumer, DeliverPolicy, ReplayPolicy};
use async_nats::jetstream::context::{
    ConsumerInfoErrorKind, CreateKeyValueErrorKind, KeyValueErrorKind,
    PublishError as JsPublishError, PublishErrorKind as JsPublishErrorKind,
    RequestError as JsRequestError, RequestErrorKind as JsRequestErrorKind,
};
use async_nats::jetstream::kv::{self, EntryErrorKind, UpdateErrorKind};
use async_nats::jetstream::stream::{ConsumerErrorKind, PurgeErrorKind};
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

/// How long `load_once` waits on `messages.next()` before checking `consumer_info` to see
/// whether a concurrent `clear()` purged all pending messages.
const LOAD_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Best-effort deletes the ephemeral `load_run` consumer if the future is dropped early
/// (for instance on an `operation_timeout`), and supports inline deletion before return.
struct ConsumerCleanupGuard {
    stream: async_nats::jetstream::stream::Stream,
    client: async_nats::Client,
    name: Option<String>,
}

impl ConsumerCleanupGuard {
    async fn delete_now(&mut self, timeout: Duration) {
        if let Some(name) = self.name.take()
            && self.client.connection_state() == ConnectionState::Connected
        {
            let _ = tokio::time::timeout(timeout, self.stream.delete_consumer(&name)).await;
        }
    }
}

impl Drop for ConsumerCleanupGuard {
    fn drop(&mut self) {
        if let Some(name) = self.name.take()
            && self.client.connection_state() == ConnectionState::Connected
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let stream = self.stream.clone();
            handle.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(1), stream.delete_consumer(&name))
                    .await;
            });
        }
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

    fn ensure_connected(
        &self,
        op: &str,
        make_error: fn(String) -> CanoError,
    ) -> Result<(), Failure> {
        if self.client.connection_state() != ConnectionState::Connected {
            return Err(Failure::transient(make_error(format!(
                "nats {op}: client is not connected"
            ))));
        }
        Ok(())
    }

    fn check_max_payload(
        &self,
        len: usize,
        op: &str,
        make_error: fn(String) -> CanoError,
    ) -> Result<(), Failure> {
        if let Some(info) = self.client.try_server_info()
            && info.max_payload > 0
            && len > info.max_payload
        {
            return Err(Failure::fatal(make_error(format!(
                "nats {op}: payload of {len} bytes exceeds server max_payload of {} bytes",
                info.max_payload
            ))));
        }
        Ok(())
    }

    async fn append_once(
        &self,
        workflow_id: &str,
        sequence: u64,
        key: &str,
        payload: &Bytes,
        sent: &AtomicBool,
    ) -> Result<(), Failure> {
        self.ensure_connected("append", CanoError::checkpoint_store)?;
        self.check_max_payload(payload.len(), "append", CanoError::checkpoint_store)?;
        // Revision 0 means "the subject has no message". `kv.create` is not used: it
        // re-reads the key after any failed publish and so reports its own lost-ack write
        // as `AlreadyExists`.
        let prior_sent = sent.swap(true, Ordering::Relaxed);
        let error = match self.kv.update(key, payload.clone(), 0).await {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        match error.kind() {
            UpdateErrorKind::WrongLastRevision => {
                if prior_sent {
                    // An earlier attempt actually sent `update` and may have been stored
                    // before its ack was lost. `payload` carries this invocation's 128-bit
                    // `write_id`, so equal bytes guarantee it was our own write.
                    match self.kv.get(key).await {
                        Ok(Some(stored)) if stored == *payload => return Ok(()),
                        Ok(_) => {}
                        Err(e) => {
                            return Err(classify_entry_error(
                                "append",
                                &e,
                                CanoError::checkpoint_store,
                            ));
                        }
                    }
                }
                Err(Failure::fatal(conflict_error(workflow_id, sequence)))
            }
            UpdateErrorKind::InvalidKey => Err(Failure::fatal(CanoError::checkpoint_store(
                format!("nats append: {error}"),
            ))),
            _ => Err(classify_update_error(
                "append",
                &error,
                CanoError::checkpoint_store,
            )),
        }
    }

    async fn load_once(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, Failure> {
        self.ensure_connected("load_run", CanoError::checkpoint_store)?;
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
            .map_err(|e| classify_consumer_error("load_run", &e))?;
        let consumer_name = consumer.cached_info().name.clone();
        let mut cleanup = ConsumerCleanupGuard {
            stream: self.kv.stream.clone(),
            client: self.client.clone(),
            name: Some(consumer_name.clone()),
        };
        let result = self
            .read_consumer_rows(workflow_id, &consumer_name, consumer)
            .await;
        let delete_limit = if result.is_ok() {
            self.operation_timeout.min(Duration::from_secs(1))
        } else {
            Duration::from_millis(200)
        };
        cleanup.delete_now(delete_limit).await;
        result
    }

    async fn read_consumer_rows(
        &self,
        workflow_id: &str,
        consumer_name: &str,
        consumer: Consumer<OrderedConfig>,
    ) -> Result<Vec<CheckpointRow>, Failure> {
        if consumer.cached_info().num_pending == 0 {
            return Ok(Vec::new());
        }
        let mut messages = consumer
            .messages()
            .await
            .map_err(|e| transient("load_run", e))?;
        let mut rows = Vec::new();
        loop {
            let next = match tokio::time::timeout(LOAD_POLL_INTERVAL, messages.next()).await {
                Ok(next) => next,
                Err(_) => {
                    // A concurrent `clear` can purge pending messages before they arrive,
                    // so no message with `pending == 0` is ever delivered. Poll the
                    // consumer's pending count to distinguish purged from slow delivery.
                    match self.kv.stream.consumer_info(consumer_name).await {
                        Ok(info) if info.num_pending == 0 => break,
                        Ok(_) => continue,
                        Err(e) => return Err(classify_consumer_info_error("load_run", &e)),
                    }
                }
            };
            let message = match next {
                Some(message) => message.map_err(|e| classify_ordered_error("load_run", &e))?,
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
        self.ensure_connected("clear", CanoError::checkpoint_store)?;
        self.kv
            .stream
            .purge()
            .filter(run_filter(&self.kv.prefix, &encode_id(workflow_id)))
            .await
            .map(drop)
            .map_err(|e| classify_purge_error("clear", &e))
    }
}

#[cfg(feature = "scheduler")]
impl NatsStore {
    async fn load_flow_once(&self, key: &str) -> Result<Option<VersionedRecord>, Failure> {
        self.ensure_connected("load", CanoError::scheduler_store)?;
        let entry = self
            .scheduler_kv
            .entry(key)
            .await
            .map_err(|e| classify_entry_error("load", &e, CanoError::scheduler_store))?;
        Ok(entry.and_then(|entry| live_record(entry.operation, entry.revision, &entry.value)))
    }

    async fn swap_once(
        &self,
        key: &str,
        expected: Option<u64>,
        value: &Bytes,
        target_rev: &AtomicU64,
        sent_target: &AtomicBool,
    ) -> Result<CasOutcome, Failure> {
        if expected == Some(0) {
            // JetStream revisions start at 1; passing 0 to `kv.update` means "create".
            return Ok(CasOutcome::Conflict);
        }
        self.ensure_connected("swap", CanoError::scheduler_store)?;
        self.check_max_payload(value.len(), "swap", CanoError::scheduler_store)?;
        // Revision 0 means "the subject has no message". `kv.create` is not used for the
        // same reason as in `append_once`. A deleted or purged key has a marker message,
        // whose revision then stands in for "absent".
        let mut revision = target_rev.load(Ordering::Relaxed);
        loop {
            let prior_sent = sent_target.swap(true, Ordering::Relaxed);
            match self.scheduler_kv.update(key, value.clone(), revision).await {
                Ok(written) => return Ok(CasOutcome::Written(written)),
                Err(error) => match error.kind() {
                    UpdateErrorKind::WrongLastRevision => {}
                    UpdateErrorKind::InvalidKey => {
                        return Err(Failure::fatal(CanoError::scheduler_store(format!(
                            "nats swap: {error}"
                        ))));
                    }
                    _ => {
                        return Err(classify_update_error(
                            "swap",
                            &error,
                            CanoError::scheduler_store,
                        ));
                    }
                },
            }
            if !prior_sent && expected.is_some() {
                return Ok(CasOutcome::Conflict);
            }
            let current = self
                .scheduler_kv
                .entry(key)
                .await
                .map_err(|e| classify_entry_error("swap", &e, CanoError::scheduler_store))?;
            match current {
                Some(entry)
                    if expected.is_none()
                        && entry.operation != kv::Operation::Put
                        && entry.revision > revision =>
                {
                    target_rev.store(entry.revision, Ordering::Relaxed);
                    sent_target.store(false, Ordering::Relaxed);
                    revision = entry.revision;
                    continue;
                }
                Some(entry)
                    if prior_sent
                        && entry.operation == kv::Operation::Put
                        && entry.value == *value
                        && entry.revision > revision =>
                {
                    // An earlier attempt actually sent `update` at `revision` and may have
                    // been stored before its ack was lost.
                    return Ok(CasOutcome::Written(entry.revision));
                }
                _ => return Ok(CasOutcome::Conflict),
            }
        }
    }

    async fn remove_once(&self, key: &str) -> Result<(), Failure> {
        self.ensure_connected("remove", CanoError::scheduler_store)?;
        loop {
            let entry = self
                .scheduler_kv
                .entry(key)
                .await
                .map_err(|e| classify_entry_error("remove", &e, CanoError::scheduler_store))?;
            let Some(entry) = entry.filter(|entry| entry.operation == kv::Operation::Put) else {
                return Ok(());
            };
            match self
                .scheduler_kv
                .purge_expect_revision(key, Some(entry.revision))
                .await
            {
                Ok(()) => return Ok(()),
                Err(error) => match error.kind() {
                    // Concurrent swap or remove changed the key's revision; re-read.
                    UpdateErrorKind::WrongLastRevision => continue,
                    UpdateErrorKind::InvalidKey => {
                        return Err(Failure::fatal(CanoError::scheduler_store(format!(
                            "nats remove: {error}"
                        ))));
                    }
                    _ => {
                        return Err(classify_update_error(
                            "remove",
                            &error,
                            CanoError::scheduler_store,
                        ));
                    }
                },
            }
        }
    }
}

#[checkpoint_store]
impl CheckpointStore for NatsStore {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        let sequence = row.sequence;
        let key = format!("{}.{sequence}", encode_id(workflow_id));
        let write_id = rand::random::<u128>();
        let payload = Bytes::from(codec::encode_with_write_id(row, write_id)?);
        let (key, payload) = (key.as_str(), &payload);
        let sent = AtomicBool::new(false);
        let sent = &sent;
        retry::run(&self.retry, move |_| {
            retry::bounded(
                "nats append",
                self.operation_timeout,
                self.append_once(workflow_id, sequence, key, payload, sent),
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
        let target_rev = AtomicU64::new(expected.unwrap_or(0));
        let sent_target = AtomicBool::new(false);
        let (target_rev, sent_target) = (&target_rev, &sent_target);
        retry::run(&self.retry, move |_| {
            scheduler_bounded(
                "nats scheduler swap",
                self.operation_timeout,
                self.swap_once(key, expected, value, target_rev, sent_target),
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
    /// replicas, placement, compression, mirror. `max_age` must remain zero so checkpoint
    /// and suspended state cannot silently expire. An empty `bucket` keeps the name already
    /// set ([`DEFAULT_CHECKPOINT_BUCKET`] unless [`with_checkpoint_bucket`](Self::with_checkpoint_bucket) ran).
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
    /// replicas, placement, compression, mirror. `max_age` must remain zero so `Tripped`
    /// and `Backoff` state cannot silently expire. An empty `bucket` keeps the name already
    /// set ([`DEFAULT_SCHEDULER_BUCKET`] unless
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

    /// Connect with the connection name [`DEFAULT_CONNECTION_NAME`], `request_timeout` aligned
    /// to `operation_timeout`, and the other [`ConnectOptions`](async_nats::ConnectOptions) at
    /// their defaults.
    pub async fn connect(
        self,
        addrs: impl async_nats::ToServerAddrs,
    ) -> Result<NatsStore, CanoError> {
        let options = async_nats::ConnectOptions::new()
            .name(DEFAULT_CONNECTION_NAME)
            .request_timeout(Some(self.operation_timeout));
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
        let limit = self.operation_timeout;
        let client = tokio::time::timeout(limit, options.connect(addrs))
            .await
            .map_err(|_| {
                CanoError::checkpoint_store(format!("nats connect: timed out after {limit:?}"))
            })?
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

    /// Use a JetStream context you built. The domain and API prefix settings do not apply;
    /// the context's timeout is aligned to `operation_timeout`.
    pub async fn build_with_context(
        self,
        mut context: async_nats::jetstream::Context,
    ) -> Result<NatsStore, CanoError> {
        self.validate_buckets()?;
        context.set_timeout(self.operation_timeout);
        let kv = open_bucket(
            &context,
            self.config,
            self.create_bucket,
            self.operation_timeout,
            &self.retry,
            CanoError::checkpoint_store::<String>,
        )
        .await?;
        #[cfg(feature = "scheduler")]
        let scheduler_kv = open_bucket(
            &context,
            self.scheduler_config,
            self.create_bucket,
            self.operation_timeout,
            &self.retry,
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
        if !self.config.max_age.is_zero() {
            return Err(CanoError::configuration(
                "checkpoint bucket max_age must be zero so recovery and suspended state cannot expire",
            ));
        }
        #[cfg(feature = "scheduler")]
        {
            validate_bucket(&self.scheduler_config.bucket)?;
            if !self.scheduler_config.max_age.is_zero() {
                return Err(CanoError::configuration(
                    "scheduler bucket max_age must be zero so tripped and backoff state cannot expire",
                ));
            }
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
    retry_mode: &RetryMode,
    error: fn(String) -> CanoError,
) -> Result<kv::Store, CanoError> {
    let bucket = config.bucket.clone();
    retry::run(retry_mode, |_| {
        let config = config.clone();
        let bucket = bucket.as_str();
        bounded_with("nats open bucket", timeout, error, async move {
            let get_err = match context.get_key_value(bucket).await {
                Ok(kv) => return Ok(kv),
                Err(e) => e,
            };
            if get_err.kind() == KeyValueErrorKind::InvalidStoreName {
                return Err(Failure::fatal(error(format!(
                    "nats bucket {bucket:?}: {get_err}"
                ))));
            }
            if !create_bucket {
                let err = error(format!(
                    "nats bucket {bucket:?} unavailable and bucket creation is disabled: {get_err}"
                ));
                return Err(if is_fatal_get_kv(&get_err) {
                    Failure::fatal(err)
                } else {
                    Failure::transient(err)
                });
            }
            context
                .create_key_value(config)
                .await
                .map_err(|create_err| {
                    let err = error(format!(
                        "nats open bucket {bucket:?}: {get_err}; create: {create_err}"
                    ));
                    if is_fatal_create_kv(&create_err) {
                        Failure::fatal(err)
                    } else {
                        Failure::transient(err)
                    }
                })
        })
    })
    .await
}

fn transient(op: &str, e: impl fmt::Display) -> Failure {
    Failure::transient(CanoError::checkpoint_store(format!("nats {op}: {e}")))
}

fn is_fatal_jetstream_error(err: &async_nats::jetstream::Error) -> bool {
    use async_nats::jetstream::ErrorCode;
    matches!(
        err.error_code(),
        ErrorCode::BAD_REQUEST
            | ErrorCode::INVALID_JSON
            | ErrorCode::STREAM_INVALID_CONFIG
            | ErrorCode::STREAM_MESSAGE_EXCEEDS_MAXIMUM
            | ErrorCode::STREAM_NOT_FOUND
            | ErrorCode::STREAM_SUBJECT_OVERLAP
            | ErrorCode::STREAM_INVALID
            | ErrorCode::STREAM_LIMITS
            | ErrorCode::STREAM_REPLICAS_NOT_SUPPORTED
            | ErrorCode::JETSTREAM_NOT_ENABLED
            | ErrorCode::JETSTREAM_NOT_ENABLED_FOR_ACCOUNT
            | ErrorCode::NO_ACCOUNT
            | ErrorCode::NOT_SUPPORTED_IN_CLUSTER_MODE
            | ErrorCode::REQUIRED_API_LEVEL
            | ErrorCode::NAME_NOT_MATCH_SUBJECT
            | ErrorCode::STREAM_MISMATCH
            | ErrorCode::STREAM_NOT_MATCH
            | ErrorCode::CONSUMER_INVALID_POLICY
            | ErrorCode::CONSUMER_CONFIG_REQUIRED
            | ErrorCode::CONSUMER_NAME_TOO_LONG
            | ErrorCode::CONSUMER_BAD_DURABLE_NAME
            | ErrorCode::CONSUMER_EMPTY_FILTER
    ) || matches!(err.code(), 400 | 403)
}

fn has_fatal_source(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = err.source();
    while let Some(src) = current {
        if let Some(js_err) = src.downcast_ref::<async_nats::jetstream::Error>()
            && is_fatal_jetstream_error(js_err)
        {
            return true;
        }
        if let Some(pub_err) = src.downcast_ref::<JsPublishError>()
            && matches!(
                pub_err.kind(),
                JsPublishErrorKind::MaxPayloadExceeded | JsPublishErrorKind::StreamNotFound
            )
        {
            return true;
        }
        if let Some(pub_err) = src.downcast_ref::<async_nats::client::PublishError>()
            && matches!(
                pub_err.kind(),
                async_nats::client::PublishErrorKind::MaxPayloadExceeded
                    | async_nats::client::PublishErrorKind::InvalidSubject
            )
        {
            return true;
        }
        if let Some(req_err) = src.downcast_ref::<async_nats::client::RequestError>()
            && matches!(
                req_err.kind(),
                async_nats::client::RequestErrorKind::InvalidSubject
                    | async_nats::client::RequestErrorKind::MaxPayloadExceeded
            )
        {
            return true;
        }
        if let Some(req_err) = src.downcast_ref::<JsRequestError>()
            && matches!(req_err.kind(), JsRequestErrorKind::InvalidSubject)
        {
            return true;
        }
        if let Some(get_err) = src.downcast_ref::<async_nats::jetstream::context::GetStreamError>()
        {
            use async_nats::jetstream::context::GetStreamErrorKind;
            match get_err.kind() {
                GetStreamErrorKind::EmptyName | GetStreamErrorKind::InvalidStreamName => {
                    return true;
                }
                GetStreamErrorKind::JetStream(js) if is_fatal_jetstream_error(&js) => return true,
                _ => {}
            }
        }
        current = src.source();
    }
    false
}

fn is_fatal_get_kv(err: &async_nats::jetstream::context::KeyValueError) -> bool {
    matches!(err.kind(), KeyValueErrorKind::InvalidStoreName) || has_fatal_source(err)
}

fn is_fatal_create_kv(err: &async_nats::jetstream::context::CreateKeyValueError) -> bool {
    matches!(
        err.kind(),
        CreateKeyValueErrorKind::InvalidStoreName
            | CreateKeyValueErrorKind::TooLongHistory
            | CreateKeyValueErrorKind::LimitMarkersNotSupported
    ) || has_fatal_source(err)
}

fn classify_update_error(
    op: &str,
    err: &kv::UpdateError,
    make_error: fn(String) -> CanoError,
) -> Failure {
    let fatal = matches!(err.kind(), UpdateErrorKind::InvalidKey) || has_fatal_source(err);
    Failure {
        error: make_error(format!("nats {op}: {err}")),
        transient: !fatal,
    }
}

fn classify_entry_error(
    op: &str,
    err: &kv::EntryError,
    make_error: fn(String) -> CanoError,
) -> Failure {
    let fatal = matches!(err.kind(), EntryErrorKind::InvalidKey) || has_fatal_source(err);
    Failure {
        error: make_error(format!("nats {op}: {err}")),
        transient: !fatal,
    }
}

fn classify_purge_error(op: &str, err: &async_nats::error::Error<PurgeErrorKind>) -> Failure {
    let fatal = match err.kind() {
        PurgeErrorKind::JetStream(js) => is_fatal_jetstream_error(&js),
        PurgeErrorKind::TimedOut => false,
        PurgeErrorKind::Request => has_fatal_source(err),
    };
    Failure {
        error: CanoError::checkpoint_store(format!("nats {op}: {err}")),
        transient: !fatal,
    }
}

fn classify_consumer_error(op: &str, err: &async_nats::error::Error<ConsumerErrorKind>) -> Failure {
    let fatal = match err.kind() {
        ConsumerErrorKind::InvalidConsumerType | ConsumerErrorKind::InvalidName => true,
        ConsumerErrorKind::JetStream(js) => is_fatal_jetstream_error(&js),
        ConsumerErrorKind::TimedOut => false,
        ConsumerErrorKind::Request | ConsumerErrorKind::Other => has_fatal_source(err),
    };
    Failure {
        error: CanoError::checkpoint_store(format!("nats {op}: {err}")),
        transient: !fatal,
    }
}

fn classify_consumer_info_error(
    op: &str,
    err: &async_nats::jetstream::context::ConsumerInfoError,
) -> Failure {
    let fatal = match err.kind() {
        ConsumerInfoErrorKind::InvalidName | ConsumerInfoErrorKind::StreamNotFound => true,
        ConsumerInfoErrorKind::JetStream(js) => is_fatal_jetstream_error(&js),
        ConsumerInfoErrorKind::Offline
        | ConsumerInfoErrorKind::NotFound
        | ConsumerInfoErrorKind::TimedOut
        | ConsumerInfoErrorKind::NoResponders => false,
        ConsumerInfoErrorKind::Request => has_fatal_source(err),
    };
    Failure {
        error: CanoError::checkpoint_store(format!("nats {op}: {err}")),
        transient: !fatal,
    }
}

fn classify_ordered_error(op: &str, err: &async_nats::error::Error<OrderedErrorKind>) -> Failure {
    let fatal = matches!(err.kind(), OrderedErrorKind::PullBasedConsumer) || has_fatal_source(err);
    Failure {
        error: CanoError::checkpoint_store(format!("nats {op}: {err}")),
        transient: !fatal,
    }
}

async fn bounded_with<T>(
    what: &str,
    limit: Duration,
    make_error: fn(String) -> CanoError,
    fut: impl Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(Failure::transient(make_error(format!(
            "{what}: timed out after {limit:?}"
        )))),
    }
}

/// [`retry::bounded`] with the scheduler store's error for an elapsed limit.
#[cfg(feature = "scheduler")]
async fn scheduler_bounded<T>(
    what: &str,
    limit: Duration,
    fut: impl Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    bounded_with(what, limit, CanoError::scheduler_store::<String>, fut).await
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

    #[test]
    fn non_zero_bucket_max_age_is_rejected() {
        let err = NatsStore::builder()
            .with_checkpoint_bucket_config(kv::Config {
                max_age: Duration::from_secs(60),
                ..Default::default()
            })
            .validate()
            .unwrap_err();
        assert_eq!(err.category(), "configuration");
        assert!(err.to_string().contains("max_age"), "{err}");

        #[cfg(feature = "scheduler")]
        {
            let err = NatsStore::builder()
                .with_scheduler_bucket_config(kv::Config {
                    max_age: Duration::from_secs(60),
                    ..Default::default()
                })
                .validate()
                .unwrap_err();
            assert_eq!(err.category(), "configuration");
            assert!(err.to_string().contains("max_age"), "{err}");
        }
    }

    #[test]
    fn permanent_jetstream_errors_are_classified_fatal() {
        use async_nats::jetstream::{Error as JsError, ErrorCode};
        for code in [
            ErrorCode::BAD_REQUEST,
            ErrorCode::STREAM_NOT_FOUND,
            ErrorCode::STREAM_MESSAGE_EXCEEDS_MAXIMUM,
            ErrorCode::JETSTREAM_NOT_ENABLED,
        ] {
            let err: JsError = serde_json::from_value(serde_json::json!({
                "code": 400,
                "err_code": code.0,
                "description": "permanent error"
            }))
            .unwrap();
            assert!(is_fatal_jetstream_error(&err), "code={code:?}");
        }
        let transient_js: JsError = serde_json::from_value(serde_json::json!({
            "code": 503,
            "err_code": ErrorCode::CLUSTER_NOT_AVAILABLE.0,
            "description": "cluster unavailable"
        }))
        .unwrap();
        assert!(!is_fatal_jetstream_error(&transient_js));
    }
}
