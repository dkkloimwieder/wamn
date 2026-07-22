//! pocsuiteproof — the POC suite gate (wamn-3rj): the F1/F3/F4 test suites AS
//! STORED DATA, seeded into `wamn_run.test_suites`/`test_cases` and then PROVEN
//! REAL by driving each flow once through its own harness path and folding the
//! stored assertions through `wamn_testkit::evaluate`.
//!
//! It is `suiteproof` generalized to the three real POC flows, plus a fixture-
//! realism pass. It is NOT the generic PG-loading executor (sibling lane 0lfu):
//! this gate is hard-wired per-POC-flow so it can drive F1 (which the generic
//! RunWorker-doubles executor cannot — F1's node types are baked into
//! `poc-webhook-f1`, not the flowrunner).
//!
//! Phases:
//!   A. data validity — seed the three embedded `wamn-flow-tests` envelopes into
//!      an ephemeral DATA schema through the SAME `ensure_*` path production
//!      provisioning uses; assert envelope round-trip, suite/case counts, version
//!      binding, the jsonb round-trip + "parses as a `wamn_testkit::TestCase`",
//!      and RLS (a foreign tenant sees zero suites).
//!   B. fixture realism (drive-and-fold) — load each suite's cases back FROM the
//!      seeded `test_cases` (round-trip through PG), drive the flow ONCE, build a
//!      `Captured` fact bundle, and fold every stored assertion:
//!        F1 — `poc-webhook-f1.wasm` over `wasi:http/incoming-handler` (ProxyPre),
//!             sync response body + final DB captured via admin queries.
//!        F3 — `flowrunner.wasm` under the RunWorker test-double set at a fixed
//!             virtual epoch; the 48h cutoff is proven by time-offset arithmetic
//!             against epoch-anchored seed rows (48h in wall-clock milliseconds).
//!        F4 — `flowrunner.wasm` under the test-double set + a real serve-node
//!             hosting `disposition-node.wasm` + a loopback ERP listener; the
//!             egress recorder witnesses EXACTLY the F2 node hop + one ERP
//!             callback and nothing else (the egress-spy invariant).
//!   C. FK cascade — dropping a flow v1 CASCADES its suite + cases (structural
//!      version binding), asserted last (destructive).
//!
//! `--seed-only` runs Phase A alone and KEEPS the schema — the seeding path the
//! wave-end composition gate points at (`--schema poc_f1 --tenant … --flow-version 1`).
//!
//! What the STORED suites deliberately do NOT restate (kept in the sibling proof
//! gates, not expressible in the stored vocabulary):
//!   * F3 credential-delivery digest + cycle-visit count — `f3proof`.
//!   * F4 429/Retry-After park, no-reclaim-during-backoff, one-effective-delivery
//!     under an idempotency key, no-stampede — `f4proof` (the ERP ledger is an
//!     in-memory audit, not a DB table; there is no retry/429/idempotency
//!     assertion in the vocabulary). The stored F4 case asserts only the egress
//!     spy + run outcome, exercising the ERP-callback path with the
//!     `idempotency-key` config PRESENT in the registered graph.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use bytes::Bytes;
use clap::Args;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::host::http::HostHandler;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi_http::p2::WasiHttpView;
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::bindings::http::types::{ErrorCode, Scheme};

use wamn_ctl::publish_catalog::{
    ensure_flow_registry, ensure_flow_tests, ensure_runstate, register_flow, seed_dataset_sql,
};
use wamn_flow_tests::TestSuite;
use wamn_gate_harness::{
    check, scope_session, seed_flow_version, seed_flow_version_if_absent, seed_test_case,
    seed_test_suite,
};
use wamn_host::doubles::{DoubleSet, EgressRecorder, EphemeralSchemaProvisioner};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_credentials::WamnCredentials;
use wamn_host::plugins::wamn_logging::WamnLogging;
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};
use wamn_host::serve_node::{self, ServeNode, ServeNodeAuthn};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};
use wamn_run_worker::{RunWorker, RunnerIdentity};
use wamn_testkit::{
    Assertion, Captured, DbCapture, Outcome, RunFacts, RunStatus, TestCase, evaluate,
};

use crate::erp_sim::ErpAudit;
use crate::f1fixture::{self, F1_FLOW_JSON, F1_SEED_JSON, F1_TENANT};

// The three committed suite envelopes — the canonical source the wave-end
// composition gate + a copy-project-env promotion also carry.
const F1_SUITE_JSON: &str = include_str!("../../../deploy/gates/poc-f1-suite.json");
const F3_SUITE_JSON: &str = include_str!("../../../deploy/gates/poc-f3-suite.json");
const F4_SUITE_JSON: &str = include_str!("../../../deploy/gates/poc-f4-suite.json");

const F1_FLOW_ID: &str = "receipt-received";
const F3_FLOW_ID: &str = "escalate-stale-holds";
const F4_FLOW_ID: &str = "disposition-recorded";

/// The virtual-clock epoch + seed the flow-level drives run under (matching the
/// run-worker `--test-doubles` constants / testkitbench).
const EPOCH_SECS: u64 = 1_700_000_000;
const SEED: u64 = 0x7492_5EED_5EED_7492;

/// F3 seed anchoring (unix seconds, relative to the virtual epoch — the
/// load-bearing mechanic, see the module + f3proof docs). The stale holds sit
/// 49h before the epoch, the fresh one AT the epoch; the flow's `time-shift`
/// cutoff is `fire-at-ms − 48h`, so the two stale holds fall before the cutoff
/// and the fresh one after it.
const F3_STALE_OPENED_SECS: i64 = EPOCH_SECS as i64 - 49 * 3600;
const F3_FRESH_OPENED_SECS: i64 = EPOCH_SECS as i64;
/// The real −48h `time-shift` offset — the "48h" the spec asks for, evaluated in
/// wall-clock milliseconds under the virtual clock.
const F3_OFFSET_MS: i64 = -172_800_000;

#[derive(Debug, Args)]
pub struct PocSuiteProofArgs {
    /// Path to poc_webhook_f1.wasm (the F1 sync-webhook ingress component).
    #[arg(long, default_value = "/bench/poc-webhook-f1.wasm")]
    pub webhook_entry: PathBuf,

    /// Path to flowrunner.wasm (drives F3 + F4 under the test-double set).
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// Path to disposition_node.wasm (the F2 node the F4 serve-node hop hosts).
    #[arg(long, default_value = "/bench/disposition-node.wasm")]
    pub node: PathBuf,

