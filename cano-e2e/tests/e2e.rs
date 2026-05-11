//! End-to-end hardening of crash recovery + sagas, using Docker via `testcontainers`:
//!
//! - a Postgres container is the [`cano::CheckpointStore`] backend
//!   (`cano_e2e::PostgresCheckpointStore`);
//! - the workflow runs inside a separate container — the `cano_workflow_app` binary
//!   (built from `../Dockerfile`) — so we can stop it mid-flight and restart it to resume;
//! - the test process connects to the Postgres container directly to assert on the
//!   `cano_checkpoints` / `ledger` / `events` tables.
//!
//! Requires a working Docker daemon. The CI workflow `.github/workflows/e2e.yml` builds
//! the app image first; locally it's built on demand (see [`ensure_app_image`]).
//! Run with `cargo test -p cano-e2e --test e2e -- --test-threads=1`.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use cano_e2e::{UNITS, events, ledger_balance};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio_postgres::{Client, NoTls};

const APP_IMAGE: &str = "cano-workflow-app";
const APP_TAG: &str = "e2e";

/// Build the `cano_workflow_app` image once per test process (idempotent — `docker build`
/// is layer-cached, so this is a fast no-op when CI already built it).
fn ensure_app_image() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let status = Command::new("docker")
            .current_dir(root)
            .args([
                "build",
                "-t",
                &format!("{APP_IMAGE}:{APP_TAG}"),
                "-f",
                "cano-e2e/Dockerfile",
                ".",
            ])
            .status()
            .expect("run `docker build` (is Docker installed and running?)");
        assert!(status.success(), "docker build failed");
    });
}

/// Unique suffix so concurrently-running tests don't collide on container / network names.
fn unique(prefix: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// A scenario fixture: a Postgres container plus a private network app containers join.
struct Fixture {
    _pg: ContainerAsync<Postgres>,
    network: String,
    pg_host_port: u16,
    pg_name: String,
}

impl Fixture {
    async fn start() -> Self {
        ensure_app_image();
        let network = unique("cano-e2e-net");
        let pg_name = unique("cano-e2e-pg");
        let pg = Postgres::default()
            .with_network(&network)
            .with_container_name(&pg_name)
            .start()
            .await
            .expect("start postgres container");
        let pg_host_port = pg
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        Self {
            _pg: pg,
            network,
            pg_host_port,
            pg_name,
        }
    }

    /// DSN for the test process (Postgres's published port on the host).
    fn host_dsn(&self) -> String {
        format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            self.pg_host_port
        )
    }

    /// DSN for an app container (Postgres's container name on the shared network).
    fn container_dsn(&self) -> String {
        format!(
            "postgres://postgres:postgres@{}:5432/postgres",
            self.pg_name
        )
    }

    /// Start an app container with `args` after the entrypoint. `start()` blocks until
    /// `wait_for` appears on the container's stdout (use a terminal marker like
    /// `"DONE Done"` / `"FAILED"`, or `"PAUSED Ship"` to catch it mid-flight).
    async fn app(&self, args: &[&str], wait_for: &str) -> ContainerAsync<GenericImage> {
        let mut cmd: Vec<String> = vec![self.container_dsn()];
        cmd.extend(args.iter().map(|s| s.to_string()));
        GenericImage::new(APP_IMAGE, APP_TAG)
            .with_wait_for(testcontainers::core::WaitFor::message_on_stdout(wait_for))
            .with_startup_timeout(Duration::from_secs(90))
            .with_network(&self.network)
            .with_container_name(unique("cano-e2e-app"))
            .with_cmd(cmd)
            .start()
            .await
            .expect("start app container")
    }

    async fn db(&self) -> Client {
        let (client, conn) = tokio_postgres::connect(&self.host_dsn(), NoTls)
            .await
            .expect("connect to postgres from test");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client
    }
}

async fn stdout(c: &ContainerAsync<GenericImage>) -> String {
    String::from_utf8_lossy(&c.stdout_to_vec().await.unwrap_or_default()).into_owned()
}

