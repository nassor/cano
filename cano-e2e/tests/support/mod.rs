//! Docker fixtures and fault scenarios shared by the checkpoint-store suites.
//!
//! Each fixture starts its server on a private network plus a Toxiproxy container in front
//! of it, reached by container name. Tests talk to the server either directly or through
//! the proxy, whose faults ([`ToxiApi`]) the hardening scenarios drive.
#![allow(dead_code)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cano::CheckpointStore;
use cano::checkpoint::CheckpointRow;
use cano::prelude::*;
use cano::scheduler::{CasOutcome, SchedulerStore};
use cano::store::nats::async_nats;
use serde_json::json;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio_postgres::{Client, NoTls};

/// Unique suffix so concurrently-running tests don't collide on container / network names.
pub fn unique(prefix: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

// ---------------------------------------------------------------------------
// Toxiproxy
// ---------------------------------------------------------------------------

/// A Toxiproxy container with one proxy, `upstream`, listening on container port 20001.
pub struct Toxiproxy {
    _container: ContainerAsync<GenericImage>,
    /// Host port of the proxy.
    pub port: u16,
    api: ToxiApi,
}

/// A cloneable handle on the Toxiproxy HTTP API.
#[derive(Clone)]
pub struct ToxiApi {
    base: String,
    http: reqwest::Client,
}

impl std::ops::Deref for Toxiproxy {
    type Target = ToxiApi;

    fn deref(&self) -> &ToxiApi {
        &self.api
    }
}

impl Toxiproxy {
    /// Start Toxiproxy on `network` and proxy container port 20001 to `upstream`
    /// (`container-name:port`).
    pub async fn start(network: &str, upstream: &str) -> Self {
        let container = GenericImage::new("ghcr.io/shopify/toxiproxy", "2.9.0")
            .with_exposed_port(8474.tcp())
            .with_exposed_port(20001.tcp())
            // Toxiproxy 2.x logs JSON on stdout; this matches inside its `message` field.
            .with_wait_for(WaitFor::message_on_stdout("Starting Toxiproxy HTTP server"))
            .with_network(network)
            .with_container_name(unique("cano-e2e-toxi"))
            .start()
            .await
            .expect("start toxiproxy container");
        let api_port = container
            .get_host_port_ipv4(8474)
            .await
            .expect("toxiproxy api port");
        let port = container
            .get_host_port_ipv4(20001)
            .await
            .expect("toxiproxy proxy port");
        let api = ToxiApi {
            base: format!("http://127.0.0.1:{api_port}"),
            http: reqwest::Client::new(),
        };
        api.post(
            "proxies",
            json!({
                "name": "upstream",
                "listen": "0.0.0.0:20001",
                "upstream": upstream,
                "enabled": true,
            }),
        )
        .await;
        Self {
            _container: container,
            port,
            api,
        }
    }

    /// A handle that outlives a borrow of the fixture, e.g. inside `tokio::spawn`.
    pub fn api(&self) -> ToxiApi {
        self.api.clone()
    }
}

impl ToxiApi {
    async fn post(&self, path: &str, body: serde_json::Value) {
        let url = format!("{}/{path}", self.base);
        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("toxiproxy POST {path}: {e}"));
        let status = response.status();
        assert!(
            status.is_success(),
            "toxiproxy POST {path}: {status}: {}",
            response.text().await.unwrap_or_default()
        );
    }

    async fn toxic(&self, name: &str, kind: &str, stream: &str, attributes: serde_json::Value) {
        self.post(
            "proxies/upstream/toxics",
            json!({
                "name": name,
                "type": kind,
                "stream": stream,
                "toxicity": 1.0,
                "attributes": attributes,
            }),
        )
        .await;
    }

    /// Delay every server-to-client chunk by `ms`.
    pub async fn latency(&self, ms: u64) {
        self.toxic(
            "latency",
            "latency",
            "downstream",
            json!({ "latency": ms, "jitter": 0 }),
        )
        .await;
    }

    /// Drop all data both ways without closing connections.
    pub async fn blackhole(&self) {
        self.toxic(
            "blackhole_up",
            "timeout",
            "upstream",
            json!({ "timeout": 0 }),
        )
        .await;
        self.toxic(
            "blackhole_down",
            "timeout",
            "downstream",
            json!({ "timeout": 0 }),
        )
        .await;
    }

    /// Drop server-to-client data only: requests arrive, replies are lost.
    pub async fn blackhole_responses(&self) {
        self.toxic(
            "blackhole_down",
            "timeout",
            "downstream",
            json!({ "timeout": 0 }),
        )
        .await;
    }

    /// Reset every connection with a TCP RST.
    pub async fn reset_peer(&self) {
        self.toxic("reset", "reset_peer", "upstream", json!({ "timeout": 0 }))
            .await;
    }

    /// Disable the proxy: open connections close, new ones are refused.
    pub async fn cut(&self) {
        self.post("proxies/upstream", json!({ "enabled": false }))
            .await;
    }

    /// Re-enable the proxy and remove every toxic.
    pub async fn heal(&self) {
        self.post("reset", json!({})).await;
    }
}

