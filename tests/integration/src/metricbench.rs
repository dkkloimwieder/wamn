//! The `metricbench` subcommand: the [9.8] metric-set gate (wamn-jn6).
//!
//! 9.8 ships the host-side metric set — run executions + success ratio, run-drive
//! duration, run-queue depth, `wamn:postgres` pool saturation + query latency,
//! per-component memory high-water/denials, and generated-API RPS — over the
//! SAME global meter provider the S5/9.1 pipeline installs (the fork's
//! `initialize_observability`, active whenever `OTEL_*` is set). This gate drives
//! the real production emission seams and asserts each family lands in the OTel
//! Collector's Prometheus scrape (`:8889`, the metrics analog of `tracebench`'s
//! Tempo query / `logbench`'s Loki query):
//!
//!   1. drive N normal runs plus exactly one forced failure through the production
//!      executor -> `wamn_run_executions` grows by N+1 and carries an
//!      `outcome="failed"` series (success ratio);
//!   2. seed a queue then run a dispatcher tick -> `wamn_run_queue_depth` > 0,
//!      then drain -> back to 0;
//!   3. `wamn_run_drive_duration_ms_count` grows by N+1 for the same drives;
//!   4. force a memory-limiter denial -> `wamn_memory_denied` > 0 and
//!      `wamn_memory_high_water_bytes` reads the ALLOWED size, not the budget;
//!   5. the run drives' own DB calls surface `wamn_postgres_pool_size` and
//!      `wamn_postgres_query_duration_ms_count` > 0;
//!   6. M api-gateway calls -> `wamn_api_requests` (the fork's inbound HTTP
//!      counter) — IN-CLUSTER ONLY (ProxyPre benches bypass the host's HTTP
//!      server), honest-skipped locally.
//!
//! Local recipe (docs/archive/observability/metrics.md): the tracebench docker collector +
//! otelcol-local's new metrics pipeline + a throwaway Postgres, with
//! `OTEL_METRIC_EXPORT_INTERVAL=1000` so the periodic reader does not wait a
//! minute. The repository-local recipe remains available; its in-cluster Job is
//! archived for MVP.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::Instant;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{Artifact, NodeImplementation};
use wamn_flow::Flow;
use wamn_node_manifest::{RecoveryClass, ResolvedNodeInterface, ResolvedPurity};
use wamn_run_state::queue::{enqueue_sql, write_ahead_triggered_run_sql};

use crate::dispatcher_process::{DispatcherProcess, ProjectSpec};
use wamn_runtime::memory_metrics::global_memory_meter;
use wash_runtime::engine::ctx::WamnStoreLimiter;
use wash_runtime::wasmtime::ResourceLimiter as _;

/// The metricbench run-plane schema inside its throwaway database.
const SCHEMA: &str = "wamn_metricbench";
const TENANT: &str = "metric-tenant";
const OWNER: &str = "metric-bench";
const CATALOG_ID: &str = "metricbench";
const CATALOG_VERSION: i32 = 1;
/// The normal (completing) fixture flow — poc-receipt (webhook, pg-write): its DB
/// write also drives the pool + query-latency families.
const FLOW_ID: &str = "poc-receipt";
/// The forced-failure fixture: its only work node is `postgres-query`, which dies
/// `Terminal("capability-denied")` at the standard-node grant check (D8 raw-SQL
/// off) — a one-step, no-I/O terminal business failure (outcome = failed),
/// deterministic and instant (unlike a runaway-budget spin).
const FAIL_FLOW_ID: &str = "metric-terminal";

/// The component id the phase-4 forced-denial limiter is labelled by.
const MEM_COMPONENT: &str = "metricbench-memhog";
const CATALOG_DDL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_DDL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_DDL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const EXECUTOR_BINARY_ENV: &str = "WAMN_RUN_WORKER_BIN";
const EXECUTOR_NATS_URL: &str = "nats://127.0.0.1:1";
const EXECUTOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const RUN_BATCH_TIMEOUT: Duration = Duration::from_secs(10);

fn fail_flow_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FAIL_FLOW_ID}","version":1,
            "nodes":[
              {{"id":"in","type":"request","config":{{"input-schema":true}}}},
              {{"id":"q","type":"postgres-query","config":{{}}}},
              {{"id":"out","type":"respond","config":{{"status":200}}}}
            ],
            "edges":[{{"from":"in","to":"q"}},{{"from":"q","to":"out"}}]}}"#
    )
}

#[derive(Debug, Args)]
pub struct MetricBenchArgs {
    /// The flowrunner guest the runner instantiates + drives.
    #[arg(long)]
    pub flowrunner: PathBuf,

    /// App (runner) Postgres URL — the NOSUPERUSER wamn_app role. Overrides
    /// WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: creates/drops the isolated metricbench database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The OTel Collector's Prometheus scrape (the new :8889 app-metrics
    /// pipeline). In-cluster: http://otel-collector:8889/metrics.
    #[arg(
        long,
        env = "METRICS_URL",
        default_value = "http://127.0.0.1:8889/metrics"
    )]
    pub metrics_url: String,

    /// Normal (completing) runs driven in phase 1.
    #[arg(long, default_value_t = 8)]
    pub runs: usize,

    /// Claimable runs seeded for the phase-2 depth check.
    #[arg(long, default_value_t = 6)]
    pub depth_seed: usize,

    /// api-gateway calls the in-cluster phase 6 would drive (SKIPPED locally).
    #[arg(long, default_value_t = 5)]
    pub api_calls: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RunBatchStatus {
    total: i64,
    completed: i64,
    failed: i64,
}

