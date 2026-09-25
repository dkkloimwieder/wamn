//! Host plugin for `wamn:flow-http-routing@0.1.0`, the route supply of the
//! http-route ingress guest.
//!
//! Reader 3 of the four the loaded release manifest enumerates
//! ([`crate::release_manifest`]): it answers `routes` out of
//! [`ServingManifest::attachments`] with no database read. Authentication
//! re-derives the selected attachment's policy from that same loaded release.
//! The plugin admits a `none` policy itself and hands every other policy to the
//! host's [`RouteAuthenticator`]. The plugin never loads,
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

use boon::{Compiler, Draft, ErrorKind, SchemaIndex, Schemas, ValidationError};
use opentelemetry::KeyValue;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
#[cfg(test)]
use wamn_catalog::PAT_AUTHENTICATION_MODE;
use wamn_catalog::{
    AttachmentAuthPolicy, AttachmentKind, AttachmentRef, OperationKind, ServingManifest,
    parse_attachment_auth_policy,
};
use wamn_session::PAT_TOKEN_PREFIX;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, Resource};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::release_manifest::LoadedRelease;
use crate::route_bindings::wamn::flow_http_routing::routing::{
    self, Cardinality, Mapping, MappingSource, RouteDefinition,
};
/// The request header and the refusal that cross a [`RouteAuthenticator`].
pub use crate::route_bindings::wamn::flow_http_routing::routing::{AuthRejection, Header};

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
    fn new(release: Option<&LoadedRelease>) -> Self {
        let Some(release) = release else {
            return Self {
                attachment_hashes: HashMap::new(),
                validators: HashMap::new(),
            };
        };
        let mut attachment_hashes = HashMap::new();
        let mut validators = HashMap::new();
        for (attachment_id, attachment) in release.manifest().every_attachment() {
            if !carries_http_route(attachment.kind())
                || route_definition(release.manifest(), attachment_id, attachment).is_none()
            {
                continue;
            }
            let schema = attachment
                .definition()
                .get("input-schema")
                .cloned()
                .unwrap_or(Value::Bool(true));
            let hash = wamn_execution_contract::canonical_json_sha256(&schema);
            attachment_hashes.insert(attachment_id.to_owned(), hash.clone());
            validators
                .entry(hash.clone())
                .or_insert_with(|| compile_input_schema(&hash, schema));
        }
        Self {
            attachment_hashes,
            validators,
        }
    }

    /// Refuse a payload with the RFC 6901 pointer of its offending value.
    ///
    /// A platform-side cause refuses with `schema-invalid`, which is not a
    /// pointer, because no payload value is at fault.
    fn validate(&self, attachment_id: &str, payload: &str) -> Result<(), String> {
        let hash = self
            .attachment_hashes
            .get(attachment_id)
            .ok_or(SCHEMA_INVALID)?;
        let validator = self.validators.get(hash).ok_or(SCHEMA_INVALID)?;
        let InputSchemaValidator::Compiled(compiled) = validator else {
            return Err(SCHEMA_INVALID.to_owned());
        };
        let payload = serde_json::from_str(payload).map_err(|_| String::new())?;
        compiled
            .schemas
            .validate(&payload, compiled.index)
            .map_err(|error| offending_pointer(&error))
    }
}

/// The top-level error always has an empty location, so the pointer comes from
/// the first leaf cause. For an unexpected property, the pointer names it.
fn offending_pointer(error: &ValidationError<'_, '_>) -> String {
    let mut leaf = error;
    while let Some(cause) = leaf.causes.first() {
        leaf = cause;
    }
    let mut pointer = leaf.instance_location.to_string();
    if let ErrorKind::AdditionalProperties { got } = &leaf.kind
        && let Some(property) = got.first()
    {
        pointer.push('/');
        pointer.push_str(&property.replace('~', "~0").replace('/', "~1"));
    }
    pointer
}

fn compile_input_schema(hash: &str, schema: Value) -> InputSchemaValidator {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    let mut schemas = Schemas::new();
    // The compile error is boxed as it is produced: it is a large value that
    // only ever reaches the warning below.
    let compilation = compiler
        .add_resource(INPUT_SCHEMA_URI, schema)
        .map_err(Box::new)
        .and_then(|()| {
            compiler
                .compile(INPUT_SCHEMA_URI, &mut schemas)
                .map_err(Box::new)
        });
    match compilation {
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

/// Host-owned record of an originating caller and its exact operation grants.
///
/// The guest can hold only the resource handle. It cannot construct this value,
/// inspect the grant set, or replace the principal while forwarding it to router
/// delivery.
#[derive(Clone)]
pub struct AuthenticatedCaller {
    attachment_id: Box<str>,
    principal_id: Box<str>,
    credential_kind: CredentialKind,
    permissions: Arc<HashSet<String>>,
}

/// The credential that authenticated the original request, owned by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    /// A freshly authenticated platform access token.
    Pat,
    /// Signed identity and roles within the approved session lifetime.
    Session,
    /// A service identity pinned by trusted queue admission.
    QueuedService,
}

impl std::fmt::Debug for AuthenticatedCaller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedCaller")
            .field("attachment_id", &self.attachment_id)
            .field("principal_id", &self.principal_id)
            .field("credential_kind", &self.credential_kind)
            .field("permission_count", &self.permissions.len())
            .finish_non_exhaustive()
    }
}

