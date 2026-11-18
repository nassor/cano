//! `PostgresStore` against a real Postgres in Docker, directly and through
//! Toxiproxy. Requires a working Docker daemon.
//! Run with `cargo test -p cano-e2e --test postgres_store -- --test-threads=1`.

mod support;

use std::sync::Arc;
use std::time::Duration;

use cano::RetryMode;
use cano::checkpoint::CheckpointStore;
use cano::scheduler::{CasOutcome, SchedulerStore};
use cano::store::postgres::PostgresStore;
use cano::store::postgres::{deadpool_postgres, tokio_postgres};
use cano::testing::{assert_checkpoint_store_contract, assert_scheduler_store_contract};
use support::{Fault, PgFixture, row};

/// The store the fault scenarios use: short timeout, one connection.
async fn fault_store(pg: &PgFixture) -> PostgresStore {
    PostgresStore::builder()
        .with_operation_timeout(Duration::from_secs(2))
        .with_max_pool_size(1)
        .connect(&pg.proxied_dsn())
        .await
        .expect("connect through the proxy")
}

async fn exists(admin: &tokio_postgres::Client, relation: &str) -> bool {
    admin
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&relation])
        .await
        .unwrap()
        .get(0)
}

async fn application_names(admin: &tokio_postgres::Client) -> Vec<String> {
    admin
        .query(
            "SELECT application_name FROM pg_stat_activity WHERE backend_type = 'client backend'",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect()
}

#[tokio::test]
async fn contract() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::connect(&pg.direct_dsn()).await.unwrap();
    assert_checkpoint_store_contract(&store, "pg").await;
}

#[tokio::test]
async fn scheduler_contract() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::connect(&pg.direct_dsn()).await.unwrap();
    assert_scheduler_store_contract(&store, "pg").await;
}

#[tokio::test]
async fn scheduler_defaults_and_custom_table() {
    let pg = PgFixture::start().await;
    let admin = pg.admin().await;
    let _default = PostgresStore::connect(&pg.direct_dsn()).await.unwrap();
    assert!(exists(&admin, "public.cano_scheduler").await);
    assert!(exists(&admin, "public.cano_scheduler_version_seq").await);

    admin.batch_execute("CREATE SCHEMA sched").await.unwrap();
    let custom = PostgresStore::builder()
        .with_schema("sched")
        .with_scheduler_table("cano_sched_custom")
        .connect(&pg.direct_dsn())
        .await
        .unwrap();
    assert_eq!(
        custom
            .compare_and_swap("f", None, b"{}".to_vec())
            .await
            .unwrap(),
        CasOutcome::Written(
            custom
                .load("f")
                .await
                .unwrap()
                .expect("record stored")
                .version
        )
    );
    assert!(exists(&admin, "sched.cano_sched_custom").await);
    assert!(exists(&admin, "sched.cano_sched_custom_version_seq").await);
    let json: String = admin
        .query_one(
            "SELECT convert_from(record, 'UTF8')::jsonb::text FROM sched.cano_sched_custom \
             WHERE flow_id = 'f'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(json, "{}");
}

#[tokio::test]
async fn scheduler_versions_never_repeat_across_remove() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::connect(&pg.direct_dsn()).await.unwrap();
    let CasOutcome::Written(first) = store
        .compare_and_swap("v", None, b"a".to_vec())
        .await
        .unwrap()
    else {
        panic!("first write conflicted");
    };
    store.remove("v").await.unwrap();
    let CasOutcome::Written(second) = store
        .compare_and_swap("v", None, b"a".to_vec())
        .await
        .unwrap()
    else {
        panic!("re-create conflicted");
    };
    assert_ne!(first, second);
    assert_eq!(
        store
            .compare_and_swap("v", Some(first), b"b".to_vec())
            .await
            .unwrap(),
        CasOutcome::Conflict
    );
}

#[tokio::test]
async fn scheduler_swap_races_have_one_winner() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::connect(&pg.direct_dsn()).await.unwrap();
    let CasOutcome::Written(base) = store
        .compare_and_swap("race", None, b"0".to_vec())
        .await
        .unwrap()
    else {
        panic!("seed conflicted");
    };
    let outcomes = futures_util::future::join_all((0..16).map(|i| {
        let store = store.clone();
        async move {
            store
                .compare_and_swap("race", Some(base), format!("w{i}").into_bytes())
                .await
                .unwrap()
        }
    }))
    .await;
    let written = outcomes
        .iter()
        .filter(|o| matches!(o, CasOutcome::Written(_)))
        .count();
    assert_eq!(written, 1, "{outcomes:?}");
}

#[tokio::test]
async fn defaults_are_cano_prefixed() {
    let pg = PgFixture::start().await;
    let _store = PostgresStore::connect(&pg.direct_dsn()).await.unwrap();
    let admin = pg.admin().await;
    assert!(exists(&admin, "public.cano_checkpoints").await);
    let names = application_names(&admin).await;
    assert!(names.iter().any(|n| n == "cano"), "{names:?}");
}

