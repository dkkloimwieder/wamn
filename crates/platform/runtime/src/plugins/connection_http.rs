//! Environment-bound HTTP for ordinary `wamn:node` components.
//!
//! The host binds the exact entry (a route, or a wiring node position and
//! occurrence) and the component digest before invoking a pooled component. The
//! guest names only a store alias; the database must resolve that alias at the
//! component grain and the mounted manifest must contain the exact route or
//! wiring version/hash and the component tuple. No run, plan, frame or effect
//! record participates.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use std::time::Duration;

use anyhow::Context as _;

use crate::EffectOutcome;
use crate::normalize_portable_http_target;
use tracing::Instrument as _;
use wamn_catalog::{ArtifactHash, ConnectionTypeDescriptor, ServingComponent, ServingManifest};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Accessor, Linker};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::connection_authority::{
    AuthorityDecision, AuthorityError, HttpConnectionAuthority, TokioDnsResolver,
    bound_http_target, bound_oauth_token, resolve_http_request,
};
use crate::plugins::effect_span::{
    EFFECT_OPERATION, EffectEvidence, EffectIdentity, EffectOutcomeGuard, EffectWiring,
    HTTP_EFFECT_DURATION_MS, effect_span, record_effect_ms, record_wiring,
};
use wamn_engine::release_manifest::LoadedRelease;

use super::wamn_postgres::{
    CandidateBindingWorld, ConnectionEffectSnapshot, ConnectionEntryLookup, WamnPostgres,
    WiringLookup,
};
use crate::connection_credentials::ConnectionGeneration;
use crate::connection_facts::{ConnectionFacts, Secret};
use crate::connection_memory::{ConnectionFact, ConnectionStatus};

mod authentication;
mod jwt;
mod network_policy;
pub mod transport;

use transport::{ClientIdentity, ConnectionIdentity, HttpTransport};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: [
            "../../../wit/wamn-connection",
            "../../../wit/wamn-flow-http-routing",
            "../../../wit/wamn-postgres-0.4",
            "../../../wit/wamn-node",
            "../../../wit/wamn-operation-0.1",
            "../../../wit/wasmcloud-blobstore",
            "wit",
        ],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

pub use bindings::wamn::connection::http::ConnectionError;
use bindings::wamn::connection::http::{self, Header, Request, Response};

pub const CONNECTION_HTTP_ID: &str = "wamn-connection-http";
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
    pub configuration: Arc<std::collections::BTreeMap<String, serde_json::Value>>,
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

/// Names the project and project only: the rest carries credential and
/// transport state that must not reach a log.
impl std::fmt::Debug for ConnectionHttp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionHttp")
            .field("project_env_key", &self.release.target().project_env_key)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// Host-owned services and identity for the trusted HTTP effect.
pub struct ConnectionHttp {
    postgres: Arc<WamnPostgres>,
    transport: Arc<HttpTransport>,
    facts: Arc<ConnectionFacts>,
    oauth2: Arc<crate::oauth_token::Cache>,
    project: Box<str>,
    allowed_hosts: Arc<[AllowedHost]>,
    release: Arc<LoadedRelease>,
}

/// A package release for host capability fixtures that execute no released component.
#[cfg(any(test, feature = "test-util"))]
pub fn test_release(project_env_key: &str) -> Arc<LoadedRelease> {
    let manifest = ServingManifest::new(
        wamn_catalog::ServingRelease {
            packages: [
                wamn_catalog::PackageCoordinate::new("capability_fixture", "1.0.0")
                    .expect("fixture package coordinate"),
            ]
            .into(),
        },
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .expect("capability fixture manifest");
    Arc::new(
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "capability fixture",
            wamn_engine::release_manifest::ReleaseTarget::new(project_env_key, "dev"),
        )
        .expect("capability fixture release"),
    )
}

impl ConnectionHttp {
    pub fn new(
        postgres: Arc<WamnPostgres>,
        transport: Arc<HttpTransport>,
        facts: Arc<ConnectionFacts>,
        project: impl Into<Box<str>>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<LoadedRelease>,
    ) -> Self {
        let project: Box<str> = project.into();
        let oauth2 = facts.oauth_tokens();
        Self {
            postgres,
            transport,
            facts,
            oauth2,
            project,
            allowed_hosts,
            release,
        }
    }

    fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        self.postgres.invocation(component_id)
    }

    /// Exercise the host dispatch path without constructing a component store.
    #[cfg(feature = "test-util")]
    pub async fn send_for_test(
        &self,
        component_id: &str,
        requirement: &str,
        method: &str,
    ) -> Result<u16, ConnectionError> {
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
    }

    /// Send once, and at most once more after the external system refuses
    /// the credential (401 or 403) when the second send can differ: a new
    /// read of the connection fact names another generation or credential,
    /// or an OAuth2 401 fetches the token again. A repeated send never
    /// carries the credential that was just refused. Each send keeps the
    /// facts it started with.
    async fn send(
        &self,
        component_id: &str,
        request: &Request,
    ) -> Result<Response, ConnectionError> {
        let first = self.attempt(component_id, request, None).await?;
        let (connection, used) = match &first {
            Attempt::Sent { response, .. } if !matches!(response.status, 401 | 403) => {
                return Ok(first.into_response().expect("a sent attempt"));
            }
            Attempt::Sent {
                connection, used, ..
            }
            | Attempt::Stale { connection, used } => (*connection, used.clone()),
        };
        self.oauth2.remove(&(connection, used.active_generation));
        let oauth2 = self
            .facts
            .generation(&connection, used.active_generation)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .auth_type
            .clone();
        let oauth2 = crate::oauth_token::is_oauth(&oauth2) && first.status() == Some(401);
        let fresh = self
            .facts
            .read_again(&connection)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        if matches!(first, Attempt::Sent { .. }) && fresh.as_ref() == Some(&used) && !oauth2 {
            return Ok(first.into_response().expect("a sent attempt"));
        }
        let rejected = match &first {
            Attempt::Sent { oauth_token, .. } if oauth2 => oauth_token.as_deref(),
            _ => None,
        };
        let second = self.attempt(component_id, request, rejected).await?;
        match second {
            Attempt::Sent {
                response,
                connection,
                used,
                ..
            } => {
                if oauth2 && response.status == 401 {
                    self.oauth2.remove(&(connection, used.active_generation));
                }
                Ok(response)
            }
            Attempt::Stale { .. } => first
                .into_response()
                .ok_or(ConnectionError::CredentialUnavailable),
        }
    }

    async fn attempt(
        &self,
        component_id: &str,
        request: &Request,
        rejected: Option<&str>,
    ) -> Result<Attempt, ConnectionError> {
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
        let invocation = self
            .invocation(component_id)
            .ok_or(ConnectionError::AttestationInvalid)?;
        let environment = self.facts.environment();
        let snapshot = crate::connection_binding::resolve(
            &self.postgres,
            &self.facts,
            &self.release,
            component_id,
            &request.requirement,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "HTTP connection authority refused");
            ConnectionError::AttestationInvalid
        })?;
        authorize_snapshot(&snapshot, &ConnectionTypeDescriptor::http_v1())?;
        authorize_method(snapshot.read_only, &request.method)?;

        let definition = snapshot
            .definition
            .as_ref()
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let authority = crate::connection_authority::parse_http_definition(definition)
            .map_err(|error| authority_denied("definition", &error))?;
        let decision = resolve_http_request(
            &authority,
            &target,
            &self.allowed_hosts,
            &network_policy::allows,
            &TokioDnsResolver,
        )
        .await
        .map_err(|error| authority_denied("request", &error))?;

        let connection: uuid::Uuid = snapshot
            .instance_id
            .as_deref()
            .and_then(|id| id.parse().ok())
            .ok_or(ConnectionError::Unbound)?;
        let generation = snapshot
            .generation
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let fact = self
            .facts
            .connection(&connection)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .filter(|fact| fact.status == ConnectionStatus::Active)
            .ok_or(ConnectionError::CredentialUnavailable)?;
        if fact.active_generation != generation {
            return Ok(Attempt::Stale {
                connection,
                used: fact,
            });
        }
        let identity = ClientIdentity {
            connection: ConnectionIdentity {
                project_env_key: self
                    .release
                    .target()
                    .project_env_key
                    .clone()
                    .into_boxed_str(),
                project: self.project.clone(),
                environment: environment.into(),
                instance: connection.to_string().into(),
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
            generation,
        };
        let configuration = self
            .facts
            .generation(&connection, generation)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let (mut response, oauth_token) = self
            .execute_authenticated(
                identity,
                &authority,
                decision,
                request,
                (&connection, &fact, &configuration),
                rejected,
            )
            .await?;
        response
            .external_system
            .clone_from(&configuration.external_system);
        Ok(Attempt::Sent {
            response,
            connection,
            used: fact,
            oauth_token,
        })
    }

    async fn execute_authenticated(
        &self,
        identity: ClientIdentity,
        authority: &HttpConnectionAuthority,
        mut decision: AuthorityDecision,
        request: &Request,
        (connection, fact, configuration): (&uuid::Uuid, &ConnectionFact, &ConnectionGeneration),
        rejected: Option<&str>,
    ) -> Result<(Response, Option<String>), ConnectionError> {
        let auth_type = configuration.auth_type.as_str();
        let parameters = configuration
            .parameters
            .get("auth")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let secret = match (auth_type, fact.credential_id) {
            ("none", _) | (_, None) => None,
            (_, Some(credential)) => Some(
                self.facts
                    .secret(&credential)
                    .await
                    .map_err(|_| ConnectionError::CredentialUnavailable)?,
            ),
        };
        let material = secret.as_deref().map(Secret::expose);
        if !crate::oauth_token::is_oauth(auth_type) {
            let headers =
                authentication::headers(auth_type, &parameters, material, request, &mut decision)?;
            return execute(&self.transport, identity, decision, request, headers)
                .await
                .map(|response| (response, None));
        }
        let key = (*connection, identity.generation);
        let token = self
            .oauth2_token(
                &identity,
                authority,
                (auth_type, &parameters, material),
                &key,
                rejected,
            )
            .await?;
        execute(
            &self.transport,
            identity,
            decision,
            request,
            [("authorization".into(), format!("Bearer {token}"))].into(),
        )
        .await
        .map(|response| (response, Some(token)))
    }

    async fn oauth2_token(
        &self,
        identity: &ClientIdentity,
        authority: &HttpConnectionAuthority,
        (auth_type, parameters, material): (&str, &serde_json::Value, Option<&str>),
        key: &crate::oauth_token::CacheKey,
        rejected: Option<&str>,
    ) -> Result<String, ConnectionError> {
        let ticket = self.facts.oauth_ticket(&key.0);
        let cache_ticket = self.oauth2.ticket();
        if rejected.is_none()
            && let Some(token) = self.oauth2.get(key)
        {
            return Ok(token);
        }
        self.oauth2.remove(key);
        let envelope = self
            .facts
            .oauth_encryption()
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let available = crate::oauth_token::get(
            crate::oauth_token::Request {
                reader: &self.facts.reader(),
                project: &self.project,
                environment: self.facts.environment(),
                connection: key.0,
                generation: key.1,
                authorization_code: auth_type == "oauth2_authorization_code",
                rejected,
            },
            &envelope,
            |old| async move {
                let refresh = if auth_type == "oauth2_authorization_code" {
                    Some(
                        old.and_then(|token| token.refresh_token)
                            .ok_or_else(|| anyhow::anyhow!("OAuth refresh token missing"))?,
                    )
                } else {
                    None
                };
                let request = oauth2_request(auth_type, parameters, material, refresh.as_deref())
                    .map_err(|_| anyhow::anyhow!("OAuth credential unavailable"))?;
                let decision = token_endpoint(
                    authority,
                    request.token_authority.as_deref(),
                    &request.token_url,
                    Some(&self.allowed_hosts),
                )
                .await?;
                let token =
                    fetch_oauth2_token(&self.transport, identity.clone(), decision, &request)
                        .await
                        .map_err(|_| anyhow::anyhow!("OAuth provider unavailable"))?;
                Ok(crate::oauth_token::Fetched {
                    token: crate::oauth_token::Token {
                        access_token: Some(token.access_token),
                        refresh_token: token.refresh_token,
                    },
                    expires_in: token.expires_in,
                })
            },
        )
        .await
        .map_err(|error| {
            if error.unauthorized {
                ConnectionError::Unauthorized
            } else {
                ConnectionError::CredentialUnavailable
            }
        })?;
        if let Some(expires) = available.retain_until {
            self.oauth2
                .put(cache_ticket, *key, available.token.clone(), expires, || {
                    self.facts.oauth_ticket(&key.0) == ticket
                });
        }
        Ok(available.token)
    }
}

