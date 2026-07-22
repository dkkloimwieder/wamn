//! The `testhostbench` subcommand: the S6 test-host plugin-swap gates
//! (docs/archive/p0-exit-criteria.md S6).
//!
//! S6 validates the mock-at-capability-boundary thesis (design-note 9): the
//! SAME compiled flow binary runs unmodified under a *prod* host and a *test*
//! host, and only the host-injected capabilities differ. This harness compiles
//! the extended flowrunner ONCE and instantiates the identical bytes into two
//! stores:
//!
//!   PROD store — real wall clock (default `WasiCtx`), a forward-all egress
//!                handler, and `wamn:postgres` pointed at the shared fixture
//!                schema `s3`.
//!   TEST store — a *virtual* wall clock the harness advances (via
//!                `CtxBuilder::with_wasi_ctx`), an egress *spy* that records and
//!                stubs/denies outbound calls (via `with_http_handler`), and
//!                `wamn:postgres` pointed at a fresh per-run *ephemeral* schema.
//!
//! Gates:
//!   sameness — the identical InstancePre (one compiled component, one byte
//!              digest) runs a delay+http flow to completion under BOTH stores.
//!   delay    — a flow with a 24h delay node completes in < 1s wall under the
//!              test store's virtual clock (parked-wake: the node records a
//!              wake deadline and parks; the harness advances the clock and
//!              re-runs). Under the prod store's real clock the same run parks
//!              and does NOT complete — proving the delay is real, and it is the
//!              virtual clock that collapses it.
//!   egress   — the test store's egress spy catches an intentionally-added
//!              unexpected outbound call: an expected call to the loopback echo
//!              is recorded and forwarded, while a planted call to an
//!              unexpected authority is flagged and denied (never leaves).
//!   regression — re-run the S3 flowbench gates (dispatch / hot-reload / resume)
//!              on the SAME extended binary, proving the added nodes did not
//!              regress S3.
//!
//! Postgres note: the test host provisions the ephemeral schema through a
//! superuser (admin) connection — the runner's `wamn_app` role is
//! NOSUPERUSER/NOCREATEDB and cannot create schemas, exactly as in production.
//! The runner uses UNQUALIFIED table names; each host injects the schema via
//! `search_path`, so the schema is a host-swapped fixture like the tenant claim.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_postgres::NoTls;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::http::{DefaultOutgoingHandler, HostHandler, OutgoingHandler};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{
    Component as WasmtimeComponent, InstancePre, Linker, TypedFunc,
};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::{HostWallClock, WasiCtxBuilder};
use wasmtime_wasi_http::p2::HttpResult;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Same binary runs under both host wirings.
    Sameness,
    /// 24h-delay flow completes < 1s under virtual time.
    Delay,
    /// Egress spy catches a planted unexpected outbound call.
    Egress,
    /// Re-run the S3 flowbench gates on the extended binary.
    Regression,
    /// Every gate in sequence.
    All,
}

#[derive(Debug, Args)]
pub struct TestHostBenchArgs {
    /// Path to the (extended) flowrunner guest component.
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// `wamn_app` Postgres URL (overrides DATABASE_URL / WAMN_PG_URL). The
    /// runner's pool connects as this non-superuser role.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser Postgres URL used ONLY to provision/drop the ephemeral test
    /// schema (env WAMN_PG_ADMIN_URL). Required for every gate except
    /// `regression`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which gate to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Wall-clock seconds the delay-gate flow parks for (default 24h).
    #[arg(long, default_value_t = 86_400)]
    pub delay_secs: u64,

    /// Pool max size (passed to both plugin instances).
    #[arg(long, default_value_t = 8)]
    pub pool_max: usize,
}

/// The single component identity the runner executes under in this bench.
const BENCH_ID: &str = "testhost-bench";
/// Tenant claim injected for both stores (kept distinct from flowbench's
/// `flow-tenant` so S6 rows in the shared `s3` schema stay RLS-isolated).
const TENANT: &str = "s6-tenant";
/// Shared production fixture schema (already in deploy/sql/postgres-init.sql).
const PROD_SCHEMA: &str = "s3";
/// Per-run ephemeral schema the test host provisions from the template DDL.
const EPH_SCHEMA: &str = "s6_test";
/// The intentionally-added unexpected outbound call: a link-local cloud
/// metadata endpoint (a classic SSRF target). Its authority is NOT on the test
/// store's expectation list, so the egress spy must flag and deny it.
const PLANTED_URL: &str = "http://169.254.169.254/latest/meta-data/";

