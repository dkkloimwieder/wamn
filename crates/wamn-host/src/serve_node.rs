//! `serve-node` — the PRODUCTION custom-node host (5.6 / wamn-bd5).
//!
//! §5.6's v0 dispatch of a *dynamically-loaded custom node* is a boring,
//! debuggable in-cluster HTTP hop. This module is the node end of it: a
//! long-lived host that instantiates ONE warm custom-node component under the
//! REAL frozen `wamn:node` world (`docs/wamn-node.wit`) and serves
//! `POST /run` invocations from the trusted flow-runner. It grew from the S4
//! `serve-node` harness (docs/p0-results.md §S4), production-ized on three axes:
//!
//! 1. **Real `wamn:node` imports, not the S4 `wait-ns` stand-in.** The linker
//!    offers the E17 tenant-node profile — `wamn:node/credentials` (get-only),
//!    `wamn:node/control` (the cancellation stub), and outbound `wasi:http`
//!    (host-allowlisted) — so the node runs with the exact capabilities a
//!    published custom node is granted, and nothing more. `wamn:node/payloads`
//!    is deliberately NOT linked (5.10): a node importing it fails instantiation,
//!    exactly as the frozen contract prescribes.
//!
//! 2. **Per-invocation credential grant + host-owned project (cjv.3).** The
//!    node is linked with the GET-ONLY credentials channel
//!    ([`wamn_credentials::add_to_linker`]) — NEVER the trusted
//!    `set-granted` channel — so it physically cannot self-grant. Before each
//!    dispatch the host installs EXACTLY the credentials the invocation envelope
//!    declares ([`WamnCredentials::set_granted_credentials`]); the PROJECT is
//!    the host's OWN injected identity (`--project`), not read from the
//!    (untrusted) request, so a forged envelope can never cross projects. An
//!    ungranted (sibling) credential is `not-granted` at the real WIT boundary —
//!    the credprobe precedent, now on the live invocation path.
//!
//! 3. **Config-parse memoization (design-note 9b).** The `json` config crosses
//!    the WIT boundary only for dynamic custom nodes; the warm instance
//!    validates a given `(node, flow-version, config-identity)` ONCE
//!    ([`ConfigCache`]) and reuses it across invocations.
//!
//! ## Trust model
//! Runner↔node authn is a SIGNED INVOCATION ENVELOPE (wamn-fqg.22): a
//! per-project-env HMAC-SHA256 over the exact request body, distributed via the
//! existing runner-credentials Secret (the reserved [`SIGNING_KEY_CREDENTIAL`]
//! vault entry). When a key is configured this host VERIFIES the signature
//! ([`ServeNode::verify_signature`]) BEFORE parsing the envelope or installing
//! the grant — a missing/malformed/wrong signature is a 401-class refusal that
//! never reaches [`ServeNode::invoke`], so a caller who lacks the env's key can
//! no longer POST attacker-chosen input/grant. mTLS (SPIFFE/cert-manager) is the
//! named later infra upgrade, not this path. With NO key configured the host
//! falls back to the v0 network-trust posture (unsigned POSTs admitted), logged
//! loudly at startup. The load-bearing host-enforced invariants that hold
//! regardless are: get-only linking (no self-grant), the host-owned project (no
//! cross-project read), and the E17 import allowlist screened at load.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail};
use clap::Args;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{
    DefaultOutgoingHandler, HostHandler, OutgoingHandler as _, check_allowed_hosts,
};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, InstancePre, Linker};
use wasmtime_wasi_http::p2::HttpResult;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};

use wamn_node_invoke::{
    ConfigCache, NodeInvokeRequest, NodeInvokeResponse, SIGNATURE_HEADER, SIGNING_KEY_CREDENTIAL,
    SIGNING_KEY_CREDENTIAL_PREVIOUS, SignatureError, TIMESTAMP_HEADER, WireEmission, WireErrorDetail,
    WireNodeError, WirePayload, WireRateLimit, timestamp_fresh, verify_envelope_with_timestamp,
};