// ---------------------------------------------------------------------------
// Postgres
// ---------------------------------------------------------------------------

/// A Postgres container plus a Toxiproxy in front of it.
pub struct PgFixture {
    _pg: ContainerAsync<GenericImage>,
    pub toxi: Toxiproxy,
    port: u16,
}

impl PgFixture {
    pub async fn start() -> Self {
        let network = unique("cano-e2e-net");
        let name = unique("cano-e2e-pg");
        let pg = GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_cmd(["-c", "fsync=off"])
            .with_network(&network)
            .with_container_name(&name)
            .start()
            .await
            .expect("start postgres container");
        let port = pg
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        // The entrypoint's init server logs the ready line too and then stops; poll until the
        // real server answers.
        let dsn = pg_dsn(port);
        eventually(Duration::from_secs(60), || async {
            let (client, connection) = tokio_postgres::connect(&dsn, NoTls).await?;
            tokio::spawn(connection);
            client.simple_query("SELECT 1").await?;
            Ok::<_, tokio_postgres::Error>(())
        })
        .await;
        let toxi = Toxiproxy::start(&network, &format!("{name}:5432")).await;
        Self {
            _pg: pg,
            toxi,
            port,
        }
    }

    pub fn direct_dsn(&self) -> String {
        pg_dsn(self.port)
    }

    pub fn proxied_dsn(&self) -> String {
        pg_dsn(self.toxi.port)
    }

    /// A plain client on the direct port.
    pub async fn admin(&self) -> Client {
        let (client, connection) = tokio_postgres::connect(&self.direct_dsn(), NoTls)
            .await
            .expect("connect to postgres");
        tokio::spawn(connection);
        client
    }
}

fn pg_dsn(port: u16) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres")
}

// ---------------------------------------------------------------------------
// NATS
// ---------------------------------------------------------------------------

/// A NATS server with JetStream plus a Toxiproxy in front of it.
pub struct NatsFixture {
    _nats: ContainerAsync<GenericImage>,
    pub toxi: Toxiproxy,
    port: u16,
}

impl NatsFixture {
    pub async fn start() -> Self {
        let network = unique("cano-e2e-net");
        let name = unique("cano-e2e-nats");
        let nats = GenericImage::new("nats", "2.12-alpine")
            .with_exposed_port(4222.tcp())
            .with_cmd(["-js", "-sd", "/tmp/nats"])
            .with_network(&network)
            .with_container_name(&name)
            .start()
            .await
            .expect("start nats container");
        let port = nats.get_host_port_ipv4(4222).await.expect("nats host port");
        // JetStream answers API requests only once it is up; until then they time out.
        let url = nats_url(port);
        eventually(Duration::from_secs(60), || async {
            let client = async_nats::connect(&url).await.map_err(|e| e.to_string())?;
            match async_nats::jetstream::new(client)
                .get_stream("CANO_PROBE")
                .await
            {
                Err(e) if e.to_string().contains("timed out") => Err(e.to_string()),
                _ => Ok(()),
            }
        })
        .await;
        let toxi = Toxiproxy::start(&network, &format!("{name}:4222")).await;
        Self {
            _nats: nats,
            toxi,
            port,
        }
    }