// ---------------------------------------------------------------------------
// Virtual clock (the time capability the test host swaps in)
// ---------------------------------------------------------------------------

/// A wall clock the harness drives. Shared (Arc) so the harness can advance the
/// same instant the store's `WasiCtx` reads.
#[derive(Clone)]
struct VirtualClock {
    nanos: Arc<std::sync::atomic::AtomicU64>,
}

impl VirtualClock {
    fn at_secs(secs: u64) -> Self {
        Self {
            nanos: Arc::new(std::sync::atomic::AtomicU64::new(
                secs.saturating_mul(1_000_000_000),
            )),
        }
    }
    fn advance_secs(&self, secs: u64) {
        self.nanos.fetch_add(
            secs.saturating_mul(1_000_000_000),
            std::sync::atomic::Ordering::SeqCst,
        );
    }
    fn now_nanos(&self) -> u64 {
        self.nanos.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// `HostWallClock` backed by the shared [`VirtualClock`]. Injected into the test
/// store's `WasiCtx` via `WasiCtxBuilder::wall_clock`.
struct VirtualWallClock(VirtualClock);

impl HostWallClock for VirtualWallClock {
    fn resolution(&self) -> Duration {
        Duration::from_nanos(1)
    }
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.now_nanos())
    }
}

// ---------------------------------------------------------------------------
// Egress spy (the wasi:http capability the test host swaps in)
// ---------------------------------------------------------------------------

/// A shared, mutable log of egress URIs (recorded and flagged lists). Shared
/// with the harness so a phase can read what the store's egress handler saw.
type EgressLog = Arc<Mutex<Vec<String>>>;

/// Records every outbound request and, in spy mode, denies any whose authority
/// is not on the expectation list. Expected calls (and all calls in
/// forward-all/prod mode) delegate to [`DefaultOutgoingHandler`] — a real HTTP
/// send to the loopback echo.
struct EgressHandler {
    inner: DefaultOutgoingHandler,
    /// `"METHOD uri"` for every outbound request seen.
    records: EgressLog,
    /// URIs that were flagged unexpected and denied.
    flagged: EgressLog,
    /// `Some(authorities)` = spy mode (deny anything not listed); `None` =
    /// forward-all (prod).
    expected: Option<HashSet<String>>,
}

impl EgressHandler {
    fn shared() -> (EgressLog, EgressLog) {
        (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        )
    }
}

#[async_trait::async_trait]
impl HostHandler for EgressHandler {
    async fn start(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn port(&self) -> u16 {
        0
    }
    async fn on_workload_resolved(
        &self,
        _resolved: &ResolvedWorkload,
        _component_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn on_workload_unbind(&self, _workload_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn outgoing_request(
        &self,
        workload_id: &str,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
        _allowed_hosts: &[wash_runtime::host::allowed_hosts::AllowedHost],
    ) -> HttpResult<HostFutureIncomingResponse> {
        let authority = request
            .uri()
            .authority()
            .map(|a| a.to_string())
            .unwrap_or_default();
        let uri = request.uri().to_string();
        self.records
            .lock()
            .expect("records lock")
            .push(format!("{} {}", request.method(), uri));

        if let Some(expected) = &self.expected
            && !expected.contains(&authority)
        {
            // Unexpected egress: record, flag, and stub a denial WITHOUT ever
            // performing the request — the call never leaves the host.
            self.flagged.lock().expect("flagged lock").push(uri);
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestDenied,
            ))));
        }
        self.inner.send_request(workload_id, request, config)
    }
}

// ---------------------------------------------------------------------------
// Worker: an instantiated flowrunner with the S6 exports resolved
// ---------------------------------------------------------------------------

/// `run-s6` returns `(outcome, http-status)` behind the WIT result/tuple ABI.
type RunS6Fn = TypedFunc<(String, String), (Result<(u32, u32), String>,)>;

