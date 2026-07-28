//! Shared WASI/Wasm adapter for the compiled flowrunner component.
//!
//! [`ExecutionHost`] instantiates one component with host-injected identity and
//! capabilities, exposes `check-flow`, and drives the guest's `run-next` export.
//! Artifact lifecycle policy such as polling, doorbell subscription, shutdown,
//! and production capability selection remains in the service leaves.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

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

use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, MAX_HOST_CALL_DURATION};
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

/// Compose production HTTP and WASI capabilities from service-selected policy.
pub fn production_capabilities(
    allowed_hosts: Arc<[AllowedHost]>,
    egress_policy: Arc<RunnerEgressPolicy>,
) -> ExecutionCapabilities {
    ExecutionCapabilities {
        mode: CapabilityMode::Production { allowed_hosts },
        egress_policy,
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
/// The exact claimed-run export's typed signature.
type ExecuteClaimedFunc = TypedFunc<(String, String, i64, u64), (Result<u32, String>,)>;
/// The side-effect-free `check-flow` export's typed signature.
type CheckFlowFunc = TypedFunc<(String,), (Result<Vec<String>, String>,)>;

fn bounded_attempt_ms(value: u64) -> u64 {
    value.clamp(1, MAX_HOST_CALL_DURATION.as_millis() as u64)
}

fn deadline_ticks(attempt_ms: u64) -> u64 {
    let tick_ms = DEFAULT_EPOCH_TICK.as_millis() as u64;
    bounded_attempt_ms(attempt_ms).div_ceil(tick_ms)
}

/// One completed guest drive, borrowed for synchronous caller observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveObservation<'a> {
    /// The claimed run identifier, when the guest returned one.
    pub run_id: Option<&'a str>,
    /// Guest outcome code: `0` completed, `1` parked, otherwise failed.
    pub outcome: u32,
    /// Wall time spent in the guest's `run-next` call.
    pub elapsed: Duration,
}

/// What one drain of the queue did — the gate's assertion surface.
///
/// `claimed` is the total runs this drain pulled; each ends `completed` (0),
/// `parked` (1, a `delay` re-offered at its wake), or `failed` (2).
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

    fn record(&mut self, outcome: u32) {
        self.claimed += 1;
        match outcome {
            0 => self.completed += 1,
            1 => self.parked += 1,
            _ => self.failed += 1,
        }
    }
}

