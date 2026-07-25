//! Shared native host for the compiled flowrunner component.
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
//! scheduler's [`wamn_scheduler::Cadence::next_interval`] cadence) guarantees pickup
//! even when a hint is lost or NATS is absent. SIGTERM is handled explicitly
//! (PID 1 in-container gets no default disposition), so a rollout exits in
//! milliseconds instead of waiting out the grace period; abrupt death is safe
//! anyway — an in-flight run's lease simply ages out and another replica
//! reclaims it (fqg.2).
//!
//! The loop core ([`ExecutionHost`]) is shared by serving and scenario
//! compositions. Artifact-specific CLI, credentials, and capability selection
//! remain in their service leaves.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram};
use tokio::sync::watch;
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx, WamnStoreLimiter};
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

use wamn_runtime::memory_metrics::{self, MemoryMeter};
use wamn_runtime::plugins::runner_egress::{self, RUNNER_EGRESS_ID, RunnerEgressPolicy};
use wamn_runtime::plugins::wamn_credentials::{self, WAMN_CREDENTIALS_ID, WamnCredentials};
use wamn_runtime::plugins::wamn_logging::{self, WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::{self, WamnPostgres};

/// Stable in-image location of the compiled flowrunner component.
pub const DEFAULT_FLOWRUNNER_PATH: &str = "/components/flowrunner.wasm";

/// Capability composition for a single execution store.
pub struct ExecutionCapabilities {
    mode: CapabilityMode,
    egress_policy: Arc<RunnerEgressPolicy>,
}

impl std::fmt::Debug for ExecutionCapabilities {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = match &self.mode {
            CapabilityMode::Production { .. } => "production",
            CapabilityMode::Injected { .. } => "injected",
        };
        formatter
            .debug_struct("ExecutionCapabilities")
            .field("mode", &mode)
            .finish()
    }
}

enum CapabilityMode {
    Production {
        allowed_hosts: Arc<[AllowedHost]>,
    },
    Injected {
        wasi: Box<wasmtime_wasi::WasiCtx>,
        http: Arc<dyn HostHandler>,
        allowed_hosts: Arc<[AllowedHost]>,
    },
}

/// Compose production HTTP and WASI capabilities.
pub fn production_capabilities(allowed_hosts: Arc<[AllowedHost]>) -> ExecutionCapabilities {
    ExecutionCapabilities {
        mode: CapabilityMode::Production { allowed_hosts },
        egress_policy: Arc::new(RunnerEgressPolicy::default()),
    }
}

/// Compose externally provided capabilities for a non-serving execution artifact.
///
/// `allowed_hosts` is the trusted host-level outer bound. `egress_policy` is
/// shared with the trusted flowrunner declaration channel so the injected HTTP
/// handler can enforce the same intersection as production. The scenario-worker
/// is the product owner of the deterministic adapters passed through this seam.
pub fn injected_capabilities(
    wasi: wasmtime_wasi::WasiCtx,
    http: Arc<dyn HostHandler>,
    allowed_hosts: Arc<[AllowedHost]>,
    egress_policy: Arc<RunnerEgressPolicy>,
) -> ExecutionCapabilities {
    ExecutionCapabilities {
        mode: CapabilityMode::Injected {
            wasi: Box::new(wasi),
            http,
            allowed_hosts,
        },
        egress_policy,
    }
}

/// The `run-next` export's typed signature: `(lease-ttl-ms) -> (claimed, run-id,
/// outcome)`.
type RunNextFunc = TypedFunc<(u64,), (Result<(bool, Option<String>, u32), String>,)>;
/// The side-effect-free `check-flow` export's typed signature.
type CheckFlowFunc = TypedFunc<(String,), (Result<Vec<String>, String>,)>;

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
pub struct ExecutionIdentity<'a> {
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
fn register_logging_claim(logging: &WamnLogging, identity: &ExecutionIdentity<'_>) {
    logging.set_claim(identity.owner, identity.tenant, identity.project);
}

/// [9.8] Map a guest drive outcome code to its `outcome` metric attribute:
/// 0 completed, 1 parked, anything else failed — the SAME fold [`DrainReport`]
/// uses for its tally. A mutant that folds `failed` into the completed bucket (or
/// drops the attribute) is caught by metricbench phase 1's forced-failure check.
fn outcome_label(outcome: u32) -> &'static str {
    match outcome {
        0 => "completed",
        1 => "parked",
        _ => "failed",
    }
}

