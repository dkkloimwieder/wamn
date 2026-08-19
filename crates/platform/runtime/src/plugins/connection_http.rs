//! Trusted one-frame HTTP connection effect for the digest-pinned flowrunner.
//!
//! # The run-to-plan binding is checked before anything is written
//!
//! `authorize_plan_closure` is the host-side restoration of link 1 of the
//! effect-authority chain (`wamn-0h0g.15.66`): the guest-declared
//! `current-plan-hash` must name a plan the run's release actually reaches. It
//! runs inside `ConnectionHttp::send` before the credential lookup and before
//! the wire, and — load-bearing, and the point of the ruling — before any effect
//! ledger write, so no `effect_attempts` row can ever exist carrying a plan hash
//! outside the run's release closure and the ledger stays audit-clean.
//!
//! That ordering is structural rather than merely arranged. `wamn:runner/http-effect`
//! exports exactly one function, `send`, so this is the whole guest-visible
//! surface of the capability; the effect ledger is host-written, never
//! guest-written (`wamn_run.effect_attempts` denies INSERT to `wamn_app`); and no
//! production caller of that writer exists yet at all. The obligation that
//! whoever activates it keeps the writer downstream of this check is recorded on
//! `BeginEffectAttempt` in `crates/execution/run-state/src/effect_writer.rs`,
//! beside the guest-supplied `current_plan_hash` it would persist.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use wamn_catalog::ServingManifest;
use wamn_flow::node_contract::normalize_portable_http_target;
use wamn_run_state::invocation_context::HttpEffectPrincipal;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::connection_authority::{
    AuthorityError, NetworkPolicy, TlsPolicy, TokioDnsResolver, TransportDecision,
    parse_http_connection_authority, resolve_http_request,
};
use crate::release_manifest::ReleaseManifestWeld;

use super::runner_egress::RunnerEgressPolicy;
use super::wamn_credentials::WamnCredentials;
use super::wamn_postgres::{ConnectionEffectLookup, ConnectionEffectSnapshot, WamnPostgres};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "runner-http-effect-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::runner::http_effect::{
    self, EffectError, Header, InvocationContext, RelativeRequest, Response,
};

pub const CONNECTION_HTTP_ID: &str = "wamn-connection-http";
const HTTP_CONTRACT: &str = "wamn:connection/http@0.1.0";

/// The authorized floor interpretation: Kubernetes/the network enforces the
/// cluster ceiling on the actual pinned connect.
#[derive(Debug, Clone, Copy)]
struct ExternallyEnforcedNetworkPolicy;

impl NetworkPolicy for ExternallyEnforcedNetworkPolicy {
    fn allows(&self, _address: SocketAddr) -> bool {
        true
    }
}

/// Host-owned services and claims for the trusted HTTP effect.
pub struct ConnectionHttp {
    postgres: Arc<WamnPostgres>,
    vault: Arc<WamnCredentials>,
    egress: Arc<RunnerEgressPolicy>,
    tenant: Box<str>,
    project: Box<str>,
    allowed_hosts: Arc<[AllowedHost]>,
    /// Reader 2 of the four the weld enumerates (`wamn-0h0g.15.100`): the ONE
    /// loaded, digest-verified serving manifest, held by reference and never
    /// loaded, parsed or digest-verified here. It is a weld, not a cache — there
    /// is no TTL, refresh or invalidation, because a digest-named object cannot go
    /// stale.
    ///
    /// `None` in a process that was given no release (gates, benches, the pool's
    /// own fixtures). Such a process cannot resolve a plan for any run, so it
    /// cannot authorize an effect either — see `authorize_plan_closure`.
    release: Option<Arc<ReleaseManifestWeld>>,
}

impl ConnectionHttp {
    pub fn new(
        postgres: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        egress: Arc<RunnerEgressPolicy>,
        tenant: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Option<Arc<ReleaseManifestWeld>>,
    ) -> Self {
        Self {
            postgres,
            vault,
            egress,
            tenant: tenant.into(),
            project: project.into(),
            allowed_hosts,
            release,
        }
    }

