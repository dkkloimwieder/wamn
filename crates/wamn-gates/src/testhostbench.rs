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

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::host::http::HostHandler;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{
    Component as WasmtimeComponent, InstancePre, Linker, TypedFunc,
};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};

use wamn_gate_harness::scope_session;
// wamn-t92: the S6 doubles now live in the production host library as reusable
// test-host machinery; this bench drives them (the regression proof that the
// extraction changed nothing).
use wamn_host::doubles::{
    DoubleSet, EgressRecorder, EphemeralSchemaProvisioner, RUN_S6_WAKE_DEADLINES_SQL,
    SchedulerBackend, TestScheduler, VirtualClock, build_virtual_wasi, case_pool,
};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_credentials::WamnCredentials;
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};
use wamn_run_worker::{RunWorker, RunnerIdentity};

/// The virtual-clock epoch + `wasi:random` seed the test host uses (fixed for
/// reproducibility, matching the run-worker `--test-doubles` constants).
const TEST_EPOCH_SECS: u64 = 1_700_000_000;
const TEST_SEED: u64 = 0x7492_5EED_5EED_7492;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Same binary runs under both host wirings.
    Sameness,
    /// 24h-delay flow completes < 1s under virtual time.
    Delay,
    /// Egress spy catches a planted unexpected outbound call.
    Egress,
    /// The test scheduler auto-advances the virtual clock to each parked-wake
    /// deadline (no manual advance) and drives a 24h delay to completion < 1s.
    Scheduler,
    /// N sequential ephemeral schema CASES (create → run → drop) prove per-case
    /// isolation via the test-runner-owned provisioner.
    Schemacase,
    /// The production `RunWorker` under the `--test-doubles` set: it claims from
    /// a real `run_queue` and drives a flow with the virtual clock + seeded
    /// random + egress recorder swapped in (the test host is the run-worker build).
    Runworker,
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

// The virtual clock (`VirtualClock`/`VirtualWallClock`) and the egress spy
// (`EgressRecorder`) that used to live here are now reusable test-host machinery
// in `wamn_host::doubles` (wamn-t92). This bench drives that library — the
// regression proof the extraction changed nothing.

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

