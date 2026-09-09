//! Environment-bound HTTP for ordinary `wamn:node` components.
//!
//! The host binds the exact wiring, node position, occurrence and component
//! digest before invoking a pooled component. The guest names only a store
//! alias; the database must resolve that alias at the component grain and the
//! mounted format-1 manifest must contain both the exact wiring version/hash and
//! component tuple. No run, plan, frame or effect-ledger fact participates.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::Instrument as _;
use wamn_catalog::{
    ArtifactHash, ConnectionTypeDescriptor, DefinitionHash, ServingManifest, ServingWiring,
};
use wamn_execution_contract::node_contract::normalize_portable_http_target;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::connection_authority::{
    AuthorityError, NetworkPolicy, TlsPolicy, TokioDnsResolver, TransportDecision,
    parse_http_connection_authority, resolve_http_request,
};
use crate::plugins::effect_span::{
    EFFECT_OPERATION, EffectIdentity, EffectOutcome, EffectOutcomeGuard, EffectWiring,
    HTTP_EFFECT_DURATION_MS, effect_span, record_effect_ms, record_wiring,
};
use crate::release_manifest::ReleaseManifestWeld;

use super::wamn_credentials::WamnCredentials;
use super::wamn_postgres::{
    CandidateBindingWorld, ConnectionEffectLookup, ConnectionEffectSnapshot, WamnPostgres,
};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "connection-http-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::connection::http::{self, ConnectionError, Header, Request, Response};

pub const CONNECTION_HTTP_ID: &str = "wamn-connection-http";
const HTTP_CONTRACT: &str = "wamn:connection/http@0.1.0";
const AUTHORITY_SNAPSHOT_UNAVAILABLE: &str = "connection-authority-unavailable";
/// The transport detail for an HTTP client that never built. One of the three
/// details this file mints itself, so [`http_outcome`] can place it before
/// dispatch instead of guessing at a rendered error string.
const HTTP_CLIENT_UNAVAILABLE: &str = "connection-client-unavailable";
/// The transport detail for a response the far side sent and the host never
/// finished reading. Minted for the same reason as the other two: only this
/// file knows the status line already arrived.
const RESPONSE_BODY_LOST: &str = "connection-response-lost";

/// Host-attested identity of one component invocation.
///
/// `component` is the name the catalog admitted the executing component under,
/// and `operation` is the node operation the router driver called on it. Both
/// come from the driver, never from the guest. They are here because
/// `component_digest` is a manifest key and no reader looks a component up by
/// it, and because the capability method an effect span records says which host
/// function ran, not which node operation raised it (`wamn-b2m6.7`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInvocation {
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub node_id: String,
    pub occurrence: u32,
    pub component_digest: String,
    pub component: String,
    pub operation: String,
    pub closure: ConnectionExecutionClosure,
}

/// Host-owned authority closure for one component invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionExecutionClosure {
    /// A digest-verified serving manifest owns the component and wiring.
    Released,
    /// Private admission froze an exact candidate wiring and binding world.
    Candidate {
        effective_release_id: u32,
        environment: String,
        wiring_hash: String,
        component: String,
        interface_version: String,
        binding_world: Arc<CandidateBindingWorld>,
    },
}

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
    /// own fixtures). Such a process cannot attest a wiring/component closure,
    /// so it cannot authorize a connection.
    release: Option<Arc<ReleaseManifestWeld>>,
    /// Component-store owner id to the invocation currently using that pooled
    /// instance. The production driver binds before `handler.run` and revokes
    /// before returning the instance to the pool.
    invocations: std::sync::RwLock<HashMap<String, ConnectionInvocation>>,
}