/// [9.8] The run-worker's OTel instruments plus this replica's `(tenant, project)`
/// base attributes: `wamn.run.executions` (by `outcome`) and the per-drive
/// `wamn.run.drive.duration_ms` histogram. On the global meter the fork's
/// observability init installs (the S5/9.1 provider — never a second one); inert
/// until `OTEL_*` selects a real provider. NO `run_id` attribute (unbounded).
struct RunMetrics {
    executions: Counter<u64>,
    drive_ms: Histogram<f64>,
    tenant: String,
    project: String,
}

impl RunMetrics {
    fn register(tenant: &str, project: &str) -> Self {
        let meter = opentelemetry::global::meter("wamn-run-worker");
        Self {
            executions: meter
                .u64_counter("wamn.run.executions")
                .with_description(
                    "flow-run drives by terminal outcome (completed / parked / failed)",
                )
                .build(),
            drive_ms: meter
                .f64_histogram("wamn.run.drive.duration_ms")
                .with_description(
                    "wall time to drive one claimed run through run-next, in ms \
                     (whole-run drive; true per-node duration is guest-side — deferred)",
                )
                .build(),
            tenant: tenant.to_string(),
            project: project.to_string(),
        }
    }

    /// Record one claimed drive: the duration histogram (tenant/project) and the
    /// executions counter (tenant/project + the terminal `outcome`).
    fn record_drive(&self, elapsed: Duration, outcome: u32) {
        let base = [
            KeyValue::new("wamn.tenant", self.tenant.clone()),
            KeyValue::new("wamn.project", self.project.clone()),
        ];
        self.drive_ms.record(elapsed.as_secs_f64() * 1000.0, &base);
        self.executions.add(
            1,
            &[
                KeyValue::new("wamn.tenant", self.tenant.clone()),
                KeyValue::new("wamn.project", self.project.clone()),
                KeyValue::new("outcome", outcome_label(outcome)),
            ],
        );
    }
}

/// [9.8] Attach the D16 per-store memory limiter to the flowrunner store when a
/// budget is configured (`WAMN_MEMORY_LIMIT_MB`), so its high-water + any denials
/// feed the `wamn.memory.*` gauges (mirrors the fork's `new_store_from_templates`
/// resolution, env-only here). Unbudgeted (the default) attaches NOTHING —
/// byte-identical to before, and the long-lived flowrunner never risks a grow
/// trap. Returns the process memory meter to snapshot into, or `None`.
fn attach_memory_limiter(store: &mut Store<SharedCtx>, component_id: &str) -> Option<MemoryMeter> {
    let budget_mb: u64 = std::env::var("WAMN_MEMORY_LIMIT_MB")
        .ok()
        .and_then(|v| v.parse().ok())?;
    store.data_mut().wamn_limiter =
        WamnStoreLimiter::new((budget_mb as usize) << 20, Arc::from(component_id));
    store.limiter(|ctx| &mut ctx.wamn_limiter);
    Some(memory_metrics::global_memory_meter())
}

/// The production flow runner: a single long-lived flowrunner instance whose
/// plugin session carries the host-injected lease owner + tenant + schema.
/// [`ExecutionHost::drain`] pulls every currently-claimable run to a terminal
/// (or parked) state; [`ExecutionHost::serve`] wraps that in the doorbell +
/// backoff + shutdown loop.
pub struct ExecutionHost {
    store: Store<SharedCtx>,
    check_flow: CheckFlowFunc,
    run_next: RunNextFunc,
    ttl_ms: u64,
    /// The doorbell subject this runner listens on (`wamn.doorbell.<tenant>`).
    subject: String,
    /// [9.8] run/drive instruments + this replica's tenant/project attributes.
    metrics: RunMetrics,
    /// [9.8] `Some` when a memory limiter is attached (a budget was configured);
    /// each drive then publishes the store's high-water into the meter.
    mem: Option<MemoryMeter>,
}

impl std::fmt::Debug for ExecutionHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionHost")
            .field("ttl_ms", &self.ttl_ms)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

