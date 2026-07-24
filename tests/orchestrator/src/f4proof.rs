//! f4proof — the POC-F4 `disposition-recorded` row-event flow + 429-throttle
//! end-to-end proof (wamn-lxk), and the EVT-CUTOVER regression gate by
//! construction.
//!
//! F4 is the CDC row-event flow: an insert on `dispositions` fires the D19 v3
//! event plane (reader → JetStream → materializer → run queue, run id
//! `disposition-recorded:evt:<stream_seq>`); the run invokes the SHIPPED F2
//! disposition node over a serve-node HTTP hop, then POSTs an ERP callback
//! carrying an idempotency key. The ERP simulator returns `429 + Retry-After`
//! on demand, and the gate asserts the queue-park backoff produces NO stampede.
//!
//! Nothing here is taped: ONE `INSERT INTO dispositions` under the tenant claim
//! is the SOLE stimulus, and it drives the REAL reader (`run_with_token`), the
//! REAL materializer guest (`materializer.wasm`), the REAL production runner
//! (`RunWorker` driving `flowrunner.wasm`), a REAL serve-node hosting the REAL
//! `disposition-node.wasm`, and the ERP simulator — over a throwaway
//! `wal_level=logical` Postgres + a throwaway JetStream. Because the whole
//! reader→materializer→queue→runner arc runs from a real WAL event, this gate IS
//! the EVT-CUTOVER regression.
//!
//! Phases:
//!   1. materialize — one insert → the reader publishes one EVT → the
//!      materializer enqueues `disposition-recorded:evt:<seq>` (padded id, REAL
//!      stream_seq, trigger_source `evt:<seq>`).
//!   2. throttle — drain: shape → F2 recommend (serve-node hop) → ERP callback.
//!      The ERP sim 429s the first request, so the run PARKS: `available_at`
//!      pushed by the Retry-After horizon, lease released, and an IMMEDIATE
//!      re-drain claims NOTHING. After the horizon a re-drain retries the
//!      callback under the SAME Idempotency-Key → one effective 202. The F2 node
//!      ran once; the ERP ledger shows K 429s then exactly ONE delivery.
//!   3. no-stampede — N concurrent dispositions all park on their first 429 (one
//!      claim each, no thrash), all complete after the horizon, and the ERP
//!      ledger shows exactly ONE delivery per key (no duplicate side effect).
//!   4. redelivery — force a JetStream redeliver: ZERO new runs (ON CONFLICT).
//!
//! Needs: `--admin-database-url` (SUPERUSER on a `wal_level=logical` PG — the
//! gate owns the throwaway `wamn_f4proof` db, slot, and role), `--nats-url`
//! (JetStream). The runnable gate flow declares NO credential on the callback
//! node (the ERP sim needs no auth), keeping the vault out of the throttle
//! proof; the design fixture `f4-disposition-recorded.flow.json` documents the
//! credentialed shape. Recipe: docs/build-and-test.md [POC-F4].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use pg_walstream::CancellationToken;
use tokio_postgres::NoTls;

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use wamn_cdc_reader::{EventReaderArgs, run_with_token};
use wamn_ddl::{Confirmation, Migration};
use wamn_gate_harness::check;
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_credentials::WamnCredentials;
use wamn_host::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};
use wamn_host::plugins::wamn_logging::WamnLogging;
use wamn_host::plugins::wamn_postgres::{self, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig};
use wamn_host::serve_node::{self, ServeNode, ServeNodeAuthn};
use wamn_provision::{cdc_object_name, event_stream_name, sql as provision_sql};
use wamn_registry::sql::{
    upsert_event_reader_sql, upsert_org_sql, upsert_project_env_sql, upsert_project_sql,
};
use wamn_run_queue::mint_evt_run_id;
use wamn_run_worker::{RunWorker, RunnerIdentity};

use crate::erp_sim::ErpAudit;

#[derive(Debug, Args)]
pub struct F4ProofArgs {
    /// The compiled materializer component (`materializer.wasm`).
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// The flowrunner guest (`flowrunner.wasm`) the runner drives.
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// The compiled zero-import disposition node (`disposition-node.wasm`).
    #[arg(long, default_value = "/bench/disposition-node.wasm")]
    pub node: PathBuf,

    /// SUPERUSER URL (path `/postgres`) on a `wal_level=logical` Postgres — the
    /// gate owns the throwaway `wamn_f4proof` database, slot, and role.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// JetStream-enabled NATS (the reader's EVT stream + the doorbell).
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub nats_url: String,

    /// Loopback port the serve-node HTTP host binds (the runner→node hop).
    #[arg(long, default_value_t = 8191)]
    pub node_port: u16,

    /// Loopback port the ERP callback simulator binds.
    #[arg(long, default_value_t = 8192)]
    pub erp_port: u16,

    /// How many requests per idempotency key the ERP sim 429s before accepting.
    #[arg(long, default_value_t = 1)]
    pub fail_first_n: u32,