struct Worker {
    store: Store<SharedCtx>,
    run_s6: RunS6Fn,
    reset: TypedFunc<(String,), (Result<u64, String>,)>,
    sink_count: TypedFunc<(String,), (Result<u64, String>,)>,
}

impl Worker {
    /// Returns (outcome, http-status): outcome 0 = completed, 1 = parked.
    async fn call_run_s6(&mut self, run_id: &str, payload: &str) -> anyhow::Result<(u32, u32)> {
        let (r,) = self
            .run_s6
            .call_async(&mut self.store, (run_id.to_string(), payload.to_string()))
            .await?;
        r.map_err(|e| anyhow::anyhow!("run-s6: {e}"))
    }
    async fn call_reset(&mut self, run_id: &str) -> anyhow::Result<u64> {
        let (r,) = self
            .reset
            .call_async(&mut self.store, (run_id.to_string(),))
            .await?;
        r.map_err(|e| anyhow::anyhow!("reset: {e}"))
    }
    async fn call_sink_count(&mut self, run_id: &str) -> anyhow::Result<u64> {
        let (r,) = self
            .sink_count
            .call_async(&mut self.store, (run_id.to_string(),))
            .await?;
        r.map_err(|e| anyhow::anyhow!("sink-count: {e}"))
    }
}

/// The compiled+linked guest, shared across both stores.
struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: InstancePre<SharedCtx>,
    digest: u64,
}