use crate::egress_guard::screen_tenant_compiled;
use crate::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use crate::plugins::wamn_credentials::{self, WAMN_CREDENTIALS_ID, WamnCredentials};
use crate::plugins::wamn_node;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "serve-node",
        // The gate binds the same vendored WIT the host plugins bind
        // (crates/wamn-host/wit); no second copy (SR7).
        path: "wit",
        imports: { default: async },
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::ServeNode as NodeHandlerBindings;
use bindings::exports::wamn::node::handler::{Emission, NodeError, Payload, RunContext};

/// The default component identity a served node runs under. Grants + project are
/// registered on the vault under this id, and `wamn:node/credentials.get` sees
/// exactly this id (the `Ctx` component identity). One served component per host
/// (single-node deployment, the api-gateway / run-worker single-project shape).
pub const DEFAULT_NODE_ID: &str = "serve-node";

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct ServeNodeArgs {
    /// The custom-node component (`wamn:node/handler` export) to warm-instantiate
    /// and serve. In-cluster this is the node's published OCI artifact, mounted.
    #[arg(long)]
    pub node: PathBuf,

    /// TCP port to serve `POST /run` on.
    #[arg(long, default_value_t = 8080)]
    pub port: u16,

    /// The PROJECT whose vault credentials this node's invocations may read (the
    /// key into the credentials file). Host-injected identity — NOT read from
    /// the request — so a forged envelope can never cross projects.
    #[arg(long, env = "WAMN_PROJECT", default_value = "default")]
    pub project: String,

    /// The credential-vault source (5.9): a JSON file `{project: {name: secret}}`
    /// mounted from a K8s Secret (the WAMN_CREDENTIALS_FILE pattern). A missing
    /// file leaves the vault EMPTY (every granted resolution is `unavailable`);
    /// a malformed file is a hard error.
    #[arg(long, env = "WAMN_CREDENTIALS_FILE")]
    pub credentials_file: Option<PathBuf>,

    /// Hosts the node's OWN outbound `wasi:http` may reach (repeatable;
    /// `host[:port]`, `scheme://host`, `*.domain`, or `*`). EMPTY = DENY-ALL,
    /// the fail-closed posture. Governs the node's egress, distinct from the
    /// runner→node hop.
    #[arg(
        long = "allowed-hosts",
        env = "WAMN_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    pub allowed_hosts: Vec<String>,

    /// wamn-fqg.31: FAIL-CLOSED — require a per-project-env signing key. When set
    /// and NO key is configured for this project, REFUSE ALL invocations
    /// (401 `signing-key-required`) instead of silently reverting to the legacy
    /// network-trust posture. Default `false` keeps the backward-compatible
    /// behavior (unkeyed = network-trust, warned loudly at startup).
    #[arg(long = "require-signing-key", env = "WAMN_REQUIRE_SIGNING_KEY")]
    pub require_signing_key: bool,

    /// wamn-fqg.32: REPLAY-FRESHNESS max-age in seconds. When set, a signed
    /// envelope MUST carry a `x-wamn-timestamp` (covered by the signature) within
    /// this many seconds of the host clock, else it is refused (401
    /// `stale-timestamp`). OFF by default (replay-within-project-env is the
    /// documented accepted risk); this is the opt-in tightening. A legacy
    /// (timestamp-less) envelope keeps verifying while this is unset.
    #[arg(long = "signature-max-age-secs", env = "WAMN_SIGNATURE_MAX_AGE_SECS")]
    pub signature_max_age_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Node egress: the served node's OWN outbound wasi:http, host-allowlisted
// ---------------------------------------------------------------------------

/// The served node's outbound-`wasi:http` egress handler: enforce the host
/// allowlist (EMPTY = DENY-ALL, fail-closed) then delegate transport to
/// [`DefaultOutgoingHandler`]. Without a handler on the store's `Ctx` an
/// outbound call TRAPS and poisons the instance, so this is wired
/// unconditionally; a denial is a clean `HttpRequestDenied` the node classifies
/// as `egress-denied`. This is the node's egress (the http-node profile), NOT
/// the runner→node hop.
struct NodeEgress {
    inner: DefaultOutgoingHandler,
}

#[async_trait::async_trait]
impl HostHandler for NodeEgress {
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
        _resolved: &wash_runtime::engine::workload::ResolvedWorkload,
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
                "serve-node: node outbound request denied by the allowed-hosts policy"
            );
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestDenied,
            ))));
        }
        self.inner.send_request(workload_id, request, config)
    }
}

