//! The `apibench` subcommand: the 4.1 generated-REST-API-gateway gates
//! (docs/platform-plan.md 4.1).
//!
//! 4.1 turns a project's catalog (3.1) into a REST surface over the 3.2 tenant
//! floor. The pure gateway logic lives in the `wamn-api` crate (exhaustively
//! unit-tested with no host/DB); the `api-gateway` component is the thin
//! `wasi:http` ⇆ `wamn:postgres` shell around it. This harness proves the whole
//! path end to end against a real Postgres: it builds a small catalog, emits the
//! 3.2 floor DDL, provisions a fresh ephemeral schema through a superuser
//! connection (the runner's `wamn_app` role is NOSUPERUSER, as in production),
//! seeds two tenants' rows plus the catalog snapshot the gateway reads, then
//! drives the component through the standard `wasi:http/incoming-handler` export
//! (via `ProxyPre`, exactly as wash-runtime serves it in production) and asserts:
//!
//!   crud      — list / get / create / update / delete round-trip, with the
//!               managed `id` returned and `numeric` shaped as an exact-decimal
//!               string (no float).
//!   expand    — a to-one relation (`receipts?expand=supplier`) embeds the parent
//!               and a to-many relation (`receipts?expand=lines`) embeds the
//!               child array — each one extra `IN (…)` SELECT, no arbitrary join.
//!   rls       — a row seeded under a *different* tenant is invisible: the
//!               gateway runs under the injected `app.tenant` claim + the floor.
//!   injection — a SQL-injection payload in a filter value is bound as a
//!               parameter (0 matches, the table survives), and unknown
//!               entity/field requests are rejected 4xx before any SQL runs.
//!
//! Postgres note: the ephemeral schema is created via a superuser (admin) URL;
//! the gateway's `wamn_app` pool uses UNQUALIFIED table names resolved through
//! the host-injected `search_path`, exactly like every other wamn workload.

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

use crate::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use crate::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// list / get / create / update / delete round-trip.
    Crud,
    /// one-level to-one + to-many relation expansion.
    Expand,
    /// a different tenant's rows are invisible (RLS + the injected claim).
    Rls,
    /// injection payloads are parameters; unknown identifiers are rejected.
    Injection,
    /// every gate in sequence.
    All,
}

#[derive(Debug, Args)]
pub struct ApiBenchArgs {
    /// Path to the api-gateway guest component.
    #[arg(long, default_value = "/bench/api-gateway.wasm")]
    pub api_gateway: PathBuf,

    /// `wamn_app` Postgres URL (overrides DATABASE_URL / WAMN_PG_URL). The
    /// gateway's pool connects as this non-superuser role.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser Postgres URL used ONLY to provision/seed the ephemeral schema
    /// (env WAMN_PG_ADMIN_URL).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which gate to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Pool max size.
    #[arg(long, default_value_t = 8)]
    pub pool_max: usize,
}

/// The component identity the gateway runs under (maps to the tenant + schema).
const BENCH_ID: &str = "api-gateway-bench";
/// The tenant the gateway is scoped to.
const TENANT_A: &str = "tenant-a";
/// A second tenant whose rows must stay invisible (the RLS witness).
const TENANT_B: &str = "tenant-b";
/// The per-run ephemeral schema the gate provisions.
const EPH_SCHEMA: &str = "api_test";

// Deterministic seed ids so the gates can address rows directly.
const S_ACME: &str = "a0000000-0000-0000-0000-000000000001";
const S_GLOBEX: &str = "a0000000-0000-0000-0000-000000000002";
const S_OTHER: &str = "b0000000-0000-0000-0000-000000000003";
const R1: &str = "c0000000-0000-0000-0000-000000000001";
const L1: &str = "d0000000-0000-0000-0000-000000000001";
const L2: &str = "d0000000-0000-0000-0000-000000000002";

