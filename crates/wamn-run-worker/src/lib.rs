//! The `run-worker` subcommand: the production flow runner (wamn-fqg.8 [5.14]).
//!
//! fqg.4 shipped the guest-side claim path — the flowrunner component's
//! `run-next` export claims one currently-claimable run from the durable
//! `run_queue` (`FOR UPDATE SKIP LOCKED`), reads its flow + trigger input from
//! the dispatcher-persisted `runs` row, flips it `running`, drives it with the
//! 5.2 engine (renewing the lease per node), and dequeues (terminal) or parks
//! (a `delay`). But the fqg.4 gates SEED `run_queue` directly; nothing consumed
//! it as a *running service*. This module is that service: a long-lived
//! wamn-run-worker process that instantiates the flowrunner component once and loops
//! `run-next`, so the LIVE chain closes —
//!
//!   dispatcher (fqg.3/a52) write-ahead + enqueue → run_queue → **this runner
//!   claims + drives** → `runs.status = completed`.
//!
//! Single-project (one Deployment per project, the api-gateway analog): one
//! flowrunner instance keyed to one component id, whose plugin session carries
//! the host-injected `app.runner` lease owner + tenant + `search_path`. The
//! owner is per-replica (the pod name), so leases are attributable and
//! `SKIP LOCKED` makes replicas + scale-out safe. Multi-project (a
//! dispatcher-style projects file, N instances) is a follow-up.
//!
//! Idle handling mirrors the dispatcher (NATS-optional): a doorbell hint on
//! `wamn.doorbell.<tenant>` — the subject the dispatcher already publishes to —
//! wakes an immediate drain, and a poll-with-backoff reconcile (reusing the
//! dispatcher's [`wamn_run_queue::Cadence::next_interval`] cadence) guarantees pickup
//! even when a hint is lost or NATS is absent. SIGTERM is handled explicitly
//! (PID 1 in-container gets no default disposition), so a rollout exits in
//! milliseconds instead of waiting out the grace period; abrupt death is safe
//! anyway — an in-flight run's lease simply ages out and another replica
//! reclaims it (fqg.2).
//!
//! The loop core ([`RunWorker`]) lives here in the library so the runnerbench
//! gate (wamn-gates) drives the identical code it verifies (SR1); the binary's
//! [`run`] wraps it in the doorbell + SIGTERM loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use tokio::sync::watch;
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{
    DefaultOutgoingHandler, HostHandler, OutgoingHandler as _, check_allowed_hosts,
};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker, TypedFunc};
use wasmtime_wasi_http::p2::HttpResult;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};

use wamn_host::doubles::{DoubleSet, EgressRecorder};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::runner_egress::{self, RUNNER_EGRESS_ID, RunnerEgressPolicy};
use wamn_host::plugins::wamn_credentials::{self, WAMN_CREDENTIALS_ID, WamnCredentials};
use wamn_host::plugins::wamn_logging::{self, WAMN_LOGGING_ID, WamnLogging};
use wamn_host::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

/// The fixed unix-epoch second the `--test-doubles virtual` clock starts at, and
/// the seed for its deterministic `wasi:random`. Constants (not args) so a test
/// run-worker is byte-reproducible.
const TEST_DOUBLES_EPOCH_SECS: u64 = 1_700_000_000;
const TEST_DOUBLES_SEED: u64 = 0x7492_5EED_5EED_7492;

/// The test-host double-set selector (wamn-t92). `Off` (default) is the
/// production host; `Virtual` selects the test host (virtual clock + seeded
/// random `WasiCtx` + an `EgressRecorder` swapped in for the prod egress
/// handler). A build/config selection, never a runtime mode toggled on a live
/// production service — the precedent is a second host build, not a test flag on
/// the prod path (docs/archive/structure-review.md).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum TestDoubles {
    #[default]
    Off,
    Virtual,
}

/// Default in-image path of the flowrunner component (baked into the prod host
/// image — the runner IS the production flowrunner service, so the component
/// travels with the binary, unlike the gate fixtures).
pub const DEFAULT_FLOWRUNNER_PATH: &str = "/components/flowrunner.wasm";