impl Harness {
    fn new(engine: wash_runtime::engine::Engine, guest: &[u8]) -> anyhow::Result<Self> {
        let raw: &RawEngine = engine.inner();
        let component = WasmtimeComponent::new(raw, guest)
            .map_err(|e| anyhow::anyhow!("compile flowrunner: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        // The http-call node imports wasi:http/outgoing-handler; egress flows
        // through the store's http_handler (our EgressHandler).
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        wamn_postgres::add_to_linker(&mut linker)?;
        // 5.9: the runner imports wamn:node/credentials unconditionally; no
        // S6 fixture declares one, so the linked vault stays unbacked.
        wamn_host::plugins::wamn_credentials::add_to_linker(&mut linker)?;
        // cjv.3: the flowrunner declares its per-run grant via this trusted
        // channel; the harness must link it or instantiation fails.
        wamn_host::plugins::wamn_credentials::add_runner_to_linker(&mut linker)?;
        // fqg.11: the flowrunner declares its per-run egress the same way.
        wamn_host::plugins::runner_egress::add_runner_to_linker(&mut linker)?;
        // l5i9.12.2: the trusted per-run causation channel (the flowrunner world
        // now imports it; instantiation traps without it).
        wamn_postgres::add_runner_causation_to_linker(&mut linker)?;
        // wamn-yf3: the flowrunner world now imports wasi:logging (run-path
        // emission). This harness registers no wamn:logging plugin, so log() is a
        // best-effort no-op — but the import must be linked or instantiation traps.
        wamn_host::plugins::wamn_logging::add_to_linker(&mut linker)?;
        let pre = linker.instantiate_pre(&component)?;
        Ok(Self {
            engine,
            pre,
            digest: fnv1a(guest),
        })
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
        // cjv.3: the flowrunner declares its per-run grant on every walk, so a
        // credentials plugin must back the linked interface. No S3/S6 fixture
        // declares a credential, so an empty unbacked vault suffices.
        m.insert(
            wamn_host::plugins::wamn_credentials::WAMN_CREDENTIALS_ID,
            Arc::new(wamn_host::plugins::wamn_credentials::WamnCredentials::empty())
                as Arc<dyn HostPlugin + Send + Sync>,
        );
        // fqg.11: the flowrunner declares its per-run egress on every walk, so
        // the policy plugin must back the linked interface. Enforcement here is
        // the harness's own http handler, so the declaration is inert — the
        // plugin exists to keep the trusted channel satisfied.
        m.insert(
            wamn_host::plugins::runner_egress::RUNNER_EGRESS_ID,
            Arc::new(wamn_host::plugins::runner_egress::RunnerEgressPolicy::default())
                as Arc<dyn HostPlugin + Send + Sync>,
        );
        m
    }

    /// Build a worker. `wasi` overrides the store's `WasiCtx` (the virtual clock
    /// for the test store; None = default real clock for prod). `egress` is the
    /// store's outbound HTTP handler.
    async fn worker(
        &self,
        plugin: &Arc<WamnPostgres>,
        wasi: Option<wasmtime_wasi::WasiCtx>,
        egress: Arc<dyn HostHandler>,
    ) -> anyhow::Result<Worker> {
        let mut builder = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(self.plugin_map(plugin))
            .with_http_handler(egress);
        if let Some(wasi) = wasi {
            builder = builder.with_wasi_ctx(wasi);
        }
        let ctx = builder.build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = self.pre.instantiate_async(&mut store).await?;
        macro_rules! f {
            ($name:literal) => {
                instance.get_typed_func(&mut store, $name)?
            };
        }
        let run_s6 = f!("run-s6");
        let reset = f!("reset");
        let sink_count = f!("sink-count");
        Ok(Worker {
            store,
            run_s6,
            reset,
            sink_count,
        })
    }
}

/// FNV-1a 64-bit digest, so the report can show both stores ran identical bytes
/// without pulling in a hashing dependency.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------
// Ephemeral schema provisioning (superuser / admin path)
// ---------------------------------------------------------------------------

/// The template DDL for the flow tables, cloned into `schema`. Mirrors the s3
/// fixture (deploy/sql/postgres-init.sql): flows / flow_runs / sink plus the 5.7
/// run-state tables (runs / node_runs — the runner's branch-aware reconstruction
/// source), same columns, idempotency keys, and RLS shape, so the ephemeral
/// schema is a faithful stand-in. (`flow_runs` is retained but unused: the runner
/// now checkpoints per node into node_runs.)
fn template_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.flows (\
            tenant_id text NOT NULL, flow_id text NOT NULL, version int NOT NULL, \
            active boolean NOT NULL DEFAULT false, graph_json jsonb NOT NULL, \
            PRIMARY KEY (tenant_id, flow_id, version));\
         ALTER TABLE {schema}.flows ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flows FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flows_tenant ON {schema}.flows \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flows TO wamn_app;\
         CREATE TABLE {schema}.flow_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, step_seq int NOT NULL DEFAULT -1, \
            status text NOT NULL DEFAULT 'running', state_json jsonb, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.flow_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flow_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flow_runs_tenant ON {schema}.flow_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flow_runs TO wamn_app;\
         CREATE TABLE {schema}.sink (\
            tenant_id text NOT NULL, run_id text NOT NULL, step int NOT NULL, \
            payload text NOT NULL, \
            CONSTRAINT sink_idem UNIQUE (tenant_id, run_id, step));\
         ALTER TABLE {schema}.sink ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.sink FORCE ROW LEVEL SECURITY;\
         CREATE POLICY sink_tenant ON {schema}.sink \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.sink TO wamn_app;\
         CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, status text NOT NULL DEFAULT 'running', \
            trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
            updated_at timestamptz NOT NULL DEFAULT now(), \
            idempotency_key text, replay_of text, root_run_id text, \
            fail_kind text, fail_node text, fail_reason text, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.node_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
            occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, attempt int NOT NULL DEFAULT 0, \
            status text NOT NULL, output_port text, output_json jsonb, input_json jsonb, \
            error_kind text, error_detail jsonb, resume_at timestamptz, \
            PRIMARY KEY (tenant_id, run_id, node_id, occurrence), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.node_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.node_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY node_runs_tenant ON {schema}.node_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.node_runs TO wamn_app;"
    )
}

/// Drop-and-recreate `schema` from the template DDL, via a superuser connection.
async fn provision_schema(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for ephemeral schema")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema} AUTHORIZATION postgres; GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        client
            .batch_execute(&template_ddl(schema))
            .await
            .context("apply template DDL")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn drop_schema(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("drop ephemeral schema: {e}"));
    drop(client);
    let _ = conn_task.await;
    r.map(|_| ())
}