// ---------------------------------------------------------------------------
// The warm node instance
// ---------------------------------------------------------------------------

/// A warm `wamn:node` instance plus the per-instance config-parse cache.
struct NodeInstance {
    store: Store<SharedCtx>,
    handler: NodeHandlerBindings,
    config_cache: ConfigCache,
}

impl NodeInstance {
    /// Call the node's `handler.run` over the real ABI.
    async fn run_raw(
        &mut self,
        ctx: &RunContext,
        input: &Payload,
    ) -> wash_runtime::wasmtime::Result<Result<Emission, NodeError>> {
        self.handler
            .wamn_node_handler()
            .call_run(&mut self.store, ctx, input)
            .await
    }
}

/// The production custom-node host: one warm node behind a mutex (requests are
/// served sequentially — single instance), a fixed component identity the grant
/// + project are keyed by, and the shared vault. Reusable core (SR1): the
/// `nodeinvoke` gate drives THIS, the binary wraps it in the accept loop.
pub struct ServeNode {
    instance: Mutex<NodeInstance>,
    vault: Arc<WamnCredentials>,
    node_id: String,
    /// The per-project-env HMAC signing key (wamn-fqg.22), resolved once from
    /// the vault's reserved [`SIGNING_KEY_CREDENTIAL`] entry. `None` = no key
    /// configured (legacy v0 network-trust: POSTs are admitted unsigned).
    signing_key: Option<Vec<u8>>,
    /// wamn-fqg.30: the PREVIOUS per-project-env key (the reserved
    /// [`SIGNING_KEY_CREDENTIAL_PREVIOUS`] vault entry), accepted ALONGSIDE
    /// `signing_key` during a rotation window. `None` = no rotation in flight.
    previous_signing_key: Option<Vec<u8>>,
    /// wamn-fqg.31: FAIL-CLOSED posture. When `true` and no `signing_key` is
    /// configured, every invocation is REFUSED (`Unconfigured`) rather than
    /// admitted under network trust. Inert when a key IS configured.
    require_signing_key: bool,
    /// wamn-fqg.32: replay-freshness max-age (seconds). `Some` enforces a signed,
    /// in-window `x-wamn-timestamp` on every invocation; `None` (default) skips
    /// the age check, so a legacy timestamp-less envelope still verifies.
    max_signature_age_secs: Option<u64>,
    /// Count of per-invocation grants INSTALLED (cjv.3). The verify-before-grant
    /// witness (wamn-fqg.22): a refused request never reaches `invoke`, so a
    /// refusal must not advance this. The gate reads it via
    /// [`ServeNode::grant_install_count`].
    grant_installs: AtomicU64,
}

