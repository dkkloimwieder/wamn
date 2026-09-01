//! Host plugin for `wamn:flow-http-routing@0.1.0`.
//!
//! Reader 3 of the four the release-manifest weld enumerates
//! ([`crate::release_manifest`]): it answers `routes` out of
//! [`ServingManifest::attachments`] with no database read. Authentication
//! re-derives the selected attachment's policy from that same weld, verifies the
//! PAT through the system identity reader, and loads the role's exact permission
//! set through the existing callable-HTTP project pool. The plugin never loads,
//! parses, or digest-verifies a manifest of its own and adds no route table over
//! the immutable in-memory projection.
//!
//! # Attachment-owned and adapter-owned fields
//!
//! An attachment definition may author `input-schema` and
//! `raw-body-bytes.maximum`. The former is compiled once per canonical schema
//! hash into this process's immutable release projection; the latter is
//! projected to the adapter. Their documented fallbacks apply only when the
//! corresponding authored field is absent. The mapped-payload ceiling has no
//! attachment carrier and remains adapter-governed; each fallback is justified
//! at its own field in [`route_definition`].

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;

use boon::{Compiler, Draft, SchemaIndex, Schemas};
use opentelemetry::KeyValue;
use serde_json::Value;
use tracing::Instrument as _;
use wamn_catalog::{
    AttachmentKind, NO_AUTHENTICATION_MODE, PAT_AUTHENTICATION_MODE, ServingAttachment,
    ServingManifest,
};
use wamn_platform_identity::{PrincipalKind, authenticate_pat, project_roles};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Resource;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::release_manifest::ReleaseManifestWeld;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "flow-http-routing-plugin",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:flow-http-routing/routing.route-permit": super::RoutePermit,
            "wamn:flow-http-routing/routing.authenticated-caller": super::AuthenticatedCaller,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::flow_http_routing::routing::{
    self, AuthRejection, Cardinality, Header, Mapping, MappingSource, RouteDefinition,
};

pub const FLOW_HTTP_ROUTING_ID: &str = "wamn-flow-http-routing";

/// Per-route concurrency supplied to every host unless its chart overrides it.
pub const DEFAULT_HTTP_ROUTE_IN_FLIGHT_LIMIT: usize = 64;

/// Host environment key rendered by the platform chart.
pub const HTTP_ROUTE_IN_FLIGHT_LIMIT_ENV: &str = "WAMN_HTTP_ROUTE_IN_FLIGHT_LIMIT";

const UNKNOWN_ROUTE_REFUSAL: &str = "http-route-not-in-release";
const ROUTE_LABEL: &str = "wamn.attachment.id";

/// A non-zero per-route concurrency ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteInFlightLimit(NonZeroUsize);

impl RouteInFlightLimit {
    fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for RouteInFlightLimit {
    fn default() -> Self {
        Self(
            NonZeroUsize::new(DEFAULT_HTTP_ROUTE_IN_FLIGHT_LIMIT)
                .expect("the default HTTP route limit is non-zero"),
        )
    }
}

impl std::fmt::Display for RouteInFlightLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.get().fmt(formatter)
    }
}

impl FromStr for RouteInFlightLimit {
    type Err = InvalidRouteInFlightLimit;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<NonZeroUsize>()
            .map(Self)
            .map_err(|_| InvalidRouteInFlightLimit)
    }
}

/// The configured host route limit is zero, non-Unicode, or not a positive integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRouteInFlightLimit;

impl std::fmt::Display for InvalidRouteInFlightLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HTTP route in-flight limit must be a non-zero integer")
    }
}

impl std::error::Error for InvalidRouteInFlightLimit {}

#[derive(Debug, Default)]
struct RouteState {
    in_flight: usize,
    shed: u64,
}

#[derive(Debug)]
struct RouteLimiter {
    limit: RouteInFlightLimit,
    routes: std::sync::Mutex<HashMap<String, RouteState>>,
}

impl RouteLimiter {
    fn new(limit: RouteInFlightLimit) -> Arc<Self> {
        let limiter = Arc::new(Self {
            limit,
            routes: std::sync::Mutex::new(HashMap::new()),
        });
        Self::register_metrics(&limiter);
        limiter
    }

    fn register_metrics(limiter: &Arc<Self>) {
        let meter = opentelemetry::global::meter("wamn-flow-http");

        let weak = Arc::downgrade(limiter);
        let _ = meter
            .u64_observable_gauge("wamn.http.route.in_flight")
            .with_description("inline HTTP router deliveries currently in flight per attachment")
            .with_callback(move |observer| {
                let Some(limiter) = weak.upgrade() else {
                    return;
                };
                if let Ok(routes) = limiter.routes.lock() {
                    for (route, state) in routes.iter() {
                        observer.observe(
                            state.in_flight as u64,
                            &[KeyValue::new(ROUTE_LABEL, route.clone())],
                        );
                    }
                }
            })
            .build();

        let weak = Arc::downgrade(limiter);
        let _ = meter
            .u64_observable_counter("wamn.http.route.shed")
            .with_description("inline HTTP requests refused because their route was at capacity")
            .with_callback(move |observer| {
                let Some(limiter) = weak.upgrade() else {
                    return;
                };
                if let Ok(routes) = limiter.routes.lock() {
                    for (route, state) in routes.iter() {
                        observer.observe(state.shed, &[KeyValue::new(ROUTE_LABEL, route.clone())]);
                    }
                }
            })
            .build();
    }

    fn try_acquire(self: &Arc<Self>, route: &str) -> Option<RoutePermit> {
        let mut routes = self
            .routes
            .lock()
            .expect("HTTP route limiter lock is not poisoned");
        let state = routes.entry(route.to_string()).or_default();
        if state.in_flight >= self.limit.get() {
            state.shed = state.shed.saturating_add(1);
            return None;
        }
        state.in_flight += 1;
        Some(RoutePermit {
            route: route.to_string(),
            limiter: Arc::clone(self),
        })
    }

    fn finish(&self, route: &str) {
        let mut routes = self
            .routes
            .lock()
            .expect("HTTP route limiter lock is not poisoned");
        let state = routes
            .get_mut(route)
            .expect("a route permit belongs to a recorded route");
        state.in_flight = state
            .in_flight
            .checked_sub(1)
            .expect("a route permit releases exactly once");
    }

