//! Shared WASI/Wasm adapter for the compiled flowrunner component.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! [`ExecutionHost`] instantiates one component with host-injected identity and
//! capabilities, claims host-side, and drives the guest's single-shot `run` export.
//! Artifact lifecycle policy such as polling, doorbell subscription, shutdown,
//! and production capability selection remains in the service leaves.

mod effect_writer;
mod pool;

pub use pool::{
    ExecutionInstancePool, ExecutionLease, ExecutionPoolKey, ExecutionPoolLimits,
    ExecutionPoolSnapshot, INVOCATIONS_PER_INSTANCE, IdentityBindFailed,
    InvalidExecutionPoolLimits, InvocationDisposition, PoolCapacityError, PoolCleanupError,
    RetirementReason, ReusableExecutionInstance,
};

use effect_writer::load_effect_writer;

include!(concat!(env!("OUT_DIR"), "/effect_provider_revision.rs"));

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx, WamnStoreLimiter};
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{
    DefaultOutgoingHandler, HostHandler, OutgoingHandler as _, check_allowed_hosts,
};
use wash_runtime::host::http_p3::{P3Body, P3RequestErrorFuture, P3SendFuture};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker, TypedFunc};
use wasmtime_wasi_http::p2::HttpResult;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p3::RequestOptions;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode as P3ErrorCode;

use tracing::Instrument as _;

use wamn_catalog::{ExecutionRuntimeRevision, HOST_EFFECT_CONTRACT_VERSION};
use wamn_run_state::{EffectWriterErrorKind, FailKind, ResetProjectionFence, RunStatus};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, MAX_HOST_CALL_DURATION, MEMORY_CAP_BYTES};
use wamn_runtime::memory_metrics::{self, MemoryMeter};
use wamn_runtime::plan_artifact::OciPlanSource;
use wamn_runtime::plugins::connection_http::{self, CONNECTION_HTTP_ID, ConnectionHttp};
use wamn_runtime::plugins::runner_egress::{self, RUNNER_EGRESS_ID, RunnerEgressPolicy};
use wamn_runtime::plugins::runner_plan_supply::{
    self, PlanRelease, RUNNER_PLAN_SUPPLY_ID, RunnerPlanSupply,
};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{self, WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::{
    self, DEFAULT_PROJECT, ProductionClaimResult, ProductionReapResult, SessionClaims, WamnPostgres,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;

/// Stable in-image location of the compiled flowrunner component.
pub const DEFAULT_FLOWRUNNER_PATH: &str = "/components/flowrunner.wasm";
/// Hot immutable plans retained per execution host; eviction is deterministic LRU.
pub const PLAN_CACHE_MAX_ENTRIES: usize = 256;

/// Host-derived identity of the exact executable runtime loaded for execution.
///
/// The values cannot be supplied independently: construction hashes the exact
/// flowrunner bytes and binds them to this build's native effect providers and
/// supported host-effect contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedExecutionRuntimeRevision {
    flowrunner_component_digest: Box<str>,
    effect_provider_revision: &'static str,
    host_effect_contract_version: &'static str,
}

impl TrustedExecutionRuntimeRevision {
    /// Derive the trusted runtime revision from exact loaded flowrunner bytes.
    pub fn from_flowrunner_bytes(flowrunner_bytes: &[u8]) -> Self {
        let digest = Sha256::digest(flowrunner_bytes);
        let mut flowrunner_component_digest = String::with_capacity(71);
        flowrunner_component_digest.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            write!(flowrunner_component_digest, "{byte:02x}")
                .expect("writing to String cannot fail");
        }

        Self {
            flowrunner_component_digest: flowrunner_component_digest.into_boxed_str(),
            effect_provider_revision: EFFECT_PROVIDER_REVISION,
            host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION,
        }
    }

    /// Return the digest of the exact flowrunner bytes loaded by the host.
    pub fn flowrunner_component_digest(&self) -> &str {
        self.flowrunner_component_digest.as_ref()
    }

    /// Return the immutable native effect-provider revision compiled into the host.
    pub fn effect_provider_revision(&self) -> &str {
        self.effect_provider_revision
    }

    /// Return the catalog host-effect contract supported by this host.
    pub fn host_effect_contract_version(&self) -> &str {
        self.host_effect_contract_version
    }

    /// Project the trusted host identity into the persisted catalog model.
    pub fn execution_runtime_revision(&self) -> ExecutionRuntimeRevision {
        ExecutionRuntimeRevision {
            flowrunner_component_digest: self.flowrunner_component_digest.to_string(),
            effect_provider_revision: self.effect_provider_revision.to_string(),
            host_effect_contract_version: self.host_effect_contract_version.to_string(),
        }
    }
}

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

/// The sole flowrunner export: one already claimed run and authoritative input.
type RunFunc = TypedFunc<(String, String), (Result<u32, String>,)>;

/// Production janitor grace, shared with the retry cap ceiling.
const PRODUCTION_JANITOR_GRACE_MS: i64 = 3_600_000;

fn bounded_attempt_ms(value: u64) -> u64 {
    value.clamp(1, MAX_HOST_CALL_DURATION.as_millis() as u64)
}

fn deadline_ticks(attempt_ms: u64) -> u64 {
    let tick_ms = DEFAULT_EPOCH_TICK.as_millis() as u64;
    bounded_attempt_ms(attempt_ms).div_ceil(tick_ms)
}

/// Outcome of one host-owned queue turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveOutcome {
    /// The guest executed and returned its frozen numeric outcome.
    Guest(u32),
    /// Claim-time classification or resolution terminalized without execution.
    ClaimTerminalized {
        status: RunStatus,
        fail_kind: FailKind,
    },
    /// The crash-budget janitor terminalized a pre-effect run.
    InfrastructureFailure,
}