#[tokio::test]
async fn primary_key_is_the_only_index_and_serves_every_statement() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::builder()
        .with_max_pool_size(1)
        .connect(&pg.direct_dsn())
        .await
        .unwrap();
    let admin = pg.admin().await;

    let indexes = admin
        .query(
            "SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'cano_checkpoints'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(indexes.len(), 1, "exactly one index");
    assert_eq!(indexes[0].get::<_, &str>(0), "cano_checkpoints_pkey");
    let indexdef: &str = indexes[0].get(1);
    assert!(indexdef.contains("(workflow_id, sequence)"), "{indexdef}");
    let collation: String = admin
        .query_one(
            "SELECT c.collname::text FROM pg_attribute a JOIN pg_collation c ON c.oid = a.attcollation \
             WHERE a.attrelid = 'cano_checkpoints'::regclass AND a.attname = 'workflow_id'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(collation, "C");

    admin
        .batch_execute(
            "INSERT INTO cano_checkpoints (workflow_id, sequence, state, task_id, kind, workflow_version) \
             SELECT 'seed-' || w, s, 'S', 't', 0, 0 FROM generate_series(1, 2000) w, generate_series(0, 4) s; \
             ANALYZE cano_checkpoints;",
        )
        .await
        .unwrap();

    // The planner reaches each lookup through the primary key, never a sequential scan.
    for statement in [
        "SELECT sequence, state, task_id, kind, workflow_version, output_blob FROM cano_checkpoints \
         WHERE workflow_id = 'seed-7' ORDER BY sequence",
        "SELECT state, task_id, kind, workflow_version, output_blob, write_id FROM cano_checkpoints \
         WHERE workflow_id = 'seed-7' AND sequence = 3",
        "DELETE FROM cano_checkpoints WHERE workflow_id = 'seed-7'",
    ] {
        let mut plan = String::new();
        for message in admin
            .simple_query(&format!("EXPLAIN (COSTS OFF) {statement}"))
            .await
            .unwrap()
        {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = message {
                plan.push_str(row.get(0).unwrap_or_default());
                plan.push('\n');
            }
        }
        assert!(
            plan.contains("cano_checkpoints_pkey"),
            "{statement}\n{plan}"
        );
        assert!(!plan.contains("Seq Scan"), "{statement}\n{plan}");
    }

    // And the store's own prepared statements scan the index: 100 calls, 100 index scans.
    let index_scans = || async {
        admin
            .query_one(
                "SELECT coalesce(idx_scan, 0) FROM pg_stat_user_tables WHERE relname = 'cano_checkpoints'",
                &[],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
    };
    let before = index_scans().await;
    for w in 1..=50 {
        let rows = store.load_run(&format!("seed-{w}")).await.unwrap();
        assert_eq!(rows.len(), 5);
    }
    for w in 51..=100 {
        store.clear(&format!("seed-{w}")).await.unwrap();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let grown = index_scans().await - before;
        if grown >= 100 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "index scans grew by {grown}, expected at least 100"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
async fn custom_schema_and_table() {
    let pg = PgFixture::start().await;
    let admin = pg.admin().await;
    admin.batch_execute("CREATE SCHEMA wf").await.unwrap();
    let store = PostgresStore::builder()
        .with_schema("wf")
        .with_checkpoint_table("cano_ckpt_custom")
        .connect(&pg.direct_dsn())
        .await
        .unwrap();
    store.append("custom", row(0)).await.unwrap();
    assert_eq!(store.load_run("custom").await.unwrap(), [row(0)]);
    store.clear("custom").await.unwrap();
    assert!(store.load_run("custom").await.unwrap().is_empty());
    assert!(exists(&admin, "wf.cano_ckpt_custom").await);
    assert!(!exists(&admin, "public.cano_checkpoints").await);
}

#[tokio::test]
async fn create_table_disabled_leaves_ddl_to_the_operator() {
    let pg = PgFixture::start().await;
    let builder = PostgresStore::builder().with_create_table(false);
    let store = builder
        .clone()
        .connect(&pg.direct_dsn())
        .await
        .expect("connect does no I/O without the migration");
    let err = store.append("ddl", row(0)).await.unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");

    pg.admin()
        .await
        .batch_execute(&builder.schema_sql().unwrap())
        .await
        .unwrap();
    store.append("ddl", row(0)).await.unwrap();
    assert_eq!(store.load_run("ddl").await.unwrap(), [row(0)]);
}

#[tokio::test]
async fn concurrent_first_connects_do_not_race_the_migration() {
    let pg = PgFixture::start().await;
    let dsn = pg.direct_dsn();
    let results =
        futures_util::future::join_all((0..16).map(|_| PostgresStore::connect(&dsn))).await;
    for result in results {
        result.expect("every concurrent first connect succeeds");
    }
}

#[tokio::test]
async fn uses_a_caller_supplied_pool() {
    let pg = PgFixture::start().await;
    let mut config: tokio_postgres::Config = pg.direct_dsn().parse().unwrap();
    config.application_name("my-app");
    let manager = deadpool_postgres::Manager::from_config(
        config,
        tokio_postgres::NoTls,
        deadpool_postgres::ManagerConfig::default(),
    );
    let pool = deadpool_postgres::Pool::builder(manager)
        .max_size(2)
        .build()
        .unwrap();
    let store = PostgresStore::builder()
        .build_with_pool(pool)
        .await
        .unwrap();
    store.append("pool", row(0)).await.unwrap();
    assert_eq!(store.load_run("pool").await.unwrap(), [row(0)]);
    let names = application_names(&pg.admin().await).await;
    assert!(names.iter().any(|n| n == "my-app"), "{names:?}");
    assert!(!names.iter().any(|n| n == "cano"), "{names:?}");
}

#[tokio::test]
async fn latency() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::connect(&pg.proxied_dsn()).await.unwrap();
    support::scenario_latency(&store, &pg.toxi).await;
}

#[tokio::test]
async fn blackhole_is_bounded_then_heals() {
    let pg = PgFixture::start().await;
    let store = fault_store(&pg).await;
    support::scenario_fault_is_bounded_then_heals(
        &store,
        &pg.toxi,
        Fault::Blackhole,
        Duration::from_secs(5),
    )
    .await;
}

#[tokio::test]
async fn cut_is_bounded_then_heals() {
    let pg = PgFixture::start().await;
    let store = fault_store(&pg).await;
    support::scenario_fault_is_bounded_then_heals(
        &store,
        &pg.toxi,
        Fault::Cut,
        Duration::from_secs(5),
    )
    .await;
}

#[tokio::test]
async fn resets_never_corrupt_the_log() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::builder()
        .with_operation_timeout(Duration::from_secs(1))
        .connect(&pg.proxied_dsn())
        .await
        .unwrap();
    support::scenario_resets_never_corrupt_the_log(&store, &pg.toxi).await;
}

#[tokio::test]
async fn retry_absorbs_a_lost_ack() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::builder()
        .with_operation_timeout(Duration::from_secs(1))
        .with_retry(RetryMode::fixed(40, Duration::from_millis(250)))
        .connect(&pg.proxied_dsn())
        .await
        .unwrap();
    support::scenario_retry_absorbs_a_lost_ack(&store, &pg.toxi).await;
}

#[tokio::test]
async fn scheduler_retry_absorbs_a_lost_ack() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::builder()
        .with_operation_timeout(Duration::from_secs(1))
        .with_retry(RetryMode::fixed(40, Duration::from_millis(250)))
        .connect(&pg.proxied_dsn())
        .await
        .unwrap();

    // Create: the insert lands, its reply is lost, the retried insert finds the row.
    store.load("warm").await.expect("warm the connection");
    pg.toxi.blackhole_responses().await;
    let api = pg.toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        api.heal().await;
    });
    let created = store
        .compare_and_swap("lost-ack", None, b"first".to_vec())
        .await
        .expect("a retried swap must absorb its own lost acknowledgement");
    healer.await.unwrap();
    let stored = store
        .load("lost-ack")
        .await
        .unwrap()
        .expect("record stored");
    assert_eq!(stored.value, b"first");
    assert_eq!(created, CasOutcome::Written(stored.version));

    // Update: same loss on an update; the stored bytes equal the bytes sent.
    pg.toxi.blackhole_responses().await;
    let api = pg.toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        api.heal().await;
    });
    let updated = store
        .compare_and_swap("lost-ack", Some(stored.version), b"second".to_vec())
        .await
        .expect("a retried update must absorb its own lost acknowledgement");
    healer.await.unwrap();
    let after = store
        .load("lost-ack")
        .await
        .unwrap()
        .expect("record stored");
    assert_eq!(after.value, b"second");
    assert_eq!(updated, CasOutcome::Written(after.version));
    assert_ne!(after.version, stored.version);

    // A different value from the stale version is still a conflict.
    let stale = store
        .compare_and_swap("lost-ack", Some(stored.version), b"third".to_vec())
        .await
        .unwrap();
    assert_eq!(stale, CasOutcome::Conflict);
    assert_eq!(store.load("lost-ack").await.unwrap(), Some(after));
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_instance_scheduler_cluster_coordinates_over_postgres() {
    use cano::prelude::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum TickState {
        Run,
        Done,
    }

    #[derive(Clone)]
    struct CountTask(Arc<AtomicUsize>);

    #[task(state = TickState)]
    impl CountTask {
        async fn run(&self) -> Result<TickState, CanoError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok(TickState::Done)
        }
    }

    let pg = PgFixture::start().await;
    let store_a = Arc::new(PostgresStore::connect(&pg.direct_dsn()).await.unwrap());
    let store_b = Arc::new(PostgresStore::connect(&pg.direct_dsn()).await.unwrap());

    let runs = Arc::new(AtomicUsize::new(0));
    let wf = Workflow::bare()
        .register(TickState::Run, CountTask(Arc::clone(&runs)))
        .add_exit_state(TickState::Done);

    let node_a = Scheduler::new()
        .manual("cluster-job", wf.clone(), TickState::Run)
        .with_store(store_a)
        .with_instance_id("pg-node-a")
        .with_lease_ttl(Duration::from_secs(5))
        .start()
        .await
        .unwrap();
    let node_b = Scheduler::new()
        .manual("cluster-job", wf, TickState::Run)
        .with_store(store_b)
        .with_instance_id("pg-node-b")
        .with_lease_ttl(Duration::from_secs(5))
        .start()
        .await
        .unwrap();

    node_a.trigger("cluster-job").await.unwrap();
    let err = node_b
        .trigger("cluster-job")
        .await
        .expect_err("node_b must be blocked while node_a holds the Postgres lease");
    assert!(err.to_string().contains("pg-node-a"), "{err}");

    node_a.wait_idle("cluster-job").await.unwrap();
    node_b
        .trigger("cluster-job")
        .await
        .expect("node_b can claim once node_a releases its lease");
    node_b.wait_idle("cluster-job").await.unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2);

    node_a.stop().await.unwrap();
    node_b.stop().await.unwrap();
}