    #[cfg(test)]
    fn snapshot(&self, route: &str) -> Option<(usize, u64)> {
        let routes = self
            .routes
            .lock()
            .expect("HTTP route limiter lock is not poisoned");
        routes.get(route).map(|state| (state.in_flight, state.shed))
    }
}

/// One admitted route slot. Dropping it is the only release operation.
#[derive(Debug)]
pub struct RoutePermit {
    route: String,
    limiter: Arc<RouteLimiter>,
}

impl Drop for RoutePermit {
    fn drop(&mut self) {
        self.limiter.finish(&self.route);
    }
}

/// The authored spelling for "any authority", normalized by the exposure
/// resolver and matched verbatim by the adapter.
const WILDCARD_HOST: &str = "*";

const INPUT_SCHEMA_URI: &str = "mem://route-input.json";
const SCHEMA_INVALID: &str = "schema-invalid";

/// A byte ceiling that can never bind, so the adapter's own limit governs.
///
/// `u32::MAX` rather than `u64::MAX` because the guest narrows this with
/// `usize::try_from` and `usize` is 32 bits on wasm32: a wider value would fail
/// that conversion and turn every `routes` call into a 503.
const ADAPTER_GOVERNED_BYTES: u64 = u32::MAX as u64;

const ROUTE_CALLER_ROLE: &str = "route-caller";
const UNAUTHORIZED_STATUS: u16 = 401;
const UNAUTHORIZED_CODE: &str = "unauthorized";
const AUTHENTICATION_UNAVAILABLE_STATUS: u16 = 503;
const AUTHENTICATION_UNAVAILABLE_CODE: &str = "authentication-unavailable";
/// 501, because the request is well formed and it is the *host* that lacks the
/// mechanism — the caller can do nothing to satisfy a policy nothing implements.
const UNSUPPORTED_POLICY_STATUS: u16 = 501;
const UNSUPPORTED_POLICY_CODE: &str = "auth-policy-unsupported";

struct CompiledInputSchema {
    schemas: Schemas,
    index: SchemaIndex,
}

enum InputSchemaValidator {
    Compiled(CompiledInputSchema),
    Invalid,
}

/// Process-lifetime validation projection of the immutable serving manifest.
struct InputSchemaValidators {
    attachment_hashes: HashMap<String, String>,
    validators: HashMap<String, InputSchemaValidator>,
}

impl InputSchemaValidators {
    fn new(release: Option<&ReleaseManifestWeld>) -> Self {
        let Some(release) = release else {
            return Self {
                attachment_hashes: HashMap::new(),
                validators: HashMap::new(),
            };
        };
        let mut attachment_hashes = HashMap::new();
        let mut validators = HashMap::new();
        for (attachment_id, attachment) in &release.manifest().attachments {
            if !carries_http_route(attachment.kind)
                || route_definition(attachment_id, attachment).is_none()
            {
                continue;
            }
            let schema = attachment
                .definition
                .get("input-schema")
                .cloned()
                .unwrap_or(Value::Bool(true));
            let hash = wamn_execution_contract::canonical_json_sha256(&schema);
            attachment_hashes.insert(attachment_id.clone(), hash.clone());
            validators
                .entry(hash.clone())
                .or_insert_with(|| compile_input_schema(&hash, schema));
        }
        Self {
            attachment_hashes,
            validators,
        }
    }

    fn validate(&self, attachment_id: &str, payload: &str) -> Result<(), &'static str> {
        let hash = self
            .attachment_hashes
            .get(attachment_id)
            .ok_or(SCHEMA_INVALID)?;
        let validator = self.validators.get(hash).ok_or(SCHEMA_INVALID)?;
        let payload = serde_json::from_str(payload).map_err(|_| SCHEMA_INVALID)?;
        match validator {
            InputSchemaValidator::Compiled(compiled) => compiled
                .schemas
                .validate(&payload, compiled.index)
                .map_err(|_| SCHEMA_INVALID),
            InputSchemaValidator::Invalid => Err(SCHEMA_INVALID),
        }
    }
}

fn compile_input_schema(hash: &str, schema: Value) -> InputSchemaValidator {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    let mut schemas = Schemas::new();
    let compiled = compiler
        .add_resource(INPUT_SCHEMA_URI, schema)
        .and_then(|()| compiler.compile(INPUT_SCHEMA_URI, &mut schemas));
    match compiled {
        Ok(index) => InputSchemaValidator::Compiled(CompiledInputSchema { schemas, index }),
        Err(error) => {
            tracing::warn!(
                schema_hash = hash,
                error = %error,
                "release route input schema is invalid"
            );
            InputSchemaValidator::Invalid
        }
    }
}

/// Host-owned proof of an originating caller and its exact operation grants.
///
/// The guest can hold only the resource handle. It cannot construct this value,
/// inspect the grant set, or replace the principal while forwarding it to router
/// delivery.
#[derive(Clone)]
pub struct AuthenticatedCaller {
    attachment_id: Box<str>,
    principal_id: Box<str>,
    permissions: Arc<HashSet<String>>,
}

impl std::fmt::Debug for AuthenticatedCaller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedCaller")
            .field("attachment_id", &self.attachment_id)
            .field("principal_id", &self.principal_id)
            .field("permission_count", &self.permissions.len())
            .finish_non_exhaustive()
    }
}

impl AuthenticatedCaller {
    /// Return the immutable attachment identity whose policy minted this proof.
    pub fn attachment_id(&self) -> &str {
        &self.attachment_id
    }

    /// Return the opaque platform principal used by router traces and refusals.
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Check one exact registered-operation token.
    pub fn permits(&self, operation: &str) -> bool {
        self.permissions.contains(operation)
    }
}

/// Trusted dependencies and scope for PAT-backed route authentication.
pub struct RouteAuthentication {
    identity_reader: Arc<tokio_postgres::Client>,
    postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    org: Box<str>,
    project: Box<str>,
    expected_subject: Box<str>,
}