fn observe_drive<F>(report: &mut DrainReport, observation: DriveObservation<'_>, observe: &mut F)
where
    F: for<'a> FnMut(DriveObservation<'a>),
{
    report.record(observation.outcome);
    observe(observation);
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

fn bounded_outgoing_config(mut config: OutgoingRequestConfig) -> OutgoingRequestConfig {
    config.connect_timeout = config.connect_timeout.min(MAX_HOST_CALL_DURATION);
    config.first_byte_timeout = config.first_byte_timeout.min(MAX_HOST_CALL_DURATION);
    config.between_bytes_timeout = config.between_bytes_timeout.min(MAX_HOST_CALL_DURATION);
    config
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
        self.inner
            .send_request(workload_id, request, bounded_outgoing_config(config))
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

/// A flowrunner instance whose plugin session carries host-injected identity.
///
/// [`ExecutionHost::drain`] pulls every currently-claimable run to a terminal
/// (or parked) state. Service leaves decide when to invoke another drain.
struct LiveExecution {
    store: Store<SharedCtx>,
    check_flow: CheckFlowFunc,
    run_next: RunNextFunc,
    execute_claimed: ExecuteClaimedFunc,
}

pub struct ExecutionHost {
    live: Option<LiveExecution>,
    ttl_ms: u64,
    /// [9.8] `Some` when a memory limiter is attached (a budget was configured);
    /// each drive then publishes the store's high-water into the meter.
    mem: Option<MemoryMeter>,
}

impl std::fmt::Debug for ExecutionHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionHost")
            .field("ttl_ms", &self.ttl_ms)
            .field("disposed", &self.live.is_none())
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
        store.set_epoch_deadline(deadline_ticks(ttl_ms));
        let instance = pre.instantiate_async(&mut store).await?;
        let check_flow = instance.get_typed_func(&mut store, "check-flow")?;
        let run_next = instance.get_typed_func(&mut store, "run-next")?;
        let execute_claimed = instance.get_typed_func(&mut store, "execute-claimed")?;

        Ok(Self {
            live: Some(LiveExecution {
                store,
                check_flow,
                run_next,
                execute_claimed,
            }),
            ttl_ms: bounded_attempt_ms(ttl_ms),
            mem,
        })
    }

    /// Whether a Wasmtime interruption or trap disposed this instance.
    pub fn is_disposed(&self) -> bool {
        self.live.is_none()
    }

    /// Return the sorted, unique node types this compiled runner cannot dispatch.
    pub async fn check_flow(&mut self, flow_json: &str) -> anyhow::Result<Vec<String>> {
        let call = {
            let live = self
                .live
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("execution instance disposed"))?;
            live.store.set_epoch_deadline(deadline_ticks(self.ttl_ms));
            live.check_flow
                .call_async(&mut live.store, (flow_json.to_owned(),))
                .await
        };
        let (result,) = match call {
            Ok(result) => result,
            Err(error) => {
                self.live.take();
                return Err(anyhow::anyhow!(
                    "check-flow trapped; execution instance disposed: {error}"
                ));
            }
        };
        result.map_err(|error| anyhow::anyhow!("check-flow: {error}"))
    }

    /// One turn of the guest's dispatch loop: claim + drive + dequeue/park the
    /// next queued run. Returns (claimed, run_id, outcome).
    async fn call_run_next(&mut self) -> anyhow::Result<(bool, Option<String>, u32)> {
        let call = {
            let live = self
                .live
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("execution instance disposed"))?;
            live.store.set_epoch_deadline(deadline_ticks(self.ttl_ms));
            live.run_next
                .call_async(&mut live.store, (self.ttl_ms,))
                .await
        };
        let (r,) = match call {
            Ok(result) => result,
            Err(error) => {
                self.live.take();
                return Err(anyhow::anyhow!(
                    "run-next trapped; execution instance disposed: {error}"
                ));
            }
        };
        r.map_err(|e| anyhow::anyhow!("run-next: {e}"))
    }

    /// Drive exactly one run already claimed by HTTP admission.
    ///
    /// This invokes the versioned `execute-claimed` guest export directly; it
    /// never calls `run-next` and therefore cannot scan or claim generic queue
    /// work. A trap disposes the Wasmtime instance before the error is returned.
    pub async fn execute_claimed(
        &mut self,
        run_id: &str,
        lease_owner: &str,
        lease_generation: i64,
    ) -> anyhow::Result<u32> {
        let call = {
            let live = self
                .live
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("execution instance disposed"))?;
            live.store.set_epoch_deadline(deadline_ticks(self.ttl_ms));
            live.execute_claimed
                .call_async(
                    &mut live.store,
                    (
                        run_id.to_owned(),
                        lease_owner.to_owned(),
                        lease_generation,
                        self.ttl_ms,
                    ),
                )
                .await
        };
        let (result,) = match call {
            Ok(result) => result,
            Err(error) => {
                self.live.take();
                return Err(anyhow::anyhow!(
                    "execute-claimed trapped; execution instance disposed: {error}"
                ));
            }
        };
        result.map_err(|error| anyhow::anyhow!("execute-claimed: {error}"))
    }

    /// Drain every currently-claimable run. Each `run-next` claims one run and
    /// drives it terminal (dequeued) or parks it (its `available_at` pushed past
    /// now, so it is no longer claimable this drain), so the claimable set
    /// strictly shrinks and the loop terminates; a parked run is picked up on a
    /// later wake. Returns the tally.
    pub async fn drain(&mut self) -> anyhow::Result<DrainReport> {
        self.drain_observing(|_| {}).await
    }

    /// Drain every currently-claimable run and synchronously observe each drive.
    ///
    /// The callback runs after a claimed guest call and memory snapshot, before
    /// the next claim. Its observation borrows the run id only for that call,
    /// keeping the report O(1) regardless of queue depth. Successful drives are
    /// therefore observable even if a later guest call ends the drain in error.
    pub async fn drain_observing<F>(&mut self, mut observe: F) -> anyhow::Result<DrainReport>
    where
        F: for<'a> FnMut(DriveObservation<'a>),
    {
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
            // [9.8] Snapshot the flowrunner store's memory high-water when a
            // limiter is attached. This remains adapter state because it
            // requires direct access to the Wasmtime store and its limiter.
            if let Some(mem) = &self.mem {
                let live = self
                    .live
                    .as_ref()
                    .expect("a successful drive retains its execution instance");
                mem.snapshot_from(&live.store.data().wamn_limiter);
            }
            observe_drive(
                &mut report,
                DriveObservation {
                    run_id: run_id.as_deref(),
                    outcome,
                    elapsed,
                },
                &mut observe,
            );
        }
        Ok(report)
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

    #[test]
    fn attempt_and_outbound_deadlines_are_finite() {
        assert_eq!(bounded_attempt_ms(0), 1);
        assert_eq!(
            bounded_attempt_ms(u64::MAX),
            MAX_HOST_CALL_DURATION.as_millis() as u64
        );
        assert_eq!(deadline_ticks(0), 1);

        let bounded = bounded_outgoing_config(OutgoingRequestConfig {
            use_tls: true,
            connect_timeout: Duration::MAX,
            first_byte_timeout: Duration::MAX,
            between_bytes_timeout: Duration::MAX,
        });
        assert!(bounded.use_tls);
        assert_eq!(bounded.connect_timeout, MAX_HOST_CALL_DURATION);
        assert_eq!(bounded.first_byte_timeout, MAX_HOST_CALL_DURATION);
        assert_eq!(bounded.between_bytes_timeout, MAX_HOST_CALL_DURATION);
    }

    #[test]
    fn callback_observations_and_aggregate_tallies_agree() {
        let mut r = DrainReport::default();
        let mut observed = Vec::new();
        let mut observe = |observation: DriveObservation<'_>| {
            observed.push((
                observation.run_id.map(str::to_owned),
                observation.outcome,
                observation.elapsed,
            ));
        };
        assert!(!r.found_work());
        // completed / parked / failed land in distinct buckets; claimed is the sum.
        for outcome in [0u32, 0, 1, 2] {
            let run_id = format!("run-{outcome}");
            observe_drive(
                &mut r,
                DriveObservation {
                    run_id: Some(&run_id),
                    outcome,
                    elapsed: Duration::from_millis(outcome as u64 + 1),
                },
                &mut observe,
            );
        }
        assert_eq!(r.claimed, 4);
        assert_eq!(r.completed, 2);
        assert_eq!(r.parked, 1);
        assert_eq!(r.failed, 1);
        assert_eq!(observed.len(), r.claimed);
        assert_eq!(observed[2].0.as_deref(), Some("run-1"));
        assert_eq!(observed[2].1, 1);
        assert_eq!(observed[2].2, Duration::from_millis(2));
        assert!(r.found_work());
    }
}