#[derive(Debug)]
struct ExecutorProcess {
    child: Child,
}

impl ExecutorProcess {
    fn spawn(flowrunner: &Path, database_url: &str, project: &str) -> anyhow::Result<Self> {
        let binary = executor_binary();
        let mut command = executor_command(&binary, flowrunner, database_url, project);
        command
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let child = command
            .spawn()
            .with_context(|| format!("launch executor process {}", binary.to_string_lossy()))?;
        Ok(Self { child })
    }

    async fn wait_for_batch(
        &mut self,
        client: &Client,
        run_prefix: &str,
        expected: RunBatchStatus,
    ) -> anyhow::Result<RunBatchStatus> {
        let deadline = Instant::now() + RUN_BATCH_TIMEOUT;
        loop {
            let observed = run_batch_status(client, run_prefix).await?;
            if observed == expected {
                return Ok(observed);
            }
            if let Some(status) = self
                .child
                .try_wait()
                .context("poll executor while waiting for run batch")?
            {
                bail!(
                    "executor exited {status} before {run_prefix} reached {expected:?}; \
                     observed {observed:?}"
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "executor did not settle {run_prefix} within {RUN_BATCH_TIMEOUT:?}; \
                     expected {expected:?}, observed {observed:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn shutdown(mut self) -> anyhow::Result<bool> {
        if let Some(status) = self
            .child
            .try_wait()
            .context("poll executor before shutdown")?
        {
            return Ok(status.success());
        }

        let pid = self
            .child
            .id()
            .context("executor has no process id before shutdown")?;
        let pid = libc::pid_t::try_from(pid).context("executor process id exceeds pid_t")?;
        // SAFETY: `kill` does not dereference pointers. The PID comes directly
        // from the live child and SIGTERM is the service's graceful boundary.
        let signal_result = unsafe { libc::kill(pid, libc::SIGTERM) };
        if signal_result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("send SIGTERM to executor process");
            }
        }

        match tokio::time::timeout(EXECUTOR_SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(status) => Ok(status.context("wait for executor shutdown")?.success()),
            Err(_) => {
                self.child
                    .start_kill()
                    .context("kill executor after shutdown timeout")?;
                let _ = self.child.wait().await;
                Ok(false)
            }
        }
    }
}

impl Drop for ExecutorProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn executor_binary() -> OsString {
    if let Some(binary) = std::env::var_os(EXECUTOR_BINARY_ENV) {
        return binary;
    }

    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("wamn-run-worker")));
    sibling
        .filter(|path| path.is_file())
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from("wamn-run-worker"))
}

fn executor_command(
    binary: &OsStr,
    flowrunner: &Path,
    database_url: &str,
    project: &str,
) -> Command {
    let mut command = Command::new(binary);
    command
        .env_remove("WAMN_ALLOWED_HOSTS")
        .env_remove("WAMN_CREDENTIALS_FILE")
        // warn, not error: the run-worker reports effect-level REFUSALS at warn
        // (e.g. "effect run artifact_digest shape: Null"). At `error` the child's
        // stderr is silent about them, and a whole wave read that silence as
        // success. The gate asserts on database state, never on this stream, so
        // the extra lines cost nothing and buy the refusal signal.
        .arg("--log-level")
        .arg("warn")
        .arg("--flowrunner")
        .arg(flowrunner)
        .arg("--database-url")
        .arg(database_url)
        .arg("--tenant")
        .arg(TENANT)
        .arg("--schema")
        .arg(SCHEMA)
        .arg("--runner")
        .arg(OWNER)
        .arg("--project")
        .arg(project)
        .arg("--nats-url")
        .arg(EXECUTOR_NATS_URL)
        .arg("--min-idle-ms")
        .arg("25")
        .arg("--max-idle-ms")
        .arg("100");
    command
}

async fn run_batch_status(client: &Client, run_prefix: &str) -> anyhow::Result<RunBatchStatus> {
    let pattern = format!("{run_prefix}%");
    let row = client
        .query_one(
            "SELECT count(*)::bigint, \
                    count(*) FILTER (WHERE status = 'completed')::bigint, \
                    count(*) FILTER (WHERE status = 'failed')::bigint \
               FROM runs WHERE run_id LIKE $1",
            &[&pattern],
        )
        .await?;
    Ok(RunBatchStatus {
        total: row.get(0),
        completed: row.get(1),
        failed: row.get(2),
    })
}

// ---------------------------------------------------------------------------
// Hermetic database + immutable release seeding. The throwaway database keeps
// the per-database `catalog` schema isolated from every earlier serial gate,
// while canonical deploy SQL keeps the fixture on the production contract.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FixtureArtifact {
    flow_id: String,
    graph_json: String,
    graph_hash: String,
    artifact_hash: String,
    interface_bundle_json: String,
    interface_bundle_hash: String,
    component_digests: Value,
    occurrence_recovery_json: String,
    occurrence_recovery_hash: String,
}

fn interface(
    node_type: &str,
    purity: ResolvedPurity,
    recovery_class: RecoveryClass,
) -> NodeImplementation {
    let recovery = match (purity, recovery_class) {
        (ResolvedPurity::Pure, RecoveryClass::Replay) => {
            wamn_node_manifest::ExecutableRecoveryContract::pure()
        }
        (ResolvedPurity::Effectful, RecoveryClass::NeverReplay) => {
            wamn_node_manifest::ExecutableRecoveryContract::effectful(false)
        }
        _ => panic!("fixture recovery semantics must be canonical"),
    };
    NodeImplementation::platform(
        ResolvedNodeInterface::new(
            node_type,
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![if purity == ResolvedPurity::Pure {
                wamn_node_manifest::CapabilityClass::Pure
            } else {
                wamn_node_manifest::CapabilityClass::Http
            }],
            Vec::new(),
        ),
        recovery,
    )
}