impl std::fmt::Debug for RouteAuthentication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteAuthentication")
            .field("org", &self.org)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl RouteAuthentication {
    /// Bind the two read authorities to trusted package coordinates.
    ///
    /// Environment and tenant remain single-sourced from the welded release.
    pub fn new(
        identity_reader: Arc<tokio_postgres::Client>,
        postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
        org: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        expected_subject: impl Into<Box<str>>,
    ) -> Self {
        Self {
            identity_reader,
            postgres,
            org: org.into(),
            project: project.into(),
            expected_subject: expected_subject.into(),
        }
    }
}

/// This process was given no release, so it can answer no route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoRelease;

impl std::fmt::Display for NoRelease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("this process carries no release manifest")
    }
}

impl std::error::Error for NoRelease {}

/// Authoritative HTTP route supply for the flow-http adapter.
pub struct FlowHttpRouting {
    /// `None` in a process that was given no manifest root. Absence is a
    /// deployment fact, not a fallback: a gate or a bench runs this host with
    /// nothing mounted, and every `routes` call on it refuses rather than serving
    /// routes from somewhere else. A *serving* pod is given a manifest root, and a
    /// root it cannot load refuses host construction outright — that decision is
    /// made where it is visible, at the construction site in
    /// `services/host/src/host.rs`, not here behind an `Option`.
    release: Option<Arc<ReleaseManifestWeld>>,
    input_schemas: InputSchemaValidators,
    authentication: Option<Arc<RouteAuthentication>>,
    limiter: Arc<RouteLimiter>,
}

/// Hand-written so a debug print names the release rather than dumping the whole
/// manifest — route definitions and resolved auth-source documents included.
impl std::fmt::Debug for FlowHttpRouting {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FlowHttpRouting")
            .field(
                "release",
                &self.release.as_deref().map(|weld| weld.release()),
            )
            .field(
                "input_schema_count",
                &self.input_schemas.attachment_hashes.len(),
            )
            .field(
                "compiled_input_schema_count",
                &self.input_schemas.validators.len(),
            )
            .field("route_in_flight_limit", &self.limiter.limit)
            .field("authentication_configured", &self.authentication.is_some())
            .finish_non_exhaustive()
    }
}

impl FlowHttpRouting {
    /// Bind this plugin to the release and per-route host ceiling.
    pub fn new(
        release: Option<Arc<ReleaseManifestWeld>>,
        route_in_flight_limit: RouteInFlightLimit,
    ) -> Self {
        let input_schemas = InputSchemaValidators::new(release.as_deref());
        Self {
            release,
            input_schemas,
            authentication: None,
            limiter: RouteLimiter::new(route_in_flight_limit),
        }
    }

    /// Enable PAT authentication with host-selected database authorities.
    #[must_use]
    pub fn with_authentication(mut self, authentication: Arc<RouteAuthentication>) -> Self {
        self.authentication = Some(authentication);
        self
    }

    /// Read the chart-carried route ceiling, defaulting only when it is absent.
    pub fn from_env(
        release: Option<Arc<ReleaseManifestWeld>>,
    ) -> Result<Self, InvalidRouteInFlightLimit> {
        let limit = match std::env::var(HTTP_ROUTE_IN_FLIGHT_LIMIT_ENV) {
            Ok(value) => value.parse()?,
            Err(std::env::VarError::NotPresent) => RouteInFlightLimit::default(),
            Err(std::env::VarError::NotUnicode(_)) => return Err(InvalidRouteInFlightLimit),
        };
        Ok(Self::new(release, limit))
    }

    fn routes(&self, method: &str, authority: &str) -> Result<Vec<RouteDefinition>, NoRelease> {
        let weld = self.release.as_ref().ok_or(NoRelease)?;
        Ok(route_definitions(weld.manifest(), method, authority))
    }

    fn carries_route(&self, attachment_id: &str) -> Result<bool, NoRelease> {
        let weld = self.release.as_ref().ok_or(NoRelease)?;
        Ok(weld
            .manifest()
            .attachments
            .get(attachment_id)
            .is_some_and(|attachment| carries_http_route(attachment.kind)))
    }

    fn validate_input(&self, attachment_id: &str, payload: &str) -> Result<(), &'static str> {
        self.input_schemas.validate(attachment_id, payload)
    }

    async fn authenticate(
        &self,
        attachment_id: &str,
        headers: &[Header],
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let weld = self
            .release
            .as_ref()
            .ok_or_else(authentication_unavailable)?;
        let manifest = weld.manifest();
        let attachment = manifest
            .attachments
            .get(attachment_id)
            .filter(|attachment| carries_http_route(attachment.kind))
            .ok_or_else(authentication_unavailable)?;
        let mode = attachment.auth_policy.get("mode").and_then(Value::as_str);
        if mode == Some(NO_AUTHENTICATION_MODE) {
            return Ok(None);
        }
        if mode != Some(PAT_AUTHENTICATION_MODE) {
            return Err(AuthRejection {
                status: UNSUPPORTED_POLICY_STATUS,
                code: UNSUPPORTED_POLICY_CODE.to_string(),
            });
        }
        let span = tracing::info_span!(
            target: "wamn::route",
            "wamn.route.authenticate",
            wamn.attachment_id = %attachment_id,
        );
        async {
            let authentication = self
                .authentication
                .as_ref()
                .ok_or_else(authentication_unavailable)?;
            let token = required_bearer_token(headers)?;
            let principal = authenticate_pat(authentication.identity_reader.as_ref(), token)
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route PAT authentication unavailable");
                    authentication_unavailable()
                })?
                .ok_or_else(unauthorized)?;
            let principal = principal.principal();
            if principal.kind() != PrincipalKind::Service
                || principal.subject() != authentication.expected_subject.as_ref()
            {
                return Err(unauthorized());
            }
            let roles = project_roles(
                authentication.identity_reader.as_ref(),
                principal.id(),
                &authentication.org,
                &authentication.project,
            )
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "route caller role lookup unavailable");
                authentication_unavailable()
            })?;
            if !roles.iter().any(|role| role.as_str() == ROUTE_CALLER_ROLE) {
                return Err(unauthorized());
            }
            let permissions = authentication
                .postgres
                .operation_permissions(
                    &authentication.project,
                    &manifest.release.tenant_id,
                    ROUTE_CALLER_ROLE,
                )
                .await
                .map_err(|error| {
                    tracing::warn!(error = %error, "route operation grants unavailable");
                    authentication_unavailable()
                })?;
            Ok(Some(AuthenticatedCaller {
                attachment_id: attachment_id.into(),
                principal_id: principal.id().as_str().into(),
                permissions: Arc::new(permissions.into_iter().collect()),
            }))
        }
        .instrument(span)
        .await
    }

    /// Exercise production route authentication from an integration proof.
    #[cfg(feature = "test-util")]
    pub async fn authenticate_authorization_for_test(
        &self,
        attachment_id: &str,
        authorization: Option<&str>,
    ) -> Result<Option<AuthenticatedCaller>, (u16, String)> {
        let headers = authorization
            .map(|value| Header {
                name: "authorization".to_string(),
                value: value.to_string(),
            })
            .into_iter()
            .collect::<Vec<_>>();
        self.authenticate(attachment_id, &headers)
            .await
            .map_err(|rejection| (rejection.status, rejection.code))
    }
}