    pub fn direct_url(&self) -> String {
        nats_url(self.port)
    }

    pub fn proxied_url(&self) -> String {
        nats_url(self.toxi.port)
    }

    /// A plain JetStream context on the direct port.
    pub async fn jetstream(&self) -> async_nats::jetstream::Context {
        let client = async_nats::connect(self.direct_url())
            .await
            .expect("connect to nats");
        async_nats::jetstream::new(client)
    }
}

fn nats_url(port: u16) -> String {
    format!("nats://127.0.0.1:{port}")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A distinct, cursor-carrying row per sequence.
pub fn row(seq: u64) -> CheckpointRow {
    CheckpointRow::new(seq, format!("S{seq}"), "t").with_cursor(seq.to_le_bytes().to_vec())
}

pub fn rows(sequences: std::ops::Range<u64>) -> Vec<CheckpointRow> {
    sequences.map(row).collect()
}

/// Poll `f` every 200 ms until it returns `Ok`; panic with the last error after `limit`.
pub async fn eventually<T, E, F, Fut>(limit: Duration, mut f: F) -> T
where
    E: std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let deadline = Instant::now() + limit;
    loop {
        match f().await {
            Ok(value) => return value,
            Err(e) if Instant::now() >= deadline => {
                panic!("still failing after {limit:?}: {e}")
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Whether `result` is `Ok` or a checkpoint conflict.
pub fn ok_or_conflict(result: &Result<(), cano::CanoError>) -> bool {
    match result {
        Ok(()) => true,
        Err(e) => e.to_string().contains("conflict"),
    }
}

fn assert_store_error(result: Result<impl std::fmt::Debug, cano::CanoError>, what: &str) -> String {
    let err = match result {
        Ok(value) => panic!("{what} must fail during the fault, got Ok({value:?})"),
        Err(err) => err,
    };
    assert_eq!(err.category(), "checkpoint_store", "{what}: {err}");
    err.to_string()
}

// ---------------------------------------------------------------------------
// Hardening scenarios, shared by both backends
// ---------------------------------------------------------------------------

/// Every call still works, only slower, under 150 ms of reply latency.
pub async fn scenario_latency(store: &dyn CheckpointStore, toxi: &Toxiproxy) {
    let id = "latency";
    toxi.latency(150).await;
    for row in rows(0..5) {
        store.append(id, row).await.expect("append under latency");
    }
    assert_eq!(store.load_run(id).await.unwrap(), rows(0..5));
    store.clear(id).await.unwrap();
    assert!(store.load_run(id).await.unwrap().is_empty());
    toxi.heal().await;
}

#[derive(Debug, Clone, Copy)]
pub enum Fault {
    /// Data is dropped silently; connections stay open.
    Blackhole,
    /// The proxy is disabled; connections close.
    Cut,
}

/// Under `fault`, calls fail with a `checkpoint_store` error within `bound`; after the
/// fault heals, the store recovers without a restart.
pub async fn scenario_fault_is_bounded_then_heals(
    store: &dyn CheckpointStore,
    toxi: &Toxiproxy,
    fault: Fault,
    bound: Duration,
) {
    let id = "fault";
    store.append(id, row(0)).await.expect("append before fault");
    match fault {
        Fault::Blackhole => toxi.blackhole().await,
        Fault::Cut => toxi.cut().await,
    }

    let started = Instant::now();
    let text = assert_store_error(store.append(id, row(1)).await, "append");
    assert!(
        started.elapsed() < bound,
        "append took {:?} under {fault:?}",
        started.elapsed()
    );
    if matches!(fault, Fault::Blackhole) {
        assert!(text.contains("timed out"), "{text}");
    }
    let started = Instant::now();
    assert_store_error(store.load_run(id).await, "load_run");
    assert!(
        started.elapsed() < bound,
        "load_run took {:?} under {fault:?}",
        started.elapsed()
    );

    toxi.heal().await;
    eventually(Duration::from_secs(30), || store.load_run(id)).await;
    let result = store.append(id, row(1)).await;
    assert!(ok_or_conflict(&result), "append after heal: {result:?}");
    assert_eq!(store.load_run(id).await.unwrap(), rows(0..2));
}

/// Connection resets in the middle of a run of appends never lose an acknowledged row and
/// never store a duplicated or altered one. Build `store` with a 1 s operation timeout.
pub async fn scenario_resets_never_corrupt_the_log(store: &dyn CheckpointStore, toxi: &Toxiproxy) {
    let id = "resets";
    let mut acknowledged = Vec::new();
    let mut failed_during_fault = 0;
    for seq in 0..60u64 {
        if seq == 20 {
            toxi.reset_peer().await;
        }
        if seq == 35 {
            toxi.heal().await;
        }
        match store.append(id, row(seq)).await {
            Ok(()) => acknowledged.push(seq),
            Err(_) if (20..35).contains(&seq) => failed_during_fault += 1,
            Err(_) => {}
        }
    }
    assert!(
        failed_during_fault > 0,
        "no append failed while connections were being reset"
    );

    let loaded = eventually(Duration::from_secs(30), || store.load_run(id)).await;
    for pair in loaded.windows(2) {
        assert!(
            pair[0].sequence < pair[1].sequence,
            "rows out of order or duplicated: {} then {}",
            pair[0].sequence,
            pair[1].sequence
        );
    }
    for stored in &loaded {
        assert_eq!(*stored, row(stored.sequence), "row altered");
    }
    for seq in &acknowledged {
        assert!(
            loaded.iter().any(|r| r.sequence == *seq),
            "acknowledged sequence {seq} is missing"
        );
    }

    // A write that failed may still land (async-nats flushes a buffered publish once it
    // reconnects), so re-append every unacknowledged sequence and accept a conflict.
    for seq in (0..60u64).filter(|s| !acknowledged.contains(s)) {
        let result = store.append(id, row(seq)).await;
        assert!(ok_or_conflict(&result), "re-append {seq}: {result:?}");
    }
    assert_eq!(store.load_run(id).await.unwrap(), rows(0..60));
}

/// With retries on, a write whose acknowledgement is lost is reported `Ok` exactly once,
/// and a different row at that sequence still conflicts. Build `store` with a 1 s
/// operation timeout and `RetryMode::fixed(40, 250 ms)`.
pub async fn scenario_retry_absorbs_a_lost_ack(store: &dyn CheckpointStore, toxi: &Toxiproxy) {
    store
        .append("warm", row(0))
        .await
        .expect("warm the connection");
    toxi.blackhole_responses().await;
    let api = toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        api.heal().await;
    });

    store
        .append("lost-ack", row(0))
        .await
        .expect("a retried append must absorb its own lost acknowledgement");
    healer.await.unwrap();
    assert_eq!(store.load_run("lost-ack").await.unwrap(), [row(0)]);

    let result = store
        .append("lost-ack", CheckpointRow::new(0, "other", "t"))
        .await;
    assert!(
        matches!(&result, Err(e) if e.to_string().contains("conflict")),
        "a different row at an existing sequence must conflict: {result:?}"
    );
}

/// Concurrent identical seq-0 `append` race under `blackhole` / `blackhole_responses`
/// with `with_retry`: a caller's retried append must NOT absorb another writer's identical
/// seq-0 row, while absorbing its own lost ACK via its 128-bit `write_id`.
pub async fn scenario_identical_seq0_append_race(
    store_a: Arc<dyn CheckpointStore>,
    store_b: Arc<dyn CheckpointStore>,
    toxi: &Toxiproxy,
) {
    store_a.append("warm-a", row(0)).await.expect("warm A");
    store_b.append("warm-b", row(0)).await.expect("warm B");

    let identical = CheckpointRow::new(0, "Identical", "same-task");

    // 1. A is blackholed before reaching the server; B commits an identical seq-0 row;
    //    after healing, A's retry must see a different `write_id` and return conflict.
    toxi.blackhole().await;
    let a_store = Arc::clone(&store_a);
    let a_row = identical.clone();
    let a_task = tokio::spawn(async move { a_store.append("seq0-race", a_row).await });

    // Let A's initial attempt time out (800 ms operation_timeout) before B commits.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    store_b
        .append("seq0-race", identical.clone())
        .await
        .expect("B commits identical seq-0 directly");
    toxi.heal().await;

    let a_err = a_task
        .await
        .unwrap()
        .expect_err("A must conflict rather than absorb B's identical seq-0 row");
    assert_eq!(a_err.category(), "checkpoint_store", "{a_err}");
    assert!(a_err.to_string().contains("conflict"), "{a_err}");
    assert_eq!(
        store_b.load_run("seq0-race").await.unwrap(),
        std::slice::from_ref(&identical)
    );

    // 2. When A's own seq-0 write actually lands and its ACK is blackholed, A's retry
    //    matches its own `write_id` and returns Ok, while B subsequently conflicts.
    toxi.blackhole_responses().await;
    let api = toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        api.heal().await;
    });
    store_a
        .append("seq0-own-ack", identical.clone())
        .await
        .expect("A must absorb its own lost ACK via write_id");
    healer.await.unwrap();

    let b_err = store_b
        .append("seq0-own-ack", identical.clone())
        .await
        .expect_err("B must conflict with A's stored row");
    assert_eq!(b_err.category(), "checkpoint_store", "{b_err}");
    assert!(b_err.to_string().contains("conflict"), "{b_err}");
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum E2eState {
    Run,
    Done,
}

#[derive(Clone)]
struct SagaGateTask {
    started: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Notify>,
    compensated: Arc<AtomicUsize>,
}

#[cano::saga::task(state = E2eState)]
impl SagaGateTask {
    type Output = ();

    async fn run(&self) -> Result<(E2eState, ()), CanoError> {
        self.started.notify_one();
        self.gate.notified().await;
        Ok((E2eState::Done, ()))
    }

    async fn compensate(&self, _output: ()) -> Result<(), CanoError> {
        self.compensated.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone)]
struct CountHoldTask {
    runs: Arc<AtomicUsize>,
    started: Option<Arc<tokio::sync::Notify>>,
    gate: Option<Arc<tokio::sync::Notify>>,
    hold: Duration,
}

#[task(state = E2eState)]
impl CountHoldTask {
    async fn run(&self) -> Result<E2eState, CanoError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if let Some(started) = &self.started {
            started.notify_one();
        }
        if let Some(gate) = &self.gate {
            gate.notified().await;
        } else if !self.hold.is_zero() {
            tokio::time::sleep(self.hold).await;
        }
        Ok(E2eState::Done)
    }
}