fn fixture_artifact(
    graph_json: &str,
    implementations: Vec<NodeImplementation>,
) -> anyhow::Result<FixtureArtifact> {
    let flow =
        Flow::from_json(graph_json).map_err(|error| anyhow::anyhow!("flow parse: {error}"))?;
    let artifact = Artifact::new(TENANT, &flow, implementations)
        .map_err(|error| anyhow::anyhow!("immutable fixture artifact: {error}"))?;
    Ok(FixtureArtifact {
        flow_id: flow.flow_id.clone(),
        graph_json: String::from_utf8(flow.canonical_bytes())
            .expect("canonical flow graph is UTF-8"),
        graph_hash: artifact.graph_hash().to_string(),
        artifact_hash: artifact.identity().artifact_hash().as_str().to_string(),
        interface_bundle_json: String::from_utf8(
            artifact.interface_bundle().canonical_bytes().to_vec(),
        )
        .expect("canonical interface bundle is UTF-8"),
        interface_bundle_hash: artifact.interface_bundle().hash().to_string(),
        component_digests: serde_json::to_value(artifact.supplied_components())?,
        occurrence_recovery_json: String::from_utf8(artifact.occurrence_recovery_bytes().to_vec())
            .expect("canonical occurrence recovery selections are UTF-8"),
        occurrence_recovery_hash: artifact.occurrence_recovery_hash().to_string(),
    })
}

fn fixture_artifacts() -> anyhow::Result<Vec<FixtureArtifact>> {
    // `request` and `respond` carry resolved interfaces like any other node
    // type: the wamn-ayq7 series moved every engine node onto the node ABI and
    // emptied the model-owned set, so publication resolves them too. Both are
    // pure/replay with a single `main` port, exactly as the standard node
    // library describes them. Implementations stay sorted by node type.
    let mut artifacts = vec![
        fixture_artifact(
            &crate::flowbench::flow_json(1),
            vec![
                interface("conditional", ResolvedPurity::Pure, RecoveryClass::Replay),
                interface(
                    "pg-write",
                    ResolvedPurity::Effectful,
                    RecoveryClass::NeverReplay,
                ),
                interface("request", ResolvedPurity::Pure, RecoveryClass::Replay),
                interface("respond", ResolvedPurity::Pure, RecoveryClass::Replay),
                interface("transform", ResolvedPurity::Pure, RecoveryClass::Replay),
            ],
        )?,
        fixture_artifact(
            &fail_flow_json(),
            vec![
                interface(
                    "postgres-query",
                    ResolvedPurity::Effectful,
                    RecoveryClass::NeverReplay,
                ),
                interface("request", ResolvedPurity::Pure, RecoveryClass::Replay),
                interface("respond", ResolvedPurity::Pure, RecoveryClass::Replay),
            ],
        )?,
    ];
    artifacts.sort_by(|left, right| left.flow_id.cmp(&right.flow_id));
    Ok(artifacts)
}

fn fixture_ddl() -> String {
    let run_state = RUN_STATE_DDL.replace("wamn_run", SCHEMA);
    let run_queue = RUN_QUEUE_DDL.replace("wamn_run", SCHEMA);
    format!(
        "{CATALOG_DDL}\n{run_state}\n{run_queue}\n\
         CREATE TABLE {SCHEMA}.sink (\
           tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           run_id text NOT NULL, step int NOT NULL, payload text NOT NULL, \
           dispatch_seq bigint GENERATED ALWAYS AS IDENTITY, \
           CONSTRAINT sink_idem UNIQUE (tenant_id, run_id, step)); \
         ALTER TABLE {SCHEMA}.sink ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {SCHEMA}.sink FORCE ROW LEVEL SECURITY; \
         CREATE POLICY sink_tenant ON {SCHEMA}.sink \
           USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')) \
           WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), '')); \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {SCHEMA}.sink TO wamn_app;"
    )
}

fn proof_database_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    format!("wamn_metricbench_{}_{}", std::process::id(), nanos)
}

fn database_url(url: &str, database: &str) -> anyhow::Result<String> {
    let (prefix, tail) = url
        .rsplit_once('/')
        .context("PostgreSQL URL must contain a database path")?;
    let query = tail
        .find('?')
        .map(|index| &tail[index..])
        .unwrap_or_default();
    Ok(format!("{prefix}/{database}{query}"))
}

async fn create_database(admin_url: &str, database: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect to create metricbench database")?;
    let conn_task = tokio::spawn(conn);
    let result = client
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .await
        .context("create metricbench database");
    drop(client);
    let _ = conn_task.await;
    result
}

