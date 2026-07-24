//! f1bench — the POC-F1 gate (wamn-067, the P1 exit criterion). Drives the
//! `poc-webhook-f1` component in-proc through `wasi:http/incoming-handler`
//! (ProxyPre, the apibench pattern) against a real Postgres, and cross-checks
//! the results through the 4.1 `api-gateway` component over the SAME schema —
//! the generated-REST half of "end-to-end via catalog API + generated REST".
//!
//! Provisions an EPHEMERAL schema (`wamn_f1_bench`) through the SUPERUSER url
//! using the SAME code path production provisioning uses (`publish-catalog`:
//! floor + run-state + flow registry + snapshot + wamn-seed dataset + flow
//! registration), so the provisioning flags are gated here too.
//!
//! Modes:
//!   happy   — one in-spec receipt: 200 `{receipt_id, holds: []}`, write-ahead
//!             runs row (trigger_source webhook, payload verbatim), 4-node
//!             node_runs trace, rows persisted.
//!   holds   — out-of-spec lines: sync holds in the response, `quality_holds`
//!             rows (status open, correct line/site), evaluate recorded on the
//!             `out-of-spec` port, respond merged from the holds branch.
//!   invalid — the malformed set: each 400 `invalid-input`, the run still
//!             write-aheads + completes with the error result, validate
//!             recorded as an `error`-port emission; plus 405/404 routing.
//!   burst   — the acceptance script: 20 receipts (3 out-of-spec) => per-
//!             receipt sync holds, 20 write-ahead runs, 4 holds total, RLS
//!             isolation of the holds from another tenant.
//!   rest    — the generated REST gateway serves the SAME schema: holds list
//!             (with one-level `line` expansion) and receipt filtering.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use bytes::Bytes;
use clap::{Args, ValueEnum};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi_http::p2::WasiHttpView;
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::bindings::http::types::{ErrorCode, Scheme};

use crate::f1fixture::{
    self, BURST_HOLDS, F1_FLOW_JSON, F1_SEED_JSON, F1_TENANT, burst, in_spec_receipt, receipt,
};
use wamn_ctl::publish_catalog;
use wamn_gate_harness::{as_array, check};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

#[derive(Debug, Args)]
pub struct F1BenchArgs {
    /// Path to poc_webhook_f1.wasm (the sync-webhook ingress component).
    #[arg(long, default_value = "/bench/poc-webhook-f1.wasm")]
    pub webhook_entry: PathBuf,

    /// Path to api_gateway.wasm (the 4.1 generated-REST component, for the
    /// rest mode's cross-check).
    #[arg(long, default_value = "/bench/api-gateway.wasm")]
    pub api_gateway: PathBuf,

    /// Postgres URL for the wamn:postgres plugin (non-superuser wamn_app).
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions the ephemeral schema + asserts DB state
    /// (env `WAMN_PG_ADMIN_URL`).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Happy,
    Holds,
    Invalid,
    Burst,
    Rest,
    All,
}

const BENCH_ID: &str = "f1-bench";
const EPH_SCHEMA: &str = "wamn_f1_bench";

// ---------------------------------------------------------------------------
// ProxyPre harness (the apibench pattern)
// ---------------------------------------------------------------------------

struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: ProxyPre<SharedCtx>,
}

impl Harness {
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

    fn plugin_map(
        &self,
        plugin: &Arc<WamnPostgres>,
    ) -> std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
        let mut m = std::collections::HashMap::new();
        m.insert(
            wamn_postgres::WAMN_POSTGRES_ID,
            plugin.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        m
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
        self.request_raw(plugin, method, uri, body_bytes).await
    }