/// One send of [`ConnectionHttp::send`] and the connection fact it used.
enum Attempt {
    Sent {
        response: Response,
        connection: uuid::Uuid,
        used: ConnectionFact,
        oauth_token: Option<String>,
    },
    /// The fact named another generation than the authorization read.
    Stale {
        connection: uuid::Uuid,
        used: ConnectionFact,
    },
}

impl Attempt {
    fn status(&self) -> Option<u16> {
        match self {
            Self::Sent { response, .. } => Some(response.status),
            Self::Stale { .. } => None,
        }
    }

    fn into_response(self) -> Option<Response> {
        match self {
            Self::Sent { response, .. } => Some(response),
            Self::Stale { .. } => None,
        }
    }
}

use crate::oauth::{Request as Oauth2Request, Response as Oauth2Token};

fn oauth2_request(
    auth_type: &str,
    parameters: &serde_json::Value,
    material: Option<&str>,
    refresh_token: Option<&str>,
) -> Result<Oauth2Request, ConnectionError> {
    if auth_type == "oauth2_client_credentials_jwt" {
        jwt::request(parameters, material, chrono::Utc::now().timestamp())
    } else {
        Oauth2Request::credentials(auth_type, parameters, material, refresh_token)
    }
}

/// Apply the same origin, path, address, and pinning rules to every OAuth grant.
pub(crate) async fn token_endpoint(
    connection: &HttpConnectionAuthority,
    token_authority: Option<&str>,
    token_url: &str,
    platform_hosts: Option<&[AllowedHost]>,
) -> Result<AuthorityDecision, AuthorityError> {
    let (authority, target, endpoint) = bound_oauth_token(connection, token_authority, token_url)?;
    let url = url::Url::parse(authority.canonical_base_url()).expect("canonical token URL");
    let exact = [AllowedHost::Authority(
        format!(
            "{}:{}",
            url.host_str().expect("canonical token host"),
            authority.authority().port()
        )
        .parse()
        .expect("canonical token authority"),
    )];
    let mut decision = resolve_http_request(
        &authority,
        &target,
        platform_hosts.unwrap_or(&exact),
        &network_policy::allows,
        &TokioDnsResolver,
    )
    .await?;
    decision.logical_url = endpoint;
    Ok(decision)
}