// Schema create/drop is now owned by `wamn_host::doubles::EphemeralSchemaProvisioner`
// (`template_ddl` above is the case template it renders). The bench passes
// `template_ddl` to the provisioner (delta 4).

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
    let needs_testhost = run_all
        || matches!(
            args.mode,
            Mode::Sameness
                | Mode::Delay
                | Mode::Egress
                | Mode::Scheduler
                | Mode::Schemacase
                | Mode::Runworker
        );

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

    // ---- the test-runner-owned ephemeral schema provisioner (delta 4) ----
    // Owns the persistent superuser session AND host-side flow seeding
    // (RLS-bypassing; SR2 retired the guest's `seed-s6` export). Renders the
    // flow tables per case from `template_ddl`.
    let admin_url = admin_url.expect("checked above");
    let provisioner = EphemeralSchemaProvisioner::connect(&admin_url, template_ddl)
        .await
        .context("connect ephemeral schema provisioner")?;
    provisioner
        .provision_case(EPH_SCHEMA)
        .await
        .context("provision ephemeral schema")?;
    println!("provisioned ephemeral schema {EPH_SCHEMA} from template DDL");

    // ---- shared infra: engine, echo server, virtual clock, egress recorders ----
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

    // Prod egress: forward everything (audit only). Test egress: a spy that
    // denies any authority not on the flow's expectation list — the S6 spy
    // generalized (delta 3). The bench flow key is the store's workload id.
    let prod_egress: Arc<dyn HostHandler> = Arc::new(EgressRecorder::forwarding());
    let spy = Arc::new(EgressRecorder::spying());
    spy.expect(BENCH_ID, [echo_authority.clone()]);
    let spy_egress: Arc<dyn HostHandler> = spy.clone();

    // The virtual clock the test store reads as its wall clock (and the seeded
    // random) — the extracted double set.
    let vclock = VirtualClock::at_secs(TEST_EPOCH_SECS);
    let test_wasi = build_virtual_wasi(&vclock, TEST_SEED);

    // Build the two workers from the SAME InstancePre.
    let mut prod = harness
        .worker(&prod_pg, None, prod_egress.clone())
        .await
        .context("build prod worker")?;
    let mut test = harness
        .worker(&test_pg, Some(test_wasi), spy_egress.clone())
        .await
        .context("build test worker")?;

    let mut pass = true;
    let admin = provisioner.admin();

    if run_all || args.mode == Mode::Sameness {
        pass &= sameness_phase(&mut prod, &mut test, admin, &echo_url, harness.digest).await?;
    }
    if run_all || args.mode == Mode::Delay {
        pass &= delay_phase(
            &mut prod,
            &mut test,
            admin,
            &vclock,
            &echo_url,
            args.delay_secs,
        )
        .await?;
    }
    if run_all || args.mode == Mode::Egress {
        pass &= egress_phase(&mut test, admin, &echo_url, &echo_authority, &spy).await?;
    }
    if run_all || args.mode == Mode::Scheduler {
        pass &= scheduler_phase(&mut test, admin, &vclock, &echo_url, args.delay_secs).await?;
    }

    // Tear down the main stores (and pools) before dropping the ephemeral schema
    // and before the self-contained phases reuse the DB.
    drop(prod);
    drop(test);
    drop(prod_pg);
    drop(test_pg);
    if let Err(e) = provisioner.drop_case(EPH_SCHEMA).await {
        tracing::warn!(error = %e, "ephemeral schema teardown failed (non-fatal)");
    }

    // The per-case + run-worker phases own their own provisioning (a fresh
    // schema per case; the run_queue union schema for the run-worker path).
    if run_all || args.mode == Mode::Schemacase {
        pass &= schemacase_phase(&harness, &cfg, &admin_url, &echo_url).await?;
    }
    if run_all || args.mode == Mode::Runworker {
        pass &= runworker_phase(
            &harness,
            &guest,
            &cfg,
            &admin_url,
            &echo_url,
            &echo_authority,
        )
        .await?;
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
    spy: &EgressRecorder,
) -> anyhow::Result<bool> {
    println!("\n## egress — the spy catches an intentionally-added unexpected outbound call");

    // Scenario A: an EXPECTED call to the loopback echo — recorded, forwarded,
    // 200, not denied.
    spy.clear();
    let a = "egress-expected";
    test.call_reset(a).await?;
    seed_s6_flow(admin, EPH_SCHEMA, 0, echo_url).await?;
    let (_, http_a) = test.call_run_s6(a, "receipt").await?;
    let denied_a = spy.denied();
    let saw_expected = spy.saw_authority(echo_authority);
    println!(
        "expected call {echo_url}: http={http_a}, denied={denied_a:?}, recorded_expected={saw_expected}"
    );
    let expected_ok = http_a == 200 && denied_a.is_empty() && saw_expected;

    // Scenario B: an intentionally-planted call to an UNEXPECTED authority — the
    // spy must record and DENY it (http status 0, never leaves the host).
    spy.clear();
    let b = "egress-planted";
    test.call_reset(b).await?;
    seed_s6_flow(admin, EPH_SCHEMA, 0, PLANTED_URL).await?;
    let (outcome_b, http_b) = test.call_run_s6(b, "receipt").await?;
    let denied_b = spy.denied();
    let caught = denied_b
        .iter()
        .any(|r| r.authority.contains("169.254.169.254"));
    println!(
        "planted call {PLANTED_URL}: outcome={outcome_b}, http={http_b}, denied={denied_b:?}, caught={caught}"
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

// ---------------------------------------------------------------------------
// scheduler (delta 2): the test scheduler auto-advances the virtual clock
// ---------------------------------------------------------------------------

/// A [`SchedulerBackend`] over the run-s6 path: the parked-wake deadlines live in
/// `runs.state_json->'wake'` (epoch seconds, from the guest's virtual wall
/// clock), and re-driving re-invokes `run-s6` for each still-parked run.
struct RunS6Backend<'a> {
    worker: &'a mut Worker,
    admin: &'a tokio_postgres::Client,
    schema: &'a str,
    /// (run_id, payload) for each run still being driven; completed runs are
    /// dropped so `run-s6` is never re-invoked on a finished run.
    runs: Vec<(String, String)>,
}

#[async_trait::async_trait]
impl SchedulerBackend for RunS6Backend<'_> {
    async fn wake_deadlines_nanos(&mut self) -> anyhow::Result<Vec<u64>> {
        // The admin (superuser) session must carry the tenant claim + search_path
        // for the RLS-scoped, unqualified `runs` read to resolve.
        scope_session(self.admin, TENANT, self.schema).await?;
        let rows = self.admin.query(RUN_S6_WAKE_DEADLINES_SQL, &[]).await?;
        Ok(rows
            .iter()
            .map(|r| {
                let secs: i64 = r.get(0);
                (secs.max(0) as u64).saturating_mul(1_000_000_000)
            })
            .collect())
    }

    async fn redrive(&mut self) -> anyhow::Result<()> {
        let active = std::mem::take(&mut self.runs);
        let mut still = Vec::with_capacity(active.len());
        for (run_id, payload) in active {
            let (outcome, _http) = self.worker.call_run_s6(&run_id, &payload).await?;
            if outcome != 0 {
                still.push((run_id, payload));
            }
        }
        self.runs = still;
        Ok(())
    }
}

