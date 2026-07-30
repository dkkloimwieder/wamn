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
//!   1. drive N runs incl. exactly one forced failure -> `wamn_run_executions`
//!      grows by N and carries an `outcome="failed"` series (success ratio);
//!   2. seed a queue then run a dispatcher tick -> `wamn_run_queue_depth` > 0,
//!      then drain -> back to 0;
//!   3. `wamn_run_drive_duration_ms_count` > 0 (a real per-drive histogram);
//!   4. force a memory-limiter denial -> `wamn_memory_denied` > 0 and
//!      `wamn_memory_high_water_bytes` reads the ALLOWED size, not the budget;
//!   5. the run drives' own DB calls surface `wamn_postgres_pool_size` and
//!      `wamn_postgres_query_duration_ms_count` > 0;
//!   6. M api-gateway calls -> `wamn_api_requests` (the fork's inbound HTTP
//!      counter) — IN-CLUSTER ONLY (ProxyPre benches bypass the host's HTTP
//!      server), honest-skipped locally.
//!
//! Local recipe (docs/metrics.md): the tracebench docker collector +
//! otelcol-local's new metrics pipeline + a throwaway Postgres, with
//! `OTEL_METRIC_EXPORT_INTERVAL=1000` so the periodic reader does not wait a
//! minute. In-cluster gate of record: `deploy/gates/metricbench-job.yaml`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{Artifact, NodeImplementation};
use wamn_flow::Flow;
use wamn_node_manifest::{RecoveryClass, ResolvedNodeInterface, ResolvedPurity};
use wamn_run_state::queue::{enqueue_sql, write_ahead_triggered_run_sql};

use crate::dispatcher_process::{DispatcherProcess, ProjectSpec};
use wamn_execution_host::{ExecutionHost, production_capabilities};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::memory_metrics::global_memory_meter;
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wash_runtime::engine::ctx::WamnStoreLimiter;
use wash_runtime::wasmtime::ResourceLimiter as _;

/// The metricbench run-plane schema inside its throwaway database.
const SCHEMA: &str = "wamn_metricbench";
const TENANT: &str = "metric-tenant";
const TENANT_METRIC_LABEL: &str = "wamn_tenant=\"metric-tenant\"";
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
}

fn interface(
    node_type: &str,
    purity: ResolvedPurity,
    recovery_class: RecoveryClass,
) -> NodeImplementation {
    NodeImplementation::platform(ResolvedNodeInterface {
        node_type: node_type.to_string(),
        output_ports: vec!["main".to_string()],
        purity,
        recovery_class,
    })
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
    })
}