/// One completed host queue turn, borrowed for synchronous caller observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveObservation<'a> {
    /// The run handled by this turn.
    pub run_id: &'a str,
    /// Guest result or exact host-owned non-execution terminalization.
    pub outcome: DriveOutcome,
    /// Wall time spent claiming and, for `Guest`, executing the turn.
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

    fn record(&mut self, outcome: DriveOutcome) {
        self.claimed += 1;
        match outcome {
            DriveOutcome::Guest(0) => self.completed += 1,
            DriveOutcome::Guest(1) => self.parked += 1,
            DriveOutcome::Guest(_)
            | DriveOutcome::ClaimTerminalized { .. }
            | DriveOutcome::InfrastructureFailure => self.failed += 1,
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
/// production fail-closed posture) AND the active connection's resolved hosts
/// (fqg.11 — supplied through trusted `wamn:runner/egress`, see
/// [`RunnerEgressPolicy`]; never-supplied or supplied-empty = deny-all,
/// both checks must pass = intersection), then delegate transport to
/// [`DefaultOutgoingHandler`] (which also stamps the 9.2 trace context).
/// Since the `wamn/2.7.0` pin that handler is a fielded struct owning a
/// `workload_id`-keyed client pool and a PRIVATE quota registry, so each
/// `RunnerEgress` built here gets its own — the isolation-first posture ruling
/// wamn-0h0g.13.48 chose over passing the host's shared registry.
/// Without a handler on the store's `Ctx`, an outbound call TRAPS ("http
/// client not available") and poisons the instance — so the runner wires this
/// unconditionally; a denial is a clean `HttpRequestDenied` the node
/// classifies as `egress-denied` (terminal).
struct RunnerEgress {
    inner: DefaultOutgoingHandler,
    /// Per-component connection-derived authority, written through the trusted
    /// `wamn:runner/egress` channel before each run.
    policy: Arc<RunnerEgressPolicy>,
}

fn bounded_outgoing_config(mut config: OutgoingRequestConfig) -> OutgoingRequestConfig {
    config.connect_timeout = config.connect_timeout.min(MAX_HOST_CALL_DURATION);
    config.first_byte_timeout = config.first_byte_timeout.min(MAX_HOST_CALL_DURATION);
    config.between_bytes_timeout = config.between_bytes_timeout.min(MAX_HOST_CALL_DURATION);
    config
}

/// The p3 twin of [`bounded_outgoing_config`]. p3 makes every timeout
/// `Option<Duration>` and `wasmtime_wasi_http::p3::default_send_request`
/// substitutes 600s for each `None` — twenty times [`MAX_HOST_CALL_DURATION`],
/// with nothing behind it, since epoch interruption cannot reach a host call
/// parked in `await`. So an absent timeout must come out AS the ceiling, not as
/// `None`: passing `None` through is what hands the guest the 600s default.
/// Destructured field-by-field so a new upstream timeout is a compile error
/// rather than a silent bypass.
fn bounded_request_options(options: Option<RequestOptions>) -> Option<RequestOptions> {
    let RequestOptions {
        connect_timeout,
        first_byte_timeout,
        between_bytes_timeout,
    } = options.unwrap_or_default();
    Some(RequestOptions {
        connect_timeout: Some(bounded_timeout(connect_timeout)),
        first_byte_timeout: Some(bounded_timeout(first_byte_timeout)),
        between_bytes_timeout: Some(bounded_timeout(between_bytes_timeout)),
    })
}

fn bounded_timeout(timeout: Option<Duration>) -> Duration {
    timeout
        .unwrap_or(MAX_HOST_CALL_DURATION)
        .min(MAX_HOST_CALL_DURATION)
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

    /// `inner`'s bind counterpart — `OutgoingHandler::on_workload_bind`, which
    /// sizes its pool for the burst a component declared — is deliberately NOT
    /// forwarded from here. `HostHandler` has no bind hook at all, and the
    /// `pool_size` × `max_concurrency` figure that one takes is not reachable
    /// from a [`ResolvedWorkload`] through the fork's public API: the decoded
    /// instance policy is consumed into a `pub(crate)` pool, and
    /// `WorkloadMetadata::component()` hands back the *wasmtime* component, not
    /// the `types::Component` that carries the limits. Passing a number we made
    /// up would size the pool off a fiction, so wamn-0h0g.15.37 forwarded the
    /// unbind half only and left this one to the fork.
    async fn on_workload_resolved(
        &self,
        _resolved: &ResolvedWorkload,
        _component_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Release `inner`'s per-workload egress state — pooled connections, TLS
    /// session store, pinned connection permits — when the workload stops,
    /// instead of leaving it to sit out the pool's idle window. The fork's hook
    /// is sync and infallible, so there is nothing to await and nothing to
    /// propagate.
    ///
    /// Nothing reaches this today, and why is the part that matters: this
    /// handler is installed on the store's `Ctx` (see
    /// [`ExecutionHost::instantiate`]), and that path only ever calls
    /// `outgoing_request`/`outgoing_request_p3` — the workload lifecycle hooks
    /// fire for a handler registered as the ENGINE's HTTP handler, which the
    /// runner has none of. Per-instance ownership is what keeps the gap
    /// harmless: `inner` and its pool are dropped with the store. The forward
    /// is what keeps it harmless if this handler is ever hoisted to engine
    /// scope or shared between stores.
    async fn on_workload_unbind(&self, workload_id: &str) -> anyhow::Result<()> {
        self.inner.on_workload_unbind(workload_id);
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
        // fqg.11: the connection-derived check. Absent and empty are the same
        // deny-all `&[]`; a resolved set must ALSO pass, while the host list
        // above stays the outer bound.
        let declared = self.policy.declared(workload_id);
        if let Err(e) = check_allowed_hosts(&request, declared.as_deref().unwrap_or(&[])) {
            tracing::warn!(
                workload_id,
                error = %e,
                declared = declared.is_some(),
                "run-worker outbound request denied by connection-derived authority"
            );
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestDenied,
            ))));
        }
        self.inner
            .send_request(workload_id, request, bounded_outgoing_config(config))
    }

    /// The p3 twin of [`Self::outgoing_request`]. The trait default checks
    /// `allowed_hosts` ALONE and then sends through `wasmtime_wasi_http::p3`
    /// directly, so inheriting it would drop BOTH the fqg.11 connection-derived
    /// narrowing and this handler's own `inner` transport — the same authority
    /// on one protocol version and not the other. Both checks run here in the
    /// same order, with the same clean-denial (never a trap) semantics and the
    /// same finite outbound ceiling ([`bounded_request_options`]).
    ///
    /// No production guest reaches this today: the runner's linker registers
    /// only the p2 `wasi:http` surface (see `instantiate`) and the flowrunner
    /// world imports no `wasi:http` interface at all (pinned by
    /// `tests/conformance/tests/flowrunner_linker_imports.rs`). The intersection
    /// is a property of this handler, not of that linker line.
    fn outgoing_request_p3(
        &self,
        workload_id: &str,
        request: hyper::Request<P3Body>,
        options: Option<RequestOptions>,
        fut: P3RequestErrorFuture,
        allowed_hosts: &[AllowedHost],
    ) -> P3SendFuture {
        if let Err(e) = check_allowed_hosts(&request, allowed_hosts) {
            tracing::warn!(
                workload_id,
                error = %e,
                "run-worker outbound p3 request denied by the allowed-hosts policy"
            );
            return Box::new(async move {
                Err(wasmtime_wasi::TrappableError::from(
                    P3ErrorCode::HttpRequestDenied,
                ))
            });
        }
        // fqg.11, on the same terms as p2: absent and empty are one deny-all `&[]`.
        let declared = self.policy.declared(workload_id);
        if let Err(e) = check_allowed_hosts(&request, declared.as_deref().unwrap_or(&[])) {
            tracing::warn!(
                workload_id,
                error = %e,
                declared = declared.is_some(),
                "run-worker outbound p3 request denied by connection-derived authority"
            );
            return Box::new(async move {
                Err(wasmtime_wasi::TrappableError::from(
                    P3ErrorCode::HttpRequestDenied,
                ))
            });
        }
        self.inner
            .send_request_p3(workload_id, request, bounded_request_options(options), fut)
    }
}

/// Where a serving process's release comes from: the mounted serving manifest,
/// and the registry its plan bytes are pulled from.
///
/// Passing this to [`ExecutionHost::instantiate`] is what makes a process a
/// *serving* one. Omitting it is how every other caller — gates, benches, the
/// pool's own fixtures — keeps working with nothing to mount: they get a host
/// whose plan supply reports `unavailable` instead of one that resolves plans
/// from a second, unwelded source.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseSupply<'a> {
    /// Directory the digest-named release-manifest ConfigMap is projected into,
    /// normally [`wamn_catalog::RELEASE_MANIFEST_MOUNT_PATH`].
    pub manifest_root: &'a Path,
    /// `<registry>/<repository>` plan artifacts are published under.
    pub plan_artifact_base: &'a str,
    /// Serve the registry over plain HTTP. The dev registry is anonymous plain
    /// HTTP; the wash host reaches the same one with
    /// `--allow-insecure-registries`.
    pub insecure_registry: bool,
    /// Bound on one plan pull, connect and read alike.
    pub fetch_timeout: Duration,
}