    /// Like [`Harness::request`] but with the body as RAW BYTES — the only way
    /// to send something that is not valid JSON (the fallback-audit path).
    async fn request_raw(
        &self,
        plugin: &Arc<WamnPostgres>,
        method: &str,
        uri: &str,
        body_bytes: Vec<u8>,
    ) -> anyhow::Result<(u16, Value)> {
        let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(self.plugin_map(plugin))
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
// Provisioning + DB assertion plumbing
// ---------------------------------------------------------------------------

/// Superuser connection with `search_path` pinned to the ephemeral schema
/// (bypasses RLS — the audit view of the world).
async fn admin_connect(
    admin_url: &str,
) -> anyhow::Result<(
    tokio_postgres::Client,
    tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
)> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let task = tokio::spawn(conn);
    client
        .batch_execute(&format!("SET search_path TO {EPH_SCHEMA};"))
        .await
        .context("set search_path")?;
    Ok((client, task))
}

/// Provision the ephemeral F1 world through the SAME helpers `publish-catalog`
/// runs in production: floor + run-state + flow registry + catalog snapshot +
/// seed dataset + registered/active flow.
async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn_task) = {
        let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
            .await
            .context("admin connect")?;
        (client, tokio::spawn(conn))
    };
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
                "DROP SCHEMA IF EXISTS {EPH_SCHEMA} CASCADE; \
                 CREATE SCHEMA {EPH_SCHEMA} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {EPH_SCHEMA} TO wamn_app; \
                 SET search_path TO {EPH_SCHEMA};"
            ))
            .await
            .context("create ephemeral schema")?;

        // 3.2 floor (unqualified — resolved by the session search_path).
        client
            .batch_execute(&f1fixture::floor_ddl()?)
            .await
            .context("apply floor")?;

        // Run-state + flow registry: the canonical deploy files.
        anyhow::ensure!(
            publish_catalog::ensure_runstate(&client, EPH_SCHEMA).await?,
            "fresh schema must apply run-state"
        );
        anyhow::ensure!(
            publish_catalog::ensure_flow_registry(&client, EPH_SCHEMA).await?,
            "fresh schema must apply the flow registry"
        );

        // Catalog snapshot (read by the api-gateway component in rest mode).
        let document = f1fixture::catalog()?.to_json();
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
                &[&F1_TENANT, &document],
            )
            .await
            .context("write snapshot")?;

        // Business seed (wamn-seed) + the registered/active F1 flow.
        let seed = publish_catalog::seed_dataset_sql(F1_SEED_JSON, &f1fixture::catalog()?, F1_TENANT)?;
        client.batch_execute(&seed).await.context("apply seed")?;
        // Register TWICE: the second call exercises the deactivate-prior +
        // ON CONFLICT re-activate arms; exactly one active row must remain.
        publish_catalog::register_flow(&client, F1_TENANT, F1_FLOW_JSON).await?;
        let (flow_id, version) =
            publish_catalog::register_flow(&client, F1_TENANT, F1_FLOW_JSON).await?;
        let active: i64 = client
            .query_one(
                "SELECT count(*) FROM flows WHERE tenant_id = $1 AND active",
                &[&F1_TENANT],
            )
            .await?
            .get(0);
        anyhow::ensure!(active == 1, "re-registration must leave ONE active row");

        // Registration-time webhook-path collision (wamn-i7i): a DIFFERENT flow
        // claiming the SAME active path must be rejected by the friendly
        // pre-check, leaving the registry untouched — and even bypassing
        // register_flow entirely, the flows_active_webhook_path unique index
        // must refuse the row (the race-proof DB backstop).
        let mut graph: serde_json::Value = serde_json::from_str(F1_FLOW_JSON)?;
        graph["flow-id"] = serde_json::json!("receipt-received-b");
        let collider = serde_json::to_string(&graph)?;
        let err = publish_catalog::register_flow(&client, F1_TENANT, &collider)
            .await
            .expect_err("same-path registration must fail");
        // State intact FIRST (under a pre-check-dropped mutant this pins the
        // one-txn rollback: the index aborts the insert, so the deactivate
        // must not survive), THEN the friendly named error (kills that mutant).
        let active_id: String = client
            .query_one(
                "SELECT flow_id FROM flows WHERE tenant_id = $1 AND active",
                &[&F1_TENANT],
            )
            .await
            .context("exactly one active flow must survive the failed registration")?
            .get(0);
        anyhow::ensure!(
            active_id == flow_id,
            "failed collision registration must leave {flow_id:?} active (got {active_id:?})"
        );
        anyhow::ensure!(
            format!("{err:#}").contains("webhook path collision"),
            "collision must be rejected by the NAMED pre-check error, got: {err:#}"
        );
        let index_err = client
            .execute(
                "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
                 VALUES ($1, 'receipt-received-b', 1, true, $2::text::jsonb)",
                &[&F1_TENANT, &collider],
            )
            .await
            .expect_err("raw same-path insert must violate the collision index");
        anyhow::ensure!(
            index_err
                .as_db_error()
                .is_some_and(|db| db.message().contains("flows_active_webhook_path")),
            "raw insert must fail on the unique index, got: {index_err}"
        );
        // A DIFFERENT path registers cleanly (the constraint is per-path, not
        // per-tenant); deactivate it again to keep the routing world single-flow.
        graph["flow-id"] = serde_json::json!("receipt-received-alt");
        graph["trigger"]["path"] = serde_json::json!("/receipts-alt");
        let alt = serde_json::to_string(&graph)?;
        publish_catalog::register_flow(&client, F1_TENANT, &alt).await?;
        client
            .execute(
                "UPDATE flows SET active = false \
                 WHERE tenant_id = $1 AND flow_id = 'receipt-received-alt'",
                &[&F1_TENANT],
            )
            .await?;
        println!("  collision rejected (pre-check + index backstop); different path accepted");
        println!("  provisioned {EPH_SCHEMA}: flow {flow_id} v{version} active");
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn drop_schema(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let task = tokio::spawn(conn);
    let result = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {EPH_SCHEMA} CASCADE;"))
        .await;
    drop(client);
    let _ = task.await;
    result.context("drop ephemeral schema")
}