    async fn send(
        &self,
        component_id: &str,
        context: &InvocationContext,
        request: &RelativeRequest,
    ) -> Result<Response, EffectError> {
        validate_claims(context, request)?;
        let target = normalize_portable_http_target(&request.path_and_query).map_err(|error| {
            tracing::warn!(
                phase = "target-normalization",
                error = %error,
                "trusted HTTP connection authority denied"
            );
            EffectError::AuthorityDenied
        })?;
        let frame_id = i64::try_from(context.frame_id).map_err(|_| EffectError::InvalidContext)?;
        let occurrence =
            i32::try_from(context.occurrence).map_err(|_| EffectError::InvalidContext)?;
        let snapshot = self
            .postgres
            .connection_effect_snapshot(
                component_id,
                &self.project,
                &self.tenant,
                &ConnectionEffectLookup {
                    run_id: &context.run_id,
                    root_plan_hash: &context.root_plan_hash,
                    current_plan_hash: &context.current_plan_hash,
                    frame_id,
                    local_node_id: &context.local_node_id,
                    occurrence,
                    source_artifact_hash: &context.source_artifact_hash,
                    requirement_name: &context.requirement_name,
                },
            )
            .await
            .map_err(|error| EffectError::Transport(error.to_string()))?
            .ok_or(EffectError::InvalidContext)?;
        // Link 1 first: every lookup parameter above is guest-supplied, so nothing
        // is trustworthy until the presented plan is bound to the run
        // (wamn-0h0g.15.66). The manifest travels from the one welded instance by
        // reference; nothing is loaded, parsed or copied here.
        authorize_plan_closure(
            self.release.as_deref().map(|weld| weld.manifest()),
            &snapshot,
            &context.current_plan_hash,
        )?;
        authorize_snapshot(&snapshot)?;

        let definition = snapshot
            .definition
            .as_ref()
            .ok_or(EffectError::InactiveGeneration)?;
        let object = definition.as_object().ok_or(EffectError::Incompatible)?;
        let primary = object
            .get("primary-authority")
            .and_then(serde_json::Value::as_str)
            .ok_or(EffectError::Incompatible)?;
        let tls = match object
            .get("tls-verification")
            .and_then(serde_json::Value::as_str)
        {
            Some("disabled") => TlsPolicy::Disabled,
            Some("verify-authority") => TlsPolicy::VerifyAuthority,
            _ => return Err(EffectError::Incompatible),
        };
        require_direct_transport(object)?;
        let proxy = None;
        let authority = parse_http_connection_authority(primary, tls, proxy)
            .map_err(|error| authority_denied("definition", error))?;
        require_declared_egress(component_id, &self.egress, authority.canonical_base_url())?;
        let decision = resolve_http_request(
            &authority,
            &target,
            &self.allowed_hosts,
            &ExternallyEnforcedNetworkPolicy,
            &TokioDnsResolver,
        )
        .await
        .map_err(|error| authority_denied("request", error))?;

        let handle = snapshot
            .credential_handle
            .as_deref()
            .ok_or(EffectError::CredentialUnavailable)?;
        let secret = self
            .vault
            .lookup(&self.project, handle)
            .ok_or(EffectError::CredentialUnavailable)?;
        let credential_headers = credential_headers(&secret)?;
        execute(decision, request, credential_headers).await
    }
}

fn authority_denied(phase: &'static str, error: AuthorityError) -> EffectError {
    tracing::warn!(
        phase,
        kind = ?error.kind(),
        error = %error,
        "trusted HTTP connection authority denied"
    );
    EffectError::AuthorityDenied
}

fn log_effect_authority_denied(phase: &'static str, error: EffectError) -> EffectError {
    if matches!(error, EffectError::AuthorityDenied) {
        tracing::warn!(phase, "trusted HTTP connection authority denied");
    }
    error
}

fn require_direct_transport(
    definition: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), EffectError> {
    if matches!(
        definition.get("proxy-transport"),
        Some(serde_json::Value::Null)
    ) && matches!(
        definition.get("proxy-authority"),
        None | Some(serde_json::Value::Null)
    ) {
        Ok(())
    } else {
        Err(EffectError::Incompatible)
    }
}

fn validate_claims(
    context: &InvocationContext,
    request: &RelativeRequest,
) -> Result<(), EffectError> {
    if context.version != "0.1" || context.run_id.is_empty() || request.method.is_empty() {
        return Err(EffectError::InvalidContext);
    }
    HttpEffectPrincipal::new(
        context.root_plan_hash.as_str(),
        context.current_plan_hash.as_str(),
        context.frame_id,
        context.local_node_id.as_str(),
        context.occurrence,
        context.source_artifact_hash.as_str(),
        context.requirement_name.as_str(),
    )
    .map_err(|_| EffectError::InvalidContext)?;
    Ok(())
}