    /// App (wamn_app, NOSUPERUSER) Postgres URL. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — provisions the ephemeral schemas + runs the DB-state
    /// assertion queries (env WAMN_PG_ADMIN_URL).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The DATA schema this gate seeds the three suites into (owns end to end,
    /// dropped at the end unless --keep / --seed-only). The composition gate
    /// points this at its shared target (e.g. `poc_f1`).
    #[arg(long, default_value = "wamn_pocsuiteproof")]
    pub schema: String,

    /// The owning tenant the suites are seeded under.
    #[arg(long, default_value = "demo-tenant")]
    pub tenant: String,

    /// A second tenant that must see ZERO suites (RLS negative).
    #[arg(long, default_value = "other-tenant")]
    pub other_tenant: String,

    /// The flow version the suites bind (documented; the envelopes pin v1).
    #[arg(long, default_value_t = 1)]
    pub flow_version: i32,

    /// Loopback port the F4 serve-node HTTP host binds (runner→node hop).
    #[arg(long, default_value_t = 18191)]
    pub node_port: u16,

    /// Loopback port the F4 ERP callback listener binds.
    #[arg(long, default_value_t = 18192)]
    pub erp_port: u16,

    /// Seed the three suites into --schema/--tenant and STOP — no drive, no
    /// FK-cascade, no drop. The composition-gate seeding path.
    #[arg(long)]
    pub seed_only: bool,

    /// Keep every schema at the end (default drops them).
    #[arg(long)]
    pub keep: bool,
}

// ---------------------------------------------------------------------------
// Graph copies (surgical: 3rj carries its own, coherence-tested against the
// committed source fixtures — see the tests module — rather than widening the
// sibling gates' private `gate_flow_json` visibility).
// ---------------------------------------------------------------------------

/// The F3 gate flow — the committed `deploy/poc/f3-flow.json` shape (time-shift →
/// list → gate → {escalate → notify (dead-end), advance → gate}) with the notify
/// url + allowed-host bound to `echo_host` and the offset as a signed ms.
fn f3_gate_flow_json(echo_host: &str, offset_ms: i64) -> String {
    format!(
        r#"{{
  "schema-version": "0.1",
  "flow-id": "{F3_FLOW_ID}",
  "version": 1,
  "name": "F3 escalate-stale-holds (pocsuiteproof)",
  "trigger": {{ "type": "cron", "schedule": "* * * * *" }},
  "entry": "shift",
  "nodes": [
    {{ "id": "shift", "type": "time-shift",
       "config": {{ "base": "\"fire-at-ms\"", "offset-ms": {offset_ms}, "format": "iso", "key": "cutoff" }} }},
    {{ "id": "list-stale", "type": "postgres",
       "config": {{ "entity": "quality_holds", "op": "list",
                    "filters": {{ "status": "eq.open", "opened_at": "lt.{{{{cutoff}}}}" }},
                    "sort": "opened_at", "limit": 500 }} }},
    {{ "id": "gate", "type": "conditional", "config": {{ "expression": "length(@) > `0`" }} }},
    {{ "id": "escalate", "type": "postgres",
       "config": {{ "entity": "quality_holds", "op": "update", "id": "[0].id", "body": "{{status: 'escalated'}}" }} }},
    {{ "id": "notify", "type": "http-request", "credential": "notify-webhook",
       "config": {{ "method": "POST", "url": "http://{echo_host}/holds",
                    "body": "{{hold: id, status: status, opened_at: opened_at}}" }} }},
    {{ "id": "advance", "type": "transform", "config": {{ "expression": "[1:]" }} }},
    {{ "id": "done", "type": "respond" }}
  ],
  "edges": [
    {{ "from": "shift", "to": "list-stale" }},
    {{ "from": "list-stale", "to": "gate" }},
    {{ "from": "gate", "from-port": "true", "to": "escalate" }},
    {{ "from": "gate", "from-port": "true", "to": "advance" }},
    {{ "from": "escalate", "to": "notify" }},
    {{ "from": "advance", "to": "gate" }},
    {{ "from": "gate", "from-port": "false", "to": "done" }}
  ],
  "credentials": [ {{ "name": "notify-webhook", "kind": "api-key" }} ],
  "allowed-hosts": ["{echo_host}"]
}}"#
    )
}

/// The minimal quality_holds catalog the F3 `postgres` node compiles against.
fn f3_holds_catalog_json() -> String {
    json!({
        "schema-version": "0.1",
        "catalog-id": "poc-f3",
        "version": 1,
        "entities": [
            { "id": "quality_holds", "name": "quality_holds", "fields": [
                { "id": "status", "name": "status",
                  "type": { "kind": "enum", "variants": ["open", "disposed", "escalated"] } },
                { "id": "opened_at", "name": "opened_at", "type": { "kind": "timestamptz" } }
            ]}
        ]
    })
    .to_string()
}

/// The F3 entity table + catalog snapshot table, under the tenant RLS floor.
fn f3_holds_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.quality_holds ( \
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
           tenant_id text NOT NULL, \
           status text NOT NULL DEFAULT 'open' CHECK (status IN ('open','disposed','escalated')), \
           opened_at timestamptz NOT NULL DEFAULT now()); \
         ALTER TABLE {schema}.quality_holds ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {schema}.quality_holds FORCE ROW LEVEL SECURITY; \
         CREATE POLICY quality_holds_tenant ON {schema}.quality_holds \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.quality_holds TO wamn_app; \
         CREATE TABLE {schema}.wamn_catalog ( \
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
           tenant_id text NOT NULL, document jsonb NOT NULL); \
         ALTER TABLE {schema}.wamn_catalog ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {schema}.wamn_catalog FORCE ROW LEVEL SECURITY; \
         CREATE POLICY wamn_catalog_tenant ON {schema}.wamn_catalog \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         GRANT SELECT ON {schema}.wamn_catalog TO wamn_app;"
    )
}