impl ConnectionHttp {
    pub fn new(
        postgres: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        tenant: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Option<Arc<ReleaseManifestWeld>>,
    ) -> Self {
        Self {
            postgres,
            vault,
            tenant: tenant.into(),
            project: project.into(),
            allowed_hosts,
            release,
            invocations: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Bind the exact invocation facts before entering one pooled component.
    /// A still-bound owner refuses rather than silently replacing leaked state.
    pub fn bind_invocation(
        &self,
        component_id: &str,
        invocation: ConnectionInvocation,
    ) -> anyhow::Result<()> {
        let digest = invocation
            .component_digest
            .strip_prefix("sha256:")
            .unwrap_or_default();
        anyhow::ensure!(
            !component_id.is_empty()
                && !invocation.package_id.is_empty()
                && !invocation.wiring_id.is_empty()
                && invocation.wiring_version > 0
                && !invocation.node_id.is_empty()
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "connection-http-invocation-invalid"
        );
        if let ConnectionExecutionClosure::Candidate {
            effective_release_id,
            environment,
            wiring_hash,
            component,
            interface_version,
            ..
        } = &invocation.closure
        {
            let graph = wiring_hash.strip_prefix("sha256:").unwrap_or_default();
            anyhow::ensure!(
                *effective_release_id > 0
                    && !environment.is_empty()
                    && !component.is_empty()
                    && !interface_version.is_empty()
                    && graph.len() == 64
                    && graph
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "connection-http-candidate-closure-invalid"
            );
        }
        let mut bound = self
            .invocations
            .write()
            .map_err(|_| anyhow::anyhow!("connection-http-invocation-lock-poisoned"))?;
        anyhow::ensure!(
            !bound.contains_key(component_id),
            "connection-http-invocation-already-bound"
        );
        bound.insert(component_id.to_owned(), invocation);
        Ok(())
    }

    /// Clear the invocation before returning an instance to the pool.
    pub fn revoke_invocation(&self, component_id: &str) {
        if let Ok(mut bound) = self.invocations.write() {
            bound.remove(component_id);
        }
    }

    fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        self.invocations.read().ok()?.get(component_id).cloned()
    }

    async fn send(
        &self,
        component_id: &str,
        request: &Request,
    ) -> Result<Response, ConnectionError> {
        let invocation = self
            .invocation(component_id)
            .ok_or(ConnectionError::AttestationInvalid)?;
        if request.requirement.is_empty() || request.method.is_empty() {
            return Err(ConnectionError::Incompatible);
        }
        let target = normalize_portable_http_target(&request.path_and_query).map_err(|error| {
            tracing::warn!(
                phase = "target-normalization",
                error = %error,
                "trusted HTTP connection authority denied"
            );
            ConnectionError::AuthorityDenied
        })?;
        let manifest = match &invocation.closure {
            ConnectionExecutionClosure::Released => Some(
                self.release
                    .as_deref()
                    .ok_or(ConnectionError::AttestationInvalid)?
                    .manifest(),
            ),
            ConnectionExecutionClosure::Candidate { .. } => None,
        };
        let (effective_release_id, environment, candidate_binding) =
            match (&invocation.closure, manifest) {
                (ConnectionExecutionClosure::Released, Some(manifest)) => {
                    if manifest.release.tenant_id != self.tenant.as_ref()
                        || !manifest
                            .release
                            .packages
                            .iter()
                            .any(|package| package.package_id() == invocation.package_id.as_str())
                    {
                        return Err(ConnectionError::AttestationInvalid);
                    }
                    (
                        i32::try_from(manifest.release.effective_release_id.get())
                            .map_err(|_| ConnectionError::AttestationInvalid)?,
                        manifest.release.environment.as_str(),
                        None,
                    )
                }
                (
                    ConnectionExecutionClosure::Candidate {
                        effective_release_id,
                        environment,
                        binding_world,
                        ..
                    },
                    None,
                ) => (
                    i32::try_from(*effective_release_id)
                        .map_err(|_| ConnectionError::AttestationInvalid)?,
                    environment.as_str(),
                    Some(
                        binding_world
                            .binding(&invocation.component_digest, &request.requirement)
                            .ok_or(ConnectionError::AttestationInvalid)?,
                    ),
                ),
                _ => return Err(ConnectionError::AttestationInvalid),
            };
        let wiring_version = i32::try_from(invocation.wiring_version)
            .map_err(|_| ConnectionError::AttestationInvalid)?;
        let snapshot = self
            .postgres
            .connection_effect_snapshot(
                component_id,
                &self.project,
                &self.tenant,
                &ConnectionEffectLookup {
                    package_id: &invocation.package_id,
                    effective_release_id,
                    environment,
                    wiring_id: &invocation.wiring_id,
                    wiring_version,
                    node_id: &invocation.node_id,
                    component_digest: &invocation.component_digest,
                    store_alias: &request.requirement,
                    candidate_binding,
                },
            )
            .await
            .map_err(|error| {
                tracing::warn!(
                    error = %error,
                    "trusted HTTP connection authority snapshot failed"
                );
                ConnectionError::Transport(AUTHORITY_SNAPSHOT_UNAVAILABLE.to_string())
            })?
            .ok_or(ConnectionError::AttestationInvalid)?;
        match (&invocation.closure, manifest) {
            (ConnectionExecutionClosure::Released, Some(manifest)) => {
                authorize_release_closure(manifest, &invocation, &snapshot)?;
            }
            (ConnectionExecutionClosure::Candidate { .. }, None) => {
                authorize_candidate_closure(
                    &invocation,
                    &snapshot,
                    candidate_binding.ok_or(ConnectionError::AttestationInvalid)?,
                )?;
            }
            _ => return Err(ConnectionError::AttestationInvalid),
        }
        authorize_snapshot(&snapshot, &http_descriptor())?;

        let definition = snapshot
            .definition
            .as_ref()
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let object = definition
            .as_object()
            .ok_or(ConnectionError::Incompatible)?;
        let primary = object
            .get("primary-authority")
            .and_then(serde_json::Value::as_str)
            .ok_or(ConnectionError::Incompatible)?;
        let tls = match object
            .get("tls-verification")
            .and_then(serde_json::Value::as_str)
        {
            Some("disabled") => TlsPolicy::Disabled,
            Some("verify-authority") => TlsPolicy::VerifyAuthority,
            _ => return Err(ConnectionError::Incompatible),
        };
        require_direct_transport(object)?;
        let proxy = None;
        let authority = parse_http_connection_authority(primary, tls, proxy)
            .map_err(|error| authority_denied("definition", error))?;
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
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let secret = self
            .vault
            .lookup(&self.project, handle)
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let credential_headers = credential_headers(&secret)?;
        execute(decision, request, credential_headers).await
    }
}

fn authority_denied(phase: &'static str, error: AuthorityError) -> ConnectionError {
    tracing::warn!(
        phase,
        kind = ?error.kind(),
        error = %error,
        "trusted HTTP connection authority denied"
    );
    ConnectionError::AuthorityDenied
}

fn log_effect_authority_denied(phase: &'static str, error: ConnectionError) -> ConnectionError {
    if matches!(error, ConnectionError::AuthorityDenied) {
        tracing::warn!(phase, "trusted HTTP connection authority denied");
    }
    error
}

fn require_direct_transport(
    definition: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConnectionError> {
    if matches!(
        definition.get("proxy-transport"),
        Some(serde_json::Value::Null)
    ) && matches!(
        definition.get("proxy-authority"),
        None | Some(serde_json::Value::Null)
    ) {
        Ok(())
    } else {
        Err(ConnectionError::Incompatible)
    }
}

/// Require the exact host-bound component and immutable wiring version/hash to
/// be members of the digest-verified format-1 release manifest.
///
/// Shared with the blobstore capability rather than reimplemented there: two
/// spellings of "does this component belong to this release" could disagree,
/// and the one that was wrong would authorize an effect.
pub(crate) fn authorize_release_closure(
    manifest: &ServingManifest,
    invocation: &ConnectionInvocation,
    snapshot: &ConnectionEffectSnapshot,
) -> Result<(), ConnectionError> {
    let digest = ArtifactHash::parse(invocation.component_digest.clone())
        .map_err(|_| ConnectionError::AttestationInvalid)?;
    let Some(((component, interface_version), operation)) = snapshot
        .component
        .as_ref()
        .zip(snapshot.interface_version.as_ref())
        .zip(snapshot.operation.as_ref())
    else {
        return Err(ConnectionError::AttestationInvalid);
    };
    let component = manifest.components.iter().find(|candidate| {
        candidate.package_id == invocation.package_id
            && candidate.component == *component
            && candidate.interface_version == *interface_version
            && candidate.digest == digest
    });
    let operation_admitted = component
        .and_then(|component| component.operations.get(operation))
        .is_some_and(|operation| operation.registered_operation == snapshot.registered_operation);
    let wiring = ServingWiring {
        package_id: invocation.package_id.clone(),
        wiring_id: invocation.wiring_id.clone(),
        wiring_version: invocation.wiring_version,
        graph_hash: DefinitionHash::parse(snapshot.wiring_hash.clone())
            .map_err(|_| ConnectionError::AttestationInvalid)?,
    };
    if !operation_admitted || !manifest.wirings.contains(&wiring) {
        return Err(ConnectionError::AttestationInvalid);
    }
    Ok(())
}

/// Require the snapshot to carry the wiring hash, component and interface
/// version frozen at candidate admission, and to equal the frozen binding row.
///
/// Shared with the blobstore capability for the reason
/// [`authorize_release_closure`] gives. Two spellings of the candidate rule can
/// disagree, and the wrong one authorizes an effect.
pub(crate) fn authorize_candidate_closure(
    invocation: &ConnectionInvocation,
    snapshot: &ConnectionEffectSnapshot,
    binding: &super::wamn_postgres::CandidateConnectionBinding,
) -> Result<(), ConnectionError> {
    let ConnectionExecutionClosure::Candidate {
        wiring_hash,
        component,
        interface_version,
        ..
    } = &invocation.closure
    else {
        return Err(ConnectionError::AttestationInvalid);
    };
    if snapshot.wiring_hash != *wiring_hash
        || snapshot.component.as_deref() != Some(component.as_str())
        || snapshot.interface_version.as_deref() != Some(interface_version.as_str())
        || !binding.matches_snapshot(snapshot)
    {
        return Err(ConnectionError::AttestationInvalid);
    }
    Ok(())
}

/// The HTTP connection's own descriptor.
///
/// Built PER CALL, never cached in a process-wide cell. `repo-lint`'s
/// per-invocation-client leg refuses deferred-initialisation state anywhere in
/// this file, and it is right to refuse it bluntly: the guard exists so a
/// credentialed HTTP path cannot acquire cross-generation reuse, and a guard
/// that accepts "but mine is only a descriptor" stops guarding. Constructing
/// nine small ownership entries is nothing beside the request it authorizes.
fn http_descriptor() -> ConnectionTypeDescriptor {
    ConnectionTypeDescriptor::http_v1()
}

/// Authorize one connection effect against the descriptor for its type.
///
/// PARAMETERIZED, not copied (wamn-jpxo). The type and contract used to be
/// four hardcoded `"http"`/`HTTP_CONTRACT` literals here. A second capability
/// needs the same authorization, and a forked copy would mint a THIRD reader
/// of one row shape — which is precisely the live defect `wamn-0h0g.21.9`
/// records, where `promote.rs` read the wrapped record and this file
/// pointer-read the top level, so a row satisfying either refused the other.
/// The authorization logic therefore stays one copy with one reader, and the
/// capability arrives as an argument.
///
/// Taking the whole descriptor rather than a `(type, contract)` pair means a
/// caller cannot supply a mismatched pair: there is no way to name HTTP's type
/// beside blobstore's contract.
pub(crate) fn authorize_snapshot(
    snapshot: &ConnectionEffectSnapshot,
    descriptor: &ConnectionTypeDescriptor,
) -> Result<(), ConnectionError> {
    if !snapshot.node_permitted {
        return Err(ConnectionError::AttestationInvalid);
    }
    let Some(requirement) = snapshot.requirement_json.as_ref() else {
        return Err(ConnectionError::Incompatible);
    };
    if !snapshot.binding_active || !snapshot.binding_valid || snapshot.instance_id.is_none() {
        return Err(ConnectionError::Unbound);
    }
    if !snapshot.instance_enabled
        || snapshot.active_generation.is_none()
        || snapshot.active_generation != snapshot.generation
    {
        return Err(ConnectionError::CredentialUnavailable);
    }
    // `requirement_json` is the WHOLE portable record component admission
    // minted — `{component-digest, store-alias, requirement}` — because that is
    // the value `requirement_hash` is the SHA-256 of. The connection SEMANTICS
    // therefore sit one level down, under `requirement`.
    let expected_type = descriptor.requirement_type.as_str();
    let expected_contract = descriptor.contract.as_str();
    if snapshot.requirement_type.as_deref() != Some(expected_type)
        || snapshot.contract.as_deref() != Some(expected_contract)
        || requirement
            .pointer("/requirement/requirement-type")
            .and_then(serde_json::Value::as_str)
            != Some(expected_type)
        || requirement
            .pointer("/requirement/contract")
            .and_then(serde_json::Value::as_str)
            != Some(expected_contract)
    {
        return Err(ConnectionError::Incompatible);
    }
    Ok(())
}

fn credential_headers(secret: &str) -> Result<HashMap<String, String>, ConnectionError> {
    let value: serde_json::Value =
        serde_json::from_str(secret).map_err(|_| ConnectionError::CredentialUnavailable)?;
    let object = value
        .as_object()
        .ok_or(ConnectionError::CredentialUnavailable)?;
    if object.len() != 1 || !object.contains_key("headers") {
        return Err(ConnectionError::CredentialUnavailable);
    }
    object["headers"]
        .as_object()
        .ok_or(ConnectionError::CredentialUnavailable)?
        .iter()
        .map(|(name, value)| {
            let value = value
                .as_str()
                .ok_or(ConnectionError::CredentialUnavailable)?;
            let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ConnectionError::CredentialUnavailable)?;
            if reserved_header(&header) {
                return Err(ConnectionError::CredentialUnavailable);
            }
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| ConnectionError::CredentialUnavailable)?;
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
    request: &Request,
    credentials: HashMap<String, String>,
) -> Result<reqwest::header::HeaderMap, ConnectionError> {
    let mut headers = reqwest::header::HeaderMap::new();
    for header in &request.headers {
        let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| ConnectionError::AuthorityDenied)?;
        if reserved_header(&name) {
            return Err(ConnectionError::AuthorityDenied);
        }
        let value = reqwest::header::HeaderValue::from_bytes(&header.value)
            .map_err(|_| ConnectionError::AuthorityDenied)?;
        headers.append(name, value);
    }
    for (name, value) in credentials {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        headers.insert(name, value);
    }
    if let Some(key) = &request.idempotency_key {
        let value = reqwest::header::HeaderValue::from_str(key)
            .map_err(|_| ConnectionError::Incompatible)?;
        headers.insert("idempotency-key", value);
    }
    inject_trace_context(&mut headers);
    Ok(headers)
}

/// The W3C fields of the effect span, written onto one outbound request.
///
/// A field the guest already sent stays: `components/no-std/http-request`
/// forwards its node context's traceparent with `push_header_unless_present`,
/// and the host adds context rather than rewriting the guest's.
struct TraceContextHeaders<'a> {
    headers: &'a mut reqwest::header::HeaderMap,
}

