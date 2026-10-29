//! Environment-bound HTTP for ordinary `wamn:node` components.
//!
//! The host binds the exact entry (a route, or a wiring node position and
//! occurrence) and the component digest before invoking a pooled component. The
//! guest names only a store alias; the database must resolve that alias at the
//! component grain and the mounted manifest must contain the exact route or
//! wiring version/hash and the component tuple. No run, plan, frame or effect
//! record participates.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;
use tracing::Instrument as _;
use wamn_catalog::{ArtifactHash, ConnectionTypeDescriptor, ServingComponent, ServingManifest};
use wamn_execution_contract::EffectOutcome;
use wamn_execution_contract::node_contract::normalize_portable_http_target;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Accessor, Linker};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::connection_authority::{
    AuthorityDecision, AuthorityError, HttpConnectionAuthority, TlsPolicy, TokioDnsResolver,
    bound_http_target, parse_http_connection_authority, resolve_http_request,
};
use crate::plugins::effect_span::{
    EFFECT_OPERATION, EffectEvidence, EffectIdentity, EffectOutcomeGuard, EffectWiring,
    HTTP_EFFECT_DURATION_MS, effect_span, record_effect_ms, record_wiring,
};
use wamn_engine::release_manifest::LoadedRelease;

use super::wamn_postgres::{
    CandidateBindingWorld, ConnectionEffectLookup, ConnectionEffectSnapshot, ConnectionEntryLookup,
    WamnPostgres, WiringLookup,
};
use crate::connection_credentials::ConnectionCredentials;

mod network_policy;
pub mod transport;

use network_policy::ConnectionNetworkPolicy;

use transport::{ClientScope, ConnectionScope, HttpTransport};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "connection-http-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::connection::http::{self, ConnectionError, Header, Request, Response};

pub const CONNECTION_HTTP_ID: &str = "wamn-connection-http";
/// The exact contract spelling the conformance test pins against the
/// descriptor. Only that test reads it.
#[cfg(test)]
const HTTP_CONTRACT: &str = "wamn:connection/http@0.1.0";
const AUTHORITY_SNAPSHOT_UNAVAILABLE: &str = "connection-authority-unavailable";
/// The transport detail for unavailable client capacity or construction.
/// These failures dispatch nothing, including refusal by a shared quota.
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
    /// The root component, unchanged by nested calls.
    pub origin: ConnectionOrigin,
    /// Where the invocation entered the release, unchanged by nested calls.
    pub entry: InvocationEntry,
    pub package_id: String,
    pub component_digest: String,
    pub component: String,
    pub operation: String,
    pub closure: ConnectionExecutionClosure,
    /// Optional bounded failure evidence owned by this invocation.
    pub effects: Option<EffectEvidence>,
}

impl ConnectionInvocation {
    /// The span coordinates of this invocation. A route has no wiring
    /// position, so it records the wiring keys empty.
    pub(crate) fn effect_wiring(&self) -> EffectWiring<'_> {
        let (wiring_id, wiring_version, node_id, occurrence) = match self.entry.wiring() {
            None => ("", 0, "", 0),
            Some(position) => (
                position.wiring_id.as_str(),
                position.wiring_version,
                position.node_id.as_str(),
                position.occurrence,
            ),
        };
        EffectWiring {
            package_id: &self.package_id,
            wiring_id,
            wiring_version,
            node_id,
            occurrence,
            component_digest: &self.component_digest,
            component_name: &self.component,
            operation: &self.operation,
        }
    }
}

/// Where one invocation entered the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationEntry {
    /// A route called the origin export once. The origin names the route.
    Route,
    /// One node occurrence of an exact wiring version.
    Wiring(WiringPosition),
}

impl InvocationEntry {
    /// The wiring position, or `None` for a route.
    pub fn wiring(&self) -> Option<&WiringPosition> {
        match self {
            Self::Route => None,
            Self::Wiring(position) => Some(position),
        }
    }
}

/// One node occurrence of an exact wiring version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringPosition {
    /// The immutable graph hash frozen on the run.
    pub wiring_hash: String,
    /// The wiring can belong to a package that imports the root component.
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub node_id: String,
    pub occurrence: u32,
}

/// Host-attested root identity for an effect's exact dependency path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionOrigin {
    pub package_id: String,
    pub component_digest: String,
    pub component: String,
    pub interface_version: String,
    pub operation: String,
}

/// Host-owned authority closure for one component invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionExecutionClosure {
    /// A digest-verified serving manifest owns the component and wiring.
    Released,
    /// Private admission froze an exact candidate wiring and binding world.
    Candidate {
        /// The release the candidate resolves under, as its manifest digest.
        manifest_digest: String,
        environment: String,
        wiring_hash: String,
        component: String,
        interface_version: String,
        binding_world: Arc<CandidateBindingWorld>,
    },
}

/// Names the tenant and project only: the rest carries credential and
/// transport state that must not reach a log.
impl std::fmt::Debug for ConnectionHttp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionHttp")
            .field("env_key", &self.env_key)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// Host-owned services and claims for the trusted HTTP effect.
pub struct ConnectionHttp {
    postgres: Arc<WamnPostgres>,
    transport: Arc<HttpTransport>,
    vault: Arc<ConnectionCredentials>,
    env_key: Box<str>,
    project: Box<str>,
    allowed_hosts: Arc<[AllowedHost]>,
    /// Reader 2 of the four the loaded release enumerates (`wamn-0h0g.15.100`): the ONE
    /// loaded, digest-verified serving manifest, held by reference and never
    /// loaded, parsed or digest-verified here. The bytes stay immutable for the
    /// process lifetime. This field has no TTL, refresh, or invalidation.
    ///
    /// `None` in a process that was given no release (gates, benches, the pool's
    /// own fixtures). Such a process cannot attest a wiring/component closure,
    /// so it cannot authorize a connection.
    release: Option<Arc<LoadedRelease>>,
    /// Component-store owner id to the invocation currently using that pooled
    /// instance. The production driver binds before `handler.run` and revokes
    /// before returning the instance to the pool.
    invocations: std::sync::RwLock<HashMap<String, ConnectionInvocation>>,
    oauth2: Oauth2Cache,
}