async fn scheduler_phase(
    test: &mut Worker,
    admin: &tokio_postgres::Client,
    vclock: &VirtualClock,
    echo_url: &str,
    delay_secs: u64,
) -> anyhow::Result<bool> {
    println!(
        "\n## scheduler — the test scheduler auto-advances the virtual clock to each parked-wake deadline (no manual advance)"
    );

    // (1) A single 24h delay drives to completion in < 1s wall with the scheduler
    //     reading the ACTUAL parked deadline (contrast the delay phase, which
    //     advances by a hand-known amount).
    let run_id = "sched-single";
    test.call_reset(run_id).await?;
    seed_s6_flow(admin, EPH_SCHEMA, delay_secs, echo_url).await?;
    let t0 = Instant::now();
    let (o1, _) = test.call_run_s6(run_id, "receipt").await?;
    let parked = o1 == 1;
    let steps = {
        let mut backend = RunS6Backend {
            worker: &mut *test,
            admin,
            schema: EPH_SCHEMA,
            runs: vec![(run_id.to_string(), "receipt".to_string())],
        };
        TestScheduler::new(vclock.clone())
            .drive_to_quiescence(&mut backend)
            .await?
    };
    let wall = t0.elapsed();
    let sink = test.call_sink_count(run_id).await?;
    let single_ok = parked && steps == 1 && sink == 1 && wall < Duration::from_secs(1);
    println!(
        "PASS(single 24h delay auto-driven < 1s): {single_ok} (parked={parked}, steps={steps}, sink={sink}, wall={wall:?})"
    );

    // (2) Two runs with DISTINCT deadlines (delay Δ and 2Δ) must wake in ORDER:
    //     the scheduler advances to the EARLIEST first (waking only run A), then
    //     the later (waking run B) — TWO steps. A mutant that advanced to the
    //     LATEST would wake both at once (ONE step) and fail this.
    let (short_secs, long_secs) = (delay_secs.max(1), delay_secs.max(1) * 2);
    let (ra, rb) = ("sched-a", "sched-b");
    test.call_reset(ra).await?;
    test.call_reset(rb).await?;
    // Park A at +Δ, then re-seed the active flow to +2Δ and park B at +2Δ.
    seed_s6_flow(admin, EPH_SCHEMA, short_secs, echo_url).await?;
    let (pa, _) = test.call_run_s6(ra, "receipt").await?;
    seed_s6_flow(admin, EPH_SCHEMA, long_secs, echo_url).await?;
    let (pb, _) = test.call_run_s6(rb, "receipt").await?;
    let both_parked = pa == 1 && pb == 1;
    let ordered_steps = {
        let mut backend = RunS6Backend {
            worker: &mut *test,
            admin,
            schema: EPH_SCHEMA,
            runs: vec![
                (ra.to_string(), "receipt".to_string()),
                (rb.to_string(), "receipt".to_string()),
            ],
        };
        TestScheduler::new(vclock.clone())
            .drive_to_quiescence(&mut backend)
            .await?
    };
    let sink_a = test.call_sink_count(ra).await?;
    let sink_b = test.call_sink_count(rb).await?;
    let ordered_ok = both_parked && ordered_steps == 2 && sink_a == 1 && sink_b == 1;
    println!(
        "PASS(distinct deadlines wake earliest-first, 2 steps): {ordered_ok} (both_parked={both_parked}, steps={ordered_steps}, sink_a={sink_a}, sink_b={sink_b})"
    );

    Ok(single_ok && ordered_ok)
}