#[derive(Debug, Args)]
pub struct RunWorkerArgs {
    /// Path to the flowrunner component (baked into the prod image).
    #[arg(long, default_value = DEFAULT_FLOWRUNNER_PATH)]
    pub flowrunner: PathBuf,

    /// App (runner) database URL — the NOSUPERUSER wamn_app role. Overrides
    /// WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Tenant claim (the RLS floor the queue SQL is scoped by).
    #[arg(long, default_value = "default")]
    pub tenant: String,

    /// search_path for the runner's session (e.g. wamn_run). The runner uses
    /// unqualified table names, resolved through the host-injected search_path.
    #[arg(long)]
    pub schema: Option<String>,

    /// The durable-queue lease owner (`app.runner`) — must be STABLE per replica
    /// and DISTINCT across replicas so leases are attributable and a reclaim
    /// after a replica dies is owner-scoped. Defaults to $WAMN_RUNNER, then
    /// $HOSTNAME (the pod name in Kubernetes), then a fixed fallback.
    #[arg(long, env = "WAMN_RUNNER")]
    pub runner: Option<String>,

    /// The credential-vault source (5.9): a JSON file `{project: {name:
    /// secret}}` mounted from a K8s Secret — the WAMN_PG_PROJECTS_FILE
    /// pattern. A missing file leaves the vault EMPTY (every resolution is
    /// `unavailable`); a malformed file is a hard error.
    #[arg(long, env = "WAMN_CREDENTIALS_FILE")]
    pub credentials_file: Option<PathBuf>,

    /// The project whose credentials this runner's flows may read (the key
    /// into the credentials file) — single-project, like --tenant/--schema.
    #[arg(long, env = "WAMN_PROJECT", default_value = wamn_postgres::DEFAULT_PROJECT)]
    pub project: String,

    /// Hosts the runner's flows may reach over outbound wasi:http (repeatable;
    /// `host[:port]`, `scheme://host`, `*.domain`, or `*`). EMPTY = DENY-ALL —
    /// the production fail-closed posture; an http-request to an unlisted host
    /// fails `egress-denied`. Per-flow governance is the fqg.11 refinement.
    #[arg(
        long = "allowed-hosts",
        env = "WAMN_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    pub allowed_hosts: Vec<String>,

    /// Lease TTL for a claimed run (ms). The guest renews it per node, so this
    /// need only exceed the longest single-node execution, not the whole walk.
    #[arg(long, default_value_t = 30_000)]
    pub lease_ttl_ms: u64,

    /// Tightest idle poll interval (ms): reset to this after a drain that found
    /// work, so a busy queue is drained promptly.
    #[arg(long, default_value_t = wamn_run_queue::DEFAULT_MIN_INTERVAL_MS as u64)]
    pub min_idle_ms: u64,

    /// Widest idle poll interval (ms): the reconciliation backstop cadence while
    /// the queue stays empty (doubles up to here).
    #[arg(long, default_value_t = wamn_run_queue::DEFAULT_MAX_INTERVAL_MS as u64)]
    pub max_idle_ms: u64,

    /// NATS URL for doorbell wakes. The runner runs without NATS (the
    /// poll-backoff reconcile still guarantees pickup), just with higher wake
    /// latency.
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,

    /// mTLS material for the doorbell NATS connection (mount the
    /// wasmcloud-runtime-tls secret in-cluster). Omit for plain NATS.
    #[arg(long)]
    pub nats_tls_ca: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_cert: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_key: Option<PathBuf>,

    /// Test-host double-set selector (wamn-t92): `off` (default) = production
    /// host; `virtual` = test host (virtual clock + seeded random + egress
    /// recorder). NOT a production configuration — for a throwaway test runner.
    #[arg(long, env = "WAMN_TEST_DOUBLES", default_value_t = TestDoubles::Off, value_enum)]
    pub test_doubles: TestDoubles,

    /// Under `--test-doubles virtual`, an authority this runner's flows may reach
    /// (repeatable / comma-separated). The `EgressRecorder` records and DENIES
    /// any outbound authority not listed. Ignored when `--test-doubles off`.
    #[arg(
        long = "test-egress-expect",
        env = "WAMN_TEST_EGRESS_EXPECT",
        value_delimiter = ','
    )]
    pub test_egress_expect: Vec<String>,
}