impl ConnectionHttp {
    pub fn new(
        postgres: Arc<WamnPostgres>,
        transport: Arc<HttpTransport>,
        vault: Arc<ConnectionCredentials>,
        env_key: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Option<Arc<LoadedRelease>>,
    ) -> Self {
        let project: Box<str> = project.into();
        postgres.set_connection_credentials(Arc::clone(&vault), &project);
        Self {
            postgres,
            transport,
            vault,
            env_key: env_key.into(),
            project,
            allowed_hosts,
            release,
            invocations: std::sync::RwLock::new(HashMap::new()),
            oauth2: Oauth2Cache {
                entries: Mutex::new(HashMap::new()),
                now: Instant::now,
            },
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
        let entry_valid = match &invocation.entry {
            InvocationEntry::Route => !matches!(
                invocation.closure,
                ConnectionExecutionClosure::Candidate { .. }
            ),
            InvocationEntry::Wiring(position) => {
                !position.package_id.is_empty()
                    && !position.wiring_id.is_empty()
                    && position.wiring_version > 0
                    && wamn_catalog::DefinitionHash::parse(&position.wiring_hash).is_ok()
                    && !position.node_id.is_empty()
            }
        };
        anyhow::ensure!(
            !component_id.is_empty()
                && !invocation.package_id.is_empty()
                && entry_valid
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "connection-http-invocation-invalid"
        );
        if let ConnectionExecutionClosure::Candidate {
            manifest_digest,
            environment,
            wiring_hash,
            component,
            interface_version,
            ..
        } = &invocation.closure
        {
            let graph = wiring_hash.strip_prefix("sha256:").unwrap_or_default();
            anyhow::ensure!(
                !manifest_digest.is_empty()
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

    /// Exercise the host dispatch path without constructing a component store.
    #[cfg(feature = "test-util")]
    pub async fn send_for_test(
        &self,
        component_id: &str,
        requirement: &str,
        method: &str,
    ) -> Result<u16, String> {
        self.send(
            component_id,
            &Request {
                requirement: requirement.to_owned(),
                method: method.to_owned(),
                path_and_query: "/probe".into(),
                headers: Vec::new(),
                body: None,
                idempotency_key: None,
            },
        )
        .await
        .map(|response| response.status)
        .map_err(|error| format!("{error:?}"))
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
        let release = match &invocation.closure {
            ConnectionExecutionClosure::Released => Some(
                self.release
                    .as_deref()
                    .ok_or(ConnectionError::AttestationInvalid)?,
            ),
            ConnectionExecutionClosure::Candidate { .. } => None,
        };
        let (manifest_digest, environment, candidate_binding) = match (&invocation.closure, release)
        {
            (ConnectionExecutionClosure::Released, Some(release)) => {
                let manifest = release.manifest();
                if release.scope().env_key != self.env_key.as_ref()
                    || !manifest
                        .release
                        .packages
                        .iter()
                        .any(|package| package.package_id() == invocation.package_id.as_str())
                {
                    return Err(ConnectionError::AttestationInvalid);
                }
                (
                    release.release().manifest_digest.as_str(),
                    release.scope().environment.as_str(),
                    None,
                )
            }
            (
                ConnectionExecutionClosure::Candidate {
                    manifest_digest,
                    environment,
                    binding_world,
                    ..
                },
                None,
            ) => (
                manifest_digest.as_str(),
                environment.as_str(),
                Some(
                    binding_world
                        .binding(&invocation.component_digest, &request.requirement)
                        .ok_or(ConnectionError::AttestationInvalid)?,
                ),
            ),
            _ => return Err(ConnectionError::AttestationInvalid),
        };
        let entry = entry_lookup(&invocation)?;
        let snapshot = self
            .postgres
            .connection_effect_snapshot(
                component_id,
                &self.project,
                &self.env_key,
                &ConnectionEffectLookup {
                    package_id: &invocation.package_id,
                    entry,
                    origin_package_id: &invocation.origin.package_id,
                    origin_component_digest: &invocation.origin.component_digest,
                    origin_component: &invocation.origin.component,
                    origin_interface_version: &invocation.origin.interface_version,
                    origin_operation: &invocation.origin.operation,
                    operation: &invocation.operation,
                    manifest_digest,
                    environment,
                    component_digest: &invocation.component_digest,
                    store_alias: &request.requirement,
                    candidate_binding,
                },
            )
            .await
            .map_err(|error| {
                let database_error = error
                    .downcast_ref::<tokio_postgres::Error>()
                    .and_then(tokio_postgres::Error::as_db_error);
                tracing::warn!(
                    error = %error,
                    sqlstate = database_error.map(|error| error.code().code()),
                    database_message = database_error.map(tokio_postgres::error::DbError::message),
                    "trusted HTTP connection authority snapshot failed"
                );
                ConnectionError::Transport(AUTHORITY_SNAPSHOT_UNAVAILABLE.to_string())
            })?
            .ok_or(ConnectionError::AttestationInvalid)?;
        match (&invocation.closure, release) {
            (ConnectionExecutionClosure::Released, Some(release)) => {
                authorize_release_closure(release.manifest(), &invocation, &snapshot)?;
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
        authorize_method(snapshot.read_only, &request.method)?;

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
        let proxy = None;
        let authority = parse_http_connection_authority(primary, tls, proxy)
            .map_err(|error| authority_denied("definition", &error))?;
        let decision = resolve_http_request(
            &authority,
            &target,
            &self.allowed_hosts,
            &ConnectionNetworkPolicy,
            &TokioDnsResolver,
        )
        .await
        .map_err(|error| authority_denied("request", &error))?;

        let handle = snapshot
            .credential_handle
            .as_deref()
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let scope = ClientScope {
            connection: ConnectionScope {
                env_key: self.env_key.clone(),
                project: self.project.clone(),
                environment: environment.into(),
                instance: snapshot
                    .instance_id
                    .as_deref()
                    .ok_or(ConnectionError::Unbound)?
                    .into(),
            },
            package: invocation.package_id.clone().into(),
            component_digest: invocation.component_digest.clone().into(),
            requirement: request.requirement.clone().into(),
            binding_hash: snapshot
                .validation_hash
                .as_deref()
                .ok_or(ConnectionError::Unbound)?
                .into(),
            definition_hash: snapshot
                .definition_hash
                .as_deref()
                .ok_or(ConnectionError::CredentialUnavailable)?
                .into(),
            generation: snapshot
                .generation
                .ok_or(ConnectionError::CredentialUnavailable)?,
        };
        if let Some(name) = &snapshot.connection_name
            && !self
                .vault
                .connection_enabled(&self.project, name)
                .await
                .map_err(|_| ConnectionError::CredentialUnavailable)?
        {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let external_system_id = self
            .vault
            .external_system_id(&self.project, handle)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let mut response = self
            .execute_authenticated(scope, &authority, decision, request, handle)
            .await?;
        response.external_system = external_system_id;
        Ok(response)
    }

    async fn execute_authenticated(
        &self,
        scope: ClientScope,
        authority: &HttpConnectionAuthority,
        decision: AuthorityDecision,
        request: &Request,
        handle: &str,
    ) -> Result<Response, ConnectionError> {
        let secret = self
            .vault
            .read_connection(&self.project, handle)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let object = credential_object(&secret)?;
        let Some(oauth2) = object.get("oauth2") else {
            let headers = credential_headers(
                request
                    .headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case("content-type"))
                    .map(|header| header.value.as_slice()),
                &secret,
                &request.method,
                &decision.logical_url,
                request.body.as_deref().unwrap_or_default(),
            )?;
            return execute(&self.transport, scope, decision, request, headers).await;
        };
        let credential: Oauth2Credential = serde_json::from_value(oauth2.clone())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if credential.client_id.is_empty() || credential.client_secret.is_empty() {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let key = (self.project.clone(), handle.into(), scope.generation);
        let token = self
            .oauth2_token(&scope, authority, &credential, &key)
            .await?;
        let first = execute(
            &self.transport,
            scope.clone(),
            decision.clone(),
            request,
            [("authorization".into(), token)].into(),
        )
        .await?;
        if first.status != 401 {
            return Ok(first);
        }
        self.oauth2
            .entries
            .lock()
            .expect("OAuth2 cache lock")
            .remove(&key);
        let Ok(token) = self
            .oauth2_token(&scope, authority, &credential, &key)
            .await
        else {
            return Ok(first);
        };
        let second = execute(
            &self.transport,
            scope,
            decision,
            request,
            [("authorization".into(), token)].into(),
        )
        .await?;
        if second.status == 401 {
            self.oauth2
                .entries
                .lock()
                .expect("OAuth2 cache lock")
                .remove(&key);
        }
        Ok(second)
    }

    async fn oauth2_token(
        &self,
        scope: &ClientScope,
        authority: &HttpConnectionAuthority,
        credential: &Oauth2Credential,
        key: &Oauth2CacheKey,
    ) -> Result<String, ConnectionError> {
        let (target, endpoint) = bound_http_target(authority, &credential.token_url)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let now = (self.oauth2.now)();
        {
            let mut entries = self.oauth2.entries.lock().expect("OAuth2 cache lock");
            entries.retain(|_, (_, expires)| *expires > now);
            if let Some((token, _)) = entries.get(key) {
                return Ok(token.clone());
            }
        }
        let mut decision = resolve_http_request(
            authority,
            &target,
            &self.allowed_hosts,
            &ConnectionNetworkPolicy,
            &TokioDnsResolver,
        )
        .await
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
        decision.logical_url = endpoint;
        let token =
            fetch_oauth2_token(&self.transport, scope.clone(), &decision, credential).await?;
        let header = format!("Bearer {}", token.access_token);
        // Missing or short expiry permits this send only. Start at fetch time,
        // so time spent obtaining the token never extends its cached lifetime.
        if let Some(expires) = token
            .expires_in
            .and_then(|seconds| seconds.checked_sub(60))
            .and_then(|seconds| now.checked_add(Duration::from_secs(seconds)))
            .filter(|expires| *expires > (self.oauth2.now)())
        {
            self.oauth2
                .entries
                .lock()
                .expect("OAuth2 cache lock")
                .insert(key.clone(), (header.clone(), expires));
        }
        Ok(header)
    }
}

type Oauth2CacheKey = (Box<str>, Box<str>, i64);

struct Oauth2Cache {
    entries: Mutex<HashMap<Oauth2CacheKey, (String, Instant)>>,
    now: fn() -> Instant,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Oauth2Credential {
    token_url: String,
    client_id: String,
    client_secret: String,
    scope: Option<String>,
}

#[derive(serde::Deserialize)]
struct Oauth2Token {
    access_token: String,
    token_type: String,
    expires_in: Option<u64>,
}

async fn fetch_oauth2_token(
    transport: &HttpTransport,
    scope: ClientScope,
    decision: &AuthorityDecision,
    credential: &Oauth2Credential,
) -> Result<Oauth2Token, ConnectionError> {
    use base64::Engine as _;

    // RFC 6749 section 2.3.1 form-encodes both Basic components first.
    let encode =
        |value: &str| url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
    let basic = base64::engine::general_purpose::STANDARD.encode(format!(
        "{}:{}",
        encode(&credential.client_id),
        encode(&credential.client_secret),
    ));
    let body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", "client_credentials");
        if let Some(scope) = &credential.scope {
            form.append_pair("scope", scope);
        }
        form.finish()
    };
    let request = hyper::Request::builder()
        .method("POST")
        .uri(decision.logical_url.as_ref())
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", format!("Basic {basic}"))
        .body(body.as_bytes())
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
    let response = transport
        .execute(scope, decision, request)
        .await
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
    if response.status() != hyper::StatusCode::OK {
        return Err(ConnectionError::CredentialUnavailable);
    }
    let token: Oauth2Token = serde_json::from_slice(response.body())
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
    if !token.token_type.eq_ignore_ascii_case("bearer")
        || token.access_token.is_empty()
        || !token
            .access_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
    {
        return Err(ConnectionError::CredentialUnavailable);
    }
    Ok(token)
}

fn authorize_method(read_only: bool, method: &str) -> Result<(), ConnectionError> {
    if read_only && !matches!(method, "GET" | "HEAD") {
        return Err(ConnectionError::AuthorityDenied);
    }
    Ok(())
}

fn authority_denied(phase: &'static str, error: &AuthorityError) -> ConnectionError {
    tracing::warn!(
        phase,
        kind = ?error.error_type(),
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

/// Require the executing operation to be reachable from the attested wiring root.
///
/// Shared with the blobstore capability rather than reimplemented there: two
/// spellings of "does this component belong to this release" could disagree,
/// and the one that was wrong would authorize an effect.
/// The snapshot entry for one bound invocation: its route, or its wiring node.
///
/// Shared with the blobstore capability for the reason
/// [`authorize_release_closure`] gives.
pub(crate) fn entry_lookup(
    invocation: &ConnectionInvocation,
) -> Result<ConnectionEntryLookup<'_>, ConnectionError> {
    Ok(match &invocation.entry {
        InvocationEntry::Route => ConnectionEntryLookup {
            package_id: &invocation.origin.package_id,
            wiring: None,
        },
        InvocationEntry::Wiring(position) => ConnectionEntryLookup {
            package_id: &position.package_id,
            wiring: Some(WiringLookup {
                wiring_id: &position.wiring_id,
                wiring_version: i32::try_from(position.wiring_version)
                    .map_err(|_| ConnectionError::AttestationInvalid)?,
                node_id: &position.node_id,
            }),
        },
    })
}

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
    if component != &invocation.component || operation != &invocation.operation {
        return Err(ConnectionError::AttestationInvalid);
    }
    let component = manifest.components.iter().find(|candidate| {
        candidate.package_id == invocation.package_id
            && candidate.descriptor.component == *component
            && candidate.descriptor.interface_version == *interface_version
            && candidate.descriptor.component_digest == digest.as_str()
    });
    let operation_admitted = component
        .and_then(|component| component.operations.get(operation))
        .is_some_and(|operation| operation.registered_operation == snapshot.registered_operation);
    let origin = &invocation.origin;
    let entry_released = match &invocation.entry {
        InvocationEntry::Route => {
            snapshot.wiring_hash.is_none()
                && manifest.routes.iter().any(|route| {
                    route.package_id == origin.package_id
                        && route.component == origin.component
                        && route.operation == origin.operation
                })
        }
        InvocationEntry::Wiring(position) => {
            snapshot.node_permitted
                && snapshot.wiring_hash.as_deref() == Some(position.wiring_hash.as_str())
        }
    };
    let root = manifest.components.iter().find(|component| {
        component.package_id == origin.package_id
            && component.descriptor.component == origin.component
            && component.descriptor.interface_version == origin.interface_version
            && component.descriptor.component_digest.as_str() == origin.component_digest
    });
    if !operation_admitted
        || !entry_released
        || !root
            .is_some_and(|root| released_operation_reachable(root, &origin.operation, invocation))
    {
        return Err(ConnectionError::AttestationInvalid);
    }
    Ok(())
}

/// The invocation must be the released entry itself. An application's
/// components compose at build into one component, so a call inside it keeps
/// the entry's invocation identity.
fn released_operation_reachable(
    root: &ServingComponent,
    root_operation: &str,
    invocation: &ConnectionInvocation,
) -> bool {
    root.operations.contains_key(root_operation)
        && root.package_id == invocation.package_id
        && root.descriptor.component == invocation.component
        && root.descriptor.component_digest.as_str() == invocation.component_digest
        && root_operation == invocation.operation
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
    if invocation.package_id != invocation.origin.package_id
        || invocation.component_digest != invocation.origin.component_digest
        || invocation.component != invocation.origin.component
        || invocation.operation != invocation.origin.operation
        || snapshot.operation.as_deref() != Some(invocation.operation.as_str())
        || snapshot.wiring_hash.as_ref() != Some(wiring_hash)
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
/// Each call builds its descriptor and authorizes the effect again.
/// The shared transport retains clients only under the complete isolation key.
/// `repo-lint` guards that key and refuses unscoped client retention.
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
        || snapshot.pinned_generation.or(snapshot.active_generation) != snapshot.generation
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HmacCredential {
    algorithm: String,
    secret: String,
    header: String,
}

fn credential_headers(
    content_type: Option<&[u8]>,
    secret: &str,
    method: &str,
    url: &str,
    body: &[u8],
) -> Result<HashMap<String, String>, ConnectionError> {
    credential_headers_with(
        content_type,
        secret,
        method,
        url,
        body,
        oauth1_request_values,
    )
}

fn credential_headers_with(
    content_type: Option<&[u8]>,
    secret: &str,
    method: &str,
    url: &str,
    body: &[u8],
    request_values: fn() -> Result<(String, u64), ConnectionError>,
) -> Result<HashMap<String, String>, ConnectionError> {
    let object = credential_object(secret)?;
    if let Some(oauth1) = object.get("oauth1") {
        let credential: Oauth1Credential = serde_json::from_value(oauth1.clone())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if [
            &credential.consumer_key,
            &credential.consumer_secret,
            &credential.token,
            &credential.token_secret,
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let (nonce, timestamp) = request_values()?;
        let form = content_type
            .and_then(|value| std::str::from_utf8(value).ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            });
        let body_params = if form {
            url::form_urlencoded::parse(body).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let body_params = body_params
            .iter()
            .map(|(name, value)| (name.as_ref(), value.as_ref()))
            .collect::<Vec<_>>();
        let header = oauth1_header(&credential, method, url, &body_params, &nonce, timestamp)?;
        return Ok([("authorization".to_string(), header)].into());
    }
    if let Some(hmac) = object.get("hmac") {
        use hmac::{Hmac, KeyInit as _, Mac as _};

        let credential: HmacCredential = serde_json::from_value(hmac.clone())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let header = reqwest::header::HeaderName::from_bytes(credential.header.as_bytes())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if credential.algorithm != "sha256"
            || credential.secret.is_empty()
            || reserved_header(&header)
        {
            return Err(ConnectionError::CredentialUnavailable);
        }
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(credential.secret.as_bytes())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        return Ok([(credential.header, signature)].into());
    }
    object
        .get("headers")
        .and_then(serde_json::Value::as_object)
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

fn credential_object(
    secret: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, ConnectionError> {
    let object: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(secret).map_err(|_| ConnectionError::CredentialUnavailable)?;
    if object.len() != 1 {
        return Err(ConnectionError::CredentialUnavailable);
    }
    Ok(object)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Oauth1Credential {
    consumer_key: String,
    consumer_secret: String,
    token: String,
    token_secret: String,
    realm: Option<String>,
}

fn oauth1_request_values() -> Result<(String, u64), ConnectionError> {
    use ring::rand::SecureRandom as _;

    // A fresh 128-bit nonce for every dispatch, including retries.
    let mut nonce = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ConnectionError::CredentialUnavailable)?
        .as_secs();
    Ok((hex::encode(nonce), timestamp))
}

fn oauth1_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    encoded
}

fn oauth1_base_string(method: &str, url: &url::Url, params: &[(&str, &str)]) -> String {
    let mut params: Vec<_> = url
        .query()
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                percent_encoding::percent_decode_str(name).decode_utf8_lossy(),
                percent_encoding::percent_decode_str(value).decode_utf8_lossy(),
            )
        })
        .map(|(name, value)| (oauth1_encode(&name), oauth1_encode(&value)))
        .chain(
            params
                .iter()
                .map(|(name, value)| (oauth1_encode(name), oauth1_encode(value))),
        )
        .filter(|(name, _)| name != "oauth_signature")
        .collect();
    params.sort_unstable();
    let normalized = params
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    // Url normalizes the scheme, host, default port and empty HTTP path.
    let base_uri = &url[url::Position::BeforeScheme..url::Position::AfterPath];
    format!(
        "{}&{}&{}",
        oauth1_encode(&method.to_ascii_uppercase()),
        oauth1_encode(base_uri),
        oauth1_encode(&normalized)
    )
}

fn oauth1_signature(credential: &Oauth1Credential, base: &str) -> String {
    use base64::Engine as _;

    let key = format!(
        "{}&{}",
        oauth1_encode(&credential.consumer_secret),
        oauth1_encode(&credential.token_secret)
    );
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(ring::hmac::sign(&key, base.as_bytes()))
}

fn oauth1_header(
    credential: &Oauth1Credential,
    method: &str,
    url: &str,
    body_params: &[(&str, &str)],
    nonce: &str,
    timestamp: u64,
) -> Result<String, ConnectionError> {
    let url = url::Url::parse(url).map_err(|_| ConnectionError::Incompatible)?;
    let timestamp = timestamp.to_string();
    let mut params = vec![
        ("oauth_consumer_key", credential.consumer_key.as_str()),
        ("oauth_token", credential.token.as_str()),
        ("oauth_signature_method", "HMAC-SHA256"),
        ("oauth_timestamp", timestamp.as_str()),
        ("oauth_nonce", nonce),
        ("oauth_version", "1.0"),
    ];
    let signature_params = params
        .iter()
        .copied()
        .chain(body_params.iter().copied())
        .collect::<Vec<_>>();
    let base = oauth1_base_string(method, &url, &signature_params);
    let signature = oauth1_signature(credential, &base);
    params.push(("oauth_signature", &signature));
    let mut fields = Vec::new();
    if let Some(realm) = &credential.realm {
        // NetSuite expects the realm verbatim, with HTTP quoted-string escaping.
        let realm = realm.replace('\\', "\\\\").replace('"', "\\\"");
        fields.push(format!("realm=\"{realm}\""));
    }
    fields.extend(
        params
            .iter()
            .map(|(name, value)| format!("{}=\"{}\"", oauth1_encode(name), oauth1_encode(value))),
    );
    let header = format!("OAuth {}", fields.join(", "));
    reqwest::header::HeaderValue::from_str(&header)
        .map_err(|_| ConnectionError::CredentialUnavailable)?;
    Ok(header)
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
            | "x-http-method-override"
            | "x-http-method"
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
        if matches!(name.as_str(), "x-http-method-override" | "x-http-method") {
            continue;
        }
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
/// A field the guest already sent stays: `apps/platform/no-std/http-request`
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
/// the runtime's `initialize_observability` installs a `tracing-opentelemetry`
/// layer and a propagator but NO global tracer provider, so the global tracer
/// silently answers with a no-op span.
///
/// Public because the `trace-test` gate drives THIS function rather than a
/// copy of it (`wamn-k9ea`): the gate's whole claim is that what crosses the
/// process boundary is what production injects, so a reimplementation there
/// does not test this one.
pub fn inject_trace_context(headers: &mut reqwest::header::HeaderMap) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut TraceContextHeaders { headers });
    });
}

async fn execute(
    transport: &HttpTransport,
    scope: ClientScope,
    decision: crate::connection_authority::AuthorityDecision,
    request: &Request,
    credentials: HashMap<String, String>,
) -> Result<Response, ConnectionError> {
    if request
        .body
        .as_ref()
        .is_some_and(|body| body.len() > transport::MAX_BODY_BYTES)
    {
        return Err(ConnectionError::Incompatible);
    }
    // Bound guest-controlled fields before copying them into a HeaderMap.
    // The transport repeats these bounds after credentials and trace injection.
    let header_bytes = request.headers.iter().try_fold(0usize, |total, header| {
        total
            .checked_add(header.name.len())?
            .checked_add(header.value.len())
    });
    if request.headers.len() > transport::MAX_HEADERS
        || header_bytes.is_none_or(|size| size > transport::MAX_HEADER_BYTES)
        || request
            .idempotency_key
            .as_ref()
            .is_some_and(|key| key.len() > transport::MAX_HEADER_BYTES)
    {
        return Err(ConnectionError::Incompatible);
    }
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| ConnectionError::Incompatible)?;
    let headers = outbound_headers(request, credentials)
        .map_err(|error| log_effect_authority_denied("outbound-headers", error))?;
    let body = request.body.as_deref().unwrap_or_default();
    let mut outbound = hyper::Request::builder()
        .method(method)
        .uri(decision.logical_url.as_ref())
        .body(body)
        .map_err(|_| ConnectionError::Incompatible)?;
    *outbound.headers_mut() = headers;
    let response = transport
        .execute(scope, &decision, outbound)
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "trusted HTTP transport failed");
            if error.is_before_dispatch() {
                ConnectionError::Transport(HTTP_CLIENT_UNAVAILABLE.to_string())
            } else if error.is_response_lost() {
                ConnectionError::Transport(RESPONSE_BODY_LOST.to_string())
            } else if error.is_timeout() {
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
    let body = response.into_body().to_vec();
    Ok(Response {
        external_system: None,
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
            env_key: &plugin.env_key,
            project: &plugin.project,
            component: component_id,
        },
        None,
        effect.operation = "send",
    );
    let invocation = plugin.invocation(component_id);
    record_wiring(
        &span,
        invocation.as_ref().map(ConnectionInvocation::effect_wiring),
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
/// * the authority read and unavailable client capacity, which dispatched
///   nothing, so both are refused before dispatch,
/// * the response body the host never finished reading, which is
///   [`EffectOutcome::ResponseLost`], because the status line shows that the far
///   side acted and the guest still got nothing.
///
/// The fourth is a rendered transport error from a request that failed in
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

impl http::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> http::HostWithStore<T> for SharedCtx {
    async fn send(
        accessor: &Accessor<T, Self>,
        request: Request,
    ) -> wash_runtime::wasmtime::Result<Result<Response, ConnectionError>> {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let mut ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                wamn_engine::invocation_trace::invocation_trace(&mut ctx),
            ))
        })?;
        trace
            .run(async move {
                let span = http_span(&plugin, &component_id);
                // Declared before the effect so it outlives the instrumented future. A
                // send dropped mid-flight records `cancelled` through this guard.
                let evidence = plugin
                    .invocation(&component_id)
                    .and_then(|invocation| invocation.effects);
                let mut observed = EffectOutcomeGuard::new(&span, evidence);
                let started = std::time::Instant::now();
                let result = plugin.send(&component_id, &request).instrument(span).await;
                record_effect_ms(
                    &HTTP_EFFECT_DURATION_MS,
                    EFFECT_OPERATION,
                    "send",
                    &plugin.project,
                    started.elapsed(),
                );
                let outcome = http_outcome(&result);
                observed.settle(outcome, result.is_err());
                if let Err(error) = &result {
                    let invocation = plugin.invocation(&component_id);
                    let wiring = invocation.as_ref().map(ConnectionInvocation::effect_wiring);
                    tracing::warn!(
                        error = ?error,
                        effect.outcome = outcome.label(),
                        wiring_id = wiring.map(|value| value.wiring_id),
                        wiring_version = wiring.map(|value| value.wiring_version),
                        node_id = wiring.map(|value| value.node_id),
                        occurrence = wiring.map(|value| value.occurrence),
                        store_alias = request.requirement,
                        "trusted HTTP effect failed"
                    );
                }
                Ok(result)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    mod oauth2_tests;
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        OperationType, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION,
        ServingComponentOperation, ServingRelease, ServingRoute,
    };

    use super::*;
    use crate::plugins::effect_span::span_tests::{SpanHarness, expected_attributes};
    use crate::plugins::wamn_postgres::WamnPostgresConfig;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn invocation() -> ConnectionInvocation {
        ConnectionInvocation {
            origin: ConnectionOrigin {
                package_id: "package_a".to_string(),
                component_digest: digest('a'),
                component: "notifier".to_string(),
                interface_version: "0.1".to_string(),
                operation: "orders:notify/dispatch@1.0.0".to_string(),
            },
            entry: crate::plugins::connection_http::InvocationEntry::Wiring(
                crate::plugins::connection_http::WiringPosition {
                    package_id: "package_a".to_string(),
                    wiring_hash: fixture_wiring_hash(),
                    wiring_id: "orders".to_string(),
                    wiring_version: 3,
                    node_id: "notify".to_string(),
                    occurrence: 2,
                },
            ),
            package_id: "package_a".to_string(),
            component_digest: digest('a'),
            component: "notifier".to_string(),
            operation: "orders:notify/dispatch@1.0.0".to_string(),
            closure: ConnectionExecutionClosure::Released,
            effects: None,
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
            Arc::new(HttpTransport::new().expect("HTTP transport")),
            Arc::new(ConnectionCredentials::empty()),
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

    /// The hash of the fixture wiring's graph, the one the manifest carries.
    fn fixture_wiring_hash() -> String {
        wamn_catalog::fixture_graph("orders", 3)
            .wiring_hash()
            .as_str()
            .to_owned()
    }

    fn snapshot() -> ConnectionEffectSnapshot {
        ConnectionEffectSnapshot {
            connection_name: None,
            read_only: false,
            wiring_hash: Some(fixture_wiring_hash()),
            component: Some("notifier".to_string()),
            interface_version: Some("0.1".to_string()),
            operation: Some("orders:notify/dispatch@1.0.0".to_string()),
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
            pinned_generation: None,
            instance_revision: Some(2),
            generation: Some(7),
            definition: Some(serde_json::json!({})),
            definition_hash: Some(digest('c')),
            credential_handle: Some("manager-v7".to_string()),
        }
    }

    fn position(invocation: &mut ConnectionInvocation) -> &mut WiringPosition {
        let InvocationEntry::Wiring(position) = &mut invocation.entry else {
            panic!("the fixture invocation enters through a wiring");
        };
        position
    }

    fn manifest() -> ServingManifest {
        let invocation = invocation();
        let snapshot = snapshot();
        ServingManifest {
            composed_entry: None,
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("package_a", "1.0.0").unwrap()]),
            },
            components: BTreeSet::from([ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                descriptor: wamn_catalog::ComponentDescriptor::named(
                    snapshot.component.clone().expect("component"),
                    snapshot
                        .interface_version
                        .clone()
                        .expect("interface version"),
                    invocation.component_digest.clone(),
                ),
                package_id: invocation.package_id.clone(),
                operations: BTreeMap::from([(
                    snapshot.operation.expect("operation"),
                    ServingComponentOperation {
                        committed_result_schema: None,
                        permissions: snapshot.registered_operation.iter().cloned().collect(),
                        registered_operation: snapshot.registered_operation,
                        statements: BTreeMap::new(),
                    },
                )]),
            }]),
            routes: BTreeSet::new(),
            attachments: BTreeMap::new(),
            host_routes: BTreeSet::new(),
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
        wrong_wiring_hash.wiring_hash = Some(digest('e'));
        assert!(matches!(
            authorize_release_closure(&manifest, &invocation, &wrong_wiring_hash),
            Err(ConnectionError::AttestationInvalid)
        ));
    }

    #[test]
    fn a_route_entry_is_authorized_only_by_a_released_route() {
        let mut manifest = manifest();
        let mut invocation = invocation();
        invocation.entry = InvocationEntry::Route;
        let mut snapshot = snapshot();
        snapshot.wiring_hash = None;
        assert!(
            matches!(
                authorize_release_closure(&manifest, &invocation, &snapshot),
                Err(ConnectionError::AttestationInvalid)
            ),
            "a release with no route for the origin refuses"
        );

        manifest.routes.insert(ServingRoute {
            package_id: invocation.origin.package_id.clone(),
            component: invocation.origin.component.clone(),
            operation: invocation.origin.operation.clone(),
            type_: OperationType::Command,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
            canonicalization: None,
        });
        authorize_release_closure(&manifest, &invocation, &snapshot)
            .expect("a released route authorizes its origin export");

        let mut with_wiring = snapshot;
        with_wiring.wiring_hash = Some(digest('b'));
        assert!(
            matches!(
                authorize_release_closure(&manifest, &invocation, &with_wiring),
                Err(ConnectionError::AttestationInvalid)
            ),
            "a route entry never resolves a wiring row"
        );
    }

    #[test]
    fn a_route_entry_binds_released_only_and_looks_up_no_wiring() {
        let plugin = offline_plugin();
        let mut route = invocation();
        route.entry = InvocationEntry::Route;
        let lookup = entry_lookup(&route).expect("a route has a lookup entry");
        assert_eq!(lookup.package_id, route.origin.package_id);
        assert!(lookup.wiring.is_none());
        plugin
            .bind_invocation("released-route", route.clone())
            .expect("a released route binds");

        route.closure = ConnectionExecutionClosure::Candidate {
            manifest_digest: digest('c'),
            environment: "prod".to_string(),
            wiring_hash: digest('b'),
            component: "notifier".to_string(),
            interface_version: "0.1".to_string(),
            binding_world: Arc::new(
                CandidateBindingWorld::from_json(serde_json::json!([])).expect("empty world"),
            ),
        };
        plugin
            .bind_invocation("candidate-route", route)
            .expect_err("a candidate closure is a wiring admission, never a route");
    }

    #[test]
    fn release_membership_alone_is_not_an_entry_grant() {
        let mut manifest = manifest();
        let mut invocation = invocation();
        let mut snapshot = snapshot();
        authorize_release_closure(&manifest, &invocation, &snapshot)
            .expect("the released entry is authorized");

        let mut unrelated = manifest.components.first().expect("root").clone();
        unrelated.package_id = "package_b".to_string();
        unrelated.descriptor.component = "unrelated".to_string();
        unrelated.descriptor.component_digest = digest('e');
        manifest
            .release
            .packages
            .insert(PackageCoordinate::new("package_b", "1.0.0").expect("unrelated package"));
        invocation.package_id.clone_from(&unrelated.package_id);
        invocation
            .component
            .clone_from(&unrelated.descriptor.component);
        invocation
            .component_digest
            .clone_from(&unrelated.descriptor.component_digest);
        snapshot.component = Some(unrelated.descriptor.component.clone());
        manifest.components.insert(unrelated);
        assert!(
            matches!(
                authorize_release_closure(&manifest, &invocation, &snapshot),
                Err(ConnectionError::AttestationInvalid)
            ),
            "release membership alone is not an entry grant"
        );
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
    fn hmac_credential_signs_exact_body_bytes_with_rfc_4231_vector() {
        // RFC 4231 test case 2: the expected value is independent of this signer.
        let secret = r#"{"hmac":{"algorithm":"sha256","secret":"Jefe","header":"x-signature"}}"#;
        let body = b"what do ya want for nothing?";
        let credentials =
            credential_headers(None, secret, "POST", "https://example.com/notify", body)
                .expect("HMAC credential");
        assert_eq!(
            credentials["x-signature"],
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_ne!(
            credentials,
            credential_headers(
                None,
                secret,
                "POST",
                "https://example.com/notify",
                b"what do ya want for nothing?\n"
            )
            .unwrap()
        );
        assert_eq!(
            credential_headers(None, secret, "POST", "https://example.com/notify", b"").unwrap()["x-signature"],
            "sha256=923598ca6d64af2a5dba79dcd021a8a0fe5c5f557519adaaf0ad532d4506dd30"
        );
        let request = Request {
            body: Some(body.to_vec()),
            ..effect_request(Vec::new())
        };
        let headers = outbound_headers(&request, credentials).expect("signed headers");
        assert!(headers.contains_key("x-signature"));
        assert!(
            request.headers.is_empty(),
            "the guest supplied no credential"
        );
        assert_eq!(request.body.as_deref(), Some(body.as_slice()));
    }

    const OAUTH1_SECRET: &str = r#"{"oauth1":{"consumer-key":"consumer","consumer-secret":"consumer/secret&","token":"token","token-secret":"token secret+","realm":"123456"}}"#;
    const OAUTH1_URL: &str =
        "https://123456.suitetalk.api.netsuite.com/services/rest/record/v1/customer?limit=5";

    fn independent_oauth1_signature(key: &str, base: &str) -> String {
        use base64::Engine as _;
        use hmac::{Hmac, KeyInit as _, Mac as _};

        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    fn oauth1_header_param(header: &str, name: &str) -> String {
        let prefix = format!("{name}=\"");
        let value = header
            .strip_prefix("OAuth ")
            .unwrap()
            .split(", ")
            .find_map(|field| field.strip_prefix(&prefix))
            .unwrap()
            .strip_suffix('"')
            .unwrap();
        url::form_urlencoded::parse(format!("value={value}").as_bytes())
            .next()
            .unwrap()
            .1
            .into_owned()
    }

    #[test]
    fn oauth1_rfc_5849_base_string_and_sha256_signature() {
        let url = url::Url::parse("http://EXAMPLE.COM:80/request?b5=%3D%253D&a3=a&c%40=&a2=r%20b")
            .unwrap();
        // RFC 5849 section 3.4.1.1 includes these decoded form fields and
        // omits oauth_version. Exercise its exact normalization vector here;
        // the production signer includes oauth_version.
        let params = [
            ("c2", ""),
            ("a3", "2 q"),
            ("oauth_consumer_key", "9djdj82h48djs9d2"),
            ("oauth_token", "kkk9d7dh3k39sjv7"),
            ("oauth_signature_method", "HMAC-SHA256"),
            ("oauth_timestamp", "137131201"),
            ("oauth_nonce", "7d8f3e4a"),
        ];
        let expected = concat!(
            "POST&http%3A%2F%2Fexample.com%2Frequest&a2%3Dr%2520b%26a3%3D2%2520q",
            "%26a3%3Da%26b5%3D%253D%25253D%26c%2540%3D%26c2%3D%26oauth_consumer_",
            "key%3D9djdj82h48djs9d2%26oauth_nonce%3D7d8f3e4a%26oauth_signature_m",
            "ethod%3DHMAC-SHA256%26oauth_timestamp%3D137131201%26oauth_token%3Dkkk",
            "9d7dh3k39sjv7"
        );
        let base = oauth1_base_string("POST", &url, &params);
        assert_eq!(base.as_bytes(), expected.as_bytes());
        let credential = Oauth1Credential {
            consumer_key: "9djdj82h48djs9d2".into(),
            consumer_secret: "j49sk3j29djd".into(),
            token: "kkk9d7dh3k39sjv7".into(),
            token_secret: "dh893hdasih9".into(),
            realm: None,
        };
        assert_eq!(
            oauth1_signature(&credential, &base),
            independent_oauth1_signature("j49sk3j29djd&dh893hdasih9", expected)
        );
    }

    #[test]
    fn oauth1_netsuite_header_signs_query_and_excludes_body() {
        let sign = |body: &[u8]| {
            credential_headers_with(None, OAUTH1_SECRET, "GET", OAUTH1_URL, body, || {
                Ok(("fixed-nonce".into(), 1_700_000_000))
            })
            .unwrap()
        };
        let credentials = sign(b"c2&a3=2+q");
        assert_eq!(credentials, sign(b"different body"));
        let header = &credentials["authorization"];
        assert!(header.starts_with("OAuth realm=\"123456\", oauth_consumer_key=\"consumer\", "));
        assert!(header.contains("oauth_signature_method=\"HMAC-SHA256\""));
        assert_eq!(oauth1_header_param(header, "oauth_version"), "1.0");
        let expected = concat!(
            "GET&https%3A%2F%2F123456.suitetalk.api.netsuite.com%2Fservices%2Frest%2Frecord%2Fv1%2Fcustomer&",
            "limit%3D5%26oauth_consumer_key%3Dconsumer%26oauth_nonce%3Dfixed-nonce",
            "%26oauth_signature_method%3DHMAC-SHA256%26oauth_timestamp%3D1700000000",
            "%26oauth_token%3Dtoken%26oauth_version%3D1.0"
        );
        assert_eq!(
            oauth1_header_param(header, "oauth_signature"),
            independent_oauth1_signature("consumer%2Fsecret%26&token%20secret%2B", expected)
        );
    }

    #[test]
    fn oauth1_preserves_query_plus_and_signs_form_body() {
        let url = url::Url::parse("https://example.com/?x=a+b").unwrap();
        assert_eq!(
            oauth1_base_string("GET", &url, &[]),
            "GET&https%3A%2F%2Fexample.com%2F&x%3Da%252Bb"
        );
        let sign = |body: &[u8], content_type| {
            credential_headers_with(
                content_type,
                OAUTH1_SECRET,
                "POST",
                "https://example.com/?x=a+b",
                body,
                || Ok(("nonce".into(), 1)),
            )
            .unwrap()
        };
        let headers = sign(
            b"a=one+two&a=%2B",
            Some(b"application/x-www-form-urlencoded".as_slice()),
        );
        let expected = concat!(
            "POST&https%3A%2F%2Fexample.com%2F&",
            "a%3D%252B%26a%3Done%2520two%26oauth_consumer_key%3Dconsumer%26oauth_nonce%3Dnonce",
            "%26oauth_signature_method%3DHMAC-SHA256%26oauth_timestamp%3D1%26oauth_token%3Dtoken",
            "%26oauth_version%3D1.0%26x%3Da%252Bb"
        );
        assert_eq!(
            oauth1_header_param(&headers["authorization"], "oauth_signature"),
            independent_oauth1_signature("consumer%2Fsecret%26&token%20secret%2B", expected)
        );
        assert_ne!(
            headers,
            sign(
                b"a=changed",
                Some(b"application/x-www-form-urlencoded".as_slice())
            )
        );
        assert_eq!(
            sign(b"a=one", Some(b"application/json".as_slice())),
            sign(b"a=two", None)
        );
    }

    #[test]
    fn oauth1_repeated_requests_get_fresh_nonces() {
        let first = credential_headers(None, OAUTH1_SECRET, "GET", OAUTH1_URL, b"").unwrap();
        let second = credential_headers(None, OAUTH1_SECRET, "GET", OAUTH1_URL, b"").unwrap();
        assert_ne!(
            oauth1_header_param(&first["authorization"], "oauth_nonce"),
            oauth1_header_param(&second["authorization"], "oauth_nonce")
        );
    }

    #[test]
    fn oauth1_overrides_guest_authorization_without_changing_guest_request() {
        let request = effect_request(vec![
            Header {
                name: "Authorization".into(),
                value: b"guest-token".to_vec(),
            },
            Header {
                name: "authorization".into(),
                value: b"second-guest-token".to_vec(),
            },
        ]);
        let credentials = credential_headers(None, OAUTH1_SECRET, "GET", OAUTH1_URL, b"").unwrap();
        let signed = credentials["authorization"].clone();
        let headers = outbound_headers(&request, credentials).unwrap();
        assert_eq!(headers["authorization"], signed);
        assert_eq!(headers.get_all("authorization").iter().count(), 1);
        assert_eq!(request.headers[0].value, b"guest-token");
        assert_eq!(request.headers[1].value, b"second-guest-token");
        assert!(!signed.contains("consumer/secret&"));
        assert!(!signed.contains("token secret+"));
    }

    #[test]
    fn oauth1_realm_is_optional_and_quoted_without_percent_encoding() {
        let mut secret: serde_json::Value = serde_json::from_str(OAUTH1_SECRET).unwrap();
        secret["oauth1"].as_object_mut().unwrap().remove("realm");
        let without =
            credential_headers_with(None, &secret.to_string(), "GET", OAUTH1_URL, b"", || {
                Ok(("fixed-nonce".into(), 1_700_000_000))
            })
            .unwrap();
        assert!(without["authorization"].starts_with("OAuth oauth_consumer_key="));
        secret["oauth1"]["realm"] = serde_json::json!("account /\"\\");
        let with =
            credential_headers_with(None, &secret.to_string(), "GET", OAUTH1_URL, b"", || {
                Ok(("fixed-nonce".into(), 1_700_000_000))
            })
            .unwrap();
        assert!(with["authorization"].starts_with("OAuth realm=\"account /\\\"\\\\\", "));
        assert_eq!(
            oauth1_header_param(&with["authorization"], "oauth_signature"),
            oauth1_header_param(&without["authorization"], "oauth_signature")
        );
    }

    #[test]
    fn oauth1_malformed_and_multiple_credential_arms_are_refused() {
        let valid: serde_json::Value = serde_json::from_str(OAUTH1_SECRET).unwrap();
        let refuses = |secret: serde_json::Value| {
            assert!(matches!(
                credential_headers(None, &secret.to_string(), "GET", OAUTH1_URL, b""),
                Err(ConnectionError::CredentialUnavailable)
            ));
        };
        for field in ["consumer-key", "consumer-secret", "token", "token-secret"] {
            let mut missing = valid.clone();
            missing["oauth1"].as_object_mut().unwrap().remove(field);
            refuses(missing);
            for value in [
                serde_json::json!(""),
                serde_json::json!(123),
                serde_json::Value::Null,
            ] {
                let mut malformed = valid.clone();
                malformed["oauth1"][field] = value;
                refuses(malformed);
            }
        }
        for arm in ["headers", "hmac"] {
            let mut two = valid.clone();
            two[arm] = serde_json::json!({});
            refuses(two);
        }
        refuses(serde_json::json!({"oauth1": null}));
        let mut extra = valid.clone();
        extra["oauth1"]["algorithm"] = serde_json::json!("HMAC-SHA1");
        refuses(extra);
        for realm in [serde_json::json!(123), serde_json::json!("bad\r\nrealm")] {
            let mut malformed = valid.clone();
            malformed["oauth1"]["realm"] = realm;
            refuses(malformed);
        }
    }

    #[test]
    fn malformed_credentials_are_refused() {
        for secret in [
            "not json",
            "null",
            "{}",
            r#"{"other":{}}"#,
            r#"{"hmac":null}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":"x-signature","extra":true}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":"x-signature"},"extra":true}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":"x-signature"},"headers":{}}"#,
            r#"{"hmac":{"secret":"key","header":"x-signature"}}"#,
            r#"{"hmac":{"algorithm":"sha256","header":"x-signature"}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key"}}"#,
            r#"{"hmac":{"algorithm":"sha512","secret":"key","header":"x-signature"}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"","header":"x-signature"}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":123,"header":"x-signature"}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":""}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":"bad header"}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":"x-signature\r\n"}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":"Content-Length"}}"#,
            r#"{"hmac":{"algorithm":"sha256","secret":"key","header":"Idempotency-Key"}}"#,
            r#"{"headers":{"authorization":123}}"#,
            r#"{"headers":{"host":"elsewhere"}}"#,
            r#"{"headers":{"authorization":"bad\r\nvalue"}}"#,
        ] {
            assert!(
                matches!(
                    credential_headers(None, secret, "POST", "https://example.com/notify", b"body"),
                    Err(ConnectionError::CredentialUnavailable)
                ),
                "accepted malformed credential: {secret}"
            );
        }
    }

    #[test]
    fn headers_credential_keeps_its_shape_and_overrides_guest_values() {
        let secret = r#"{"headers":{"authorization":"Bearer host-token"}}"#;
        let request = effect_request(vec![Header {
            name: "Authorization".into(),
            value: b"guest-token".to_vec(),
        }]);
        let headers = outbound_headers(
            &request,
            credential_headers(None, secret, "POST", "https://example.com/notify", b"body")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(headers["authorization"], "Bearer host-token");
        assert_eq!(headers.get_all("authorization").iter().count(), 1);
        assert!(
            credential_headers(
                None,
                r#"{"headers":{}}"#,
                "POST",
                "https://example.com/notify",
                b""
            )
            .unwrap()
            .is_empty()
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

    #[test]
    fn guest_headers_cannot_supply_transport_or_proxy_authority() {
        for name in [
            "host",
            "content-length",
            "transfer-encoding",
            "proxy-authorization",
            "connection",
        ] {
            for name in [name.to_owned(), name.to_ascii_uppercase()] {
                let request = effect_request(vec![Header {
                    name,
                    value: b"guest".to_vec(),
                }]);
                assert!(matches!(
                    outbound_headers(&request, HashMap::new()),
                    Err(ConnectionError::AuthorityDenied)
                ));
            }
        }
        assert!(outbound_headers(&effect_request(Vec::new()), HashMap::new()).is_ok());
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

    /// `apps/platform/no-std/http-request` forwards its node context's traceparent
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
            Arc::new(HttpTransport::new().expect("HTTP transport")),
            Arc::new(ConnectionCredentials::empty()),
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
            expected_attributes(
                ["tenant-a", "project-a", component_id],
                [
                    "package_a",
                    "orders",
                    "3",
                    "notify",
                    "2",
                    component_digest.as_str(),
                    "notifier",
                    "orders:notify/dispatch@1.0.0",
                ],
                &[("effect.operation", "send")],
            ),
        );
    }

    /// The SAME wiring id, version and node in ANOTHER package, visited at
    /// another occurrence. A wiring id is package-scoped, so this is a different
    /// wiring wearing the same name.
    fn second_package_invocation() -> ConnectionInvocation {
        let mut invocation = ConnectionInvocation {
            package_id: "package_b".to_string(),
            ..invocation()
        };
        position(&mut invocation).occurrence = 5;
        invocation
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
            expected_attributes(
                ["tenant-a", "project-a", component_id],
                [
                    package,
                    "orders",
                    "3",
                    "notify",
                    occurrence,
                    component_digest.as_str(),
                    "notifier",
                    "orders:notify/dispatch@1.0.0",
                ],
                &[("effect.operation", "send")],
            )
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
            expected_attributes(
                ["tenant-a", "project-a", component_id],
                ["", "", "0", "", "0", "", "", ""],
                &[("effect.operation", "send")],
            ),
        );
    }

    /// One effect span, opened and settled the way `send` settles it.
    fn observed_http_span(
        plugin: &ConnectionHttp,
        component_id: &str,
        result: &Result<Response, ConnectionError>,
    ) {
        let span = http_span(plugin, component_id);
        let evidence = plugin
            .invocation(component_id)
            .and_then(|invocation| invocation.effects);
        let mut observed = EffectOutcomeGuard::new(&span, evidence);
        observed.settle(http_outcome(result), result.is_err());
    }

    #[test]
    fn bound_http_failure_evidence_uses_the_existing_classifier_and_resets_on_rebind() {
        let plugin = offline_plugin();
        let component_id = "component-evidence-test";
        let evidence = EffectEvidence::new();
        let mut bound = invocation();
        bound.effects = Some(evidence.clone());
        plugin.bind_invocation(component_id, bound).unwrap();
        observed_http_span(&plugin, component_id, &Err(ConnectionError::Timeout));
        assert_eq!(evidence.outcome(), Some(EffectOutcome::Timeout));
        observed_http_span(
            &plugin,
            component_id,
            &Err(ConnectionError::Transport(RESPONSE_BODY_LOST.to_owned())),
        );
        assert_eq!(evidence.outcome(), None);
        plugin.revoke_invocation(component_id);
        let replacement = EffectEvidence::new();
        let mut bound = invocation();
        bound.effects = Some(replacement.clone());
        plugin.bind_invocation(component_id, bound).unwrap();
        observed_http_span(
            &plugin,
            component_id,
            &Err(ConnectionError::AuthorityDenied),
        );
        assert_eq!(
            replacement.outcome(),
            Some(EffectOutcome::RefusedBeforeDispatch)
        );
        assert_eq!(evidence.outcome(), None);
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
    /// answer shows it. The guest received nothing, so it is not responded
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
            expected_attributes(
                ["tenant-a", "project-a", component_id],
                [
                    "package_a",
                    "orders",
                    "3",
                    "notify",
                    "2",
                    component_digest.as_str(),
                    "notifier",
                    "orders:notify/dispatch@1.0.0",
                ],
                &[("effect.operation", "send"), ("effect.outcome", "timeout")],
            ),
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
            expected_attributes(
                ["tenant-a", "project-a", component_id],
                [
                    "package_a",
                    "orders",
                    "3",
                    "notify",
                    "2",
                    component_digest.as_str(),
                    "notifier",
                    "orders:notify/dispatch@1.0.0",
                ],
                &[("effect.operation", "send")],
            ),
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
            expected_attributes(
                ["tenant-a", "project-a", component_id],
                [
                    "package_a",
                    "orders",
                    "3",
                    "notify",
                    "2",
                    component_digest.as_str(),
                    "notifier",
                    operation,
                ],
                &[("effect.operation", "send")],
            )
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

#[cfg(test)]
mod connection_mode_tests {
    #[test]
    fn method_override_headers_are_stripped_and_credentials_cannot_supply_them() {
        use super::*;
        let request = Request {
            requirement: "erp".into(),
            method: "GET".into(),
            path_and_query: "/".into(),
            headers: vec![
                Header {
                    name: "X-HTTP-Method-Override".into(),
                    value: b"POST".to_vec(),
                },
                Header {
                    name: "X-HTTP-Method".into(),
                    value: b"DELETE".to_vec(),
                },
            ],
            body: None,
            idempotency_key: None,
        };
        let headers = outbound_headers(&request, HashMap::new()).unwrap();
        assert!(!headers.contains_key("x-http-method-override"));
        assert!(!headers.contains_key("x-http-method"));
        for name in ["X-HTTP-Method-Override", "X-HTTP-Method"] {
            let secret = serde_json::json!({"headers": {(name): "POST"}}).to_string();
            assert!(
                credential_headers(None, &secret, "GET", "https://example.test/", b"body").is_err()
            );
        }
    }

    #[test]
    fn read_mode_refuses_every_mutating_http_method() {
        for method in ["GET", "HEAD"] {
            super::authorize_method(true, method).unwrap();
        }
        for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS", "get"] {
            assert!(super::authorize_method(true, method).is_err());
            super::authorize_method(false, method).unwrap();
        }
    }
}