impl ServeNode {
    /// Compile, screen (E17 tenant profile), link the real `wamn:node` world,
    /// register the host-owned project, and warm-instantiate the node.
    pub async fn new(
        engine: &Engine,
        wasm: &[u8],
        vault: Arc<WamnCredentials>,
        node_id: &str,
        project: &str,
        allowed_hosts: Arc<[AllowedHost]>,
        require_signing_key: bool,
        max_signature_age_secs: Option<u64>,
    ) -> anyhow::Result<Self> {
        let raw = engine.inner();
        let component =
            WasmtimeComponent::new(raw, wasm).map_err(|e| anyhow::anyhow!("compile node: {e}"))?;
        // E17 posture: a custom node may import ONLY the tenant-node allowlist —
        // wamn:node interfaces + wasi:http + determinism/std shims. This refuses
        // wamn:postgres (raw DB) AND wamn:runner (the self-grant channel) at load.
        screen_tenant_compiled(&component, node_id)
            .map_err(|e| anyhow::anyhow!("serve-node refuses this node: {e}"))?;

        // 5.9: the vault resolves per (project, name); the project is a
        // host-injected claim, registered ONCE here for this served component.
        vault.set_project(node_id, project)?;

        // wamn-fqg.22: resolve the per-project-env signing key from the SAME
        // vault (the reserved entry banked in the runner-credentials Secret) — a
        // host-side lookup, never installed into any node grant. Absent = the
        // legacy network-trust posture, warned loudly so an unauthed deploy is
        // never silent.
        let signing_key = vault
            .lookup(project, SIGNING_KEY_CREDENTIAL)
            .map(String::into_bytes);
        // wamn-fqg.30: the optional PREVIOUS key for a rotation window (resolved
        // from the same vault, the second reserved name). Accepted alongside the
        // current key; never installed into a node grant.
        let previous_signing_key = vault
            .lookup(project, SIGNING_KEY_CREDENTIAL_PREVIOUS)
            .map(String::into_bytes);
        if previous_signing_key.is_some() {
            tracing::info!(
                project,
                key = SIGNING_KEY_CREDENTIAL_PREVIOUS,
                "serve-node: a PREVIOUS signing key is also accepted (wamn-fqg.30 rotation window)"
            );
        }
        if let Some(max_age) = max_signature_age_secs {
            tracing::info!(
                project,
                max_age_secs = max_age,
                "serve-node: replay-freshness ENFORCED (wamn-fqg.32; a signed in-window x-wamn-timestamp is required)"
            );
        }
        match (&signing_key, require_signing_key) {
            (Some(_), _) => tracing::info!(
                project,
                "serve-node: runner→node authn ENABLED (per-project-env signed envelope; verify-before-grant)"
            ),
            // wamn-fqg.31: fail-closed with no key is a MISCONFIGURATION — the
            // host will refuse every invocation. Loud, at error level.
            (None, true) => tracing::error!(
                project,
                key = SIGNING_KEY_CREDENTIAL,
                "serve-node: FAIL-CLOSED (--require-signing-key) but NO signing key in the vault — REFUSING ALL invocations (401 signing-key-required). Bank the reserved key in the runner-credentials Secret."
            ),
            (None, false) => tracing::warn!(
                project,
                key = SIGNING_KEY_CREDENTIAL,
                "serve-node: NO signing key in the vault — runner→node authn DISABLED (v0 network-trust; unsigned POSTs admitted). Bank the reserved key in the runner-credentials Secret, or set --require-signing-key to fail closed."
            ),
        }

        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        // The GET-ONLY credentials channel — NEVER `add_runner_to_linker`. A
        // custom node imports `wamn:node/credentials` directly and must not be
        // able to declare its own grant; the host installs it per invocation.
        wamn_credentials::add_to_linker(&mut linker)?;
        // The cooperative-cancellation stub (a node MAY link control).
        wamn_node::add_to_linker(&mut linker)?;
        // wamn:node/payloads is deliberately NOT linked (5.10): a node importing
        // it fails instantiation, exactly as the frozen contract prescribes.
        let pre: InstancePre<SharedCtx> = linker.instantiate_pre(&component)?;

        let instance =
            Self::instantiate(engine, &pre, vault.clone(), node_id, allowed_hosts).await?;
        Ok(Self {
            instance: Mutex::new(instance),
            vault,
            node_id: node_id.to_string(),
            signing_key,
            previous_signing_key,
            require_signing_key,
            max_signature_age_secs,
            grant_installs: AtomicU64::new(0),
        })
    }