impl opentelemetry::propagation::Injector for TraceContextHeaders<'_> {
    fn set(&mut self, key: &str, value: String) {
        // The W3C propagator writes `tracestate` even when the context carries
        // none, and an empty field is not a field.
        if value.is_empty() {
            return;
        }
        let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_bytes()) else {
            return;
        };
        if self.headers.contains_key(&name) {
            return;
        }
        let Ok(value) = reqwest::header::HeaderValue::from_str(&value) else {
            return;
        };
        self.headers.insert(name, value);
    }
}

/// [12.6] Put the running `wamn.connection_http` span on the wire.
///
/// Without this the only traceparent leaving the process was whatever the guest
/// happened to forward, so a downstream service parented to the host's caller
/// and never saw the effect at all.
///
/// The span is read through `tracing`, never `opentelemetry::global::tracer`:
/// the fork's `initialize_observability` installs a `tracing-opentelemetry`
/// layer and a propagator but NO global tracer provider, so the global tracer
/// silently answers with a no-op span.
///
/// Public because the `traceproof` gate drives THIS function rather than a
/// copy of it (`wamn-k9ea`): the gate's whole claim is that what crosses the
/// process boundary is what production injects, so a reimplementation there
/// would prove nothing about this one.
pub fn inject_trace_context(headers: &mut reqwest::header::HeaderMap) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut TraceContextHeaders { headers });
    });
}