/// The F4 gate flow — the f4proof runnable shape (row-event insert → shape →
/// recommend (F2 node hop) → callback (ERP POST, idempotency-key ON)), with both
/// loopback hops declared egress. NO credential (the sim needs no auth — the
/// vault is out of this egress-spy proof).
fn f4_gate_flow_json(node_port: u16, erp_port: u16) -> String {
    json!({
        "schema-version": "0.1",
        "flow-id": F4_FLOW_ID,
        "version": 1,
        "name": "F4 disposition-recorded (pocsuiteproof)",
        "trigger": { "type": "row-event", "table": "dispositions", "event": "insert" },
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

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

pub async fn run(args: PocSuiteProofArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    for s in [
        &args.schema,
        &f1_schema(&args),
        &f3_schema(&args),
        &f4_schema(&args),
    ] {
        if !is_bare_ident(s) {
            bail!("--schema must be a bare identifier [a-z_][a-z0-9_]*: {s:?}");
        }
    }
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    println!(
        "# wamn-gates pocsuiteproof — F1/F3/F4 stored suites (schema {}, tenant {}, flow-version {})",
        args.schema, args.tenant, args.flow_version
    );

    // --- the three embedded suites parse + validate-on-write (pure) ---
    let f1 = TestSuite::from_json(F1_SUITE_JSON).context("poc-f1-suite envelope")?;
    let f3 = TestSuite::from_json(F3_SUITE_JSON).context("poc-f3-suite envelope")?;
    let f4 = TestSuite::from_json(F4_SUITE_JSON).context("poc-f4-suite envelope")?;
    let suites = [&f1, &f3, &f4];

    // --- Phase A: provision the DATA schema + seed all three suites ---
    let (admin, admin_task) = connect(&admin_url).await?;
    if args.seed_only {
        // seed-only targets a LIVE schema (the composition path seeds poc_f1):
        // NEVER drop it — additively ensure the run-plane + flow-test tables via
        // the same IF-NOT-EXISTS `ensure_*` path production reconcile uses.
        ensure_runstate(&admin, &args.schema)
            .await
            .context("ensure run-state (additive)")?;
        ensure_flow_registry(&admin, &args.schema)
            .await
            .context("ensure flow registry (additive)")?;
        ensure_flow_tests(&admin, &args.schema)
            .await
            .context("ensure flow-test tables (additive)")?;
        println!(
            "## seed-only: additive ensure on live schema {} (no drop)",
            args.schema
        );
    } else {
        provision_data_schema(&admin, &args.schema).await?;
    }

    let (app, app_task) = connect(&app_url).await?;
    scope_session(&app, &args.tenant, &args.schema).await?;
    for suite in suites {
        // If-absent: a LIVE target (seed-only into poc_f1) keeps its
        // production-registered graph_json/active untouched.
        seed_flow_version_if_absent(
            &app,
            &args.tenant,
            &suite.flow_id,
            args.flow_version,
            true,
            &data_schema_graph(&suite.flow_id),
        )
        .await
        .with_context(|| format!("register flow {}", suite.flow_id))?;
        seed_test_suite(
            &app,
            &args.tenant,
            &suite.flow_id,
            args.flow_version,
            &suite.suite_id,
            &suite.name,
        )
        .await
        .with_context(|| format!("seed suite {}", suite.suite_id))?;
        for case in &suite.cases {
            seed_test_case(
                &app,
                &args.tenant,
                &suite.flow_id,
                args.flow_version,
                &suite.suite_id,
                &case.case_id,
                case.ordinal as i32,
                &case.case.to_string(),
            )
            .await
            .with_context(|| format!("seed case {}/{}", suite.suite_id, case.case_id))?;
        }
    }

    let mut ok = true;
    println!("\n## data — envelope round-trip + seed + version binding + RLS");
    for suite in suites {
        let round_trips = TestSuite::from_json(&suite.to_json()).is_ok_and(|s| &s == suite);
        check(
            &mut ok,
            &format!("ENVELOPE: {} round-trips to_json/from_json", suite.suite_id),
            round_trips,
        );
    }
    let n_suites: i64 = scalar(&app, "SELECT count(*) FROM test_suites").await?;
    let n_cases: i64 = scalar(&app, "SELECT count(*) FROM test_cases").await?;
    let want_cases: i64 = suites.iter().map(|s| s.cases.len() as i64).sum();
    check(
        &mut ok,
        &format!("STORE: three POC suites seeded (got {n_suites})"),
        n_suites == 3,
    );
    check(
        &mut ok,
        &format!("STORE: {want_cases} cases seeded across the suites (got {n_cases})"),
        n_cases == want_cases,
    );
    let bound: i64 = scalar(
        &app,
        &format!(
            "SELECT count(*) FROM test_cases WHERE flow_version = {}",
            args.flow_version
        ),
    )
    .await?;
    check(
        &mut ok,
        &format!(
            "BIND: every case pins flow_version = {} (got {bound})",
            args.flow_version
        ),
        bound == want_cases,
    );
    // The opaque case body reached jsonb intact AND parses as a canonical
    // wamn-testkit TestCase (validate-on-write).
    let stored: Value = app
        .query_one(
            "SELECT case_body FROM test_cases WHERE flow_id = $1 AND case_id = $2",
            &[&F4_FLOW_ID, &"exactly-one-erp-callback"],
        )
        .await
        .context("read a stored case body")?
        .get(0);
    check(
        &mut ok,
        "STORE: opaque case body round-trips through jsonb intact",
        stored == f4.cases[0].case,
    );
    check(
        &mut ok,
        "STORE: stored case body parses as a wamn-testkit TestCase",
        serde_json::from_value::<TestCase>(stored).is_ok(),
    );
    // RLS: a second tenant's claim sees ZERO suites.
    let (other, other_task) = connect(&app_url).await?;
    scope_session(&other, &args.other_tenant, &args.schema).await?;
    let other_sees: i64 = scalar(&other, "SELECT count(*) FROM test_suites").await?;
    check(
        &mut ok,
        &format!("RLS: a foreign tenant sees no suites (got {other_sees})"),
        other_sees == 0,
    );
    drop(other);
    let _ = other_task.await;

    if args.seed_only {
        drop(app);
        let _ = app_task.await;
        drop(admin);
        let _ = admin_task.await;
        println!(
            "\n## seed-only — 3 POC suites seeded into {}/{} (kept); no drive, no drop",
            args.schema, args.tenant
        );
        println!("\npocsuiteproof (seed-only) complete — overall PASS: {ok}");
        if !ok {
            bail!("pocsuiteproof seed-only failed");
        }
        return Ok(());
    }

    // --- Phase B: fixture realism — load cases FROM PG, drive, fold ---
    let f1_cases = load_cases(&app, F1_FLOW_ID).await?;
    let f3_cases = load_cases(&app, F3_FLOW_ID).await?;
    let f4_cases = load_cases(&app, F4_FLOW_ID).await?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    ok &= drive_f1(&engine, &args, &app_url, &admin_url, &f1_cases).await?;
    ok &= drive_f3(&engine, &args, &app_url, &admin_url, &f3_cases).await?;
    ok &= drive_f4(&engine, &args, &app_url, &admin_url, &f4_cases).await?;

    ticker.abort();

    // --- Phase C: FK cascade (destructive, last) in the DATA schema ---
    println!("\n## fk — dropping a flow v1 cascades its suite + cases");
    scope_session(&app, &args.tenant, &args.schema).await?;
    app.execute(
        "DELETE FROM flows WHERE tenant_id = $1 AND flow_id = $2 AND version = $3",
        &[&args.tenant, &F4_FLOW_ID, &args.flow_version],
    )
    .await
    .context("delete F4 flow v1")?;
    let after_suites: i64 = scalar(&app, "SELECT count(*) FROM test_suites").await?;
    let f4_suites: i64 = scalar(
        &app,
        &format!("SELECT count(*) FROM test_suites WHERE flow_id = '{F4_FLOW_ID}'"),
    )
    .await?;
    check(
        &mut ok,
        &format!(
            "FK: the dropped flow's suite cascaded (F4 suites now {f4_suites}), 2 remain (got {after_suites})"
        ),
        f4_suites == 0 && after_suites == 2,
    );

    drop(app);
    let _ = app_task.await;

    if !args.keep {
        admin
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", args.schema))
            .await
            .context("drop DATA schema")?;
    }
    drop(admin);
    let _ = admin_task.await;

    println!("\npocsuiteproof complete — overall PASS: {ok}");
    if !ok {
        bail!("pocsuiteproof failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// F1 drive — poc-webhook-f1 over wasi:http/incoming-handler (ProxyPre)
// ---------------------------------------------------------------------------

const F1_BENCH_ID: &str = "pocsuite-f1";

async fn drive_f1(
    engine: &wash_runtime::engine::Engine,
    args: &PocSuiteProofArgs,
    app_url: &str,
    admin_url: &str,
    cases: &[TestCase],
) -> anyhow::Result<bool> {
    println!(
        "\n## F1 drive — {} case(s) via poc-webhook-f1 (sync webhook), body+DB via admin queries",
        cases.len()
    );
    let schema = f1_schema(args);
    provision_f1(admin_url, &schema).await?;

    let webhook_wasm = std::fs::read(&args.webhook_entry)
        .with_context(|| format!("read {}", args.webhook_entry.display()))?;
    let harness = ProxyHarness::new(engine.clone(), &webhook_wasm)?;

    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.to_string());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    plugin.set_tenant(F1_BENCH_ID, F1_TENANT)?;
    plugin.set_schema(F1_BENCH_ID, &schema)?;
    plugin.probe_checkout().await?;

    let (admin, admin_task) = connect(admin_url).await?;
    scope_session(&admin, F1_TENANT, &schema).await?;

    let mut ok = true;
    for case in cases {
        // Drive: POST the case input to the sync webhook.
        let (status, _body) = harness
            .request(&plugin, "POST", "/receipts", Some(case.input.clone()))
            .await
            .with_context(|| format!("F1 request for case {}", case.name))?;
        // Capture the run outcome (from runs.status, keyed by the input's
        // receipt_no) + each DbState query the case reads.
        let receipt_no = case.input["receipt_no"].as_str().unwrap_or_default();
        let run_status: Option<String> = admin
            .query_opt(
                "SELECT status FROM runs WHERE input_json->>'receipt_no' = $1",
                &[&receipt_no],
            )
            .await?
            .map(|r| r.get(0));
        let mut captured = Captured {
            run: run_status.as_deref().and_then(run_facts),
            ..Default::default()
        };
        for a in &case.expect {
            if let Assertion::DbState { query, params, .. } = a {
                let rows = admin_query_json(&admin, query, params)
                    .await
                    .with_context(|| format!("F1 db-state query for {}", case.name))?;
                captured.db.push(DbCapture {
                    query: query.clone(),
                    params: params.clone(),
                    rows,
                });
            }
        }
        println!("  F1 {} — http status {status}", case.name);
        fold_outcome(&mut ok, &evaluate(case, &captured));
    }

    drop(admin);
    let _ = admin_task.await;
    drop(plugin);
    if !args.keep {
        drop_schema(admin_url, &schema).await.ok();
    }
    Ok(ok)
}

/// Provision the ephemeral F1 world (floor + run-state + flow registry + catalog
/// snapshot + business seed + the registered/active F1 flow) — the f1bench
/// provisioning path, trimmed of its collision-check drill.
async fn provision_f1(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    let (client, conn_task) = connect(admin_url).await?;
    let result = async {
        client
            .batch_execute(
                "DO $$ BEGIN \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                     CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
                   END IF; \
                 END $$;",
            )
            .await
            .context("ensure wamn_app role")?;
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                 CREATE SCHEMA {schema} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {schema} TO wamn_app; \
                 SET search_path TO {schema};"
            ))
            .await
            .context("create ephemeral F1 schema")?;
        client
            .batch_execute(&f1fixture::floor_ddl()?)
            .await
            .context("apply F1 floor")?;
        ensure_runstate(&client, schema).await?;
        ensure_flow_registry(&client, schema).await?;
        client
            .batch_execute(
                "CREATE TABLE wamn_catalog ( \
                   id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
                   tenant_id text NOT NULL, \
                   document jsonb NOT NULL); \
                 ALTER TABLE wamn_catalog ENABLE ROW LEVEL SECURITY; \
                 ALTER TABLE wamn_catalog FORCE ROW LEVEL SECURITY; \
                 CREATE POLICY wamn_catalog_tenant ON wamn_catalog \
                   USING (tenant_id = current_setting('app.tenant', true)) \
                   WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
                 GRANT SELECT ON wamn_catalog TO wamn_app;",
            )
            .await
            .context("create wamn_catalog")?;
        client
            .execute(
                "INSERT INTO wamn_catalog (tenant_id, document) VALUES ($1, $2::text::jsonb)",
                &[&F1_TENANT, &f1fixture::catalog()?.to_json()],
            )
            .await
            .context("write catalog snapshot")?;
        let seed = seed_dataset_sql(F1_SEED_JSON, &f1fixture::catalog()?, F1_TENANT)?;
        client.batch_execute(&seed).await.context("apply F1 seed")?;
        register_flow(&client, F1_TENANT, F1_FLOW_JSON)
            .await
            .context("register F1 flow")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

/// A minimal ProxyPre harness (the f1bench/apibench pattern) — compile the
/// component once, then drive one `wasi:http/incoming-handler` request per call.
struct ProxyHarness {
    engine: wash_runtime::engine::Engine,
    pre: ProxyPre<SharedCtx>,
}

impl ProxyHarness {
    fn new(engine: wash_runtime::engine::Engine, guest: &[u8]) -> anyhow::Result<Self> {
        let raw: &RawEngine = engine.inner();
        let component = WasmtimeComponent::new(raw, guest)
            .map_err(|e| anyhow::anyhow!("compile component: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        wamn_postgres::add_to_linker(&mut linker)?;
        let pre = ProxyPre::new(linker.instantiate_pre(&component)?)?;
        Ok(Self { engine, pre })
    }

    async fn request(
        &self,
        plugin: &Arc<WamnPostgres>,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(u16, Value)> {
        let body_bytes = match body {
            Some(v) => serde_json::to_vec(&v)?,
            None => Vec::new(),
        };
        let mut plugins: std::collections::HashMap<
            &'static str,
            Arc<dyn HostPlugin + Send + Sync>,
        > = std::collections::HashMap::new();
        plugins.insert(
            wamn_postgres::WAMN_POSTGRES_ID,
            plugin.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        let ctx = Ctx::builder(F1_BENCH_ID.to_string(), F1_BENCH_ID.to_string())
            .with_plugins(plugins)
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);

        let body: BoxBody<Bytes, ErrorCode> = Full::new(Bytes::from(body_bytes))
            .map_err(|e| match e {})
            .boxed();
        let req = hyper::Request::builder()
            .method(method)
            .uri(uri)
            .header(hyper::header::HOST, "f1.local")
            .body(body)
            .context("build request")?;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let req_res = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, req)?;
        let out_res = store.data_mut().http().new_response_outparam(tx)?;

        let pre = self.pre.clone();
        let task = tokio::task::spawn(async move {
            let proxy = pre.instantiate_async(&mut store).await?;
            proxy
                .wasi_http_incoming_handler()
                .call_handle(&mut store, req_res, out_res)
                .await
        });

        let resp = match rx.await {
            Ok(Ok(resp)) => resp,
            Ok(Err(code)) => {
                task.await??;
                bail!("guest set an error code: {code:?}");
            }
            Err(_) => {
                task.await??;
                bail!("guest never set the response outparam");
            }
        };
        let status = resp.status().as_u16();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("collect response body: {e}"))?
            .to_bytes();
        task.await??;
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Ok((status, json))
    }
}

// ---------------------------------------------------------------------------
// F3 drive — flowrunner under the test-double set at a fixed virtual epoch
// ---------------------------------------------------------------------------

async fn drive_f3(
    engine: &wash_runtime::engine::Engine,
    args: &PocSuiteProofArgs,
    app_url: &str,
    admin_url: &str,
    cases: &[TestCase],
) -> anyhow::Result<bool> {
    println!(
        "\n## F3 drive — {} case(s) via flowrunner doubles at virtual epoch {EPOCH_SECS} (48h cutoff)",
        cases.len()
    );
    let schema = f3_schema(args);
    let tenant: &str = &args.tenant;
    let flowrunner = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read {}", args.flowrunner.display()))?;

    // The notify egress target: a loopback echo that answers 200 to any POST.
    let (echo_addr, echo_task) = crate::testhostbench::spawn_echo().await?;
    let echo_authority = format!("127.0.0.1:{}", echo_addr.port());

    // Provision: run-state (runner_ddl) + the holds world + catalog snapshot.
    let provisioner =
        EphemeralSchemaProvisioner::connect(admin_url, crate::runnerbench::runner_ddl).await?;
    provisioner.provision_case(&schema).await?;
    let admin = provisioner.admin();
    admin
        .batch_execute(&f3_holds_ddl(&schema))
        .await
        .context("apply F3 holds DDL")?;
    scope_session(admin, tenant, &schema).await?;
    admin
        .execute(
            "INSERT INTO wamn_catalog (tenant_id, document) VALUES ($1, $2::text::jsonb)",
            &[&tenant, &f3_holds_catalog_json()],
        )
        .await
        .context("write F3 catalog snapshot")?;
    // Anchor the seed to the epoch (the load-bearing mechanic): 2 stale + 1
    // fresh + 1 stale-disposed hold.
    admin
        .execute(
            &format!(
                "INSERT INTO quality_holds (tenant_id, status, opened_at) VALUES \
                   ($1, 'open', to_timestamp({stale})), \
                   ($1, 'open', to_timestamp({stale})), \
                   ($1, 'open', to_timestamp({fresh})), \
                   ($1, 'disposed', to_timestamp({stale}))",
                stale = F3_STALE_OPENED_SECS,
                fresh = F3_FRESH_OPENED_SECS,
            ),
            &[&tenant],
        )
        .await
        .context("seed F3 holds")?;
    seed_flow_version(
        admin,
        tenant,
        F3_FLOW_ID,
        1,
        true,
        &f3_gate_flow_json(&echo_authority, F3_OFFSET_MS),
        true,
    )
    .await
    .context("register F3 flow")?;

    // The single case's input carries fire-at-ms (the cutoff base).
    let case = &cases[0];
    let run_id = "pocsuite-f3-0";
    admin
        .execute(
            &write_ahead_triggered_run_sql(),
            &[
                &run_id,
                &F3_FLOW_ID,
                &1i32,
                &"cron",
                &case.input.to_string(),
            ],
        )
        .await
        .context("seed F3 run")?;
    admin
        .execute(
            &enqueue_sql(),
            &[&run_id, &Option::<&str>::None, &0i32, &0i64],
        )
        .await
        .context("enqueue F3 run")?;

    // Drive under the test-double set: virtual clock + a spying egress recorder
    // that expects only the echo (owner == flow_id — the portable egress key).
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.to_string());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    let recorder = Arc::new(EgressRecorder::spying());
    recorder.expect(F3_FLOW_ID, [echo_authority.clone()]);
    let (doubles, _clock) =
        DoubleSet::virtual_host(EPOCH_SECS, SEED, recorder.clone() as Arc<dyn HostHandler>);
    // The notify node declares `credential: notify-webhook`; give the vault a
    // project-"default" entry so it resolves (the RunWorker sets owner→project
    // "default"). The stored suite asserts only that the notify EGRESS happens
    // (count 2) — the credential-delivery DIGEST proof stays in f3proof.
    let vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            "default".to_string(),
            std::collections::HashMap::from([(
                "notify-webhook".to_string(),
                "pocsuite-f3-notify-secret".to_string(),
            )]),
        )]),
    ));
    let mut worker = RunWorker::instantiate(
        engine,
        &flowrunner,
        plugin.clone(),
        vault,
        Arc::new(WamnLogging::from_env()?),
        RunnerIdentity {
            owner: F3_FLOW_ID,
            tenant,
            schema: Some(schema.as_str()),
            project: "default",
        },
        Arc::from([]),
        30_000,
        Some(doubles),
    )
    .await
    .context("instantiate F3 RunWorker")?;
    drain_to_terminal(&mut worker, admin, run_id).await?;
    println!("  F3 drained, egress={:?}", recorder.records());

    let captured = build_flow_captured(&recorder, admin, run_id, case).await?;
    let mut ok = true;
    fold_outcome(&mut ok, &evaluate(case, &captured));

    drop(worker);
    drop(plugin);
    echo_task.abort();
    if !args.keep {
        provisioner.drop_case(&schema).await.ok();
    }
    Ok(ok)
}