async fn fetch_oauth2_token(
    transport: &HttpTransport,
    identity: ClientIdentity,
    decision: AuthorityDecision,
    request: &Oauth2Request,
) -> Result<Oauth2Token, ConnectionError> {
    request
        .send(&crate::oauth::Transport {
            http: transport.clone(),
            decision,
            identity: Some(identity),
        })
        .await
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
        type_ = ?error.error_type(),
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

/// Find the application authorized to execute this exact released component.
pub fn released_invocation_application<'a>(
    manifest: &ServingManifest,
    target: &'a wamn_engine::release_manifest::ReleaseTarget,
    invocation: &ConnectionInvocation,
) -> Option<&'a str> {
    if !manifest.components.iter().any(|component| {
        component.package_id == invocation.package_id
            && component.descriptor.component == invocation.component
            && component.descriptor.component_digest == invocation.component_digest
            && component
                .descriptor
                .operations
                .contains_key(&invocation.operation)
    }) {
        return None;
    }
    if invocation.package_id == wamn_catalog::PALETTE_PACKAGE_ID {
        let InvocationEntry::Wiring(position) = &invocation.entry else {
            return None;
        };
        target
            .applications
            .get_key_value(&position.package_id)
            .map(|(name, _)| name.as_str())
    } else {
        if !manifest
            .release
            .packages
            .iter()
            .any(|package| package.package_id() == invocation.package_id)
        {
            return None;
        }
        let application =
            wamn_catalog::application_for_package(&target.applications, &invocation.package_id)?;
        if invocation
            .entry
            .wiring()
            .is_some_and(|position| position.package_id != application)
        {
            return None;
        }
        Some(application)
    }
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
        .and_then(|component| component.descriptor.operations.get(operation))
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
    root.descriptor.operations.contains_key(root_operation)
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

/// Require an active binding, the admitted generation and the selected capability's contract.
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