    /// The `Retry-After` (integer seconds) the ERP sim sends with each 429.
    #[arg(long, default_value_t = 2)]
    pub retry_after_secs: u64,

    /// Concurrent dispositions for the no-stampede phase.
    #[arg(long, default_value_t = 3)]
    pub concurrent: usize,
}

const BENCH_ID: &str = "f4proof";
const DB: &str = "wamn_f4proof";
const ORG: &str = "f4";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "t1";
const CDC_PW: &str = "wamn_cdc_pw";
const ENTITY_ID: &str = "dispositions";
const TABLE: &str = "dispositions";
const CATALOG_ID: &str = "f4cat";
const FLOW_ID: &str = "disposition-recorded";
const REG_ID: &str = "r-disp";

// The REAL shipped DDL, compiled in — the gate cannot drift from deploy/sql.
const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The gate catalog: one entity (`dispositions`) whose fields carry the F2
/// node's decimal inputs as exact-decimal STRINGS (the no-float rule — text
/// columns so the CDC new image serializes them as JSON strings the node reads
/// directly). `id`/`tenant_id` are the managed floor.
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "f4cat",
  "version": 1,
  "entities": [
    { "id": "dispositions", "name": "dispositions", "fields": [
      { "id": "material", "name": "material", "type": { "kind": "text" } },
      { "id": "moisture_pct", "name": "moisture_pct", "type": { "kind": "text" } },
      { "id": "moisture_max_pct", "name": "moisture_max_pct", "type": { "kind": "text" } }
    ] }
  ]
}"#;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("f4proof catalog parse: {e}"))
}

/// The runnable F4 flow (f3proof programmatic-JSON pattern): row-event trigger,
/// `shape` reshapes the event payload to the F2 node's `{hold: …}` contract,
/// `recommend` invokes the F2 node over the serve-node hop, and `callback` POSTs
/// the recommendation to the ERP sim with `idempotency-key: true`. NO credential
/// on the callback (the sim needs no auth — the vault is out of the throttle
/// proof); `allowed-hosts` admits both loopback hops.
fn gate_flow_json(node_port: u16, erp_port: u16) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "flow-id": FLOW_ID,
        "version": 1,
        "name": "F4 disposition-recorded (gate)",
        "trigger": { "type": "row-event", "table": TABLE, "event": "insert" },
        "entry": "shape",
        "nodes": [
            { "id": "shape", "type": "transform", "config": { "expression": "{hold: payload}" } },
            { "id": "recommend", "type": "custom", "label": "F2 disposition recommendation",
              "config": { "endpoint": format!("http://127.0.0.1:{node_port}") } },
            { "id": "callback", "type": "http-request", "label": "POST ERP callback",
              "config": { "method": "POST", "url": format!("http://127.0.0.1:{erp_port}/dispositions"),
                          "body": "@", "idempotency-key": true } }
        ],
        "edges": [
            { "from": "shape", "to": "recommend" },
            { "from": "recommend", "to": "callback" }
        ],
        "allowed-hosts": [format!("127.0.0.1:{node_port}"), format!("127.0.0.1:{erp_port}")],
    })
    .to_string()
}

/// The insert-only registration on `dispositions` — NO old-image condition, so
/// NO REPLICA IDENTITY FULL is derived (the RI reconcile is a no-op, asserted).
fn registration_json() -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "registration-id": REG_ID,
        "catalog-id": CATALOG_ID,
        "flow-id": FLOW_ID,
        "entity": ENTITY_ID,
        "ops": ["insert"],
        "condition": null,
        "partition-key": null,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Small helpers (the reader-live-gate idioms, from rie2ebench)
// ---------------------------------------------------------------------------

fn swap_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has a path");
    format!("{base}/{db}")
}

fn role_url(super_url: &str, role: &str, pw: &str) -> String {
    let after_scheme = super_url.strip_prefix("postgres://").expect("postgres://");
    let (_, host_and_path) = after_scheme.rsplit_once('@').expect("url has userinfo");
    let (host_port, _) = host_and_path.split_once('/').expect("url has a path");
    format!("postgres://{role}:{pw}@{host_port}/{DB}")
}

async fn connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connect {url}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(client)
}

fn counter(report: &serde_json::Value, key: &str) -> i64 {
    report.get(key).and_then(|v| v.as_i64()).unwrap_or(-1)
}

async fn scalar(db: &tokio_postgres::Client, sql: &str) -> anyhow::Result<i64> {
    Ok(db
        .query_one(sql, &[])
        .await
        .with_context(|| sql.to_string())?
        .get(0))
}

/// Insert one disposition (superuser, explicit tenant — the sole stimulus,
/// captured because it lands AFTER the slot). Returns its uuid.
async fn insert_disposition(
    db: &tokio_postgres::Client,
    material: &str,
    moisture: &str,
    max: &str,
) -> anyhow::Result<String> {
    Ok(db
        .query_one(
            "INSERT INTO app.dispositions (tenant_id, material, moisture_pct, moisture_max_pct) \
             VALUES ($1, $2, $3, $4) RETURNING id::text",
            &[&TENANT, &material, &moisture, &max],
        )
        .await
        .context("insert disposition stimulus")?
        .get(0))
}