/// Every candidate the adapter could select for this request.
///
/// Free of `self` and of the weld so the projection can be proven against a
/// manifest fixture without a mount.
fn route_definitions(
    manifest: &ServingManifest,
    method: &str,
    authority: &str,
) -> Vec<RouteDefinition> {
    manifest
        .attachments
        .iter()
        .filter(|(_, attachment)| carries_http_route(attachment.kind))
        .filter_map(|(attachment_id, attachment)| {
            // Decoded before it is matched, so a malformed attachment is reported
            // whenever this pod serves at all rather than only once some request
            // happens to name its route.
            let definition = route_definition(attachment_id, attachment);
            if definition.is_none() {
                tracing::warn!(
                    attachment_id = attachment_id.as_str(),
                    "release attachment carries no serviceable HTTP route"
                );
            }
            definition
        })
        .filter(|definition| matches_request(definition, method, authority))
        .collect()
}

/// The two kinds that carry an HTTP route. An `internal` or `cron` attachment has
/// none and must never be reachable over HTTP.
fn carries_http_route(kind: AttachmentKind) -> bool {
    matches!(kind, AttachmentKind::Http | AttachmentKind::Studio)
}

/// Return whether a serving release requires PAT-backed route authentication.
///
/// Only externally selectable HTTP route kinds participate. An internal or
/// cron attachment carrying an otherwise identical document cannot make the
/// host acquire route-authentication credentials it will never use.
pub fn requires_pat_route_authentication(manifest: &ServingManifest) -> bool {
    manifest
        .attachments
        .iter()
        .filter(|(_, attachment)| carries_http_route(attachment.kind))
        .any(|(attachment_id, attachment)| {
            route_definition(attachment_id, attachment).is_some()
                && attachment.auth_policy.get("mode").and_then(Value::as_str)
                    == Some(PAT_AUTHENTICATION_MODE)
        })
}

/// Exactly the host and method predicates the adapter's own `select_route`
/// applies (`components/ingress/http-route/src/lib.rs`).
///
/// Mirrored rather than tightened on purpose: this provider returns candidates and
/// the adapter performs final selection and path matching, so a candidate dropped
/// here is one the adapter would have accepted. Case cannot be the reason a route
/// is missed — the adapter uppercases the method and lowercases the authority
/// before it asks, and the exposure resolver normalizes the projection the same
/// way, but neither normalization is assumed.
fn matches_request(definition: &RouteDefinition, method: &str, authority: &str) -> bool {
    definition.method.eq_ignore_ascii_case(method)
        && (definition.host == WILDCARD_HOST || definition.host.eq_ignore_ascii_case(authority))
}

/// One attachment's route definition, or `None` when its definition document
/// carries no route this host can serve.
///
/// The fields are read off the definition `Value` rather than through the
/// authoring-side decoder. Keys this host does not serve are ignored rather than
/// refused: the document's shape is owned by the exposure boundary, and a
/// producer adding a field must not take a pod's routing offline.
fn route_definition(
    attachment_id: &str,
    attachment: &ServingAttachment,
) -> Option<RouteDefinition> {
    let route = attachment.definition.get("route")?;
    let body_limit = match attachment.definition.get("raw-body-bytes") {
        Some(raw_body_bytes) => {
            let authored = raw_body_bytes.get("maximum")?.as_u64()?;
            u32::try_from(authored).ok()?.into()
        }
        None => ADAPTER_GOVERNED_BYTES,
    };
    let mappings = match attachment.definition.get("mappings") {
        Some(mappings) => mappings
            .as_array()?
            .iter()
            .map(input_mapping)
            .collect::<Option<Vec<_>>>()?,
        None => Vec::new(),
    };
    Some(RouteDefinition {
        attachment_id: attachment_id.to_string(),
        host: route.get("host")?.as_str()?.to_string(),
        path: route.get("path")?.as_str()?.to_string(),
        method: route.get("method")?.as_str()?.to_string(),
        mappings,
        body_limit,
        // No authored mapped-payload ceiling exists. This value leaves the
        // adapter's own mapped-byte limit in charge rather than inventing a
        // second policy here.
        mapped_limit: ADAPTER_GOVERNED_BYTES,
    })
}

fn input_mapping(value: &Value) -> Option<Mapping> {
    Some(Mapping {
        from: match value.get("from")?.as_str()? {
            "body" => MappingSource::Body,
            "path" => MappingSource::Path,
            "query" => MappingSource::Query,
            "header" => MappingSource::Header,
            _ => return None,
        },
        name: value.get("name")?.as_str()?.to_string(),
        to: value.get("to")?.as_str()?.to_string(),
        // Both keys are serde defaults on the authored shape, so absence is legal
        // and means that default; a present value of the wrong type is not, and
        // fails the whole attachment closed.
        optional: match value.get("optional") {
            Some(optional) => optional.as_bool()?,
            None => false,
        },
        cardinality: match value.get("cardinality") {
            Some(cardinality) => match cardinality.as_str()? {
                "one" => Cardinality::One,
                "many" => Cardinality::Many,
                _ => return None,
            },
            None => Cardinality::One,
        },
    })
}

