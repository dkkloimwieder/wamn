//! Environment-bound HTTP for ordinary `wamn:node` components.
//!
//! The host binds the exact wiring, node position, occurrence and component
//! digest before invoking a pooled component. The guest names only a store
//! alias; the database must resolve that alias at the component grain and the
//! mounted format-2 manifest must contain both the exact wiring version/hash and
//! component tuple. No run, plan, frame or effect-ledger fact participates.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::Instrument as _;
use wamn_catalog::{ServingComponent, ServingManifest, ServingWiring};
use wamn_flow::node_contract::normalize_portable_http_target;
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
    EFFECT_OPERATION, EffectIdentity, HTTP_EFFECT_DURATION_MS, effect_span, record_effect_ms,
};
use crate::release_manifest::ReleaseManifestWeld;

use super::wamn_credentials::WamnCredentials;
use super::wamn_postgres::{ConnectionEffectLookup, ConnectionEffectSnapshot, WamnPostgres};

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

/// Host-attested identity of one component invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInvocation {
    pub wiring_id: String,
    pub wiring_version: u32,
    pub node_id: String,
    pub occurrence: u32,
    pub component_digest: String,
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
                && !invocation.wiring_id.is_empty()
                && invocation.wiring_version > 0
                && !invocation.node_id.is_empty()
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "connection-http-invocation-invalid"
        );
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
        let release = self
            .release
            .as_deref()
            .ok_or(ConnectionError::AttestationInvalid)?;
        let manifest = release.manifest();
        if manifest.release.tenant_id != self.tenant.as_ref() {
            return Err(ConnectionError::AttestationInvalid);
        }
        let catalog_version = i32::try_from(manifest.release.catalog_version)
            .map_err(|_| ConnectionError::AttestationInvalid)?;
        let wiring_version = i32::try_from(invocation.wiring_version)
            .map_err(|_| ConnectionError::AttestationInvalid)?;
        let snapshot = self
            .postgres
            .connection_effect_snapshot(
                component_id,
                &self.project,
                &self.tenant,
                &ConnectionEffectLookup {
                    catalog_id: &manifest.release.catalog_id,
                    catalog_version,
                    environment: &manifest.release.environment,
                    wiring_id: &invocation.wiring_id,
                    wiring_version,
                    node_id: &invocation.node_id,
                    component_digest: &invocation.component_digest,
                    store_alias: &request.requirement,
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
        authorize_release_closure(manifest, &invocation, &snapshot)?;
        authorize_snapshot(&snapshot)?;

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
/// be members of the digest-verified format-2 release manifest.
fn authorize_release_closure(
    manifest: &ServingManifest,
    invocation: &ConnectionInvocation,
    snapshot: &ConnectionEffectSnapshot,
) -> Result<(), ConnectionError> {
    let component = snapshot
        .component
        .as_ref()
        .zip(snapshot.interface_version.as_ref())
        .map(|(component, interface_version)| ServingComponent {
            component: component.clone(),
            interface_version: interface_version.clone(),
            digest: invocation.component_digest.clone(),
        });
    let wiring = ServingWiring {
        wiring_id: invocation.wiring_id.clone(),
        wiring_version: invocation.wiring_version,
        graph_hash: snapshot.wiring_hash.clone(),
    };
    if component.is_none_or(|component| !manifest.components.contains(&component))
        || !manifest.wirings.contains(&wiring)
    {
        return Err(ConnectionError::AttestationInvalid);
    }
    Ok(())
}

fn authorize_snapshot(snapshot: &ConnectionEffectSnapshot) -> Result<(), ConnectionError> {
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
    Ok(headers)
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
    let client = builder
        .build()
        .map_err(|error| ConnectionError::Transport(error.to_string()))?;
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
    let body = response
        .bytes()
        .await
        .map_err(|error| ConnectionError::Transport(error.to_string()))?
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

impl http::Host for ActiveCtx<'_> {
    async fn send(
        &mut self,
        request: Request,
    ) -> wash_runtime::wasmtime::Result<Result<Response, ConnectionError>> {
        let plugin = plugin_of(self)?;
        let span = effect_span!(
            "wamn.connection_http",
            EffectIdentity {
                tenant: &plugin.tenant,
                project: &plugin.project,
                component: self.component_id.as_ref(),
            },
            None,
            effect.operation = "send",
        );
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
        if let Err(error) = &result {
            let invocation = plugin.invocation(self.component_id.as_ref());
            tracing::warn!(
                error = ?error,
                wiring_id = invocation.as_ref().map(|value| value.wiring_id.as_str()),
                wiring_version = invocation.as_ref().map(|value| value.wiring_version),
                node_id = invocation.as_ref().map(|value| value.node_id.as_str()),
                occurrence = invocation.as_ref().map(|value| value.occurrence),
                store_alias = request.requirement,
                "trusted HTTP effect refused"
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    use wamn_catalog::{SERVING_MANIFEST_FORMAT_VERSION, ServingRelease};

    use super::*;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn invocation() -> ConnectionInvocation {
        ConnectionInvocation {
            wiring_id: "orders".to_string(),
            wiring_version: 3,
            node_id: "notify".to_string(),
            occurrence: 2,
            component_digest: digest('a'),
        }
    }

    fn snapshot() -> ConnectionEffectSnapshot {
        ConnectionEffectSnapshot {
            wiring_hash: digest('b'),
            component: Some("http-request".to_string()),
            interface_version: Some("0.1".to_string()),
            requirement_json: Some(serde_json::json!({
                "requirement-type": "http",
                "contract": HTTP_CONTRACT,
            })),
            node_permitted: true,
            binding_active: true,
            binding_valid: true,
            instance_id: Some("manager".to_string()),
            requirement_type: Some("http".to_string()),
            contract: Some(HTTP_CONTRACT.to_string()),
            instance_enabled: true,
            active_generation: Some(7),
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
                catalog_id: "catalog-a".to_string(),
                catalog_version: 4,
                environment: "prod".to_string(),
            },
            components: BTreeSet::from([ServingComponent {
                component: snapshot.component.expect("component"),
                interface_version: snapshot.interface_version.expect("interface version"),
                digest: invocation.component_digest,
            }]),
            wirings: BTreeSet::from([ServingWiring {
                wiring_id: invocation.wiring_id,
                wiring_version: invocation.wiring_version,
                graph_hash: snapshot.wiring_hash,
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
        assert_eq!(
            authorize_release_closure(&manifest, &invocation, &snapshot),
            Ok(())
        );

        let mut wrong_digest = invocation.clone();
        wrong_digest.component_digest = digest('d');
        assert_eq!(
            authorize_release_closure(&manifest, &wrong_digest, &snapshot),
            Err(ConnectionError::AttestationInvalid)
        );

        let mut wrong_wiring_hash = snapshot;
        wrong_wiring_hash.wiring_hash = digest('e');
        assert_eq!(
            authorize_release_closure(&manifest, &invocation, &wrong_wiring_hash),
            Err(ConnectionError::AttestationInvalid)
        );
    }

    #[test]
    fn component_grain_snapshot_refuses_each_missing_authority_layer() {
        let valid = snapshot();
        assert_eq!(authorize_snapshot(&valid), Ok(()));

        let mut missing_node = valid.clone();
        missing_node.node_permitted = false;
        assert_eq!(
            authorize_snapshot(&missing_node),
            Err(ConnectionError::AttestationInvalid)
        );

        let mut inactive_binding = valid.clone();
        inactive_binding.binding_active = false;
        assert_eq!(
            authorize_snapshot(&inactive_binding),
            Err(ConnectionError::Unbound)
        );

        let mut stale_generation = valid.clone();
        stale_generation.generation = Some(6);
        assert_eq!(
            authorize_snapshot(&stale_generation),
            Err(ConnectionError::CredentialUnavailable)
        );

        let mut wrong_contract = valid;
        wrong_contract.contract = Some("wamn:connection/postgres@0.1.0".to_string());
        assert_eq!(
            authorize_snapshot(&wrong_contract),
            Err(ConnectionError::Incompatible)
        );
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
}
