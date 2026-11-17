//! `NatsStore` against a real NATS server with JetStream in Docker, directly
//! and through Toxiproxy. Requires a working Docker daemon.
//! Run with `cargo test -p cano-e2e --test nats_store -- --test-threads=1`.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cano::checkpoint::{CheckpointRow, CheckpointStore};
use cano::prelude::*;
use cano::store::nats::NatsStore;
use cano::store::nats::async_nats;
use cano::testing::{assert_checkpoint_store_contract, assert_scheduler_store_contract};
use cano::{CasOutcome, SchedulerStore};
use futures_util::TryStreamExt;
use support::{Fault, NatsFixture, row};

fn proxied_options() -> async_nats::ConnectOptions {
    async_nats::ConnectOptions::new()
        .name("cano-e2e")
        .ping_interval(Duration::from_secs(2))
        .ignore_discovered_servers()
}

/// A store through the proxy; `configure` adjusts the builder.
async fn proxied_store(
    nats: &NatsFixture,
    configure: impl FnOnce(cano::store::nats::NatsStoreBuilder) -> cano::store::nats::NatsStoreBuilder,
) -> NatsStore {
    configure(NatsStore::builder())
        .connect_with_options(nats.proxied_url(), proxied_options())
        .await
        .expect("connect through the proxy")
}

async fn sorted_keys(store: &NatsStore) -> Vec<String> {
    let mut keys: Vec<String> = store
        .kv()
        .keys()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    keys.sort();
    keys
}

#[tokio::test]
async fn contract() {
    let nats = NatsFixture::start().await;
    let store = NatsStore::connect(nats.direct_url()).await.unwrap();
    assert_checkpoint_store_contract(&store, "nats").await;
}

#[tokio::test]
async fn scheduler_contract() {
    let nats = NatsFixture::start().await;
    let store = NatsStore::connect(nats.direct_url()).await.unwrap();
    assert_scheduler_store_contract(&store, "nats").await;
}

#[tokio::test]
async fn scheduler_bucket_defaults_and_keys() {
    let nats = NatsFixture::start().await;
    let store = NatsStore::connect(nats.direct_url()).await.unwrap();
    assert_eq!(store.scheduler_kv().name, "cano_scheduler");
    nats.jetstream()
        .await
        .get_stream("KV_cano_scheduler")
        .await
        .expect("the scheduler bucket's stream exists");

    let CasOutcome::Written(first) = store
        .compare_and_swap("a.b", None, b"v1".to_vec())
        .await
        .unwrap()
    else {
        panic!("create must write");
    };
    let keys: Vec<String> = store
        .scheduler_kv()
        .keys()
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(keys, ["a=2Eb"]);
    assert!(
        sorted_keys(&store).await.is_empty(),
        "the checkpoint bucket is untouched"
    );

    // A purged key can be created again, at a revision that never repeats.
    store.remove("a.b").await.unwrap();
    assert!(store.load("a.b").await.unwrap().is_none());
    let CasOutcome::Written(second) = store
        .compare_and_swap("a.b", None, b"v1".to_vec())
        .await
        .unwrap()
    else {
        panic!("create after remove must write");
    };
    assert!(second > first, "{second} must exceed {first}");

    let err = NatsStore::builder()
        .with_scheduler_bucket("cano_scheduler_other")
        .with_create_bucket(false)
        .connect(nats.direct_url())
        .await
        .unwrap_err();
    assert_eq!(err.category(), "scheduler_store", "{err}");
}

#[tokio::test]
async fn defaults_are_cano_prefixed() {
    let nats = NatsFixture::start().await;
    let store = NatsStore::connect(nats.direct_url()).await.unwrap();
    assert_eq!(store.bucket(), "cano_checkpoints");
    nats.jetstream()
        .await
        .get_stream("KV_cano_checkpoints")
        .await
        .expect("the bucket's stream exists");
}

#[tokio::test]
async fn keys_are_readable_and_run_scoped() {
    let nats = NatsFixture::start().await;
    let store = NatsStore::connect(nats.direct_url()).await.unwrap();
    store.append("order-1", row(0)).await.unwrap();
    store.append("order-1", row(1)).await.unwrap();
    store.append("order.2", row(0)).await.unwrap();
    assert_eq!(
        sorted_keys(&store).await,
        ["order-1.0", "order-1.1", "order=2E2.0"]
    );

    store.clear("order-1").await.unwrap();
    assert_eq!(sorted_keys(&store).await, ["order=2E2.0"]);
    let mut stream = nats
        .jetstream()
        .await
        .get_stream("KV_cano_checkpoints")
        .await
        .unwrap();
    assert_eq!(stream.info().await.unwrap().state.messages, 1);
}