/// Active-lease network partition -> holder lease-loss cancel -> failover -> healed
/// zombie write fencing over Toxiproxy.
pub async fn scenario_scheduler_partition_failover_and_zombie_fencing(
    store_a: Arc<dyn SchedulerStore>,
    store_b: Arc<dyn SchedulerStore>,
    toxi: &Toxiproxy,
) {
    let a_started = Arc::new(tokio::sync::Notify::new());
    let a_gate = Arc::new(tokio::sync::Notify::new());
    let a_compensated = Arc::new(AtomicUsize::new(0));
    let wf_a = Workflow::bare()
        .register(
            E2eState::Run,
            SagaGateTask {
                started: Arc::clone(&a_started),
                gate: Arc::clone(&a_gate),
                compensated: Arc::clone(&a_compensated),
            },
        )
        .add_exit_state(E2eState::Done);

    let b_runs = Arc::new(AtomicUsize::new(0));
    let b_started = Arc::new(tokio::sync::Notify::new());
    let wf_b = Workflow::bare()
        .register(
            E2eState::Run,
            CountHoldTask {
                runs: Arc::clone(&b_runs),
                started: Some(Arc::clone(&b_started)),
                gate: None,
                hold: Duration::from_millis(300),
            },
        )
        .add_exit_state(E2eState::Done);

    let ttl = Duration::from_secs(2);
    let node_a = Scheduler::new()
        .manual("failover-job", wf_a, E2eState::Run)
        .with_store(Arc::clone(&store_a))
        .with_instance_id("node-a")
        .with_lease_ttl(ttl)
        .start()
        .await
        .unwrap();
    let node_b = Scheduler::new()
        .manual("failover-job", wf_b, E2eState::Run)
        .with_store(Arc::clone(&store_b))
        .with_instance_id("node-b")
        .with_lease_ttl(ttl)
        .start()
        .await
        .unwrap();

    node_a.trigger("failover-job").await.unwrap();
    a_started.notified().await;
    let a_version = store_b
        .load("failover-job")
        .await
        .unwrap()
        .expect("node-a lease must be present")
        .version;

    // Partition node-a while it holds the active lease.
    toxi.blackhole().await;

    // Once node-a's 2 s lease expires, node-b reaps it and takes ownership.
    eventually(Duration::from_secs(12), || node_b.trigger("failover-job")).await;
    b_started.notified().await;
    let b_info = node_b.wait_idle("failover-job").await.unwrap();
    assert_eq!(b_info.status, Status::Completed);
    assert_eq!(b_runs.load(Ordering::SeqCst), 1);

    let after_b = store_b
        .load("failover-job")
        .await
        .unwrap()
        .expect("node-b record must exist");
    assert!(after_b.version > a_version);

    // Heal node-a's partition, allow its stalled saga task to finish, and verify
    // node-a's lease loss drained compensation and never overwrote node-b's record.
    toxi.heal().await;
    let stale_cas = eventually(Duration::from_secs(10), || {
        store_a.compare_and_swap("failover-job", Some(a_version), b"zombie".to_vec())
    })
    .await;
    assert_eq!(stale_cas, CasOutcome::Conflict);

    a_gate.notify_one();
    let a_info = tokio::time::timeout(Duration::from_secs(10), node_a.wait_idle("failover-job"))
        .await
        .expect("node-a settles promptly after heal")
        .unwrap();
    assert_ne!(a_info.status, Status::Running);
    assert_eq!(a_compensated.load(Ordering::SeqCst), 1);

    let final_rec = store_b.load("failover-job").await.unwrap().unwrap();
    assert_eq!(
        final_rec, after_b,
        "healed zombie node-a must not modify node-b's completed record"
    );

    node_a.stop().await.unwrap();
    node_b.stop().await.unwrap();
}