async fn execute(
    decision: crate::connection_authority::AuthorityDecision,
    request: &Request,
    credentials: HashMap<String, String>,
) -> Result<Response, ConnectionError> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| ConnectionError::Incompatible)?;
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
        TransportDecision::Proxy { .. } => return Err(ConnectionError::Incompatible),
    }
    // A named detail, not the rendered error, for the same reason the authority
    // snapshot uses one: this failure provably dispatched nothing, and
    // `http_outcome` has no other way to tell it apart from a failure in flight.
    // The rendered error stays host-side in the log.
    let client = builder.build().map_err(|error| {
        tracing::warn!(error = %error, "trusted HTTP client construction failed");
        ConnectionError::Transport(HTTP_CLIENT_UNAVAILABLE.to_string())
    })?;
    let mut outbound = client
        .request(method, decision.logical_url.as_ref())
        .headers(headers);
    if let Some(body) = &request.body {
        outbound = outbound.body(body.clone());
    }
    let response = outbound.send().await.map_err(|error| {
        if error.is_timeout() {
            ConnectionError::Timeout
        } else {
            ConnectionError::Transport(error.to_string())
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
    // The status line and headers are already in hand here, so the far side
    // acted. Only the body was lost. A named detail carries that fact to
    // `http_outcome`, which the rendered error cannot.
    let body = response
        .bytes()
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, status, "trusted HTTP response body was lost");
            ConnectionError::Transport(RESPONSE_BODY_LOST.to_string())
        })?
        .to_vec();
    Ok(Response {
        status,
        headers,
        body,
    })
}

pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    http::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

impl HostPlugin for ConnectionHttp {
    fn id(&self) -> &'static str {
        CONNECTION_HTTP_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:connection/http@0.1.0")]),
            exports: HashSet::new(),
        }
    }
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<ConnectionHttp>> {
    ctx.try_get_plugin::<ConnectionHttp>(CONNECTION_HTTP_ID)
}

/// [9.1] The `wamn.connection_http` span over one guest HTTP effect: the shared
/// identity vocabulary, plus the wiring position this component was invoked at.
///
/// Five of those fields COPY what `wamn.component.invoke` already carries one
/// level up, so a reader filtering effect spans directly can say which wiring and
/// which node raised one without walking parents (`wamn-0h0g.24.12`). The
/// package and the occurrence appear on no parent span, and they are what
/// separates two calls that name their wiring and node alike (`wamn-b2m6.2`).
/// `wamn.component_name` appears on no parent span either, and it is the only
/// key on this span that names the executing component the way a person does:
/// `wamn.component` is the pooled instance scope and `wamn.component_digest` is
/// a manifest key (`wamn-b2m6.7`).
/// All eight come from the [`ConnectionInvocation`] the router driver binds
/// before the component runs — the same host-attested record
/// [`ConnectionHttp::send`] authorizes against, never anything the guest sent.
///
/// A pooled instance with no invocation bound holds no such claim and records the
/// wiring keys empty. That send is about to be refused as
/// `ConnectionError::AttestationInvalid`, and an empty value says so where a
/// missing field would look like lost instrumentation.
fn http_span(plugin: &ConnectionHttp, component_id: &str) -> tracing::Span {
    let span = effect_span!(
        "wamn.connection_http",
        EffectIdentity {
            tenant: &plugin.tenant,
            project: &plugin.project,
            component: component_id,
        },
        None,
        effect.operation = "send",
    );
    let invocation = plugin.invocation(component_id);
    record_wiring(
        &span,
        invocation.as_ref().map(|invocation| EffectWiring {
            package_id: &invocation.package_id,
            wiring_id: &invocation.wiring_id,
            wiring_version: invocation.wiring_version,
            node_id: &invocation.node_id,
            occurrence: invocation.occurrence,
            component_digest: &invocation.component_digest,
            component_name: &invocation.component,
            operation: &invocation.operation,
        }),
    );
    span
}

/// Which of the five outcomes one HTTP effect reached.
///
/// The match is exhaustive on purpose. A new `connection-error` case is then a
/// compile error here, and whoever adds it decides what the platform knows
/// about it instead of inheriting a wildcard.
///
/// `Transport` carries four different facts, and the wire enum names none of
/// them. Three of them this file mints itself and therefore recognises:
///
/// * the authority read and the client that never built, which dispatched
///   nothing, so both are refused before dispatch,
/// * the response body the host never finished reading, which is
///   [`EffectOutcome::ResponseLost`], because the status line proves the far
///   side acted and the guest still got nothing.
///
/// The fourth is a rendered `reqwest` error from a request that failed in
/// flight. It records [`EffectOutcome::EffectUncertain`], because the platform
/// sent an attempt and holds no outcome for it. `Timeout` is wrong for it, since
/// it names a deadline that never elapsed, and refused before dispatch is wrong
/// too, since the request left (owner ruling, `wamn-b2m6.3`).
fn http_outcome(result: &Result<Response, ConnectionError>) -> EffectOutcome {
    match result {
        Ok(_) => EffectOutcome::Responded,
        Err(ConnectionError::Timeout) => EffectOutcome::Timeout,
        Err(ConnectionError::Transport(detail))
            if detail.as_str() == AUTHORITY_SNAPSHOT_UNAVAILABLE
                || detail.as_str() == HTTP_CLIENT_UNAVAILABLE =>
        {
            EffectOutcome::RefusedBeforeDispatch
        }
        Err(ConnectionError::Transport(detail)) if detail.as_str() == RESPONSE_BODY_LOST => {
            EffectOutcome::ResponseLost
        }
        Err(ConnectionError::Transport(_)) => EffectOutcome::EffectUncertain,
        Err(
            ConnectionError::Unbound
            | ConnectionError::Incompatible
            | ConnectionError::AuthorityDenied
            | ConnectionError::AttestationInvalid
            | ConnectionError::CredentialUnavailable,
        ) => EffectOutcome::RefusedBeforeDispatch,
    }
}