async fn drop_database(admin_url: &str, database: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect to drop metricbench database")?;
    let conn_task = tokio::spawn(conn);
    let result = client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"))
        .await
        .context("drop metricbench database");
    drop(client);
    let _ = conn_task.await;
    result
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (mut client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for metricbench fixture")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute(&fixture_ddl())
            .await
            .context("apply canonical catalog and run-plane DDL")?;

        let artifacts = fixture_artifacts()?;
        let members = Value::Array(
            artifacts
                .iter()
                .map(|artifact| {
                    json!({
                        "flow-id": artifact.flow_id,
                        "flow-version": 1,
                        "artifact-hash": artifact.artifact_hash,
                    })
                })
                .collect(),
        );
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id,catalog_id,version,environment,schema_version,state,document) \
                 VALUES ($1,$2,$3,'metricbench','0.1','applied','{}')",
                &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
            )
            .await?;
        for artifact in &artifacts {
            transaction
                .execute(
                    "INSERT INTO catalog.flow_artifacts \
                       (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                        artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests, \
                        occurrence_recovery_json,occurrence_recovery_hash) \
                     VALUES ($1,$2,1,'0.1',$3::text::jsonb,$4,$5,$6,$7,$8,$9,$10)",
                    &[
                        &TENANT,
                        &artifact.flow_id,
                        &artifact.graph_json,
                        &artifact.graph_hash,
                        &artifact.artifact_hash,
                        &artifact.interface_bundle_json,
                        &artifact.interface_bundle_hash,
                        &artifact.component_digests,
                        &artifact.occurrence_recovery_json,
                        &artifact.occurrence_recovery_hash,
                    ],
                )
                .await?;
        }
        transaction
            .execute(
                "INSERT INTO catalog.release_manifests \
                   (tenant_id,catalog_id,catalog_version,members_json) \
                 VALUES ($1,$2,$3,$4)",
                &[&TENANT, &CATALOG_ID, &CATALOG_VERSION, &members],
            )
            .await?;
        for artifact in &artifacts {
            transaction
                .execute(
                    "INSERT INTO catalog.release_flows \
                       (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
                     VALUES ($1,$2,$3,$4,1)",
                    &[
                        &TENANT,
                        &CATALOG_ID,
                        &CATALOG_VERSION,
                        &artifact.flow_id,
                    ],
                )
                .await?;
        }
        transaction.commit().await?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

/// A wamn_app connection pinned to the ephemeral schema + tenant claim (the RLS
/// floor + search_path the runner's plugin session runs under).
async fn connect_app(app_url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("set search_path + tenant claim")?;
    Ok((client, handle))
}

/// Seed a run the way the dispatcher does (write-ahead `dispatched` row +
/// immediately-claimable queue row), for the given flow at version 1.
async fn seed_run(client: &mut Client, run_id: &str, flow_id: &str) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &flow_id, &1i32, &"cron", &"\"receipt\""],
    )
    .await?;
    // Release pin + the durable invocation context admission would have written.
    // The effect path reads the trusted principal off the run row
    // (`invocation_context #>> '{principal,artifact-digest}'`) and refuses a run
    // whose digest is absent, so a fixture that only pinned catalog columns left
    // every drive dying in `run-next: effect run artifact_digest shape` and the
    // executor settled nothing (wamn-wddi). The digest comes from the seeded
    // release member itself, so the pin and the principal cannot disagree.
    let catalog_version = i64::from(CATALOG_VERSION);
    let pinned = tx
        .execute(
            "UPDATE runs AS r \
                SET catalog_id = $2, catalog_version = $3, environment = 'metricbench', \
                    invocation_context = jsonb_build_object( \
                      'version', 1, \
                      'principal', jsonb_build_object( \
                        'tenant-id', r.tenant_id, 'environment', 'metricbench', \
                        'catalog-id', $2::text, 'catalog-version', $3::bigint, \
                        'run-id', r.run_id, 'flow-id', r.flow_id, \
                        'flow-version', r.flow_version, \
                        'artifact-digest', a.artifact_hash), \
                      'source', jsonb_build_object('producer', r.trigger_source)) \
               FROM catalog.flow_artifacts AS a \
              WHERE a.tenant_id = r.tenant_id AND a.flow_id = r.flow_id \
                AND a.flow_version = r.flow_version \
                AND r.tenant_id = current_setting('app.tenant', true) AND r.run_id = $1",
            &[&run_id, &CATALOG_ID, &catalog_version],
        )
        .await?;
    anyhow::ensure!(pinned == 1, "seeded run {run_id} was not release-pinned");
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Option::<&str>::None, &0i32, &0i64],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