/// Seed the S6 flow (`poc-s6` v1, active) into `schema` host-side — the
/// replacement for the guest's retired `seed-s6` export (SR2). `admin` is the
/// persistent superuser connection (RLS-bypassing); `scope_session` pins its
/// `search_path` to the target schema so the unqualified `flows` insert lands
/// there. The flow JSON is the shared flowbench S6 fixture.
async fn seed_s6_flow(
    admin: &tokio_postgres::Client,
    schema: &str,
    delay_secs: u64,
    url: &str,
) -> anyhow::Result<()> {
    wamn_gate_harness::scope_session(admin, TENANT, schema).await?;
    wamn_gate_harness::seed_flow_version(
        admin,
        TENANT,
        "poc-s6",
        1,
        true,
        &crate::flowbench::flow_json_s6(delay_secs, url),
        true,
    )
    .await
}

/// Re-seed `poc-s6` v1 as the TWO-delay fixture (wamn-2jkm.51), replacing the
/// single-delay graph in place (ON CONFLICT DO UPDATE) so `run-s6` drives the
/// two-delay flow. The direct `execute` path re-reads the active flow per call
/// (no plan cache), so the new graph is picked up immediately.
async fn seed_s6_twodelay_flow(
    admin: &tokio_postgres::Client,
    schema: &str,
    delay_secs: u64,
) -> anyhow::Result<()> {
    wamn_gate_harness::scope_session(admin, TENANT, schema).await?;
    wamn_gate_harness::seed_flow_version(
        admin,
        TENANT,
        "poc-s6",
        1,
        true,
        &crate::flowbench::flow_json_s6_twodelay(delay_secs),
        true,
    )
    .await
}

// ---------------------------------------------------------------------------
// Loopback echo server (the real egress target for expected/prod calls)
// ---------------------------------------------------------------------------