    /// Verify the runner→node signature over the RAW request body BEFORE any
    /// parse or grant install (wamn-fqg.22, the load-bearing property). With a
    /// per-project-env key configured, a request MUST carry a matching
    /// `x-wamn-signature`; with NO key (legacy v0 network-trust) every request is
    /// admitted. Refusals return a MAC-free [`SignatureError`] — never the
    /// expected MAC (no verification oracle).
    pub fn verify_signature(
        &self,
        body: &[u8],
        signature: Option<&str>,
        timestamp: Option<&str>,
        now_secs: u64,
    ) -> Result<(), SignatureError> {
        let Some(key) = self.signing_key.as_deref() else {
            // wamn-fqg.31: with no key configured, a FAIL-CLOSED host refuses ALL
            // invocations rather than silently reverting to network trust; the
            // default (backward-compatible) admits them under legacy network-trust.
            return if self.require_signing_key {
                Err(SignatureError::Unconfigured)
            } else {
                Ok(())
            };
        };
        let sig = signature.ok_or(SignatureError::Missing)?;
        // wamn-fqg.30: accept the CURRENT key, else the PREVIOUS key if one is
        // configured (a rotation window). wamn-fqg.32: the `x-wamn-timestamp`
        // (when present) is COVERED BY the MAC — pass it through so the signed
        // bytes match, so a stripped/edited timestamp fails here. The canonical
        // constant-time verify lives in wamn-node-invoke; we call it once per key.
        match verify_envelope_with_timestamp(key, body, timestamp, sig) {
            Ok(()) => {}
            Err(e) => match self.previous_signing_key.as_deref() {
                Some(prev) => verify_envelope_with_timestamp(prev, body, timestamp, sig)?,
                None => return Err(e),
            },
        }
        // wamn-fqg.32: freshness gate — OFF by default (replay-within-project-env
        // is the documented accepted risk). When a max-age is configured, a
        // timestamp is REQUIRED and must be in-window. Checked AFTER signature
        // verification, so a freshness refusal is only ever returned to an
        // authenticated caller — a forged envelope always gets `bad-signature`,
        // never a freshness oracle.
        if let Some(max_age) = self.max_signature_age_secs {
            let ts = timestamp.ok_or(SignatureError::MissingTimestamp)?;
            let ts = ts
                .trim()
                .parse::<u64>()
                .map_err(|_| SignatureError::MalformedTimestamp)?;
            if !timestamp_fresh(ts, now_secs, max_age) {
                return Err(SignatureError::Stale);
            }
        }
        Ok(())
    }

    /// How many per-invocation grants have been INSTALLED (cjv.3) — the
    /// verify-before-grant witness (wamn-fqg.22). A signature-refused request
    /// never reaches [`ServeNode::invoke`], so a refusal must not advance this.
    pub fn grant_install_count(&self) -> u64 {
        self.grant_installs.load(Ordering::Relaxed)
    }

    async fn instantiate(
        engine: &Engine,
        pre: &InstancePre<SharedCtx>,
        vault: Arc<WamnCredentials>,
        node_id: &str,
        allowed_hosts: Arc<[AllowedHost]>,
    ) -> anyhow::Result<NodeInstance> {
        let mut plugins: std::collections::HashMap<
            &'static str,
            Arc<dyn HostPlugin + Send + Sync>,
        > = std::collections::HashMap::new();
        plugins.insert(
            WAMN_CREDENTIALS_ID,
            vault as Arc<dyn HostPlugin + Send + Sync>,
        );
        let ctx = Ctx::builder(node_id.to_string(), node_id.to_string())
            .with_plugins(plugins)
            .with_http_handler(Arc::new(NodeEgress {
                inner: DefaultOutgoingHandler::default(),
            }))
            .with_allowed_hosts(allowed_hosts)
            .build();
        let mut store = Store::new(engine.inner(), SharedCtx::new(ctx));
        // A generous epoch deadline: the ticker advances it, but a legitimately
        // slow node (an outbound call) is never epoch-killed mid-invocation.
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = pre.instantiate_async(&mut store).await?;
        let handler = NodeHandlerBindings::new(&mut store, &instance)?;
        Ok(NodeInstance {
            store,
            handler,
            config_cache: ConfigCache::new(),
        })
    }

