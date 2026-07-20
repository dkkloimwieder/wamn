//! The `rie2ebench` subcommand: the reader-inclusive REPLICA IDENTITY flip
//! end-to-end regression (wamn-3glr, [EVT-RI-E2E]).
//!
//! The coverage the l5i9.19 teardown deleted with `cutbench`'s phase 3: NO gate
//! proves a REAL decoded WAL old image reaches the materializer AFTER a live
//! REPLICA IDENTITY flip. `matbench` covers the old-image-absent refusal and a
//! SYNTHESIZED FULL old image (a hand-published tape); `ri_orch_live` covers the
//! ctl flip machinery on `pg_class.relreplident` — but neither drives a real
//! reader.
//!
//! This gate embeds the REAL `wamn-cdc-reader` service body
//! (`run_with_token`) as a tokio task next to the REAL materializer Service
//! guest (`materializer.wasm`, wasi:cli/run) — the matbench harness shape — over
//! a throwaway `wal_level=logical` Postgres and a throwaway JetStream. Nothing is
//! taped or synthesized between the DELETE and the materializer's verdict: the
//! reader decodes the WAL old image the table's replica identity actually
//! carries.
//!
//! ONE FULL-flipped entity (`dispositions`), ONE delete-subscribed flow
//! (`disp-del`). Floor-entity PKs are a bare `id uuid` (wamn-ddl `emit.rs`), so a
//! DELETE under REPLICA IDENTITY DEFAULT carries a key-only old image WITHOUT
//! `tenant_id`:
//!   1. pre-flip DELETE (RI DEFAULT) → the reader decodes a key-only old image →
//!      the materializer cannot tenant-scope it → an alertable
//!      `tenant-unscopable` REFUSAL (never condition-false, never a cross-tenant
//!      enqueue);
//!   2. flip RI → FULL via the REAL l5i9.31/l5i9.61 reconcile
//!      (`wamn_ctl::reconcile_replica_identity::reconcile`);
//!   3. post-flip DELETE (RI FULL) → the reader decodes a REAL FULL old image
//!      carrying `tenant_id` → the materializer tenant-scopes it and enqueues a
//!      scoped `disp-del:evt:<stream_seq>` run.
//!
//! The non-retroactive boundary: the pre-flip refusal STANDS (the flip enriches
//! only WAL written after it — it never retro-fires).
//!
//! Recipe facts honored (mined from the archived cutbench, git `f0cebca^`):
//!   * OWN throwaway Postgres WITH `wal_level=logical` + a throwaway JetStream —
//!     the shared fixture recipe does not apply; this gate owns its DB and slot.
//!   * provisioning order: the `wamn-provision` SQL builders + `wamn-registry`
//!     upserts FIRST, the replication slot LAST, so provisioning + seed writes
//!     stay UNCAPTURED and the stream holds ONLY the two deletes.
//!   * an idle `pg_walstream` flushes feedback only on the ~30s keepalive — the
//!     gate DRIVES traffic (the two deletes) and waits on the stream depth (the
//!     awaited `PublishAck` is the only NATS delivery truth), never on a timer.
//!   * jsonb/uuid params bind as `$n::text::jsonb` / `$n::text::uuid`.
//!   * the logical slot pins WAL until dropped — the teardown drops it
//!     deterministically (zero residue).
//!
//! Needs: `--admin-database-url` (SUPERUSER on a `wal_level=logical` PG — the
//! gate creates/drops the throwaway `wamn_rie2e` database, the slot, and the
//! replication role), `--nats-url` (JetStream). Recipe: docs/build-and-test.md
//! [EVT-RI-E2E].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use futures_util::StreamExt as _;
use pg_walstream::CancellationToken;
use tokio_postgres::NoTls;

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use wamn_cdc_reader::{EventReaderArgs, run_with_token};
use wamn_ddl::{Confirmation, Migration};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};
use wamn_host::plugins::wamn_postgres::{self, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig};
use wamn_provision::{cdc_object_name, event_stream_name, sql as provision_sql};
use wamn_registry::sql::{
    upsert_event_reader_sql, upsert_org_sql, upsert_project_env_sql, upsert_project_sql,
};
use wamn_run_queue::mint_evt_run_id;

#[derive(Debug, Args)]
pub struct Rie2eBenchArgs {
    /// The compiled materializer component.
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// SUPERUSER URL (path `/postgres`) on a `wal_level=logical` Postgres —
    /// the gate owns the throwaway `wamn_rie2e` database, slot, and role.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// JetStream-enabled NATS (the reader's EVT stream AND the doorbell).
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub nats_url: String,
}