/// The `run-next` export's typed signature: `(lease-ttl-ms) -> (claimed, run-id,
/// outcome)`.
type RunNextFunc = TypedFunc<(u64,), (Result<(bool, Option<String>, u32), String>,)>;

/// What one drain of the queue did — the gate's assertion surface. `claimed` is
/// the total runs this drain pulled; each ends `completed` (0), `parked` (1, a
/// `delay` re-offered at its wake), or `failed` (2).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DrainReport {
    pub claimed: usize,
    pub completed: usize,
    pub parked: usize,
    pub failed: usize,
}

impl DrainReport {
    pub fn found_work(&self) -> bool {
        self.claimed > 0
    }
}

/// The runner's outbound-`wasi:http` egress handler: enforce the host
/// allowlist (the fork's [`check_allowed_hosts`] — EMPTY = DENY-ALL, the
/// production fail-closed posture) AND the current flow's declared
/// `allowed-hosts` (fqg.11 — the trusted `wamn:runner/egress` declaration,
/// see [`RunnerEgressPolicy`]; never-declared or declared-empty = deny-all,
/// both checks must pass = intersection), then delegate transport to
/// [`DefaultOutgoingHandler`] (which also stamps the 9.2 trace context).
/// Without a handler on the store's `Ctx`, an outbound call TRAPS ("http
/// client not available") and poisons the instance — so the runner wires this
/// unconditionally; a denial is a clean `HttpRequestDenied` the node
/// classifies as `egress-denied` (terminal).
struct RunnerEgress {
    inner: DefaultOutgoingHandler,
    /// The per-component declared flow allowlists, written through the trusted
    /// `wamn:runner/egress` channel by the flowrunner before each run.
    policy: Arc<RunnerEgressPolicy>,
}

#[async_trait::async_trait]
impl HostHandler for RunnerEgress {
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
        allowed_hosts: &[AllowedHost],
    ) -> HttpResult<HostFutureIncomingResponse> {
        if let Err(e) = check_allowed_hosts(&request, allowed_hosts) {
            tracing::warn!(
                workload_id,
                error = %e,
                "run-worker outbound request denied by the allowed-hosts policy"
            );
            // A DENIAL, never a trap: the guest sees HttpRequestDenied and the
            // node classifies it egress-denied (terminal); the instance lives.
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestDenied,
            ))));
        }
        // fqg.11: the flow-level check. Undeclared and declared-empty are the
        // same deny-all `&[]` (egress is opt-in per flow); a declared set must
        // ALSO pass — the host list above stays the outer bound.
        let declared = self.policy.declared(workload_id);
        if let Err(e) = check_allowed_hosts(&request, declared.as_deref().unwrap_or(&[])) {
            tracing::warn!(
                workload_id,
                error = %e,
                declared = declared.is_some(),
                "run-worker outbound request denied by the flow's allowed-hosts"
            );
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestDenied,
            ))));
        }
        self.inner.send_request(workload_id, request, config)
    }
}

/// The host-injected, non-spoofable identity one runner replica carries: the
/// lease owner (== the component id), the tenant claim, the session
/// search_path, and — 5.9 — the project whose vault credentials its flows may
/// read. The guest reads these from its session; it never chooses them.
#[derive(Debug, Clone, Copy)]
pub struct RunnerIdentity<'a> {
    pub owner: &'a str,
    pub tenant: &'a str,
    pub schema: Option<&'a str>,
    pub project: &'a str,
}

/// Register this replica's HOST-INJECTED wasi:logging claim: the run-path log
/// records enrich with the runner's own `(tenant, project)` — the same
/// non-spoofable identity the postgres session carries — keyed by the component
/// id (== the lease `owner`, the id the store's `Ctx` is built with). A guest
/// can NOT set its tenant; it only supplies flow/run/node in the log context.
/// Factored out so the run-worker wiring test can assert the identity landed
/// without a full instantiate.
fn register_logging_claim(logging: &WamnLogging, identity: &RunnerIdentity<'_>) {
    logging.set_claim(identity.owner, identity.tenant, identity.project);
}