/// Load and verify this process's release, or record that it carries none.
///
/// This is the flowrunner in-process host's weld construction site: the one place
/// in this process that reads the mounted manifest. Plan supply and effect
/// authority both take the result by reference; nobody loads a second copy.
///
/// # The absent-mount posture, decided once and recorded here
///
/// An absent manifest is a deployment fact, not a fallback, and the two cases are
/// told apart by the *argument*, never by inspecting a failure:
///
/// - **No [`ReleaseSupply`] passed.** This process was never given a release.
///   There is nothing to mount and nothing to refuse: plan supply reports
///   `unavailable` for every run. Gates, benches and non-serving callers stay
///   exactly as they were.
/// - **Passed, but the mount is absent, unreadable or non-canonical.** This
///   process was told it serves a release and cannot. Host construction fails, so
///   the pod never goes ready — the only refusal that means anything, since a pod
///   that served with an unverified manifest would be resolving plans against
///   nothing.
///
/// Encoding the distinction in the argument is not a style choice.
/// `wamn-0h0g.15.104` collapsed [`WeldErrorKind`](wamn_runtime::release_manifest::WeldErrorKind)
/// to two variants, so "no mount" and "corrupt mount" now arrive as the same
/// `ManifestUnreadable` and cannot be separated after the fact. Whoever wires the
/// wash host (`wamn-0h0g.15.101`) must make the same call at *its* construction
/// site or restore a variant; recovering it from an error kind is not available.
fn load_plan_release(
    release: Option<ReleaseSupply<'_>>,
) -> anyhow::Result<Option<(Arc<ReleaseManifestWeld>, PlanRelease)>> {
    let Some(supply) = release else {
        return Ok(None);
    };
    let weld = ReleaseManifestWeld::load_from(supply.manifest_root).map_err(|error| {
        anyhow::anyhow!(
            "serving release manifest under {} is unusable ({:?}): {error}",
            supply.manifest_root.display(),
            error.kind()
        )
    })?;
    let source = OciPlanSource::new(
        supply.plan_artifact_base,
        supply.insecure_registry,
        supply.fetch_timeout,
    )?;
    tracing::info!(
        release_version = weld.release().release_version,
        manifest_digest = %weld.release().manifest_digest,
        plan_artifacts = supply.plan_artifact_base,
        "execution host welded to its release"
    );
    // ONE loaded instance, handed out by reference-count: reader 1 (plan supply)
    // takes it inside `PlanRelease`, reader 2 (effect authority, wamn-0h0g.15.66)
    // takes the same `Arc`. Nobody loads a second copy.
    let weld = Arc::new(weld);
    Ok(Some((
        Arc::clone(&weld),
        PlanRelease::new(weld, Arc::new(source)),
    )))
}

/// The host-injected, non-spoofable identity one runner replica carries: the
/// lease owner (== the component id), the tenant claim, the session
/// search_path, and — 5.9 — the project whose environment connection
/// credentials may be resolved. The guest reads these from its session; it
/// never chooses them.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionIdentity<'a> {
    pub owner: &'a str,
    pub tenant: &'a str,
    pub schema: Option<&'a str>,
    pub project: &'a str,
    /// Project-environment org, required only by the private production writer.
    pub org: Option<&'a str>,
    /// Project-environment name, required only by the private production writer.
    pub environment: Option<&'a str>,
    /// Exact project database, required only by the private production writer.
    pub database: Option<&'a str>,
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
    run: RunFunc,
}

pub struct ExecutionHost {
    live: Option<LiveExecution>,
    postgres: Arc<WamnPostgres>,
    logging: Arc<WamnLogging>,
    /// This instance's CLAIM SCOPE: the key every process-resident claim
    /// registry — `wamn:postgres`, `wasi:logging`, the egress declaration — is
    /// keyed by, and the id the store's `Ctx` was built with.
    ///
    /// It names the STORE, not the tenant. It is fixed for this instance's life
    /// so that concurrent acquisitions of different instances can never collide
    /// on one registry entry, and so that the ids captured into the fork's
    /// private `CtxHttpHooks` at construction stay in agreement with it. Identity
    /// is rebound UNDER it at every checkout — see [`ExecutionAcquisition`].
    component_id: Box<str>,
    runtime_revision: TrustedExecutionRuntimeRevision,
    /// Fixed private writer. Only expired-pre-effect projection reset is active;
    /// effect dispatch and frame-fact persistence remain unmounted until .5.4.
    effect_writer: Option<wamn_run_state::EffectWriterClient>,
    ttl_ms: u64,
    /// [9.8] `Some` when a memory limiter is attached (a budget was configured);
    /// each drive then publishes the store's high-water into the meter.
    mem: Option<MemoryMeter>,
    /// The trace parent for the acquisition this instance is serving, so a
    /// host-side span raised during a guest call nests under the run that caused
    /// it. [`tracing::Span::none`] until a checkout binds one.
    span: tracing::Span,
}

/// Everything about one execution instance that is derived from WHO it runs for.
///
/// A pooled instance is fungible compute. This is the state that is not: it
/// arrives at [`checkout`](ExecutionInstancePool::checkout) and is cleared when
/// that checkout ends, so one warm instance can serve two tenants in sequence
/// without either seeing the other's identity.
///
/// # The identity tuple, element by element
///
/// A PARTIAL REBIND IS THE SAME LEAK WEARING A FIX, so every element is named
/// here with where it is rebound. Adding identity-derived state to this host
/// without adding it here re-opens `wamn-0h0g.17.7`.
///
/// | Element | Rebound by |
/// |---|---|
/// | `app.tenant` — the RLS claim | [`SessionClaims::tenant`] → `WamnPostgres::bind_session_claims` |
/// | project (which database) | [`SessionClaims::project`] → same |
/// | `search_path` schema | [`SessionClaims::schema`] → same |
/// | `app.runner` — the lease owner | [`SessionClaims::runner`] → same |
/// | `app.role` (`wamn-0h0g.23.1`) | [`SessionClaims::role`] → same |
/// | `app.user_id` (`wamn-0h0g.23.1`) | [`SessionClaims::user_id`] → same |
/// | the carried `(release version, manifest digest)` | [`SessionClaims::release`] → same |
/// | the causation run context | cleared by the same call; the runner re-declares it per run |
/// | the `wasi:logging` `(tenant, project)` claim | [`Self::claims`] → `WamnLogging::set_claim` |
/// | the private effect-writer's project-environment scope | [`Self::effect_writer`] |
/// | the trace parent for this acquisition | [`Self::span`] |
///
/// Connection-generation resolution has no element of its own: a generation is
/// resolved per effect by `WamnPostgres::connection_effect_snapshot`, whose
/// scope is exactly `(tenant, project, schema)` from the claims above.
///
/// # What is deliberately NOT rebound, and why
///
/// - **The claim scope itself** (`ExecutionHost::component_id`, the store's
///   `Ctx.component_id`). It names the store, not the tenant. Rebinding it per
///   acquisition would desynchronize it from the workload id the fork captured
///   into its private `CtxHttpHooks` at `Ctx` construction, which is not
///   reachable from here.
/// - **The store's memory limiter.** Its label is the claim scope, not an
///   identity; its high-water and denial counters start at zero on a prewarmed
///   instance and cannot span two acquisitions, because
///   [`ExecutionHost::reset_invocation_state`] always fails and the pool
///   therefore destroys this instance at the end of every checkout.
/// - **Guest linear memory, globals and tables.** Covered by the separate
///   `INVOCATIONS_PER_INSTANCE` argument, not by this one.
///
/// # The one element that cannot be rebound from here
///
/// `ConnectionHttp` freezes `(tenant, project)` into `Box<str>` fields when the
/// store is constructed, and the store's `Ctx.plugins` map is private to the
/// fork, so nothing outside it can swap the plugin for a rebound one. That is a
/// live leak channel for the trusted HTTP effect, and it is closed HERE by
/// failing closed rather than by rebinding: `connection_effect_snapshot` refuses
/// a caller-supplied tenant that disagrees with the tenant bound to the claim
/// scope. A stale `ConnectionHttp` therefore produces a denial, never another
/// tenant's rows. Making it a rebind rather than a denial needs an owner outside
/// this crate's file domain.
#[derive(Debug, Clone)]
pub struct ExecutionAcquisition {
    /// Every `wamn:postgres` claim this checkout resolves under.
    pub claims: SessionClaims,
    /// The private production writer scoped to this acquisition's
    /// project-environment, or `None` where no private writer is mounted.
    ///
    /// Carried already-constructed rather than built here: the client owns a
    /// connection pool, and opening one is async while a rebind is not.
    pub effect_writer: Option<wamn_run_state::EffectWriterClient>,
    /// The trace parent for this acquisition. [`tracing::Span::none`] leaves
    /// host-side spans parented wherever the driving task puts them.
    pub span: tracing::Span,
}

