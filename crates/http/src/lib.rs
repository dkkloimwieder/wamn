//! Environment-bound HTTP for ordinary `wamn:node` components.
//!
//! The host binds the exact entry (a route, or a wiring node position and
//! occurrence) and the component digest before invoking a pooled component. The
//! guest names only a store alias; the database must resolve that alias at the
//! component grain and the mounted manifest must contain the exact route or
//! wiring version/hash and the component tuple. No run, plan, frame or effect
//! record participates.

use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
use wamn_connection::connection_credentials::Secret;
use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionFacts};
use wamn_connection::connection_memory::ConnectionStatus;
use wamn_engine::invocation_scope::InvocationScopes;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Accessor;
use wash_runtime::wit::{WitInterface, WitWorld};

use anyhow::Context as _;
use tracing::Instrument as _;
use wamn_authority::EffectOutcome;
use wamn_authority::effect_span::{
    EFFECT_OPERATION, EffectIdentity, EffectOutcomeGuard, HTTP_EFFECT_DURATION_MS, effect_span,
    record_effect_ms, record_wiring,
};

use wash_runtime::host::allowed_hosts::AllowedHost;

use wamn_authority::connection_authority::{
    AuthorityDecision, AuthorityError, HttpConnectionAuthority, TokioDnsResolver,
    bound_http_target, bound_oauth_token, resolve_http_request,
};

mod authentication;
mod jwt;
mod network_policy;
pub mod transport;

use crate::oauth::{Request as Oauth2Request, Response as Oauth2Token};
use network_policy::AddressPolicy;
use transport::HttpTransport;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: [
            "../../wit/wamn-connection",
            "../../wit/wamn-ingress",
            "../../wit/wamn-postgres-0.4",
            "../../wit/wamn-node",
            "../../wit/wamn-operation-0.1",
            "../../wit/wasmcloud-blobstore",
            "../authority/wit",
        ],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::connection::http::{self, Header, Request, Response};
use wamn_authority::connection::ConnectionError;

pub const CONNECTION_HTTP_ID: &str = "wamn-connection-http";

#[derive(Debug)]
struct ApplicationBinding {
    workload: String,
    application: Arc<wamn_engine::application_config::ApplicationConfig>,
}