impl AuthenticatedCaller {
    /// Record the caller that a host authenticated for one attachment.
    pub fn new(
        attachment_id: impl Into<Box<str>>,
        principal_id: impl Into<Box<str>>,
        credential_kind: CredentialKind,
        permissions: HashSet<String>,
    ) -> Self {
        Self {
            attachment_id: attachment_id.into(),
            principal_id: principal_id.into(),
            credential_kind,
            permissions: Arc::new(permissions),
        }
    }

    /// Return the immutable attachment identity whose policy produced this caller record.
    pub fn attachment_id(&self) -> &str {
        &self.attachment_id
    }

    /// Return the opaque platform principal used by router traces and refusals.
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Return the original credential kind, unchanged across nested calls.
    pub fn credential_kind(&self) -> CredentialKind {
        self.credential_kind
    }

    /// Check one exact registered-operation token.
    pub fn permits(&self, operation: &str) -> bool {
        self.permissions.contains(operation)
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

/// The host's mechanism for a route whose policy names a credential.
///
/// The route plugin resolves the attachment and its policy, and it admits a
/// `none` policy itself. Every other policy reaches the authenticator. It reads
/// the credential from the headers and returns the caller with its exact
/// operation grants, or the refusal that the guest answers with.
#[async_trait::async_trait]
pub trait RouteAuthenticator: Send + Sync + std::fmt::Debug {
    /// Authenticate one request to a route whose policy is not `none`.
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<AuthenticatedCaller, AuthRejection>;
}

/// One request to a protected route, as the route plugin resolved it.
#[derive(Clone, Copy)]
pub struct AuthenticationRequest<'a> {
    /// The loaded release that carries the attachment.
    pub manifest: &'a ServingManifest,
    pub attachment_id: &'a str,
    pub attachment: AttachmentRef<'a>,
    /// The attachment's parsed policy. It is never `none`.
    pub policy: AttachmentAuthPolicy,
    pub headers: &'a [Header],
}

/// Hand-written so a debug print never shows a header, because the headers
/// carry the credential.
impl std::fmt::Debug for AuthenticationRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticationRequest")
            .field("attachment_id", &self.attachment_id)
            .field("policy", &self.policy)
            .field("header_count", &self.headers.len())
            .finish_non_exhaustive()
    }
}

/// Authoritative HTTP route supply for the flow-http adapter.
pub struct FlowHttpRouting {
    /// `None` in a process that was given no manifest root. Absence is a
    /// deployment fact, not a fallback: a gate or a bench runs this host with
    /// nothing mounted, and every `routes` call on it refuses rather than serving
    /// routes from somewhere else. A *serving* pod is given a manifest root, and a
    /// root it cannot load refuses host construction outright — that decision is
    /// made where it is visible, at the construction site in
    /// `services/host/src/host.rs`, not here behind an `Option`.
    release: Option<Arc<LoadedRelease>>,
    input_schemas: InputSchemaValidators,
    /// `None` on a host with no credential mechanism. Such a host refuses every
    /// protected route as authentication-unavailable.
    authenticator: Option<Arc<dyn RouteAuthenticator>>,
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
                &self.release.as_deref().map(LoadedRelease::release),
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
            .field("authenticator", &self.authenticator)
            .finish_non_exhaustive()
    }
}

impl FlowHttpRouting {
    /// Bind this plugin to the release and per-route host ceiling.
    pub fn new(
        release: Option<Arc<LoadedRelease>>,
        route_in_flight_limit: RouteInFlightLimit,
    ) -> Self {
        let input_schemas = InputSchemaValidators::new(release.as_deref());
        Self {
            release,
            input_schemas,
            authenticator: None,
            limiter: RouteLimiter::new(route_in_flight_limit),
        }
    }

    /// Hand every protected route to the host's credential mechanism.
    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn RouteAuthenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Read the chart-carried route ceiling, defaulting only when it is absent.
    pub fn from_env(
        release: Option<Arc<LoadedRelease>>,
    ) -> Result<Self, InvalidRouteInFlightLimit> {
        let limit = match std::env::var(HTTP_ROUTE_IN_FLIGHT_LIMIT_ENV) {
            Ok(value) => value.parse()?,
            Err(std::env::VarError::NotPresent) => RouteInFlightLimit::default(),
            Err(std::env::VarError::NotUnicode(_)) => return Err(InvalidRouteInFlightLimit),
        };
        Ok(Self::new(release, limit))
    }

    fn routes(&self, method: &str, authority: &str) -> Result<Vec<RouteDefinition>, NoRelease> {
        let loaded_release = self.release.as_ref().ok_or(NoRelease)?;
        Ok(route_definitions(
            loaded_release.manifest(),
            method,
            authority,
        ))
    }

