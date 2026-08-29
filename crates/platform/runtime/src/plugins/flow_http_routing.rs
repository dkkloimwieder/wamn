//! Host plugin for `wamn:flow-http-routing@0.1.0`.
//!
//! Reader 3 of the four the release-manifest weld enumerates
//! ([`crate::release_manifest`]): it answers `routes` out of
//! [`ServingManifest::attachments`] and performs **no database read on the serving
//! path**. The weld is consulted by reference — this plugin never loads, parses or
//! digest-verifies a manifest of its own, and adds no layer over the one it is
//! given. The manifest is already parsed, in memory, for the life of the process;
//! a route table derived from it would be a second copy of immutable state on the
//! hottest path in the system, so there is none.
//!
//! # Fields the attachment projection cannot source
//!
//! `input-schema` and the byte ceilings have no carrier in [`ServingAttachment`].
//! Each is answered with the value that leaves whichever authority *can* decide
//! in charge, never with a guess; each is justified at its own field in
//! [`route_definition`].

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;

use opentelemetry::KeyValue;
use serde_json::Value;
use wamn_catalog::{AttachmentKind, ServingAttachment, ServingManifest};
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

/// The JSON Schema that constrains nothing — already this system's spelling for an
/// absent entry guard (an event entry's `entry-input-schema-guard` is required to
/// be exactly `true`).
const UNCONSTRAINED_INPUT_SCHEMA: &str = "true";

/// A byte ceiling that can never bind, so the adapter's own limit governs.
///
/// `u32::MAX` rather than `u64::MAX` because the guest narrows this with
/// `usize::try_from` and `usize` is 32 bits on wasm32: a wider value would fail
/// that conversion and turn every `routes` call into a 503.
const ADAPTER_GOVERNED_BYTES: u64 = u32::MAX as u64;

/// The one auth-source document this host can honour with no secret store.
const NO_AUTHENTICATION_MODE: &str = "none";

/// 501, because the request is well formed and it is the *host* that lacks the
/// mechanism — the caller can do nothing to satisfy a policy nothing implements.
const UNSUPPORTED_POLICY_STATUS: u16 = 501;
const UNSUPPORTED_POLICY_CODE: &str = "auth-policy-unsupported";

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
            .field("route_in_flight_limit", &self.limiter.limit)
            .finish_non_exhaustive()
    }
}