/// Extract one standard bearer presentation without exposing why it failed.
fn bearer_token(headers: &[Header]) -> Option<&str> {
    let mut values = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("authorization"));
    let value = values.next()?.value.as_str();
    if values.next().is_some() {
        return None;
    }
    let mut fields = value.split_ascii_whitespace();
    let scheme = fields.next()?;
    let token = fields.next()?;
    (scheme.eq_ignore_ascii_case("bearer") && fields.next().is_none()).then_some(token)
}

fn required_bearer_token(headers: &[Header]) -> Result<&str, AuthRejection> {
    bearer_token(headers).ok_or_else(unauthorized)
}

fn unauthorized() -> AuthRejection {
    AuthRejection {
        status: UNAUTHORIZED_STATUS,
        code: UNAUTHORIZED_CODE.to_string(),
    }
}

fn authentication_unavailable() -> AuthRejection {
    AuthRejection {
        status: AUTHENTICATION_UNAVAILABLE_STATUS,
        code: AUTHENTICATION_UNAVAILABLE_CODE.to_string(),
    }
}

#[async_trait::async_trait]
impl HostPlugin for FlowHttpRouting {
    fn id(&self) -> &'static str {
        FLOW_HTTP_ROUTING_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:flow-http-routing/routing@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    /// `world` only decides whether this callback fires; the linker entry is made
    /// here or the guest's import resolves to nothing.
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "flow-http-routing", &["routing"]) {
            return Ok(());
        }
        routing::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        Ok(())
    }
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<FlowHttpRouting>> {
    ctx.try_get_plugin::<FlowHttpRouting>(FLOW_HTTP_ROUTING_ID)
}

impl routing::Host for ActiveCtx<'_> {
    async fn routes(
        &mut self,
        method: String,
        authority: String,
    ) -> wash_runtime::wasmtime::Result<Result<Vec<RouteDefinition>, String>> {
        let plugin = plugin_of(self)?;
        Ok(plugin.routes(&method, &authority).map_err(|error| {
            tracing::warn!(method, authority, error = %error, "flow-http route supply refused");
            error.to_string()
        }))
    }

    async fn authenticate(
        &mut self,
        attachment_id: String,
        headers: Vec<Header>,
    ) -> wash_runtime::wasmtime::Result<Result<Option<Resource<AuthenticatedCaller>>, AuthRejection>>
    {
        let plugin = plugin_of(self)?;
        let caller = match plugin.authenticate(&attachment_id, &headers).await {
            Ok(caller) => caller,
            Err(rejection) => return Ok(Err(rejection)),
        };
        Ok(Ok(caller
            .map(|caller| self.table.push(caller))
            .transpose()?))
    }

    async fn validate_input(
        &mut self,
        attachment_id: String,
        payload: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), String>> {
        let plugin = plugin_of(self)?;
        Ok(plugin
            .validate_input(&attachment_id, &payload)
            .map_err(str::to_owned))
    }

    async fn try_acquire(
        &mut self,
        attachment_id: String,
    ) -> wash_runtime::wasmtime::Result<Result<Option<Resource<RoutePermit>>, String>> {
        let plugin = plugin_of(self)?;
        match plugin.carries_route(&attachment_id) {
            Ok(true) => {}
            Ok(false) => return Ok(Err(UNKNOWN_ROUTE_REFUSAL.to_string())),
            Err(error) => {
                tracing::warn!(
                    attachment_id,
                    error = %error,
                    "HTTP route permit refused without a serving release"
                );
                return Ok(Err(error.to_string()));
            }
        }
        let Some(permit) = plugin.limiter.try_acquire(&attachment_id) else {
            return Ok(Ok(None));
        };
        Ok(Ok(Some(self.table.push(permit)?)))
    }
}

impl routing::HostRoutePermit for ActiveCtx<'_> {
    async fn drop(&mut self, permit: Resource<RoutePermit>) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(permit)?;
        Ok(())
    }
}