/// The gate's catalog: suppliers ← receipts ← receipt_lines, with a to-one
/// relation `supplier` (receipts→suppliers) and a to-many relation `lines`
/// (receipt_lines→receipts). Stored as the snapshot the gateway loads.
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "apibench",
  "version": 1,
  "entities": [
    { "id": "suppliers", "name": "suppliers", "fields": [
      { "id": "name", "name": "name", "type": { "kind": "text" } },
      { "id": "standard_cost", "name": "standard_cost", "type": { "kind": "numeric", "precision": 12, "scale": 2 }, "nullable": true }
    ] },
    { "id": "receipts", "name": "receipts", "fields": [
      { "id": "receipt_no", "name": "receipt_no", "type": { "kind": "text", "max-len": 64 } },
      { "id": "supplier_id", "name": "supplier_id", "type": { "kind": "reference", "entity": "suppliers" } },
      { "id": "received_at", "name": "received_at", "type": { "kind": "timestamptz" } }
    ] },
    { "id": "receipt_lines", "name": "receipt_lines", "fields": [
      { "id": "receipt_id", "name": "receipt_id", "type": { "kind": "reference", "entity": "receipts" } },
      { "id": "quantity", "name": "quantity", "type": { "kind": "numeric", "precision": 12, "scale": 3 } }
    ] }
  ],
  "relations": [
    { "id": "receipt_supplier", "name": "supplier", "cardinality": "one-to-many", "from": "receipts", "to": "suppliers", "from-field": "supplier_id" },
    { "id": "receipt_lines_rel", "name": "lines", "cardinality": "one-to-many", "from": "receipt_lines", "to": "receipts", "from-field": "receipt_id" }
  ]
}"#;

/// The compiled + linked gateway component, driven through its wasi:http export.
struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: ProxyPre<SharedCtx>,
}

