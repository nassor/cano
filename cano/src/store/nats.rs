//! [`NatsStore`]: a [`CheckpointStore`] backed by a NATS JetStream Key/Value
//! bucket.
//!
//! Compiled with the `store-nats` feature. [`async_nats`] is re-exported here so you can
//! build [`ConnectOptions`](async_nats::ConnectOptions), a client or a JetStream context
//! without adding it as a dependency.
//!
//! ## Bucket layout
//!
//! One bucket, `cano_checkpoints` by default (its stream is `KV_cano_checkpoints`), holds
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
//! By default the store creates the bucket on connect with `history: 1`. Use
//! [`with_checkpoint_bucket_config`](NatsStoreBuilder::with_checkpoint_bucket_config) to set storage,
//! replicas, placement, compression or a mirror, and
//! [`with_create_bucket(false)`](NatsStoreBuilder::with_create_bucket) when an
//! operator provisions it.
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
use cano_macros::checkpoint_store;

pub use async_nats;

/// The bucket name used unless [`with_checkpoint_bucket`](NatsStoreBuilder::with_checkpoint_bucket)
/// sets another.
pub const DEFAULT_CHECKPOINT_BUCKET: &str = "cano_checkpoints";

/// The client connection name set by [`NatsStoreBuilder::connect`].
pub const DEFAULT_CONNECTION_NAME: &str = "cano";

/// The bound on each store call unless
/// [`with_operation_timeout`](NatsStoreBuilder::with_operation_timeout) sets
/// another.
pub const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// A [`CheckpointStore`] that keeps every run in one NATS JetStream Key/Value bucket.
///
/// Cheap to clone: clones share the client. Build one with [`builder`](Self::builder), or
/// [`connect`](Self::connect) for the defaults. See the [module docs](self) for the key
/// layout and the failure semantics.
#[derive(Clone)]
pub struct NatsStore {
    client: async_nats::Client,
    kv: kv::Store,
    operation_timeout: Duration,
    retry: RetryMode,
}

impl fmt::Debug for NatsStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NatsStore")
            .field("bucket", &self.kv.name)
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

/// Settings for a [`NatsStore`]; start from [`NatsStore::builder`].
#[derive(Debug, Clone)]
pub struct NatsStoreBuilder {
    config: kv::Config,
    create_bucket: bool,
    domain: Option<String>,
    api_prefix: Option<String>,
    operation_timeout: Duration,
    retry: RetryMode,
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

    /// Whether to create the bucket on connect when it is missing. Default `true`.
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

    /// The bound on each attempt of `append`, `load_run` and `clear`, and on opening the
    /// bucket. Default [`DEFAULT_OPERATION_TIMEOUT`].
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
        validate_bucket(&self.config.bucket)?;
        let bucket = self.config.bucket.clone();
        let create_bucket = self.create_bucket;
        let config = self.config;
        let kv = retry::bounded("nats open bucket", self.operation_timeout, async {
            let get_err = match context.get_key_value(bucket.as_str()).await {
                Ok(kv) => return Ok(kv),
                Err(e) => e,
            };
            if !create_bucket {
                return Err(Failure::fatal(CanoError::checkpoint_store(format!(
                    "nats bucket {bucket:?} unavailable and bucket creation is disabled: {get_err}"
                ))));
            }
            context
                .create_key_value(config)
                .await
                .map_err(|create_err| {
                    Failure::fatal(CanoError::checkpoint_store(format!(
                        "nats open bucket {bucket:?}: {get_err}; create: {create_err}"
                    )))
                })
        })
        .await
        .map_err(|failure| failure.error)?;
        Ok(NatsStore {
            client: context.client(),
            kv,
            operation_timeout: self.operation_timeout,
            retry: self.retry,
        })
    }

    fn validate(&self) -> Result<(), CanoError> {
        validate_bucket(&self.config.bucket)?;
        if self.domain.is_some() && self.api_prefix.is_some() {
            return Err(CanoError::configuration(
                "set either a JetStream domain or an API prefix, not both",
            ));
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

fn transient(op: &str, e: impl fmt::Display) -> Failure {
    Failure::transient(CanoError::checkpoint_store(format!("nats {op}: {e}")))
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