#[tokio::test]
async fn tolerates_kv_tombstones() {
    let nats = NatsFixture::start().await;
    let store = NatsStore::connect(nats.direct_url()).await.unwrap();
    store.append("t", row(0)).await.unwrap();
    store.append("t", row(1)).await.unwrap();
    store.kv().delete("t.1").await.unwrap();
    assert_eq!(store.load_run("t").await.unwrap(), [row(0)]);
}

#[tokio::test]
async fn custom_bucket_and_no_create() {
    let nats = NatsFixture::start().await;
    let err = NatsStore::builder()
        .with_checkpoint_bucket("cano_custom")
        .with_create_bucket(false)
        .connect(nats.direct_url())
        .await
        .unwrap_err();
    assert_eq!(err.category(), "checkpoint_store", "{err}");

    NatsStore::builder()
        .with_checkpoint_bucket("cano_custom")
        .connect(nats.direct_url())
        .await
        .unwrap();
    nats.jetstream()
        .await
        .get_stream("KV_cano_custom")
        .await
        .expect("the custom bucket's stream exists");

    let store = NatsStore::builder()
        .with_checkpoint_bucket("cano_custom")
        .with_create_bucket(false)
        .connect(nats.direct_url())
        .await
        .unwrap();
    assert_eq!(store.bucket(), "cano_custom");
}

#[tokio::test]
async fn uses_a_caller_supplied_client_and_context() {
    let nats = NatsFixture::start().await;
    let client = async_nats::connect(nats.direct_url()).await.unwrap();

    let from_client = NatsStore::builder()
        .build_with_client(client.clone())
        .await
        .unwrap();
    from_client.append("client", row(0)).await.unwrap();
    assert_eq!(from_client.load_run("client").await.unwrap(), [row(0)]);

    let from_context = NatsStore::builder()
        .build_with_context(async_nats::jetstream::new(client))
        .await
        .unwrap();
    from_context.append("context", row(0)).await.unwrap();
    assert_eq!(from_context.load_run("context").await.unwrap(), [row(0)]);
}

// ----- a workflow crashes, then resumes on a new store instance -----

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum St {
    Start,
    Work,
    Done,
}

#[derive(Clone, Default)]
struct SideEffects(Arc<Mutex<Vec<String>>>);
impl Resource for SideEffects {}

#[derive(Clone)]
struct StartTask;
#[derive(Clone)]
struct WorkTask;

#[task(state = St)]
impl StartTask {
    async fn run(&self, res: &Resources) -> Result<St, CanoError> {
        res.get::<SideEffects>()?
            .0
            .lock()
            .unwrap()
            .push("Start".into());
        Ok(St::Work)
    }
}

#[task(state = St)]
impl WorkTask {
    async fn run(&self, res: &Resources) -> Result<St, CanoError> {
        res.get::<SideEffects>()?
            .0
            .lock()
            .unwrap()
            .push("Work".into());
        Ok(St::Done)
    }
}

fn build(store: Arc<dyn CheckpointStore>, id: &str, effects: &SideEffects) -> Workflow<St> {
    Workflow::new(Resources::new().insert(effects.clone()))
        .register(St::Start, StartTask)
        .register(St::Work, WorkTask)
        .add_exit_state(St::Done)
        .with_checkpoint_store(store)
        .with_workflow_id(id)
}

/// Fails every append after the first `ok_appends`: a process dying mid-run with its
/// earlier checkpoints already stored.
struct KillAfter {
    inner: NatsStore,
    ok_appends: AtomicUsize,
}

#[checkpoint_store]
impl CheckpointStore for KillAfter {
    async fn append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError> {
        if self.ok_appends.fetch_sub(1, Ordering::SeqCst) == 0 {
            return Err(CanoError::checkpoint_store("simulated crash"));
        }
        self.inner.append(workflow_id, row).await
    }
    async fn load_run(&self, id: &str) -> Result<Vec<CheckpointRow>, CanoError> {
        self.inner.load_run(id).await
    }
    async fn clear(&self, id: &str) -> Result<(), CanoError> {
        self.inner.clear(id).await
    }
}