/// One row of the run trace, in seq order — including the payload columns
/// (what 5.7 reconstruction folds), so a regression that stops persisting
/// them fails the gate.
#[derive(Debug)]
struct TraceRow {
    node_id: String,
    status: String,
    output_port: Option<String>,
    error_kind: Option<String>,
    output: Value,
    input_present: bool,
    error_detail: Value,
}

async fn run_trace(
    db: &tokio_postgres::Client,
    receipt_no: &str,
) -> anyhow::Result<(String, String, Value, Vec<TraceRow>)> {
    let row = db
        .query_one(
            "SELECT run_id, status, coalesce(result_json::text, 'null'), trigger_source, \
                    input_json IS NOT NULL \
             FROM runs WHERE input_json->>'receipt_no' = $1",
            &[&receipt_no],
        )
        .await
        .with_context(|| format!("runs row for {receipt_no}"))?;
    let run_id: String = row.get(0);
    let status: String = row.get(1);
    let result: Value = serde_json::from_str(row.get::<_, &str>(2)).unwrap_or(Value::Null);
    let trigger: Option<String> = row.get(3);
    anyhow::ensure!(
        trigger.as_deref() == Some("webhook"),
        "trigger_source must be webhook"
    );
    let trace = db
        .query(
            "SELECT node_id, status, output_port, error_kind, \
                    coalesce(output_json::text, 'null'), input_json IS NOT NULL, \
                    coalesce(error_detail::text, 'null') \
             FROM node_runs WHERE run_id = $1 ORDER BY seq",
            &[&run_id],
        )
        .await?
        .into_iter()
        .map(|r| TraceRow {
            node_id: r.get(0),
            status: r.get(1),
            output_port: r.get(2),
            error_kind: r.get(3),
            output: serde_json::from_str(r.get::<_, &str>(4)).unwrap_or(Value::Null),
            input_present: r.get(5),
            error_detail: serde_json::from_str(r.get::<_, &str>(6)).unwrap_or(Value::Null),
        })
        .collect();
    Ok((run_id, status, result, trace))
}