pub async fn run(args: MetricBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    // The exporter only activates when OTEL_* is set (fork observability init) —
    // without it nothing reaches the collector and every scrape is empty.
    if !std::env::vars().any(|(k, _)| k.starts_with("OTEL_")) {
        bail!(
            "no OTEL_* env set: metricbench needs OTEL_EXPORTER_OTLP_ENDPOINT pointing at the \
             collector (+ OTEL_METRIC_EXPORT_INTERVAL=1000) — else nothing is exported"
        );
    }

    std::fs::metadata(&args.flowrunner)
        .with_context(|| format!("failed to read {}", args.flowrunner.display()))?;
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "metricbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;
    let database = proof_database_name();
    let project = database.clone();
    let metric_app_url = database_url(&app_url, &database)?;
    let metric_admin_url = database_url(&admin_url, &database)?;
    create_database(&admin_url, &database).await?;

    let result = async {
        println!(
            "# wamn-gates [9.8] metricbench \
             (database {database}, schema {SCHEMA}, tenant {TENANT}, project {project})"
        );
        println!("metrics = {}", args.metrics_url);
        provision(&metric_admin_url)
            .await
            .context("provision hermetic catalog and run plane")?;

        let outcome = async {
            let (mut seed_conn, _h) = connect_app(&metric_app_url).await?;

            let mut pass = true;

            // === (1) executions counter + success ratio =========================
            let n = args.runs;
            let tenant_label = format!("wamn_tenant=\"{TENANT}\"");
            let project_label = format!("wamn_project=\"{project}\"");
            let failed_label = "outcome=\"failed\"";
            let baseline = fetch(&args.metrics_url).await.unwrap_or_default();
            let base_exec = labels_sum(
                &baseline,
                "wamn_run_executions",
                &[&tenant_label, &project_label],
            );
            let base_failed = labels_sum(
                &baseline,
                "wamn_run_executions",
                &[&tenant_label, &project_label, failed_label],
            );
            let base_duration = labels_sum(
                &baseline,
                "wamn_run_drive_duration_ms_count",
                &[&tenant_label, &project_label],
            );
            for i in 0..n {
                seed_run(&mut seed_conn, &format!("mb-{i}"), FLOW_ID).await?;
            }
            seed_run(&mut seed_conn, "mb-fail", FAIL_FLOW_ID).await?;
            let expected_first = RunBatchStatus {
                total: i64::try_from(n + 1).context("phase-1 run count exceeds i64")?,
                completed: i64::try_from(n).context("phase-1 completed count exceeds i64")?,
                failed: 1,
            };
            let mut executor = ExecutorProcess::spawn(&args.flowrunner, &metric_app_url, &project)?;
            let first = executor
                .wait_for_batch(&seed_conn, "mb-", expected_first)
                .await?;
            // Delta == N+1 executions, with at least one `failed` (the mutant target:
            // an outcome-fold would keep failed at its baseline).
            let want_total = (n + 1) as f64;
            let (exec_ok, (exec_total, failed_delta, executor_instance)) =
                poll(&args.metrics_url, |text| {
                    let run_labels = [tenant_label.as_str(), project_label.as_str()];
                    let failed_labels =
                        [tenant_label.as_str(), project_label.as_str(), failed_label];
                    let total = labels_sum(text, "wamn_run_executions", &run_labels) - base_exec;
                    let failed =
                        labels_sum(text, "wamn_run_executions", &failed_labels) - base_failed;
                    let instance = series_label_fragment(
                        text,
                        "wamn_run_executions",
                        &failed_labels,
                        "instance",
                    );
                    (
                        total == want_total && failed >= 1.0 && instance.is_some(),
                        (total, failed, instance),
                    )
                })
                .await;
            check(
                &mut pass,
                "(1) executions: delta == N+1 with a failed series",
                first == expected_first && exec_ok,
                &format!(
                    "database total={}/{} completed={} failed={} ; scrape delta={exec_total} \
                 (want {want_total}), failed delta={failed_delta} (want >=1), \
                 executor {executor_instance:?}",
                    first.total, expected_first.total, first.completed, first.failed,
                ),
            );

            // === (3) run-drive duration histogram (same drives) =================
            let (dur_ok, dur_delta) = poll(&args.metrics_url, |text| {
                let count = labels_sum(
                    text,
                    "wamn_run_drive_duration_ms_count",
                    &[&tenant_label, &project_label],
                );
                let delta = count - base_duration;
                (delta == want_total, delta)
            })
            .await;
            check(
                &mut pass,
                "(3) run-drive duration histogram delta == N+1",
                dur_ok,
                &format!("wamn_run_drive_duration_ms_count delta = {dur_delta}"),
            );

            // === (5) pool saturation + query latency (from the drives' DB writes) =
            // The guest's configured Postgres pool is `default`, independently of
            // the executor identity's unique project label. Correlate both families
            // through the OTel resource instance shared by this executor process.
            let executor_instance =
                executor_instance.unwrap_or_else(|| "instance=\"<missing>\"".to_string());
            let (pool_ok, (pool_size, query_count)) = poll(&args.metrics_url, |text| {
                let pool_present =
                    present_with_labels(text, "wamn_postgres_pool_size", &[&executor_instance]);
                let pool_size = labels_sum(text, "wamn_postgres_pool_size", &[&executor_instance]);
                let query_count = labels_sum(
                    text,
                    "wamn_postgres_query_duration_ms_count",
                    &[&executor_instance],
                );
                (pool_present && query_count > 0.0, (pool_size, query_count))
            })
            .await;
            check(
                &mut pass,
                "(5) postgres pool gauge present + query-latency count > 0",
                pool_ok,
                &format!(
                    "{executor_instance}: wamn_postgres_pool_size={pool_size}, \
                 query_count={query_count}"
                ),
            );
            anyhow::ensure!(
                executor.shutdown().await?,
                "executor did not shut down cleanly after the phase-1 metric export"
            );

            // === (2) run-queue depth via the dispatcher tick ====================
            // The real dispatcher executable owns the gauge and samples it during
            // stepped sweeps. Seed a claimable batch, tick -> depth > 0; drain ->
            // tick -> depth back to 0. The claimable predicate is the mutant target.
            let specs = [ProjectSpec {
                name: project.clone(),
                url: metric_app_url.clone(),
                tenant: TENANT.to_string(),
                schema: Some(SCHEMA.to_string()),
            }];
            let mut dispatcher =
                DispatcherProcess::spawn(&specs, "nats://127.0.0.1:1", None, None, None, None)?;
            let m = args.depth_seed;
            for i in 0..m {
                seed_run(&mut seed_conn, &format!("mq-{i}"), FLOW_ID).await?;
            }
            dispatcher
                .tick_project(0, chrono::Utc::now().timestamp_millis())
                .await?;
            let (depth_up_ok, depth_up) = poll(&args.metrics_url, |text| {
                let d = labels_sum(
                    text,
                    "wamn_run_queue_depth",
                    &[&tenant_label, &project_label],
                );
                (d >= m as f64, d)
            })
            .await;
            // Start the production executor only after the dispatcher has sampled
            // the high-water mark, then stop it before the zero-depth sample.
            let expected_second = RunBatchStatus {
                total: i64::try_from(m).context("phase-2 run count exceeds i64")?,
                completed: i64::try_from(m).context("phase-2 completed count exceeds i64")?,
                failed: 0,
            };
            let mut executor = ExecutorProcess::spawn(&args.flowrunner, &metric_app_url, &project)?;
            let second = executor
                .wait_for_batch(&seed_conn, "mq-", expected_second)
                .await?;
            anyhow::ensure!(
                executor.shutdown().await?,
                "executor did not shut down cleanly after draining the phase-2 queue"
            );
            dispatcher
                .tick_project(0, chrono::Utc::now().timestamp_millis())
                .await?;
            let (depth_zero_ok, depth_zero) = poll(&args.metrics_url, |text| {
                let d = labels_sum(
                    text,
                    "wamn_run_queue_depth",
                    &[&tenant_label, &project_label],
                );
                (d == 0.0, d)
            })
            .await;
            check(
                &mut pass,
                "(2) run_queue depth > 0 on a seeded queue, drains to 0",
                depth_up_ok && depth_zero_ok && second == expected_second,
                &format!(
                    "seeded {m}: depth peaked {depth_up} (want >= {m}), after drain \
                 (completed {}) depth {depth_zero} (want 0)",
                    second.completed
                ),
            );

            // === (4) memory limiter denial + high-water (budget knob) ===========
            // Force one allowed grow (sets high-water) then one over-budget grow
            // (denied) on a budgeted limiter, snapshot it into the process memory
            // meter, and assert the SCRAPE: denied >= 1, high_water reads the ALLOWED
            // 32 MiB (NOT the 64 MiB budget — the budget-vs-high-water swap mutant).
            const MIB: usize = 1 << 20;
            let mut limiter = WamnStoreLimiter::new(64 * MIB, Arc::from(MEM_COMPONENT));
            let allowed = limiter.memory_growing(0, 32 * MIB, None)?;
            let denied = limiter.memory_growing(32 * MIB, 128 * MIB, None)?;
            let mem = global_memory_meter();
            mem.snapshot_from(&limiter);
            let inproc = mem.snapshot_of(MEM_COMPONENT);
            let (mem_ok, (mem_denied, mem_hw)) = poll(&args.metrics_url, |text| {
                let d = family_sum(text, "wamn_memory_denied");
                let hw = label_value(
                    text,
                    "wamn_memory_high_water_bytes",
                    &format!("component=\"{MEM_COMPONENT}\""),
                );
                let budget_present = present(text, "wamn_memory_budget_bytes");
                (
                    d >= 1.0 && hw == Some((32 * MIB) as f64) && budget_present,
                    (d, hw),
                )
            })
            .await;
            check(
                &mut pass,
                "(4) memory: denied >= 1 and high_water is the allowed size, not the budget",
                allowed && !denied && mem_ok,
                &format!(
                    "limiter allowed={allowed} denied={denied}; in-proc snapshot={inproc:?}; \
                 scrape denied={mem_denied} high_water={mem_hw:?} (want {} not the 64 MiB budget)",
                    (32 * MIB) as f64
                ),
            );

            // === (6) generated-API RPS (IN-CLUSTER ONLY) ========================
            // The fork's wamn.api.requests counter fires in the host HTTP server's
            // record_response_status; ProxyPre benches bypass that server, so there
            // is no local way to drive it. Honest-skip (traceproof-style) — this
            // phase does NOT touch `pass`.
            println!(
                "## (6) api RPS — SKIP: wamn_api_requests needs the deployed api-gateway \
             ({} calls); ProxyPre bypasses the host HTTP server locally (in-cluster only)",
                args.api_calls
            );

            // Housekeeping counts (informational).
            let queued = count(
                &seed_conn,
                &format!("SELECT count(*) FROM {SCHEMA}.run_queue"),
            )
            .await?;
            println!("queue drained fully = {}", queued == 0);

            anyhow::Ok(pass)
        }
        .await;

        let pass = outcome?;

        println!("\nmetricbench complete — overall PASS: {pass}");
        if !pass {
            bail!("metricbench gate failed");
        }
        anyhow::Ok(())
    }
    .await;
    let cleanup = drop_database(&admin_url, &database).await;
    result.and(cleanup)
}