/// Link 1 of the effect-authority chain: bind the guest-declared current plan to
/// the run through the release manifest the run's recorded digest names
/// (`wamn-0h0g.15.66`, owner ruling option C — the host-side pre-check).
///
/// Every parameter of the authority lookup is guest-supplied, and since
/// `wamn-0h0g.15.10` deleted `run_flow_resolutions` no DATABASE-side statement
/// binds `current-plan-hash` to the run: `resolution_matches` only asks that a
/// bundle with that hash exist in the tenant, and `effect_attempts` is written
/// from the same guest-supplied context authority then checks against. Without
/// this function a compromised or buggy interpreter could present the bundle hash
/// of any other flow in its tenant and perform that flow's effect node under that
/// node's connection generation and credential handle.
///
/// # Why the host may answer this
///
/// The manifest this pod loaded IS the manifest the run's recorded digest names,
/// by construction rather than by comparison — see
/// [`WamnPostgres::set_release_identity`](super::wamn_postgres::WamnPostgres::set_release_identity)
/// for why, and owner ruling `wamn-0h0g.15.102` for why a comparator here would
/// carry no information and gets no mutant. The guest can forge neither half: the
/// recorded digest is host-injected and write-once (`wamn-0h0g.15.11`), and the
/// manifest is content-addressed and digest-verified at load
/// (`wamn-0h0g.15.100`). The threat model is unchanged from the deleted SQL
/// predicate's — the guest is untrusted, the host is not.
///
/// # Where the frame boundary is enforced, and why not here
///
/// The closure is taken from the run's ROOT flow. That is not a shortfall: it is
/// the same set the deleted `run_flow_resolutions` predicate held, which was by
/// construction the transitive reachable set from the root under the run's
/// release. This check computes it at check time instead of materializing it at
/// claim time — same predicate, same granularity, different carrier.
///
/// Frame-to-plan binding is interpreter-attested per the attempt-attestation
/// ruling; the database verifies closure membership. That ruling withdrew the
/// "descends from root" database check and rejected a `run_frames` registry, so
/// flow identity is absent from the effect wire BY DECISION rather than by
/// omission — the `wamn-0h0g.15.62` probe records the consequence, not a gap.
/// Should the interpreter's trust model ever change (non-first-party guest code,
/// custom nodes returning), `wamn-0h0g.15.128` carries the price.
fn authorize_plan_closure(
    manifest: Option<&ServingManifest>,
    snapshot: &ConnectionEffectSnapshot,
    current_plan_hash: &str,
) -> Result<(), EffectError> {
    // A process given no release resolves no plan for any run, so it can never
    // legitimately reach an effect. Refuse rather than admit an unbindable one;
    // plan supply takes the same posture and reports `unavailable`.
    let Some(manifest) = manifest else {
        tracing::warn!(
            phase = "effect-authority-violation",
            reason = "no-release-manifest",
            root_flow_id = snapshot.root_flow_id,
            "trusted HTTP connection authority denied"
        );
        return Err(EffectError::AuthorityDenied);
    };
    if !plan_reachable_from_root(manifest, &snapshot.root_flow_id, current_plan_hash) {
        // Host-side only, by owner recommendation: the flowrunner collapses
        // `authority-denied` with six sibling variants into one
        // `HttpCapError::Denied`, which standard-nodes codes as an egress refusal,
        // so the guest cannot be told this apart from an allowedHosts denial
        // without an SDK change. The name belongs in the audit record anyway.
        tracing::warn!(
            phase = "effect-authority-violation",
            reason = "plan-outside-run-closure",
            root_flow_id = snapshot.root_flow_id,
            current_plan_hash,
            "trusted HTTP connection authority denied"
        );
        return Err(EffectError::AuthorityDenied);
    }
    Ok(())
}

/// True when `current_plan_hash` is the plan of a flow the release manifest
/// reaches from `root_flow_id` over its call-edge adjacency.
///
/// The same walk plan supply resolves a run's plan set with
/// ([`ServingManifest::reachable_flows`]), read here instead of stored per run.
/// An absent root yields the empty set, so a run whose root is not a member of
/// this release admits nothing.
fn plan_reachable_from_root(
    manifest: &ServingManifest,
    root_flow_id: &str,
    current_plan_hash: &str,
) -> bool {
    manifest
        .reachable_flows(root_flow_id)
        .iter()
        // Total by construction: `from_canonical_bytes` has already proved every
        // call edge names a member flow.
        .filter_map(|flow_id| manifest.flows.get(flow_id))
        .any(|flow| flow.plan_hash == current_plan_hash)
}

fn authorize_snapshot(snapshot: &ConnectionEffectSnapshot) -> Result<(), EffectError> {
    if snapshot.run_status != "running"
        || !snapshot.root_plan_matches
        || !snapshot.resolution_matches
        || !snapshot.attempt_matches
    {
        return Err(EffectError::InvalidContext);
    }
    let Some(requirement) = snapshot.requirement_json.as_ref() else {
        return Err(EffectError::UndeclaredRequirement);
    };
    if !snapshot.node_permitted {
        return Err(EffectError::NodeNotPermitted);
    }
    if !snapshot.binding_active || !snapshot.binding_valid || snapshot.instance_id.is_none() {
        return Err(EffectError::Unbound);
    }
    if !snapshot.instance_enabled
        || snapshot.active_generation.is_none()
        || snapshot.active_generation != snapshot.generation
    {
        return Err(EffectError::InactiveGeneration);
    }
    if !snapshot.draft_generation_granted {
        return Err(EffectError::AuthorityDenied);
    }
    if snapshot.requirement_type.as_deref() != Some("http")
        || snapshot.contract.as_deref() != Some(HTTP_CONTRACT)
        || requirement
            .pointer("/requirement-type")
            .and_then(serde_json::Value::as_str)
            != Some("http")
        || requirement
            .pointer("/contract")
            .and_then(serde_json::Value::as_str)
            != Some(HTTP_CONTRACT)
    {
        return Err(EffectError::Incompatible);
    }
    Ok(())
}