impl Harness {
    fn new(engine: wash_runtime::engine::Engine, guest: &[u8]) -> anyhow::Result<Self> {
        let raw: &RawEngine = engine.inner();
        let component = WasmtimeComponent::new(raw, guest)
            .map_err(|e| anyhow::anyhow!("compile api-gateway: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        // The component exports wasi:http/incoming-handler and imports
        // wasi:http/types; add_only_http links the http side (no outbound use).
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

    /// Drive one HTTP request through the guest's `wasi:http/incoming-handler`
    /// export (a fresh store/instance per request, as wash-runtime serves it),
    /// returning `(status, json-body)`.
    async fn request(
        &self,
        plugin: &Arc<WamnPostgres>,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(u16, Value)> {
        let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(self.plugin_map(plugin))
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);

        let body_bytes = match body {
            Some(v) => serde_json::to_vec(&v)?,
            None => Vec::new(),
        };
        // `new_incoming_request` requires the body's error type to be
        // `Into<ErrorCode>`; `Full`'s is `Infallible`, so box it with `ErrorCode`.
        let body: BoxBody<Bytes, ErrorCode> = Full::new(Bytes::from(body_bytes))
            .map_err(|e| match e {})
            .boxed();
        let req = hyper::Request::builder()
            .method(method)
            .uri(uri)
            // wasi:http requires an authority; supply it via the Host header.
            .header(hyper::header::HOST, "gateway.local")
            .body(body)
            .context("build request")?;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let req_res = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, req)?;
        let out_res = store.data_mut().http().new_response_outparam(tx)?;

        // The guest may stream the response body while `call_handle` is still
        // running, so drive it on a task and read the response concurrently.
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
// Ephemeral schema provisioning (superuser) — floor DDL + snapshot + seed
// ---------------------------------------------------------------------------

/// Drop-and-recreate the ephemeral schema, apply the 3.2 floor for the gate's
/// catalog, create the `wamn_catalog` snapshot table, and seed two tenants.
async fn provision(admin_url: &str, floor_ddl: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        // Ensure the non-superuser runtime role exists (as in production).
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
        // The 3.2-generated tenant floor for suppliers / receipts / receipt_lines.
        client.batch_execute(floor_ddl).await.context("apply floor DDL")?;
        // The catalog snapshot table the gateway reads (tenant-scoped, read-only
        // to wamn_app). Managed shape mirrors the floor.
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
            .context("create wamn_catalog table")?;
        // Seed as superuser (bypasses RLS): the catalog snapshot + two tenants.
        client
            .batch_execute(&seed_sql())
            .await
            .context("seed rows")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

fn seed_sql() -> String {
    // CATALOG_JSON has no single quotes, so it embeds safely as a SQL literal.
    format!(
        "INSERT INTO wamn_catalog (tenant_id, document) VALUES ('{TENANT_A}', '{CATALOG_JSON}'::jsonb); \
         INSERT INTO suppliers (id, tenant_id, name, standard_cost) VALUES \
           ('{S_ACME}', '{TENANT_A}', 'Acme', 12.50), \
           ('{S_GLOBEX}', '{TENANT_A}', 'Globex', 99.99), \
           ('{S_OTHER}', '{TENANT_B}', 'OtherTenantCo', 5.00); \
         INSERT INTO receipts (id, tenant_id, receipt_no, supplier_id, received_at) VALUES \
           ('{R1}', '{TENANT_A}', 'R-001', '{S_ACME}', '2026-01-01T00:00:00Z'); \
         INSERT INTO receipt_lines (id, tenant_id, receipt_id, quantity) VALUES \
           ('{L1}', '{TENANT_A}', '{R1}', 3.000), \
           ('{L2}', '{TENANT_A}', '{R1}', 5.500);"
    )
}

async fn drop_schema(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {EPH_SCHEMA} CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("drop ephemeral schema: {e}"));
    drop(client);
    let _ = conn_task.await;
    r.map(|_| ())
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// Print a check line and fold it into the running pass flag.
fn check(pass: &mut bool, label: &str, ok: bool) {
    println!("  [{}] {label}", if ok { "PASS" } else { "FAIL" });
    *pass &= ok;
}

fn as_array(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

fn has_name(rows: &[Value], name: &str) -> bool {
    rows.iter()
        .any(|r| r.get("name").and_then(Value::as_str) == Some(name))
}

async fn crud_phase(h: &Harness, pg: &Arc<WamnPostgres>) -> anyhow::Result<bool> {
    println!("\n## crud");
    let mut ok = true;

    let (s, body) = h.request(pg, "GET", "/api/rest/suppliers", None).await?;
    let rows = as_array(&body);
    check(
        &mut ok,
        "list suppliers -> 200 with the tenant's two rows",
        s == 200 && rows.len() == 2 && has_name(&rows, "Acme") && has_name(&rows, "Globex"),
    );

    let (s, body) = h
        .request(pg, "GET", &format!("/api/rest/suppliers/{S_ACME}"), None)
        .await?;
    check(
        &mut ok,
        "get by id -> 200, numeric is an exact-decimal string",
        s == 200
            && body.get("name").and_then(Value::as_str) == Some("Acme")
            && body.get("standard_cost").and_then(Value::as_str) == Some("12.50"),
    );

    let (s, body) = h
        .request(
            pg,
            "GET",
            "/api/rest/suppliers?standard_cost=eq.99.99",
            None,
        )
        .await?;
    let rows = as_array(&body);
    check(
        &mut ok,
        "filter standard_cost=eq.99.99 -> 200, one match",
        s == 200 && rows.len() == 1 && has_name(&rows, "Globex"),
    );

    let (s, created) = h
        .request(
            pg,
            "POST",
            "/api/rest/suppliers",
            Some(json!({ "name": "NewCo", "standard_cost": "7.25" })),
        )
        .await?;
    let new_id = created
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    check(
        &mut ok,
        "create -> 201 with a generated id + the row",
        s == 201
            && !new_id.is_empty()
            && created.get("name").and_then(Value::as_str) == Some("NewCo"),
    );

    let (s, updated) = h
        .request(
            pg,
            "PATCH",
            &format!("/api/rest/suppliers/{new_id}"),
            Some(json!({ "standard_cost": "8.00" })),
        )
        .await?;
    check(
        &mut ok,
        "update -> 200 with the new value",
        s == 200 && updated.get("standard_cost").and_then(Value::as_str) == Some("8.00"),
    );

    let (s, _) = h
        .request(pg, "DELETE", &format!("/api/rest/suppliers/{new_id}"), None)
        .await?;
    check(&mut ok, "delete -> 204", s == 204);

    let (s, _) = h
        .request(pg, "GET", &format!("/api/rest/suppliers/{new_id}"), None)
        .await?;
    check(&mut ok, "get deleted -> 404", s == 404);

    Ok(ok)
}

async fn expand_phase(h: &Harness, pg: &Arc<WamnPostgres>) -> anyhow::Result<bool> {
    println!("\n## expand");
    let mut ok = true;

    let (s, body) = h
        .request(pg, "GET", "/api/rest/receipts?expand=supplier", None)
        .await?;
    let rows = as_array(&body);
    let supplier_ok = rows
        .first()
        .and_then(|r| r.get("supplier"))
        .and_then(|sup| sup.get("name"))
        .and_then(Value::as_str)
        == Some("Acme");
    check(
        &mut ok,
        "expand=supplier embeds the to-one parent (Acme)",
        s == 200 && supplier_ok,
    );

    let (s, body) = h
        .request(pg, "GET", "/api/rest/receipts?expand=lines", None)
        .await?;
    let rows = as_array(&body);
    let lines = rows
        .first()
        .and_then(|r| r.get("lines"))
        .map(as_array)
        .unwrap_or_default();
    let qty_ok = lines
        .iter()
        .any(|l| l.get("quantity").and_then(Value::as_str) == Some("3.000"))
        && lines
            .iter()
            .any(|l| l.get("quantity").and_then(Value::as_str) == Some("5.500"));
    check(
        &mut ok,
        "expand=lines embeds the to-many child array (2 lines, exact-decimal)",
        s == 200 && lines.len() == 2 && qty_ok,
    );

    Ok(ok)
}

async fn rls_phase(h: &Harness, pg: &Arc<WamnPostgres>) -> anyhow::Result<bool> {
    println!("\n## rls");
    let mut ok = true;

    let (s, body) = h.request(pg, "GET", "/api/rest/suppliers", None).await?;
    let rows = as_array(&body);
    // The tenant-B row must be invisible under tenant-A's injected claim.
    check(
        &mut ok,
        "the other tenant's row is invisible (RLS + app.tenant)",
        s == 200 && !has_name(&rows, "OtherTenantCo"),
    );

    let (s, _) = h
        .request(pg, "GET", &format!("/api/rest/suppliers/{S_OTHER}"), None)
        .await?;
    check(&mut ok, "get another tenant's row by id -> 404", s == 404);

    Ok(ok)
}

async fn injection_phase(h: &Harness, pg: &Arc<WamnPostgres>) -> anyhow::Result<bool> {
    println!("\n## injection");
    let mut ok = true;

    // A SQL-injection payload as a filter value (percent-encoded in the URL).
    // It must be bound as a parameter: 0 matches, and the table survives.
    let evil = "/api/rest/suppliers?name=eq.Acme%27%3B%20DROP%20TABLE%20suppliers%3B%20--";
    let (s, body) = h.request(pg, "GET", evil, None).await?;
    check(
        &mut ok,
        "injection filter value -> 200, zero matches (bound as a param)",
        s == 200 && as_array(&body).is_empty(),
    );

    let (s, body) = h.request(pg, "GET", "/api/rest/suppliers", None).await?;
    check(
        &mut ok,
        "the suppliers table survived the injection attempt",
        s == 200 && !as_array(&body).is_empty(),
    );

    let (s, _) = h.request(pg, "GET", "/api/rest/nonexistent", None).await?;
    check(
        &mut ok,
        "unknown entity -> 400 (rejected before any SQL)",
        s == 400,
    );

    let (s, _) = h
        .request(pg, "GET", "/api/rest/suppliers?bogus_col=1", None)
        .await?;
    check(&mut ok, "unknown filter column -> 400", s == 400);

    Ok(ok)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run(args: ApiBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let guest = std::fs::read(&args.api_gateway)
        .with_context(|| format!("failed to read {}", args.api_gateway.display()))?;

    println!("# wamn-host 4.1 apibench");

    let run_all = args.mode == Mode::All;

    // Emit the 3.2 tenant floor for the gate's catalog.
    let catalog = wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("gate catalog parse: {e}"))?;
    let floor_ddl = wamn_ddl::Migration::create(&catalog)
        .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
        .sql(wamn_ddl::Confirmation::None)
        .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;

    // Plugin (wamn_app pool), scoped to the gate tenant + ephemeral schema.
    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.pool_max_size = args.pool_max;
    if cfg.database_url.is_none() {
        bail!("no database url: pass --database-url or set DATABASE_URL / WAMN_PG_URL");
    }
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    provision(&admin_url, &floor_ddl)
        .await
        .context("provision ephemeral schema")?;
    println!("provisioned ephemeral schema {EPH_SCHEMA} (floor + snapshot + seed)");

    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    plugin.set_tenant(BENCH_ID, TENANT_A)?;
    plugin.set_schema(BENCH_ID, EPH_SCHEMA)?;
    plugin
        .probe_checkout()
        .await
        .context("postgres preflight")?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let harness = Harness::new(engine, &guest)?;

    let mut pass = true;
    if run_all || args.mode == Mode::Crud {
        pass &= crud_phase(&harness, &plugin).await?;
    }
    if run_all || args.mode == Mode::Expand {
        pass &= expand_phase(&harness, &plugin).await?;
    }
    if run_all || args.mode == Mode::Rls {
        pass &= rls_phase(&harness, &plugin).await?;
    }
    if run_all || args.mode == Mode::Injection {
        pass &= injection_phase(&harness, &plugin).await?;
    }

    drop(plugin);
    if let Err(e) = drop_schema(&admin_url).await {
        tracing::warn!(error = %e, "ephemeral schema teardown failed (non-fatal)");
    }
    ticker.abort();

    println!("\napibench complete — overall PASS: {pass}");
    if !pass {
        bail!("4.1 apibench gate failed");
    }
    Ok(())
}