fn check(pass: &mut bool, label: &str, ok: bool, detail: &str) {
    if ok {
        println!("## {label} -> PASS");
    } else {
        *pass = false;
        println!("## {label} -> FAIL ({detail})");
    }
}

// ---------------------------------------------------------------------------
// Prometheus scrape + text parsing (the logbench :8888 helper, generalized to
// arbitrary wamn_* families / labels on the :8889 app-metrics pipeline)
// ---------------------------------------------------------------------------

/// Poll the scrape (~30s bounded) until `f` accepts, returning `(accepted, value)`
/// from the last observation. The bound covers the OTel periodic export
/// (`OTEL_METRIC_EXPORT_INTERVAL`) + the collector batch + Prometheus refresh.
async fn poll<T, F>(url: &str, f: F) -> (bool, T)
where
    F: Fn(&str) -> (bool, T),
{
    let mut last = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(750)).await;
        let text = fetch(url).await.unwrap_or_default();
        let (ok, value) = f(&text);
        if ok {
            return (true, value);
        }
        last = Some(value);
    }
    // Report the final observation for the failure detail.
    let text = fetch(url).await.unwrap_or_default();
    let (_, value) = f(&text);
    (false, last.unwrap_or(value))
}

/// Whether a scrape line is exactly `name` (followed by `{` or a space), so
/// `wamn_run_executions` never matches `wamn_run_executions_created` or the
/// `_bucket`/`_sum` siblings of a histogram.
fn line_is(line: &str, name: &str) -> bool {
    line.strip_prefix(name)
        .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
}