/// Wait for the EVT stream to hold `want` messages; returns the depth.
async fn wait_stream_count(
    js: &async_nats::jetstream::Context,
    name: &str,
    want: u64,
    secs: u64,
) -> anyhow::Result<u64> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let have = match js.get_stream(name).await {
            Ok(mut s) => s.info().await.map(|i| i.state.messages).unwrap_or(0),
            Err(_) => 0,
        };
        if have >= want {
            return Ok(have);
        }
        if Instant::now() > deadline {
            bail!("stream {name} holds {have}/{want} after {secs}s");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The `available_at`-vs-now horizon (seconds) + lease owner of a queue row.
async fn queue_park_state(
    db: &tokio_postgres::Client,
    run_id: &str,
) -> anyhow::Result<Option<(f64, Option<String>)>> {
    Ok(db
        .query_opt(
            "SELECT EXTRACT(EPOCH FROM (available_at - now()))::float8, lease_owner \
             FROM wamn_run.run_queue WHERE tenant_id = 't1' AND run_id = $1",
            &[&run_id],
        )
        .await?
        .map(|r| (r.get(0), r.get(1))))
}

// ---------------------------------------------------------------------------
// The materializer guest harness (the rie2ebench/matbench shape)
// ---------------------------------------------------------------------------

struct MatHarness {
    engine: wash_runtime::engine::Engine,
    pre: CommandPre<SharedCtx>,
    pg: Arc<WamnPostgres>,
    js: Arc<WamnJetstream>,
    report_dir: PathBuf,
    stream_name: String,
}

impl MatHarness {
    fn plugin_map(
        &self,
    ) -> std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
        let mut m: std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> =
            std::collections::HashMap::new();
        m.insert(WAMN_POSTGRES_ID, self.pg.clone());
        m.insert(WAMN_JETSTREAM_ID, self.js.clone());
        m
    }

    async fn run_guest(&self, max_sweeps: u64, batch: u32) -> anyhow::Result<serde_json::Value> {
        let report_path = self.report_dir.join("counters.json");
        let _ = std::fs::remove_file(&report_path);

        let mut wasi = WasiCtxBuilder::new();
        wasi.args(&["materializer.wasm"])
            .inherit_stdout()
            .inherit_stderr()
            .envs(&[
                ("WAMN_MAT_STREAM", self.stream_name.as_str()),
                ("WAMN_MAT_ORG", ORG),
                ("WAMN_MAT_PROJECT", PROJECT),
                ("WAMN_MAT_ENV", ENV),
                ("WAMN_MAT_TENANT", TENANT),
                ("WAMN_MAT_BATCH", &batch.to_string()),
                ("WAMN_MAT_FETCH_MS", "1500"),
                ("WAMN_MAT_SWEEP_MS", "200"),
                ("WAMN_MAT_MAX_SWEEPS", &max_sweeps.to_string()),
                ("WAMN_MAT_ACK_WAIT_MS", "30000"),
                ("WAMN_MAT_NACK_DELAY_MS", "500"),
                ("WAMN_MAT_REPORT_PATH", "/report/counters.json"),
            ])
            .preopened_dir(
                &self.report_dir,
                "/report",
                DirPerms::all(),
                FilePerms::all(),
            )
            .map_err(|e| anyhow::anyhow!("preopen report dir: {e}"))?;

        let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(self.plugin_map())
            .with_wasi_ctx(wasi.build())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);

        let cmd = self
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| anyhow::anyhow!("instantiate materializer: {e}"))?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(120),
            cmd.wasi_cli_run().call_run(&mut store),
        )
        .await
        .context("materializer run deadline (120s) exceeded")?
        .map_err(|e| anyhow::anyhow!("materializer run trapped: {e}"))?;
        if outcome.is_err() {
            bail!("materializer exited with error status");
        }

        let raw = std::fs::read_to_string(&report_path)
            .with_context(|| format!("read guest report {}", report_path.display()))?;
        serde_json::from_str(&raw).context("parse guest report")
    }
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