impl FlowHttpRouting {
    /// Bind this plugin to the release and per-route host ceiling.
    pub fn new(
        release: Option<Arc<ReleaseManifestWeld>>,
        route_in_flight_limit: RouteInFlightLimit,
    ) -> Self {
        Self {
            release,
            limiter: RouteLimiter::new(route_in_flight_limit),
        }
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
        auth_policy: attachment.auth_policy.to_string(),
        mappings,
        // The release currently carries no entry input-schema projection. An
        // unconstraining schema is the truthful answer: it claims no validation
        // this host cannot perform, while the guest still refuses malformed JSON
        // and enforces the route's mapping contract.
        input_schema: UNCONSTRAINED_INPUT_SCHEMA.to_string(),
        body_limit: ADAPTER_GOVERNED_BYTES,
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

/// Apply the policy the adapter selected, after route selection supplied it.
///
/// Only one policy can be honoured truthfully today: there is no auth-policy
/// vocabulary in this system — the resolved auth-source document is validated as
/// nothing more than "an object" — and no secret store this host could verify a
/// credential against. So an explicit no-authentication policy yields the
/// no caller identity and every other document is a typed refusal. Fail-closed:
/// no unrecognized policy can reach router delivery.
fn authenticate_policy(policy: &str) -> Result<Option<String>, AuthRejection> {
    let document = serde_json::from_str::<Value>(policy).ok();
    let mode = document
        .as_ref()
        .and_then(|document| document.get("mode"))
        .and_then(Value::as_str);
    if mode == Some(NO_AUTHENTICATION_MODE) {
        return Ok(None);
    }
    Err(AuthRejection {
        status: UNSUPPORTED_POLICY_STATUS,
        code: UNSUPPORTED_POLICY_CODE.to_string(),
    })
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
        policy: String,
        // Unread: the only policy this host can honour authenticates nothing. A
        // policy that inspects headers cannot land on this signature anyway —
        // trusting a policy string the guest hands back would let a defective
        // adapter downgrade a protected route, so a real policy has to be
        // re-derived from the attachment instead.
        _headers: Vec<Header>,
    ) -> wash_runtime::wasmtime::Result<Result<Option<String>, AuthRejection>> {
        Ok(authenticate_policy(&policy))
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use serde_json::json;
    use wamn_catalog::{
        ArtifactHash, DefinitionHash, RELEASE_MANIFEST_FILE_NAME, ServingComponent, ServingRelease,
        ServingWiring,
    };

    use super::*;

    const COMPONENT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GRAPH: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DEFINITION_HASH: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const CATALOG_VERSION: u32 = 7;

    fn components() -> BTreeSet<ServingComponent> {
        BTreeSet::from([ServingComponent {
            component: "http-request".into(),
            interface_version: "0.1".into(),
            digest: ArtifactHash::parse(COMPONENT).expect("fixture artifact hash is canonical"),
        }])
    }

    fn wirings() -> BTreeSet<ServingWiring> {
        BTreeSet::from([ServingWiring {
            wiring_id: "orders".into(),
            wiring_version: 1,
            graph_hash: DefinitionHash::parse(GRAPH).expect("fixture definition hash is canonical"),
        }])
    }

    fn attachment(kind: AttachmentKind, definition: Value) -> ServingAttachment {
        ServingAttachment {
            kind,
            wiring_id: "orders".into(),
            wiring_version: 1,
            definition_hash: DefinitionHash::parse(DEFINITION_HASH)
                .expect("fixture definition hash is canonical"),
            definition,
            auth_policy: json!({"mode": "none"}),
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
                catalog_id: "cat".into(),
                catalog_version: CATALOG_VERSION,
                environment: "prod".into(),
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
        // The resolved auth-source document, handed on as the opaque string the
        // adapter returns to `authenticate`.
        assert_eq!(definition.auth_policy, r#"{"mode":"none"}"#);
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

    /// Fields with no carrier in the attachment projection. Each must leave
    /// the authority that can decide in charge — see [`route_definition`].
    #[test]
    fn the_fields_the_manifest_cannot_source_defer_to_their_real_authority() {
        let manifest = one_http_route();

        let served = route_definitions(&manifest, "POST", "api.example.test");

        let [definition] = served.as_slice() else {
            panic!("exactly one attachment matches this request");
        };
        assert_eq!(
            serde_json::from_str::<Value>(&definition.input_schema)
                .expect("the guest parses this with serde_json and 503s if it cannot"),
            Value::Bool(true)
        );
        for limit in [definition.body_limit, definition.mapped_limit] {
            assert!(
                u32::try_from(limit).is_ok(),
                "a ceiling wider than u32 fails the guest's usize::try_from on wasm32 and \
                 turns every route lookup into a 503"
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

    #[test]
    fn only_the_no_authentication_policy_yields_an_anonymous_absence() {
        assert_eq!(
            authenticate_policy(r#"{"mode":"none"}"#)
                .expect("an explicit no-authentication policy authenticates"),
            None
        );

        for unsupported in [
            r#"{"mode":"bearer"}"#,
            r#"{"policy":"api-key"}"#,
            r#"{"scheme":"bearer"}"#,
            "{}",
            "not json at all",
        ] {
            let rejection = authenticate_policy(unsupported)
                .expect_err("a policy this host cannot apply must refuse");
            assert_eq!(rejection.status, UNSUPPORTED_POLICY_STATUS);
            assert_eq!(rejection.code, UNSUPPORTED_POLICY_CODE);
        }
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