const BENCH_ID: &str = "rie2ebench";
const DB: &str = "wamn_rie2e";
const ORG: &str = "rie";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "t1";
const CDC_PW: &str = "wamn_cdc_pw";
/// The entity ID deliberately differs from the table name (`dispositions`) so a
/// scoped fire proves the OID→entity map was consulted end to end.
const ENTITY_ID: &str = "evt_disp";
const TABLE: &str = "dispositions";
const CATALOG_ID: &str = "ricat";
/// The single delete-subscribed flow the RI flip makes cut-over-able.
const FLOW_ID: &str = "disp-del";
const REG_ID: &str = "r-del";

// The REAL shipped DDL, compiled in — the gate cannot drift from deploy/sql.
const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The fixture catalog: one entity (`evt_disp` → `dispositions`) with a single
/// text column. The floor's PK is the managed bare `id uuid` (wamn-ddl), so a
/// DELETE under RI DEFAULT carries a key-only old image (no `tenant_id`).
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "ricat",
  "version": 1,
  "entities": [
    { "id": "evt_disp", "name": "dispositions", "fields": [
      { "id": "site", "name": "site", "type": { "kind": "text" } }
    ] }
  ]
}"#;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("rie2ebench catalog parse: {e}"))
}

/// The delete-subscribed flow (manual trigger, one noop node — the materializer
/// fires on the REGISTRATION match, not the flow trigger; matbench proves it).
fn flow_json() -> String {
    serde_json::json!({
        "schema-version": "0.1", "flow-id": FLOW_ID, "version": 1,
        "trigger": {"type": "manual"},
        "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
    })
    .to_string()
}

/// A delete-only registration on `evt_disp`, no condition, no partition key.
/// State omitted → the served (live) path (matbench proves a stateless doc
/// fires); the RI reconcile derives FULL from the `delete` op subscription.
fn registration_json() -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "registration-id": REG_ID,
        "catalog-id": CATALOG_ID,
        "flow-id": FLOW_ID,
        "entity": ENTITY_ID,
        "ops": ["delete"],
        "condition": null,
        "partition-key": null,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Small helpers (the reader-live-gate idioms, from the archived cutbench)
// ---------------------------------------------------------------------------

/// Swap the database path segment of a libpq URL (the gate controls the URL —
/// no query string).
fn swap_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has a path");
    format!("{base}/{db}")
}

/// `host:port` with credentials swapped out → a role's plain URL into DB.
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

/// A table's `pg_class.relreplident` in the app data schema ('d'/'f'/'n'/'i').
async fn relreplident(db: &tokio_postgres::Client, table: &str) -> anyhow::Result<String> {
    Ok(db
        .query_one(
            "SELECT c.relreplident::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'app' AND c.relname = $1",
            &[&table],
        )
        .await
        .context("read relreplident")?
        .get(0))
}

/// Wait for the EVT stream to hold `want` messages (the reader is async; the
/// awaited PublishAck is the delivery truth — this polls the server-side depth).
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
            Err(_) => 0, // the reader may not have created it yet
        };
        if have >= want {
            return Ok(have);
        }
        if Instant::now() > deadline {
            bail!("stream {name} holds {have}/{want} after {secs}s");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// ---------------------------------------------------------------------------
// The guest harness (the matbench shape)
// ---------------------------------------------------------------------------

struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: CommandPre<SharedCtx>,
    pg: Arc<WamnPostgres>,
    js: Arc<WamnJetstream>,
    report_dir: PathBuf,
    stream_name: String,
}

impl Harness {
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

pub async fn run(args: Rie2eBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates rie2ebench (wamn-3glr EVT-RI-E2E — reader-inclusive RI flip)");

    let guest = std::fs::read(&args.component)
        .with_context(|| format!("read {}", args.component.display()))?;
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let stream_name = event_stream_name(ORG, ENV);

    // --- hermetic preamble (the reader-live-gate lesson: leftovers mask) ----
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
        &[
            &ORG,
            &PROJECT,
            &ENV,
            &"wamn-db-rie--app--dev",
            &None::<&str>,
        ],
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

    // The app floor (the REAL 3.2 DDL) + CDC — NO outbox triggers, NO dispatcher
    // (this gate is reader → materializer only).
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
    .context("map evt_disp -> dispositions")?;
    db.batch_execute(&provision_sql::grant_replication_access_sql(
        DB, &cdc_name, "app",
    ))
    .await
    .context("grants")?;

    // The flow + the delete registration.
    db.execute(
        "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
         VALUES ($1, $2, 1, true, $3::text::jsonb)",
        &[&TENANT, &FLOW_ID, &flow_json()],
    )
    .await
    .context("seed flow disp-del")?;
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
    .context("seed registration r-del")?;
    println!("seeded flow {FLOW_ID} + delete registration {REG_ID} on entity {ENTITY_ID}");

    // Two rows to delete — seeded BEFORE the slot (provisioning + seed writes
    // stay UNCAPTURED). The superuser bypasses RLS; the CHECK (tenant_id <> '')
    // is satisfied by the explicit tenant.
    let seed_row = |site: &str| {
        let db = &db;
        let site = site.to_string();
        async move {
            let id: String = db
                .query_one(
                    "INSERT INTO app.dispositions (tenant_id, site) VALUES ($1, $2) RETURNING id::text",
                    &[&TENANT, &site],
                )
                .await
                .context("seed disposition")?
                .get(0);
            anyhow::Ok::<String>(id)
        }
    };
    let pre_id = seed_row("pre-flip").await?;
    let post_id = seed_row("post-flip").await?;
    println!("seeded 2 dispositions (pre={pre_id}, post={post_id}) BEFORE the slot");

    // --- NATS + the doorbell observer + the reader --------------------------
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats.clone());
    let _ = js.delete_stream(&stream_name).await;
    let mut bells = nats
        .subscribe(format!("wamn.doorbell.{TENANT}"))
        .await
        .context("subscribe doorbell")?;

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