// ---------------------------------------------------------------------------
// F4 drive — flowrunner doubles + a real serve-node hop + a loopback ERP sink
// ---------------------------------------------------------------------------

async fn drive_f4(
    engine: &wash_runtime::engine::Engine,
    args: &PocSuiteProofArgs,
    app_url: &str,
    admin_url: &str,
    cases: &[TestCase],
) -> anyhow::Result<bool> {
    println!(
        "\n## F4 drive — {} case(s) via flowrunner doubles + serve-node hop + loopback ERP (egress spy)",
        cases.len()
    );
    let schema = f4_schema(args);
    let tenant: &str = &args.tenant;
    let flowrunner = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read {}", args.flowrunner.display()))?;
    let node_wasm =
        std::fs::read(&args.node).with_context(|| format!("read {}", args.node.display()))?;

    // Provision run-state (no business tables — F4's flow touches none).
    let provisioner =
        EphemeralSchemaProvisioner::connect(admin_url, crate::runnerbench::runner_ddl).await?;
    provisioner.provision_case(&schema).await?;
    let admin = provisioner.admin();
    scope_session(admin, tenant, &schema).await?;
    seed_flow_version(
        admin,
        tenant,
        F4_FLOW_ID,
        1,
        true,
        &f4_gate_flow_json(args.node_port, args.erp_port),
        true,
    )
    .await
    .context("register F4 flow")?;
    let case = &cases[0];
    let run_id = "pocsuite-f4-0";
    admin
        .execute(
            &write_ahead_triggered_run_sql(),
            &[
                &run_id,
                &F4_FLOW_ID,
                &1i32,
                &"evt:0",
                &case.input.to_string(),
            ],
        )
        .await
        .context("seed F4 run")?;
    admin
        .execute(
            &enqueue_sql(),
            &[&run_id, &Option::<&str>::None, &0i32, &0i64],
        )
        .await
        .context("enqueue F4 run")?;

    // The serve-node (keyless, network-trust) hosting the F2 node + a loopback
    // ERP sink that answers 202 immediately (no throttle — the egress-spy case
    // needs only the callback to succeed; the 429/park mechanics stay in f4proof).
    let serve = Arc::new(
        ServeNode::new(
            engine,
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
        .context("build F4 serve-node")?,
    );
    let erp = ErpAudit::new(0, 2);
    let erp_task = tokio::spawn(crate::erp_sim::serve(erp.clone(), args.erp_port));

    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.to_string());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    let recorder = Arc::new(EgressRecorder::spying());
    recorder.expect(
        F4_FLOW_ID,
        [
            format!("127.0.0.1:{}", args.node_port),
            format!("127.0.0.1:{}", args.erp_port),
        ],
    );
    let (doubles, _clock) =
        DoubleSet::virtual_host(EPOCH_SECS, SEED, recorder.clone() as Arc<dyn HostHandler>);
    let worker = RunWorker::instantiate(
        engine,
        &flowrunner,
        plugin.clone(),
        Arc::new(WamnCredentials::empty()),
        Arc::new(WamnLogging::from_env()?),
        RunnerIdentity {
            owner: F4_FLOW_ID,
            tenant,
            schema: Some(schema.as_str()),
            project: "default",
        },
        Arc::from([]),
        30_000,
        Some(doubles),
    )
    .await
    .context("instantiate F4 RunWorker")?;

    // The serve-node accept loop's wasmtime store is !Send (cannot be spawned);
    // drive the gate and the accept loop on the SAME task via select! (the
    // f4proof pattern). The ERP sink is Send + already spawned.
    let serve_loop = serve_node::serve(serve.clone(), args.node_port);
    let gate = drive_f4_gate(worker, &recorder, admin, run_id, case);
    let outcome = tokio::select! {
        r = serve_loop => r.map(|_| false),
        r = gate => r,
    };

    erp_task.abort();
    drop(plugin);
    if !args.keep {
        provisioner.drop_case(&schema).await.ok();
    }
    outcome
}