    /// Dispatch one invocation: install the per-invocation grant (cjv.3),
    /// validate + memoize the config (9b), then call the node's handler over the
    /// real `wamn:node` world and shape the reply.
    pub async fn invoke(&self, req: NodeInvokeRequest) -> NodeInvokeResponse {
        // Install EXACTLY this invocation's declared grant BEFORE dispatch. The
        // project stays the host's own (set once in `new`); a `get` for anything
        // outside `req.grant` is `not-granted` host-side. The per-project-env
        // signing key is a RUNNER secret, never a node grant — strip it
        // defensively (a well-behaved runner never sends it; this closes a
        // flow-authoring footgun where a node.credential collides with the
        // reserved name). The install is COUNTED so the gate can prove a
        // signature-refused request never got this far (wamn-fqg.22).
        let grant = req
            .grant
            .iter()
            .filter(|n| {
                // wamn-fqg.30: BOTH reserved signing-key names are runner secrets,
                // never node grants.
                n.as_str() != SIGNING_KEY_CREDENTIAL
                    && n.as_str() != SIGNING_KEY_CREDENTIAL_PREVIOUS
            })
            .cloned();
        self.vault.set_granted_credentials(&self.node_id, grant);
        self.grant_installs.fetch_add(1, Ordering::Relaxed);

        let mut inst = self.instance.lock().await;

        // 9b: validate + memoize the config once per (node, flow-version,
        // identity). A malformed config is rejected here, before the guest call.
        if let Err(e) =
            inst.config_cache
                .prepared(&req.ctx.node_id, req.ctx.flow_version, &req.ctx.config)
        {
            return NodeInvokeResponse::Err(WireNodeError::InvalidInput(WireErrorDetail {
                message: e.to_string(),
                code: Some("invalid-config".to_string()),
                data: None,
            }));
        }

        let ctx = RunContext {
            run_id: req.ctx.run_id.clone(),
            flow_id: req.ctx.flow_id.clone(),
            flow_version: req.ctx.flow_version,
            node_id: req.ctx.node_id.clone(),
            attempt: req.ctx.attempt,
            idempotency_key: req.ctx.idempotency_key.clone(),
            traceparent: req.ctx.traceparent.clone(),
            tracestate: req.ctx.tracestate.clone(),
            deadline_ms: req.ctx.deadline_ms,
            config: req.ctx.config.clone(),
        };
        let input = Payload::Inline(req.input.inline().unwrap_or("null").to_string());

        match inst.run_raw(&ctx, &input).await {
            Ok(Ok(em)) => NodeInvokeResponse::Ok(emission_to_wire(em)),
            Ok(Err(e)) => NodeInvokeResponse::Err(node_error_to_wire(e)),
            // A trap poisons only THIS call's semantics (the store survives for
            // one instance); surface it as retryable so the runner's policy
            // decides — a boring, safe default for v0.
            Err(trap) => NodeInvokeResponse::Err(WireNodeError::Retryable(WireErrorDetail {
                message: format!("node invocation trapped: {trap}"),
                code: Some("node-trap".to_string()),
                data: None,
            })),
        }
    }

    /// Config-parse count witness (design-note 9b): one parse per distinct
    /// config identity, regardless of how many invocations shared it.
    pub async fn config_parse_count(&self) -> u64 {
        self.instance.lock().await.config_cache.parse_count()
    }
}

// ---------------------------------------------------------------------------
// WIT <-> wire mapping (the frozen node-error taxonomy, variant for variant)
// ---------------------------------------------------------------------------

fn wire_payload(p: Payload) -> WirePayload {
    match p {
        Payload::Inline(s) => WirePayload::Inline(s),
        // v0 nodes emit inline; a streamed emission waits for the payload store
        // (5.10). Surface it as an inline handle marker so nothing silently
        // vanishes (the runner never sees streamed in v0).
        Payload::Streamed(r) => WirePayload::Inline(format!("{{\"streamed\":{:?}}}", r.handle)),
    }
}

fn emission_to_wire(em: Emission) -> WireEmission {
    WireEmission {
        payload: wire_payload(em.payload),
        port: em.port,
    }
}

fn detail_to_wire(d: bindings::wamn::node::types::ErrorDetail) -> WireErrorDetail {
    WireErrorDetail {
        message: d.message,
        code: d.code,
        data: d.data,
    }
}