/// The production flow runner: a single long-lived flowrunner instance whose
/// plugin session carries the host-injected lease owner + tenant + schema.
/// [`drain`] pulls every currently-claimable run to a terminal (or parked)
/// state; [`serve`] wraps that in the doorbell + backoff + shutdown loop.
pub struct RunWorker {
    store: Store<SharedCtx>,
    run_next: RunNextFunc,
    ttl_ms: u64,
    /// The doorbell subject this runner listens on (`wamn.doorbell.<tenant>`).
    subject: String,
}

impl RunWorker {
    /// Instantiate the flowrunner component and inject this replica's identity.
    /// `identity.owner` is BOTH the component id and the `app.runner` lease
    /// owner (one process = one project = one owner, the single-project shape).
    /// Mirrors the failoverbench claimer store-build (SR1: the gate drives the
    /// same code).
    #[expect(
        clippy::too_many_arguments,
        reason = "the engine + guest + the three session plugins (postgres/credentials/logging) + identity + egress + ttl + the wamn-t92 test-double selector are each a distinct host-injected input; grouping them into a struct would only move the list"
    )]
    pub async fn instantiate(
        engine: &Engine,
        guest: &[u8],
        plugin: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        identity: RunnerIdentity<'_>,
        allowed_hosts: Arc<[AllowedHost]>,
        ttl_ms: u64,
        doubles: Option<DoubleSet>,
    ) -> anyhow::Result<Self> {
        let RunnerIdentity {
            owner,
            tenant,
            schema,
            project,
        } = identity;
        // Non-spoofable, host-injected: the guest reads these from its session,
        // never chooses them. set_runner validates the owner charset.
        plugin.set_tenant(owner, tenant)?;
        if let Some(s) = schema {
            plugin.set_schema(owner, s)?;
        }
        plugin.set_runner(owner, owner)?;
        // 5.9: the vault resolves per (project, name); the project is a
        // host-injected claim like the tenant/schema/runner above.
        vault.set_project(owner, project)?;
        // wamn-yf3: the wasi:logging tenant/project claim is host-injected too —
        // the guest's run-path log records enrich with THIS replica's identity,
        // never a guest-chosen one (the same trust boundary as the tenant above).
        register_logging_claim(&logging, &identity);

        let raw = engine.inner();
        let component = WasmtimeComponent::new(raw, guest)
            .map_err(|e| anyhow::anyhow!("compile flowrunner: {e}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        wamn_postgres::add_to_linker(&mut linker)?;
        // The flowrunner imports wamn:node/credentials unconditionally; the
        // linker must satisfy it even when the vault is empty.
        wamn_credentials::add_to_linker(&mut linker)?;
        // cjv.3: the TRUSTED per-run grant channel — the compiled-in flowrunner
        // declares each run's grant (the flow's declared credentials) so the
        // host can enforce the frozen `not-granted` grant. A custom node
        // (wamn-bd5) is NOT instantiated here and never gets this channel.
        wamn_credentials::add_runner_to_linker(&mut linker)?;
        // fqg.11: the TRUSTED per-run egress channel — same trust argument.
        runner_egress::add_runner_to_linker(&mut linker)?;
        // l5i9.12.2: the TRUSTED per-run causation channel — same trust
        // argument. The runner declares the run it drives so the wamn:postgres
        // plugin stamps a transactional wamn.causation message onto every
        // run-owned txn (the CDC reader stitches it).
        wamn_postgres::add_runner_causation_to_linker(&mut linker)?;
        // wamn-yf3: wasi:logging — the flowrunner emits a few structured records
        // per run (node/run lifecycle) that the wamn:logging plugin enriches +
        // ships. The guest imports it unconditionally, so the linker must satisfy
        // it (as with credentials); with OTEL unset the plugin's provider is a
        // no-op, so this links safely with no collector.
        wamn_logging::add_to_linker(&mut linker)?;
        let pre = linker.instantiate_pre(&component)?;

        let egress_policy = Arc::new(RunnerEgressPolicy::default());
        let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
        plugins.insert(
            wamn_postgres::WAMN_POSTGRES_ID,
            plugin as Arc<dyn HostPlugin + Send + Sync>,
        );
        plugins.insert(
            WAMN_CREDENTIALS_ID,
            vault as Arc<dyn HostPlugin + Send + Sync>,
        );
        plugins.insert(
            RUNNER_EGRESS_ID,
            egress_policy.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        plugins.insert(
            WAMN_LOGGING_ID,
            logging as Arc<dyn HostPlugin + Send + Sync>,
        );
        // The egress handler is unconditional (an outbound call without one
        // TRAPS); the allowlists gate it — the host-level list here plus the
        // per-flow declaration (fqg.11), both empty-deny-all, fail-closed.
        let builder = Ctx::builder(owner.to_string(), owner.to_string()).with_plugins(plugins);
        // wamn-t92: `Some(doubles)` selects the TEST HOST — the virtual-clock +
        // seeded-random `WasiCtx` and the `EgressRecorder` swapped in for the
        // prod egress handler. This is the ONLY production seam that can inject a
        // per-workload `WasiCtx` (the washlet host cannot); see `wamn_host::doubles`.
        // `None` is the production host, byte-unchanged.
        let ctx = match doubles {
            Some(ds) => builder
                .with_http_handler(ds.egress)
                .with_wasi_ctx(ds.wasi)
                .build(),
            None => builder
                .with_http_handler(Arc::new(RunnerEgress {
                    inner: DefaultOutgoingHandler,
                    policy: egress_policy,
                }))
                .with_allowed_hosts(allowed_hosts)
                .build(),
        };
        let mut store = Store::new(raw, SharedCtx::new(ctx));
        // No kill semantics: a huge deadline so the epoch (which the ticker
        // still advances) never traps a legitimately long run.
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = pre.instantiate_async(&mut store).await?;
        let run_next = instance.get_typed_func(&mut store, "run-next")?;

        Ok(Self {
            store,
            run_next,
            ttl_ms,
            subject: format!("wamn.doorbell.{tenant}"),
        })
    }

    /// One turn of the guest's dispatch loop: claim + drive + dequeue/park the
    /// next queued run. Returns (claimed, run_id, outcome).
    async fn call_run_next(&mut self) -> anyhow::Result<(bool, Option<String>, u32)> {
        let (r,) = self
            .run_next
            .call_async(&mut self.store, (self.ttl_ms,))
            .await?;
        r.map_err(|e| anyhow::anyhow!("run-next: {e}"))
    }

    /// Drain every currently-claimable run. Each `run-next` claims one run and
    /// drives it terminal (dequeued) or parks it (its `available_at` pushed past
    /// now, so it is no longer claimable this drain), so the claimable set
    /// strictly shrinks and the loop terminates; a parked run is picked up on a
    /// later wake. Returns the tally.
    pub async fn drain(&mut self) -> anyhow::Result<DrainReport> {
        let mut report = DrainReport::default();
        loop {
            let (claimed, run_id, outcome) = self.call_run_next().await?;
            if !claimed {
                break;
            }
            report.claimed += 1;
            match outcome {
                0 => report.completed += 1,
                1 => report.parked += 1,
                _ => report.failed += 1,
            }
            tracing::info!(
                run_id = run_id.as_deref().unwrap_or("?"),
                outcome,
                "run-worker: drove a claimed run"
            );
        }
        Ok(report)
    }

    /// The always-on serve loop: drain, then wait for a doorbell hint, the idle
    /// timeout, or shutdown — backing off toward `max_idle_ms` while the queue
    /// stays empty and resetting to `min_idle_ms` on work or a hint. A drain
    /// error is non-fatal (logged + backed off): the pool re-dials on the next
    /// call, and an in-flight run's lease ages out for another replica (fqg.2).
    pub async fn serve(
        &mut self,
        nats: Option<async_nats::Client>,
        cadence: wamn_run_queue::Cadence,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        use futures_util::StreamExt;

        let min = cadence.min();
        let mut sub = match &nats {
            Some(c) => Some(c.subscribe(self.subject.clone()).await?),
            None => None,
        };
        let mut idle = min;
        loop {
            let found_work = match self.drain().await {
                Ok(r) => {
                    if r.claimed > 0 {
                        tracing::info!(
                            claimed = r.claimed,
                            completed = r.completed,
                            parked = r.parked,
                            failed = r.failed,
                            "run-worker: drained"
                        );
                    }
                    r.found_work()
                }
                Err(e) => {
                    tracing::warn!(error = %e, "run-worker: drain failed (retrying after backoff)");
                    false
                }
            };
            idle = cadence.next_interval(idle, found_work);

            tokio::select! {
                hint = async {
                    match sub.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if hint.is_none() {
                        // The subscription closed; drop it (the poll-backoff
                        // reconcile still guarantees pickup).
                        sub = None;
                        tracing::warn!("run-worker: doorbell subscription closed; poll-backoff only");
                    } else {
                        // A hint means work is likely — drain now at min cadence.
                        idle = min;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(idle as u64)) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Resolve the lease owner: `--runner` / $WAMN_RUNNER, then $HOSTNAME (the pod
/// name in Kubernetes), then a fixed fallback. Every replica must be distinct;
/// the fallback is only for a bare local run.
fn resolve_owner(arg: Option<String>) -> String {
    arg.filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "wamn-runner".to_string())
}

pub async fn run(args: RunWorkerArgs) -> anyhow::Result<()> {
    use wash_runtime::washlet::{NatsConnectionOptions, connect_nats};

    wash_runtime::init_crypto();

    // R13: validate the idle poll cadence once, at startup — an inverted band
    // (`--min-idle-ms` > `--max-idle-ms`) would otherwise panic in
    // `next_interval`'s `clamp` on the first idle sweep. Bail here instead.
    let cadence = wamn_run_queue::Cadence::new(args.min_idle_ms as i64, args.max_idle_ms as i64)
        .context("invalid idle poll cadence (--min-idle-ms / --max-idle-ms)")?;

    let url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let owner = resolve_owner(args.runner.clone());

    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner component {}", args.flowrunner.display()))?;

    // The plugin owns the per-project pool (single URL = the default project)
    // and the component→claim maps the runner identity is injected through.
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(url);
    let plugin = Arc::new(WamnPostgres::new(cfg)?);

    // 5.9: the credential vault, sourced from the mounted file when present
    // (a missing file = an empty vault, warned inside from_file).
    let vault = Arc::new(match &args.credentials_file {
        Some(path) => WamnCredentials::from_file(path)?,
        None => WamnCredentials::empty(),
    });

    // wamn-yf3: THE guest-log pipeline (its own OTLP LoggerProvider). Config from
    // WAMN_LOG_* env (from_env); with OTEL unset it links + drops to a no-op
    // provider, so a collector-less runner still instantiates cleanly.
    let logging = Arc::new(WamnLogging::from_env().context("wamn:logging plugin init")?);

    // The outbound egress allowlist (empty = deny-all, fail-closed).
    let allowed_hosts: Arc<[AllowedHost]> = args
        .allowed_hosts
        .iter()
        .map(|s| s.parse::<AllowedHost>())
        .collect::<Result<Vec<_>, _>>()
        .context("parse --allowed-hosts")?
        .into();

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    // wamn-t92: assemble the test-host double set when selected. A live serve
    // loop has no scheduler to advance the virtual clock, so a delayed run would
    // park indefinitely — this path is for a throwaway/manual test runner; the
    // gate drives `instantiate` + `drain` directly. WARN so an accidental prod
    // enablement is visible.
    let doubles = match args.test_doubles {
        TestDoubles::Off => None,
        TestDoubles::Virtual => {
            let recorder = Arc::new(EgressRecorder::spying());
            recorder.expect(&owner, args.test_egress_expect.iter().cloned());
            let (ds, _clock) = DoubleSet::virtual_host(
                TEST_DOUBLES_EPOCH_SECS,
                TEST_DOUBLES_SEED,
                recorder as Arc<dyn HostHandler>,
            );
            tracing::warn!(
                "run-worker: --test-doubles=virtual — TEST HOST (virtual clock + seeded \
                 random + egress recorder), NOT a production configuration"
            );
            Some(ds)
        }
    };

    let mut worker = RunWorker::instantiate(
        &engine,
        &guest,
        plugin,
        vault,
        logging,
        RunnerIdentity {
            owner: &owner,
            tenant: &args.tenant,
            schema: args.schema.as_deref(),
            project: &args.project,
        },
        allowed_hosts,
        args.lease_ttl_ms,
        doubles,
    )
    .await?;

    // NATS is best-effort: no doorbell just raises wake latency (the poll-backoff
    // reconcile still guarantees pickup) — the dispatcher's exact posture.
    let nats_opts = NatsConnectionOptions {
        request_timeout: None,
        tls_ca: args.nats_tls_ca.clone(),
        tls_first: false,
        tls_cert: args.nats_tls_cert.clone(),
        tls_key: args.nats_tls_key.clone(),
    };
    let nats = match connect_nats(args.nats_url.clone(), nats_opts).await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(url = %args.nats_url, error = %e,
                "run-worker: no NATS — doorbell wakes disabled, poll-backoff still guarantees pickup");
            None
        }
    };

    // SIGTERM handled explicitly: in-container the runner is PID 1, which gets no
    // default signal disposition, so an unhandled SIGTERM would be ignored and a
    // rollout would wait out the full grace period before SIGKILL. (Abrupt death
    // is safe — the lease ages out and another replica reclaims — but a clean
    // exit makes rollouts fast.)
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "run-worker: no SIGTERM handler; Ctrl-C only");
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = tx.send(true);
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
        let _ = tx.send(true);
    });

    tracing::info!(
        runner = %owner,
        tenant = %args.tenant,
        schema = args.schema.as_deref().unwrap_or("<default>"),
        lease_ttl_ms = args.lease_ttl_ms,
        "run-worker up (single-project claim loop; doorbell + poll-backoff)"
    );

    let result = worker.serve(nats, cadence, rx).await;
    ticker.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wamn-yf3: the run-path wasi:logging claim is HOST-INJECTED — the
    /// registration keys the runner's own `(tenant, project)` under the component
    /// id (== the lease owner). A mutant that swaps in a guest-supplied value,
    /// drops a field, or swaps tenant/project fails this readback.
    #[tokio::test]
    async fn register_logging_claim_uses_host_injected_identity() {
        let logging =
            WamnLogging::new(wamn_host::plugins::wamn_logging::WamnLoggingConfig::default())
                .expect("logging plugin");
        let identity = RunnerIdentity {
            owner: "runner-replica-7",
            tenant: "acme",
            schema: Some("wamn_run"),
            project: "receiving",
        };
        register_logging_claim(&logging, &identity);
        // Keyed by the component id (== owner); the enrichment is the runner's.
        assert_eq!(
            logging.claim_snapshot("runner-replica-7"),
            Some(("acme".to_string(), "receiving".to_string()))
        );
        // Nothing registered under any other id (no accidental broad claim).
        assert_eq!(logging.claim_snapshot("some-other-id"), None);
    }

    #[test]
    fn owner_falls_back_from_arg_to_hostname_to_fixed() {
        // An explicit non-empty arg always wins.
        assert_eq!(resolve_owner(Some("replica-7".into())), "replica-7");
        // An empty arg is ignored (falls through to HOSTNAME/fallback).
        let via_env = resolve_owner(Some(String::new()));
        assert!(!via_env.is_empty());
    }

    #[test]
    fn drain_report_tallies_by_outcome() {
        let mut r = DrainReport::default();
        assert!(!r.found_work());
        // completed / parked / failed land in distinct buckets; claimed is the sum.
        for outcome in [0u32, 0, 1, 2] {
            r.claimed += 1;
            match outcome {
                0 => r.completed += 1,
                1 => r.parked += 1,
                _ => r.failed += 1,
            }
        }
        assert_eq!(
            r,
            DrainReport {
                claimed: 4,
                completed: 2,
                parked: 1,
                failed: 1
            }
        );
        assert!(r.found_work());
    }

    #[test]
    fn idle_backoff_resets_on_work_and_doubles_while_idle() {
        // The runner reuses the dispatcher cadence: work resets to min, idleness
        // doubles toward max.
        let cadence = wamn_run_queue::Cadence::new(250, 30_000).unwrap();
        let (min, max) = (cadence.min(), cadence.max());
        assert_eq!(cadence.next_interval(min, true), min);
        let a = cadence.next_interval(min, false);
        let b = cadence.next_interval(a, false);
        assert!(a > min && b > a && b <= max);
        assert_eq!(cadence.next_interval(a, true), min);
    }
}
