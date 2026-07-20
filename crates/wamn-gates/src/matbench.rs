//! The `matbench` subcommand: the l5i9.17 materializer gate + first C-MAT
//! numbers (D19 v3 §8 — measured, not gated).
//!
//! Drives the REAL Service guest (`materializer.wasm`, wasi:cli/run) in
//! process — the apibench harness shape — against a REAL throwaway Postgres
//! (the actual `deploy/sql` DDL, compiled in via `include_str!` so the gate
//! can never drift from the shipped schema — the 9mg8 lesson) and a REAL
//! JetStream (a throwaway stream on a local/CI NATS). The guest links the
//! production plugins: `WamnPostgres` (wamn_app pool, tenant claim t1, schema
//! wamn_run) and `WamnJetstream` (data-plane URL + the doorbell client, whose
//! rings the harness observes on a `wamn.doorbell.t1` subscription).
//!
//! Phases:
//!   1. decide  — seed 4 flows + 4 registrations (unconditional / conditional /
//!      partitioned-with-extractor / old-value SERVED, l5i9.31), publish a
//!      fixture tape of 8 envelopes (fires, condition-false, foreign tenant,
//!      unscopable table, unscopable DELETE, causation depth 3, causation depth
//!      16, and an UPDATE carrying a FULL old image → the changed-to eval fires),
//!      run the
//!      guest, and assert: the run/queue rows (padded run ids, REAL
//!      `stream_seq`, kq0z-coherent key+policy), the causation thread
//!      (`input_json.causation.depth = parent+1`), the doorbell rings, and the
//!      DISTINCT refusal counters (the guest's report file).
//!   2. burst   — publish `--burst` more matching inserts and time the drain:
//!      the first C-MAT deliveries→enqueue number (provenance: local, debug).
//!   3. redeliver — delete every durable consumer server-side and rerun the
//!      guest: the WHOLE tape redelivers, and the run/queue row counts must
//!      not move (`ON CONFLICT` exactly-once past the dedupe window; the
//!      report's `duplicate` counter proves the collisions happened).
//!
//! Needs: `--admin-database-url` (superuser, throwaway DB), `--database-url`
//! (the wamn_app pool URL), `--nats-url` (JetStream enabled). Recipe:
//! docs/build-and-test.md [EVT-MAT].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use futures_util::StreamExt as _;
use tokio_postgres::NoTls;

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use wamn_event_wire::Op;
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};
use wamn_host::plugins::wamn_postgres::{self, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig};
use wamn_run_queue::mint_evt_run_id;