// ---------------------------------------------------------------------------
// schemacase (delta 4): N sequential ephemeral schema cases prove isolation
// ---------------------------------------------------------------------------

async fn schemacase_phase(
    harness: &Harness,
    cfg: &WamnPostgresConfig,
    admin_url: &str,
    echo_url: &str,
) -> anyhow::Result<bool> {
    println!(
        "\n## schemacase — N sequential ephemeral schema CASES (create → run → drop) prove per-case isolation"
    );
    let provisioner = EphemeralSchemaProvisioner::connect(admin_url, template_ddl)
        .await
        .context("connect schemacase provisioner")?;

    let mut pass = true;
    for i in 0..2u32 {
        let schema = format!("s6_case_{i}");
        provisioner
            .provision_case(&schema)
            .await
            .with_context(|| format!("provision case {schema}"))?;

        // A FRESH case must start empty — the isolation proof. If a prior case's
        // rows survived (schema reuse), this count would be non-zero.
        scope_session(provisioner.admin(), TENANT, &schema).await?;
        let before: i64 = provisioner
            .admin()
            .query_one("SELECT count(*) FROM sink", &[])
            .await?
            .get(0);

        // A fresh app-role pool per case (prepared plans never alias schemas).
        let case_pg = case_pool(cfg, TENANT, &schema, BENCH_ID)?;
        let mut worker = harness
            .worker(
                &case_pg,
                Some(build_virtual_wasi(
                    &VirtualClock::at_secs(TEST_EPOCH_SECS),
                    TEST_SEED,
                )),
                Arc::new(EgressRecorder::forwarding()),
            )
            .await
            .with_context(|| format!("build case worker {schema}"))?;

        let run_id = format!("case-{i}");
        worker.call_reset(&run_id).await?;
        seed_s6_flow(provisioner.admin(), &schema, 0, echo_url).await?;
        let (outcome, _http) = worker.call_run_s6(&run_id, "receipt").await?;
        let sink = worker.call_sink_count(&run_id).await?;

        // Confirm the write landed in THIS case's schema (and only here).
        scope_session(provisioner.admin(), TENANT, &schema).await?;
        let after: i64 = provisioner
            .admin()
            .query_one("SELECT count(*) FROM sink", &[])
            .await?
            .get(0);

        let case_ok = before == 0 && outcome == 0 && sink == 1 && after == 1;
        println!(
            "case {schema}: fresh_before={before} (want 0), completed={}, sink={sink}, after={after} -> {case_ok}",
            outcome == 0
        );
        pass &= case_ok;

        drop(worker);
        drop(case_pg);
        provisioner.drop_case(&schema).await.ok();
    }

    println!("PASS(per-case ephemeral schema isolation across sequential cases): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// runworker (delta 1): the production RunWorker under the --test-doubles set
// ---------------------------------------------------------------------------

/// The tenant + owner the run-worker path runs under (kept distinct from the
/// run-s6 tenant so the two schemas never alias).
const RW_TENANT: &str = "s6-rw-tenant";
const RW_OWNER: &str = "s6-runworker";
const RW_SCHEMA: &str = "s6_runworker";

async fn runworker_phase(
    harness: &Harness,
    guest: &[u8],
    cfg: &WamnPostgresConfig,
    admin_url: &str,
    echo_url: &str,
    echo_authority: &str,
) -> anyhow::Result<bool> {
    println!(
        "\n## runworker — the production RunWorker claims from run_queue under the --test-doubles set (virtual clock + seeded random + egress recorder)"
    );

    // Provision the union schema (flow tables + run_queue) via the SAME
    // drift-guarded DDL the runnerbench gate uses.
    let provisioner =
        EphemeralSchemaProvisioner::connect(admin_url, crate::runnerbench::runner_ddl)
            .await
            .context("connect runworker provisioner")?;
    provisioner
        .provision_case(RW_SCHEMA)
        .await
        .context("provision runworker schema")?;

    // Seed the flow + a dispatched run + its queue row (delay 0 so it drives
    // straight through: in → delay(0) → http-call(echo) → pg-write → respond).
    let admin = provisioner.admin();
    scope_session(admin, RW_TENANT, RW_SCHEMA).await?;
    let flow_json = crate::flowbench::flow_json_s6(0, echo_url);
    wamn_gate_harness::seed_flow_version(admin, RW_TENANT, "poc-s6", 1, true, &flow_json, true)
        .await?;
    let run_id = "rw-run-0";
    admin
        .execute(
            &write_ahead_triggered_run_sql(),
            &[&run_id, &"poc-s6", &1i32, &"cron", &"\"receipt\""],
        )
        .await
        .context("seed dispatched runs row")?;
    admin
        .execute(
            &enqueue_sql(),
            &[&run_id, &Option::<&str>::None, &0i32, &0i64],
        )
        .await
        .context("enqueue run_queue row")?;

    // Build the production runner store under the test double set: virtual clock
    // + seeded random `WasiCtx` and an EgressRecorder swapped in for the prod
    // egress handler. The flow key is the runner owner (the store's workload id).
    let plugin = Arc::new(WamnPostgres::new(cfg.clone())?);
    let vault = Arc::new(WamnCredentials::empty());
    let recorder = Arc::new(EgressRecorder::spying());
    recorder.expect(RW_OWNER, [echo_authority.to_string()]);
    let (doubles, _clock) = DoubleSet::virtual_host(
        TEST_EPOCH_SECS,
        TEST_SEED,
        recorder.clone() as Arc<dyn HostHandler>,
    );

    let mut worker = RunWorker::instantiate(
        &harness.engine,
        guest,
        plugin.clone(),
        vault,
        Arc::new(wamn_host::plugins::wamn_logging::WamnLogging::from_env()?),
        RunnerIdentity {
            owner: RW_OWNER,
            tenant: RW_TENANT,
            schema: Some(RW_SCHEMA),
            project: "default",
        },
        Arc::from([]),
        30_000,
        Some(doubles),
    )
    .await
    .context("instantiate RunWorker with the test double set")?;

    let report = worker.drain().await.context("drain run_queue")?;
    let egress_ok = recorder.saw_authority(echo_authority) && recorder.denied().is_empty();
    let drain_ok = report.claimed == 1 && report.completed == 1 && report.failed == 0;
    println!(
        "runworker: {report:?}, egress_recorded={}, denied={:?}",
        recorder.saw_authority(echo_authority),
        recorder.denied()
    );
    let pass = drain_ok && egress_ok;
    println!(
        "PASS(RunWorker --test-doubles claims + drives a flow; egress recorded): {pass} (drain_ok={drain_ok}, egress_ok={egress_ok})"
    );

    drop(worker);
    drop(plugin);
    provisioner.drop_case(RW_SCHEMA).await.ok();
    Ok(pass)
}