    fn carries_route(&self, attachment_id: &str) -> Result<bool, NoRelease> {
        let loaded_release = self.release.as_ref().ok_or(NoRelease)?;
        Ok(loaded_release
            .manifest()
            .attachment(attachment_id)
            .is_some_and(|attachment| carries_http_route(attachment.kind())))
    }

    fn validate_input(&self, attachment_id: &str, payload: &str) -> Result<(), String> {
        self.input_schemas.validate(attachment_id, payload)
    }

    async fn authenticate(
        &self,
        attachment_id: &str,
        headers: &[Header],
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let loaded_release = self
            .release
            .as_ref()
            .ok_or_else(authentication_unavailable)?;
        let manifest = loaded_release.manifest();
        let attachment = manifest
            .attachment(attachment_id)
            .filter(|attachment| carries_http_route(attachment.kind()))
            .ok_or_else(authentication_unavailable)?;
        let policy = parse_attachment_auth_policy(attachment.auth_policy()).ok_or_else(|| {
            AuthRejection {
                status: UNSUPPORTED_POLICY_STATUS,
                code: UNSUPPORTED_POLICY_CODE.to_string(),
            }
        })?;
        if policy == AttachmentAuthPolicy::None {
            return Ok(None);
        }
        let authenticator = self
            .authenticator
            .as_ref()
            .ok_or_else(authentication_unavailable)?;
        authenticator
            .authenticate(AuthenticationRequest {
                manifest,
                attachment_id,
                attachment,
                policy,
                headers,
            })
            .await
            .map(Some)
    }

    /// Exercise production route authentication from an integration test.
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

    /// Exercise production route authentication with exact request headers.
    #[cfg(feature = "test-util")]
    pub async fn authenticate_headers_for_test(
        &self,
        attachment_id: &str,
        headers: &[(&str, &str)],
    ) -> Result<Option<AuthenticatedCaller>, (u16, String)> {
        let headers = headers
            .iter()
            .map(|(name, value)| Header {
                name: (*name).to_string(),
                value: (*value).to_string(),
            })
            .collect::<Vec<_>>();
        self.authenticate(attachment_id, &headers)
            .await
            .map_err(|rejection| (rejection.status, rejection.code))
    }
}