#[derive(Debug, Args)]
pub struct MatBenchArgs {
    /// The compiled materializer component.
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// wamn_app pool URL for the plugin (falls back to WAMN_PG_URL /
    /// DATABASE_URL).
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL for provisioning the throwaway schemas (falls back to
    /// WAMN_PG_ADMIN_URL).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// JetStream-enabled NATS (the throwaway EVT stream AND the doorbell ride
    /// it in this gate).
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub nats_url: String,

    /// Burst size for the C-MAT drain measurement.
    #[arg(long, default_value_t = 200)]
    pub burst: usize,
}

const BENCH_ID: &str = "matbench";
const TENANT: &str = "t1";
const STREAM: &str = "WAMN_MATBENCH";
const ORG: &str = "morg";
const PROJECT: &str = "mproj";
const ENV: &str = "menv";
const ENTITY: &str = "receipts";

// The REAL shipped DDL, compiled in — the gate cannot drift from deploy/sql.
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn flow_json(flow_id: &str, ordering: serde_json::Value, policy: Option<&str>) -> String {
    let mut flow = serde_json::json!({
        "schema-version": "0.1", "flow-id": flow_id, "version": 1,
        "trigger": {"type": "manual"},
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
    ops: Vec<Op>,
    condition: Option<&str>,
    partition_key: Option<&str>,
) -> String {
    // The frozen EVT-REG builder — no hand-built JSON copy (wamn-idx3). The guest
    // reads this back through the same `EventRegistration` type, so the tape's
    // registrations cannot drift from the model of record.
    wamn_event_reg::EventRegistration {
        schema_version: wamn_event_reg::SCHEMA_VERSION.to_string(),
        registration_id: registration_id.to_string(),
        catalog_id: "matcat".to_string(),
        flow_id: flow_id.to_string(),
        entity: wamn_catalog::EntityId::from(ENTITY),
        ops,
        condition: condition.map(str::to_string),
        partition_key: partition_key.map(str::to_string),
    }
    .to_json()
}

/// One tape envelope. `lsn` doubles as the Nats-Msg-Id discriminator.
fn envelope_json(
    op: Op,
    old: Option<serde_json::Value>,
    new: Option<serde_json::Value>,
    lsn: u64,
    causation: Option<(u32, &str)>,
) -> String {
    let mut env = serde_json::json!({
        "op": op.as_str(),
        "entity": ENTITY,
        "table": "receipts_v2",
        "lsn": lsn,
        "txid": 100 + lsn,
        "commit_ts": "2026-07-19T12:00:00Z",
    });
    if let Some(o) = old {
        env["old"] = o;
    }
    if let Some(n) = new {
        env["new"] = n;
    }
    if let Some((depth, root)) = causation {
        env["causation"] = serde_json::json!({
            // The frozen evt run-id builder — no inline padded copy (l5i9.30).
            "run": mint_evt_run_id("parent", u64::from(depth)),
            "root": root,
            "depth": depth,
        });
    }
    env.to_string()
}

// ---------------------------------------------------------------------------
// Harness
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

    /// One guest run to completion (`WAMN_MAT_MAX_SWEEPS` bounds it) under a
    /// deadline, with a fresh store; returns the parsed counters report.
    async fn run_guest(&self, max_sweeps: u64, batch: u32) -> anyhow::Result<serde_json::Value> {
        let report_path = self.report_dir.join("counters.json");
        let _ = std::fs::remove_file(&report_path);

        let mut wasi = WasiCtxBuilder::new();
        wasi.args(&["materializer.wasm"])
            .inherit_stdout()
            .inherit_stderr()
            .envs(&[
                ("WAMN_MAT_STREAM", STREAM),
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

async fn admin_connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect admin postgres")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(client)
}

fn counter(report: &serde_json::Value, key: &str) -> i64 {
    report.get(key).and_then(|v| v.as_i64()).unwrap_or(-1)
}

pub async fn run(args: MatBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates matbench (l5i9.17 EVT-MAT)");

    let guest = std::fs::read(&args.component)
        .with_context(|| format!("read {}", args.component.display()))?;

    // --- Postgres: provision the throwaway schemas from the REAL DDL --------
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;
    let admin = admin_connect(&admin_url).await?;
    admin
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
             CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
             DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
             DROP SCHEMA IF EXISTS catalog CASCADE;",
        )
        .await
        .context("hermetic preamble")?;
    admin
        .batch_execute(RUN_STATE_SQL)
        .await
        .context("apply run-state.sql")?;
    admin
        .batch_execute(RUN_QUEUE_SQL)
        .await
        .context("apply run-queue.sql")?;
    admin
        .batch_execute(FLOWS_SQL)
        .await
        .context("apply flows.sql")?;
    admin
        .batch_execute(CATALOG_SQL)
        .await
        .context("apply catalog-schema.sql")?;
    println!("provisioned wamn_run + catalog from deploy/sql (include_str! — drift-proof)");

    // Seed flows: unordered/unconditional, unordered/conditional, partitioned
    // (leapfrog; the registration carries the event-context extractor), and
    // the old-condition flow (held at the registration, so never fired).
    for (flow_id, ordering, policy) in [
        ("f-plain", serde_json::Value::Null, None),
        ("f-cond", serde_json::Value::Null, None),
        (
            "f-key",
            serde_json::json!({"mode": "partitioned", "partition-key": "payload.site"}),
            Some("leapfrog"),
        ),
        ("f-old", serde_json::Value::Null, None),
    ] {
        admin
            .execute(
                "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
                 VALUES ($1, $2, 1, true, $3::text::jsonb)",
                &[&TENANT, &flow_id, &flow_json(flow_id, ordering, policy)],
            )
            .await
            .with_context(|| format!("seed flow {flow_id}"))?;
    }
    // Registrations (superuser bypasses the tenant-FORCE RLS for seeding).
    for (rid, flow_id, ops, condition, extractor) in [
        (
            "r-plain",
            "f-plain",
            vec![Op::Insert, Op::Delete],
            None,
            None,
        ),
        (
            "r-cond",
            "f-cond",
            vec![Op::Insert],
            Some("new.status == 'received'"),
            None,
        ),
        ("r-key", "f-key", vec![Op::Insert], None, Some("new.site")),
        (
            // l5i9.31: a root-`old` condition is SERVED now (no longer held). It
            // refuses old-image-absent on the inserts (old absent under RI
            // DEFAULT — cannot-evaluate, never condition-false) and FIRES on the
            // UPDATE that carries a full old image (E8 — the changed-to eval).
            "r-old",
            "f-old",
            vec![Op::Insert, Op::Update],
            Some("new.status != old.status"),
            None,
        ),
    ] {
        admin
            .execute(
                "INSERT INTO catalog.event_registrations \
                 (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
                 VALUES ($1, 'matcat', $2, $3, $4, $5::text::jsonb)",
                &[
                    &TENANT,
                    &rid,
                    &flow_id,
                    &ENTITY,
                    &registration_json(rid, flow_id, ops, condition, extractor),
                ],
            )
            .await
            .with_context(|| format!("seed registration {rid}"))?;
    }
    println!("seeded 4 flows + 4 registrations (r-old is the SERVED old-condition case — l5i9.31)");

    // --- NATS: throwaway stream + the fixture tape --------------------------
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats.clone());
    let _ = js.delete_stream(STREAM).await;
    js.create_stream(async_nats::jetstream::stream::Config {
        name: STREAM.into(),
        // The frozen wire stream-subject filter (wamn-idx3) — `evt.<org>.*.<env>.>`,
        // the exact filter a production EVT_ stream binds; captures the tape's
        // evt.morg.mproj.menv.receipts.* subjects.
        subjects: vec![wamn_event_wire::stream_subjects(ORG, ENV)],
        storage: async_nats::jetstream::stream::StorageType::File,
        num_replicas: 1,
        duplicate_window: Duration::from_secs(120),
        ..Default::default()
    })
    .await
    .context("create throwaway stream")?;

    // The doorbell observer — subscribe BEFORE the guest can ring.
    let mut bells = nats
        .subscribe(format!("wamn.doorbell.{TENANT}"))
        .await
        .context("subscribe doorbell")?;

    // The tape (stream seqs 1..=8 in publish order):
    //  1 fires plain+cond+key; 2 fires plain+key, condition-false on cond;
    //  3 foreign tenant; 4 unscopable (no tenant_id); 5 unscopable DELETE
    //  (r-plain registers deletes); 6 chained at depth 3 → child depth 4;
    //  7 chained at depth 16 → the loop-bound refusal; 8 is an UPDATE carrying a
    //  FULL old image (RI FULL, l5i9.31): r-old evaluates `new.status !=
    //  old.status` end to end and FIRES (f-old:evt:8). The inserts 1/2/6/7 that
    //  reach r-old's condition have NO old image → old-image-absent refusals
    //  (cannot-evaluate, never condition-false).
    let tape: Vec<(Op, String)> = vec![
        (
            Op::Insert,
            envelope_json(
                Op::Insert,
                None,
                Some(
                    serde_json::json!({"id": "1", "tenant_id": TENANT, "status": "received", "site": "s-1", "qty": "12.3400"}),
                ),
                1,
                None,
            ),
        ),
        (
            Op::Insert,
            envelope_json(
                Op::Insert,
                None,
                Some(
                    serde_json::json!({"id": "2", "tenant_id": TENANT, "status": "draft", "site": "s-2"}),
                ),
                2,
                None,
            ),
        ),
        (
            Op::Insert,
            envelope_json(
                Op::Insert,
                None,
                Some(
                    serde_json::json!({"id": "3", "tenant_id": "t2", "status": "received", "site": "s-1"}),
                ),
                3,
                None,
            ),
        ),
        (
            Op::Insert,
            envelope_json(
                Op::Insert,
                None,
                Some(serde_json::json!({"id": "4", "status": "received", "site": "s-1"})),
                4,
                None,
            ),
        ),
        (
            Op::Delete,
            envelope_json(
                Op::Delete,
                Some(serde_json::json!({"id": "1"})),
                None,
                5,
                None,
            ),
        ),
        (
            Op::Insert,
            envelope_json(
                Op::Insert,
                None,
                Some(
                    serde_json::json!({"id": "6", "tenant_id": TENANT, "status": "received", "site": "s-1"}),
                ),
                6,
                Some((3, "origin-root")),
            ),
        ),
        (
            Op::Insert,
            envelope_json(
                Op::Insert,
                None,
                Some(
                    serde_json::json!({"id": "7", "tenant_id": TENANT, "status": "received", "site": "s-1"}),
                ),
                7,
                Some((16, "loop-root")),
            ),
        ),
        (
            // E8: an UPDATE carrying a FULL old image (the entity at REPLICA
            // IDENTITY FULL, l5i9.31). r-old subscribes update, reads root `old`,
            // and evaluates `new.status != old.status` → 'received' != 'draft' →
            // TRUE → fires f-old:evt:8 (the end-to-end old-image evaluation).
            Op::Update,
            envelope_json(
                Op::Update,
                Some(
                    serde_json::json!({"id": "8", "tenant_id": TENANT, "status": "draft", "site": "s-1"}),
                ),
                Some(
                    serde_json::json!({"id": "8", "tenant_id": TENANT, "status": "received", "site": "s-1"}),
                ),
                8,
                None,
            ),
        ),
    ];
    for (i, (op, body)) in tape.iter().enumerate() {
        let lsn = (i + 1) as u64;
        let mut headers = async_nats::HeaderMap::new();
        headers.append(
            "Nats-Msg-Id",
            wamn_event_wire::msg_id(PROJECT, ENV, lsn).as_str(),
        );
        js.publish_with_headers(
            wamn_event_wire::subject(ORG, PROJECT, ENV, ENTITY, *op),
            headers,
            body.clone().into(),
        )
        .await
        .context("publish send")?
        .await
        .context("publish ack")?;
    }
    println!("published the 8-event fixture tape (stream seqs 1..=8)");

    // --- Plugins + engine + guest -------------------------------------------
    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    if cfg.database_url.is_none() {
        bail!("no database url: pass --database-url or set DATABASE_URL / WAMN_PG_URL");
    }
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

    let report_dir = std::env::temp_dir().join(format!("wamn-matbench-{}", std::process::id()));
    std::fs::create_dir_all(&report_dir).context("create report dir")?;

    let harness = Harness {
        engine,
        pre,
        pg,
        js: jsp,
        report_dir: report_dir.clone(),
    };

    let mut pass = true;
    let mut check = |name: &str, ok: bool| {
        println!("PASS({name}): {ok}");
        if !ok {
            pass = false;
        }
    };

    // --- Phase 1: decide ------------------------------------------------------
    let t0 = Instant::now();
    let report = harness.run_guest(2, 64).await?;
    println!(
        "phase 1 (decide) guest run: {:?}; report: {report}",
        t0.elapsed()
    );

    // The DB truth (superuser reads, explicit tenant predicates).
    let runs: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?
        .get(0);
    let queued: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?
        .get(0);
    check(
        "9 runs written ahead (3 f-plain + 2 f-cond + 3 f-key + 1 f-old update)",
        runs == 9,
    );
    check("9 queue rows co-transacted", queued == 9);

    // E1 through f-plain: unkeyed, blocking default, REAL stream_seq, padded id.
    let plain_e1 = mint_evt_run_id("f-plain", 1);
    let row = admin
        .query_one(
            "SELECT partition_key, partition_policy, stream_seq::bigint FROM wamn_run.run_queue \
             WHERE tenant_id = $1 AND run_id = $2",
            &[&TENANT, &plain_e1],
        )
        .await
        .with_context(|| format!("queue row {plain_e1}"))?;
    check(
        "unkeyed evt row: NULL key, default policy, stream_seq 1",
        row.get::<_, Option<String>>(0).is_none()
            && row.get::<_, String>(1) == "blocking"
            && row.get::<_, i64>(2) == 1,
    );

    // E1 through f-key: the registration extractor key + declared leapfrog.
    let key_e1 = mint_evt_run_id("f-key", 1);
    let row = admin
        .query_one(
            "SELECT partition_key, partition_policy, stream_seq::bigint FROM wamn_run.run_queue \
             WHERE tenant_id = $1 AND run_id = $2",
            &[&TENANT, &key_e1],
        )
        .await
        .with_context(|| format!("queue row {key_e1}"))?;
    check(
        "keyed evt row: extractor key s-1 + declared leapfrog + stream_seq 1 (kq0z coherence)",
        row.get::<_, Option<String>>(0).as_deref() == Some("s-1")
            && row.get::<_, String>(1) == "leapfrog"
            && row.get::<_, i64>(2) == 1,
    );

    // E6: the causation thread — child depth = parent(3) + 1, root carried.
    let plain_e6 = mint_evt_run_id("f-plain", 6);
    let row = admin
        .query_one(
            "SELECT trigger_source, input_json->'causation'->>'depth', \
                    input_json->'causation'->>'root', input_json->>'trigger' \
             FROM wamn_run.runs WHERE tenant_id = $1 AND run_id = $2",
            &[&TENANT, &plain_e6],
        )
        .await
        .with_context(|| format!("runs row {plain_e6}"))?;
    check(
        "evt run persists trigger_source + the causation thread (depth 4, root carried)",
        row.get::<_, String>(0) == "evt:6"
            && row.get::<_, String>(1) == "4"
            && row.get::<_, String>(2) == "origin-root"
            && row.get::<_, String>(3) == "event",
    );

    // The depth-16 parent (E7) fired NOTHING.
    let e7_runs: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = $1 AND trigger_source = 'evt:7'",
            &[&TENANT],
        )
        .await?
        .get(0);
    check("depth-16 chain fired no run (loop bound)", e7_runs == 0);

    // E8: the old-image evaluation fires end to end (l5i9.31). r-old read root
    // `old`, compared `new.status != old.status` over the FULL old image, and
    // fired a real f-old run — proving the served (no-longer-held) old condition.
    let old_e8 = mint_evt_run_id("f-old", 8);
    let e8 = admin
        .query_one(
            "SELECT trigger_source, input_json->>'trigger' FROM wamn_run.runs \
             WHERE tenant_id = $1 AND run_id = $2",
            &[&TENANT, &old_e8],
        )
        .await
        .with_context(|| format!("old-image fire {old_e8} (E8 must fire under RI FULL)"))?;
    check(
        "old-image UPDATE evaluates end to end and fires (f-old:evt:8)",
        e8.get::<_, String>(0) == "evt:8" && e8.get::<_, String>(1) == "event",
    );

    // The guest's DISTINCT counters (v3 §4 alertable refusals).
    check("report: fired = 9 (8 inserts + the E8 old-image update)", counter(&report, "fired") == 9);
    check(
        "report: condition-false skip counted (E2 on r-cond)",
        counter(&report, "skip-condition-false") == 1,
    );
    check(
        "report: foreign-tenant skips counted (E3 across 4 servings incl. r-old)",
        counter(&report, "skip-foreign-tenant") == 4,
    );
    check(
        "report: unscopable refusals counted (E4 x4 servings + the DELETE on r-plain)",
        counter(&report, "refuse-tenant-unscopable") == 5,
    );
    check(
        "report: depth refusals counted (E7 across 3 servings; r-old refuses old-image first)",
        counter(&report, "refuse-depth") == 3,
    );
    check(
        // l5i9.31: r-old is SERVED, not held; its 4 matching inserts (E1/E2/E6/E7)
        // carry no old image → old-image-absent refusals (cannot-evaluate).
        "report: old-image-absent refusals counted (r-old E1/E2/E6/E7) and NOTHING held",
        counter(&report, "refuse-old-image-absent") == 4
            && counter(&report, "held-registrations") == 0,
    );
    check(
        "report: no duplicates yet",
        counter(&report, "duplicate") == 0,
    );

    // Doorbells: exactly one ring per WON firing, strictly post-commit.
    let mut rings = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while rings.len() < 9 && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), bells.next()).await {
            Ok(Some(msg)) => rings.push(String::from_utf8_lossy(&msg.payload).to_string()),
            _ => break,
        }
    }
    check(
        "9 doorbell rings observed on wamn.doorbell.t1",
        rings.len() == 9,
    );
    check(
        "doorbell payloads are the minted run ids",
        rings.contains(&plain_e1) && rings.contains(&key_e1),
    );

    // --- Phase 2: burst (the first C-MAT number; measured, not gated) --------
    for i in 0..args.burst {
        let lsn = 1000 + i as u64;
        let body = envelope_json(
            Op::Insert,
            None,
            Some(serde_json::json!({
                "id": format!("b{i}"), "tenant_id": TENANT,
                "status": "received", "site": format!("s-{}", i % 8),
            })),
            lsn,
            None,
        );
        let mut headers = async_nats::HeaderMap::new();
        headers.append(
            "Nats-Msg-Id",
            wamn_event_wire::msg_id(PROJECT, ENV, lsn).as_str(),
        );
        js.publish_with_headers(
            wamn_event_wire::subject(ORG, PROJECT, ENV, ENTITY, Op::Insert),
            headers,
            body.into(),
        )
        .await
        .context("burst publish send")?
        .await
        .context("burst publish ack")?;
    }
    let before: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?
        .get(0);
    let t1 = Instant::now();
    let sweeps = (args.burst as u64).div_ceil(64) + 2;
    let report2 = harness.run_guest(sweeps, 64).await?;
    let drain = t1.elapsed();
    let after: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?
        .get(0);
    let new_runs = after - before;
    check(
        "burst drained: 3 runs per event (plain + cond + key)",
        new_runs == (args.burst as i64) * 3,
    );
    println!(
        "C-MAT[local,debug]: {} events -> {} runs in {:.2?} ({:.0} deliveries/s, {:.0} enqueues/s); duplicates {}",
        args.burst,
        new_runs,
        drain,
        args.burst as f64 / drain.as_secs_f64(),
        new_runs as f64 / drain.as_secs_f64(),
        counter(&report2, "duplicate"),
    );

    // --- Phase 3: redeliver (exactly-once past the dedupe window) ------------
    let stream = js.get_stream(STREAM).await.context("get stream")?;
    for rid in ["r-plain", "r-cond", "r-key", "r-old"] {
        // The guest's durable grammar: mat_<tenant>_<catalog>_<registration>
        // ('-' is NATS-legal and survives the guest's sanitize).
        let name = format!("mat_{TENANT}_matcat_{rid}");
        stream
            .delete_consumer(&name)
            .await
            .with_context(|| format!("delete durable {name} (must exist after phase 1)"))?;
    }
    let total_before: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?
        .get(0);
    let report3 = harness.run_guest(sweeps, 64).await?;
    let total_after: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?
        .get(0);
    check(
        "full redelivery mints ZERO new runs (ON CONFLICT exactly-once)",
        total_after == total_before,
    );
    check(
        "redelivery collisions observed (duplicate counter > 0)",
        counter(&report3, "duplicate") > 0,
    );

    // --- Teardown -------------------------------------------------------------
    let _ = js.delete_stream(STREAM).await;
    let _ = admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; DROP SCHEMA IF EXISTS catalog CASCADE;",
        )
        .await;
    let _ = std::fs::remove_dir_all(&report_dir);
    ticker.abort();

    println!("\nmatbench complete — overall PASS: {pass}");
    if !pass {
        bail!("l5i9.17 matbench gate failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard (wamn-l5i9.30): the tape's hand-built `envelope_json` MUST
    /// stay a valid `wamn_event_wire::Envelope` — the frozen wire type. A field
    /// rename/removal on either side (the type's `deny_unknown_fields`, or a
    /// missing required field) fails deserialization here, at `cargo test`,
    /// before the gate ever needs a live JetStream.
    #[test]
    fn tape_envelopes_match_the_frozen_wire_type() {
        // Insert with a new image, no causation.
        let insert = envelope_json(
            Op::Insert,
            None,
            Some(serde_json::json!({"id": "1", "tenant_id": TENANT})),
            1,
            None,
        );
        let env: wamn_event_wire::Envelope =
            serde_json::from_str(&insert).expect("insert tape is a frozen Envelope");
        assert_eq!(env.op, wamn_event_wire::Op::Insert);
        assert_eq!(env.entity.as_deref(), Some(ENTITY));

        // Delete carrying an old key image + a causation stamp (the run id is
        // the frozen mint — proves the causation record shape too).
        let del = envelope_json(
            Op::Delete,
            Some(serde_json::json!({"id": "1"})),
            None,
            5,
            Some((3, "origin-root")),
        );
        let env: wamn_event_wire::Envelope =
            serde_json::from_str(&del).expect("delete tape is a frozen Envelope");
        assert_eq!(env.op, wamn_event_wire::Op::Delete);
        assert_eq!(
            env.causation.expect("stamped").run,
            mint_evt_run_id("parent", 3)
        );
    }
}
