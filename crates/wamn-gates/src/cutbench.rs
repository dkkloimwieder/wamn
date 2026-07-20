//! The `cutbench` subcommand: the l5i9.18 EVT-CUTOVER gate — shadow
//! equivalence, then the cutover flip (D19 v3 §7 Phase 2).
//!
//! ONE traffic source feeds BOTH capture paths simultaneously: real app writes
//! (a `wamn_app` connection under the tenant claim) hit floor tables carrying
//! the REAL outbox triggers (`Migration::outbox_triggers`) AND a REAL logical
//! slot — the old path runs the real `wamn_dispatcher` engine
//! (poll/match/fire/ack), the new path runs the real `wamn-cdc-reader`
//! (pg_walstream → JetStream) into the real `materializer.wasm` Service guest.
//! Nothing is taped or synthesized between the write and either path.
//!
//! Phases:
//!   1. shadow — the registrations are `state: shadow`: the guest evaluates
//!      its full pipeline but only writes the `evt_shadow` ledger. Asserts the
//!      DEFINED COMPARISON (v3 §7): zero `:evt:` runs and zero doorbells
//!      (compare-only is structural); a per-(flow, table, op, row-id)
//!      bijection between old-path firings and ledger `fire` rows; payload
//!      agreement under the table-type canonicalization
//!      (`to_jsonb(jsonb_populate_record(NULL::app.<t>, …))` — CDC's
//!      text-typed images vs the trigger's `to_jsonb` meet at the column
//!      types); kq0z key+policy agreement pairwise; and EVERY unmatched
//!      old-path firing accounted to a DECLARED divergence class
//!      (condition-narrowed registration = ledger `condition-false` skips;
//!      DELETE under REPLICA IDENTITY DEFAULT = ledger `tenant-unscopable`
//!      refusals — the l5i9.31 knob's territory). An old firing with NO ledger
//!      row, or a ledger fire with no old firing, fails the gate — that is the
//!      capture gap / double-fire the shadow exists to catch. A server-side
//!      consumer wipe + rerun proves the ledger's ON CONFLICT redelivery
//!      dedupe.
//!   2. cutover — the runbook flip (deactivate flows → settle → registrations
//!      `shadow`→`live` → reactivate), then more traffic: the dispatcher must
//!      YIELD the cut-over flows (their outbox rows consume unmatched) while
//!      the materializer fires them for real — including the writes that
//!      landed DURING the flip window (delayed, never lost, exactly once).
//!      `disp-del` deliberately stays shadow (its registration refuses
//!      REPLICA-IDENTITY-DEFAULT deletes), proving cutover is per-flow and a
//!      delete-subscribed flow keeps its old-path coverage until l5i9.31.
//!      A cross-path join on (flow, row-id) must be EMPTY — no source row
//!      fires on both paths.
//!
//! Needs: `--admin-database-url` (SUPERUSER on a `wal_level=logical` PG —
//! the gate creates/drops the throwaway `wamn_cutbench` database, the slot,
//! and the replication role), `--nats-url` (JetStream). Recipe:
//! docs/build-and-test.md [EVT-CUTOVER].

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
use wamn_ddl::{Confirmation, Migration, OutboxOptions};
use wamn_dispatcher::{Dispatcher, DispatcherConfig, ProjectSpec};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};
use wamn_host::plugins::wamn_postgres::{self, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig};
use wamn_provision::{cdc_object_name, event_stream_name, sql as provision_sql};
use wamn_registry::sql::{
    upsert_event_reader_sql, upsert_org_sql, upsert_project_env_sql, upsert_project_sql,
};

#[derive(Debug, Args)]
pub struct CutBenchArgs {
    /// The compiled materializer component.
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// SUPERUSER URL (path `/postgres`) on a `wal_level=logical` Postgres —
    /// the gate owns the throwaway `wamn_cutbench` database, slot, and role.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// JetStream-enabled NATS (the reader's EVT stream AND the doorbell).
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub nats_url: String,
}

const BENCH_ID: &str = "cutbench";
const DB: &str = "wamn_cutbench";
const ORG: &str = "cb0";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "t1";
const CDC_PW: &str = "wamn_cdc_pw";
/// The entity ID deliberately differs from the table name (`dispositions`) so
/// a matched comparison proves the OID→entity map was consulted end to end.
const ENTITY_ID: &str = "evt_disp";
const TABLE: &str = "dispositions";
const CATALOG_ID: &str = "cutcat";