impl ExecutionAcquisition {
    /// An acquisition with no trace parent and no private writer.
    ///
    /// The shape a caller that has neither has — a gate, a bench, or any driver
    /// running outside a trace. Host-side spans then parent wherever the driving
    /// task puts them, which is what they did before this seam existed.
    pub fn untraced(claims: SessionClaims) -> Self {
        Self {
            claims,
            effect_writer: None,
            span: tracing::Span::none(),
        }
    }
}

/// One acquisition's identity could not be bound to a warm execution instance.
#[derive(Debug)]
pub struct ExecutionIdentityBindError {
    scope: Box<str>,
    tenant: Box<str>,
    source: anyhow::Error,
}

impl std::fmt::Display for ExecutionIdentityBindError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "binding tenant {} to execution claim scope {} failed: {}",
            self.tenant, self.scope, self.source
        )
    }
}

impl std::error::Error for ExecutionIdentityBindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// A component instance cannot be reset in place from the host.
///
/// The host holds no handle on a component's linear memory, globals or tables —
/// a component exports none of them — so the only host-side zeroing of guest
/// state is destroying the store. Failing closed here is what makes
/// [`INVOCATIONS_PER_INSTANCE`] load-bearing rather than advisory: an
/// [`ExecutionHost`] is destroyed at the end of every checkout, whatever the
/// configured invocation cap says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionInstanceResetUnavailable;

impl std::fmt::Display for ExecutionInstanceResetUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a component instance cannot be reset in place from the host")
    }
}

impl std::error::Error for ExecutionInstanceResetUnavailable {}

impl ReusableExecutionInstance for ExecutionHost {
    type Identity = ExecutionAcquisition;
    type BindError = ExecutionIdentityBindError;
    type ResetError = ExecutionInstanceResetUnavailable;

    /// The platform memory ceiling the pooling allocator caps every store at.
    fn reserved_bytes(&self) -> usize {
        MEMORY_CAP_BYTES
    }

    /// Rebind every element of [`ExecutionAcquisition`]'s tuple under this
    /// instance's claim scope. The table on that type is the contract; this is
    /// the whole of its implementation, in the same order.
    fn bind_identity(
        &mut self,
        acquisition: &ExecutionAcquisition,
    ) -> Result<(), ExecutionIdentityBindError> {
        let claims = &acquisition.claims;
        self.postgres
            .bind_session_claims(&self.component_id, claims)
            .map_err(|source| ExecutionIdentityBindError {
                scope: self.component_id.clone(),
                tenant: claims.tenant.as_str().into(),
                source,
            })?;
        self.logging.set_claim(
            &self.component_id,
            &claims.tenant,
            claims.project.as_deref().unwrap_or(DEFAULT_PROJECT),
        );
        self.effect_writer = acquisition.effect_writer.clone();
        self.span = acquisition.span.clone();
        Ok(())
    }

    /// Clear the same elements. `wamn:postgres` is the RLS-load-bearing half, so
    /// its revocation is exact; `wasi:logging` has no removal on its plugin, and
    /// its claim is overwritten by the next bind under this same scope.
    fn revoke_identity(&mut self) {
        self.postgres.revoke_session_claims(&self.component_id);
        self.effect_writer = None;
        self.span = tracing::Span::none();
    }

    fn reset_invocation_state(&mut self) -> Result<(), ExecutionInstanceResetUnavailable> {
        Err(ExecutionInstanceResetUnavailable)
    }
}

impl std::fmt::Debug for ExecutionHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionHost")
            .field("runtime_revision", &self.runtime_revision)
            .field("component_id", &self.component_id)
            .field("effect_writer_loaded", &self.effect_writer.is_some())
            .field("ttl_ms", &self.ttl_ms)
            .field("disposed", &self.live.is_none())
            .finish_non_exhaustive()
    }
}