pub async fn run(args: F4ProofArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates f4proof (wamn-lxk POC-F4 — CDC row-event flow + 429 throttle)");

    let mat_wasm = std::fs::read(&args.component)
        .with_context(|| format!("read {}", args.component.display()))?;
    let flowrunner = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read {}", args.flowrunner.display()))?;
    let node_wasm =
        std::fs::read(&args.node).with_context(|| format!("read {}", args.node.display()))?;

    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let stream_name = event_stream_name(ORG, ENV);

    // --- hermetic preamble --------------------------------------------------
    let admin = connect(&args.admin_database_url).await?;
    let _ = admin
        .execute(
            "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
             WHERE slot_name = $1 AND active",
            &[&cdc_name],
        )
        .await;
    let _ = admin
        .execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
             WHERE slot_name = $1",
            &[&cdc_name],
        )
        .await;
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {DB} WITH (FORCE)"))
        .await
        .context("drop leftover db")?;
    admin
        .batch_execute(&format!("DROP ROLE IF EXISTS {cdc_name}"))
        .await
        .context("drop leftover role")?;
    admin
        .batch_execute(&format!("CREATE DATABASE {DB}"))
        .await
        .context("create db")?;

    // --- the REAL substrate: shipped DDL + real builders --------------------
    let db = connect(&swap_db(&args.admin_database_url, DB)).await?;
    db.batch_execute(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
         THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;\n\
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
         THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
           NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;",
    )
    .await
    .context("roles")?;
    for (name, ddl) in [
        ("system-schema.sql", SYSTEM_SQL),
        ("run-state.sql", RUN_STATE_SQL),
        ("run-queue.sql", RUN_QUEUE_SQL),
        ("flows.sql", FLOWS_SQL),
        ("catalog-schema.sql", CATALOG_SQL),
    ] {
        db.batch_execute(ddl)
            .await
            .with_context(|| format!("apply deploy/sql/{name}"))?;
    }
    println!("provisioned {DB} from deploy/sql (include_str! — drift-proof)");

    // Registry rows (the reader reads its registration from here).
    db.execute(upsert_org_sql(), &[&ORG, &"pooled", &"wamn-pg"])
        .await
        .context("org row")?;
    db.execute(upsert_project_sql(), &[&ORG, &PROJECT])
        .await
        .context("project row")?;
    db.execute(
        wamn_registry::sql::stamp_env_policy_sql(),
        &[
            &ORG,
            &ENV,
            &r#"{"kind":"pool"}"#,
            &0i32,
            &1i32,
            &"1Gi",
            &"250m",
            &"256Mi",
            &"postgres:18",
            &"",
            &"",
            &"off",
        ],
    )
    .await
    .context("env-policy row")?;
    db.execute(
        upsert_project_env_sql(),
        &[&ORG, &PROJECT, &ENV, &"wamn-db-f4--app--dev", &None::<&str>],
    )
    .await
    .context("project-env row")?;
    let secret = format!("wamn-cdc-{ORG}--{PROJECT}--{ENV}");
    db.execute(
        upsert_event_reader_sql(),
        &[
            &ORG,
            &PROJECT,
            &ENV,
            &cdc_name,
            &cdc_name,
            &stream_name,
            &secret,
            &None::<&str>,
            &true,
        ],
    )
    .await
    .context("event_readers row")?;

    // The app floor (the REAL 3.2 DDL) + CDC.
    db.batch_execute(&provision_sql::ensure_schema_sql("app"))
        .await
        .context("app schema")?;
    let floor = Migration::create(&catalog()?)
        .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
        .sql(Confirmation::None)
        .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;
    db.batch_execute(&format!("SET search_path TO app; {floor}"))
        .await
        .context("apply the 3.2 floor")?;
    db.batch_execute("SET search_path TO public")
        .await
        .context("reset search_path")?;
    db.batch_execute(&provision_sql::ensure_replication_role_sql(
        &cdc_name, CDC_PW,
    ))
    .await
    .context("replication role")?;
    db.batch_execute(&provision_sql::create_publication_sql(&cdc_name, "app"))
        .await
        .context("publication")?;
    db.batch_execute(&provision_sql::ensure_entity_map_sql("app"))
        .await
        .context("entity map")?;
    db.execute(
        &provision_sql::upsert_entity_map_sql("app"),
        &[&ENTITY_ID, &TABLE],
    )
    .await
    .context("map dispositions entity -> table")?;
    db.batch_execute(&provision_sql::grant_replication_access_sql(
        DB, &cdc_name, "app",
    ))
    .await
    .context("grants")?;

    // The flow + the insert registration.
    db.execute(
        "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
         VALUES ($1, $2, 1, true, $3::text::jsonb)",
        &[
            &TENANT,
            &FLOW_ID,
            &gate_flow_json(args.node_port, args.erp_port),
        ],
    )
    .await
    .context("seed flow")?;
    db.execute(
        "INSERT INTO catalog.event_registrations \
         (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ($1, $2, $3, $4, $5, $6::text::jsonb)",
        &[
            &TENANT,
            &CATALOG_ID,
            &REG_ID,
            &FLOW_ID,
            &ENTITY_ID,
            &registration_json(),
        ],
    )
    .await
    .context("seed registration")?;
    println!("seeded flow {FLOW_ID} + insert registration {REG_ID} on entity {ENTITY_ID}");

    // The insert-only subscription needs NO REPLICA IDENTITY FULL — the RI
    // reconcile must be a NO-OP (nothing to flip). Asserted below.
    let ri_plan =
        wamn_ctl::reconcile_replica_identity::reconcile(&db, &catalog()?, "app", true).await?;

    // --- NATS + the reader --------------------------------------------------
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats.clone());
    let _ = js.delete_stream(&stream_name).await;

    // The slot LAST (capture starts here — provisioning + seed writes stay out).
    db.batch_execute(&provision_sql::create_failover_slot_sql(&cdc_name))
        .await
        .context("create failover slot")?;

    let token = CancellationToken::new();
    let reader = tokio::spawn(run_with_token(
        EventReaderArgs {
            org: ORG.into(),
            project: PROJECT.into(),
            env: ENV.into(),
            system_database_url: swap_db(&args.admin_database_url, DB),
            cdc_url: role_url(&args.admin_database_url, &cdc_name, CDC_PW),
            nats_url: args.nats_url.clone(),
            sslmode: "disable".into(),
            stream_replicas: 1,
            dup_window_secs: 120,
            feedback_secs: 1,
            stall_threshold_secs: 30,
            slot_poll_secs: 0,
            slot_safe_wal_warn_bytes: 268_435_456,
        },
        token.clone(),
    ));
    println!("reader up (one pg_walstream session -> {stream_name})");

    // --- the materializer harness (rie2ebench shape) ------------------------
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let app_url = role_url(&args.admin_database_url, "wamn_app", "wamn_app");
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.clone());
    let mat_pg = Arc::new(WamnPostgres::new(cfg)?);
    mat_pg.set_tenant(BENCH_ID, TENANT)?;
    mat_pg.set_schema(BENCH_ID, "wamn_run")?;
    mat_pg
        .probe_checkout()
        .await
        .context("materializer postgres preflight")?;
    let mat_js = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(args.nats_url.clone()),
        })
        .with_doorbell(nats.clone()),
    );
    mat_js.set_tenant(BENCH_ID, TENANT)?;
    let raw: &RawEngine = engine.inner();
    let mat_component = WasmtimeComponent::new(raw, &mat_wasm)
        .map_err(|e| anyhow::anyhow!("compile materializer: {e}"))?;
    let mut mat_linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut mat_linker)?;
    wamn_postgres::add_to_linker(&mut mat_linker)?;
    wamn_jetstream::add_to_linker(&mut mat_linker)?;
    let mat_pre = CommandPre::new(mat_linker.instantiate_pre(&mat_component)?)?;
    let report_dir = std::env::temp_dir().join(format!("wamn-f4proof-{}", std::process::id()));
    std::fs::create_dir_all(&report_dir).context("create report dir")?;
    let harness = MatHarness {
        engine: engine.clone(),
        pre: mat_pre,
        pg: mat_pg,
        js: mat_js,
        report_dir: report_dir.clone(),
        stream_name: stream_name.clone(),
    };

    // --- the serve-node (keyless, network-trust) + the ERP simulator --------
    let serve = Arc::new(
        ServeNode::new(
            &engine,
            &node_wasm,
            Arc::new(WamnCredentials::empty()),
            serve_node::DEFAULT_NODE_ID,
            "default",
            Arc::from([]),
            ServeNodeAuthn {
                require_signing_key: false,
                max_signature_age_secs: None,
            },
        )
        .await
        .context("build serve-node (disposition node)")?,
    );
    let erp = ErpAudit::new(args.fail_first_n, args.retry_after_secs);
    let erp_task = tokio::spawn(crate::erp_sim::serve(erp.clone(), args.erp_port));

    // The production runner: empty vault (the gate flow declares no credential),
    // host allowlist admits BOTH loopback hops (serve-node + ERP sim).
    let mut runner_cfg = WamnPostgresConfig::from_env();
    runner_cfg.database_url = Some(app_url.clone());
    let runner_pg = Arc::new(WamnPostgres::new(runner_cfg)?);
    let allowed: Arc<[AllowedHost]> = vec![
        format!("127.0.0.1:{}", args.node_port).parse::<AllowedHost>()?,
        format!("127.0.0.1:{}", args.erp_port).parse::<AllowedHost>()?,
    ]
    .into();

    // Drive the gate while the serve-node accept loop runs on the SAME task
    // (select!): its wasmtime store is !Send, so it cannot be spawned. The
    // reader + ERP sim are Send and already spawned.
    let serve_loop = serve_node::serve(serve.clone(), args.node_port);
    let gate = gate_body(
        &engine,
        &flowrunner,
        runner_pg,
        allowed,
        &harness,
        &db,
        &js,
        &stream_name,
        &erp,
        &args,
        ri_plan.flips.len(),
    );

    let outcome = tokio::select! {
        r = serve_loop => r.map(|_| false),
        r = gate => r,
    };

    // --- teardown (zero residue) --------------------------------------------
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(15), reader).await;
    erp_task.abort();
    let _ = db
        .execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
             WHERE slot_name = $1",
            &[&cdc_name],
        )
        .await;
    let _ = js.delete_stream(&stream_name).await;
    drop(db);
    let _ = admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {DB} WITH (FORCE)"))
        .await;
    let _ = admin
        .batch_execute(&format!("DROP ROLE IF EXISTS {cdc_name}"))
        .await;
    let _ = std::fs::remove_dir_all(&report_dir);
    ticker.abort();

    let pass = outcome?;
    println!("\nf4proof complete — overall PASS: {pass}");
    if !pass {
        bail!("wamn-lxk f4proof gate failed");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn gate_body(
    engine: &wash_runtime::engine::Engine,
    flowrunner: &[u8],
    runner_pg: Arc<WamnPostgres>,
    allowed: Arc<[AllowedHost]>,
    harness: &MatHarness,
    db: &tokio_postgres::Client,
    js: &async_nats::jetstream::Context,
    stream_name: &str,
    erp: &ErpAudit,
    args: &F4ProofArgs,
    ri_flips: usize,
) -> anyhow::Result<bool> {
    let mut pass = true;

    // The production runner (RunWorker driving flowrunner.wasm).
    let mut worker = RunWorker::instantiate(
        engine,
        flowrunner,
        runner_pg,
        Arc::new(WamnCredentials::empty()),
        Arc::new(WamnLogging::from_env()?),
        RunnerIdentity {
            owner: BENCH_ID,
            tenant: TENANT,
            schema: Some("wamn_run"),
            project: "default",
        },
        allowed,
        30_000,
        None,
    )
    .await?;

    // Insert-only registration ⇒ RI reconcile is a no-op.
    check(
        &mut pass,
        &format!(
            "RI reconcile is a NO-OP for the insert-only subscription (0 flips, got {ri_flips})"
        ),
        ri_flips == 0,
    );

    // ========================================================================
    // Phase 1 — materialize: one insert -> one EVT -> one enqueued evt run.
    // ========================================================================
    println!("\n## phase 1: materialize (one INSERT -> reader -> materializer -> run queue)");
    let disp_id = insert_disposition(db, "resin-A", "12.00", "5.00").await?;
    let seq = wait_stream_count(js, stream_name, 1, 60).await?;
    check(
        &mut pass,
        &format!("the insert reached the EVT stream (1 event; disp {disp_id})"),
        seq == 1,
    );
    let report1 = harness.run_guest(4, 64).await?;
    println!("phase-1 materializer report: {report1}");

    let run_id = mint_evt_run_id(FLOW_ID, seq);
    let queued = scalar(
        db,
        &format!(
            "SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = 't1' \
             AND run_id = '{run_id}' AND stream_seq = {seq}"
        ),
    )
    .await?;
    let run_row = db
        .query_opt(
            "SELECT trigger_source, flow_id FROM wamn_run.runs WHERE tenant_id = 't1' AND run_id = $1",
            &[&run_id],
        )
        .await?;
    check(
        &mut pass,
        &format!(
            "materializer enqueued exactly-once run {run_id} (padded id, real stream_seq {seq})"
        ),
        queued == 1
            && run_row.as_ref().is_some_and(|r| {
                r.get::<_, String>(0) == format!("evt:{seq}") && r.get::<_, String>(1) == FLOW_ID
            }),
    );
    check(
        &mut pass,
        "materializer fired exactly one run",
        counter(&report1, "fired") == 1,
    );

    // ========================================================================
    // Phase 2 — throttle: drain -> F2 hop -> ERP callback 429 -> queue PARK.
    // ========================================================================
    println!("\n## phase 2: throttle (429 parks the run; no re-claim before the wake)");
    let r_first = worker.drain().await?;
    check(
        &mut pass,
        &format!(
            "the first drain PARKED the 429'd run (claimed {}, parked {}, completed {})",
            r_first.claimed, r_first.parked, r_first.completed
        ),
        r_first.claimed == 1 && r_first.parked == 1 && r_first.completed == 0,
    );

    // The F2 node ran once, on the serve-node hop, with a recommendation.
    let recommend_out: Option<String> = db
        .query_opt(
            "SELECT output_json::text FROM wamn_run.node_runs \
             WHERE tenant_id = 't1' AND run_id = $1 AND node_id = 'recommend'",
            &[&run_id],
        )
        .await?
        .map(|r| r.get(0));
    let recommended = recommend_out
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("recommended")
                .and_then(|x| x.as_str())
                .map(str::to_string)
        });
    check(
        &mut pass,
        &format!(
            "the F2 disposition node ran over the serve-node hop (recommended = {recommended:?}, want reject)"
        ),
        recommended.as_deref() == Some("reject"),
    );

    // The 429 parked the queue row: available_at pushed by the Retry-After
    // horizon, lease RELEASED.
    let park = queue_park_state(db, &run_id).await?;
    let horizon = park.as_ref().map(|(h, _)| *h).unwrap_or(-999.0);
    let lease = park.as_ref().and_then(|(_, o)| o.clone());
    check(
        &mut pass,
        &format!(
            "the 429 parked the queue row: available_at pushed ~{:.1}s ahead (want > 1s from Retry-After {}), lease released ({lease:?})",
            horizon, args.retry_after_secs
        ),
        horizon > 1.0 && lease.is_none(),
    );

    // The ERP ledger: one 429 for this key, no effective delivery yet.
    let key = format!("{run_id}:callback:0");
    let rec = erp.key(&key);
    check(
        &mut pass,
        &format!(
            "ERP saw exactly one 429 under the run's idempotency key, no delivery yet (requests {}, 429 {}, delivered {})",
            rec.requests, rec.rejected_429, rec.delivered
        ),
        rec.requests == 1 && rec.rejected_429 == 1 && rec.delivered == 0,
    );

    // IMMEDIATE re-drain: the parked run is NOT re-claimed before its wake.
    let r_early = worker.drain().await?;
    check(
        &mut pass,
        &format!(
            "an immediate re-drain claims NOTHING (no re-claim during backoff; claimed {})",
            r_early.claimed
        ),
        r_early.claimed == 0,
    );

    // Wait past the Retry-After horizon, then re-drain: the callback retries
    // under the SAME idempotency key and lands its one effective 202.
    tokio::time::sleep(Duration::from_millis(args.retry_after_secs * 1000 + 400)).await;
    let r_woke = worker.drain().await?;
    check(
        &mut pass,
        &format!(
            "after the wake the run COMPLETED (claimed {}, completed {})",
            r_woke.claimed, r_woke.completed
        ),
        r_woke.claimed == 1 && r_woke.completed == 1,
    );
    let run_status: String = db
        .query_one(
            "SELECT status FROM wamn_run.runs WHERE tenant_id = 't1' AND run_id = $1",
            &[&run_id],
        )
        .await?
        .get(0);
    let rec = erp.key(&key);
    check(
        &mut pass,
        &format!(
            "EXACTLY ONE effective ERP delivery after the backoff under the same key ({} 429s then {} delivery; run {run_status})",
            rec.rejected_429, rec.delivered
        ),
        run_status == "completed"
            && rec.rejected_429 == u64::from(args.fail_first_n)
            && rec.delivered == 1
            && rec.requests == u64::from(args.fail_first_n) + 1,
    );
    // The callback node recorded a successful terminal status (202) carrying the
    // Idempotency-Key — the header rode (a dropped header = no key on the ledger).
    check(
        &mut pass,
        "the ERP callback carried the run's Idempotency-Key header (the ledger keyed the run's id)",
        erp.distinct_keys() == 1 && erp.key(&key).delivered == 1,
    );

    // ========================================================================
    // Phase 3 — no-stampede: N concurrent dispositions all park, none thrash.
    // ========================================================================
    println!(
        "\n## phase 3: no-stampede ({} concurrent dispositions each park + retry once)",
        args.concurrent
    );
    let n = args.concurrent as u64;
    for i in 0..args.concurrent {
        insert_disposition(db, &format!("resin-{i}"), "12.00", "5.00").await?;
    }
    // seqs 2..=1+n now on the stream.
    let want = 1 + n;
    wait_stream_count(js, stream_name, want, 60).await?;
    let report3 = harness.run_guest(6, 64).await?;
    println!("phase-3 materializer report: {report3}");
    let evt_runs = scalar(
        db,
        "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' AND trigger_source LIKE 'evt:%'",
    )
    .await?;
    check(
        &mut pass,
        &format!("materializer enqueued all {n} concurrent evt runs (total evt runs {evt_runs})"),
        evt_runs == (1 + n) as i64,
    );

    // First drain: ALL N park on their first 429, ONE claim each (no thrash).
    let deliveries_before = erp.total_deliveries();
    let r_stampede = worker.drain().await?;
    check(
        &mut pass,
        &format!(
            "all {n} concurrent runs PARKED on the first drain, one claim each (claimed {}, parked {}, completed {})",
            r_stampede.claimed, r_stampede.parked, r_stampede.completed
        ),
        r_stampede.claimed == n as usize
            && r_stampede.parked == n as usize
            && r_stampede.completed == 0,
    );
    // No effective delivery landed during the backoff (all 429'd).
    check(
        &mut pass,
        &format!(
            "NO ERP delivery during the concurrent backoff (deliveries still {})",
            erp.total_deliveries()
        ),
        erp.total_deliveries() == deliveries_before,
    );
    // Immediate re-drain: claim-thrash bound — the parked set is not re-claimed.
    let r_thrash = worker.drain().await?;
    check(
        &mut pass,
        &format!(
            "an immediate re-drain claims NOTHING for the whole parked set (no thundering re-claim; claimed {})",
            r_thrash.claimed
        ),
        r_thrash.claimed == 0,
    );

    // Wake, drain: all N complete, each with EXACTLY ONE delivery (no dup).
    tokio::time::sleep(Duration::from_millis(args.retry_after_secs * 1000 + 400)).await;
    let mut woke_completed = 0usize;
    let mut woke_claims = 0usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let r = worker.drain().await?;
        woke_claims += r.claimed;
        woke_completed += r.completed;
        let outstanding = scalar(
            db,
            "SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = 't1'",
        )
        .await?;
        if outstanding == 0 {
            break;
        }
        if Instant::now() > deadline {
            bail!("no-stampede FAIL: {outstanding} runs still queued after 30s");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    check(
        &mut pass,
        &format!(
            "all {n} concurrent runs completed after the wake (completed {woke_completed}, claims {woke_claims})"
        ),
        woke_completed == n as usize,
    );
    // Exactly one effective delivery per key, N total new — no duplicate side
    // effect for any key despite the concurrent park/retry.
    let total_deliveries = erp.total_deliveries();
    let max_per_key_ok = (0..args.concurrent).all(|i| {
        let rid = mint_evt_run_id(FLOW_ID, 2 + i as u64);
        erp.key(&format!("{rid}:callback:0")).delivered == 1
    });
    check(
        &mut pass,
        &format!(
            "exactly ONE effective delivery per key, {} total ({}+{n}); no duplicate side effect for any key",
            total_deliveries, deliveries_before
        ),
        total_deliveries == deliveries_before + n && max_per_key_ok,
    );
    // Claim-thrash bound: the whole no-stampede phase claimed each run at most
    // twice (the park + the one wake retry) — N parked claims + N wake claims.
    check(
        &mut pass,
        &format!(
            "claim attempts bounded (no thrash): {} park-claims + {woke_claims} wake-claims for {n} runs (<= 2N)",
            r_stampede.claimed
        ),
        (r_stampede.claimed + woke_claims) <= (2 * n) as usize,
    );

    // ========================================================================
    // Phase 4 — redelivery: force a JetStream redeliver -> ZERO new runs.
    // ========================================================================
    println!(
        "\n## phase 4: redelivery (delete the durable consumer, re-run — ON CONFLICT exactly-once)"
    );
    let runs_before = scalar(
        db,
        "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1'",
    )
    .await?;
    let stream = js.get_stream(stream_name).await.context("get stream")?;
    let consumer = format!("mat_{TENANT}_{CATALOG_ID}_{REG_ID}");
    stream
        .delete_consumer(&consumer)
        .await
        .with_context(|| format!("delete durable {consumer} (must exist after phase 1)"))?;
    let report4 = harness.run_guest(6, 64).await?;
    let runs_after = scalar(
        db,
        "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1'",
    )
    .await?;
    check(
        &mut pass,
        &format!(
            "full redelivery minted ZERO new runs (before {runs_before}, after {runs_after}; ON CONFLICT)"
        ),
        runs_after == runs_before,
    );
    check(
        &mut pass,
        &format!(
            "redelivery collisions observed (duplicate counter {} > 0)",
            counter(&report4, "duplicate")
        ),
        counter(&report4, "duplicate") > 0,
    );

    Ok(pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate flow the proof registers is a real, valid F4 flow: row-event
    /// trigger on the insert, the `shape → recommend → callback` shape, the
    /// callback opts into idempotency, and both hops are declared egress. A
    /// malformed builder fails here at `cargo test`, before any live infra.
    #[test]
    fn gate_flow_is_a_valid_f4_flow() {
        use wamn_flow::{RowEvent, Trigger};
        let json = gate_flow_json(8191, 8192);
        let flow = wamn_flow::Flow::from_json(&json).expect("gate flow parses");
        flow.validate().expect("gate flow validates");
        assert_eq!(flow.flow_id, FLOW_ID);
        assert!(
            matches!(flow.trigger, Trigger::RowEvent { ref table, event: RowEvent::Insert } if table == TABLE),
            "F4 is a row-event insert trigger on dispositions"
        );
        assert_eq!(flow.entry, "shape");
        // The callback opts into the idempotency header (the exactly-once belt).
        let cb = flow
            .nodes
            .iter()
            .find(|n| n.id == "callback")
            .expect("callback node");
        assert_eq!(cb.config["idempotency-key"], serde_json::Value::Bool(true));
        assert_eq!(cb.config["method"], "POST");
        // Both hops are declared egress (fqg.11 fail-closed).
        assert_eq!(flow.allowed_hosts.len(), 2);
    }

    /// The registration is a frozen, insert-only EventRegistration — the op set
    /// that derives NO REPLICA IDENTITY FULL (the reconcile stays a no-op).
    #[test]
    fn registration_is_insert_only() {
        let reg = wamn_event_reg::EventRegistration::from_json(&registration_json())
            .expect("insert registration is a frozen EventRegistration");
        assert!(
            reg.ops
                .iter()
                .all(|op| format!("{op:?}").to_lowercase().contains("insert")),
            "the registration subscribes ONLY insert (no old-image ⇒ no RI FULL)"
        );
        assert!(
            reg.condition.is_none(),
            "no condition ⇒ no old image needed"
        );
    }

    /// The catalog names exactly the one entity → table the gate maps + inserts,
    /// carrying the F2 node's decimal-string inputs.
    #[test]
    fn catalog_names_the_dispositions_entity() {
        let cat = catalog().expect("catalog parses");
        assert_eq!(cat.catalog_id, CATALOG_ID);
        assert!(
            cat.entities
                .iter()
                .any(|e| e.id == ENTITY_ID && e.name == TABLE),
            "catalog must carry the dispositions entity"
        );
    }
}