fn node_error_to_wire(e: NodeError) -> WireNodeError {
    match e {
        NodeError::Retryable(d) => WireNodeError::Retryable(detail_to_wire(d)),
        NodeError::RateLimited(r) => WireNodeError::RateLimited(WireRateLimit {
            detail: detail_to_wire(r.detail),
            retry_after_ms: r.retry_after_ms,
            target_host: r.target_host,
        }),
        NodeError::Terminal(d) => WireNodeError::Terminal(detail_to_wire(d)),
        NodeError::InvalidInput(d) => WireNodeError::InvalidInput(detail_to_wire(d)),
        NodeError::Cancelled => WireNodeError::Cancelled,
    }
}

// ---------------------------------------------------------------------------
// HTTP server (one keep-alive connection at a time; minimal HTTP/1.1)
// ---------------------------------------------------------------------------

/// Serve `POST /run` invocations on `port` until the process exits. Sequential
/// (one warm instance behind a mutex) — the boring v0 shape the S4 hop measured
/// at p50 33 µs cross-pod.
pub async fn serve(node: Arc<ServeNode>, port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "serve-node up (POST /run {{ctx,input,grant}})");
    loop {
        let (sock, peer) = listener.accept().await?;
        let node = node.clone();
        // Sequential: a connection at a time (single warm instance). A slow
        // connection blocks others — acceptable for the boring v0 hop.
        if let Err(e) = serve_connection(sock, &node).await {
            tracing::warn!(%peer, error = %e, "serve-node: connection error");
        }
    }
}

async fn serve_connection(sock: TcpStream, node: &ServeNode) -> anyhow::Result<()> {
    sock.set_nodelay(true)?;
    let mut reader = BufReader::new(sock);
    loop {
        let (body, signature, timestamp) = match read_http_request_body(&mut reader).await? {
            Some(parts) => parts,
            None => break, // clean EOF
        };
        // wamn-fqg.22: verify the signature over the RAW body BEFORE parsing the
        // envelope or installing the grant. A refusal is a 401-class response
        // that never reaches `invoke` — verify-before-grant, the load-bearing
        // property. wamn-fqg.32: pass the freshness timestamp + the host clock so
        // a stale envelope is refused when the max-age gate is enabled.
        if let Err(e) =
            node.verify_signature(&body, signature.as_deref(), timestamp.as_deref(), now_unix_secs())
        {
            tracing::warn!(
                reason = e.reason(),
                "serve-node: invocation REFUSED — runner→node signature check failed (before grant install)"
            );
            let http = unauthorized_response(e);
            reader.get_mut().write_all(http.as_bytes()).await?;
            reader.get_mut().flush().await?;
            continue;
        }
        let resp = match NodeInvokeRequest::from_json(&String::from_utf8_lossy(&body)) {
            Ok(req) => node.invoke(req).await,
            Err(e) => NodeInvokeResponse::Err(WireNodeError::InvalidInput(WireErrorDetail {
                message: format!("malformed invocation envelope: {e}"),
                code: Some("bad-envelope".to_string()),
                data: None,
            })),
        };
        let out = resp.to_json();
        let http = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            out.len(),
            out
        );
        reader.get_mut().write_all(http.as_bytes()).await?;
        reader.get_mut().flush().await?;
    }
    Ok(())
}

/// A 401-class refusal for a runner→node signature failure (wamn-fqg.22).
/// Carries ONLY the MAC-free reason class — never the expected MAC, so a refusal
/// can never become a verification oracle. Distinct from the `NodeInvokeResponse`
/// envelope (which is a NODE outcome), so the gate — and the runner — tell an
/// authn refusal apart from a node error.
fn unauthorized_response(err: SignatureError) -> String {
    let body = format!(
        r#"{{"error":"invocation-unauthorized","reason":"{}"}}"#,
        err.reason()
    );
    format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
        body.len(),
        body
    )
}