impl routing::HostAuthenticatedCaller for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        caller: Resource<AuthenticatedCaller>,
    ) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(caller)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use serde_json::json;
    use wamn_catalog::{
        ArtifactHash, DefinitionHash, EffectiveReleaseId, PackageCoordinate,
        RELEASE_MANIFEST_FILE_NAME, ServingComponent, ServingComponentOperation, ServingRelease,
        ServingWiring,
    };

    use super::*;

    const COMPONENT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GRAPH: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DEFINITION_HASH: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const EFFECTIVE_RELEASE_ID: u32 = 7;

    fn components() -> BTreeSet<ServingComponent> {
        BTreeSet::from([ServingComponent {
            package_id: "cat".into(),
            component: "http-request".into(),
            interface_version: "0.1".into(),
            digest: ArtifactHash::parse(COMPONENT).expect("fixture artifact hash is canonical"),
            operations: BTreeMap::from([(
                "request".into(),
                ServingComponentOperation {
                    registered_operation: None,
                },
            )]),
        }])
    }

    fn wirings() -> BTreeSet<ServingWiring> {
        BTreeSet::from([ServingWiring {
            package_id: "cat".into(),
            wiring_id: "orders".into(),
            wiring_version: 1,
            graph_hash: DefinitionHash::parse(GRAPH).expect("fixture definition hash is canonical"),
        }])
    }

    fn attachment(kind: AttachmentKind, definition: Value) -> ServingAttachment {
        ServingAttachment {
            kind,
            package_id: "cat".into(),
            wiring_id: "orders".into(),
            wiring_version: 1,
            definition_hash: DefinitionHash::parse(DEFINITION_HASH)
                .expect("fixture definition hash is canonical"),
            definition,
            auth_policy: json!({"mode": "none"}),
            registered_operation: None,
        }
    }

    /// The authored attachment document, exactly as the exposure boundary
    /// normalizes and stores it.
    fn orders_definition() -> Value {
        json!({
            "id": "orders",
            "kind": "http",
            "source-id": "public",
            "route": {"host": "api.example.test", "path": "/orders/{order}", "method": "POST"},
            "mappings": [
                {"from": "body", "name": "amount", "to": "/amount"},
                {"from": "path", "name": "order", "to": "/order", "optional": false},
                {"from": "query", "name": "tag", "to": "/tags", "cardinality": "many"}
            ]
        })
    }

    fn release_manifest(attachments: BTreeMap<String, ServingAttachment>) -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                tenant_id: "tenant-a".into(),
                effective_release_id: EffectiveReleaseId::new(EFFECTIVE_RELEASE_ID).unwrap(),
                environment: "prod".into(),
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            components(),
            wirings(),
            attachments,
            BTreeMap::new(),
        )
        .expect("fixture manifest is valid")
    }

    fn one_http_route() -> ServingManifest {
        release_manifest(BTreeMap::from([(
            "orders".to_string(),
            attachment(AttachmentKind::Http, orders_definition()),
        )]))
    }

    fn served_ids(served: &[RouteDefinition]) -> Vec<&str> {
        served
            .iter()
            .map(|definition| definition.attachment_id.as_str())
            .collect()
    }

    /// The wire spellings a mapping round-trips through, so the assertion holds
    /// whatever the generated enums do or do not derive.
    fn mapping_shape(mapping: &Mapping) -> (&'static str, &str, &str, bool, &'static str) {
        (
            match mapping.from {
                MappingSource::Body => "body",
                MappingSource::Path => "path",
                MappingSource::Query => "query",
                MappingSource::Header => "header",
            },
            mapping.name.as_str(),
            mapping.to.as_str(),
            mapping.optional,
            match mapping.cardinality {
                Cardinality::One => "one",
                Cardinality::Many => "many",
            },
        )
    }

    /// A scratch manifest mount, named for its test so runs cannot collide.
    ///
    /// The weld has exactly one constructor and it reads a file, so a test that
    /// needs a weld writes the mount: there is no shortcut past the verification a
    /// serving pod performs, not even here.
    struct Mount {
        root: PathBuf,
    }

    impl Mount {
        fn holding(manifest: &ServingManifest, test: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "wamn-flow-http-routing-{}-{test}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("scratch mount");
            std::fs::write(
                root.join(RELEASE_MANIFEST_FILE_NAME),
                manifest.canonical_bytes(),
            )
            .expect("write manifest");
            Self { root }
        }

        fn weld(&self) -> Arc<ReleaseManifestWeld> {
            Arc::new(ReleaseManifestWeld::load_from(&self.root).expect("fixture mount loads"))
        }
    }

    impl Drop for Mount {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_matching_method_and_authority_serves_the_release_attachment() {
        let manifest = one_http_route();

        let served = route_definitions(&manifest, "POST", "api.example.test");

        let [definition] = served.as_slice() else {
            panic!("exactly one attachment matches this request");
        };
        assert_eq!(definition.attachment_id, "orders");
        assert_eq!(definition.host, "api.example.test");
        assert_eq!(definition.path, "/orders/{order}");
        assert_eq!(definition.method, "POST");
        assert_eq!(
            definition
                .mappings
                .iter()
                .map(mapping_shape)
                .collect::<Vec<_>>(),
            vec![
                ("body", "amount", "/amount", false, "one"),
                ("path", "order", "/order", false, "one"),
                ("query", "tag", "/tags", false, "many"),
            ]
        );
    }

    /// Omitted attachment fields leave their downstream authorities in charge.
    #[test]
    fn omitted_projection_fields_defer_to_their_real_authority() {
        let manifest = one_http_route();
        let mount = Mount::holding(&manifest, "unconstrained-input");
        let plugin = FlowHttpRouting::new(Some(mount.weld()), RouteInFlightLimit::default());

        let served = route_definitions(&manifest, "POST", "api.example.test");

        let [definition] = served.as_slice() else {
            panic!("exactly one attachment matches this request");
        };
        assert_eq!(definition.body_limit, ADAPTER_GOVERNED_BYTES);
        assert_eq!(definition.mapped_limit, ADAPTER_GOVERNED_BYTES);
        plugin
            .validate_input("orders", r#"{"any":"json"}"#)
            .expect("the absent input schema is the unconstrained true schema");
    }

    #[test]
    fn authored_input_schema_and_raw_body_limit_stay_with_their_owners() {
        let input_schema = json!({
            "type": "array",
            "minItems": 1,
            "maxItems": 100,
            "items": {
                "type": "object",
                "required": ["request_id"],
                "properties": {
                    "request_id": {"type": "string"},
                },
                "additionalProperties": false,
            },
        });
        let mut definition = orders_definition();
        definition["input-schema"] = input_schema.clone();
        definition["raw-body-bytes"] = json!({"maximum": 1_048_576});
        let manifest = release_manifest(BTreeMap::from([(
            "orders".to_string(),
            attachment(AttachmentKind::Http, definition),
        )]));
        let mount = Mount::holding(&manifest, "authored-input");
        let plugin = FlowHttpRouting::new(Some(mount.weld()), RouteInFlightLimit::default());

        let served = route_definitions(&manifest, "POST", "api.example.test");

        let [definition] = served.as_slice() else {
            panic!("exactly one attachment matches this request");
        };
        assert_eq!(definition.body_limit, 1_048_576);
        assert_eq!(definition.mapped_limit, ADAPTER_GOVERNED_BYTES);
        plugin
            .validate_input("orders", r#"[{"request_id":"r-1"}]"#)
            .expect("the authored schema accepts its matching payload");
        assert_eq!(
            plugin.validate_input("orders", r#"{"request_id":"r-1"}"#),
            Err(SCHEMA_INVALID)
        );
    }

    #[test]
    fn canonical_schema_hash_deduplicates_reordered_schemas_and_isolates_distinct_ones() {
        let first: Value = serde_json::from_str(
            r#"{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}}"#,
        )
        .expect("first schema parses");
        let reordered: Value = serde_json::from_str(
            r#"{"properties":{"id":{"type":"string"}},"required":["id"],"type":"object"}"#,
        )
        .expect("reordered schema parses");
        let distinct = json!({"type": "integer"});
        let with_schema = |id: &str, schema: Value| {
            let mut definition = orders_definition();
            definition["id"] = json!(id);
            definition["route"]["path"] = json!(format!("/{id}"));
            definition["input-schema"] = schema;
            attachment(AttachmentKind::Http, definition)
        };
        let manifest = release_manifest(BTreeMap::from([
            ("first".to_string(), with_schema("first", first)),
            ("reordered".to_string(), with_schema("reordered", reordered)),
            ("distinct".to_string(), with_schema("distinct", distinct)),
        ]));
        let mount = Mount::holding(&manifest, "schema-dedup");
        let plugin = FlowHttpRouting::new(Some(mount.weld()), RouteInFlightLimit::default());

        assert_eq!(
            plugin.input_schemas.attachment_hashes["first"],
            plugin.input_schemas.attachment_hashes["reordered"]
        );
        assert_ne!(
            plugin.input_schemas.attachment_hashes["first"],
            plugin.input_schemas.attachment_hashes["distinct"]
        );
        assert_eq!(plugin.input_schemas.validators.len(), 2);
        plugin
            .validate_input("first", r#"{"id":"order-1"}"#)
            .expect("the shared object schema accepts an object");
        plugin
            .validate_input("reordered", r#"{"id":"order-2"}"#)
            .expect("the reordered schema uses the same validator");
        plugin
            .validate_input("distinct", "7")
            .expect("the distinct integer schema keeps its own validator");
        assert_eq!(
            plugin.validate_input("distinct", r#"{"id":"order-1"}"#),
            Err(SCHEMA_INVALID)
        );
    }

    #[test]
    fn invalid_schema_and_nonmatching_payload_share_the_exact_refusal() {
        let mut invalid_definition = orders_definition();
        invalid_definition["route"]["path"] = json!("/invalid");
        invalid_definition["input-schema"] = json!({"type": 7});
        let mut string_definition = orders_definition();
        string_definition["route"]["path"] = json!("/string");
        string_definition["input-schema"] = json!({"type": "string"});
        let manifest = release_manifest(BTreeMap::from([
            (
                "invalid".to_string(),
                attachment(AttachmentKind::Http, invalid_definition),
            ),
            (
                "string".to_string(),
                attachment(AttachmentKind::Http, string_definition),
            ),
        ]));
        let mount = Mount::holding(&manifest, "schema-invalid");
        let plugin = FlowHttpRouting::new(Some(mount.weld()), RouteInFlightLimit::default());
        let invalid_hash = &plugin.input_schemas.attachment_hashes["invalid"];

        assert!(matches!(
            plugin.input_schemas.validators.get(invalid_hash),
            Some(InputSchemaValidator::Invalid)
        ));
        assert_eq!(
            plugin.validate_input("invalid", r#""anything""#),
            Err(SCHEMA_INVALID)
        );
        assert_eq!(plugin.validate_input("string", "7"), Err(SCHEMA_INVALID));
        assert_eq!(
            plugin.validate_input("missing", r#""anything""#),
            Err(SCHEMA_INVALID)
        );
    }

    #[test]
    fn a_present_raw_body_limit_never_falls_back_when_malformed() {
        for raw_body_bytes in [
            json!({}),
            json!({"maximum": "1048576"}),
            json!({"maximum": u64::from(u32::MAX) + 1}),
        ] {
            let mut definition = orders_definition();
            definition["raw-body-bytes"] = raw_body_bytes;
            let manifest = release_manifest(BTreeMap::from([(
                "orders".to_string(),
                attachment(AttachmentKind::Http, definition),
            )]));

            assert!(
                route_definitions(&manifest, "POST", "api.example.test").is_empty(),
                "a malformed authored ceiling must fail its attachment closed"
            );
        }
    }

    #[test]
    fn a_method_or_authority_that_matches_nothing_serves_no_route() {
        let manifest = one_http_route();

        assert!(route_definitions(&manifest, "GET", "api.example.test").is_empty());
        assert!(route_definitions(&manifest, "POST", "other.example.test").is_empty());
        // Both sides are normalized before they meet here, so case can never be
        // the reason a live route is missed.
        assert_eq!(
            served_ids(&route_definitions(&manifest, "post", "API.EXAMPLE.TEST")),
            ["orders"]
        );
    }

    #[test]
    fn a_wildcard_host_attachment_is_served_for_any_authority() {
        let mut definition = orders_definition();
        definition["route"]["host"] = json!(WILDCARD_HOST);
        let manifest = release_manifest(BTreeMap::from([(
            "orders".to_string(),
            attachment(AttachmentKind::Http, definition),
        )]));

        assert_eq!(
            served_ids(&route_definitions(
                &manifest,
                "POST",
                "anything.example.test"
            )),
            ["orders"]
        );
    }

    #[test]
    fn only_the_attachment_kinds_that_carry_an_http_route_are_served() {
        // Every kind is given a route document, including the two that cannot
        // legally have one: the filter under test is the kind, not the route.
        let attachments = [
            ("cron-attachment", AttachmentKind::Cron),
            ("http-attachment", AttachmentKind::Http),
            ("internal-attachment", AttachmentKind::Internal),
            ("studio-attachment", AttachmentKind::Studio),
        ]
        .into_iter()
        .map(|(id, kind)| (id.to_string(), attachment(kind, orders_definition())))
        .collect();

        let served = route_definitions(&release_manifest(attachments), "POST", "api.example.test");

        assert_eq!(
            served_ids(&served),
            ["http-attachment", "studio-attachment"],
            "an internal or cron attachment has no HTTP route and must never be reachable \
             over HTTP"
        );
    }

    #[test]
    fn only_an_externally_selectable_pat_attachment_requires_route_authentication() {
        let mut internal = attachment(AttachmentKind::Internal, orders_definition());
        internal.auth_policy = json!({"mode": PAT_AUTHENTICATION_MODE});
        let internal_only = release_manifest(BTreeMap::from([("internal".to_string(), internal)]));
        assert!(!requires_pat_route_authentication(&internal_only));

        let mut malformed = attachment(AttachmentKind::Http, json!({"route": {}}));
        malformed.auth_policy = json!({"mode": PAT_AUTHENTICATION_MODE});
        let malformed_only =
            release_manifest(BTreeMap::from([("malformed".to_string(), malformed)]));
        assert!(!requires_pat_route_authentication(&malformed_only));

        let mut http = attachment(AttachmentKind::Http, orders_definition());
        http.auth_policy = json!({"mode": PAT_AUTHENTICATION_MODE});
        let protected = release_manifest(BTreeMap::from([("orders".to_string(), http)]));
        assert!(requires_pat_route_authentication(&protected));
        assert!(!requires_pat_route_authentication(&one_http_route()));
    }

    #[test]
    fn an_attachment_without_a_serviceable_route_is_skipped_and_the_rest_still_serve() {
        // A route with no path: an object, so the manifest still validates, but
        // nothing this host can serve.
        let broken = json!({
            "id": "broken",
            "kind": "http",
            "source-id": "public",
            "route": {"host": "api.example.test", "method": "POST"}
        });
        let manifest = release_manifest(BTreeMap::from([
            (
                "broken".to_string(),
                attachment(AttachmentKind::Http, broken),
            ),
            (
                "orders".to_string(),
                attachment(AttachmentKind::Http, orders_definition()),
            ),
        ]));

        let served = route_definitions(&manifest, "POST", "api.example.test");

        assert_eq!(
            served_ids(&served),
            ["orders"],
            "one malformed attachment must not take the release's other routes offline"
        );
    }

    #[test]
    fn a_mapping_whose_wire_value_is_unknown_fails_its_attachment_closed() {
        let mut definition = orders_definition();
        definition["mappings"][0]["from"] = json!("cookie");
        let manifest = release_manifest(BTreeMap::from([(
            "orders".to_string(),
            attachment(AttachmentKind::Http, definition),
        )]));

        assert!(
            route_definitions(&manifest, "POST", "api.example.test").is_empty(),
            "a mapping source this host cannot honour must not be silently dropped from \
             the route it belongs to"
        );
    }

    #[test]
    fn a_process_without_a_release_refuses_instead_of_serving_an_empty_route_set() {
        let plugin = FlowHttpRouting::new(None, RouteInFlightLimit::default());

        assert_eq!(
            plugin
                .routes("POST", "api.example.test")
                .expect_err("a process carrying no release can answer no route"),
            NoRelease
        );
    }

    #[test]
    fn the_welded_manifest_is_the_only_source_the_plugin_reads() {
        let mount = Mount::holding(&one_http_route(), "welded");
        let plugin = FlowHttpRouting::new(Some(mount.weld()), RouteInFlightLimit::default());

        let served = plugin
            .routes("POST", "api.example.test")
            .expect("a welded release serves its own routes");

        // The route came through the canonical round-trip the weld performs.
        assert_eq!(served_ids(&served), ["orders"]);
    }

    #[tokio::test]
    async fn pat_mode_is_recognized_and_an_absent_backend_is_one_generic_outage() {
        let mut protected = attachment(AttachmentKind::Http, orders_definition());
        protected.auth_policy = json!({"mode": PAT_AUTHENTICATION_MODE});
        let manifest = release_manifest(BTreeMap::from([("orders".to_string(), protected)]));
        let mount = Mount::holding(&manifest, "pat-backend");
        let plugin = FlowHttpRouting::new(Some(mount.weld()), RouteInFlightLimit::default());

        let rejection = plugin
            .authenticate("orders", &[])
            .await
            .expect_err("a protected route without its backend refuses");

        assert_eq!(rejection.status, AUTHENTICATION_UNAVAILABLE_STATUS);
        assert_eq!(rejection.code, AUTHENTICATION_UNAVAILABLE_CODE);
    }

    #[test]
    fn bearer_parsing_has_one_success_shape_and_one_opaque_refusal_class() {
        let header = |name: &str, value: &str| Header {
            name: name.to_string(),
            value: value.to_string(),
        };
        assert_eq!(
            bearer_token(&[header("Authorization", "bEaReR secret")]),
            Some("secret")
        );
        for refused in [
            vec![],
            vec![header("authorization", "Basic secret")],
            vec![header("authorization", "Bearer")],
            vec![header("authorization", "Bearer secret extra")],
            vec![
                header("authorization", "Bearer first"),
                header("Authorization", "Bearer second"),
            ],
        ] {
            let rejection = required_bearer_token(&refused)
                .expect_err("every malformed bearer presentation refuses");
            assert_eq!(rejection.status, UNAUTHORIZED_STATUS);
            assert_eq!(rejection.code, UNAUTHORIZED_CODE);
        }
    }

    #[test]
    fn originating_caller_keeps_exact_permissions_only() {
        let caller = AuthenticatedCaller {
            attachment_id: "receiving-http".into(),
            principal_id: "11111111-1111-4111-8111-111111111111".into(),
            permissions: Arc::new(HashSet::from([
                "wamn-receiving:receipt/get@1.0.0".to_string()
            ])),
        };
        assert_eq!(caller.attachment_id(), "receiving-http");
        assert_eq!(
            caller.principal_id(),
            "11111111-1111-4111-8111-111111111111"
        );
        assert!(caller.permits("wamn-receiving:receipt/get@1.0.0"));
        assert!(!caller.permits("wamn-receiving:receipt/query@1.0.0"));
        assert!(!caller.permits("receipt.get"));
    }

    #[test]
    fn route_limit_is_nonzero_and_has_one_chart_default() {
        assert_eq!(RouteInFlightLimit::default().get(), 64);
        assert_eq!(
            "2".parse::<RouteInFlightLimit>().map(|limit| limit.get()),
            Ok(2)
        );
        for refused in ["", "0", "-1", "many"] {
            assert_eq!(
                refused.parse::<RouteInFlightLimit>(),
                Err(InvalidRouteInFlightLimit)
            );
        }
    }

    #[test]
    fn route_slots_are_independent_shed_without_queueing_and_release_on_drop() {
        let limiter = RouteLimiter::new("2".parse().expect("fixture limit is valid"));
        let first = limiter.try_acquire("orders").expect("first slot");
        let second = limiter.try_acquire("orders").expect("second slot");
        assert!(limiter.try_acquire("orders").is_none());
        let other = limiter
            .try_acquire("receipts")
            .expect("another route has its own ceiling");

        assert_eq!(limiter.snapshot("orders"), Some((2, 1)));
        assert_eq!(limiter.snapshot("receipts"), Some((1, 0)));

        drop(first);
        assert_eq!(limiter.snapshot("orders"), Some((1, 1)));
        drop(second);
        drop(other);
        assert_eq!(limiter.snapshot("orders"), Some((0, 1)));
        assert_eq!(limiter.snapshot("receipts"), Some((0, 0)));
    }
}