// The REAL shipped DDL, compiled in — the gate cannot drift from deploy/sql.
const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The fixture catalog: the subscribed entity (text + numeric — the numeric
/// column is the text-vs-number canonicalization case) and an unsubscribed
/// bystander table whose events must bother neither path.
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "cutcat",
  "version": 1,
  "entities": [
    { "id": "evt_disp", "name": "dispositions", "fields": [
      { "id": "site", "name": "site", "type": { "kind": "text" } },
      { "id": "status", "name": "status", "type": { "kind": "text" } },
      { "id": "qty", "name": "qty", "type": { "kind": "numeric", "precision": 12, "scale": 2 } }
    ] },
    { "id": "evt_note", "name": "notes", "fields": [
      { "id": "body", "name": "body", "type": { "kind": "text" } }
    ] }
  ]
}"#;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("cutbench catalog parse: {e}"))
}

fn row_event_flow_json(
    flow_id: &str,
    event: &str,
    ordering: serde_json::Value,
    policy: Option<&str>,
) -> String {
    let mut flow = serde_json::json!({
        "schema-version": "0.1", "flow-id": flow_id, "version": 1,
        "trigger": {"type": "row-event", "table": TABLE, "event": event},
        "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
    });
    if !ordering.is_null() {
        flow["ordering"] = ordering;
    }
    if let Some(p) = policy {
        flow["partition-policy"] = serde_json::Value::String(p.into());
    }
    flow.to_string()
}

fn registration_json(
    registration_id: &str,
    flow_id: &str,
    ops: &[&str],
    condition: Option<&str>,
    partition_key: Option<&str>,
    state: &str,
) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "registration-id": registration_id,
        "catalog-id": CATALOG_ID,
        "flow-id": flow_id,
        "entity": ENTITY_ID,
        "ops": ops,
        "condition": condition,
        "partition-key": partition_key,
        "state": state,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Small helpers (the reader-live-test idioms)
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
    Ok(db.query_one(sql, &[]).await.with_context(|| sql.to_string())?.get(0))
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

        let stream_name = event_stream_name(ORG, ENV);
        let mut wasi = WasiCtxBuilder::new();
        wasi.args(&["materializer.wasm"])
            .inherit_stdout()
            .inherit_stderr()
            .envs(&[
                ("WAMN_MAT_STREAM", stream_name.as_str()),
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
// The comparator (the DEFINED comparison, v3 §7 Phase 2)
// ---------------------------------------------------------------------------

/// Old-path firings vs ledger `fire` rows, joined on the source identity
/// (flow, table, op, payload row id): `(matched, old_only, new_only)`.
const JOIN_COUNTS_SQL: &str = "\
WITH old_fires AS (
  SELECT flow_id, input_json->>'table' AS t, input_json->>'event' AS op,
         input_json->'payload'->>'id' AS rid
    FROM wamn_run.runs
   WHERE tenant_id = 't1' AND trigger_source LIKE 'outbox:%'
), new_fires AS (
  SELECT flow_id, table_name AS t, op, input_json->'payload'->>'id' AS rid
    FROM wamn_run.evt_shadow
   WHERE tenant_id = 't1' AND verdict = 'fire'
)
SELECT count(*) FILTER (WHERE o.rid IS NOT NULL AND n.rid IS NOT NULL),
       count(*) FILTER (WHERE n.rid IS NULL),
       count(*) FILTER (WHERE o.rid IS NULL)
  FROM old_fires o FULL OUTER JOIN new_fires n USING (flow_id, t, op, rid)";

/// Matched fire pairs whose payloads DIVERGE under the table-type
/// canonicalization: both sides round-trip through the table's real composite
/// type, so CDC's text-typed image and the trigger's `to_jsonb` image meet at
/// the column types (numeric `12.34` == `"12.34"`); any surviving difference
/// is a REAL data divergence. All fire-able rows in this program are
/// `dispositions` (deletes never match — the new path refuses them).
const PAYLOAD_DIVERGENCE_SQL: &str = "\
WITH old_fires AS (
  SELECT flow_id, input_json->>'table' AS t, input_json->>'event' AS op,
         input_json->'payload'->>'id' AS rid, input_json->'payload' AS payload
    FROM wamn_run.runs
   WHERE tenant_id = 't1' AND trigger_source LIKE 'outbox:%'
), new_fires AS (
  SELECT flow_id, table_name AS t, op, input_json->'payload'->>'id' AS rid,
         input_json->'payload' AS payload
    FROM wamn_run.evt_shadow
   WHERE tenant_id = 't1' AND verdict = 'fire'
)
SELECT count(*)
  FROM old_fires o JOIN new_fires n USING (flow_id, t, op, rid)
 WHERE to_jsonb(jsonb_populate_record(NULL::app.dispositions, o.payload))
       IS DISTINCT FROM
       to_jsonb(jsonb_populate_record(NULL::app.dispositions, n.payload))";