    // --- plugins + engine (the matbench harness) ----------------------------
    let app_url = role_url(&args.admin_database_url, "wamn_app", "wamn_app");
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.clone());
    let pg = Arc::new(WamnPostgres::new(cfg)?);
    pg.set_tenant(BENCH_ID, TENANT)?;
    pg.set_schema(BENCH_ID, "wamn_run")?;
    pg.probe_checkout().await.context("postgres preflight")?;
    let jsp = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(args.nats_url.clone()),
        })
        .with_doorbell(nats.clone()),
    );
    jsp.set_tenant(BENCH_ID, TENANT)?;
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let raw: &RawEngine = engine.inner();
    let component =
        WasmtimeComponent::new(raw, &guest).map_err(|e| anyhow::anyhow!("compile guest: {e}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wamn_postgres::add_to_linker(&mut linker)?;
    wamn_jetstream::add_to_linker(&mut linker)?;
    let pre = CommandPre::new(linker.instantiate_pre(&component)?)?;
    let report_dir = std::env::temp_dir().join(format!("wamn-rie2ebench-{}", std::process::id()));
    std::fs::create_dir_all(&report_dir).context("create report dir")?;
    let harness = Harness {
        engine,
        pre,
        pg,
        js: jsp,
        report_dir: report_dir.clone(),
        stream_name: stream_name.clone(),
    };

    let mut pass = true;
    let mut check = |name: &str, ok: bool| {
        println!("PASS({name}): {ok}");
        if !ok {
            pass = false;
        }
    };

    let evt_runs_for = |id: &str| {
        let db = &db;
        let id = id.to_string();
        async move {
            scalar(
                db,
                &format!(
                    "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' \
                     AND flow_id = 'disp-del' AND trigger_source LIKE 'evt:%' \
                     AND input_json->'payload'->>'id' = '{id}'"
                ),
            )
            .await
        }
    };

    // ========================================================================
    // Phase 1 — pre-flip DELETE under RI DEFAULT: alertable refusal, no fire.
    // ========================================================================
    println!("\n## phase 1: pre-flip DELETE (RI DEFAULT — key-only old image)");
    check(
        "dispositions starts at REPLICA IDENTITY DEFAULT ('d')",
        relreplident(&db, TABLE).await? == "d",
    );
    db.execute(
        "DELETE FROM app.dispositions WHERE id = $1::text::uuid",
        &[&pre_id],
    )
    .await
    .context("pre-flip delete")?;
    let have = wait_stream_count(&js, &stream_name, 1, 60).await?;
    check(
        "the pre-flip delete reached the stream (1 event)",
        have == 1,
    );
    if reader.is_finished() {
        bail!("reader died mid-gate: {:?}", reader.await);
    }
    let report1 = harness.run_guest(4, 64).await?;
    println!("phase-1 guest report: {report1}");

    check(
        "pre-flip DELETE refused (tenant-unscopable — the real key-only old image)",
        counter(&report1, "refuse-tenant-unscopable") == 1,
    );
    check(
        "pre-flip refusal is NOT a condition-false skip (alertable, distinct)",
        counter(&report1, "skip-condition-false") == 0,
    );
    check(
        "pre-flip DELETE fired NOTHING (zero :evt: runs)",
        scalar(
            &db,
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' AND trigger_source LIKE 'evt:%'",
        )
        .await?
            == 0,
    );
    // No doorbell during a refusal (nothing enqueued, nothing may wake).
    let ring = tokio::time::timeout(Duration::from_millis(600), bells.next()).await;
    check(
        "zero doorbell rings during the refused pre-flip delete",
        ring.is_err(),
    );

    // ========================================================================
    // Phase 2 — flip RI → FULL via the REAL reconcile (l5i9.31/l5i9.61).
    // ========================================================================
    println!(
        "\n## phase 2: reconcile REPLICA IDENTITY (delete subscription drives dispositions -> FULL)"
    );
    let plan =
        wamn_ctl::reconcile_replica_identity::reconcile(&db, &catalog()?, "app", true).await?;
    check(
        "reconcile flips ONLY dispositions -> FULL (the delete registration drives it)",
        plan.flips.len() == 1
            && plan.flips[0].table == TABLE
            && relreplident(&db, TABLE).await? == "f",
    );

    // ========================================================================
    // Phase 3 — post-flip DELETE under RI FULL: a REAL FULL old image reaches
    // the materializer, tenant-scopes, and enqueues a scoped :evt: run.
    // ========================================================================
    println!("\n## phase 3: post-flip DELETE (RI FULL — a REAL full old image carries tenant_id)");
    db.execute(
        "DELETE FROM app.dispositions WHERE id = $1::text::uuid",
        &[&post_id],
    )
    .await
    .context("post-flip delete")?;
    let have = wait_stream_count(&js, &stream_name, 2, 60).await?;
    check(
        "the post-flip delete reached the stream (2 events total)",
        have == 2,
    );
    if reader.is_finished() {
        bail!("reader died mid-gate: {:?}", reader.await);
    }
    let report2 = harness.run_guest(4, 64).await?;
    println!("phase-3 guest report: {report2}");

    // The scoped run: disp-del:evt:<stream_seq> for the post-flip row, queued
    // with the REAL numeric stream_seq.
    let scoped = mint_evt_run_id(FLOW_ID, have);
    check(
        "post-flip DELETE fired ONE scoped :evt: delete run under FULL",
        scalar(
            &db,
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' \
             AND flow_id = 'disp-del' AND trigger_source LIKE 'evt:%'",
        )
        .await?
            == 1
            && evt_runs_for(&post_id).await? == 1,
    );
    check(
        "the scoped run is queued with the REAL stream_seq (seq > 0) under its minted id",
        scalar(
            &db,
            &format!(
                "SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = 't1' \
                 AND run_id = '{scoped}' AND stream_seq > 0"
            ),
        )
        .await?
            == 1,
    );
    check(
        "phase-3 guest fired exactly one run",
        counter(&report2, "fired") == 1,
    );
    // The scoped fire rings the doorbell exactly once.
    let mut rings = 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    while rings < 1 && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(400), bells.next()).await {
            Ok(Some(_)) => rings += 1,
            _ => break,
        }
    }
    check(
        "one doorbell ring for the scoped post-flip fire",
        rings == 1,
    );

    // The NON-RETROACTIVE boundary: the pre-flip delete STAYS refused — the flip
    // enriches only WAL written after it, and it never retro-fires.
    check(
        "non-retroactive: the pre-flip DEFAULT delete never fired (0 :evt: runs for it)",
        evt_runs_for(&pre_id).await? == 0,
    );
    check(
        "non-retroactive: phase 3 raised NO new tenant-unscopable refusal (the pre-flip one stands, not retried)",
        counter(&report2, "refuse-tenant-unscopable") == 0,
    );

    // --- teardown (zero residue: slot FIRST, then stream, then the db) ------
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(15), reader).await;
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

    println!("\nrie2ebench complete — overall PASS: {pass}");
    if !pass {
        bail!("wamn-3glr rie2ebench gate failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guards: the fixture registration parses as the FROZEN
    /// wamn-event-reg type (delete-subscribed — the op that drives the RI
    /// reconcile to FULL), and the fixture flow parses + validates as a
    /// wamn-flow graph — at `cargo test`, before the gate needs live infra.
    #[test]
    fn fixture_registration_and_flow_match_the_frozen_types() {
        let reg = wamn_event_reg::EventRegistration::from_json(&registration_json())
            .expect("delete registration is a frozen EventRegistration");
        assert!(
            reg.ops
                .iter()
                .any(|op| format!("{op:?}").to_lowercase().contains("delete")),
            "the fixture registration must subscribe delete (it drives RI -> FULL)"
        );

        let flow = wamn_flow::Flow::from_json(&flow_json()).expect("flow fixture parses");
        flow.validate().expect("flow fixture validates");
    }

    /// The catalog compiles and names exactly the one entity → table the gate
    /// maps and flips (the reconcile derives FULL for THIS table).
    #[test]
    fn fixture_catalog_names_the_one_flipped_entity() {
        let cat = catalog().expect("catalog parses");
        assert_eq!(cat.catalog_id, CATALOG_ID);
        assert!(
            cat.entities
                .iter()
                .any(|e| e.id == ENTITY_ID && e.name == TABLE),
            "catalog must carry evt_disp -> dispositions (the flipped entity)"
        );
    }
}