/// `blackhole_responses` lost-ACK on `claim`, `renew`, and `release` with `with_retry`
/// preserves `every` and `cron` ticks and completes without leaking leases or false streaks.
pub async fn scenario_scheduler_lost_ack_preserves_ticks(
    store: Arc<dyn SchedulerStore>,
    toxi: &Toxiproxy,
) {
    store.load("warm-sched").await.expect("warm connection");

    let every_runs = Arc::new(AtomicUsize::new(0));
    let cron_runs = Arc::new(AtomicUsize::new(0));
    let wf_every = Workflow::bare()
        .register(
            E2eState::Run,
            CountHoldTask {
                runs: Arc::clone(&every_runs),
                started: None,
                gate: None,
                hold: Duration::from_millis(20),
            },
        )
        .add_exit_state(E2eState::Done);
    let wf_cron = Workflow::bare()
        .register(
            E2eState::Run,
            CountHoldTask {
                runs: Arc::clone(&cron_runs),
                started: None,
                gate: None,
                hold: Duration::from_millis(20),
            },
        )
        .add_exit_state(E2eState::Done);

    // 1. Drop ACKs during startup `claim` for both `every` and `cron`.
    toxi.blackhole_responses().await;
    let api = toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        api.heal().await;
    });

    let sched = Scheduler::new()
        .every(
            "every-lost-ack",
            wf_every,
            E2eState::Run,
            Duration::from_millis(450),
        )
        .cron("cron-lost-ack", wf_cron, E2eState::Run, "* * * * * *")
        .with_store(Arc::clone(&store))
        .with_instance_id("lost-ack-node")
        .with_lease_ttl(Duration::from_secs(6))
        .start()
        .await
        .unwrap();

    healer.await.unwrap();
    eventually(Duration::from_secs(6), || {
        let e = every_runs.load(Ordering::SeqCst);
        let c = cron_runs.load(Ordering::SeqCst);
        async move {
            if e >= 2 && c >= 2 {
                Ok(())
            } else {
                Err(format!("every={e}, cron={c}"))
            }
        }
    })
    .await;
    sched.stop().await.unwrap();

    assert_eq!(sched.status("every-lost-ack").unwrap().failure_streak, 0);
    assert_eq!(sched.status("cron-lost-ack").unwrap().failure_streak, 0);

    // 2. Drop ACKs during `renew` and `release` of a manual flow.
    let manual_runs = Arc::new(AtomicUsize::new(0));
    let manual_started = Arc::new(tokio::sync::Notify::new());
    let manual_gate = Arc::new(tokio::sync::Notify::new());
    let wf_manual = Workflow::bare()
        .register(
            E2eState::Run,
            CountHoldTask {
                runs: Arc::clone(&manual_runs),
                started: Some(Arc::clone(&manual_started)),
                gate: Some(Arc::clone(&manual_gate)),
                hold: Duration::ZERO,
            },
        )
        .add_exit_state(E2eState::Done);

    let sched2 = Scheduler::new()
        .manual("manual-lost-ack", wf_manual, E2eState::Run)
        .with_store(Arc::clone(&store))
        .with_instance_id("lost-ack-node-2")
        .with_lease_ttl(Duration::from_secs(3))
        .start()
        .await
        .unwrap();

    sched2.trigger("manual-lost-ack").await.unwrap();
    manual_started.notified().await;

    // Drop ACKs across the `ttl / 3 = 1s` renewal boundary; retries must keep the lease held.
    toxi.blackhole_responses().await;
    let api = toxi.api();
    let renew_healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1300)).await;
        api.heal().await;
    });
    renew_healer.await.unwrap();
    assert_eq!(
        sched2.status("manual-lost-ack").unwrap().status,
        Status::Running,
        "lost renewal ACK must not cancel the running flow"
    );

    // Drop ACKs while `release` commits the outcome; retries must complete cleanly.
    toxi.blackhole_responses().await;
    let api = toxi.api();
    let release_healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        api.heal().await;
    });
    manual_gate.notify_one();
    let info = tokio::time::timeout(Duration::from_secs(6), sched2.wait_idle("manual-lost-ack"))
        .await
        .expect("release must complete")
        .unwrap();
    release_healer.await.unwrap();
    assert_eq!(info.status, Status::Completed);
    assert_eq!(info.failure_streak, 0);
    assert_eq!(manual_runs.load(Ordering::SeqCst), 1);

    sched2.stop().await.unwrap();
}