fn reserved_header(name: &hyper::header::HeaderName) -> bool {
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
) -> Result<hyper::header::HeaderMap, ConnectionError> {
    let mut headers = hyper::header::HeaderMap::new();
    for header in &request.headers {
        let name = hyper::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| ConnectionError::AuthorityDenied)?;
        if matches!(name.as_str(), "x-http-method-override" | "x-http-method") {
            continue;
        }
        if reserved_header(&name) {
            return Err(ConnectionError::AuthorityDenied);
        }
        let value = hyper::header::HeaderValue::from_bytes(&header.value)
            .map_err(|_| ConnectionError::AuthorityDenied)?;
        headers.append(name, value);
    }
    for (name, value) in credentials {
        let name = hyper::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let value = hyper::header::HeaderValue::from_str(&value)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        headers.insert(name, value);
    }
    if let Some(key) = &request.idempotency_key {
        let value =
            hyper::header::HeaderValue::from_str(key).map_err(|_| ConnectionError::Incompatible)?;
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
    headers: &'a mut hyper::header::HeaderMap,
}

impl opentelemetry::propagation::Injector for TraceContextHeaders<'_> {
    fn set(&mut self, key: &str, value: String) {
        // The W3C propagator writes `tracestate` even when the context carries
        // none, and an empty field is not a field.
        if value.is_empty() {
            return;
        }
        let Ok(name) = hyper::header::HeaderName::from_bytes(key.as_bytes()) else {
            return;
        };
        if self.headers.contains_key(&name) {
            return;
        }
        let Ok(value) = hyper::header::HeaderValue::from_str(&value) else {
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
/// Public because the `trace-test` condition drives THIS function rather than a
/// copy of it (`wamn-k9ea`): the condition's whole assertion is that what crosses the
/// process boundary is what production injects, so a reimplementation there
/// does not test this one.
pub fn inject_trace_context(headers: &mut hyper::header::HeaderMap) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut TraceContextHeaders { headers });
    });
}