/// Matched fire pairs whose kq0z key+policy stamps DIVERGE: the old side's
/// queue row (still queued — the gate runs no workers) vs the ledger columns.
const KEY_POLICY_DIVERGENCE_SQL: &str = "\
WITH old_fires AS (
  SELECT flow_id, input_json->>'table' AS t, input_json->>'event' AS op,
         input_json->'payload'->>'id' AS rid, run_id
    FROM wamn_run.runs
   WHERE tenant_id = 't1' AND trigger_source LIKE 'outbox:%'
), new_fires AS (
  SELECT flow_id, table_name AS t, op, input_json->'payload'->>'id' AS rid,
         partition_key, partition_policy
    FROM wamn_run.evt_shadow
   WHERE tenant_id = 't1' AND verdict = 'fire'
)
SELECT count(*)
  FROM old_fires o JOIN new_fires n USING (flow_id, t, op, rid)
  JOIN wamn_run.run_queue q ON q.tenant_id = 't1' AND q.run_id = o.run_id
 WHERE q.partition_key IS DISTINCT FROM n.partition_key
    OR q.partition_policy IS DISTINCT FROM n.partition_policy";

/// The cross-path double-fire probe: a source row with BOTH an `:outbox:` and
/// an `:evt:` run for the same flow. Must be empty at every point in the
/// gate's life — during shadow (compare-only) AND after the flip (yield).
const DOUBLE_FIRE_SQL: &str = "\
SELECT count(*)
  FROM wamn_run.runs a
  JOIN wamn_run.runs b
    ON b.tenant_id = a.tenant_id AND b.flow_id = a.flow_id
   AND b.input_json->'payload'->>'id' = a.input_json->'payload'->>'id'
   AND b.trigger_source LIKE 'evt:%'
 WHERE a.tenant_id = 't1' AND a.trigger_source LIKE 'outbox:%'";

// ---------------------------------------------------------------------------
// Traffic + drivers
// ---------------------------------------------------------------------------

struct Writer {
    app: tokio_postgres::Client,
}

impl Writer {
    /// Insert one disposition; returns the floor-minted uuid (text).
    async fn insert(&self, site: &str, status: &str, qty: &str) -> anyhow::Result<String> {
        Ok(self
            .app
            .query_one(
                "INSERT INTO \"dispositions\" (tenant_id, site, status, qty) \
                 VALUES (current_setting('app.tenant', true), $1, $2, $3::text::numeric) \
                 RETURNING id::text",
                &[&site, &status, &qty],
            )
            .await
            .context("insert disposition")?
            .get(0))
    }

    async fn update_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        self.app
            .execute(
                "UPDATE \"dispositions\" SET status = $2 WHERE id = $1::text::uuid",
                &[&id, &status],
            )
            .await
            .context("update disposition")?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.app
            .execute(
                "DELETE FROM \"dispositions\" WHERE id = $1::text::uuid",
                &[&id],
            )
            .await
            .context("delete disposition")?;
        Ok(())
    }

    async fn note(&self, body: &str) -> anyhow::Result<()> {
        self.app
            .execute(
                "INSERT INTO \"notes\" (tenant_id, body) \
                 VALUES (current_setting('app.tenant', true), $1)",
                &[&body],
            )
            .await
            .context("insert note")?;
        Ok(())
    }
}