/// Every candidate the adapter could select for this request.
///
/// Free of `self` and of the loaded release so the projection can be shown against a
/// manifest fixture without a mount.
fn route_definitions(
    manifest: &ServingManifest,
    method: &str,
    authority: &str,
) -> Vec<RouteDefinition> {
    manifest
        .every_attachment()
        .filter(|(_, attachment)| carries_http_route(attachment.kind()))
        .filter_map(|(attachment_id, attachment)| {
            // Decoded before it is matched, so a malformed attachment is reported
            // whenever this pod serves at all rather than only once some request
            // happens to name its route.
            let definition = route_definition(manifest, attachment_id, attachment);
            if definition.is_none() {
                tracing::warn!(
                    attachment_id,
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

/// Project explicit hostnames from the same routes that the HTTP plugin serves.
///
/// Wildcards cannot identify a host before its workload binds. This projection
/// neither expands them nor invents aliases from operator-managed Services.
pub(crate) fn expected_http_hostnames(manifest: &ServingManifest) -> HashSet<String> {
    manifest
        .every_attachment()
        .filter(|(_, attachment)| carries_http_route(attachment.kind()))
        .filter_map(|(id, attachment)| route_definition(manifest, id, attachment))
        .map(|definition| definition.host)
        .filter(|host| !host.is_empty() && host != WILDCARD_HOST)
        .collect()
}

/// Return whether a serving release requires PAT-backed route authentication.
///
/// Only externally selectable HTTP route kinds participate. An internal or
/// cron attachment carrying an otherwise identical document cannot make the
/// host acquire route-authentication credentials it will never use.
pub fn requires_pat_route_authentication(manifest: &ServingManifest) -> bool {
    manifest
        .every_attachment()
        .filter(|(_, attachment)| carries_http_route(attachment.kind()))
        .any(|(attachment_id, attachment)| {
            route_definition(manifest, attachment_id, attachment).is_some()
                && parse_attachment_auth_policy(attachment.auth_policy())
                    .is_some_and(AttachmentAuthPolicy::allows_pat)
        })
}

/// Return whether the release contains an externally selectable session route.
pub fn requires_session_route_authentication(manifest: &ServingManifest) -> bool {
    manifest
        .every_attachment()
        .filter(|(_, attachment)| carries_http_route(attachment.kind()))
        .any(|(id, attachment)| {
            route_definition(manifest, id, attachment).is_some()
                && parse_attachment_auth_policy(attachment.auth_policy())
                    .is_some_and(AttachmentAuthPolicy::allows_session)
        })
}

/// Exactly the host and method predicates the adapter's own `select_route`
/// applies (`apps/platform/ingress/http-route/src/lib.rs`).
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
    manifest: &ServingManifest,
    attachment_id: &str,
    attachment: AttachmentRef<'_>,
) -> Option<RouteDefinition> {
    let route = attachment.definition().get("route")?;
    let body_limit = match attachment.definition().get("raw-body-bytes") {
        Some(raw_body_bytes) => {
            let authored = raw_body_bytes.get("maximum")?.as_u64()?;
            u32::try_from(authored).ok()?.into()
        }
        None => ADAPTER_GOVERNED_BYTES,
    };
    let mappings = match attachment.definition().get("mappings") {
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
        cache_control: route_kind(manifest, attachment).and_then(|kind| {
            read_cache_control(kind, parse_attachment_auth_policy(attachment.auth_policy()))
        }),
    })
}

/// The Cache-Control value of a successful read response
/// (`docs/plan/http-reads.md` section 4.6), or `None` for a kind that is not
/// a read. A read is private unless its route admits anonymous callers.
fn read_cache_control(kind: OperationKind, policy: Option<AttachmentAuthPolicy>) -> Option<String> {
    let scope = if policy == Some(AttachmentAuthPolicy::None) {
        "public"
    } else {
        "private"
    };
    match kind {
        OperationKind::Get => Some(format!("{scope}, no-cache")),
        OperationKind::Query | OperationKind::Projection => {
            Some(format!("{scope}, max-age=10, stale-while-revalidate=60"))
        }
        OperationKind::Create
        | OperationKind::Update
        | OperationKind::Delete
        | OperationKind::Command
        | OperationKind::EventHandler => None,
    }
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

/// The one credential that a request to a protected route presents.
#[derive(Clone, Copy)]
pub enum RouteCredential<'a> {
    /// A session token. `csrf` is `Some` when the token came by the session
    /// cookie: the request then passes the CSRF check, and the flag says
    /// whether the route requires the header, which a read route does not.
    Session { token: &'a str, csrf: Option<bool> },
    /// A PAT. The authenticator reads it with [`required_bearer_token`] once
    /// it has a mechanism to check one.
    Pat,
}

/// Hand-written so a debug print never shows the token.
impl std::fmt::Debug for RouteCredential<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session { csrf, .. } => formatter
                .debug_struct("Session")
                .field("csrf", csrf)
                .finish_non_exhaustive(),
            Self::Pat => formatter.write_str("Pat"),
        }
    }
}

/// Select the one credential that a protected route request presents.
///
/// A request with both a session cookie and an `authorization` header refuses.
/// A session cookie on a session route is a session. Otherwise a bearer token
/// is a session, unless the route also admits a PAT and the token carries the
/// PAT prefix. The wire shape selects one mechanism, and a failed
/// authentication never falls back to another credential.
pub fn route_credential<'a>(
    request: &AuthenticationRequest<'a>,
) -> Result<RouteCredential<'a>, AuthRejection> {
    let AuthenticationRequest {
        manifest,
        attachment,
        policy,
        headers,
        ..
    } = *request;
    let cookie = session_cookie(headers)?;
    let has_authorization = headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case("authorization"));
    if has_authorization && cookie.is_some() {
        return Err(unauthorized());
    }
    if let Some(token) = cookie.filter(|_| policy.allows_session()) {
        return Ok(RouteCredential::Session {
            token,
            csrf: Some(!serves_read(manifest, attachment)),
        });
    }
    let session = policy.allows_session()
        && (!policy.allows_pat()
            || bearer_token(headers).is_some_and(|token| !token.starts_with(PAT_TOKEN_PREFIX)));
    if session {
        return Ok(RouteCredential::Session {
            token: required_bearer_token(headers)?,
            csrf: None,
        });
    }
    Ok(RouteCredential::Pat)
}

/// Extract one standard bearer presentation without exposing why it failed.
pub fn bearer_token(headers: &[Header]) -> Option<&str> {
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

/// The one standard bearer token, or the opaque refusal.
pub fn required_bearer_token(headers: &[Header]) -> Result<&str, AuthRejection> {
    bearer_token(headers).ok_or_else(unauthorized)
}

/// The session cookie the identity service sets for the cookie carrier.
const SESSION_COOKIE: &str = "__Host-wamn-session";
/// The request header that repeats the readable CSRF cookie.
const CSRF_HEADER: &str = "x-wamn-csrf";

/// Find the one session cookie across every `cookie` header; a duplicate refuses.
pub fn session_cookie(headers: &[Header]) -> Result<Option<&str>, AuthRejection> {
    let mut found = None;
    let pairs = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("cookie"))
        .flat_map(|header| header.value.split(';'));
    for pair in pairs {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name.trim() == SESSION_COOKIE && found.replace(value.trim()).is_some() {
            return Err(unauthorized());
        }
    }
    Ok(found)
}

/// Whether an attachment only reads: it targets a route whose operation kind is
/// one of `OperationKind::READ_KINDS`. A wiring can write, so it never reads
/// only.
pub fn serves_read(manifest: &ServingManifest, attachment: AttachmentRef<'_>) -> bool {
    route_kind(manifest, attachment).is_some_and(OperationKind::is_read)
}