fn trace_path(trace: &[TraceRow]) -> Vec<String> {
    trace
        .iter()
        .map(|t| {
            format!(
                "{}:{}:{}",
                t.node_id,
                t.status,
                t.output_port.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

async fn happy_phase(
    h: &Harness,
    pg: &Arc<WamnPostgres>,
    db: &tokio_postgres::Client,
) -> anyhow::Result<bool> {
    println!("## happy — in-spec receipt, sync 200 + write-ahead + trace");
    let mut ok = true;

    let (status, body) = h
        .request(pg, "POST", "/receipts", Some(in_spec_receipt("r-2001")))
        .await?;
    check(&mut ok, "status 200", status == 200);
    let receipt_id = body["receipt_id"].as_str().unwrap_or("").to_string();
    check(&mut ok, "receipt_id returned", receipt_id.len() == 36);
    check(&mut ok, "holds empty", as_array(&body["holds"]).is_empty());

    let (_, run_status, result, trace) = run_trace(db, "r-2001").await?;
    check(&mut ok, "run completed", run_status == "completed");
    check(
        &mut ok,
        "result recorded",
        result["receipt_id"] == json!(receipt_id),
    );
    check(
        &mut ok,
        "trace = validate/upsert/evaluate/respond-ok on main",
        trace_path(&trace)
            == [
                "validate:success:main",
                "upsert:success:main",
                "evaluate:success:main",
                "respond-ok:success:main",
            ],
    );
    // The 5.7 payload columns are actually persisted: every node recorded its
    // input, and the terminal respond's recorded output IS the response body.
    check(
        &mut ok,
        "node_runs payloads persisted (inputs + respond output == body)",
        trace.iter().all(|t| t.input_present && !t.output.is_null())
            && trace.last().map(|t| &t.output) == Some(&body),
    );

    // Persisted world: the receipt + its line, quantity exact.
    let rows = db
        .query(
            "SELECT l.quantity::text FROM receipts r JOIN receipt_lines l ON l.receipt_id = r.id \
             WHERE r.receipt_no = $1",
            &[&"r-2001"],
        )
        .await?;
    check(&mut ok, "one line persisted", rows.len() == 1);
    check(
        &mut ok,
        "quantity exact-decimal",
        rows.first().map(|r| r.get::<_, String>(0)) == Some("100.000".to_string()),
    );

    // Clean committed replace: re-POST r-2001 (no holds on it) with a changed
    // line — the ON CONFLICT DO UPDATE + DELETE/INSERT path COMMITS: still one
    // receipt, one line, the NEW quantity.
    let (status, _) = h
        .request(
            pg,
            "POST",
            "/receipts",
            Some(receipt(
                "r-2001",
                "acme",
                "hq",
                &[("resin-a", "90.000", "11.20", "89.990")],
            )),
        )
        .await?;
    check(&mut ok, "clean re-POST: 200", status == 200);
    let rows = db
        .query(
            "SELECT l.quantity::text FROM receipts r JOIN receipt_lines l ON l.receipt_id = r.id \
             WHERE r.receipt_no = $1",
            &[&"r-2001"],
        )
        .await?;
    check(
        &mut ok,
        "replace committed: one line with the new quantity",
        rows.len() == 1 && rows[0].get::<_, String>(0) == "90.000",
    );
    Ok(ok)
}

async fn holds_phase(
    h: &Harness,
    pg: &Arc<WamnPostgres>,
    db: &tokio_postgres::Client,
) -> anyhow::Result<bool> {
    println!("## holds — out-of-spec lines create quality_holds + sync holds response");
    let mut ok = true;

    // Line 1 moisture-over, line 2 weight-over, line 3 clean.
    let payload = receipt(
        "r-2002",
        "acme",
        "west",
        &[
            ("resin-a", "40.000", "13.00", "40.010"),
            ("pigment-c", "20.000", "4.90", "20.050"),
            ("solvent-b", "10.000", "0.05", "10.100"),
        ],
    );
    let (status, body) = h.request(pg, "POST", "/receipts", Some(payload)).await?;
    check(&mut ok, "status 200", status == 200);
    let holds = as_array(&body["holds"]);
    check(&mut ok, "two holds in the response", holds.len() == 2);
    check(
        &mut ok,
        "hold lines are 1 and 2",
        holds.iter().map(|h| h["line"].as_u64()).collect::<Vec<_>>() == [Some(1), Some(2)],
    );
    check(
        &mut ok,
        "reasons name the exceedance",
        holds[0]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("moisture")
            && holds[1]["reason"].as_str().unwrap_or("").contains("weight"),
    );
    check(
        &mut ok,
        "holds are open",
        holds.iter().all(|h| h["status"] == json!("open")),
    );

    // DB: the holds reference the receipt's lines and the receipt's site.
    let rows = db
        .query(
            "SELECT q.status, s.code FROM quality_holds q \
             JOIN receipt_lines l ON q.line_id = l.id \
             JOIN receipts r ON l.receipt_id = r.id \
             JOIN sites s ON q.site_id = s.id \
             WHERE r.receipt_no = $1",
            &[&"r-2002"],
        )
        .await?;
    check(&mut ok, "two quality_holds rows", rows.len() == 2);
    check(
        &mut ok,
        "hold rows open at the receipt's site",
        rows.iter()
            .all(|r| r.get::<_, String>(0) == "open" && r.get::<_, String>(1) == "west"),
    );

    let (_, run_status, _, trace) = run_trace(db, "r-2002").await?;
    check(&mut ok, "run completed", run_status == "completed");
    check(
        &mut ok,
        "evaluate branched out-of-spec, holds merged into respond-ok",
        trace_path(&trace)
            == [
                "validate:success:main",
                "upsert:success:main",
                "evaluate:success:out-of-spec",
                "holds:success:main",
                "respond-ok:success:main",
            ],
    );

    // Re-POST of a receipt whose lines carry holds: the replace-lines DELETE
    // hits the quality_holds FK, the transaction rolls back, and the run FAILS
    // mid-flow (upsert has no error edge — the documented conservative v1).
    // This is also the D15 ordering discriminator: the FAILED run's audit row
    // exists with the payload, which is only possible if the write-ahead
    // preceded the failing effect.
    let payload = receipt(
        "r-2002",
        "acme",
        "west",
        &[("resin-a", "40.000", "13.00", "40.010")],
    );
    let (status, body) = h.request(pg, "POST", "/receipts", Some(payload)).await?;
    check(
        &mut ok,
        "re-POST under holds: 500 run-failed",
        status == 500 && body["error"]["code"] == json!("run-failed"),
    );
    let failed = db
        .query(
            "SELECT status, fail_kind, fail_node, input_json IS NOT NULL FROM runs \
             WHERE input_json->>'receipt_no' = 'r-2002' AND status = 'failed'",
            &[],
        )
        .await?;
    check(
        &mut ok,
        "failed run write-ahead row exists (terminal at upsert, payload intact)",
        failed.len() == 1
            && failed[0].get::<_, String>(1) == "terminal"
            && failed[0].get::<_, String>(2) == "upsert"
            && failed[0].get::<_, bool>(3),
    );
    // The failed transaction left the world intact: lines + holds unchanged.
    let holds_intact: i64 = db
        .query_one(
            "SELECT count(*) FROM quality_holds q JOIN receipt_lines l ON q.line_id = l.id \
             JOIN receipts r ON l.receipt_id = r.id WHERE r.receipt_no = 'r-2002'",
            &[],
        )
        .await?
        .get(0);
    check(&mut ok, "rollback left the holds intact", holds_intact == 2);
    Ok(ok)
}

async fn invalid_phase(
    h: &Harness,
    pg: &Arc<WamnPostgres>,
    db: &tokio_postgres::Client,
) -> anyhow::Result<bool> {
    println!("## invalid — malformed payloads: 400 invalid-input, run still audited");
    let mut ok = true;

    let float_line = json!({
        "receipt_no": "r-3002", "supplier": "acme", "site": "hq",
        "received_at": "2026-07-12T08:00:00Z",
        "lines": [{ "material": "resin-a", "quantity": 12.5,
                    "moisture_pct": "1.00", "weight_kg": "12.000" }]
    });
    let unknown_supplier = receipt(
        "r-3003",
        "wayne-enterprises",
        "hq",
        &[("resin-a", "1.000", "1.00", "1.000")],
    );
    let cases: Vec<(&str, Value)> = vec![
        ("missing fields", json!({ "receipt_no": "r-3001" })),
        ("float quantity", float_line),
        ("unknown supplier", unknown_supplier),
        (
            "bad decimal",
            receipt(
                "r-3004",
                "acme",
                "hq",
                &[("resin-a", "12.5.0", "1.00", "1.000")],
            ),
        ),
        (
            "bad timestamp",
            json!({ "receipt_no": "r-3005", "supplier": "acme", "site": "hq",
                    "received_at": "yesterday",
                    "lines": [{ "material": "resin-a", "quantity": "1.000",
                                "moisture_pct": "1.00", "weight_kg": "1.000" }] }),
        ),
    ];
    for (label, payload) in cases {
        let (status, body) = h.request(pg, "POST", "/receipts", Some(payload)).await?;
        check(
            &mut ok,
            &format!("{label}: 400 invalid-input"),
            status == 400 && body["error"]["code"] == json!("invalid-input"),
        );
    }

    // A body that is VALID JSON but not an object: 400, still audited.
    let before: i64 = db.query_one("SELECT count(*) FROM runs", &[]).await?.get(0);
    let (status, body) = h
        .request(
            pg,
            "POST",
            "/receipts",
            Some(Value::String("not a receipt".into())),
        )
        .await?;
    check(
        &mut ok,
        "non-object body: 400 invalid-input",
        status == 400 && body["error"]["code"] == json!("invalid-input"),
    );
    // A body that is NOT JSON AT ALL: carried as a JSON string, so the
    // write-ahead row records verbatim what arrived; the run still answers 400.
    let (status, body) = h
        .request_raw(pg, "POST", "/receipts", b"{unclosed garbage".to_vec())
        .await?;
    check(
        &mut ok,
        "non-JSON body: 400 invalid-input",
        status == 400 && body["error"]["code"] == json!("invalid-input"),
    );
    let verbatim: i64 = db
        .query_one(
            "SELECT count(*) FROM runs WHERE input_json = to_jsonb('{unclosed garbage'::text)",
            &[],
        )
        .await?
        .get(0);
    check(&mut ok, "non-JSON body audited verbatim", verbatim == 1);
    let after: i64 = db.query_one("SELECT count(*) FROM runs", &[]).await?.get(0);
    check(
        &mut ok,
        "both malformed bodies audited",
        after == before + 2,
    );

    // The audit trail of one invalid run: completed, error result recorded,
    // validate recorded as an error-port emission with the taxonomy kind.
    let (_, run_status, result, trace) = run_trace(db, "r-3003").await?;
    check(&mut ok, "invalid run completes", run_status == "completed");
    check(
        &mut ok,
        "error result recorded",
        result["error"]["code"] == json!("invalid-input"),
    );
    check(
        &mut ok,
        "validate recorded on the error port, respond-bad merged",
        trace_path(&trace) == ["validate:error:error", "respond-bad:success:main"],
    );
    check(
        &mut ok,
        "error_kind is invalid-input",
        trace.first().and_then(|t| t.error_kind.as_deref()) == Some("invalid-input"),
    );
    // The error row's payload columns: output_json is the {"error":...}
    // payload the engine routed (== what respond-bad emitted == the response),
    // and error_detail carries the taxonomy detail for the run history.
    check(
        &mut ok,
        "error row payloads persisted (output == routed error, detail coded)",
        trace.first().is_some_and(|t| {
            t.input_present
                && t.output["error"]["code"] == json!("invalid-input")
                && t.error_detail["code"] == json!("invalid-input")
        }) && trace.last().map(|t| &t.output) == Some(&result),
    );

    // Routing: wrong method / unknown path never mint a run.
    let before: i64 = db.query_one("SELECT count(*) FROM runs", &[]).await?.get(0);
    let (status, _) = h.request(pg, "GET", "/receipts", None).await?;
    check(&mut ok, "GET => 405", status == 405);
    let (status, _) = h.request(pg, "POST", "/nope", None).await?;
    check(&mut ok, "unknown path => 404", status == 404);
    let after: i64 = db.query_one("SELECT count(*) FROM runs", &[]).await?.get(0);
    check(&mut ok, "routing rejects mint no runs", after == before);
    Ok(ok)
}

async fn burst_phase(
    h: &Harness,
    pg: &Arc<WamnPostgres>,
    db: &tokio_postgres::Client,
    database_url: &str,
) -> anyhow::Result<bool> {
    println!("## burst — 20 receipts (3 out-of-spec): sync holds + write-ahead audit + RLS");
    let mut ok = true;

    let holds_before: i64 = db
        .query_one("SELECT count(*) FROM quality_holds", &[])
        .await?
        .get(0);
    let mut receipt_ids = std::collections::HashSet::new();
    for (payload, expected_holds) in burst() {
        let no = payload["receipt_no"].as_str().unwrap_or("").to_string();
        let (status, body) = h.request(pg, "POST", "/receipts", Some(payload)).await?;
        let holds = as_array(&body["holds"]);
        if status != 200 || holds.len() != expected_holds {
            check(
                &mut ok,
                &format!(
                    "{no}: 200 with {expected_holds} holds (got {status}, {})",
                    holds.len()
                ),
                false,
            );
            continue;
        }
        receipt_ids.insert(body["receipt_id"].as_str().unwrap_or("").to_string());
    }
    check(&mut ok, "20 distinct receipt ids", receipt_ids.len() == 20);

    let runs: i64 = db
        .query_one(
            "SELECT count(*) FROM runs WHERE input_json->>'receipt_no' LIKE 'r-10%' \
             AND status = 'completed' AND trigger_source = 'webhook' AND input_json IS NOT NULL",
            &[],
        )
        .await?
        .get(0);
    check(&mut ok, "20 completed write-ahead runs", runs == 20);
    let holds_after: i64 = db
        .query_one("SELECT count(*) FROM quality_holds", &[])
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("{BURST_HOLDS} holds created"),
        holds_after - holds_before == BURST_HOLDS as i64,
    );
    // The write-ahead payload is verbatim: the exact-decimal strings survive.
    let verbatim: i64 = db
        .query_one(
            "SELECT count(*) FROM runs WHERE input_json->>'receipt_no' = 'r-1005' \
             AND input_json->'lines'->0->>'moisture_pct' = '13.10'",
            &[],
        )
        .await?
        .get(0);
    check(&mut ok, "write-ahead payload verbatim", verbatim == 1);

    // RLS: as wamn_app, another tenant sees no holds; the F1 tenant sees them.
    let (app, conn) = tokio_postgres::connect(database_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let rls = async {
        app.batch_execute(&format!(
            "SET search_path TO {EPH_SCHEMA}; SET app.tenant = 'someone-else';"
        ))
        .await?;
        let other: i64 = app
            .query_one("SELECT count(*) FROM quality_holds", &[])
            .await?
            .get(0);
        app.batch_execute(&format!("SET app.tenant = '{F1_TENANT}';"))
            .await?;
        let own: i64 = app
            .query_one("SELECT count(*) FROM quality_holds", &[])
            .await?
            .get(0);
        anyhow::Ok((other, own))
    }
    .await;
    drop(app);
    let _ = conn_task.await;
    let (other, own) = rls?;
    check(&mut ok, "RLS: other tenant sees 0 holds", other == 0);
    check(
        &mut ok,
        "RLS: f1 tenant sees the holds",
        own >= BURST_HOLDS as i64,
    );
    Ok(ok)
}

async fn rest_phase(
    rest: &Harness,
    pg: &Arc<WamnPostgres>,
    db: &tokio_postgres::Client,
) -> anyhow::Result<bool> {
    println!("## rest — the generated REST gateway serves the same schema");
    let mut ok = true;

    let db_holds: i64 = db
        .query_one("SELECT count(*) FROM quality_holds", &[])
        .await?
        .get(0);
    let (status, body) = rest
        .request(pg, "GET", "/api/rest/quality_holds?limit=100", None)
        .await?;
    check(&mut ok, "GET quality-holds 200", status == 200);
    let rows = as_array(&body);
    check(
        &mut ok,
        &format!("REST lists all {db_holds} holds"),
        rows.len() as i64 == db_holds,
    );
    check(
        &mut ok,
        "holds are open",
        rows.iter().all(|r| r["status"] == json!("open")),
    );

    // One-level expansion: each hold embeds its receipt line.
    let (status, body) = rest
        .request(
            pg,
            "GET",
            "/api/rest/quality_holds?expand=line&limit=100",
            None,
        )
        .await?;
    check(&mut ok, "expand=line 200", status == 200);
    let rows = as_array(&body);
    check(
        &mut ok,
        "each hold embeds its line with exact quantity",
        !rows.is_empty()
            && rows
                .iter()
                .all(|r| r["line"]["id"].is_string() && r["line"]["quantity"].is_string()),
    );

    // Receipt filtering (the ERP's audit query).
    let (status, body) = rest
        .request(pg, "GET", "/api/rest/receipts?receipt_no=eq.r-2001", None)
        .await?;
    check(
        &mut ok,
        "receipts filter finds r-2001",
        status == 200 && as_array(&body).len() == 1,
    );
    Ok(ok)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run(args: F1BenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let webhook_guest = std::fs::read(&args.webhook_entry)
        .with_context(|| format!("read {}", args.webhook_entry.display()))?;
    let rest_guest = std::fs::read(&args.api_gateway)
        .with_context(|| format!("read {}", args.api_gateway.display()))?;

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    let database_url = cfg
        .database_url
        .clone()
        .context("no database url: pass --database-url or set WAMN_PG_URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    println!("# wamn-host f1bench — POC-F1 receipt-received gate (P1 exit)");
    provision(&admin_url).await?;

    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    plugin.set_tenant(BENCH_ID, F1_TENANT)?;
    plugin.set_schema(BENCH_ID, EPH_SCHEMA)?;
    plugin.probe_checkout().await?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let webhook = Harness::new(engine.clone(), &webhook_guest)?;
    let rest = Harness::new(engine, &rest_guest)?;

    let (db, db_task) = admin_connect(&admin_url).await?;

    let mut pass = true;
    let mode = args.mode;
    let want = |m: Mode| mode == m || mode == Mode::All;
    let result = async {
        if want(Mode::Happy) {
            pass &= happy_phase(&webhook, &plugin, &db).await?;
        }
        if want(Mode::Holds) {
            pass &= holds_phase(&webhook, &plugin, &db).await?;
        }
        if want(Mode::Invalid) {
            pass &= invalid_phase(&webhook, &plugin, &db).await?;
        }
        if want(Mode::Burst) {
            pass &= burst_phase(&webhook, &plugin, &db, &database_url).await?;
        }
        if want(Mode::Rest) {
            pass &= rest_phase(&rest, &plugin, &db).await?;
        }
        anyhow::Ok(())
    }
    .await;

    drop(db);
    let _ = db_task.await;
    drop(plugin);
    if let Err(e) = drop_schema(&admin_url).await {
        eprintln!("warning: {e}");
    }
    ticker.abort();
    result?;

    println!("\nf1bench complete — overall PASS: {pass}");
    if !pass {
        bail!("f1bench gate failed");
    }
    Ok(())
}