/// HTTP host plugin. Only importing components receive its linker entries.
#[derive(Debug)]
pub struct ConnectionHttp {
    scopes: Arc<InvocationScopes>,
    transport: Arc<HttpTransport>,
    applications: Arc<RwLock<HashMap<String, ApplicationBinding>>>,
    component: Option<String>,
}
impl ConnectionHttp {
    pub fn new(scopes: Arc<InvocationScopes>, transport: Arc<HttpTransport>) -> Self {
        Self {
            scopes,
            transport,
            applications: Arc::default(),
            component: None,
        }
    }
    async fn send(&self, scope: &str, request: &Request) -> Result<Response, ConnectionError> {
        let execution = self
            .scopes
            .execution(scope)
            .ok_or(ConnectionError::AttestationInvalid)?;
        let context = wamn_connection::invocation::context(&self.scopes, scope)
            .map_err(|_| ConnectionError::AttestationInvalid)?;
        let invocation = wamn_connection::invocation::invocation(&self.scopes, scope)
            .ok_or(ConnectionError::AttestationInvalid)?;
        let alias = request
            .requirement
            .parse()
            .map_err(|_| ConnectionError::Incompatible)?;
        let binding = {
            let applications = self
                .applications
                .read()
                .expect("HTTP application bindings poisoned");
            let application = self
                .component
                .as_ref()
                .and_then(|id| applications.get(id))
                .ok_or(ConnectionError::Unbound)?;
            if application.application != context.release {
                return Err(ConnectionError::AttestationInvalid);
            }
            invocation
                .connection(&context.release, &alias)
                .ok_or(ConnectionError::Unbound)?
        };
        if binding.runtime_type != wamn_connection_type::RuntimeType::Host
            || binding.integration_type.as_str() != "http"
        {
            return Err(ConnectionError::Incompatible);
        }
        let addresses =
            AddressPolicy::for_target(&binding.target_type).ok_or(ConnectionError::Incompatible)?;
        let read_only = binding
            .mode
            .read_only(&invocation.package_id, &binding.name)
            .ok_or(ConnectionError::Incompatible)?;
        if read_only
            && !matches!(
                request.method.to_ascii_uppercase().as_str(),
                "GET" | "HEAD" | "OPTIONS"
            )
        {
            return Err(ConnectionError::AuthorityDenied);
        }
        let id = binding
            .connection_id
            .parse()
            .map_err(|_| ConnectionError::Unbound)?;
        let facts = &context.connections;
        let answer = facts
            .answer(&id)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .filter(|answer| answer.fact.status == ConnectionStatus::Active)
            .ok_or(ConnectionError::Unbound)?;
        let transport = self.transport.for_invocation(scope, execution, addresses);
        self.send_active(facts, id, answer, &transport, scope, request)
            .await
    }
    async fn send_active(
        &self,
        facts: &ConnectionFacts,
        id: uuid::Uuid,
        answer: Arc<ConnectionAnswer>,
        transport: &HttpTransport,
        scope: &str,
        request: &Request,
    ) -> Result<Response, ConnectionError> {
        let secret = credential(facts, &answer, None).await?;
        let (first, token) = self
            .attempt(
                facts,
                id,
                &answer,
                transport,
                scope,
                request,
                (secret.as_deref().map(Secret::expose), None),
            )
            .await?;
        if !matches!(first.status, 401 | 403) {
            return Ok(first);
        }
        facts
            .oauth_tokens()
            .remove(&(id, answer.fact.active_generation));
        let fresh = facts
            .read_again(&id)
            .await
            .map_err(|_| ConnectionError::CredentialUnavailable)?
            .filter(|answer| answer.fact.status == ConnectionStatus::Active)
            .ok_or(ConnectionError::Unbound)?;
        let same_credential = fresh.fact.credential_id == answer.fact.credential_id;
        let refreshed = credential(
            facts,
            &fresh,
            if same_credential {
                secret.as_ref()
            } else {
                None
            },
        )
        .await?;
        drop(secret);
        let same_generation =
            same_credential && fresh.fact.active_generation == answer.fact.active_generation;
        let rejected_token = token.as_deref().filter(|_| same_generation);
        let result = self
            .attempt(
                facts,
                id,
                &fresh,
                transport,
                scope,
                request,
                (refreshed.as_deref().map(Secret::expose), rejected_token),
            )
            .await;
        match result {
            Ok((response, _)) => {
                if matches!(response.status, 401 | 403) {
                    facts
                        .oauth_tokens()
                        .remove(&(id, fresh.fact.active_generation));
                }
                Ok(response)
            }
            Err(_) if rejected_token.is_some() => Ok(first),
            Err(error) => Err(error),
        }
    }
    async fn attempt(
        &self,
        facts: &ConnectionFacts,
        id: uuid::Uuid,
        answer: &ConnectionAnswer,
        transport: &HttpTransport,
        scope: &str,
        request: &Request,
        (material, rejected): (Option<&str>, Option<&str>),
    ) -> Result<(Response, Option<String>), ConnectionError> {
        let target = wamn_authority::normalize_portable_http_target(&request.path_and_query)
            .map_err(|_| ConnectionError::AuthorityDenied)?;
        let authority = wamn_authority::connection_authority::parse_http_definition(
            &answer.generation.parameters,
        )
        .map_err(|_| ConnectionError::AuthorityDenied)?;
        let mut decision = resolve_http_request(
            &authority,
            &target,
            &[exact_host(&authority)],
            &|address| transport.addresses().allows(address),
            &TokioDnsResolver,
        )
        .await
        .map_err(|_| ConnectionError::AuthorityDenied)?;
        let auth_type = answer.generation.auth_type.as_str();
        let empty = serde_json::json!({});
        let parameters = answer.generation.parameters.get("auth").unwrap_or(&empty);
        let (headers, token) = if wamn_connection::oauth_token::is_oauth(auth_type) {
            let principal = self
                .scopes
                .read::<wamn_connection::invocation::Scope, _>(scope, |record| record.principal)
                .flatten()
                .ok_or(ConnectionError::AttestationInvalid)?;
            let token = self
                .oauth2_token(
                    facts,
                    principal,
                    &authority,
                    (auth_type, parameters, material),
                    &(id, answer.fact.active_generation),
                    rejected,
                    transport,
                )
                .await?;
            (
                HashMap::from([("authorization".into(), format!("Bearer {token}"))]),
                Some(token),
            )
        } else {
            (
                authentication::headers(auth_type, parameters, material, request, &mut decision)?,
                None,
            )
        };
        let mut response = execute(transport, decision, request, headers).await?;
        response
            .external_system
            .clone_from(&answer.generation.external_system);
        Ok((response, token))
    }
    async fn oauth2_token(
        &self,
        facts: &ConnectionFacts,
        principal: wamn_identity_protocol::PrincipalId,
        authority: &HttpConnectionAuthority,
        (auth_type, parameters, material): (&str, &serde_json::Value, Option<&str>),
        key: &wamn_connection::oauth_token::CacheKey,
        rejected: Option<&str>,
        transport: &HttpTransport,
    ) -> Result<String, ConnectionError> {
        let oauth2_cache = facts.oauth_tokens();
        if rejected.is_none()
            && let Some(token) = oauth2_cache.get(key)
        {
            return Ok(token);
        }
        oauth2_cache.remove(key);
        let envelope = facts
            .oauth_encryption()
            .ok_or(ConnectionError::CredentialUnavailable)?;
        let available = wamn_connection::oauth_token::get(
            wamn_connection::oauth_token::Request {
                reader: facts.reader(),
                principal,
                project: facts.project(),
                environment: facts.environment(),
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
                    transport.addresses(),
                )
                .await?;
                let token = fetch_health_token(transport, decision, &request)
                    .await
                    .map_err(|_| anyhow::anyhow!("OAuth provider unavailable"))?;
                Ok(wamn_connection::oauth_token::Fetched {
                    token: wamn_connection::oauth_token::Token {
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
        Ok(available.token)
    }
}
async fn credential(
    facts: &ConnectionFacts,
    answer: &ConnectionAnswer,
    rejected: Option<&Arc<Secret>>,
) -> Result<Option<Arc<Secret>>, ConnectionError> {
    match answer.fact.credential_id {
        Some(id) => facts
            .secret(id, rejected)
            .await
            .map(Some)
            .map_err(|_| ConnectionError::CredentialUnavailable),
        None => Ok(None),
    }
}

async fn execute(
    transport: &HttpTransport,
    decision: wamn_authority::connection_authority::AuthorityDecision,
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
    let headers = outbound_headers(request, credentials).map_err(|error| error)?;
    let body = request.body.as_deref().unwrap_or_default();
    let mut outbound = hyper::Request::builder()
        .method(method)
        .uri(decision.logical_url.as_ref())
        .body(body)
        .map_err(|_| ConnectionError::Incompatible)?;
    *outbound.headers_mut() = headers;
    let response = transport
        .send_once(&decision, outbound)
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "trusted HTTP transport failed");
            if error.is_before_dispatch() {
                ConnectionError::Transport("connection-client-unavailable".to_owned())
            } else if error.is_response_lost() {
                ConnectionError::Transport("connection-response-lost".into())
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

#[async_trait::async_trait]
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
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let WorkloadItem::Component(component) = &*item else {
            anyhow::bail!("HTTP connection requires an application component");
        };
        let bound = Arc::new(Self {
            scopes: Arc::clone(&self.scopes),
            transport: Arc::clone(&self.transport),
            applications: Arc::clone(&self.applications),
            component: Some(component.id().to_owned()),
        });
        item.add_plugin(CONNECTION_HTTP_ID, bound);
        http::add_to_linker::<_, SharedCtx>(item.linker()?, extract_active_ctx)?;
        Ok(())
    }
    async fn on_workload_resolved(
        &self,
        workload: &ResolvedWorkload,
        component_id: &str,
    ) -> anyhow::Result<()> {
        let components = workload.components();
        let components = components.read().await;
        let route = components
            .values()
            .find(|component| component.name() == "ingress")
            .context("ingress missing")?;
        let application =
            wamn_engine::application::bound_application_config(&self.scopes, route.id())
                .context("wamn.application missing")?;
        if application
            .components
            .iter()
            .any(|component| !component.connections.is_empty())
        {
            self.applications
                .write()
                .expect("HTTP application bindings poisoned")
                .insert(
                    component_id.to_owned(),
                    ApplicationBinding {
                        workload: workload.id().to_owned(),
                        application,
                    },
                );
        }
        Ok(())
    }
    async fn on_workload_unbind(&self, workload: &str, _: WitInterfaces<'_>) -> anyhow::Result<()> {
        self.applications
            .write()
            .expect("HTTP application bindings poisoned")
            .retain(|_, binding| binding.workload != workload);
        Ok(())
    }
}
impl http::Host for ActiveCtx<'_> {}
impl<T: Send + 'static> http::HostWithStore<T> for SharedCtx {
    async fn send(
        accessor: &Accessor<T, Self>,
        request: Request,
    ) -> wash_runtime::wasmtime::Result<Result<Response, http::ConnectionError>> {
        let (plugin, scope) = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((
                ctx.try_get_plugin::<ConnectionHttp>(CONNECTION_HTTP_ID)?,
                Arc::clone(&ctx.component_id),
            ))
        })?;
        let trace = accessor
            .with(|mut access| wamn_engine::invocation_trace::invocation_trace(&mut access.get()));
        trace
            .run(async move {
                let context = wamn_connection::invocation::context(&plugin.scopes, &scope).ok();
                let project_key = context
                    .as_ref()
                    .map(|context| context.connections.project_env_key());
                let project = context
                    .as_ref()
                    .map_or("", |context| context.connections.project().as_str());
                let span = effect_span!(
                    "wamn.connection_http",
                    EffectIdentity {
                        project_env_key: project_key,
                        project,
                        component: &scope
                    },
                    None,
                    effect.operation = "send",
                );
                let invocation = wamn_connection::invocation::invocation(&plugin.scopes, &scope);
                record_wiring(
                    &span,
                    invocation
                        .as_ref()
                        .map(wamn_connection::connection::ConnectionInvocation::effect_wiring),
                );
                let mut observed = EffectOutcomeGuard::new(
                    &span,
                    invocation.and_then(|invocation| invocation.effects),
                );
                let started = std::time::Instant::now();
                let result = plugin.send(&scope, &request).instrument(span).await;
                record_effect_ms(
                    &HTTP_EFFECT_DURATION_MS,
                    EFFECT_OPERATION,
                    "send",
                    project,
                    started.elapsed(),
                );
                let outcome = match &result {
                    Ok(_) => EffectOutcome::Responded,
                    Err(ConnectionError::Timeout) => EffectOutcome::Timeout,
                    Err(ConnectionError::Transport(detail))
                        if detail == "connection-client-unavailable" =>
                    {
                        EffectOutcome::RefusedBeforeDispatch
                    }
                    Err(ConnectionError::Transport(detail))
                        if detail == "connection-response-lost" =>
                    {
                        EffectOutcome::ResponseLost
                    }
                    Err(ConnectionError::Transport(_)) => EffectOutcome::EffectUncertain,
                    Err(_) => EffectOutcome::RefusedBeforeDispatch,
                };
                observed.settle(outcome, result.is_err());
                Ok(result.map_err(wit_error))
            })
            .await
    }
}
fn wit_error(error: ConnectionError) -> http::ConnectionError {
    match error {
        ConnectionError::Unbound => http::ConnectionError::Unbound,
        ConnectionError::Incompatible => http::ConnectionError::Incompatible,
        ConnectionError::AuthorityDenied => http::ConnectionError::AuthorityDenied,
        ConnectionError::AttestationInvalid => http::ConnectionError::AttestationInvalid,
        ConnectionError::CredentialUnavailable => http::ConnectionError::CredentialUnavailable,
        ConnectionError::Timeout => http::ConnectionError::Timeout,
        ConnectionError::Transport(detail) => http::ConnectionError::Transport(detail),
        ConnectionError::Unauthorized => http::ConnectionError::Unauthorized,
    }
}

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
    addresses: AddressPolicy,
) -> Result<AuthorityDecision, AuthorityError> {
    let (authority, target, endpoint) = bound_oauth_token(connection, token_authority, token_url)?;
    let mut decision = resolve_http_request(
        &authority,
        &target,
        &[exact_host(&authority)],
        &|address| addresses.allows(address),
        &TokioDnsResolver,
    )
    .await?;
    decision.logical_url = endpoint;
    Ok(decision)
}