#[tokio::test]
async fn identical_seq0_append_race_with_lost_ack() {
    let pg = PgFixture::start().await;
    let store_a = Arc::new(
        PostgresStore::builder()
            .with_operation_timeout(Duration::from_millis(800))
            .with_retry(RetryMode::fixed(20, Duration::from_millis(150)))
            .connect(&pg.proxied_dsn())
            .await
            .unwrap(),
    );
    let store_b = Arc::new(
        PostgresStore::builder()
            .with_operation_timeout(Duration::from_secs(2))
            .with_retry(RetryMode::fixed(4, Duration::from_millis(100)))
            .connect(&pg.direct_dsn())
            .await
            .unwrap(),
    );
    support::scenario_identical_seq0_append_race(store_a, store_b, &pg.toxi).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduler_partition_failover_and_zombie_fencing() {
    let pg = PgFixture::start().await;
    let store_a = Arc::new(
        PostgresStore::builder()
            .with_operation_timeout(Duration::from_millis(500))
            .connect(&pg.proxied_dsn())
            .await
            .unwrap(),
    );
    let store_b = Arc::new(
        PostgresStore::builder()
            .with_operation_timeout(Duration::from_secs(1))
            .connect(&pg.direct_dsn())
            .await
            .unwrap(),
    );
    support::scenario_scheduler_partition_failover_and_zombie_fencing(store_a, store_b, &pg.toxi)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduler_lost_ack_on_claim_renew_release_preserves_ticks() {
    let pg = PgFixture::start().await;
    let store = Arc::new(
        PostgresStore::builder()
            .with_operation_timeout(Duration::from_millis(800))
            .with_retry(RetryMode::fixed(20, Duration::from_millis(150)))
            .connect(&pg.proxied_dsn())
            .await
            .unwrap(),
    );
    support::scenario_scheduler_lost_ack_preserves_ticks(store, &pg.toxi).await;
}

#[tokio::test]
async fn reset_peer_recovers_idle_pool_connections() {
    let pg = PgFixture::start().await;
    let store = PostgresStore::builder()
        .with_max_pool_size(2)
        .with_operation_timeout(Duration::from_secs(2))
        .with_retry(RetryMode::fixed(5, Duration::from_millis(100)))
        .connect(&pg.proxied_dsn())
        .await
        .unwrap();

    // Warm both pool slots so idle connections stay open in deadpool.
    let (a, b) = tokio::join!(store.load_run("warm-1"), store.load_run("warm-2"));
    a.unwrap();
    b.unwrap();

    // Kill connections via reset_peer, then heal after the first reset is hit so
    // retries must evict the broken pooled sockets and open fresh healthy ones.
    pg.toxi.reset_peer().await;
    let api = pg.toxi.api();
    let healer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        api.heal().await;
    });

    store
        .append("after-reset", row(0))
        .await
        .expect("PostgresStore must evict reset idle connections and recover via retry");
    healer.await.unwrap();
    assert_eq!(store.load_run("after-reset").await.unwrap(), [row(0)]);
    assert!(store.load("after-reset").await.unwrap().is_none());
}