/// The operation kind of the route an attachment targets. A wiring has none.
fn route_kind(manifest: &ServingManifest, attachment: AttachmentRef<'_>) -> Option<OperationKind> {
    let AttachmentRef::Route(attachment) = attachment else {
        return None;
    };
    manifest
        .routes
        .iter()
        .find(|route| {
            route.package_id == attachment.package_id
                && route.component == attachment.component
                && route.operation == attachment.operation
        })
        .map(|route| route.kind)
}

/// Check the signed double-submit of a cookie session.
///
/// A cookie token must carry the `csrf` claim. When the route requires it, one
/// `x-wamn-csrf` header must hash to that claim.
pub fn check_csrf(
    requires_csrf: bool,
    claims_csrf: Option<&str>,
    headers: &[Header],
) -> Result<(), AuthRejection> {
    let claim = claims_csrf.ok_or_else(unauthorized)?;
    if !requires_csrf {
        return Ok(());
    }
    let mut values = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(CSRF_HEADER));
    let value = values.next().ok_or_else(unauthorized)?;
    if values.next().is_some() {
        return Err(unauthorized());
    }
    let digest = hex::encode(Sha256::digest(value.value.as_bytes()));
    // Constant time over the equal-length case; the claim is always 64 hex.
    let difference = digest
        .bytes()
        .zip(claim.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        });
    if digest.len() != claim.len() || difference != 0 {
        return Err(unauthorized());
    }
    Ok(())
}

/// The refusal for a missing, malformed or rejected credential.
pub fn unauthorized() -> AuthRejection {
    AuthRejection {
        status: UNAUTHORIZED_STATUS,
        code: UNAUTHORIZED_CODE.to_string(),
    }
}