/// The one host entry that admits a connection's own authority and no other.
fn exact_host(authority: &HttpConnectionAuthority) -> AllowedHost {
    let url = url::Url::parse(authority.canonical_base_url()).expect("canonical connection URL");
    AllowedHost::Authority(
        format!(
            "{}:{}",
            url.host_str().expect("canonical connection host"),
            authority.authority().port()
        )
        .parse()
        .expect("canonical connection authority"),
    )
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
/// A field the guest already sent stays: `guests/http-request`
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

async fn fetch_health_token(
    transport: &HttpTransport,
    decision: AuthorityDecision,
    request: &Oauth2Request,
) -> Result<Oauth2Token, ConnectionError> {
    request
        .send(&crate::oauth::Transport {
            http: transport.clone(),
            decision,
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
        let authority = wamn_authority::connection_authority::parse_http_definition(parameters)?;
        let url = url::Url::parse(authority.canonical_base_url())?;
        anyhow::ensure!(url.scheme() == "https", "HTTP health requires TLS");
        let (target, endpoint) = bound_http_target(&authority, authority.canonical_base_url())?;
        let mut decision = resolve_http_request(
            &authority,
            &target,
            &[exact_host(&authority)],
            &|address| transport.addresses().allows(address),
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
                transport.addresses(),
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

#[cfg(test)]
mod tests {

    use super::*;
    use crate::bindings::wamn::connection::http::Header;
    use std::sync::Arc;

    use crate::test_util::{effect_request, interface_address};

    mod oauth2_tests;
    mod outside_call_tests;

    /// The hash of the fixture wiring's graph, the one the manifest carries.

    #[test]
    fn guest_headers_cannot_supply_transport_or_proxy_authority() {
        for name in [
            "idempotency-key",
            "host",
            "content-length",
            "transfer-encoding",
            "proxy-authorization",
            "connection",
        ] {
            for name in [name.to_owned(), name.to_ascii_uppercase()] {
                let request = Request {
                    idempotency_key: Some("host".into()),
                    ..effect_request(vec![Header {
                        name,
                        value: b"guest".to_vec(),
                    }])
                };
                assert!(matches!(
                    outbound_headers(&request, HashMap::new()),
                    Err(ConnectionError::AuthorityDenied)
                ));
            }
        }
        let request = Request {
            idempotency_key: Some("host".into()),
            ..effect_request(vec![
                Header {
                    name: "x-http-method".into(),
                    value: b"DELETE".to_vec(),
                },
                Header {
                    name: "X-HTTP-Method-Override".into(),
                    value: b"PUT".to_vec(),
                },
            ])
        };
        let headers = outbound_headers(&request, HashMap::new()).unwrap();
        assert!(!headers.contains_key("x-http-method"));
        assert!(!headers.contains_key("x-http-method-override"));
        assert_eq!(request.method, "POST");
        assert_eq!(
            headers["idempotency-key"],
            request.idempotency_key.as_ref().unwrap()
        );
    }

    #[test]
    fn the_effect_span_is_injected_onto_the_outbound_request() {
        use opentelemetry::propagation::TextMapPropagator as _;
        use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
        opentelemetry::global::set_text_map_propagator(propagator.clone());
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("connection-http-test")),
        );
        let _guard = tracing::subscriber::set_default(subscriber);
        let span = tracing::info_span!("wamn.connection_http");
        let _entered = span.enter();
        let guest = HashMap::from([(
            "traceparent".to_owned(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned(),
        )]);
        let guest_context = propagator.extract(&guest);
        for (fields, expected) in [(HashMap::new(), span.context()), (guest, guest_context)] {
            let request = effect_request(
                fields
                    .into_iter()
                    .map(|(name, value)| Header {
                        name,
                        value: value.into_bytes(),
                    })
                    .collect(),
            );
            let headers = outbound_headers(&request, HashMap::new()).unwrap();
            assert!(!headers.contains_key("tracestate"));
            let fields: HashMap<String, String> = headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
                .collect();
            let actual = propagator.extract(&fields);
            let actual_span = actual.span();
            let actual = actual_span.span_context();
            let expected_span = expected.span();
            let expected = expected_span.span_context();
            assert!(actual.is_valid());
            assert_eq!(actual.trace_id(), expected.trace_id());
            assert_eq!(actual.span_id(), expected.span_id());
            assert_eq!(actual.trace_flags(), expected.trace_flags());
            assert_eq!(actual.trace_state(), expected.trace_state());
        }
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
            super::health_status(status).expect_err("unhealthy status");
        }
    }
}

pub mod oauth;

/// HTTP fixtures for this crate's unit tests.
#[cfg(test)]
mod test_util;