impl http::Host for ActiveCtx<'_> {
    async fn send(
        &mut self,
        request: Request,
    ) -> wash_runtime::wasmtime::Result<Result<Response, ConnectionError>> {
        let plugin = plugin_of(self)?;
        let span = http_span(&plugin, self.component_id.as_ref());
        // Declared before the effect so it outlives the instrumented future. A
        // send dropped mid-flight records `cancelled` through this guard.
        let mut observed = EffectOutcomeGuard::new(&span);
        let started = std::time::Instant::now();
        let result = plugin
            .send(self.component_id.as_ref(), &request)
            .instrument(span)
            .await;
        record_effect_ms(
            &HTTP_EFFECT_DURATION_MS,
            EFFECT_OPERATION,
            "send",
            &plugin.project,
            started.elapsed(),
        );
        let outcome = http_outcome(&result);
        observed.settle(outcome);
        if let Err(error) = &result {
            let invocation = plugin.invocation(self.component_id.as_ref());
            tracing::warn!(
                error = ?error,
                effect.outcome = outcome.label(),
                wiring_id = invocation.as_ref().map(|value| value.wiring_id.as_str()),
                wiring_version = invocation.as_ref().map(|value| value.wiring_version),
                node_id = invocation.as_ref().map(|value| value.node_id.as_str()),
                occurrence = invocation.as_ref().map(|value| value.occurrence),
                store_alias = request.requirement,
                "trusted HTTP effect failed"
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        EffectiveReleaseId, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION, ServingComponent,
        ServingComponentOperation, ServingRelease,
    };

    use super::*;
    use crate::plugins::effect_span::span_proof::{SpanHarness, expected_attributes};
    use crate::plugins::wamn_postgres::WamnPostgresConfig;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn invocation() -> ConnectionInvocation {
        ConnectionInvocation {
            package_id: "package_a".to_string(),
            wiring_id: "orders".to_string(),
            wiring_version: 3,
            node_id: "notify".to_string(),
            occurrence: 2,
            component_digest: digest('a'),
            component: "notifier".to_string(),
            operation: "orders:notify/dispatch@1.0.0".to_string(),
            closure: ConnectionExecutionClosure::Released,
        }
    }

    fn offline_postgres() -> WamnPostgres {
        WamnPostgres::new(WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1,
            statement_timeout_ms: 1,
            row_limit: 1,
        })
        .expect("an offline postgres plugin does not open a connection")
    }

    #[tokio::test]
    async fn revoked_invocation_refuses_send_and_can_be_rebound() {
        let plugin = ConnectionHttp::new(
            Arc::new(offline_postgres()),
            Arc::new(WamnCredentials::empty()),
            "tenant-a",
            "project-a",
            Vec::<AllowedHost>::new().into(),
            None,
        );
        let component_id = "component-store-7";
        let invocation = invocation();
        plugin
            .bind_invocation(component_id, invocation.clone())
            .expect("the fresh store accepts its invocation");

        plugin.revoke_invocation(component_id);

        let request = Request {
            requirement: "manager".to_string(),
            method: "POST".to_string(),
            path_and_query: "/notify".to_string(),
            headers: Vec::new(),
            body: None,
            idempotency_key: None,
        };
        assert!(matches!(
            plugin.send(component_id, &request).await,
            Err(ConnectionError::AttestationInvalid)
        ));
        plugin
            .bind_invocation(component_id, invocation.clone())
            .expect("revocation makes the store safe to bind again");
        assert_eq!(plugin.invocation(component_id), Some(invocation));
    }

    fn snapshot() -> ConnectionEffectSnapshot {
        ConnectionEffectSnapshot {
            wiring_hash: digest('b'),
            component: Some("http-request".to_string()),
            interface_version: Some("0.1".to_string()),
            operation: Some("package-a:orders/notify@1.0.0".to_string()),
            registered_operation: Some("package-a:orders/notify@1.0.0".to_string()),
            requirement_json: Some(serde_json::json!({
                "component-digest": digest('a'),
                "store-alias": "erp",
                "requirement": {
                    "requirement-type": "http",
                    "contract": HTTP_CONTRACT,
                },
            })),
            requirement_hash: Some(digest('d')),
            node_permitted: true,
            binding_active: true,
            binding_valid: true,
            instance_id: Some("manager".to_string()),
            validation_hash: Some(digest('e')),
            requirement_type: Some("http".to_string()),
            contract: Some(HTTP_CONTRACT.to_string()),
            instance_enabled: true,
            active_generation: Some(7),
            instance_revision: Some(2),
            generation: Some(7),
            definition: Some(serde_json::json!({})),
            definition_hash: Some(digest('c')),
            credential_handle: Some("manager-v7".to_string()),
        }
    }

    fn manifest() -> ServingManifest {
        let invocation = invocation();
        let snapshot = snapshot();
        ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: "tenant-a".to_string(),
                effective_release_id: EffectiveReleaseId::new(4).unwrap(),
                environment: "prod".to_string(),
                packages: BTreeSet::from([PackageCoordinate::new("package_a", "1.0.0").unwrap()]),
            },
            components: BTreeSet::from([ServingComponent {
                package_id: invocation.package_id.clone(),
                component: snapshot.component.expect("component"),
                interface_version: snapshot.interface_version.expect("interface version"),
                digest: ArtifactHash::parse(invocation.component_digest)
                    .expect("fixture artifact hash is canonical"),
                operations: BTreeMap::from([(
                    snapshot.operation.expect("operation"),
                    ServingComponentOperation {
                        fresh_only: false,
                        registered_operation: snapshot.registered_operation,
                        dependencies: Vec::new(),
                        statements: BTreeMap::new(),
                    },
                )]),
            }]),
            wirings: BTreeSet::from([ServingWiring {
                package_id: invocation.package_id,
                wiring_id: invocation.wiring_id,
                wiring_version: invocation.wiring_version,
                graph_hash: DefinitionHash::parse(snapshot.wiring_hash)
                    .expect("fixture definition hash is canonical"),
            }]),
            attachments: BTreeMap::new(),
            registrations: BTreeMap::new(),
        }
    }

    #[test]
    fn release_closure_requires_the_exact_component_and_wiring_version_hash() {
        let manifest = manifest();
        let invocation = invocation();
        let snapshot = snapshot();
        authorize_release_closure(&manifest, &invocation, &snapshot)
            .expect("the fixture closure is exactly the released one");

        let mut wrong_digest = invocation.clone();
        wrong_digest.component_digest = digest('d');
        assert!(matches!(
            authorize_release_closure(&manifest, &wrong_digest, &snapshot),
            Err(ConnectionError::AttestationInvalid)
        ));

        let mut wrong_wiring_hash = snapshot;
        wrong_wiring_hash.wiring_hash = digest('e');
        assert!(matches!(
            authorize_release_closure(&manifest, &invocation, &wrong_wiring_hash),
            Err(ConnectionError::AttestationInvalid)
        ));
    }

    /// `HTTP_CONTRACT` is pinned by conformance as an exact source line, while
    /// `authorize_snapshot` now compares against the descriptor. If those two
    /// spellings ever drift, authorization would silently start demanding a
    /// contract nothing mints.
    #[test]
    fn the_pinned_contract_literal_matches_the_descriptor() {
        assert_eq!(http_descriptor().contract, HTTP_CONTRACT);
        assert_eq!(http_descriptor().requirement_type, "http");
    }

    /// The parameter must actually discriminate. A snapshot carrying a valid
    /// HTTP authority chain is still refused when authorized against another
    /// capability's descriptor — otherwise the argument is decoration, and one
    /// capability could authorize another's binding.
    #[test]
    fn a_valid_http_snapshot_is_refused_against_another_capabilitys_descriptor() {
        let valid = snapshot();
        authorize_snapshot(&valid, &http_descriptor()).expect("valid against its own descriptor");
        assert!(
            matches!(
                authorize_snapshot(&valid, &ConnectionTypeDescriptor::blobstore_v1()),
                Err(ConnectionError::Incompatible)
            ),
            "an HTTP binding must not authorize as blobstore"
        );
    }

    #[test]
    fn component_grain_snapshot_refuses_each_missing_authority_layer() {
        let valid = snapshot();
        authorize_snapshot(&valid, &http_descriptor())
            .expect("the fixture snapshot carries every authority layer");

        let mut missing_node = valid.clone();
        missing_node.node_permitted = false;
        assert!(matches!(
            authorize_snapshot(&missing_node, &http_descriptor()),
            Err(ConnectionError::AttestationInvalid)
        ));

        let mut inactive_binding = valid.clone();
        inactive_binding.binding_active = false;
        assert!(matches!(
            authorize_snapshot(&inactive_binding, &http_descriptor()),
            Err(ConnectionError::Unbound)
        ));

        let mut stale_generation = valid.clone();
        stale_generation.generation = Some(6);
        assert!(matches!(
            authorize_snapshot(&stale_generation, &http_descriptor()),
            Err(ConnectionError::CredentialUnavailable)
        ));

        let mut wrong_contract = valid;
        wrong_contract.contract = Some("wamn:connection/postgres@0.1.0".to_string());
        assert!(matches!(
            authorize_snapshot(&wrong_contract, &http_descriptor()),
            Err(ConnectionError::Incompatible)
        ));
    }

    #[test]
    fn guest_headers_cannot_spoof_the_host_owned_idempotency_key() {
        let spoofed = Request {
            requirement: "manager".to_string(),
            method: "POST".to_string(),
            path_and_query: "/notify".to_string(),
            headers: vec![Header {
                name: "idempotency-key".to_string(),
                value: b"guest".to_vec(),
            }],
            body: None,
            idempotency_key: Some("host".to_string()),
        };
        assert!(matches!(
            outbound_headers(&spoofed, HashMap::new()),
            Err(ConnectionError::AuthorityDenied)
        ));

        let admitted = Request {
            headers: Vec::new(),
            ..spoofed
        };
        let headers = outbound_headers(&admitted, HashMap::new()).expect("valid headers");
        assert_eq!(headers["idempotency-key"], "host");
    }

    fn effect_request(headers: Vec<Header>) -> Request {
        Request {
            requirement: "manager".to_string(),
            method: "POST".to_string(),
            path_and_query: "/notify".to_string(),
            headers,
            body: None,
            idempotency_key: None,
        }
    }

    /// A `tracing` subscriber whose OTel layer gives the effect span a real,
    /// injectable span context — the only way to observe what leaves the host.
    fn with_effect_span<T>(body: impl FnOnce() -> T) -> (T, opentelemetry::trace::SpanContext) {
        use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("connection-http-test")),
        );
        let _guard = tracing::subscriber::set_default(subscriber);
        let span = tracing::info_span!("wamn.connection_http");
        span.in_scope(|| {
            let context = span.context().span().span_context().clone();
            (body(), context)
        })
    }

    /// The effect the guest performed is what downstream must parent to. Before
    /// `wamn-0h0g.12.6` nothing was injected at all, so the only traceparent on
    /// the wire was whatever the guest forwarded from ingress.
    #[test]
    fn the_effect_span_is_injected_onto_the_outbound_request() {
        let (headers, span_context) =
            with_effect_span(|| outbound_headers(&effect_request(Vec::new()), HashMap::new()));
        let headers = headers.expect("valid headers");
        assert_eq!(
            headers["traceparent"],
            format!(
                "00-{}-{}-01",
                span_context.trace_id(),
                span_context.span_id()
            )
            .as_str(),
            "the running effect span must be the outbound parent"
        );
        assert!(
            !headers.contains_key("tracestate"),
            "an empty tracestate is not a field"
        );
    }

    /// `components/no-std/http-request` forwards its node context's traceparent
    /// with `push_header_unless_present`; the host stays coherent with it.
    #[test]
    fn a_guest_supplied_traceparent_is_not_overwritten() {
        const GUEST: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let request = effect_request(vec![Header {
            name: "traceparent".to_string(),
            value: GUEST.as_bytes().to_vec(),
        }]);
        let (headers, _) = with_effect_span(|| outbound_headers(&request, HashMap::new()));
        let headers = headers.expect("valid headers");
        assert_eq!(headers["traceparent"], GUEST);
    }

    fn offline_plugin() -> ConnectionHttp {
        ConnectionHttp::new(
            Arc::new(offline_postgres()),
            Arc::new(WamnCredentials::empty()),
            "tenant-a",
            "project-a",
            Vec::<AllowedHost>::new().into(),
            None,
        )
    }

    /// An effect span names the wiring position the call originated at, the
    /// package that executed it, and the digest the release manifest keys the
    /// executing component on. A trace attributes the effect to one caller
    /// without walking parents.
    #[test]
    fn an_effect_span_names_its_originating_wiring_position_and_executing_package() {
        let plugin = offline_plugin();
        let component_id = "component-store-7";
        plugin
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its invocation");
        let component_digest = digest('a');

        let harness = SpanHarness::install("connection-http-span-test");
        drop(http_span(&plugin, component_id));

        assert_eq!(
            harness.attributes("wamn.connection_http"),
            expected_attributes(&[
                ("effect.operation", "send"),
                ("wamn.tenant", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", component_id),
                ("wamn.package_id", "package_a"),
                ("wamn.wiring_id", "orders"),
                ("wamn.wiring_version", "3"),
                ("wamn.node_id", "notify"),
                ("wamn.occurrence", "2"),
                ("wamn.component_digest", component_digest.as_str()),
                ("wamn.component_name", "notifier"),
                ("wamn.operation", "orders:notify/dispatch@1.0.0"),
            ]),
        );
    }

    /// The SAME wiring id, version and node in ANOTHER package, visited at
    /// another occurrence. A wiring id is package-scoped, so this is a different
    /// wiring wearing the same name.
    fn second_package_invocation() -> ConnectionInvocation {
        ConnectionInvocation {
            package_id: "package_b".to_string(),
            occurrence: 5,
            ..invocation()
        }
    }

    /// THE TEST THAT WOULD HAVE CAUGHT THE DEFECT. One pooled instance serves
    /// two invocations in turn, which is what the driver does. Without the
    /// package and the occurrence the two calls record byte-identical spans, so
    /// neither effect can be attributed to the call that raised it.
    #[test]
    fn two_calls_to_the_same_capability_from_different_wirings_record_distinguishable_spans() {
        let plugin = offline_plugin();
        let component_id = "component-store-7";
        let component_digest = digest('a');
        let shared = |package: &str, occurrence: &str| {
            expected_attributes(&[
                ("effect.operation", "send"),
                ("wamn.tenant", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", component_id),
                ("wamn.package_id", package),
                ("wamn.wiring_id", "orders"),
                ("wamn.wiring_version", "3"),
                ("wamn.node_id", "notify"),
                ("wamn.occurrence", occurrence),
                ("wamn.component_digest", component_digest.as_str()),
                ("wamn.component_name", "notifier"),
                ("wamn.operation", "orders:notify/dispatch@1.0.0"),
            ])
        };

        let harness = SpanHarness::install("connection-http-two-wirings-test");
        plugin
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its first invocation");
        drop(http_span(&plugin, component_id));
        plugin.revoke_invocation(component_id);
        plugin
            .bind_invocation(component_id, second_package_invocation())
            .expect("revocation makes the store safe to bind again");
        drop(http_span(&plugin, component_id));

        let spans = harness.every_span("wamn.connection_http");
        assert_eq!(
            spans,
            vec![shared("package_a", "2"), shared("package_b", "5")],
            "each effect names the call that raised it",
        );
        assert_ne!(spans[0], spans[1], "two callers must not read alike");
    }

    /// A pooled instance with no invocation bound holds no wiring claim — the
    /// send it is about to serve is refused as `AttestationInvalid`. The eight
    /// keys are still emitted, empty, so "this effect was raised outside a node
    /// walk" never reads as "the enrichment was dropped".
    #[test]
    fn an_unbound_component_records_the_wiring_keys_empty() {
        let plugin = offline_plugin();
        let component_id = "component-store-7";

        let harness = SpanHarness::install("connection-http-unbound-span-test");
        drop(http_span(&plugin, component_id));

        assert_eq!(
            harness.attributes("wamn.connection_http"),
            expected_attributes(&[
                ("effect.operation", "send"),
                ("wamn.tenant", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", component_id),
                ("wamn.package_id", ""),
                ("wamn.wiring_id", ""),
                ("wamn.wiring_version", "0"),
                ("wamn.node_id", ""),
                ("wamn.occurrence", "0"),
                ("wamn.component_digest", ""),
                ("wamn.component_name", ""),
                ("wamn.operation", ""),
            ]),
        );
    }

    /// One effect span, opened and settled the way `send` settles it.
    fn observed_http_span(
        plugin: &ConnectionHttp,
        component_id: &str,
        result: &Result<Response, ConnectionError>,
    ) {
        let span = http_span(plugin, component_id);
        let mut observed = EffectOutcomeGuard::new(&span);
        observed.settle(http_outcome(result));
    }

    /// THE TEST THAT WOULD HAVE CAUGHT THE OLD BEHAVIOUR. Every failure of this
    /// surface rendered as one word, "refused", and the span carried no outcome
    /// at all. A timeout then read as a refusal before dispatch, which tells a
    /// reader the request never left and the far side did nothing with it. The
    /// platform does not know that.
    #[test]
    fn a_timeout_never_reads_as_a_refusal_before_dispatch() {
        assert_eq!(
            http_outcome(&Err(ConnectionError::Timeout)),
            EffectOutcome::Timeout,
        );
        assert_ne!(
            http_outcome(&Err(ConnectionError::Timeout)),
            http_outcome(&Err(ConnectionError::AttestationInvalid)),
            "a timeout and a wiring refusal must not read alike",
        );
    }

    /// A wiring failure refuses ONE effect, and so does every other refusal the
    /// platform decides before dispatch. The command that reached this component
    /// committed before the effect ran, and the observation claims nothing about
    /// it: it says the platform declined to dispatch, and stops there.
    #[test]
    fn a_wiring_failure_names_the_refused_effect_and_not_the_command() {
        for error in [
            ConnectionError::Unbound,
            ConnectionError::AttestationInvalid,
            ConnectionError::AuthorityDenied,
            ConnectionError::CredentialUnavailable,
            ConnectionError::Incompatible,
        ] {
            let outcome = http_outcome(&Err(error));
            assert_eq!(outcome, EffectOutcome::RefusedBeforeDispatch);
            assert_eq!(outcome.label(), "refused-before-dispatch");
        }
    }

    /// Two of the three transport details this file mints dispatched nothing,
    /// so both are refusals. The authority read never reached a client, and the
    /// client never built.
    #[test]
    fn the_two_details_that_dispatched_nothing_are_refused_before_dispatch() {
        for detail in [AUTHORITY_SNAPSHOT_UNAVAILABLE, HTTP_CLIENT_UNAVAILABLE] {
            assert_eq!(
                http_outcome(&Err(ConnectionError::Transport(detail.to_owned()))),
                EffectOutcome::RefusedBeforeDispatch,
                "{detail} dispatched nothing",
            );
        }
    }

    /// THE TEST THAT WOULD HAVE CAUGHT THE MAPPING THE OWNER OVERTURNED
    /// SECOND. A body lost after the status line arrived was recorded as
    /// effect-uncertain, which claims an unknown. The far side acted and the
    /// answer proves it. The guest received nothing, so it is not responded
    /// either. The remedy is to re-read, not to resend.
    #[test]
    fn a_lost_response_never_reads_as_effect_uncertain() {
        let lost = http_outcome(&Err(ConnectionError::Transport(
            RESPONSE_BODY_LOST.to_owned(),
        )));
        assert_eq!(lost, EffectOutcome::ResponseLost);
        assert_ne!(
            lost,
            EffectOutcome::EffectUncertain,
            "the far side acted, so nothing about it is unknown",
        );
        assert_ne!(
            lost,
            EffectOutcome::Responded,
            "the guest received no response",
        );
    }

    /// THE TEST THAT WOULD HAVE CAUGHT THE MAPPING THE OWNER OVERTURNED. A
    /// transport failure in flight was recorded as a timeout, which names a
    /// deadline that never elapsed. The attempt went out and the platform holds
    /// no outcome for it, which is effect-uncertain.
    #[test]
    fn a_transport_failure_in_flight_never_reads_as_a_timeout() {
        let in_flight = http_outcome(&Err(ConnectionError::Transport(
            "connection reset by peer".to_owned(),
        )));
        assert_eq!(in_flight, EffectOutcome::EffectUncertain);
        assert_ne!(
            in_flight,
            EffectOutcome::Timeout,
            "a failure in flight must not claim a deadline elapsed",
        );
        assert_ne!(
            in_flight,
            EffectOutcome::RefusedBeforeDispatch,
            "a failure in flight must not claim the request never left",
        );
    }

    /// A timed-out effect is one whole span value. `effect.outcome` is part of
    /// the shape, so a reader filtering on it finds every effect this surface
    /// ran and never a subset.
    #[test]
    fn a_timed_out_effect_span_carries_the_timeout_outcome() {
        let plugin = offline_plugin();
        let component_id = "component-store-7";
        plugin
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its invocation");
        let component_digest = digest('a');

        let harness = SpanHarness::install("connection-http-outcome-test");
        observed_http_span(&plugin, component_id, &Err(ConnectionError::Timeout));

        assert_eq!(
            harness.attributes("wamn.connection_http"),
            expected_attributes(&[
                ("effect.operation", "send"),
                ("effect.outcome", "timeout"),
                ("wamn.tenant", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", component_id),
                ("wamn.package_id", "package_a"),
                ("wamn.wiring_id", "orders"),
                ("wamn.wiring_version", "3"),
                ("wamn.node_id", "notify"),
                ("wamn.occurrence", "2"),
                ("wamn.component_digest", component_digest.as_str()),
                ("wamn.component_name", "notifier"),
                ("wamn.operation", "orders:notify/dispatch@1.0.0"),
            ]),
        );
    }

    /// THE TEST THAT WOULD HAVE CAUGHT THE GAP. An effect span could name the
    /// executing component only by `wamn.component`, which is the pooled
    /// instance scope, and by `wamn.component_digest`, which is a manifest key.
    /// It named the call only by `effect.operation`, the capability method. So
    /// nothing on the span answered "which component, running which node
    /// operation". Both keys are part of the whole value, and the two operation
    /// keys stand side by side here because they answer different questions.
    #[test]
    fn an_effect_span_names_the_executing_component_and_the_node_operation() {
        let plugin = offline_plugin();
        let component_id = "component-store-7";
        plugin
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its invocation");
        let component_digest = digest('a');

        let harness = SpanHarness::install("connection-http-component-operation-test");
        drop(http_span(&plugin, component_id));

        let attributes = harness.attributes("wamn.connection_http");
        assert_eq!(
            attributes,
            expected_attributes(&[
                ("effect.operation", "send"),
                ("wamn.tenant", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", component_id),
                ("wamn.package_id", "package_a"),
                ("wamn.wiring_id", "orders"),
                ("wamn.wiring_version", "3"),
                ("wamn.node_id", "notify"),
                ("wamn.occurrence", "2"),
                ("wamn.component_digest", component_digest.as_str()),
                ("wamn.component_name", "notifier"),
                ("wamn.operation", "orders:notify/dispatch@1.0.0"),
            ]),
        );

        let value = |key: &str| {
            attributes
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, recorded)| recorded.clone())
                .expect("the span carries the key")
        };
        assert_ne!(
            value("effect.operation"),
            value("wamn.operation"),
            "the capability method is not the node operation",
        );
        assert_ne!(
            value("wamn.component"),
            value("wamn.component_name"),
            "the pooled instance scope is not the component identity",
        );
    }

    /// The SAME component at the SAME wiring position, entered at another node
    /// operation. Every other coordinate is held fixed on purpose, so the
    /// operation is the only thing that can separate the two recorded spans.
    fn second_operation_invocation() -> ConnectionInvocation {
        ConnectionInvocation {
            operation: "orders:notify/escalate@1.0.0".to_string(),
            ..invocation()
        }
    }

    /// One pooled instance serves two operations of one component in turn. The
    /// wiring position, the package, the occurrence and the digest are equal
    /// across the pair, so before the operation reached the span the two effects
    /// recorded byte-identical values and neither could be attributed.
    #[test]
    fn two_effects_under_different_node_operations_of_one_component_record_distinguishable_spans() {
        let plugin = offline_plugin();
        let component_id = "component-store-7";
        let component_digest = digest('a');
        let shared = |operation: &str| {
            expected_attributes(&[
                ("effect.operation", "send"),
                ("wamn.tenant", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", component_id),
                ("wamn.package_id", "package_a"),
                ("wamn.wiring_id", "orders"),
                ("wamn.wiring_version", "3"),
                ("wamn.node_id", "notify"),
                ("wamn.occurrence", "2"),
                ("wamn.component_digest", component_digest.as_str()),
                ("wamn.component_name", "notifier"),
                ("wamn.operation", operation),
            ])
        };

        let harness = SpanHarness::install("connection-http-two-operations-test");
        plugin
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its first invocation");
        drop(http_span(&plugin, component_id));
        plugin.revoke_invocation(component_id);
        plugin
            .bind_invocation(component_id, second_operation_invocation())
            .expect("revocation makes the store safe to bind again");
        drop(http_span(&plugin, component_id));

        let spans = harness.every_span("wamn.connection_http");
        assert_eq!(
            spans,
            vec![
                shared("orders:notify/dispatch@1.0.0"),
                shared("orders:notify/escalate@1.0.0"),
            ],
            "each effect names the node operation that raised it",
        );
        assert_ne!(
            spans[0], spans[1],
            "two node operations of one component must not read alike",
        );
    }
}