/// The refusal for a credential mechanism that cannot answer now.
pub fn authentication_unavailable() -> AuthRejection {
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

impl<T: 'static + Send> routing::HostWithStore<T> for SharedCtx {
    async fn authenticate(
        accessor: &Accessor<T, Self>,
        attachment_id: String,
        headers: Vec<Header>,
    ) -> wash_runtime::wasmtime::Result<Result<Option<Resource<AuthenticatedCaller>>, AuthRejection>>
    {
        let (plugin, trace) = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                crate::invocation_trace::invocation_trace(&ctx),
            ))
        })?;
        trace
            .run(async move {
                let caller = match plugin.authenticate(&attachment_id, &headers).await {
                    Ok(caller) => caller,
                    Err(rejection) => return Ok(Err(rejection)),
                };
                accessor.with(|mut access| {
                    Ok(Ok(caller
                        .map(|caller| access.get().table.push(caller))
                        .transpose()?))
                })
            })
            .await
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "`routing::Host` is generated by wit-bindgen and declares `async fn`; the three \
              verbs that answer from the loaded release still have to match it"
)]
impl routing::Host for ActiveCtx<'_> {
    async fn routes(
        &mut self,
        method: String,
        authority: String,
    ) -> wash_runtime::wasmtime::Result<Result<Vec<RouteDefinition>, String>> {
        let plugin = plugin_of(self)?;
        let _span = tracing::info_span!(
            target: "wamn::route",
            "wamn.route.match",
            wamn.method = %method,
        )
        .entered();
        Ok(plugin.routes(&method, &authority).map_err(|error| {
            tracing::warn!(method, authority, error = %error, "flow-http route supply refused");
            error.to_string()
        }))
    }

    async fn validate_input(
        &mut self,
        attachment_id: String,
        payload: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), String>> {
        let plugin = plugin_of(self)?;
        let _span = tracing::info_span!(
            target: "wamn::route",
            "wamn.route.validate_input",
            wamn.attachment_id = %attachment_id,
            wamn.payload_bytes = payload.len(),
        )
        .entered();
        Ok(plugin.validate_input(&attachment_id, &payload))
    }

    async fn try_acquire(
        &mut self,
        attachment_id: String,
    ) -> wash_runtime::wasmtime::Result<Result<Option<Resource<RoutePermit>>, String>> {
        let plugin = plugin_of(self)?;
        let _span = tracing::info_span!(
            target: "wamn::route",
            "wamn.route.permit",
            wamn.attachment_id = %attachment_id,
        )
        .entered();
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

#[expect(
    clippy::unused_async_trait_impl,
    reason = "`routing::HostRoutePermit` is generated by wit-bindgen and declares `async fn`; \
              releasing a permit awaits nothing but still has to match it"
)]
impl routing::HostRoutePermit for ActiveCtx<'_> {
    async fn drop(&mut self, permit: Resource<RoutePermit>) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(permit)?;
        Ok(())
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "`routing::HostAuthenticatedCaller` is generated by wit-bindgen and declares \
              `async fn`; dropping a caller awaits nothing but still has to match it"
)]
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
        RELEASE_MANIFEST_FILE_NAME, ServingAttachment, ServingComponent, ServingComponentOperation,
        ServingRelease, ServingRoute, ServingWiring,
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
                    pre_commit: None,
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: None,
                    permissions: BTreeSet::new(),
                    participant: None,
                    statements: BTreeMap::new(),
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
            target: wamn_catalog::AttachmentTarget::Wiring {
                wiring_id: "orders".into(),
                wiring_version: 1,
            },
            definition_hash: DefinitionHash::parse(DEFINITION_HASH)
                .expect("fixture definition hash is canonical"),
            definition,
            auth_policy: json!({"modes": ["none"]}),
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
        release_manifest_with_routes(BTreeSet::new(), attachments)
    }

    fn release_manifest_with_routes(
        routes: BTreeSet<ServingRoute>,
        attachments: BTreeMap<String, ServingAttachment>,
    ) -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                tenant_id: "tenant-a".into(),
                effective_release_id: EffectiveReleaseId::new(EFFECTIVE_RELEASE_ID).unwrap(),
                environment: "prod".into(),
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            components(),
            routes,
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
    /// The test loads a release from its manifest file. It writes the mount
    /// and uses the same verification as a serving pod.
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

        fn load_release(&self) -> Arc<LoadedRelease> {
            Arc::new(LoadedRelease::load_from(&self.root).expect("fixture mount loads"))
        }
    }

    impl Drop for Mount {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Each kind's Cache-Control value (docs/plan/http-reads.md section 4.6).
    /// A read is private unless its route admits anonymous callers, and a
    /// route that is not a read, or a wiring, carries none.
    #[test]
    fn a_read_route_carries_the_cache_control_of_its_kind_and_auth_policy() {
        const SHORT: &str = "max-age=10, stale-while-revalidate=60";
        let session = json!({"modes": ["session"]});
        let both = json!({"modes": ["pat", "session"]});
        let none = json!({"modes": ["none"]});
        for (kind, policy, expected) in [
            (
                OperationKind::Get,
                &session,
                Some("private, no-cache".to_string()),
            ),
            (
                OperationKind::Query,
                &both,
                Some(format!("private, {SHORT}")),
            ),
            (
                OperationKind::Projection,
                &session,
                Some(format!("private, {SHORT}")),
            ),
            (
                OperationKind::Get,
                &none,
                Some("public, no-cache".to_string()),
            ),
            (
                OperationKind::Query,
                &none,
                Some(format!("public, {SHORT}")),
            ),
            (OperationKind::Command, &session, None),
            (OperationKind::Create, &none, None),
        ] {
            let mut route = attachment(
                AttachmentKind::Http,
                json!({
                    "id": "route",
                    "kind": "http",
                    "source-id": "public",
                    "route": {
                        "host": "api.example.test",
                        "path": "/orders",
                        "method": kind.http_method()
                    }
                }),
            );
            route.target = wamn_catalog::AttachmentTarget::Route {
                component: "http-request".into(),
                operation: "request".into(),
            };
            route.auth_policy = policy.clone();
            let manifest = release_manifest_with_routes(
                BTreeSet::from([ServingRoute {
                    package_id: "cat".into(),
                    component: "http-request".into(),
                    operation: "request".into(),
                    kind,
                    reads: BTreeSet::new(),
                    revision: None,
                }]),
                BTreeMap::from([("route".to_string(), route)]),
            );

            let served = route_definitions(&manifest, kind.http_method(), "api.example.test");

            let [definition] = served.as_slice() else {
                panic!("the {kind:?} route is served");
            };
            assert_eq!(definition.cache_control, expected, "{kind:?} {policy}");
        }

        let wiring = route_definitions(&one_http_route(), "POST", "api.example.test");
        assert_eq!(wiring[0].cache_control, None, "a wiring can write");
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
        let plugin =
            FlowHttpRouting::new(Some(mount.load_release()), RouteInFlightLimit::default());

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
        let plugin =
            FlowHttpRouting::new(Some(mount.load_release()), RouteInFlightLimit::default());

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
            Err(String::new())
        );
    }

    #[test]
    fn a_nonmatching_payload_refuses_with_the_pointer_of_the_offending_value() {
        let mut definition = orders_definition();
        definition["input-schema"] = json!({
            "type": "array",
            "items": {
                "type": "object",
                "required": ["request_id"],
                "properties": {
                    "request_id": {"type": "string"},
                    "change": {
                        "type": "object",
                        "properties": {"quantity": {"type": "integer"}},
                        "additionalProperties": false,
                    },
                },
                "additionalProperties": false,
            },
        });
        let manifest = release_manifest(BTreeMap::from([(
            "orders".to_string(),
            attachment(AttachmentKind::Http, definition),
        )]));
        let mount = Mount::holding(&manifest, "schema-pointer");
        let plugin =
            FlowHttpRouting::new(Some(mount.load_release()), RouteInFlightLimit::default());

        for (payload, pointer) in [
            (
                r#"[{"request_id":"r-1","change":{"created_by":"u-1"}}]"#,
                "/0/change/created_by",
            ),
            (
                r#"[{"request_id":"r-1"},{"request_id":"r-2","change":{"quantity":"7"}}]"#,
                "/1/change/quantity",
            ),
            (r#"[{"request_id":"r-1","a/b~c":1}]"#, "/0/a~1b~0c"),
            (r#"[{"change":{}}]"#, "/0"),
            (r#"{"request_id":"r-1"}"#, ""),
            ("[", ""),
        ] {
            assert_eq!(
                plugin.validate_input("orders", payload),
                Err(pointer.to_owned()),
                "{payload}"
            );
        }
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
        let plugin =
            FlowHttpRouting::new(Some(mount.load_release()), RouteInFlightLimit::default());

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
            Err(String::new())
        );
    }

    #[test]
    fn platform_causes_refuse_without_a_pointer() {
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
        let plugin =
            FlowHttpRouting::new(Some(mount.load_release()), RouteInFlightLimit::default());
        let invalid_hash = &plugin.input_schemas.attachment_hashes["invalid"];

        assert!(matches!(
            plugin.input_schemas.validators.get(invalid_hash),
            Some(InputSchemaValidator::Invalid)
        ));
        assert_eq!(
            plugin.validate_input("invalid", "["),
            Err(SCHEMA_INVALID.to_owned())
        );
        assert_eq!(plugin.validate_input("string", "7"), Err(String::new()));
        assert_eq!(
            plugin.validate_input("missing", r#""anything""#),
            Err(SCHEMA_INVALID.to_owned())
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
    fn expected_hosts_follow_the_serviceable_projection_without_wildcard_expansion() {
        let attachments = [
            ("http", AttachmentKind::Http, "api.example.test", false),
            ("duplicate", AttachmentKind::Http, "api.example.test", false),
            (
                "studio",
                AttachmentKind::Studio,
                "studio.example.test",
                false,
            ),
            ("cron", AttachmentKind::Cron, "cron.example.test", false),
            (
                "internal",
                AttachmentKind::Internal,
                "internal.example.test",
                false,
            ),
            ("wildcard", AttachmentKind::Http, WILDCARD_HOST, false),
            ("empty", AttachmentKind::Http, "", false),
            (
                "malformed",
                AttachmentKind::Http,
                "broken.example.test",
                true,
            ),
        ]
        .into_iter()
        .map(|(id, kind, host, malformed)| {
            let mut definition = orders_definition();
            definition["route"]["host"] = json!(host);
            if malformed {
                definition["raw-body-bytes"] = json!({"maximum": "invalid"});
            }
            (id.to_string(), attachment(kind, definition))
        })
        .collect();

        assert_eq!(
            expected_http_hostnames(&release_manifest(attachments)),
            HashSet::from([
                "api.example.test".to_string(),
                "studio.example.test".to_string()
            ]),
        );
    }

    #[test]
    fn only_an_externally_selectable_pat_attachment_requires_route_authentication() {
        let mut internal = attachment(AttachmentKind::Internal, orders_definition());
        internal.auth_policy = json!({"modes": [PAT_AUTHENTICATION_MODE]});
        let internal_only = release_manifest(BTreeMap::from([("internal".to_string(), internal)]));
        assert!(!requires_pat_route_authentication(&internal_only));

        let mut malformed = attachment(AttachmentKind::Http, json!({"route": {}}));
        malformed.auth_policy = json!({"modes": [PAT_AUTHENTICATION_MODE]});
        let malformed_only =
            release_manifest(BTreeMap::from([("malformed".to_string(), malformed)]));
        assert!(!requires_pat_route_authentication(&malformed_only));

        let mut http = attachment(AttachmentKind::Http, orders_definition());
        http.auth_policy = json!({"modes": [PAT_AUTHENTICATION_MODE]});
        let protected = release_manifest(BTreeMap::from([("orders".to_string(), http)]));
        assert!(requires_pat_route_authentication(&protected));
        assert!(!requires_pat_route_authentication(&one_http_route()));
    }

    #[test]
    fn session_and_pat_authorities_follow_only_the_released_modes() {
        for (modes, pat, session) in [
            (json!(["none"]), false, false),
            (json!(["pat"]), true, false),
            (json!(["session"]), false, true),
            (json!(["pat", "session"]), true, true),
        ] {
            for kind in [
                AttachmentKind::Http,
                AttachmentKind::Studio,
                AttachmentKind::Internal,
            ] {
                let mut route = attachment(kind, orders_definition());
                route.auth_policy = json!({"modes": modes});
                let manifest = release_manifest(BTreeMap::from([("orders".to_string(), route)]));
                let external = kind != AttachmentKind::Internal;
                assert_eq!(
                    requires_pat_route_authentication(&manifest),
                    pat && external
                );
                assert_eq!(
                    requires_session_route_authentication(&manifest),
                    session && external
                );
            }
        }
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
    fn the_loaded_manifest_is_the_only_source_the_plugin_reads() {
        let mount = Mount::holding(&one_http_route(), "loaded");
        let plugin =
            FlowHttpRouting::new(Some(mount.load_release()), RouteInFlightLimit::default());

        let served = plugin
            .routes("POST", "api.example.test")
            .expect("a loaded release serves its own routes");

        // The route came through the canonical round-trip the loaded release performs.
        assert_eq!(served_ids(&served), ["orders"]);
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

    fn header(name: &str, value: &str) -> Header {
        Header {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    fn assert_unauthorized(rejection: &AuthRejection) {
        assert_eq!(rejection.status, UNAUTHORIZED_STATUS);
        assert_eq!(rejection.code, UNAUTHORIZED_CODE);
    }

    #[test]
    fn the_session_cookie_is_found_among_others_once_or_refused() {
        assert_eq!(
            session_cookie(&[
                header("accept", "text/plain"),
                header("Cookie", "theme=dark; __Host-wamn-csrf=c"),
                header("cookie", " __Host-wamn-session = token.sig ;lang"),
            ])
            .expect("one session cookie is found"),
            Some("token.sig")
        );
        assert_eq!(session_cookie(&[]).expect("no cookie header"), None);
        assert_eq!(
            session_cookie(&[header("cookie", "__Host-wamn-csrf=c; other=1")])
                .expect("no session cookie"),
            None
        );
        for duplicated in [
            vec![header(
                "cookie",
                "__Host-wamn-session=a; __Host-wamn-session=b",
            )],
            vec![
                header("cookie", "__Host-wamn-session=a"),
                header("Cookie", "__Host-wamn-session=a"),
            ],
        ] {
            assert_unauthorized(
                &session_cookie(&duplicated).expect_err("a duplicate session cookie refuses"),
            );
        }
    }

    #[test]
    fn csrf_requires_the_claim_and_one_header_that_hashes_to_it() {
        let token = "csrf-token-value";
        let claim = hex::encode(Sha256::digest(token.as_bytes()));
        check_csrf(true, Some(&claim), &[header("X-Wamn-Csrf", token)])
            .expect("a matching header passes");
        for (claims_csrf, headers) in [
            (Some(claim.as_str()), vec![]),
            (Some(claim.as_str()), vec![header("x-wamn-csrf", "wrong")]),
            (
                Some(claim.as_str()),
                vec![header("x-wamn-csrf", token), header("x-wamn-csrf", token)],
            ),
            (None, vec![header("x-wamn-csrf", token)]),
        ] {
            assert_unauthorized(
                &check_csrf(true, claims_csrf, &headers).expect_err("the CSRF check refuses"),
            );
        }
    }

    #[test]
    fn csrf_not_required_skips_the_header_but_still_needs_the_claim() {
        let claim = hex::encode(Sha256::digest(b"csrf-token-value"));
        check_csrf(false, Some(&claim), &[]).expect("no header is needed");
        assert_unauthorized(
            &check_csrf(false, None, &[]).expect_err("a cookie token without the claim refuses"),
        );
    }

    #[test]
    fn originating_caller_keeps_exact_permissions_only() {
        let caller = AuthenticatedCaller {
            attachment_id: "widget-http".into(),
            principal_id: "11111111-1111-4111-8111-111111111111".into(),
            credential_kind: CredentialKind::Pat,
            permissions: Arc::new(HashSet::from([
                "platform-fixture:widget/get@1.0.0".to_string()
            ])),
        };
        assert_eq!(caller.attachment_id(), "widget-http");
        assert_eq!(
            caller.principal_id(),
            "11111111-1111-4111-8111-111111111111"
        );
        assert!(caller.permits("platform-fixture:widget/get@1.0.0"));
        assert!(!caller.permits("platform-fixture:widget/query@1.0.0"));
        assert!(!caller.permits("widget.get"));
        assert_eq!(caller.credential_kind(), CredentialKind::Pat);
        for kind in [CredentialKind::Pat, CredentialKind::Session] {
            let caller = AuthenticatedCaller {
                credential_kind: kind,
                ..caller.clone()
            };
            let nested = caller.clone();
            assert_eq!(nested.credential_kind(), kind);
            assert_eq!(nested.principal_id(), caller.principal_id());
            assert!(nested.permits("platform-fixture:widget/get@1.0.0"));
        }
    }

    #[test]
    fn route_limit_is_nonzero_and_has_one_chart_default() {
        assert_eq!(RouteInFlightLimit::default().get(), 64);
        assert_eq!(
            "2".parse::<RouteInFlightLimit>()
                .map(RouteInFlightLimit::get),
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
            .try_acquire("widgets")
            .expect("another route has its own ceiling");

        assert_eq!(limiter.snapshot("orders"), Some((2, 1)));
        assert_eq!(limiter.snapshot("widgets"), Some((1, 0)));

        drop(first);
        assert_eq!(limiter.snapshot("orders"), Some((1, 1)));
        drop(second);
        drop(other);
        assert_eq!(limiter.snapshot("orders"), Some((0, 1)));
        assert_eq!(limiter.snapshot("widgets"), Some((0, 0)));
    }
}