impl ExecutionHost {
    /// Instantiate the flowrunner component and inject this replica's identity.
    /// `identity.owner` is BOTH the component id and the `app.runner` lease
    /// owner (one process = one project = one owner, the single-project shape).
    /// Mirrors the failoverbench claimer store-build (SR1: the gate drives the
    /// same code).
    #[expect(
        clippy::too_many_arguments,
        reason = "the engine, guest, session plugins, identity, capabilities, and lease TTL are distinct host-injected inputs"
    )]
    pub async fn instantiate(
        engine: &Engine,
        guest: &[u8],
        plugin: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        identity: ExecutionIdentity<'_>,
        capabilities: ExecutionCapabilities,
        ttl_ms: u64,
    ) -> anyhow::Result<Self> {
        let ExecutionIdentity {
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

        let ExecutionCapabilities {
            mode,
            egress_policy,
        } = capabilities;
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
        let builder = Ctx::builder(owner.to_string(), owner.to_string()).with_plugins(plugins);
        let ctx = match mode {
            CapabilityMode::Injected {
                wasi,
                http,
                allowed_hosts,
            } => builder
                .with_http_handler(http)
                .with_allowed_hosts(allowed_hosts)
                .with_wasi_ctx(*wasi)
                .build(),
            CapabilityMode::Production { allowed_hosts } => builder
                .with_http_handler(Arc::new(RunnerEgress {
                    inner: DefaultOutgoingHandler,
                    policy: egress_policy,
                }))
                .with_allowed_hosts(allowed_hosts)
                .build(),
        };
        let mut store = Store::new(raw, SharedCtx::new(ctx));
        // [9.8] Attach the D16 memory limiter when a budget is configured (before
        // instantiation, so baseline-memory creation is counted) — unbudgeted =
        // no limiter, unchanged behavior.
        let mem = attach_memory_limiter(&mut store, owner);
        // No kill semantics: a huge deadline so the epoch (which the ticker
        // still advances) never traps a legitimately long run.
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = pre.instantiate_async(&mut store).await?;
        let check_flow = instance.get_typed_func(&mut store, "check-flow")?;
        let run_next = instance.get_typed_func(&mut store, "run-next")?;

        Ok(Self {
            store,
            check_flow,
            run_next,
            ttl_ms,
            subject: format!("wamn.doorbell.{tenant}"),
            metrics: RunMetrics::register(tenant, project),
            mem,
        })
    }

    /// Return the sorted, unique node types this compiled runner cannot dispatch.
    pub async fn check_flow(&mut self, flow_json: &str) -> anyhow::Result<Vec<String>> {
        let (result,) = self
            .check_flow
            .call_async(&mut self.store, (flow_json.to_owned(),))
            .await?;
        result.map_err(|error| anyhow::anyhow!("check-flow: {error}"))
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
            // [9.8] time the whole run-drive; record only for a CLAIMED run (an
            // empty claim is the idle poll, not a drive).
            let t0 = std::time::Instant::now();
            let (claimed, run_id, outcome) = self.call_run_next().await?;
            if !claimed {
                break;
            }
            let elapsed = t0.elapsed();
            report.claimed += 1;
            match outcome {
                0 => report.completed += 1,
                1 => report.parked += 1,
                _ => report.failed += 1,
            }
            // [9.8] the drive's duration + outcome, then the flowrunner store's
            // memory high-water when a limiter is attached.
            self.metrics.record_drive(elapsed, outcome);
            if let Some(mem) = &self.mem {
                mem.snapshot_from(&self.store.data().wamn_limiter);
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
        cadence: wamn_scheduler::Cadence,
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
            WamnLogging::new(wamn_runtime::plugins::wamn_logging::WamnLoggingConfig::default())
                .expect("logging plugin");
        let identity = ExecutionIdentity {
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

    // [9.8] the executions counter's `outcome` attribute maps 0/1/other to
    // completed/parked/failed — the SAME fold DrainReport uses. A mutant folding
    // `failed` into `completed` (or dropping the attribute) diverges here and at
    // metricbench phase 1.
    #[test]
    fn outcome_label_maps_codes_to_buckets() {
        assert_eq!(outcome_label(0), "completed");
        assert_eq!(outcome_label(1), "parked");
        assert_eq!(outcome_label(2), "failed");
        // Any non-0/1 code is a failure (defensive — the guest only emits 0/1/2).
        assert_eq!(outcome_label(99), "failed");
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
    fn idle_backoff_resets_on_work_and_expands_while_idle() {
        // The runner reuses the dispatcher cadence: work resets to min, idleness
        // expands toward max.
        let cadence = wamn_scheduler::Cadence::new(250, 30_000).unwrap();
        let (min, max) = (cadence.min(), cadence.max());
        assert_eq!(cadence.next_interval(min, true), min);
        let a = cadence.next_interval(min, false);
        let b = cadence.next_interval(a, false);
        assert!(a > min && b > a && b <= max);
        assert_eq!(cadence.next_interval(a, true), min);
    }
}