/// A minimal HTTP/1.1 server that answers every request with `200 ok` and
/// closes. It gives the prod host (and the test host's *expected* calls) a real
/// reachable endpoint so the same http-call node succeeds for real.
async fn spawn_echo() -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                // Read (and ignore) the request head; we only need to answer.
                let _ = sock.read(&mut buf).await;
                let body = b"ok";
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
            });
        }
    });
    Ok((addr, handle))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run(args: TestHostBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("failed to read {}", args.flowrunner.display()))?;

    let run_all = args.mode == Mode::All;
    let needs_testhost =
        run_all || matches!(args.mode, Mode::Sameness | Mode::Delay | Mode::Egress);

    println!("# wamn-host S6 testhostbench");

    // ---- regression-only fast path: no test host / DB provisioning needed ----
    if args.mode == Mode::Regression {
        let ok = regression_phase(&args).await?;
        println!("\ntesthostbench complete — overall PASS: {ok}");
        if !ok {
            bail!("S3 regression failed on the extended binary");
        }
        return Ok(());
    }

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.pool_max_size = args.pool_max;
    if needs_testhost && cfg.database_url.is_none() {
        bail!("no database url: pass --database-url or set DATABASE_URL / WAMN_PG_URL");
    }
    let admin_url = args.admin_database_url.clone();
    if needs_testhost && admin_url.is_none() {
        bail!("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL");
    }

    // ---- prod + test plugin instances (separate pools => stable per-pool
    //      search_path, so prepared-statement plans never alias schemas) ----
    let prod_pg = Arc::new(WamnPostgres::new(cfg.clone())?);
    prod_pg.set_tenant(BENCH_ID, TENANT)?;
    prod_pg.set_schema(BENCH_ID, PROD_SCHEMA)?;
    let test_pg = Arc::new(WamnPostgres::new(cfg.clone())?);
    test_pg.set_tenant(BENCH_ID, TENANT)?;
    test_pg.set_schema(BENCH_ID, EPH_SCHEMA)?;

    prod_pg
        .probe_checkout()
        .await
        .context("prod postgres preflight")?;

    // ---- provision the ephemeral test schema (superuser) ----
    let admin_url = admin_url.expect("checked above");
    provision_schema(&admin_url, EPH_SCHEMA)
        .await
        .context("provision ephemeral schema")?;
    println!("provisioned ephemeral schema {EPH_SCHEMA} from template DDL");

    // A persistent superuser connection for host-side flow seeding (SR2: the
    // guest's `seed-s6` export is retired). Superuser bypasses RLS; each seed
    // re-scopes `search_path` so the flow lands in the target schema (`s3` for the
    // prod wiring, `s6_test` for the test wiring).
    let (admin, admin_conn) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .context("admin connect for host-side flow seeding")?;
    let admin_handle = tokio::spawn(async move {
        let _ = admin_conn.await;
    });

    // ---- shared infra: engine, echo server, virtual clock, egress handlers ----
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let harness = Harness::new(engine, &guest)?;
    println!(
        "compiled flowrunner once (fnv1a digest {:#018x})",
        harness.digest
    );

    let (echo_addr, echo_task) = spawn_echo().await?;
    let echo_authority = format!("127.0.0.1:{}", echo_addr.port());
    let echo_url = format!("http://{echo_authority}/echo");
    println!("loopback echo listening on {echo_authority}");

    // Prod egress: forward everything (no spy).
    let (prod_rec, prod_flag) = EgressHandler::shared();
    let prod_egress: Arc<dyn HostHandler> = Arc::new(EgressHandler {
        inner: DefaultOutgoingHandler,
        records: prod_rec,
        flagged: prod_flag,
        expected: None,
    });
    // Test egress spy: only the echo authority is expected; anything else is
    // flagged and denied.
    let (spy_rec, spy_flag) = EgressHandler::shared();
    let mut expected = HashSet::new();
    expected.insert(echo_authority.clone());
    let spy_egress: Arc<dyn HostHandler> = Arc::new(EgressHandler {
        inner: DefaultOutgoingHandler,
        records: spy_rec.clone(),
        flagged: spy_flag.clone(),
        expected: Some(expected),
    });

    let vclock = VirtualClock::at_secs(1_700_000_000); // arbitrary fixed epoch base
    let test_wasi = || {
        WasiCtxBuilder::new()
            .args(&["main.wasm"])
            .inherit_stderr()
            .wall_clock(VirtualWallClock(vclock.clone()))
            .build()
    };

    // Build the two workers from the SAME InstancePre.
    let mut prod = harness
        .worker(&prod_pg, None, prod_egress.clone())
        .await
        .context("build prod worker")?;
    let mut test = harness
        .worker(&test_pg, Some(test_wasi()), spy_egress.clone())
        .await
        .context("build test worker")?;

    let mut pass = true;

    if run_all || args.mode == Mode::Sameness {
        pass &= sameness_phase(&mut prod, &mut test, &admin, &echo_url, harness.digest).await?;
    }
    if run_all || args.mode == Mode::Delay {
        pass &= delay_phase(
            &mut prod,
            &mut test,
            &admin,
            &vclock,
            &echo_url,
            args.delay_secs,
        )
        .await?;
    }
    if run_all || args.mode == Mode::Egress {
        pass &= egress_phase(
            &mut test,
            &admin,
            &echo_url,
            &echo_authority,
            &spy_rec,
            &spy_flag,
        )
        .await?;
    }

    // Tear down stores (and their pools) before dropping the ephemeral schema.
    drop(prod);
    drop(test);
    drop(prod_pg);
    drop(test_pg);
    drop(admin);
    admin_handle.abort();
    if let Err(e) = drop_schema(&admin_url, EPH_SCHEMA).await {
        tracing::warn!(error = %e, "ephemeral schema teardown failed (non-fatal)");
    }
    echo_task.abort();

    if run_all {
        pass &= regression_phase(&args).await?;
    }

    ticker.abort();
    println!("\ntesthostbench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more S6 gates failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// sameness
// ---------------------------------------------------------------------------

async fn sameness_phase(
    prod: &mut Worker,
    test: &mut Worker,
    admin: &tokio_postgres::Client,
    echo_url: &str,
    digest: u64,
) -> anyhow::Result<bool> {
    println!("\n## sameness — identical bytes (fnv1a {digest:#018x}) run under BOTH host wirings");
    // A zero-delay delay+http flow: completes in one call on each store.
    let mut ok = true;
    for (label, schema, w) in [
        ("prod", PROD_SCHEMA, &mut *prod),
        ("test", EPH_SCHEMA, &mut *test),
    ] {
        let run_id = format!("same-{label}");
        w.call_reset(&run_id).await?;
        seed_s6_flow(admin, schema, 0, echo_url).await?;
        let (outcome, http) = w.call_run_s6(&run_id, "receipt").await?;
        let sink = w.call_sink_count(&run_id).await?;
        let this = outcome == 0 && sink == 1;
        // http status: prod forwards to the echo (200); test's expected call is
        // also forwarded (200). Report it but don't gate sameness on it.
        println!(
            "{label}: outcome={outcome} (0=completed), http={http}, sink_rows={sink} -> {this}"
        );
        ok &= this;
    }
    println!("PASS(sameness: same binary completes under prod + test host): {ok}");
    Ok(ok)
}

// ---------------------------------------------------------------------------
// delay
// ---------------------------------------------------------------------------

async fn delay_phase(
    prod: &mut Worker,
    test: &mut Worker,
    admin: &tokio_postgres::Client,
    vclock: &VirtualClock,
    echo_url: &str,
    delay_secs: u64,
) -> anyhow::Result<bool> {
    println!(
        "\n## delay — a {delay_secs}s ({:.1}h) delay flow completes < 1s wall under virtual time",
        delay_secs as f64 / 3600.0
    );

    // Test store: seed the long delay, run once (parks), advance the virtual
    // clock past the deadline, run again (completes) — all in real milliseconds.
    let run_id = "delay-test";
    test.call_reset(run_id).await?;
    seed_s6_flow(admin, EPH_SCHEMA, delay_secs, echo_url).await?;

    let t0 = Instant::now();
    let (o1, _) = test.call_run_s6(run_id, "receipt").await?;
    let parked = o1 == 1;
    println!(
        "test: first run -> outcome={o1} ({})",
        if parked { "parked" } else { "NOT parked" }
    );
    vclock.advance_secs(delay_secs + 1);
    let (o2, http) = test.call_run_s6(run_id, "receipt").await?;
    let wall = t0.elapsed();
    let completed = o2 == 0;
    let sink = test.call_sink_count(run_id).await?;
    println!(
        "test: advanced virtual clock +{}s, second run -> outcome={o2} ({}), http={http}, sink_rows={sink}",
        delay_secs + 1,
        if completed {
            "completed"
        } else {
            "NOT completed"
        }
    );
    println!("test: wall time for the whole 24h-delay flow = {wall:?}");

    // Prod store (real clock): the same flow parks and does NOT complete within
    // the bench window — proving the delay is real and only virtual time
    // collapses it. (We do not wait 24h.)
    let prun = "delay-prod";
    prod.call_reset(prun).await?;
    seed_s6_flow(admin, PROD_SCHEMA, delay_secs, echo_url).await?;
    let (po, _) = prod.call_run_s6(prun, "receipt").await?;
    let prod_parks = po == 1;
    println!(
        "prod: real-clock run -> outcome={po} ({}) — stays parked",
        if prod_parks { "parked" } else { "NOT parked" }
    );

    let time_ok = wall < Duration::from_secs(1);
    let single_pass = parked && completed && sink == 1 && time_ok && prod_parks;
    println!(
        "PASS(delay < 1s under virtual time; real clock stays parked): {single_pass} (parked={parked}, completed={completed}, wall_ok={time_ok}, prod_parks={prod_parks})"
    );

    // wamn-2jkm.51: two delay nodes must park INDEPENDENTLY. Re-seed poc-s6 as a
    // TWO-delay flow (in -> d1 -> d2 -> pg-write -> respond) and drive it. Pre-fix,
    // one global `wake` key made d2 read d1's already-elapsed deadline and emit
    // AT ONCE, so the run COMPLETED after a single clock advance. Post-fix (wake
    // keyed by node id + cleared on emit), d1's elapse leaves d2 to arm a FRESH
    // deadline and PARK again — the run needs a SECOND advance to complete.
    println!("\n## two-delay (wamn-2jkm.51) — the second delay actually delays");
    seed_s6_twodelay_flow(admin, EPH_SCHEMA, delay_secs).await?;
    let r2 = "delay-twodelay";
    test.call_reset(r2).await?;
    let (a1, _) = test.call_run_s6(r2, "receipt").await?; // parks on d1
    let d1_parked = a1 == 1;
    vclock.advance_secs(delay_secs + 1);
    let (a2, _) = test.call_run_s6(r2, "receipt").await?; // d1 emits -> d2 must PARK
    let d2_parked = a2 == 1;
    let sink_mid = test.call_sink_count(r2).await?; // still 0 — pg-write not reached
    vclock.advance_secs(delay_secs + 1);
    let (a3, _) = test.call_run_s6(r2, "receipt").await?; // d2 emits -> completes
    let two_done = a3 == 0;
    let sink_two = test.call_sink_count(r2).await?;
    let two_pass = d1_parked && d2_parked && sink_mid == 0 && two_done && sink_two == 1;
    println!(
        "PASS(second delay actually delays): {two_pass} (d1_parked={d1_parked}, d2_parked={d2_parked} [pre-fix: false — d2 emits at once], sink_after_d1={sink_mid}, completed={two_done}, sink_final={sink_two})"
    );

    Ok(single_pass && two_pass)
}

// ---------------------------------------------------------------------------
// egress
// ---------------------------------------------------------------------------

async fn egress_phase(
    test: &mut Worker,
    admin: &tokio_postgres::Client,
    echo_url: &str,
    echo_authority: &str,
    records: &EgressLog,
    flagged: &EgressLog,
) -> anyhow::Result<bool> {
    println!("\n## egress — the spy catches an intentionally-added unexpected outbound call");

    // Scenario A: an EXPECTED call to the loopback echo — recorded, forwarded,
    // 200, not flagged.
    records.lock().expect("rec").clear();
    flagged.lock().expect("flag").clear();
    let a = "egress-expected";
    test.call_reset(a).await?;
    seed_s6_flow(admin, EPH_SCHEMA, 0, echo_url).await?;
    let (_, http_a) = test.call_run_s6(a, "receipt").await?;
    let flagged_a = flagged.lock().expect("flag").clone();
    let saw_expected = records
        .lock()
        .expect("rec")
        .iter()
        .any(|r| r.contains(echo_authority));
    println!(
        "expected call {echo_url}: http={http_a}, flagged={flagged_a:?}, recorded_expected={saw_expected}"
    );
    let expected_ok = http_a == 200 && flagged_a.is_empty() && saw_expected;

    // Scenario B: an intentionally-planted call to an UNEXPECTED authority — the
    // spy must flag and DENY it (http status 0, never leaves the host).
    records.lock().expect("rec").clear();
    flagged.lock().expect("flag").clear();
    let b = "egress-planted";
    test.call_reset(b).await?;
    seed_s6_flow(admin, EPH_SCHEMA, 0, PLANTED_URL).await?;
    let (outcome_b, http_b) = test.call_run_s6(b, "receipt").await?;
    let flagged_b = flagged.lock().expect("flag").clone();
    let caught = flagged_b.iter().any(|u| u.contains("169.254.169.254"));
    println!(
        "planted call {PLANTED_URL}: outcome={outcome_b}, http={http_b}, flagged={flagged_b:?}, caught={caught}"
    );
    // Denied => guest observed status 0 (no response), and the spy flagged it.
    let planted_ok = caught && http_b == 0;

    let pass = expected_ok && planted_ok;
    println!(
        "PASS(egress spy: forwards expected, catches+denies planted): {pass} (expected_ok={expected_ok}, planted_ok={planted_ok})"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// regression (re-run the S3 flowbench gates on the extended binary)
// ---------------------------------------------------------------------------

async fn regression_phase(args: &TestHostBenchArgs) -> anyhow::Result<bool> {
    println!("\n## regression — re-run the S3 flowbench gates on the extended binary");
    let fb = crate::flowbench::FlowBenchArgs {
        flowrunner: args.flowrunner.clone(),
        database_url: args.database_url.clone(),
        mode: crate::flowbench::Mode::All,
        dispatch_iters: 200_000,
        hotreload_iters: 5,
        resume_iters: 10,
        pool_max: args.pool_max,
    };
    match crate::flowbench::run(fb).await {
        Ok(()) => {
            println!("PASS(regression: S3 gates hold on the extended binary): true");
            Ok(true)
        }
        Err(e) => {
            println!("PASS(regression: S3 gates hold on the extended binary): false ({e})");
            Ok(false)
        }
    }
}