/// Drive the real dispatcher engine until the outbox settles: tick with
/// stepped time until two consecutive quiet sweeps (batch 64 > any program
/// here, so one sweep drains; the second proves it). Returns every outbox run
/// id fired across the drain.
async fn drain_dispatcher(
    d: &mut Dispatcher,
    now_ms: &mut i64,
) -> anyhow::Result<Vec<String>> {
    let mut fired = Vec::new();
    let mut quiet = 0;
    for _ in 0..20 {
        *now_ms += 1_000;
        let report = d.tick_project(0, *now_ms).await?;
        if report.outbox_fired.is_empty() {
            quiet += 1;
            if quiet >= 2 {
                return Ok(fired);
            }
        } else {
            quiet = 0;
            fired.extend(report.outbox_fired);
        }
    }
    bail!("outbox never settled in 20 sweeps");
}

/// Wait for the EVT stream to hold `want` messages (the reader is async).
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
// The gate
// ---------------------------------------------------------------------------

pub async fn run(args: CutBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates cutbench (l5i9.18 EVT-CUTOVER — shadow equivalence + flip)");

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
        &[&ORG, &PROJECT, &ENV, &"wamn-db-cb0--app--dev", &None::<&str>],
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

    // The app floor (the REAL 3.2 DDL) + the REAL outbox triggers + CDC.
    db.batch_execute(&provision_sql::ensure_schema_sql("app"))
        .await
        .context("app schema")?;
    db.batch_execute("GRANT USAGE ON SCHEMA app TO wamn_app")
        .await
        .context("app schema usage")?;
    let floor = Migration::create(&catalog()?)
        .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
        .sql(Confirmation::None)
        .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;
    db.batch_execute(&format!("SET search_path TO app; {floor}"))
        .await
        .context("apply the 3.2 floor")?;
    let triggers = Migration::outbox_triggers(
        &catalog()?,
        &OutboxOptions {
            schema: "wamn_run".into(),
        },
    )
    .map_err(|e| anyhow::anyhow!("outbox plan: {e}"))?
    .sql(Confirmation::None)
    .map_err(|e| anyhow::anyhow!("outbox sql: {e}"))?;
    db.batch_execute(&format!("SET search_path TO app; {triggers}"))
        .await
        .context("apply the REAL outbox triggers")?;
    db.batch_execute("SET search_path TO public")
        .await
        .context("reset search_path")?;
    db.batch_execute(&provision_sql::ensure_replication_role_sql(&cdc_name, CDC_PW))
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
    .context("map evt_disp -> dispositions (notes stays unmapped)")?;
    db.batch_execute(&provision_sql::grant_replication_access_sql(DB, &cdc_name, "app"))
        .await
        .context("grants")?;

    // Flows (both-path fixtures): keyed leapfrog, conditional-on-the-new-side,
    // and the delete subscriber that must NOT cut over.
    for (flow_id, event, ordering, policy) in [
        (
            "disp-flow",
            "insert",
            serde_json::json!({"mode": "partitioned", "partition-key": "payload.site"}),
            Some("leapfrog"),
        ),
        ("disp-cond", "insert", serde_json::Value::Null, None),
        ("disp-del", "delete", serde_json::Value::Null, None),
    ] {
        db.execute(
            "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
             VALUES ($1, $2, 1, true, $3::text::jsonb)",
            &[&TENANT, &flow_id, &row_event_flow_json(flow_id, event, ordering, policy)],
        )
        .await
        .with_context(|| format!("seed flow {flow_id}"))?;
    }
    // Registrations — ALL shadow for phase 1 (the compare-only dual run).
    for (rid, flow_id, ops, condition, key) in [
        ("r-disp", "disp-flow", vec!["insert"], None, Some("new.site")),
        (
            "r-cond",
            "disp-cond",
            vec!["insert"],
            Some("new.status == 'ok'"),
            None,
        ),
        ("r-del", "disp-del", vec!["delete"], None, None),
    ] {
        db.execute(
            "INSERT INTO catalog.event_registrations \
             (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb)",
            &[
                &TENANT,
                &CATALOG_ID,
                &rid,
                &flow_id,
                &ENTITY_ID,
                &registration_json(rid, flow_id, &ops, condition, key, "shadow"),
            ],
        )
        .await
        .with_context(|| format!("seed registration {rid}"))?;
    }
    println!("seeded 3 flows (row-event) + 3 SHADOW registrations on entity {ENTITY_ID}");

    // --- NATS + the doorbell observer + the reader ---------------------------
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats.clone());
    let _ = js.delete_stream(&stream_name).await;
    let mut bells = nats
        .subscribe(format!("wamn.doorbell.{TENANT}"))
        .await
        .context("subscribe doorbell")?;

    // The slot LAST (capture starts here — provisioning writes stay out).
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
    let report_dir = std::env::temp_dir().join(format!("wamn-cutbench-{}", std::process::id()));
    std::fs::create_dir_all(&report_dir).context("create report dir")?;
    let harness = Harness {
        engine,
        pre,
        pg,
        js: jsp,
        report_dir: report_dir.clone(),
    };

    // The old path: the REAL dispatcher engine over the same database.
    let mut dispatcher = Dispatcher::connect(
        &[ProjectSpec {
            name: "cut".into(),
            url: app_url.clone(),
            tenant: TENANT.into(),
            schema: Some("wamn_run".into()),
        }],
        None,
        DispatcherConfig::default(),
    )
    .await
    .context("dispatcher connect")?;
    let mut now_ms: i64 = 1_753_000_000_000;

    // The single traffic source: a wamn_app writer under the tenant claim.
    let writer = Writer {
        app: connect(&app_url).await?,
    };
    writer
        .app
        .batch_execute(&format!(
            "SET search_path TO app; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("writer claims")?;

    let mut pass = true;
    let mut check = |name: &str, ok: bool| {
        println!("PASS({name}): {ok}");
        if !ok {
            pass = false;
        }
    };

    // ========================================================================
    // Phase 1 — SHADOW: one write program, both paths, compare-only.
    // ========================================================================
    println!("\n## phase 1: shadow (dual-run, compare-only)");
    let mut ok_ids = Vec::new();
    let mut hold_ids = Vec::new();
    // 12 inserts (7 ok / 5 hold) across 3 sites; 3 of them in ONE transaction
    // (multi-row txn commit ordering rides the same stream positions).
    for i in 0..9 {
        let site = format!("s-{}", i % 3);
        let status = if i % 3 == 2 { "hold" } else { "ok" };
        let id = writer.insert(&site, status, &format!("{}.50", 10 + i)).await?;
        if status == "ok" { ok_ids.push(id) } else { hold_ids.push(id) };
    }
    writer.app.batch_execute("BEGIN").await?;
    for i in 9..12 {
        let site = format!("s-{}", i % 3);
        let status = if i >= 10 { "hold" } else { "ok" };
        let id = writer.insert(&site, status, "99.99").await?;
        if status == "ok" { ok_ids.push(id) } else { hold_ids.push(id) };
    }
    writer.app.batch_execute("COMMIT").await?;
    // 3 updates (both sides subscribe inserts only — op-mismatch on the new
    // side, unmatched on the old), 2 deletes (the DECLARED divergence class),
    // 2 bystander notes.
    for id in hold_ids.iter().take(3) {
        writer.update_status(id, "ok").await?;
    }
    let deleted: Vec<String> = ok_ids.drain(..2).collect();
    for id in &deleted {
        writer.delete(id).await?;
    }
    writer.note("bystander-1").await?;
    writer.note("bystander-2").await?;
    println!("wrote the program: 12 inserts (7 ok / 5 hold, 3 in one txn) + 3 updates + 2 deletes + 2 notes");

    // Old path fires now.
    let fired = drain_dispatcher(&mut dispatcher, &mut now_ms).await?;
    check(
        "old path fired 26 runs (12 disp-flow + 12 disp-cond + 2 disp-del)",
        fired.len() == 26,
    );
    check(
        "old-path outbox settled (no pending rows)",
        scalar(&db, "SELECT count(*) FROM wamn_run.outbox WHERE dispatched_at IS NULL").await? == 0,
    );

    // New path: capture catches up (19 = 12 ins + 3 upd + 2 del + 2 notes),
    // then one shadow drain.
    let have = wait_stream_count(&js, &stream_name, 19, 60).await?;
    check("the stream holds the whole program (19 events)", have == 19);
    if reader.is_finished() {
        bail!("reader died mid-gate: {:?}", reader.await);
    }
    let report = harness.run_guest(3, 64).await?;
    println!("shadow guest report: {report}");

    // The comparison (the l5i9.18 definition, executable form).
    check(
        "shadow fired NOTHING real: zero :evt: runs",
        scalar(&db, "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' AND trigger_source LIKE 'evt:%'").await? == 0,
    );
    check(
        "shadow enqueued NOTHING: queue rows == old-path rows (26)",
        scalar(&db, "SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = 't1'").await? == 26,
    );
    let row = db.query_one(JOIN_COUNTS_SQL, &[]).await?;
    let (matched, old_only, new_only): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
    check(
        "bijection: 19 matched fire pairs (12 disp-flow + 7 disp-cond ok-rows)",
        matched == 19,
    );
    check("no new-path-only fires (a would-be double-fire)", new_only == 0);
    check(
        "7 old-only firings await classification (5 condition + 2 delete)",
        old_only == 7,
    );
    // EVERY old-only firing lands in a DECLARED class — nothing unexplained.
    let cond_class = scalar(
        &db,
        "SELECT count(*) FROM wamn_run.evt_shadow WHERE tenant_id = 't1' \
         AND flow_id = 'disp-cond' AND verdict = 'skip' AND reason = 'condition-false'",
    )
    .await?;
    let del_class = scalar(
        &db,
        "SELECT count(*) FROM wamn_run.evt_shadow WHERE tenant_id = 't1' \
         AND flow_id = 'disp-del' AND verdict = 'refuse' AND reason = 'tenant-unscopable'",
    )
    .await?;
    check(
        "class CONDITION-SCOPE: 5 ledger condition-false skips on disp-cond",
        cond_class == 5,
    );
    check(
        "class EXPECTED-DELETE-RI: 2 ledger tenant-unscopable refusals on disp-del",
        del_class == 2,
    );
    check(
        "every old-only firing is classified (5 + 2 == 7)",
        cond_class + del_class == old_only,
    );
    check(
        "payload agreement under table-type canonicalization (0 divergent pairs)",
        scalar(&db, PAYLOAD_DIVERGENCE_SQL).await? == 0,
    );
    check(
        "kq0z key+policy agreement pairwise (0 divergent pairs)",
        scalar(&db, KEY_POLICY_DIVERGENCE_SQL).await? == 0,
    );
    check(
        "no cross-path double-fire (vacuously — shadow fired nothing)",
        scalar(&db, DOUBLE_FIRE_SQL).await? == 0,
    );
    check(
        "guest counters: 19 shadow fires, 0 real fires",
        counter(&report, "shadow-fire") == 19 && counter(&report, "fired") == 0,
    );
    // No doorbells in shadow — nothing was enqueued, nothing may wake.
    let ring = tokio::time::timeout(Duration::from_millis(600), bells.next()).await;
    check("zero doorbell rings during shadow", ring.is_err());

    // Redelivery: wipe the durables server-side, rerun — the ledger must not
    // move (its PK ON CONFLICT is the exactly-once discipline).
    let ledger_before = scalar(&db, "SELECT count(*) FROM wamn_run.evt_shadow WHERE tenant_id = 't1'").await?;
    let stream = js.get_stream(&stream_name).await.context("get stream")?;
    for rid in ["r-disp", "r-cond", "r-del"] {
        let name = format!("mat_{TENANT}_{CATALOG_ID}_{rid}");
        stream
            .delete_consumer(&name)
            .await
            .with_context(|| format!("delete durable {name} (must exist after phase 1)"))?;
    }
    let _ = harness.run_guest(3, 64).await?;
    check(
        "full redelivery leaves the ledger unmoved (ON CONFLICT exactly-once)",
        scalar(&db, "SELECT count(*) FROM wamn_run.evt_shadow WHERE tenant_id = 't1'").await? == ledger_before,
    );
    check(
        "redelivery still fired nothing real",
        scalar(&db, "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' AND trigger_source LIKE 'evt:%'").await? == 0,
    );

    // ========================================================================
    // Phase 2 — CUTOVER: the runbook flip, then traffic on the live path.
    // ========================================================================
    println!("\n## phase 2: cutover (deactivate -> settle -> live -> reactivate)");
    // (1) Deactivate: both paths quiesce (old consumes-unfired, new holds).
    db.execute(
        "UPDATE wamn_run.flows SET active = false WHERE tenant_id = $1 \
         AND flow_id IN ('disp-flow', 'disp-cond')",
        &[&TENANT],
    )
    .await
    .context("deactivate flows")?;
    // Writes DURING the flip window — must fire exactly once, on the NEW path,
    // after reactivation (delayed, never lost).
    let mut gap_ids = Vec::new();
    for i in 0..2 {
        gap_ids.push(writer.insert(&format!("s-{i}"), "ok", "7.00").await?);
    }
    // (2) Settle: the old path consumes the gap rows unfired (inactive flow =
    // unmatched; disp-del stays active but subscribes deletes only).
    let flip_fired = drain_dispatcher(&mut dispatcher, &mut now_ms).await?;
    check(
        "flip-window rows consumed UNFIRED by the old path",
        flip_fired.is_empty()
            && scalar(&db, "SELECT count(*) FROM wamn_run.outbox WHERE dispatched_at IS NULL").await? == 0,
    );
    // (3) Flip shadow -> live (disp-del deliberately stays shadow: its
    // registration refuses RI-DEFAULT deletes — cutover waits on l5i9.31).
    db.execute(
        "UPDATE catalog.event_registrations \
         SET registration = jsonb_set(registration, '{state}', '\"live\"') \
         WHERE tenant_id = $1 AND registration_id IN ('r-disp', 'r-cond')",
        &[&TENANT],
    )
    .await
    .context("flip registrations live")?;
    // (4) Reactivate.
    db.execute(
        "UPDATE wamn_run.flows SET active = true WHERE tenant_id = $1 \
         AND flow_id IN ('disp-flow', 'disp-cond')",
        &[&TENANT],
    )
    .await
    .context("reactivate flows")?;

    // Post-flip traffic: 6 inserts (4 ok / 2 hold) + 1 delete.
    let mut post_ids = Vec::new();
    for i in 0..6 {
        let status = if i < 4 { "ok" } else { "hold" };
        post_ids.push(writer.insert(&format!("s-{}", i % 3), status, "3.30").await?);
    }
    writer.delete(&post_ids[5]).await?;

    // The old path must now YIELD the cut-over flows — and still own deletes.
    let old_before_p2 = 26i64;
    let post_fired = drain_dispatcher(&mut dispatcher, &mut now_ms).await?;
    check(
        "dispatcher yields the live flows: only the delete fired old-path",
        post_fired.len() == 1 && post_fired[0].starts_with("disp-del:outbox:"),
    );
    check(
        "yielded rows consumed (no pending outbox)",
        scalar(&db, "SELECT count(*) FROM wamn_run.outbox WHERE dispatched_at IS NULL").await? == 0,
    );
    check(
        "old-path run count grew by exactly the delete",
        scalar(&db, "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' AND trigger_source LIKE 'outbox:%'").await?
            == old_before_p2 + 1,
    );

    // New path drains: gap(2) + post inserts(6) + delete(1) = 9 more events.
    wait_stream_count(&js, &stream_name, 19 + 2 + 7, 60).await?;
    let report2 = harness.run_guest(3, 64).await?;
    println!("cutover guest report: {report2}");

    // Live fires: disp-flow = 2 gap + 6 post = 8; disp-cond ok-rows = 2 gap +
    // 4 post = 6. All 14 real, queued, doorbelled.
    let evt_runs = scalar(&db, "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' AND trigger_source LIKE 'evt:%'").await?;
    check("live path fired 14 :evt: runs (8 disp-flow + 6 disp-cond)", evt_runs == 14);
    check(
        "every :evt: run is queued with the REAL stream_seq (14 rows, seq > 0)",
        scalar(
            &db,
            "SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = 't1' \
             AND run_id LIKE '%:evt:%' AND stream_seq > 0",
        )
        .await?
            == 14,
    );
    check(
        "keyed evt rows carry the extractor site + declared leapfrog (8 disp-flow)",
        scalar(
            &db,
            "SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = 't1' \
             AND run_id LIKE 'disp-flow:evt:%' AND partition_key LIKE 's-%' \
             AND partition_policy = 'leapfrog'",
        )
        .await?
            == 8,
    );
    check(
        "the flip-window writes fired EXACTLY ONCE, on the new path",
        {
            let mut all = true;
            for id in &gap_ids {
                let n: i64 = db
                    .query_one(
                        "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' \
                         AND flow_id = 'disp-flow' AND input_json->'payload'->>'id' = $1",
                        &[&id],
                    )
                    .await?
                    .get(0);
                let evt: i64 = db
                    .query_one(
                        "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = 't1' \
                         AND flow_id = 'disp-flow' AND trigger_source LIKE 'evt:%' \
                         AND input_json->'payload'->>'id' = $1",
                        &[&id],
                    )
                    .await?
                    .get(0);
                all &= n == 1 && evt == 1;
            }
            all
        },
    );
    check(
        "NO source row fired on both paths (the cross-path join is empty)",
        scalar(&db, DOUBLE_FIRE_SQL).await? == 0,
    );
    check(
        "the still-shadow delete flow observed its refusal (ledger grew by 1)",
        scalar(
            &db,
            "SELECT count(*) FROM wamn_run.evt_shadow WHERE tenant_id = 't1' \
             AND flow_id = 'disp-del' AND verdict = 'refuse' AND reason = 'tenant-unscopable'",
        )
        .await?
            == 3,
    );
    // 14 doorbell rings for the 14 live fires.
    let mut rings = 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    while rings < 14 && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), bells.next()).await {
            Ok(Some(_)) => rings += 1,
            _ => break,
        }
    }
    check("14 doorbell rings for the 14 live fires", rings == 14);

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
    drop(writer);
    drop(dispatcher);
    drop(db);
    let _ = admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {DB} WITH (FORCE)"))
        .await;
    let _ = admin.batch_execute(&format!("DROP ROLE IF EXISTS {cdc_name}")).await;
    let _ = std::fs::remove_dir_all(&report_dir);
    ticker.abort();

    println!("\ncutbench complete — overall PASS: {pass}");
    if !pass {
        bail!("l5i9.18 cutbench gate failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guards: the fixture registrations parse as the FROZEN
    /// wamn-event-reg type with the intended states (the shadow seed IS
    /// shadow; a flipped doc IS live), and the fixture flows parse as
    /// wamn-flow graphs — at `cargo test`, before the gate needs live infra.
    #[test]
    fn fixture_registrations_and_flows_match_the_frozen_types() {
        let shadow = registration_json(
            "r-disp",
            "disp-flow",
            &["insert"],
            None,
            Some("new.site"),
            "shadow",
        );
        let reg = wamn_event_reg::EventRegistration::from_json(&shadow)
            .expect("shadow fixture is a frozen EventRegistration");
        assert!(reg.state.is_shadow());

        let live = registration_json(
            "r-cond",
            "disp-cond",
            &["insert"],
            Some("new.status == 'ok'"),
            None,
            "live",
        );
        let reg = wamn_event_reg::EventRegistration::from_json(&live).expect("live fixture parses");
        assert!(!reg.state.is_shadow());

        let flow = row_event_flow_json(
            "disp-flow",
            "insert",
            serde_json::json!({"mode": "partitioned", "partition-key": "payload.site"}),
            Some("leapfrog"),
        );
        let f = wamn_flow::Flow::from_json(&flow).expect("flow fixture parses");
        f.validate().expect("flow fixture validates");
    }

    /// The comparator's join keys are the DEFINED comparison (v3 §7 Phase 2):
    /// (flow, table, op, payload row id) — never run_id (the namespaces are
    /// disjoint by design) and never seq (the sequence domains never align).
    #[test]
    fn comparator_joins_on_source_identity_never_ids_or_seqs() {
        for sql in [JOIN_COUNTS_SQL, PAYLOAD_DIVERGENCE_SQL, KEY_POLICY_DIVERGENCE_SQL] {
            assert!(sql.contains("USING (flow_id, t, op, rid)"), "join key drifted: {sql}");
            assert!(!sql.contains("USING (run_id"), "must never join on run_id");
        }
        assert!(PAYLOAD_DIVERGENCE_SQL.contains("jsonb_populate_record(NULL::app.dispositions"));
        assert!(DOUBLE_FIRE_SQL.contains("trigger_source LIKE 'evt:%'"));
        assert!(DOUBLE_FIRE_SQL.contains("trigger_source LIKE 'outbox:%'"));
    }
}