fn require_declared_egress(
    component_id: &str,
    policy: &RunnerEgressPolicy,
    logical_url: &str,
) -> Result<(), EffectError> {
    let uri: hyper::Uri = logical_url.parse().map_err(|error| {
        tracing::warn!(
            phase = "flow-egress-url",
            error = %error,
            "trusted HTTP connection authority denied"
        );
        EffectError::AuthorityDenied
    })?;
    if policy.allows_connection(component_id, &uri) {
        Ok(())
    } else {
        tracing::warn!(
            component_id,
            logical_url,
            "trusted HTTP connection denied by the flow egress narrowing"
        );
        Err(EffectError::AuthorityDenied)
    }
}

fn credential_headers(secret: &str) -> Result<HashMap<String, String>, EffectError> {
    let value: serde_json::Value =
        serde_json::from_str(secret).map_err(|_| EffectError::CredentialUnavailable)?;
    let object = value
        .as_object()
        .ok_or(EffectError::CredentialUnavailable)?;
    if object.len() != 1 || !object.contains_key("headers") {
        return Err(EffectError::CredentialUnavailable);
    }
    object["headers"]
        .as_object()
        .ok_or(EffectError::CredentialUnavailable)?
        .iter()
        .map(|(name, value)| {
            let value = value.as_str().ok_or(EffectError::CredentialUnavailable)?;
            let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| EffectError::CredentialUnavailable)?;
            if reserved_header(&header) {
                return Err(EffectError::CredentialUnavailable);
            }
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| EffectError::CredentialUnavailable)?;
            Ok((name.clone(), value.to_string()))
        })
        .collect()
}

fn reserved_header(name: &reqwest::header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-connection"
            | "proxy-authorization"
            | "idempotency-key"
    )
}

fn outbound_headers(
    request: &RelativeRequest,
    credentials: HashMap<String, String>,
) -> Result<reqwest::header::HeaderMap, EffectError> {
    let mut headers = reqwest::header::HeaderMap::new();
    for header in &request.headers {
        let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| EffectError::AuthorityDenied)?;
        if reserved_header(&name) {
            return Err(EffectError::AuthorityDenied);
        }
        let value = reqwest::header::HeaderValue::from_bytes(&header.value)
            .map_err(|_| EffectError::AuthorityDenied)?;
        headers.append(name, value);
    }
    for (name, value) in credentials {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| EffectError::CredentialUnavailable)?;
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| EffectError::CredentialUnavailable)?;
        headers.insert(name, value);
    }
    Ok(headers)
}

async fn execute(
    decision: crate::connection_authority::AuthorityDecision,
    request: &RelativeRequest,
    credentials: HashMap<String, String>,
) -> Result<Response, EffectError> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| EffectError::Transport("invalid HTTP method".into()))?;
    let headers = outbound_headers(request, credentials)
        .map_err(|error| log_effect_authority_denied("outbound-headers", error))?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(30));
    let host = decision.logical_authority.host();
    match &decision.transport {
        TransportDecision::Direct { origin } => {
            builder = builder.resolve(host, origin.address);
        }
        TransportDecision::Proxy { .. } => return Err(EffectError::Incompatible),
    }
    let client = builder
        .build()
        .map_err(|error| EffectError::Transport(error.to_string()))?;
    let mut outbound = client
        .request(method, decision.logical_url.as_ref())
        .headers(headers);
    if let Some(body) = &request.body {
        outbound = outbound.body(body.clone());
    }
    let response = outbound.send().await.map_err(|error| {
        if error.is_timeout() {
            EffectError::Timeout
        } else {
            EffectError::Transport(error.to_string())
        }
    })?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| Header {
            name: name.as_str().to_string(),
            value: value.as_bytes().to_vec(),
        })
        .collect();
    let body = response
        .bytes()
        .await
        .map_err(|error| EffectError::Transport(error.to_string()))?
        .to_vec();
    Ok(Response {
        status,
        headers,
        body,
    })
}

pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    http_effect::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

impl HostPlugin for ConnectionHttp {
    fn id(&self) -> &'static str {
        CONNECTION_HTTP_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:runner/http-effect@0.1.0")]),
            exports: HashSet::new(),
        }
    }
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<ConnectionHttp>> {
    ctx.try_get_plugin::<ConnectionHttp>(CONNECTION_HTTP_ID)
}