impl ExecutionHost {
    /// Instantiate the flowrunner component and inject this replica's identity.
    /// `identity.owner` is BOTH the component id and the `app.runner` lease
    /// owner (one process = one project = one owner, the single-project shape).
    /// The host retains the same plugin for private claim composition and for
    /// the guest-visible execution transaction capability.
    #[expect(
        clippy::too_many_arguments,
        reason = "the engine, guest, session plugins, identity, capabilities, release supply, and lease TTL are distinct host-injected inputs"
    )]
    pub async fn instantiate(
        engine: &Engine,
        guest: &[u8],
        plugin: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        identity: ExecutionIdentity<'_>,
        capabilities: ExecutionCapabilities,
        release: Option<ReleaseSupply<'_>>,
        ttl_ms: u64,
    ) -> anyhow::Result<Self> {
        let runtime_revision = TrustedExecutionRuntimeRevision::from_flowrunner_bytes(guest);
        let (effect_authority_weld, plan_release) = load_plan_release(release)?.unzip();
        let effect_writer = load_effect_writer(&identity).await?;
        let ExecutionIdentity {
            owner,
            tenant,
            schema,
            project,
            ..
        } = identity;
        // Non-spoofable, host-injected: the guest reads these from its session,
        // never chooses them. set_runner validates the owner charset.
        plugin.set_tenant(owner, tenant)?;
        // wamn-0h0g.17.9: the SAME project the `ConnectionHttp` built below
        // freezes into its `Box<str>`. This plugin's registry is what the
        // guest's own data path resolves (`project_for`), and an unregistered
        // project falls back to `DEFAULT_PROJECT` — so omitting this leaves one
        // process resolving TWO databases the moment the configured project is
        // not the default.
        plugin.set_project(owner, project)?;
        if let Some(s) = schema {
            plugin.set_schema(owner, s)?;
        }
        plugin.set_runner(owner, owner)?;
        // wamn-0h0g.15.103: the release a claim records comes from the SAME
        // verified object plan supply resolves against, so the pair stamped onto a
        // run and the manifest that run was resolved from cannot disagree. Ruling
        // wamn-0h0g.15.102 struck the per-workload config keys that used to assert
        // this pair — an asserted carrier cannot correct a welded one, so a
        // comparator between them would have carried no information.
        //
        // A process with no release records nothing, which is why this is
        // conditional rather than required: gates, benches and the pool's own
        // fixtures pass no `ReleaseSupply` and stay byte-unchanged.
        if let Some(plan_release) = plan_release.as_ref() {
            let carried = plan_release.weld().release();
            plugin.set_release_identity(
                owner,
                carried.release_version,
                carried.manifest_digest.clone(),
            )?;
        }
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
        runner_plan_supply::add_to_linker(&mut linker)?;
        // fqg.11: the TRUSTED per-run egress channel — same trust argument.
        runner_egress::add_runner_to_linker(&mut linker)?;
        // l5i9.12.2: the TRUSTED per-run causation channel — same trust
        // argument. The runner declares the run it drives so the wamn:postgres
        // plugin stamps a transactional wamn.causation message onto every
        // run-owned txn (the CDC reader stitches it).
        wamn_postgres::add_runner_causation_to_linker(&mut linker)?;
        connection_http::add_to_linker(&mut linker)?;
        // wamn-yf3: wasi:logging — the flowrunner emits a few structured records
        // per run (node/run lifecycle) that the wamn:logging plugin enriches +
        // ships. The guest imports it unconditionally, so the linker must satisfy
        // it; with OTEL unset the plugin's provider is a no-op, so this links
        // safely with no collector.
        wamn_logging::add_to_linker(&mut linker)?;
        let pre = linker.instantiate_pre(&component)?;

        let ExecutionCapabilities {
            mode,
            egress_policy,
        } = capabilities;
        let connection_allowed_hosts = match &mode {
            CapabilityMode::Production { allowed_hosts }
            | CapabilityMode::Injected { allowed_hosts, .. } => allowed_hosts.clone(),
        };
        let connection_http = Arc::new(ConnectionHttp::new(
            plugin.clone(),
            vault.clone(),
            egress_policy.clone(),
            tenant,
            project,
            connection_allowed_hosts,
            effect_authority_weld,
        ));
        let plan_supply = Arc::new(RunnerPlanSupply::new(
            plugin.clone(),
            plan_release,
            PLAN_CACHE_MAX_ENTRIES,
        )?);
        let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
        plugins.insert(
            wamn_postgres::WAMN_POSTGRES_ID,
            plugin.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        plugins.insert(
            RUNNER_EGRESS_ID,
            egress_policy.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        plugins.insert(
            RUNNER_PLAN_SUPPLY_ID,
            plan_supply as Arc<dyn HostPlugin + Send + Sync>,
        );
        plugins.insert(
            WAMN_LOGGING_ID,
            logging.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        plugins.insert(
            CONNECTION_HTTP_ID,
            connection_http as Arc<dyn HostPlugin + Send + Sync>,
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
                    inner: DefaultOutgoingHandler::default(),
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
        let run = instance.get_typed_func(&mut store, "run")?;

        Ok(Self {
            live: Some(LiveExecution { store, run }),
            postgres: plugin,
            logging,
            component_id: owner.into(),
            runtime_revision,
            effect_writer,
            ttl_ms: bounded_attempt_ms(ttl_ms),
            mem,
            span: tracing::Span::none(),
        })
    }

    /// This instance's claim scope — the key its host-side claims are registered
    /// under. Names the STORE, never the tenant; see [`ExecutionAcquisition`].
    pub fn claim_scope(&self) -> &str {
        &self.component_id
    }

    /// Return the trusted identity retained for this loaded runtime instance.
    pub fn runtime_revision(&self) -> &TrustedExecutionRuntimeRevision {
        &self.runtime_revision
    }

    /// Whether a Wasmtime interruption or trap disposed this instance.
    pub fn is_disposed(&self) -> bool {
        self.live.is_none()
    }

    /// Execute one run already selected, resolved, and leased by the host.
    ///
    /// The guest call runs inside this acquisition's trace parent, so every
    /// host-side span a guest import raises — `wamn.postgres` among them — nests
    /// under the run that caused it rather than under whichever run happened to
    /// be current when the store was built.
    async fn call_run(&mut self, run_id: &str, payload: &str) -> anyhow::Result<u32> {
        let span = self.span.clone();
        let call = {
            let live = self
                .live
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("execution instance disposed"))?;
            live.store.set_epoch_deadline(deadline_ticks(self.ttl_ms));
            live.run
                .call_async(&mut live.store, (run_id.to_owned(), payload.to_owned()))
                .instrument(span)
                .await
        };
        let (result,) = match call {
            Ok(result) => result,
            Err(error) => {
                self.live.take();
                return Err(anyhow::anyhow!(
                    "run trapped; execution instance disposed: {error}"
                ));
            }
        };
        result.map_err(|error| anyhow::anyhow!("run: {error}"))
    }

    /// Drain every currently claimable run through host-owned queue composition.
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
            let reap_started = std::time::Instant::now();
            match self
                .postgres
                .reap_one_exhausted_production(&self.component_id, PRODUCTION_JANITOR_GRACE_MS)
                .await?
            {
                ProductionReapResult::Empty | ProductionReapResult::EffectAttempt { .. } => {}
                ProductionReapResult::Reaped { run_id } => observe_drive(
                    &mut report,
                    DriveObservation {
                        run_id: &run_id,
                        outcome: DriveOutcome::InfrastructureFailure,
                        elapsed: reap_started.elapsed(),
                    },
                    &mut observe,
                ),
            }

            let claim_started = std::time::Instant::now();
            let claim = self
                .postgres
                .claim_next_production(&self.component_id, self.ttl_ms as i64)
                .await?;
            if let Some((run_id, payload)) = claim_guest_input(&claim) {
                let outcome = self.call_run(run_id, payload).await?;
                // [9.8] Snapshot the flowrunner store's memory high-water
                // after a successful guest drive when a limiter is attached.
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
                        run_id,
                        outcome: DriveOutcome::Guest(outcome),
                        elapsed: claim_started.elapsed(),
                    },
                    &mut observe,
                );
                continue;
            }
            match claim {
                ProductionClaimResult::Empty => break,
                ProductionClaimResult::ResetRequired {
                    run_id,
                    prior_lease_owner,
                    prior_lease_expires_at,
                    prior_lease_generation,
                } => {
                    let writer = self.effect_writer.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "expired pre-effect projection requires the fixed private writer"
                        )
                    })?;
                    match writer
                        .reset_expired_pre_effect_projection(ResetProjectionFence {
                            run_id: &run_id,
                            prior_lease_owner: &prior_lease_owner,
                            prior_lease_expires_at: &prior_lease_expires_at,
                            prior_lease_generation,
                        })
                        .await
                    {
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                EffectWriterErrorKind::EffectAttemptPresent
                                    | EffectWriterErrorKind::ResetFenceLost
                            ) => {}
                        Err(error) => return Err(error.into()),
                    }
                    continue;
                }
                ProductionClaimResult::Terminalized {
                    run_id,
                    status,
                    fail_kind,
                } => observe_drive(
                    &mut report,
                    DriveObservation {
                        run_id: &run_id,
                        outcome: DriveOutcome::ClaimTerminalized { status, fail_kind },
                        elapsed: claim_started.elapsed(),
                    },
                    &mut observe,
                ),
                ProductionClaimResult::Ready { .. } => {
                    unreachable!("ready claims are handled by claim_guest_input")
                }
            }
        }
        Ok(report)
    }
}