/// The F4 gate body: drain to terminal (on the SAME task as the serve-node accept
/// loop, via the caller's select!), then build + fold the captured egress/run
/// facts. The ERP sink answers 202 on the first request, so this completes in one
/// drain — but drain-to-terminal keeps it uniform with F3's cyclic drain.
async fn drive_f4_gate(
    mut worker: RunWorker,
    recorder: &Arc<EgressRecorder>,
    admin: &Client,
    run_id: &str,
    case: &TestCase,
) -> anyhow::Result<bool> {
    drain_to_terminal(&mut worker, admin, run_id).await?;
    println!("  F4 drained, egress={:?}", recorder.records());
    let captured = build_flow_captured(recorder, admin, run_id, case).await?;
    let mut ok = true;
    fold_outcome(&mut ok, &evaluate(case, &captured));
    drop(worker);
    Ok(ok)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Whether a run status is terminal (the run-worker will not touch it again).
fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "infrastructure-failure"
    )
}

/// Drain the run_queue REPEATEDLY until the seeded run reaches a terminal status
/// (a real runner-loop): a structural cycle (F3's `gate → advance → gate`)
/// checkpoints + re-queues the run each iteration, so one `drain()` does only a
/// partial traversal. The run parks with an epoch-based `available_at` (2023,
/// under the virtual clock) that is already past the DB clock, so each re-drain
/// re-claims immediately. Capped so a non-terminating flow fails loudly.
async fn drain_to_terminal(
    worker: &mut RunWorker,
    admin: &Client,
    run_id: &str,
) -> anyhow::Result<()> {
    for _ in 0..64 {
        let report = worker.drain().await.context("drain run_queue")?;
        let status: Option<String> = admin
            .query_opt("SELECT status FROM runs WHERE run_id = $1", &[&run_id])
            .await?
            .map(|r| r.get(0));
        if status.as_deref().is_some_and(is_terminal) {
            break;
        }
        if report.claimed == 0 {
            // Nothing progressed this pass and the run is not terminal — a park
            // whose horizon has not passed, or a stall. Give the DB clock a beat.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    Ok(())
}

/// Build the captured fact bundle a flow-level (RunWorker doubles) case reads:
/// the run outcome (the PERSISTED `runs.status`), the egress audit log, and each
/// DbState query's rows (via the admin pool, tenant+schema-scoped).
async fn build_flow_captured(
    recorder: &Arc<EgressRecorder>,
    admin: &Client,
    run_id: &str,
    case: &TestCase,
) -> anyhow::Result<Captured> {
    let run_status: Option<String> = admin
        .query_opt("SELECT status FROM runs WHERE run_id = $1", &[&run_id])
        .await?
        .map(|r| r.get(0));
    let mut captured = Captured {
        run: run_status.as_deref().and_then(run_facts),
        egress: recorder.records(),
        ..Default::default()
    };
    for a in &case.expect {
        if let Assertion::DbState { query, params, .. } = a {
            let rows = admin_query_json(admin, query, params)
                .await
                .with_context(|| format!("db-state query for case {}", case.name))?;
            captured.db.push(DbCapture {
                query: query.clone(),
                params: params.clone(),
                rows,
            });
        }
    }
    Ok(captured)
}

/// Fold one case's outcome into the running pass flag — one self-describing
/// [`check`] line per assertion (the testkitbench idiom).
fn fold_outcome(ok: &mut bool, outcome: &Outcome) {
    for r in &outcome.results {
        let desc = serde_json::to_string(&r.assertion).unwrap_or_default();
        let label = match (r.passed, &r.detail) {
            (false, Some(d)) => format!("{} :: {desc} — {d}", outcome.name),
            _ => format!("{} :: {desc}", outcome.name),
        };
        check(ok, &label, r.passed);
    }
}

/// Load a flow's cases back FROM the seeded `test_cases` (round-trip through PG),
/// in ordinal order — proving the stored bytes drive the real behavior.
async fn load_cases(app: &Client, flow_id: &str) -> anyhow::Result<Vec<TestCase>> {
    let rows = app
        .query(
            "SELECT case_body FROM test_cases WHERE flow_id = $1 ORDER BY ordinal",
            &[&flow_id],
        )
        .await
        .with_context(|| format!("load cases for {flow_id}"))?;
    rows.iter()
        .map(|r| {
            serde_json::from_value::<TestCase>(r.get(0))
                .with_context(|| format!("stored {flow_id} case parses as a TestCase"))
        })
        .collect()
}

/// Run a DbState query that SELECTs a single json column; collect one Value per
/// row. String params bind as text (the only param kind the v0 suites use).
async fn admin_query_json(
    admin: &Client,
    query: &str,
    params: &[Value],
) -> anyhow::Result<Vec<Value>> {
    let owned: Vec<String> = params
        .iter()
        .map(|p| {
            p.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| p.to_string())
        })
        .collect();
    let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = owned
        .iter()
        .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    let rows = admin.query(query, &refs).await?;
    Ok(rows.iter().map(|r| r.get::<usize, Value>(0)).collect())
}

/// A run's terminal-status string → [`RunFacts`] (absent for a non-terminal /
/// missing row, so a `RunOutcome` assertion fails with "no run facts captured"
/// rather than false-passing).
fn run_facts(status: &str) -> Option<RunFacts> {
    let status: RunStatus = serde_json::from_value(Value::String(status.to_string())).ok()?;
    Some(RunFacts {
        status,
        fail_kind: None,
        fail_node: None,
    })
}

/// Provision the DATA schema (run-state + flow registry + test-suite tables) via
/// the SAME `ensure_*` path production provisioning uses.
async fn provision_data_schema(admin: &Client, schema: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
             END IF; END $$;"
        ))
        .await
        .context("reset DATA schema + ensure wamn_app role")?;
    ensure_runstate(admin, schema).await.context("run-state")?;
    ensure_flow_registry(admin, schema)
        .await
        .context("flow registry")?;
    ensure_flow_tests(admin, schema)
        .await
        .context("flow-test tables")?;
    println!("## provisioned DATA schema {schema} (run-state + flows + test_suites/test_cases)");
    Ok(())
}