async fn execute(
    transport: &HttpTransport,
    identity: ClientIdentity,
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
    let method = hyper::Method::from_bytes(request.method.as_bytes())
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
        .execute(identity, &decision, outbound)
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

async fn fetch_health_token(
    transport: &HttpTransport,
    decision: AuthorityDecision,
    request: &Oauth2Request,
) -> Result<Oauth2Token, ConnectionError> {
    request
        .send(&crate::oauth::Transport {
            http: transport.clone(),
            decision,
            identity: None,
        })
        .await
}

/// Test candidate HTTP parameters without writing connection facts.
pub async fn health(
    transport: &HttpTransport,
    parameters: &serde_json::Value,
    auth_type: &str,
    credential: Option<&str>,
) -> Result<(), String> {
    async {
        let authority = crate::connection_authority::parse_http_definition(parameters)?;
        let url = url::Url::parse(authority.canonical_base_url())?;
        anyhow::ensure!(url.scheme() == "https", "HTTP health requires TLS");
        let host = url.host_str().context("HTTP authority has no host")?;
        let port = url
            .port_or_known_default()
            .context("HTTP authority has no port")?;
        let allowed = [AllowedHost::Authority(format!("{host}:{port}").parse()?)];
        let (target, endpoint) = bound_http_target(&authority, authority.canonical_base_url())?;
        let mut decision = resolve_http_request(
            &authority,
            &target,
            &allowed,
            &network_policy::allows,
            &TokioDnsResolver,
        )
        .await?;
        decision.logical_url = endpoint;
        let request = Request {
            requirement: "health".into(),
            method: "HEAD".into(),
            path_and_query: "/".into(),
            headers: Vec::new(),
            body: None,
            idempotency_key: None,
        };
        let auth = parameters
            .get("auth")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let headers = if matches!(
            auth_type,
            "oauth2_client_credentials" | "oauth2_client_credentials_jwt"
        ) {
            let token_request = oauth2_request(auth_type, &auth, credential, None)
                .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?;
            let token_decision = token_endpoint(
                &authority,
                token_request.token_authority.as_deref(),
                &token_request.token_url,
                None,
            )
            .await?;
            let token = fetch_health_token(transport, token_decision, &token_request)
                .await
                .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?;
            HashMap::from([(
                "authorization".into(),
                format!("Bearer {}", token.access_token),
            )])
        } else {
            authentication::headers(auth_type, &auth, credential, &request, &mut decision)
                .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?
        };
        let headers = outbound_headers(&request, headers)
            .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?;
        let mut outbound = hyper::Request::builder()
            .method("HEAD")
            .uri(decision.logical_url.as_ref())
            .body(&[] as &[u8])?;
        *outbound.headers_mut() = headers;
        let response = transport
            .send_once(&decision, outbound)
            .await
            .map_err(|_| anyhow::anyhow!("HTTP health request failed"))?;
        health_status(response.status().as_u16())
    }
    .await
    .map_err(|error: anyhow::Error| error.to_string())
}

fn health_status(status: u16) -> anyhow::Result<()> {
    anyhow::ensure!(!matches!(status, 401 | 403 | 500..=599), "{status}");
    Ok(())
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
            imports: HashSet::from([WitInterface::from(
                ConnectionTypeDescriptor::http_v1().contract.as_str(),
            )]),
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
/// `wamn.component` is the pooled instance id and `wamn.component_digest` is
/// a manifest key (`wamn-b2m6.7`).
/// All eight come from the [`ConnectionInvocation`] the router driver binds
/// before the component runs — the same host-attested record
/// [`ConnectionHttp::send`] authorizes against, never anything the guest sent.
///
/// A pooled instance with no invocation bound holds no such fact and records the
/// wiring keys empty. That send is about to be refused as
/// `ConnectionError::AttestationInvalid`, and an empty value says so where a
/// missing field would look like lost instrumentation.
fn http_span(plugin: &ConnectionHttp, component_id: &str) -> tracing::Span {
    let span = effect_span!(
        "wamn.connection_http",
        EffectIdentity {
            project_env_key: &plugin.release.target().project_env_key,
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
            | ConnectionError::CredentialUnavailable
            | ConnectionError::Unauthorized,
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
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        AdmittedComponentOperation, OperationType, PackageCoordinate,
        SERVING_MANIFEST_FORMAT_VERSION, ServingRelease, ServingRoute,
    };

    use super::*;

    use crate::connection_credentials::ConnectionCredentials;
    use crate::connection_memory::ConnectionMemory;
    use wamn_broker::Publisher;

    mod credential_retry_tests;
    mod oauth2_tests;

    use crate::plugins::effect_span::span_tests::{SpanHarness, expected_attributes};
    use crate::plugins::wamn_postgres::WamnPostgresConfig;

    pub(super) fn interface_address() -> std::net::IpAddr {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").expect("address selection socket");
        socket
            .connect("192.0.2.1:9")
            .expect("select a private interface");
        socket.local_addr().expect("interface address").ip()
    }

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
                    configuration: Default::default(),
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
        let plugin = offline_plugin();
        let component_id = "component-store-7";
        let invocation = invocation();
        plugin
            .postgres
            .bind_invocation(component_id, invocation.clone())
            .expect("the fresh store accepts its invocation");

        plugin.postgres.revoke_invocation(component_id);

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
            .postgres
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
                    "contract": ConnectionTypeDescriptor::http_v1().contract,
                },
            })),
            requirement_hash: Some(digest('d')),
            node_permitted: true,
            binding_active: true,
            binding_valid: true,
            instance_id: Some("manager".to_string()),
            validation_hash: Some(digest('e')),
            requirement_type: Some("http".to_string()),
            contract: Some(ConnectionTypeDescriptor::http_v1().contract),
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

    #[test]
    fn admitted_connection_facts_cannot_be_retargeted() {
        use crate::plugins::wamn_postgres::CandidateConnectionBinding;
        let mut snapshot = snapshot();
        snapshot.pinned_generation = snapshot.generation;
        let mut admitted = invocation();
        let binding = CandidateConnectionBinding {
            read_only: snapshot.read_only,
            component_digest: admitted.component_digest.clone(),
            store_alias: "erp".into(),
            requirement_hash: snapshot.requirement_hash.clone().unwrap(),
            instance_id: snapshot.instance_id.clone().unwrap(),
            instance_revision: snapshot.instance_revision.unwrap(),
            requirement_type: snapshot.requirement_type.clone().unwrap(),
            contract: snapshot.contract.clone().unwrap(),
            validation_hash: snapshot.validation_hash.clone().unwrap(),
            generation: snapshot.generation.unwrap(),
            definition_hash: snapshot.definition_hash.clone().unwrap(),
            credential_set_handle: snapshot.credential_handle.clone(),
        };
        admitted.closure = ConnectionExecutionClosure::Candidate {
            manifest_digest: digest('f'),
            environment: "dev".into(),
            wiring_hash: fixture_wiring_hash(),
            component: admitted.component.clone(),
            interface_version: admitted.origin.interface_version.clone(),
            binding_world: Arc::new(
                CandidateBindingWorld::from_json(serde_json::to_value([&binding]).unwrap())
                    .unwrap(),
            ),
        };
        assert!(authorize_candidate_closure(&admitted, &snapshot, &binding).is_ok());
        for change in [
            (|snapshot: &mut ConnectionEffectSnapshot| snapshot.wiring_hash = None)
                as fn(&mut ConnectionEffectSnapshot),
            |snapshot| snapshot.component = None,
            |snapshot| snapshot.interface_version = None,
            |snapshot| snapshot.operation = None,
            |snapshot| snapshot.read_only = !snapshot.read_only,
            |snapshot| snapshot.instance_id = None,
            |snapshot| snapshot.instance_revision = Some(0),
            |snapshot| snapshot.requirement_hash = None,
            |snapshot| snapshot.requirement_type = None,
            |snapshot| snapshot.contract = None,
            |snapshot| snapshot.validation_hash = None,
            |snapshot| snapshot.pinned_generation = None,
            |snapshot| snapshot.generation = None,
            |snapshot| snapshot.definition_hash = None,
            |snapshot| snapshot.credential_handle = None,
        ] {
            let mut changed = snapshot.clone();
            change(&mut changed);
            assert!(authorize_candidate_closure(&admitted, &changed, &binding).is_err());
        }
        admitted.origin.operation = "another-operation".into();
        assert!(authorize_candidate_closure(&admitted, &snapshot, &binding).is_err());
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
                descriptor: {
                    let mut descriptor = wamn_catalog::ComponentDescriptor::named(
                        snapshot.component.clone().expect("component"),
                        snapshot
                            .interface_version
                            .clone()
                            .expect("interface version"),
                        invocation.component_digest.clone(),
                    );
                    descriptor.operations = BTreeMap::from([(
                        snapshot.operation.expect("operation"),
                        AdmittedComponentOperation {
                            host_transaction: false,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                            committed_result_schema: None,

                            registered_operation: snapshot.registered_operation,
                            statements: BTreeMap::new(),
                        },
                    )]);
                    descriptor
                },
                package_id: invocation.package_id,
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
            .postgres
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
            .postgres
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

    /// The parameter must actually discriminate. A snapshot carrying a valid
    /// HTTP authority chain is still refused when authorized against another
    /// capability's descriptor — otherwise the argument is decoration, and one
    /// capability could authorize another's binding.
    #[test]
    fn a_valid_http_snapshot_is_refused_against_another_capabilitys_descriptor() {
        let valid = snapshot();
        authorize_snapshot(&valid, &ConnectionTypeDescriptor::http_v1())
            .expect("valid against its own descriptor");
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
        authorize_snapshot(&valid, &ConnectionTypeDescriptor::http_v1())
            .expect("the fixture snapshot carries every authority layer");

        let mut missing_node = valid.clone();
        missing_node.node_permitted = false;
        assert!(matches!(
            authorize_snapshot(&missing_node, &ConnectionTypeDescriptor::http_v1()),
            Err(ConnectionError::AttestationInvalid)
        ));

        let mut inactive_binding = valid.clone();
        inactive_binding.binding_active = false;
        assert!(matches!(
            authorize_snapshot(&inactive_binding, &ConnectionTypeDescriptor::http_v1()),
            Err(ConnectionError::Unbound)
        ));

        let mut stale_generation = valid.clone();
        stale_generation.generation = Some(6);
        assert!(matches!(
            authorize_snapshot(&stale_generation, &ConnectionTypeDescriptor::http_v1()),
            Err(ConnectionError::CredentialUnavailable)
        ));

        let mut wrong_contract = valid;
        wrong_contract.contract = Some("wamn:connection/postgres@0.1.0".to_string());
        assert!(matches!(
            authorize_snapshot(&wrong_contract, &ConnectionTypeDescriptor::http_v1()),
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

    #[test]
    fn guest_method_override_headers_are_removed() {
        let request = effect_request(vec![
            Header {
                name: "x-http-method".into(),
                value: b"DELETE".to_vec(),
            },
            Header {
                name: "X-HTTP-Method-Override".into(),
                value: b"PUT".to_vec(),
            },
        ]);
        let headers = outbound_headers(&request, HashMap::new()).unwrap();
        assert!(!headers.contains_key("x-http-method"));
        assert!(!headers.contains_key("x-http-method-override"));
        assert_eq!(request.method, "POST");
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
        let postgres = Arc::new(offline_postgres());
        let facts = Arc::new(ConnectionFacts::new(
            Arc::new(ConnectionMemory::default()),
            Publisher::default(),
            Arc::new(ConnectionCredentials::empty()),
            Arc::clone(&postgres),
            "org-a",
            "project-a",
            "dev",
        ));
        ConnectionHttp::new(
            postgres,
            Arc::new(HttpTransport::new().expect("HTTP transport")),
            facts,
            "project-a",
            Vec::<AllowedHost>::new().into(),
            crate::plugins::connection_http::test_release("org-a"),
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
            .postgres
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its invocation");
        let component_digest = digest('a');

        let harness = SpanHarness::install("connection-http-span-test");
        drop(http_span(&plugin, component_id));

        assert_eq!(
            harness.attributes("wamn.connection_http"),
            expected_attributes(
                ["org-a", "project-a", component_id],
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
    /// another occurrence. A wiring id is package-owned, so this is a different
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
                ["org-a", "project-a", component_id],
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
            .postgres
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its first invocation");
        drop(http_span(&plugin, component_id));
        plugin.postgres.revoke_invocation(component_id);
        plugin
            .postgres
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

    /// A pooled instance with no invocation bound holds no wiring identity — the
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
                ["org-a", "project-a", component_id],
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
        plugin
            .postgres
            .bind_invocation(component_id, bound)
            .unwrap();
        observed_http_span(&plugin, component_id, &Err(ConnectionError::Timeout));
        assert_eq!(evidence.outcome(), Some(EffectOutcome::Timeout));
        observed_http_span(
            &plugin,
            component_id,
            &Err(ConnectionError::Transport(RESPONSE_BODY_LOST.to_owned())),
        );
        assert_eq!(evidence.outcome(), None);
        plugin.postgres.revoke_invocation(component_id);
        let replacement = EffectEvidence::new();
        let mut bound = invocation();
        bound.effects = Some(replacement.clone());
        plugin
            .postgres
            .bind_invocation(component_id, bound)
            .unwrap();
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
    /// committed before the effect ran, and the observation states nothing about
    /// it: it says the platform declined to dispatch, and stops there.
    #[test]
    fn a_wiring_failure_names_the_refused_effect_and_not_the_command() {
        for error in [
            ConnectionError::Unbound,
            ConnectionError::AttestationInvalid,
            ConnectionError::AuthorityDenied,
            ConnectionError::CredentialUnavailable,
            ConnectionError::Unauthorized,
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
    /// effect-uncertain, which reports an unknown. The far side acted and the
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
            "a failure in flight must not state a deadline elapsed",
        );
        assert_ne!(
            in_flight,
            EffectOutcome::RefusedBeforeDispatch,
            "a failure in flight must not state the request never left",
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
            .postgres
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its invocation");
        let component_digest = digest('a');

        let harness = SpanHarness::install("connection-http-outcome-test");
        observed_http_span(&plugin, component_id, &Err(ConnectionError::Timeout));

        assert_eq!(
            harness.attributes("wamn.connection_http"),
            expected_attributes(
                ["org-a", "project-a", component_id],
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
    /// instance id, and by `wamn.component_digest`, which is a manifest key.
    /// It named the call only by `effect.operation`, the capability method. So
    /// nothing on the span answered "which component, running which node
    /// operation". Both keys are part of the whole value, and the two operation
    /// keys stand side by side here because they answer different questions.
    #[test]
    fn an_effect_span_names_the_executing_component_and_the_node_operation() {
        let plugin = offline_plugin();
        let component_id = "component-store-7";
        plugin
            .postgres
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its invocation");
        let component_digest = digest('a');

        let harness = SpanHarness::install("connection-http-component-operation-test");
        drop(http_span(&plugin, component_id));

        let attributes = harness.attributes("wamn.connection_http");
        assert_eq!(
            attributes,
            expected_attributes(
                ["org-a", "project-a", component_id],
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
            "the pooled instance id is not the component identity",
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
                ["org-a", "project-a", component_id],
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
            .postgres
            .bind_invocation(component_id, invocation())
            .expect("the fresh store accepts its first invocation");
        drop(http_span(&plugin, component_id));
        plugin.postgres.revoke_invocation(component_id);
        plugin
            .postgres
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
    fn health_accepts_only_the_ruled_statuses() {
        for status in [200, 204, 301, 400, 404, 429] {
            super::health_status(status).unwrap();
        }
        for status in [401, 403, 500, 503, 599] {
            assert_eq!(
                super::health_status(status).unwrap_err().to_string(),
                status.to_string()
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