/// Read one HTTP message's headers+body (server side), returning the body, the
/// `x-wamn-signature` header value (wamn-fqg.22), and the `x-wamn-timestamp`
/// header value (wamn-fqg.32) if present. None on a clean EOF. Handles BOTH
/// `Content-Length` and `Transfer-Encoding: chunked` — the `wasi:http` outbound
/// path frames a streamed body as chunked (no Content-Length), so a
/// Content-Length-only parser would read an empty body.
async fn read_http_request_body<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Option<(Vec<u8>, Option<String>, Option<String>)>> {
    let mut content_length = 0usize;
    let mut chunked = false;
    let mut saw_any = false;
    let mut signature: Option<String> = None;
    let mut timestamp: Option<String> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return if saw_any {
                bail!("connection closed mid-headers")
            } else {
                Ok(None)
            };
        }
        saw_any = true;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break; // end of headers
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = lower.strip_prefix("transfer-encoding:") {
            chunked = v.contains("chunked");
        } else if let Some(v) = lower
            .strip_prefix(SIGNATURE_HEADER)
            .and_then(|r| r.strip_prefix(':'))
        {
            // "<header>:<hex>" — the hex is case-insensitive, so the lowercased
            // line is faithful. The exact `header:` prefix avoids matching a
            // longer header name.
            signature = Some(v.trim().to_string());
        } else if let Some(v) = lower
            .strip_prefix(TIMESTAMP_HEADER)
            .and_then(|r| r.strip_prefix(':'))
        {
            // wamn-fqg.32: "<header>:<decimal>" — an ASCII decimal, so the
            // lowercased line is faithful.
            timestamp = Some(v.trim().to_string());
        }
    }
    let body = if chunked {
        read_chunked_body(reader).await?
    } else {
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).await?;
        body
    };
    Ok(Some((body, signature, timestamp)))
}

/// The host clock in whole unix seconds — the reference the wamn-fqg.32 freshness
/// gate compares a request's `x-wamn-timestamp` against. A pre-epoch clock (never
/// in practice) reads 0, which simply fails every freshness check closed.
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Decode a `Transfer-Encoding: chunked` body: `<hexlen>\r\n<data>\r\n` chunks
/// terminated by a zero-length chunk (plus any trailers).
async fn read_chunked_body<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            bail!("connection closed mid-chunk-size");
        }
        // A chunk size may carry `;ext` — take the hex prefix only.
        let hex = line.trim_end();
        let hex = hex.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(hex, 16)
            .map_err(|_| anyhow::anyhow!("bad chunk size {hex:?}"))?;
        if size == 0 {
            // Consume trailers up to the terminating blank line.
            loop {
                line.clear();
                let n = reader.read_line(&mut line).await?;
                if n == 0 || line.trim_end().is_empty() {
                    break;
                }
            }
            break;
        }
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk).await?;
        body.extend_from_slice(&chunk);
        // Consume the CRLF trailing this chunk's data.
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf).await?;
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Binary entry (`wamn-host serve-node`)
// ---------------------------------------------------------------------------

pub async fn run(args: ServeNodeArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let wasm = std::fs::read(&args.node)
        .with_context(|| format!("read node component {}", args.node.display()))?;

    let vault = Arc::new(match &args.credentials_file {
        Some(path) => WamnCredentials::from_file(path)?,
        None => WamnCredentials::empty(),
    });

    let allowed_hosts: Arc<[AllowedHost]> = args
        .allowed_hosts
        .iter()
        .map(|s| s.parse::<AllowedHost>())
        .collect::<Result<Vec<_>, _>>()
        .context("parse --allowed-hosts")?
        .into();

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    let node = Arc::new(
        ServeNode::new(
            &engine,
            &wasm,
            vault,
            DEFAULT_NODE_ID,
            &args.project,
            allowed_hosts,
            args.require_signing_key,
            args.signature_max_age_secs,
        )
        .await?,
    );

    tracing::info!(
        node = %args.node.display(),
        project = %args.project,
        port = args.port,
        "serve-node: warm node instantiated (real wamn:node world)"
    );

    let result = serve(node, args.port).await;
    ticker.abort();
    result
}