/// The value (last whitespace token) of a scrape line, if it parses.
fn line_value(line: &str) -> Option<f64> {
    line.rsplit(' ').next().and_then(|v| v.parse::<f64>().ok())
}

/// Sum every series of `name`.
fn family_sum(text: &str, name: &str) -> f64 {
    text.lines()
        .filter(|l| !l.starts_with('#') && line_is(l, name))
        .filter_map(line_value)
        .sum()
}

/// Sum every series of `name` whose label set contains every raw label fragment.
fn labels_sum(text: &str, name: &str, labels: &[&str]) -> f64 {
    text.lines()
        .filter(|line| {
            !line.starts_with('#')
                && line_is(line, name)
                && labels.iter().all(|label| line.contains(label))
        })
        .filter_map(line_value)
        .sum()
}

/// Return one raw `key="value"` label fragment from a matching series.
fn series_label_fragment(
    text: &str,
    name: &str,
    required_labels: &[&str],
    key: &str,
) -> Option<String> {
    let prefix = format!("{key}=\"");
    text.lines()
        .filter(|line| {
            !line.starts_with('#')
                && line_is(line, name)
                && required_labels.iter().all(|label| line.contains(label))
        })
        .find_map(|line| {
            let labels = line
                .strip_prefix(name)?
                .strip_prefix('{')?
                .split_once('}')?
                .0;
            labels.split(',').find_map(|label| {
                let value = label.strip_prefix(&prefix)?;
                let end = value.find('"')?;
                Some(format!("{prefix}{}\"", &value[..end]))
            })
        })
}

/// The value of the first series of `name` carrying `label`, if any.
fn label_value(text: &str, name: &str, label: &str) -> Option<f64> {
    text.lines()
        .find(|l| !l.starts_with('#') && line_is(l, name) && l.contains(label))
        .and_then(line_value)
}

/// Whether any series of `name` is present.
fn present(text: &str, name: &str) -> bool {
    present_with_labels(text, name, &[])
}