fn claim_guest_input(claim: &ProductionClaimResult) -> Option<(&str, &str)> {
    match claim {
        ProductionClaimResult::Ready {
            run_id, payload, ..
        } => Some((run_id, payload)),
        ProductionClaimResult::Empty
        | ProductionClaimResult::ResetRequired { .. }
        | ProductionClaimResult::Terminalized { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use wash_runtime::host::http_p3::P3SendResult;

    use super::*;

    async fn deadline_gate(increments: Arc<[u64]>) -> (ExecutionHost, Arc<AtomicUsize>) {
        // The sole core export calls the host tick, then returns a zeroed
        // canonical-ABI record encoding its successful component-level result.
        const COMPONENT: &str = r#"
            (component
                (import "tick" (func $tick))
                (core func $tick-lowered (canon lower (func $tick)))
                (core module $guest
                    (import "" "tick" (func $tick))
                    (memory (export "memory") 1)
                    (global $next (mut i32) (i32.const 1024))
                    (func $realloc
                        (export "realloc")
                        (param $old i32)
                        (param $old-size i32)
                        (param $align i32)
                        (param $new-size i32)
                        (result i32)
                        (local $result i32)
                        global.get $next
                        local.tee $result
                        local.get $new-size
                        i32.add
                        global.set $next
                        local.get $result)
                    (func $epoch-checkpoint
                        (local $return i32)
                        loop $checkpoint
                            local.get $return
                            if
                                return
                            end
                            i32.const 1
                            local.set $return
                            br $checkpoint
                        end)
                    (func (export "run")
                        (param i32 i32 i32 i32)
                        (result i32)
                        call $tick
                        call $epoch-checkpoint
                        i32.const 64
                        i32.const 0
                        i32.store
                        i32.const 68
                        i32.const 0
                        i32.store
                        i32.const 72
                        i32.const 0
                        i32.store
                        i32.const 64)
                    )
                (core instance $guest
                    (instantiate $guest
                        (with "" (instance
                            (export "tick" (func $tick-lowered))))))
                (func (export "run")
                    (param "run-id" string)
                    (param "payload" string)
                    (result (result u32 (error string)))
                    (canon lift
                        (core func $guest "run")
                        (memory $guest "memory")
                        (realloc (func $guest "realloc"))))
            )
        "#;

        let engine = wamn_runtime::engine::build_engine(&[]).expect("deadline gate engine");
        let raw = engine.inner();
        let bytes = wat::parse_str(COMPONENT).expect("encode deadline gate component");
        let component =
            WasmtimeComponent::new(raw, &bytes).expect("compile deadline gate component");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        linker
            .root()
            .func_wrap("tick", {
                let calls = calls.clone();
                let increments = increments.clone();
                let raw = raw.clone();
                move |_caller, (): ()| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    for _ in 0..increments.get(call).copied().unwrap_or_default() {
                        raw.increment_epoch();
                    }
                    Ok(())
                }
            })
            .expect("link deadline gate tick");
        let ctx = Ctx::builder("deadline-gate".to_string(), "deadline-gate".to_string()).build();
        let mut store = Store::new(raw, SharedCtx::new(ctx));
        store.set_epoch_deadline(deadline_ticks(40));
        let instance = linker
            .instantiate_async(&mut store, &component)
            .await
            .expect("instantiate deadline gate");
        let run = instance
            .get_typed_func(&mut store, "run")
            .expect("typed run gate");
        let postgres = Arc::new(
            WamnPostgres::new(wamn_runtime::plugins::wamn_postgres::WamnPostgresConfig {
                database_url: None,
                pool_max_size: 1,
                wait_timeout_ms: 100,
                statement_timeout_ms: 100,
                row_limit: 10,
            })
            .expect("offline postgres plugin"),
        );
        (
            ExecutionHost {
                live: Some(LiveExecution { store, run }),
                postgres,
                logging: Arc::new(WamnLogging::from_env().expect("offline logging plugin")),
                component_id: "deadline-gate".into(),
                runtime_revision: TrustedExecutionRuntimeRevision::from_flowrunner_bytes(&bytes),
                effect_writer: None,
                ttl_ms: 40,
                mem: None,
                span: tracing::Span::none(),
            },
            calls,
        )
    }

    #[tokio::test]
    async fn invocation_b_receives_a_fresh_epoch_window_after_a_consumes_ticks() {
        let (mut host, calls) = deadline_gate(Arc::from([2, 3])).await;

        host.call_run("run-a", "{}")
            .await
            .expect("A remains inside its four-tick window after consuming two ticks");
        host.call_run("run-b", "{}")
            .await
            .expect("B has four fresh ticks, not the two remaining from A");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn trapped_call_disposes_instance_before_later_invocation() {
        let (mut host, calls) = deadline_gate(Arc::from([5, 0])).await;

        let trapped = host
            .call_run("interrupt", "{}")
            .await
            .expect_err("advancing past the four-tick window interrupts the invocation");
        assert!(trapped.to_string().contains("run trapped"));
        assert!(trapped.to_string().contains("execution instance disposed"));
        assert!(host.is_disposed());

        let later = host
            .call_run("must-not-reuse", "{}")
            .await
            .expect_err("a later invocation cannot reuse the trapped instance");
        assert_eq!(later.to_string(), "execution instance disposed");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the later call never enters the trapped guest"
        );
    }

    #[test]
    fn trusted_runtime_revision_is_derived_from_exact_flowrunner_bytes() {
        let revision = TrustedExecutionRuntimeRevision::from_flowrunner_bytes(b"flowrunner-a");

        assert_eq!(
            revision.flowrunner_component_digest(),
            "sha256:e7153eea8dd20d7a44885a5656f8a83a2b2892ca5dd3d0a4f4684f97458cee63"
        );
        assert_eq!(
            revision.effect_provider_revision(),
            EFFECT_PROVIDER_REVISION
        );
        assert_eq!(
            revision.host_effect_contract_version(),
            HOST_EFFECT_CONTRACT_VERSION
        );
    }

    #[test]
    fn trusted_runtime_revision_projects_all_catalog_fields_exactly() {
        let trusted = TrustedExecutionRuntimeRevision::from_flowrunner_bytes(b"flowrunner-a");

        assert_eq!(
            trusted.execution_runtime_revision(),
            ExecutionRuntimeRevision {
                flowrunner_component_digest: trusted.flowrunner_component_digest().to_string(),
                effect_provider_revision: EFFECT_PROVIDER_REVISION.to_string(),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.to_string(),
            }
        );
    }

    fn method_source<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start = source.find(start).expect("method start");
        let end = source[start..].find(end).expect("next method") + start;
        &source[start..end]
    }

    #[test]
    fn run_rearms_before_calling_the_guest() {
        let source = include_str!("lib.rs");
        let method = method_source(source, "async fn call_run", "pub async fn drain");
        assert!(method.contains("live.store.set_epoch_deadline(deadline_ticks(self.ttl_ms));"));
    }

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
            org: None,
            environment: None,
            database: None,
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

    /// `None` is not "no policy" on p3 — the wasi-http default reads it as 600s.
    /// A clamp that only lowers PRESENT values leaves all three `None` and hands
    /// the guest that default, which is the whole of wamn-0h0g.15.87.
    #[test]
    fn absent_p3_timeouts_become_the_host_call_ceiling_not_the_wasi_default() {
        let bounded = bounded_request_options(None).expect("p3 options are always supplied");

        assert_eq!(bounded.connect_timeout, Some(MAX_HOST_CALL_DURATION));
        assert_eq!(bounded.first_byte_timeout, Some(MAX_HOST_CALL_DURATION));
        assert_eq!(bounded.between_bytes_timeout, Some(MAX_HOST_CALL_DURATION));
        // Substituting the ceiling only narrows while it stays under the 600s
        // wasi-http default; raising the ceiling past it would make this a
        // widening.
        assert!(MAX_HOST_CALL_DURATION < Duration::from_secs(600));
    }

    /// The ceiling is a ceiling, not a rewrite: a guest asking for LESS keeps it.
    /// Distinct per-field values so a clamp wired to one field, or one that stamps
    /// 30s over everything, fails here.
    #[test]
    fn over_long_p3_timeouts_clamp_down_while_shorter_ones_survive() {
        let bounded = bounded_request_options(Some(RequestOptions {
            connect_timeout: Some(Duration::from_secs(600)),
            first_byte_timeout: Some(Duration::MAX),
            between_bytes_timeout: Some(Duration::from_secs(5)),
        }))
        .expect("p3 options are always supplied");

        assert_eq!(bounded.connect_timeout, Some(MAX_HOST_CALL_DURATION));
        assert_eq!(bounded.first_byte_timeout, Some(MAX_HOST_CALL_DURATION));
        assert_eq!(
            bounded.between_bytes_timeout,
            Some(Duration::from_secs(5)),
            "a guest timeout below the ceiling stands"
        );
    }

    /// A `RunnerEgress` whose connection-derived set is unset (`None`) or seeded.
    fn runner_egress(declared: Option<&[&str]>) -> RunnerEgress {
        let policy = Arc::new(RunnerEgressPolicy::default());
        if let Some(hosts) = declared {
            let hosts: Vec<String> = hosts.iter().copied().map(String::from).collect();
            policy.set_declared("runner", &hosts);
        }
        RunnerEgress {
            inner: DefaultOutgoingHandler::default(),
            policy,
        }
    }

    fn allowed(entry: &str) -> AllowedHost {
        entry.parse().expect("allowed-host entry")
    }

    /// Drive one p3 outgoing request through the handler. The target is a closed
    /// loopback port, so a build that WRONGLY admits the request fails fast at
    /// the transport instead of denying — and a build that correctly denies
    /// never opens a socket at all.
    async fn p3_send(egress: &RunnerEgress, allowed_hosts: &[AllowedHost]) -> P3SendResult {
        let request = hyper::Request::builder()
            .uri("http://127.0.0.1:1/hook")
            .body(P3Body::default())
            .expect("build p3 outgoing request");
        let request_error: P3RequestErrorFuture = Box::new(async { Ok(()) });
        Box::into_pin(egress.outgoing_request_p3(
            "runner",
            request,
            None,
            request_error,
            allowed_hosts,
        ))
        .await
    }

    fn assert_p3_denied(outcome: P3SendResult) {
        let error = outcome
            .err()
            .expect("a denied p3 request never reaches the transport");
        assert!(
            matches!(error.downcast_ref(), Some(&P3ErrorCode::HttpRequestDenied)),
            "a p3 denial is a clean HttpRequestDenied, never a trap: {error:?}"
        );
    }

    /// fqg.11 on the p3 path: the connection-derived set is consulted even when
    /// the host-level allowlist would admit the request. Deleting the
    /// `outgoing_request_p3` override falls back to the fork's trait default,
    /// which checks `allowed_hosts` ALONE — this request passes that check, so
    /// the fallback reaches the transport instead of denying.
    #[tokio::test]
    async fn p3_egress_denies_a_component_that_declared_no_connection_authority() {
        let egress = runner_egress(None);

        assert_p3_denied(p3_send(&egress, &[allowed("127.0.0.1")]).await);
    }

    /// A declared-EMPTY set is deny-all on the raw egress path, not "no
    /// narrowing" — the opposite of `RunnerEgressPolicy::allows_connection`,
    /// which is the portable-connection rule. A mutant that only narrows when
    /// the declaration is `Some(non-empty)` admits this request.
    #[tokio::test]
    async fn p3_egress_denies_a_declared_empty_connection_set() {
        let egress = runner_egress(Some(&[]));

        assert_p3_denied(p3_send(&egress, &[allowed("127.0.0.1")]).await);
    }

    /// The host-level allowlist stays the OUTER bound on p3: a connection can
    /// never widen it. A mutant that keeps the override but drops the
    /// `allowed_hosts` leg admits this request. (This case alone does not
    /// discriminate override-vs-trait-default — the fork default denies it too.)
    #[tokio::test]
    async fn p3_egress_denies_when_the_host_allowlist_rejects_a_declared_host() {
        let egress = runner_egress(Some(&["127.0.0.1"]));

        assert_p3_denied(p3_send(&egress, &[]).await);
    }

    /// Both checks precede transport, and transport is THIS handler's `inner` —
    /// the trait default sends through `wasmtime_wasi_http::p3` directly and
    /// drops `inner` entirely. Catches an override that denies unconditionally
    /// (no delegation survives), or one that bypasses `inner`.
    #[test]
    fn p3_egress_checks_both_policies_before_delegating_transport() {
        let source = include_str!("lib.rs");
        let method = method_source(
            source,
            "fn outgoing_request_p3",
            "/// The host-injected, non-spoofable identity",
        );
        let host_bound = method
            .find("check_allowed_hosts(&request, allowed_hosts)")
            .expect("p3 keeps the host allowlist as the outer bound");
        let narrowed = method
            .find("check_allowed_hosts(&request, declared.as_deref().unwrap_or(&[]))")
            .expect("p3 applies the connection-derived set, absent/empty = deny-all");
        let delegate = method
            .find(".send_request_p3(")
            .expect("p3 transport is delegated, not denied unconditionally");
        assert!(host_bound < narrowed && narrowed < delegate);
        assert!(
            method.contains("self.inner"),
            "p3 must delegate to this handler's own inner OutgoingHandler"
        );
    }

    /// wamn-0h0g.15.37: workload stop RELEASES `inner`'s per-workload egress
    /// state rather than leaving it to the pool's idle window. A mutant that
    /// restores the bare `Ok(())` fails here. Pinned at the source, not driven:
    /// `DefaultOutgoingHandler` publishes no readback of its client cache, so
    /// observing the release would take a live server.
    #[test]
    fn workload_unbind_releases_the_pooled_handler_state() {
        let source = include_str!("lib.rs");
        let method = method_source(
            source,
            "async fn on_workload_unbind",
            "fn outgoing_request(",
        );

        assert!(
            method.contains("self.inner.on_workload_unbind(workload_id);"),
            "unbind must forward to this handler's own inner OutgoingHandler"
        );
    }

    /// The forward is safe on a handler that never sent a request: the fork's
    /// hook reads its client cache with `get()`, so a cold handler has nothing
    /// to release and nothing to lazily build — stopping a workload that never
    /// made an outbound call must not read a trust store or fail.
    #[tokio::test]
    async fn workload_unbind_is_a_no_op_on_a_handler_that_never_sent_a_request() {
        let egress = runner_egress(None);

        egress
            .on_workload_unbind("runner")
            .await
            .expect("unbinding a cold egress handler cannot fail");
    }

    #[test]
    fn callback_observations_and_aggregate_tallies_agree() {
        let mut r = DrainReport::default();
        let mut observed = Vec::new();
        let mut observe = |observation: DriveObservation<'_>| {
            observed.push((
                observation.run_id.to_owned(),
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
                    run_id: &run_id,
                    outcome: DriveOutcome::Guest(outcome),
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
        assert_eq!(observed[2].0, "run-1");
        assert_eq!(observed[2].1, DriveOutcome::Guest(1));
        assert_eq!(observed[2].2, Duration::from_millis(2));
        assert!(r.found_work());
    }

    #[test]
    fn host_never_calls_guest_for_nonexecute_claim_results() {
        assert_eq!(claim_guest_input(&ProductionClaimResult::Empty), None);
        assert_eq!(
            claim_guest_input(&ProductionClaimResult::ResetRequired {
                run_id: "reset".into(),
                prior_lease_owner: "dead".into(),
                prior_lease_expires_at: "2000-01-01 00:00:00+00".into(),
                prior_lease_generation: 4,
            }),
            None
        );
        assert_eq!(
            claim_guest_input(&ProductionClaimResult::Terminalized {
                run_id: "uncertain".into(),
                status: RunStatus::EffectUncertain,
                fail_kind: FailKind::EffectUncertain,
            }),
            None
        );
        let ready = ProductionClaimResult::Ready {
            run_id: "ready".into(),
            payload: "{\"input\":true}".into(),
            lease_generation: 9,
        };
        assert_eq!(
            claim_guest_input(&ready),
            Some(("ready", "{\"input\":true}"))
        );
    }

    /// Records which `acq-*` spans were entered, in order.
    ///
    /// Hand-rolled rather than pulled from `tracing-subscriber`: the only fact
    /// under test is which span a guest call runs inside, and a subscriber that
    /// answers exactly that costs less than a dependency.
    #[derive(Default)]
    struct AcquisitionSpanLog {
        names: std::sync::Mutex<std::collections::HashMap<u64, &'static str>>,
        entered: std::sync::Mutex<Vec<&'static str>>,
        next_id: std::sync::atomic::AtomicU64,
    }

    struct RecordingSubscriber(Arc<AcquisitionSpanLog>);

    impl tracing::Subscriber for RecordingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            let id = self
                .0
                .next_id
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            let name = span.metadata().name();
            if name.starts_with("acq-") {
                self.0
                    .names
                    .lock()
                    .expect("span name log poisoned")
                    .insert(id, name);
            }
            tracing::span::Id::from_u64(id)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {}

        fn enter(&self, span: &tracing::span::Id) {
            if let Some(name) = self
                .0
                .names
                .lock()
                .expect("span name log poisoned")
                .get(&span.into_u64())
            {
                self.0
                    .entered
                    .lock()
                    .expect("entered log poisoned")
                    .push(name);
            }
        }

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn acquisition(tenant: &str, span: tracing::Span) -> ExecutionAcquisition {
        ExecutionAcquisition {
            claims: SessionClaims {
                tenant: tenant.to_string(),
                ..SessionClaims::default()
            },
            effect_writer: None,
            span,
        }
    }

    /// The trace-parent element of the identity tuple: a guest call runs inside
    /// the span of the acquisition it is serving, never the previous one's.
    ///
    /// Without the rebind, a host-side span raised while tenant B's run is
    /// executing would nest under whatever parent the store was built with —
    /// cross-attributing B's database work to A's trace silently, which no claim
    /// assertion would catch. Both mutants die here: dropping `.instrument` from
    /// `call_run` leaves the log empty, and dropping the span assignment from
    /// `bind_identity` leaves both calls under the same (none) parent.
    #[tokio::test]
    async fn each_guest_call_runs_inside_its_own_acquisition_span() {
        let log = Arc::new(AcquisitionSpanLog::default());
        let _subscriber = tracing::subscriber::set_default(RecordingSubscriber(log.clone()));

        let (mut host, _calls) = deadline_gate(Arc::from([0, 0])).await;

        host.bind_identity(&acquisition("tenant-a", tracing::info_span!("acq-a")))
            .expect("tenant A binds");
        host.call_run("run-a", "{}").await.expect("A drives");
        host.revoke_identity();
        assert!(
            host.span.is_none(),
            "an ended checkout leaves no trace parent behind"
        );

        host.bind_identity(&acquisition("tenant-b", tracing::info_span!("acq-b")))
            .expect("tenant B binds");
        host.call_run("run-b", "{}").await.expect("B drives");

        // One call may enter its span more than once (a future is polled more
        // than once), so the fact under test is the ORDER of distinct parents:
        // A's call ran only under A, B's only under B, and nothing ran under A
        // after B acquired.
        let mut entered = log.entered.lock().expect("entered log poisoned").clone();
        entered.dedup();
        assert_eq!(
            entered,
            vec!["acq-a", "acq-b"],
            "each guest call must run under the span of the acquisition it serves"
        );
    }

    /// Binding refuses an invalid claim instead of half-writing it, and the
    /// error names both the claim scope and the tenant that was refused.
    #[tokio::test]
    async fn an_invalid_claim_refuses_the_bind_and_leaves_no_identity() {
        let (mut host, _calls) = deadline_gate(Arc::from([0])).await;
        let scope = host.claim_scope().to_string();

        let refused = host
            .bind_identity(&acquisition("not a valid tenant", tracing::Span::none()))
            .expect_err("an invalid tenant charset is refused");
        assert!(
            refused.to_string().starts_with(
                "binding tenant not a valid tenant to execution claim scope deadline-gate failed:"
            ),
            "the refusal names the scope and the tenant: {refused}"
        );
        assert_eq!(
            host.postgres.session_claims(&scope),
            None,
            "a refused bind leaves nothing resolvable"
        );
    }

    /// Every element the acquisition tuple names is bound, and every one is
    /// cleared again — the pair is what makes the tuple exhaustive rather than
    /// aspirational.
    #[tokio::test]
    async fn binding_and_revoking_cover_the_whole_claim_tuple() {
        let (mut host, _calls) = deadline_gate(Arc::from([0])).await;
        let scope = host.claim_scope().to_string();

        let claims = SessionClaims {
            tenant: "tenant-a".to_string(),
            project: Some("project-a".to_string()),
            schema: Some("schema_a".to_string()),
            runner: Some("runner-a".to_string()),
            role: Some("inspector".to_string()),
            user_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
            release: None,
        };
        host.bind_identity(&ExecutionAcquisition::untraced(claims.clone()))
            .expect("the full claim tuple binds");
        assert_eq!(host.postgres.session_claims(&scope), Some(claims));
        assert_eq!(
            host.logging.claim_snapshot(&scope),
            Some(("tenant-a".to_string(), "project-a".to_string()))
        );

        // A second acquisition that declares less must not inherit more: the
        // absent claims are removed, not left standing at the previous values.
        host.bind_identity(&ExecutionAcquisition::untraced(SessionClaims {
            tenant: "tenant-b".to_string(),
            ..SessionClaims::default()
        }))
        .expect("a narrower claim tuple binds");
        assert_eq!(
            host.postgres.session_claims(&scope),
            Some(SessionClaims {
                tenant: "tenant-b".to_string(),
                ..SessionClaims::default()
            }),
            "an acquisition that declares no schema, runner or role must get the deny floors"
        );

        host.revoke_identity();
        assert_eq!(host.postgres.session_claims(&scope), None);
    }

    #[test]
    fn reset_handoff_is_private_fail_closed_and_reenters_claim_loop() {
        let source = include_str!("lib.rs");
        let reset = source
            .find("ProductionClaimResult::ResetRequired {")
            .unwrap();
        let missing = source[reset..]
            .find("requires the fixed private writer")
            .unwrap();
        let call = source[reset..]
            .find(".reset_expired_pre_effect_projection(")
            .unwrap();
        let attempt_won = source[reset..]
            .find("EffectWriterErrorKind::EffectAttemptPresent")
            .unwrap();
        let fence_lost = source[reset..]
            .find("EffectWriterErrorKind::ResetFenceLost")
            .unwrap();
        let retry = source[reset..].find("continue;").unwrap();
        assert!(missing < call && call < attempt_won && attempt_won < retry);
        assert!(call < fence_lost && fence_lost < retry);
    }
}