/// The graph a flow is registered with in the DATA schema (only the FK row is
/// load-bearing there — the drive schemas register with runtime-bound ports).
fn data_schema_graph(flow_id: &str) -> String {
    match flow_id {
        F1_FLOW_ID => F1_FLOW_JSON.to_string(),
        F3_FLOW_ID => f3_gate_flow_json("serve-echo:8091", F3_OFFSET_MS),
        _ => f4_gate_flow_json(8191, 8192),
    }
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .context("postgres connect")?;
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok((client, task))
}

async fn drop_schema(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    let (client, task) = connect(admin_url).await?;
    let r = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
        .await;
    drop(client);
    let _ = task.await;
    r.context("drop schema")
}

async fn scalar(c: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(c.query_one(sql, &[]).await.context("scalar count")?.get(0))
}

fn f1_schema(args: &PocSuiteProofArgs) -> String {
    format!("{}_f1", args.schema)
}
fn f3_schema(args: &PocSuiteProofArgs) -> String {
    format!("{}_f3", args.schema)
}
fn f4_schema(args: &PocSuiteProofArgs) -> String {
    format!("{}_f4", args.schema)
}

/// A bare lowercase SQL identifier (schemas are interpolated).
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every embedded suite is a valid, version-bound `wamn-flow-tests` envelope
    /// whose case bodies pass validate-on-write (parse as `wamn_testkit::TestCase`)
    /// — a broken fixture fails HERE, at `cargo test`, not only against Postgres.
    #[test]
    fn embedded_suites_are_valid_and_bound() {
        for (json, flow_id, suite_id, n) in [
            (F1_SUITE_JSON, F1_FLOW_ID, "poc-f1-suite", 3usize),
            (F3_SUITE_JSON, F3_FLOW_ID, "poc-f3-suite", 1),
            (F4_SUITE_JSON, F4_FLOW_ID, "poc-f4-suite", 1),
        ] {
            let suite = TestSuite::from_json(json).expect("suite envelope is valid");
            assert_eq!(suite.flow_id, flow_id);
            assert_eq!(suite.flow_version, 1);
            assert_eq!(suite.suite_id, suite_id);
            assert_eq!(suite.cases.len(), n);
            assert_eq!(TestSuite::from_json(&suite.to_json()).unwrap(), suite);
        }
    }

    /// The F1 stored suite drives against the committed F1 flow: its flow-ref
    /// names `receipt-received` v1 (the flow `poc-webhook-f1` serves), and every
    /// case is a flow-level case (not a node case the ProxyPre driver can't run).
    #[test]
    fn f1_suite_targets_the_receipt_received_flow() {
        let suite = TestSuite::from_json(F1_SUITE_JSON).unwrap();
        let flow = wamn_flow::Flow::from_json(F1_FLOW_JSON).expect("F1 flow parses");
        assert_eq!(flow.flow_id, F1_FLOW_ID);
        for case in &suite.cases {
            let tc: TestCase = serde_json::from_value(case.case.clone()).unwrap();
            let fref = tc.flow_ref.expect("F1 case is flow-level");
            assert_eq!(fref.flow_id, F1_FLOW_ID);
            assert_eq!(fref.version, 1);
        }
    }

    /// COHERENCE: 3rj's F3 graph copy mirrors the committed `deploy/poc/f3-flow.json`
    /// — same flow id, cron trigger, the declared credential, the structural
    /// cycle (advance loops to the gate), and notify as a dead-end. A drift in
    /// either the source fixture or 3rj's copy fails this NAMED test.
    #[test]
    fn f3_graph_copy_mirrors_the_source_fixture() {
        const SRC: &str = include_str!("../../../deploy/poc/f3-flow.json");
        let src = wamn_flow::Flow::from_json(SRC).expect("source F3 flow parses");
        let mine = wamn_flow::Flow::from_json(&f3_gate_flow_json("serve-echo:8091", F3_OFFSET_MS))
            .expect("3rj F3 graph parses");
        mine.validate().expect("3rj F3 graph validates");
        assert_eq!(mine.flow_id, src.flow_id);
        assert_eq!(mine.flow_id, F3_FLOW_ID);
        assert_eq!(mine.entry, src.entry);
        // Same node id → type set.
        let types = |f: &wamn_flow::Flow| {
            let mut v: Vec<(String, String)> = f
                .nodes
                .iter()
                .map(|n| (n.id.clone(), n.node_type.clone()))
                .collect();
            v.sort();
            v
        };
        assert_eq!(types(&mine), types(&src), "node id→type set drifted");
        assert!(mine.credentials.iter().any(|c| c.name == "notify-webhook"));
        assert!(
            mine.edges
                .iter()
                .any(|e| e.from == "advance" && e.to == "gate"),
            "the structural cycle must close back to the gate"
        );
        assert!(
            !mine.edges.iter().any(|e| e.from == "notify"),
            "notify is a dead-end"
        );
    }

    /// COHERENCE: 3rj's F4 graph copy mirrors the committed design fixture
    /// `f4-disposition-recorded.flow.json` — same flow id + row-event insert
    /// trigger on `dispositions`, the callback POSTs `/dispositions` with the
    /// idempotency key ON. (3rj's drive graph adds a `shape` reshape node and
    /// drops the credential — the egress-spy proof keeps the vault out — so the
    /// shared invariants, not byte-equality, are pinned.)
    #[test]
    fn f4_graph_copy_mirrors_the_design_fixture() {
        use wamn_flow::{RowEvent, Trigger};
        const SRC: &str = include_str!(
            "../../../crates/wamn-flow/tests/fixtures/f4-disposition-recorded.flow.json"
        );
        let src = wamn_flow::Flow::from_json(SRC).expect("design F4 fixture parses");
        let mine = wamn_flow::Flow::from_json(&f4_gate_flow_json(18191, 18192))
            .expect("3rj F4 graph parses");
        mine.validate().expect("3rj F4 graph validates");
        assert_eq!(mine.flow_id, src.flow_id);
        assert_eq!(mine.flow_id, F4_FLOW_ID);
        assert!(
            matches!(&mine.trigger, Trigger::RowEvent { table, event: RowEvent::Insert } if table.as_str() == "dispositions"),
            "F4 is a row-event insert on dispositions"
        );
        let cb = mine
            .nodes
            .iter()
            .find(|n| n.id == "callback")
            .expect("callback node");
        assert_eq!(cb.config["idempotency-key"], Value::Bool(true));
        assert_eq!(cb.config["method"], "POST");
        assert!(
            cb.config["url"]
                .as_str()
                .unwrap_or_default()
                .ends_with("/dispositions"),
            "the callback targets the ERP /dispositions path"
        );
        assert_eq!(
            mine.allowed_hosts.len(),
            2,
            "both loopback hops are declared egress"
        );
    }

    /// The F3 epoch-anchor arithmetic is coherent: the two stale holds sit BEFORE
    /// the `fire-at-ms − 48h` cutoff and the fresh hold AFTER it. A broken anchor
    /// (a mutant that seeds `now()`-relative or flips the sign) fails here.
    #[test]
    fn f3_epoch_anchor_straddles_the_cutoff() {
        let suite = TestSuite::from_json(F3_SUITE_JSON).unwrap();
        let tc: TestCase = serde_json::from_value(suite.cases[0].case.clone()).unwrap();
        let fire_at_ms = tc.input["fire-at-ms"].as_i64().expect("fire-at-ms present");
        let cutoff_secs = (fire_at_ms + F3_OFFSET_MS) / 1000;
        assert!(
            F3_STALE_OPENED_SECS < cutoff_secs,
            "stale holds ({F3_STALE_OPENED_SECS}) must fall before the cutoff ({cutoff_secs})"
        );
        assert!(
            F3_FRESH_OPENED_SECS > cutoff_secs,
            "the fresh hold ({F3_FRESH_OPENED_SECS}) must fall after the cutoff ({cutoff_secs})"
        );
        // The offset really is 48h (the spec's requirement).
        assert_eq!(F3_OFFSET_MS, -48 * 3600 * 1000);
    }

    /// The F4 stored egress assertion names EXACTLY the F2 node hop (`/run`) + the
    /// one ERP callback (`/dispositions`) and nothing else — the egress-spy set
    /// (path-keyed so it is port-independent). A mutant that widens or narrows
    /// this set breaks the stored proof.
    #[test]
    fn f4_egress_spy_names_exactly_the_hop_and_callback() {
        use wamn_testkit::{EgressAssertion, EgressMatcher};
        let suite = TestSuite::from_json(F4_SUITE_JSON).unwrap();
        let tc: TestCase = serde_json::from_value(suite.cases[0].case.clone()).unwrap();
        let exactly = tc
            .expect
            .iter()
            .find_map(|a| match a {
                Assertion::Egress {
                    flow,
                    calls: EgressAssertion::ExactlyThese(ms),
                } if flow == F4_FLOW_ID => Some(ms.clone()),
                _ => None,
            })
            .expect("an ExactlyThese egress assertion keyed on the flow id");
        let paths: std::collections::BTreeSet<Option<String>> = exactly
            .iter()
            .map(|m: &EgressMatcher| m.path.clone())
            .collect();
        assert_eq!(
            paths,
            ["/run", "/dispositions"]
                .into_iter()
                .map(|s| Some(s.to_string()))
                .collect()
        );
        assert!(
            exactly.iter().all(|m| m.method.as_deref() == Some("POST")),
            "both expected calls are POSTs"
        );
    }

    #[test]
    fn bare_ident_rejects_injection() {
        assert!(is_bare_ident("wamn_pocsuiteproof"));
        assert!(is_bare_ident("poc_f1"));
        assert!(!is_bare_ident("a; DROP"));
        assert!(!is_bare_ident("Cap"));
    }
}