/// Whether a series of `name` carrying every raw label fragment is present.
fn present_with_labels(text: &str, name: &str, labels: &[&str]) -> bool {
    text.lines().any(|line| {
        !line.starts_with('#')
            && line_is(line, name)
            && labels.iter().all(|label| line.contains(label))
    })
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 GET (the collector's Prometheus endpoint is plain http;
// Go/chunked like Loki/Tempo — same helper shape as logbench/tracebench).
// ---------------------------------------------------------------------------

async fn fetch(url: &str) -> anyhow::Result<String> {
    let host_port = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = match host_port.find('/') {
        Some(i) => (&host_port[..i], &host_port[i..]),
        None => (host_port, "/"),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80)),
        None => (host_port.to_string(), 80),
    };
    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("connect {host}:{port}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: text/plain\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    if !(text.starts_with("HTTP/1.1 2") || text.starts_with("HTTP/1.0 2")) {
        bail!("GET {path} -> {}", text.lines().next().unwrap_or("<none>"));
    }
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_default();
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        Ok(dechunk(&body))
    } else {
        Ok(body)
    }
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].strip_prefix("\r\n").unwrap_or(&after[size..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermetic_preamble_contains_release_flows_required_by_run_next() {
        let ddl = fixture_ddl();
        for relation in [
            "CREATE TABLE catalog.flow_artifacts (",
            "CREATE TABLE catalog.release_manifests (",
            "CREATE TABLE catalog.release_flows (",
        ] {
            assert!(
                ddl.contains(relation),
                "metricbench preamble omitted {relation}"
            );
        }
        assert!(ddl.contains(&format!("CREATE TABLE {SCHEMA}.runs")));
        assert!(ddl.contains("catalog_id      text"));
        assert!(ddl.contains("catalog_version bigint"));
        assert!(
            !ddl.contains(&format!("CREATE TABLE {SCHEMA}.flows")),
            "metricbench must not fall back to the legacy mutable flow table"
        );
    }

    #[test]
    fn release_fixture_round_trips_through_pinned_artifact_verification() {
        let artifacts = fixture_artifacts().unwrap();
        assert_eq!(
            artifacts
                .iter()
                .map(|artifact| artifact.flow_id.as_str())
                .collect::<Vec<_>>(),
            [FAIL_FLOW_ID, FLOW_ID]
        );
        for artifact in artifacts {
            wamn_catalog::PinnedArtifact::from_storage(
                TENANT,
                &artifact.flow_id,
                1,
                &artifact.graph_json,
                &artifact.graph_hash,
                &artifact.artifact_hash,
                &artifact.interface_bundle_json,
                &artifact.interface_bundle_hash,
                &artifact.component_digests.to_string(),
                Some(&artifact.occurrence_recovery_json),
                Some(&artifact.occurrence_recovery_hash),
            )
            .unwrap();
        }
    }

    #[test]
    fn database_url_replaces_only_the_database_path() {
        assert_eq!(
            database_url(
                "postgresql://u:p@db:5432/base?sslmode=disable",
                "metricbench"
            )
            .unwrap(),
            "postgresql://u:p@db:5432/metricbench?sslmode=disable"
        );
    }

    #[test]
    fn executor_command_preserves_the_production_metric_boundary() {
        let command = executor_command(
            OsStr::new("/proof/wamn-run-worker"),
            Path::new("/proof/flowrunner.wasm"),
            "postgres://app@db/metricbench",
            "metric-proof",
        );
        let command = command.as_std();
        assert_eq!(command.get_program(), "/proof/wamn-run-worker");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--log-level",
                "warn",
                "--flowrunner",
                "/proof/flowrunner.wasm",
                "--database-url",
                "postgres://app@db/metricbench",
                "--tenant",
                TENANT,
                "--schema",
                SCHEMA,
                "--runner",
                OWNER,
                "--project",
                "metric-proof",
                "--nats-url",
                EXECUTOR_NATS_URL,
                "--min-idle-ms",
                "25",
                "--max-idle-ms",
                "100",
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn run_metrics_proof_cannot_bypass_executor_process() {
        let source = include_str!("metricbench.rs");
        let process_spawn = ["ExecutorProcess", "::spawn"].concat();
        let direct_instantiate = ["ExecutionHost", "::instantiate"].concat();
        let unobserved_drain = ["worker", ".drain().await"].concat();
        assert!(
            source.contains(&process_spawn),
            "metricbench must spawn the production executor process"
        );
        assert!(
            !source.contains(&direct_instantiate) && !source.contains(&unobserved_drain),
            "metricbench bypassed executor-owned RunMetrics"
        );

        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest
                .lines()
                .any(|line| line.trim_start().starts_with("wamn-executor")),
            "metricbench must drive wamn-executor through its executable boundary"
        );
        let dockerfile = include_str!("../../../Dockerfile");
        assert!(dockerfile.contains(
            "COPY --from=builder /native-output/wamn-run-worker \
             /usr/local/bin/wamn-run-worker"
        ));
    }

    // The prometheus-text parser: exact family matching (never a `_created` or
    // `_bucket` sibling), label filtering, and value extraction — the pure bit
    // the scrape assertions stand on.
    #[test]
    fn prom_text_parse_family_label_present() {
        let text = "\
# HELP wamn_run_executions runs
# TYPE wamn_run_executions counter
wamn_run_executions{instance=\"metric-process\",outcome=\"completed\",wamn_project=\"metric-proof\",wamn_tenant=\"metric-tenant\"} 8
wamn_run_executions{instance=\"metric-process\",outcome=\"failed\",wamn_project=\"metric-proof\",wamn_tenant=\"metric-tenant\"} 1
wamn_run_executions_created{outcome=\"completed\"} 1.72e9
wamn_run_drive_duration_ms_count{instance=\"metric-process\",wamn_project=\"metric-proof\",wamn_tenant=\"metric-tenant\"} 9
wamn_postgres_pool_size{instance=\"metric-process\",wamn_project=\"default\"} 1
wamn_postgres_query_duration_ms_count{db_operation=\"query\",instance=\"metric-process\",wamn_project=\"default\"} 9
wamn_postgres_query_duration_ms_count{db_operation=\"query\",instance=\"other-process\",wamn_project=\"default\"} 44182
wamn_memory_high_water_bytes{component=\"metricbench-memhog\"} 33554432
wamn_run_queue_depth{wamn_project=\"metric-proof\",wamn_tenant=\"metric-tenant\"} 6
wamn_run_queue_depth{wamn_project=\"f1\",wamn_tenant=\"f1-tenant\"} 3619
";
        let tenant = "wamn_tenant=\"metric-tenant\"";
        let project = "wamn_project=\"metric-proof\"";
        // Family sum ignores the `_created` sibling (its huge timestamp would
        // otherwise dominate) and the `_count` of a different family.
        assert_eq!(family_sum(text, "wamn_run_executions"), 9.0);
        // Multi-label filters isolate this proof's failed series.
        assert_eq!(
            labels_sum(
                text,
                "wamn_run_executions",
                &[tenant, project, "outcome=\"failed\""]
            ),
            1.0
        );
        // A distinct family matched exactly.
        assert_eq!(
            labels_sum(text, "wamn_run_drive_duration_ms_count", &[tenant, project]),
            9.0
        );
        // A live cluster can carry unrelated project backlog; the gate must
        // isolate both the ephemeral metricbench tenant and project.
        assert_eq!(
            labels_sum(text, "wamn_run_queue_depth", &[tenant, project]),
            6.0
        );
        assert!(present_with_labels(
            text,
            "wamn_run_executions",
            &[tenant, project]
        ));
        let instance = series_label_fragment(
            text,
            "wamn_run_executions",
            &[tenant, project, "outcome=\"failed\""],
            "instance",
        )
        .expect("executor series must carry an instance label");
        assert_eq!(instance, "instance=\"metric-process\"");
        assert_eq!(
            labels_sum(text, "wamn_postgres_pool_size", &[&instance]),
            1.0
        );
        assert_eq!(
            labels_sum(text, "wamn_postgres_query_duration_ms_count", &[&instance]),
            9.0
        );
        // Label value read (high-water = the allowed 32 MiB, not a budget).
        assert_eq!(
            label_value(
                text,
                "wamn_memory_high_water_bytes",
                "component=\"metricbench-memhog\""
            ),
            Some(33554432.0)
        );
        assert!(present(text, "wamn_memory_high_water_bytes"));
        assert!(!present(text, "wamn_api_requests"));
    }

    #[test]
    fn chunked_body_is_reassembled() {
        // "wamn_x 1\n" split into two chunks.
        let framed = "8\r\nwamn_x 1\r\n0\r\n\r\n";
        assert_eq!(dechunk(framed), "wamn_x 1");
    }
}