impl http_effect::Host for ActiveCtx<'_> {
    async fn send(
        &mut self,
        context: InvocationContext,
        request: RelativeRequest,
    ) -> wash_runtime::wasmtime::Result<Result<Response, EffectError>> {
        let plugin = plugin_of(self)?;
        let result = plugin
            .send(self.component_id.as_ref(), &context, &request)
            .await;
        if let Err(error) = &result {
            tracing::warn!(
                error = ?error,
                run_id = context.run_id,
                frame_id = context.frame_id,
                local_node_id = context.local_node_id,
                occurrence = context.occurrence,
                requirement_name = context.requirement_name,
                "trusted HTTP effect refused"
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{ServingFlow, ServingRelease};

    use super::*;

    fn hash(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn context() -> InvocationContext {
        InvocationContext {
            version: "0.1".to_string(),
            run_id: "run-a".into(),
            root_plan_hash: hash('a'),
            current_plan_hash: hash('b'),
            frame_id: 4,
            local_node_id: "notify".into(),
            occurrence: 0,
            source_artifact_hash: hash('c'),
            requirement_name: "manager-notifications".into(),
        }
    }

    fn snapshot() -> ConnectionEffectSnapshot {
        ConnectionEffectSnapshot {
            run_status: "running".into(),
            root_plan_matches: true,
            resolution_matches: true,
            attempt_matches: true,
            requirement_json: Some(serde_json::json!({
                "requirement-type": "http",
                "contract": HTTP_CONTRACT
            })),
            node_permitted: true,
            binding_active: true,
            binding_valid: true,
            instance_id: Some("notifications".into()),
            requirement_type: Some("http".into()),
            contract: Some(HTTP_CONTRACT.into()),
            instance_enabled: true,
            active_generation: Some(7),
            generation: Some(7),
            definition: Some(serde_json::json!({})),
            definition_hash: Some("sha256:def".into()),
            credential_handle: Some("notify-auth".into()),
            draft_generation_granted: true,
            root_flow_id: ROOT_FLOW.into(),
        }
    }

    const ROOT_FLOW: &str = "root";
    const CALLEE_FLOW: &str = "callee";
    const UNREACHED_FLOW: &str = "sibling";

    /// A member flow whose plan hash is [`hash`] of `plan`.
    fn flow(plan: char, calls: BTreeSet<String>) -> ServingFlow {
        ServingFlow {
            flow_version: 1,
            plan_hash: hash(plan),
            source_artifact: hash('c'),
            binding_base_artifact: hash('c'),
            callable_contract: None,
            calls,
        }
    }

    /// One release holding three flows: `root`, the `callee` it calls, and an
    /// unreachable `sibling` — the borrowing target the deleted SQL predicate used
    /// to exclude (wamn-0h0g.15.66).
    fn release() -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                tenant_id: "t1".into(),
                catalog_id: "cat".into(),
                catalog_version: 7,
                environment: "prod".into(),
            },
            BTreeMap::from([
                (
                    ROOT_FLOW.to_string(),
                    flow('a', BTreeSet::from([CALLEE_FLOW.to_string()])),
                ),
                (CALLEE_FLOW.to_string(), flow('b', BTreeSet::new())),
                (UNREACHED_FLOW.to_string(), flow('d', BTreeSet::new())),
            ]),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect("release fixture is a valid manifest")
    }

    #[test]
    fn refusal_precedence_is_explicit_and_typed() {
        let mut facts = snapshot();
        facts.requirement_json = None;
        assert!(matches!(
            authorize_snapshot(&facts),
            Err(EffectError::UndeclaredRequirement)
        ));

        let mut facts = snapshot();
        facts.node_permitted = false;
        assert!(matches!(
            authorize_snapshot(&facts),
            Err(EffectError::NodeNotPermitted)
        ));

        let mut facts = snapshot();
        facts.instance_enabled = false;
        facts.active_generation = None;
        assert!(matches!(
            authorize_snapshot(&facts),
            Err(EffectError::InactiveGeneration)
        ));
    }

    #[test]
    fn callback_binding_mutant_is_denied_before_transport_resolution() {
        let mut inactive = snapshot();
        inactive.binding_active = false;
        let mut invalid = snapshot();
        invalid.binding_valid = false;
        let mut missing = snapshot();
        missing.instance_id = None;
        for facts in [&inactive, &invalid, &missing] {
            assert!(matches!(
                authorize_snapshot(facts),
                Err(EffectError::Unbound)
            ));
        }
    }

    #[test]
    fn wrong_attempt_and_wrong_run_identity_fail_before_authorization() {
        let mut wrong_attempt = snapshot();
        wrong_attempt.attempt_matches = false;
        assert!(matches!(
            authorize_snapshot(&wrong_attempt),
            Err(EffectError::InvalidContext)
        ));

        let mut wrong_run = snapshot();
        wrong_run.root_plan_matches = false;
        assert!(matches!(
            authorize_snapshot(&wrong_run),
            Err(EffectError::InvalidContext)
        ));
    }

    #[test]
    fn trusted_attempt_context_is_validated_before_authority_lookup() {
        let context = context();
        let request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/holds".into(),
            headers: Vec::new(),
            body: None,
        };
        assert!(validate_claims(&context, &request).is_ok());

        let mut invalid = context;
        invalid.current_plan_hash = "sha256:NOT-CANONICAL".into();
        assert!(matches!(
            validate_claims(&invalid, &request),
            Err(EffectError::InvalidContext)
        ));
    }

    #[test]
    fn durable_intent_is_required_before_the_wire_path() {
        let mut missing = snapshot();
        missing.attempt_matches = false;
        assert!(matches!(
            authorize_snapshot(&missing),
            Err(EffectError::InvalidContext)
        ));
        assert!(authorize_snapshot(&snapshot()).is_ok());
    }

    #[test]
    fn mismatched_source_or_revoked_draft_generation_is_denied_before_network_data() {
        let mut mismatched_source = snapshot();
        mismatched_source.resolution_matches = false;
        assert!(matches!(
            authorize_snapshot(&mismatched_source),
            Err(EffectError::InvalidContext)
        ));

        let mut revoked = snapshot();
        revoked.draft_generation_granted = false;
        assert!(matches!(
            authorize_snapshot(&revoked),
            Err(EffectError::AuthorityDenied)
        ));
    }

    /// wamn-0h0g.15.66, link 1: a run may present only a plan its own release
    /// reaches from its root flow.
    ///
    /// The `sibling` flow is a legal member of the SAME release and the SAME
    /// tenant, so every database-side fact about its bundle holds — it exists, it
    /// is tenant-scoped, and its bytes parse. Nothing but this check separates it
    /// from the run's own closure, which is exactly what the deleted
    /// `run_flow_resolutions` EXISTS predicate used to do.
    #[test]
    fn a_run_may_present_only_a_plan_its_own_release_closure_reaches() {
        let manifest = release();
        let facts = snapshot();

        // The root's own plan and its callee's are both inside the closure.
        for admitted in [hash('a'), hash('b')] {
            assert!(
                authorize_plan_closure(Some(&manifest), &facts, &admitted).is_ok(),
                "plan {admitted} is reachable from {ROOT_FLOW}"
            );
        }

        // The unreached sibling's plan, and a plan in no release at all, are not.
        for denied in [hash('d'), hash('e')] {
            assert!(
                matches!(
                    authorize_plan_closure(Some(&manifest), &facts, &denied),
                    Err(EffectError::AuthorityDenied)
                ),
                "plan {denied} is outside {ROOT_FLOW}'s closure and must be denied"
            );
        }

        // The closure is the RUN's, not the release's: a run rooted at the sibling
        // reaches the sibling's plan and nothing else.
        let sibling_run = ConnectionEffectSnapshot {
            root_flow_id: UNREACHED_FLOW.into(),
            ..snapshot()
        };
        assert!(authorize_plan_closure(Some(&manifest), &sibling_run, &hash('d')).is_ok());
        assert!(matches!(
            authorize_plan_closure(Some(&manifest), &sibling_run, &hash('b')),
            Err(EffectError::AuthorityDenied)
        ));
    }

    /// A root flow this release does not contain reaches nothing, so it authorizes
    /// nothing — including a plan the release does carry under another root.
    #[test]
    fn a_run_rooted_outside_this_release_authorizes_no_plan_at_all() {
        let manifest = release();
        let foreign = ConnectionEffectSnapshot {
            root_flow_id: "not-a-member".into(),
            ..snapshot()
        };
        for plan in [hash('a'), hash('b'), hash('d')] {
            assert!(matches!(
                authorize_plan_closure(Some(&manifest), &foreign, &plan),
                Err(EffectError::AuthorityDenied)
            ));
        }
    }

    /// A process that was given no release cannot bind a plan to a run, so it
    /// refuses instead of admitting an unbindable one — the same fail-closed
    /// posture plan supply takes with `unavailable`.
    #[test]
    fn a_process_carrying_no_release_authorizes_no_effect() {
        assert!(matches!(
            authorize_plan_closure(None, &snapshot(), &hash('b')),
            Err(EffectError::AuthorityDenied)
        ));
    }

    /// Rider 1 of the owner ruling: the run-to-plan binding is checked before the
    /// credential is fetched and before the wire, so nothing downstream — the
    /// effect ledger included — can observe an out-of-closure plan hash. Reordering
    /// `send` fails this.
    #[test]
    fn the_run_plan_binding_is_checked_before_credentials_and_the_wire() {
        // First occurrence of each: `send` is the first item in the file, so every
        // needle below lands on its call rather than on a later definition.
        let source = include_str!("connection_http.rs");
        let binding = source
            .find("authorize_plan_closure(")
            .expect("send checks the run-to-plan binding");
        let snapshot_gate = source
            .find("authorize_snapshot(&snapshot)?;")
            .expect("send checks the database-side facts");
        let credential = source
            .find("let secret = self")
            .expect("send resolves the credential");
        let wire = source
            .find("execute(decision, request, credential_headers)")
            .expect("send reaches the wire");
        assert!(binding < snapshot_gate && snapshot_gate < credential && credential < wire);
    }

    /// Until .4.9 installs the immutable attempt reader/writer together, the
    /// production `send` path refuses before a socket reaches the listener.
    #[tokio::test]
    async fn live_missing_immutable_attempt_authority_is_denied_before_wire() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use tokio::io::AsyncWriteExt as _;

        use crate::plugins::wamn_postgres::{DEFAULT_PROJECT, WamnPostgresConfig};

        const PROBE_AUTHORITY: &str = "127.0.0.1:18081";

        let Some(url) = std::env::var("WAMN_CONNECTION_EFFECT_PG_URL").ok() else {
            return;
        };
        let listener = tokio::net::TcpListener::bind(PROBE_AUTHORITY)
            .await
            .expect("bind revoked-draft zero-wire probe");
        let wire_count = Arc::new(AtomicUsize::new(0));
        let server_wire_count = Arc::clone(&wire_count);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept probe request");
            server_wire_count.fetch_add(1, Ordering::SeqCst);
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write probe response");
        });

        let postgres = Arc::new(
            WamnPostgres::new(WamnPostgresConfig {
                database_url: Some(url),
                pool_max_size: 1,
                wait_timeout_ms: 2_000,
                statement_timeout_ms: 5_000,
                row_limit: 1_000,
            })
            .expect("build live Postgres adapter"),
        );
        // wamn-0h0g.17.11: `connection_effect_snapshot` requires a BOUND tenant
        // claim, not merely one that agrees. The production path always has one
        // (`ExecutionHost::instantiate` feeds the same tenant to the registry
        // and to `ConnectionHttp`), so this binds the same `"t1"` the
        // `ConnectionHttp` below is constructed with — the agreeing case. What
        // is under test here is the ABSENT run row, which still denies before
        // the wire.
        postgres
            .set_tenant("draft-http-runner", "t1")
            .expect("bind the live effect tenant claim");
        postgres
            .set_schema("draft-http-runner", "wamn_run")
            .expect("set live run schema");

        let request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/probe".into(),
            headers: Vec::new(),
            body: None,
        };
        let vault = Arc::new(WamnCredentials::from_projects(HashMap::from([(
            DEFAULT_PROJECT.to_string(),
            HashMap::from([(
                "manager-credential-v1".to_string(),
                r#"{"headers":{}}"#.to_string(),
            )]),
        )])));
        let allowed_hosts: Arc<[AllowedHost]> =
            vec![PROBE_AUTHORITY.parse().expect("parse probe host policy")].into();
        // No release: this probe's run row does not exist, so `send` refuses on the
        // absent snapshot before the plan-closure check is reached. The zero-wire
        // property is what is under test, and it holds either way.
        let connection = ConnectionHttp::new(
            postgres,
            vault,
            Arc::new(RunnerEgressPolicy::default()),
            "t1",
            DEFAULT_PROJECT,
            allowed_hosts,
            None,
        );
        let context = InvocationContext {
            version: "0.1".to_string(),
            run_id: "missing-effect-attempt".into(),
            root_plan_hash: hash('a'),
            current_plan_hash: hash('b'),
            frame_id: 8,
            local_node_id: "notify".into(),
            occurrence: 0,
            source_artifact_hash: hash('c'),
            requirement_name: "manager".into(),
        };

        let result = connection
            .send("draft-http-runner", &context, &request)
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let observed_wires = wire_count.load(Ordering::SeqCst);
        server.abort();

        assert!(matches!(result, Err(EffectError::InvalidContext)));
        assert_eq!(
            observed_wires, 0,
            "missing immutable attempt authority must deny all network access"
        );
    }

    /// End offset of the first complete HTTP/1.1 request head in `pending`.
    fn head_end(pending: &[u8]) -> Option<usize> {
        pending
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|start| start + 4)
    }

    /// The `authorization` value one probe request carried, or the empty string.
    fn authorization_of(head: &str) -> String {
        head.lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map_or_else(String::new, |(_, value)| value.trim().to_string())
    }

    /// Two credential generations must never share one connection.
    ///
    /// The trusted effect builds its HTTP client inside `execute` and drops it
    /// at function exit, so the client's idle pool cannot outlive the single
    /// invocation whose credential authorized it. The probe answers with a
    /// keep-alive, fully delimited 204 — exactly the response a pooling client
    /// would file away and reuse — so a client hoisted into a struct field, a
    /// process-wide cell, or the wash-runtime pooled egress path (whose pool is
    /// keyed by `workload_id` alone) would carry generation 8 over the very
    /// connection generation 7 opened, and this test would see one connection
    /// instead of two. No database is involved: `execute` is the whole wire.
    #[tokio::test]
    async fn credential_generations_never_share_a_connection() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        const KEEP_ALIVE_204: &[u8] =
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind credential-generation probe listener");
        let probe = listener.local_addr().expect("probe listener address");

        let accepted = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::<(usize, String)>::new()));
        let server_accepted = Arc::clone(&accepted);
        let server_observed = Arc::clone(&observed);
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let connection = server_accepted.fetch_add(1, Ordering::SeqCst);
                let observed = Arc::clone(&server_observed);
                tokio::spawn(async move {
                    let mut pending = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while let Ok(read) = stream.read(&mut chunk).await {
                        if read == 0 {
                            break;
                        }
                        pending.extend_from_slice(&chunk[..read]);
                        while let Some(end) = head_end(&pending) {
                            let head = String::from_utf8_lossy(&pending[..end]).into_owned();
                            pending.drain(..end);
                            observed
                                .lock()
                                .expect("probe observation log")
                                .push((connection, authorization_of(&head)));
                            if stream.write_all(KEEP_ALIVE_204).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });

        let authority =
            parse_http_connection_authority(&format!("http://{probe}/"), TlsPolicy::Disabled, None)
                .expect("probe connection authority");
        let target = normalize_portable_http_target("/probe").expect("probe request target");
        let allowed_hosts: Arc<[AllowedHost]> =
            vec![probe.to_string().parse().expect("probe host policy")].into();
        let decision = resolve_http_request(
            &authority,
            &target,
            &allowed_hosts,
            &ExternallyEnforcedNetworkPolicy,
            &TokioDnsResolver,
        )
        .await
        .expect("probe authority decision");

        let request = RelativeRequest {
            method: "GET".into(),
            path_and_query: "/probe".into(),
            headers: Vec::new(),
            body: None,
        };
        for generation in [7, 8] {
            let credentials = HashMap::from([(
                "authorization".to_string(),
                format!("Bearer generation-{generation}"),
            )]);
            let response = execute(decision.clone(), &request, credentials)
                .await
                .expect("probe response");
            assert_eq!(response.status, 204);
        }
        server.abort();

        let calls = observed.lock().expect("probe observation log").clone();
        assert_eq!(
            calls,
            vec![
                (0, "Bearer generation-7".to_string()),
                (1, "Bearer generation-8".to_string()),
            ],
            "each credential generation must open its own connection; a client that outlives \
             one execute call would carry the second generation over the first generation's \
             keep-alive connection"
        );
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn credential_set_is_strict_structured_json() {
        let headers = credential_headers(
            r#"{"headers":{"authorization":"Bearer secret","x-api-key":"key"}}"#,
        )
        .expect("credential set");
        assert_eq!(headers["authorization"], "Bearer secret");
        assert_eq!(headers["x-api-key"], "key");
        for invalid in [
            "secret",
            r#"{"headers":[],"extra":{}}"#,
            r#"{"headers":{"bad header":"secret"}}"#,
            r#"{"headers":{"host":"other.example"}}"#,
        ] {
            assert!(matches!(
                credential_headers(invalid),
                Err(EffectError::CredentialUnavailable)
            ));
        }
    }

    #[test]
    fn authority_headers_are_host_owned_and_credentials_override_author_headers() {
        let mut request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/holds".into(),
            headers: vec![Header {
                name: "authorization".into(),
                value: b"caller-value".to_vec(),
            }],
            body: None,
        };
        let headers = outbound_headers(
            &request,
            HashMap::from([("authorization".into(), "Bearer host-value".into())]),
        )
        .expect("headers");
        assert_eq!(headers["authorization"], "Bearer host-value");
        assert!(!headers.contains_key("idempotency-key"));

        request.headers = vec![Header {
            name: "host".into(),
            value: b"other.example".to_vec(),
        }];
        assert!(matches!(
            outbound_headers(&request, HashMap::new()),
            Err(EffectError::AuthorityDenied)
        ));
    }

    #[test]
    fn idempotency_key_is_reserved_but_never_injected() {
        let request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/holds".into(),
            headers: Vec::new(),
            body: None,
        };
        let headers = outbound_headers(&request, HashMap::new()).expect("headers");
        assert!(!headers.contains_key("idempotency-key"));

        let authored = RelativeRequest {
            headers: vec![Header {
                name: "Idempotency-Key".into(),
                value: b"author-controlled".to_vec(),
            }],
            ..request
        };
        assert!(matches!(
            outbound_headers(&authored, HashMap::new()),
            Err(EffectError::AuthorityDenied)
        ));
        assert!(matches!(
            credential_headers(r#"{"headers":{"idempotency-key":"credential-controlled"}}"#),
            Err(EffectError::CredentialUnavailable)
        ));
    }

    #[test]
    fn proxy_generation_is_incompatible_without_direct_fallback() {
        let direct = serde_json::json!({"proxy-transport": null});
        assert!(require_direct_transport(direct.as_object().unwrap()).is_ok());

        let proxied = serde_json::json!({
            "proxy-transport": "connect",
            "proxy-authority": "http://proxy.internal:8080/"
        });
        assert!(matches!(
            require_direct_transport(proxied.as_object().unwrap()),
            Err(EffectError::Incompatible)
        ));
    }
}
