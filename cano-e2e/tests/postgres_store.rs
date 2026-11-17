//! `PostgresStore` against a real Postgres in Docker, directly and through
//! Toxiproxy. Requires a working Docker daemon.
//! Run with `cargo test -p cano-e2e --test postgres_store -- --test-threads=1`.

mod support;

use std::time::Duration;

use cano::RetryMode;
use cano::checkpoint::CheckpointStore;
use cano::store::postgres::PostgresStore;
use cano::store::postgres::{deadpool_postgres, tokio_postgres};
use cano::testing::assert_checkpoint_store_contract;
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
        "SELECT state, task_id, kind, workflow_version, output_blob FROM cano_checkpoints \
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