fn fixture_artifacts() -> anyhow::Result<Vec<FixtureArtifact>> {
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
                interface("transform", ResolvedPurity::Pure, RecoveryClass::Replay),
            ],
        )?,
        fixture_artifact(
            &fail_flow_json(),
            vec![interface(
                "postgres-query",
                ResolvedPurity::Effectful,
                RecoveryClass::NeverReplay,
            )],
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
                        artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests) \
                     VALUES ($1,$2,1,'0.1',$3::text::jsonb,$4,$5,$6,$7,$8)",
                    &[
                        &TENANT,
                        &artifact.flow_id,
                        &artifact.graph_json,
                        &artifact.graph_hash,
                        &artifact.artifact_hash,
                        &artifact.interface_bundle_json,
                        &artifact.interface_bundle_hash,
                        &artifact.component_digests,
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
    let catalog_version = i64::from(CATALOG_VERSION);
    let pinned = tx
        .execute(
            "UPDATE runs \
                SET catalog_id = $2, catalog_version = $3, environment = 'metricbench' \
              WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1",
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

    let guest = std::fs::read(&args.flowrunner)
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
    let metric_app_url = database_url(&app_url, &database)?;
    let metric_admin_url = database_url(&admin_url, &database)?;
    create_database(&admin_url, &database).await?;

    let result = async {
        println!(
            "# wamn-gates [9.8] metricbench (database {database}, schema {SCHEMA}, tenant {TENANT})"
        );
        println!("metrics = {}", args.metrics_url);
        provision(&metric_admin_url)
            .await
            .context("provision hermetic catalog and run plane")?;

        let mut cfg = WamnPostgresConfig::from_env();
        cfg.database_url = Some(metric_app_url.clone());
        let plugin = Arc::new(WamnPostgres::new(cfg)?);
        // [9.8-5] pool-saturation gauges over the runner's own project pool.
        plugin.register_pool_metrics();

        let engine = build_engine(&[])?;
        let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

        let outcome = async {
            let (mut seed_conn, _h) = connect_app(&metric_app_url).await?;

            // The production runner (registers wamn.run.* on instantiate).
            let vault = Arc::new(wamn_runtime::plugins::wamn_credentials::WamnCredentials::empty());
            let logging = Arc::new(wamn_runtime::plugins::wamn_logging::WamnLogging::from_env()?);
            let mut worker = ExecutionHost::instantiate(
                &engine,
                &guest,
                plugin.clone(),
                vault,
                logging,
                wamn_execution_host::ExecutionIdentity {
                    owner: OWNER,
                    tenant: TENANT,
                    schema: Some(SCHEMA),
                    project: "default",
                },
                production_capabilities(
                    Arc::from([]),
                    Arc::new(RunnerEgressPolicy::default()),
                ),
                30_000,
            )
            .await?;

            let mut pass = true;

        // === (1) executions counter + success ratio =========================
        let n = args.runs;
        let base_exec = scrape_sum(&args.metrics_url, "wamn_run_executions").await;
        let base_failed = scrape_where(&args.metrics_url, "wamn_run_executions", "outcome=\"failed\"")
            .await;
        for i in 0..n {
            seed_run(&mut seed_conn, &format!("mb-{i}"), FLOW_ID).await?;
        }
        seed_run(&mut seed_conn, "mb-fail", FAIL_FLOW_ID).await?;
        let r1 = worker.drain().await?;
        // Local sanity on the drive itself before waiting on the export.
        let drove_ok = r1.claimed == n + 1 && r1.completed == n && r1.failed == 1;
        // Delta == N+1 executions, with at least one `failed` (the mutant target:
        // an outcome-fold would keep failed at its baseline).
        let want_total = (n + 1) as f64;
        let (exec_ok, (exec_total, failed_delta)) = poll(&args.metrics_url, |text| {
            let total = family_sum(text, "wamn_run_executions") - base_exec;
            let failed = label_sum(text, "wamn_run_executions", "outcome=\"failed\"") - base_failed;
            (total >= want_total && failed >= 1.0, (total, failed))
        })
        .await;
        check(
            &mut pass,
            "(1) executions: delta == N+1 with a failed series",
            drove_ok && exec_ok,
            &format!(
                "drove claimed={}/{} completed={} failed={} ; scrape delta={exec_total} (want {want_total}), failed delta={failed_delta} (want >=1)",
                r1.claimed, n + 1, r1.completed, r1.failed
            ),
        );

        // === (3) run-drive duration histogram (same drives) =================
        let (dur_ok, dur_count) = poll(&args.metrics_url, |text| {
            let c = family_sum(text, "wamn_run_drive_duration_ms_count");
            (c > 0.0, c)
        })
        .await;
        check(
            &mut pass,
            "(3) run-drive duration histogram count > 0",
            dur_ok,
            &format!("wamn_run_drive_duration_ms_count = {dur_count}"),
        );

        // === (5) pool saturation + query latency (from the drives' DB writes) =
        let (pool_ok, pool_size) = poll(&args.metrics_url, |text| {
            let present = present(text, "wamn_postgres_pool_size")
                && family_sum(text, "wamn_postgres_query_duration_ms_count") > 0.0;
            (present, family_sum(text, "wamn_postgres_pool_size"))
        })
        .await;
        check(
            &mut pass,
            "(5) postgres pool gauge present + query-latency count > 0",
            pool_ok,
            &format!(
                "wamn_postgres_pool_size present={} size={pool_size}, query_count={}",
                present_now(&args.metrics_url, "wamn_postgres_pool_size").await,
                scrape_sum(&args.metrics_url, "wamn_postgres_query_duration_ms_count").await
            ),
        );

        // === (2) run-queue depth via the dispatcher tick ====================
        // The real dispatcher executable owns the gauge and samples it during
        // stepped sweeps. Seed a claimable batch, tick -> depth > 0; drain ->
        // tick -> depth back to 0. The claimable predicate is the mutant target.
        let specs = [ProjectSpec {
            name: "default".to_string(),
            url: metric_app_url.clone(),
            tenant: TENANT.to_string(),
            schema: Some(SCHEMA.to_string()),
        }];
        let mut dispatcher = DispatcherProcess::spawn(
            &specs,
            "nats://127.0.0.1:1",
            None,
            None,
            None,
            None,
        )?;
        let m = args.depth_seed;
        for i in 0..m {
            seed_run(&mut seed_conn, &format!("mq-{i}"), FLOW_ID).await?;
        }
        dispatcher
            .tick_project(0, chrono::Utc::now().timestamp_millis())
            .await?;
        let (depth_up_ok, depth_up) = poll(&args.metrics_url, |text| {
            let d = label_sum(
                text,
                "wamn_run_queue_depth",
                TENANT_METRIC_LABEL,
            );
            (d >= m as f64, d)
        })
        .await;
        // Drain the seeded batch, re-tick: the gauge must fall to 0.
        let r2 = worker.drain().await?;
        dispatcher
            .tick_project(0, chrono::Utc::now().timestamp_millis())
            .await?;
        let (depth_zero_ok, depth_zero) = poll(&args.metrics_url, |text| {
            let d = label_sum(
                text,
                "wamn_run_queue_depth",
                TENANT_METRIC_LABEL,
            );
            (d == 0.0, d)
        })
        .await;
        check(
            &mut pass,
            "(2) run_queue depth > 0 on a seeded queue, drains to 0",
            depth_up_ok && depth_zero_ok && r2.claimed == m,
            &format!(
                "seeded {m}: depth peaked {depth_up} (want >= {m}), after drain (claimed {}) depth {depth_zero} (want 0)",
                r2.claimed
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
        let queued = count(&seed_conn, &format!("SELECT count(*) FROM {SCHEMA}.run_queue")).await?;
        println!("queue drained fully = {}", queued == 0);

            anyhow::Ok(pass)
        }
        .await;

        ticker.abort();
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

async fn scrape_sum(url: &str, name: &str) -> f64 {
    family_sum(&fetch(url).await.unwrap_or_default(), name)
}

async fn scrape_where(url: &str, name: &str, label: &str) -> f64 {
    label_sum(&fetch(url).await.unwrap_or_default(), name, label)
}

async fn present_now(url: &str, name: &str) -> bool {
    present(&fetch(url).await.unwrap_or_default(), name)
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

/// Sum every series of `name` whose label set contains `label` (a raw
/// `key="value"` fragment).
fn label_sum(text: &str, name: &str, label: &str) -> f64 {
    text.lines()
        .filter(|l| !l.starts_with('#') && line_is(l, name) && l.contains(label))
        .filter_map(line_value)
        .sum()
}

/// The value of the first series of `name` carrying `label`, if any.
fn label_value(text: &str, name: &str, label: &str) -> Option<f64> {
    text.lines()
        .find(|l| !l.starts_with('#') && line_is(l, name) && l.contains(label))
        .and_then(line_value)
}

/// Whether any series of `name` is present.
fn present(text: &str, name: &str) -> bool {
    text.lines()
        .any(|l| !l.starts_with('#') && line_is(l, name))
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
            "CREATE TABLE catalog.flow_artifacts",
            "CREATE TABLE catalog.release_manifests",
            "CREATE TABLE catalog.release_flows",
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

    // The prometheus-text parser: exact family matching (never a `_created` or
    // `_bucket` sibling), label filtering, and value extraction — the pure bit
    // the scrape assertions stand on.
    #[test]
    fn prom_text_parse_family_label_present() {
        let text = "\
# HELP wamn_run_executions runs
# TYPE wamn_run_executions counter
wamn_run_executions{outcome=\"completed\",wamn_project=\"default\"} 8
wamn_run_executions{outcome=\"failed\",wamn_project=\"default\"} 1
wamn_run_executions_created{outcome=\"completed\"} 1.72e9
wamn_run_drive_duration_ms_count{wamn_project=\"default\"} 9
wamn_memory_high_water_bytes{component=\"metricbench-memhog\"} 33554432
wamn_run_queue_depth{wamn_project=\"default\",wamn_tenant=\"metric-tenant\"} 6
wamn_run_queue_depth{wamn_project=\"f1\",wamn_tenant=\"f1-tenant\"} 3619
";
        // Family sum ignores the `_created` sibling (its huge timestamp would
        // otherwise dominate) and the `_count` of a different family.
        assert_eq!(family_sum(text, "wamn_run_executions"), 9.0);
        // Label filter isolates the failed series.
        assert_eq!(
            label_sum(text, "wamn_run_executions", "outcome=\"failed\""),
            1.0
        );
        // A distinct family matched exactly.
        assert_eq!(family_sum(text, "wamn_run_drive_duration_ms_count"), 9.0);
        // A live cluster can carry unrelated project backlog; the gate must
        // isolate the ephemeral metricbench tenant instead of summing it.
        assert_eq!(
            label_sum(text, "wamn_run_queue_depth", TENANT_METRIC_LABEL),
            6.0
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