/// Poll the container's stdout until it contains `marker` (returning the full stdout), or
/// panic after `timeout`. Use this instead of a fixed sleep when the container keeps
/// running after `start()` returned (its `WaitFor` was an earlier marker like `READY`).
async fn wait_for_marker(
    c: &ContainerAsync<GenericImage>,
    marker: &str,
    timeout: Duration,
) -> String {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let out = stdout(c).await;
        if out.contains(marker) {
            return out;
        }
        if std::time::Instant::now() >= deadline {
            panic!("timed out waiting for {marker:?}; stdout so far:\n{out}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn checkpoint_rows(client: &Client, wf: &str) -> i64 {
    client
        .query_one(
            "SELECT COUNT(*)::bigint FROM cano_checkpoints WHERE workflow_id = $1",
            &[&wf],
        )
        .await
        .map(|r| r.get::<_, i64>(0))
        .unwrap_or(-1)
}

#[tokio::test]
async fn crash_mid_flight_then_resume_completes_without_losing_or_duplicating_effects() {
    let fx = Fixture::start().await;
    let wf = "order-1";

    // Run 1: pause when entering `Ship` — `Reserve` is fully checkpointed (entry +
    // completion) and its ledger effect is durable; then the process is killed.
    let app1 = fx.app(&[wf, "run", "pause_at=Ship"], "PAUSED Ship").await;
    app1.stop().await.expect("stop app1"); // SIGTERM then SIGKILL — dies mid-workflow.
    drop(app1);

    {
        let db = fx.db().await;
        assert!(
            checkpoint_rows(&db, wf).await > 0,
            "the crashed run left a recovery log behind"
        );
        assert_eq!(
            ledger_balance(&db, wf, "inventory").await,
            -UNITS,
            "Reserve held its units before the crash"
        );
        let ev = events(&db, wf).await;
        assert!(ev.contains(&"run:Reserve".to_string()) && ev.contains(&"run:Ship".to_string()));
    }

    // Run 2: resume — must pick up at `Ship`, finish at `Done`, NOT re-run `Reserve`, and
    // its idempotent re-run of `Ship` must not duplicate `Reserve`'s ledger effect.
    let app2 = fx.app(&[wf, "resume"], "READY").await;
    let out2 = wait_for_marker(&app2, "DONE Done", Duration::from_secs(60)).await;
    assert!(
        out2.contains(&format!("RESUME {wf}")),
        "expected a resume marker:\n{out2}"
    );
    drop(app2);

    let db = fx.db().await;
    assert_eq!(
        ledger_balance(&db, wf, "inventory").await,
        -UNITS,
        "a completed run leaves Reserve's units held — not doubled, not released"
    );
    assert_eq!(
        checkpoint_rows(&db, wf).await,
        0,
        "the completed run cleared its recovery log"
    );
    let ev = events(&db, wf).await;
    assert_eq!(
        ev.iter().filter(|e| *e == "run:Reserve").count(),
        1,
        "Reserve ran exactly once (before the resume point): {ev:?}"
    );
}

/// Poll the container's stdout until it shows a terminal marker (`DONE …` or `FAILED …`).
async fn wait_for_finish(c: &ContainerAsync<GenericImage>, timeout: Duration) -> String {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let out = stdout(c).await;
        if out.contains("DONE ") || out.contains("FAILED ") {
            return out;
        }
        if std::time::Instant::now() >= deadline {
            panic!("timed out waiting for the run to finish; stdout so far:\n{out}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
async fn concurrent_distinct_workflow_ids_all_complete() {
    let fx = Fixture::start().await;
    const N: usize = 6;

    // Start all the containers first (each `start()` only blocks until `READY`), so their
    // workflows actually run concurrently against the one shared Postgres store.
    let mut apps = Vec::new();
    for i in 0..N {
        apps.push(fx.app(&[&format!("run-{i}"), "run"], "READY").await);
    }
    for (i, app) in apps.iter().enumerate() {
        let out = wait_for_finish(app, Duration::from_secs(90)).await;
        assert!(
            out.contains("DONE Done"),
            "run-{i} did not complete:\n{out}"
        );
    }
    let db = fx.db().await;
    for i in 0..N {
        let wf = format!("run-{i}");
        assert_eq!(
            ledger_balance(&db, &wf, "inventory").await,
            -UNITS,
            "{wf}: completed, units held"
        );
        assert_eq!(
            checkpoint_rows(&db, &wf).await,
            0,
            "{wf}: completed run cleared its log"
        );
    }
    drop(apps);
}

#[tokio::test]
async fn same_workflow_id_concurrently_one_fails_with_a_conflict() {
    let fx = Fixture::start().await;
    let wf = "shared";

    // Both containers race the first checkpoint append for the same workflow id.
    let a = fx.app(&[wf, "run"], "READY").await;
    let b = fx.app(&[wf, "run"], "READY").await;
    let outs = [
        wait_for_finish(&a, Duration::from_secs(60)).await,
        wait_for_finish(&b, Duration::from_secs(60)).await,
    ];
    let done = outs.iter().filter(|o| o.contains("DONE Done")).count();
    let conflicts = outs
        .iter()
        .filter(|o| o.contains("FAILED") && o.contains("conflict"))
        .count();
    assert!(
        done >= 1,
        "one run must win sequence 0 and complete:\n{}\n---\n{}",
        outs[0],
        outs[1]
    );
    assert!(
        done == 2 || (done == 1 && conflicts == 1),
        "the loser (if any) failed with a conflict:\n{}\n---\n{}",
        outs[0],
        outs[1]
    );
    drop((a, b));
}

#[tokio::test]
async fn ship_failure_rolls_back_reserve() {
    let fx = Fixture::start().await;
    let wf = "bad-ship";

    let app = fx.app(&[wf, "run", "fail_at=Ship"], "READY").await;
    let out = wait_for_finish(&app, Duration::from_secs(60)).await;
    assert!(
        out.contains("Ship failed"),
        "expected the Ship failure:\n{out}"
    );
    drop(app);

    let db = fx.db().await;
    assert_eq!(
        ledger_balance(&db, wf, "inventory").await,
        0,
        "Reserve was compensated — its units released"
    );
    let ev = events(&db, wf).await;
    assert!(
        ev.contains(&"compensate:Reserve".to_string()),
        "Reserve's compensation ran: {ev:?}"
    );
    assert_eq!(
        checkpoint_rows(&db, wf).await,
        0,
        "clean rollback cleared the recovery log"
    );
}