#[tokio::test]
async fn workflow_resumes_on_a_fresh_store_instance() {
    let nats = NatsFixture::start().await;
    let id = "gen";
    let effects = SideEffects::default();

    let crashing = Arc::new(KillAfter {
        inner: NatsStore::connect(nats.direct_url()).await.unwrap(),
        ok_appends: AtomicUsize::new(2),
    });
    let err = build(crashing, id, &effects)
        .orchestrate(St::Start)
        .await
        .expect_err("generation 1 must crash before reaching Done");
    assert_eq!(err.inner().category(), "checkpoint_store");

    let fresh = NatsStore::connect(nats.direct_url()).await.unwrap();
    let resumed = build(Arc::new(fresh.clone()), id, &effects)
        .resume_from(id)
        .await
        .unwrap();
    assert_eq!(resumed, St::Done);
    assert_eq!(*effects.0.lock().unwrap(), ["Start", "Work", "Work"]);
    assert!(fresh.load_run(id).await.unwrap().is_empty());
}

// ----- hardening through Toxiproxy -----

#[tokio::test]
async fn latency() {
    let nats = NatsFixture::start().await;
    let store = proxied_store(&nats, |b| b).await;
    support::scenario_latency(&store, &nats.toxi).await;
}

#[tokio::test]
async fn blackhole_is_bounded_then_heals() {
    let nats = NatsFixture::start().await;
    let store = proxied_store(&nats, |b| b.with_operation_timeout(Duration::from_secs(2))).await;
    support::scenario_fault_is_bounded_then_heals(
        &store,
        &nats.toxi,
        Fault::Blackhole,
        Duration::from_secs(5),
    )
    .await;
}

#[tokio::test]
async fn cut_is_bounded_then_heals() {
    let nats = NatsFixture::start().await;
    let store = proxied_store(&nats, |b| b.with_operation_timeout(Duration::from_secs(2))).await;
    support::scenario_fault_is_bounded_then_heals(
        &store,
        &nats.toxi,
        Fault::Cut,
        Duration::from_secs(5),
    )
    .await;
}

#[tokio::test]
async fn resets_never_corrupt_the_log() {
    let nats = NatsFixture::start().await;
    let store = proxied_store(&nats, |b| b.with_operation_timeout(Duration::from_secs(1))).await;
    support::scenario_resets_never_corrupt_the_log(&store, &nats.toxi).await;
}

#[tokio::test]
async fn retry_absorbs_a_lost_ack() {
    let nats = NatsFixture::start().await;
    let store = proxied_store(&nats, |b| {
        b.with_operation_timeout(Duration::from_secs(1))
            .with_retry(RetryMode::fixed(40, Duration::from_millis(250)))
    })
    .await;
    support::scenario_retry_absorbs_a_lost_ack(&store, &nats.toxi).await;
}

#[tokio::test]
async fn scheduler_retry_absorbs_a_lost_ack() {
    let nats = NatsFixture::start().await;
    let store = proxied_store(&nats, |b| {
        b.with_operation_timeout(Duration::from_secs(1))
            .with_retry(RetryMode::fixed(40, Duration::from_millis(250)))
    })
    .await;
    store.load("warm").await.expect("warm the connection");
    store
        .compare_and_swap("warm", None, b"w".to_vec())
        .await
        .unwrap();

    // Create: the first attempt is stored, its acknowledgement is lost, the retry meets
    // its own bytes.
    nats.toxi.blackhole_responses().await;
    let api = nats.toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        api.heal().await;
    });
    let CasOutcome::Written(created) = store
        .compare_and_swap("lost-ack", None, b"v1".to_vec())
        .await
        .expect("a retried swap must absorb its own lost acknowledgement")
    else {
        panic!("a retried create must report Written");
    };
    healer.await.unwrap();
    let record = store.load("lost-ack").await.unwrap().unwrap();
    assert_eq!(
        (record.version, record.value.as_slice()),
        (created, &b"v1"[..])
    );

    // Swap from the current version, same fault.
    nats.toxi.blackhole_responses().await;
    let api = nats.toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        api.heal().await;
    });
    let CasOutcome::Written(swapped) = store
        .compare_and_swap("lost-ack", Some(created), b"v2".to_vec())
        .await
        .expect("a retried swap must absorb its own lost acknowledgement")
    else {
        panic!("a retried swap must report Written");
    };
    healer.await.unwrap();
    assert!(swapped > created);
    let record = store.load("lost-ack").await.unwrap().unwrap();
    assert_eq!(
        (record.version, record.value.as_slice()),
        (swapped, &b"v2"[..])
    );

    // Different bytes from a stale version still conflict.
    assert_eq!(
        store
            .compare_and_swap("lost-ack", Some(created), b"other".to_vec())
            .await
            .unwrap(),
        CasOutcome::Conflict
    );
}
