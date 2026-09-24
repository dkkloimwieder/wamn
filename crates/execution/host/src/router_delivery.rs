//! Guest delivery. A route calls its one operation on the operation host, and
//! a wiring target goes to the wiring layer through `WiringDelivery`.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use wamn_catalog::{
    AdmittedComponent, AttachmentAuthPolicy, AttachmentTarget, ServingManifest,
    parse_attachment_auth_policy,
};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_event_wire::Causation;
use wamn_project_state::PlatformComponent;
pub use wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller;
use wamn_runtime::plugins::wamn_jetstream::{RouterTapPhase, RouterTapPreview, WamnJetstream};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Accessor;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::RouterDriver;
use crate::operation::{
    OperationHost, OperationRefusal, OperationRefusalKind, authorize_registered_operation,
};
use crate::route::{RouteCall, invoke_route};
use wamn_engine::operation::node_types;

mod wiring;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: ["../../platform/runtime/wit/deps/wamn-flow-http-routing", "wit"],
        world: "wamn:execution-host/router-delivery-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:flow-http-routing/routing.authenticated-caller": super::AuthenticatedCaller,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::router_delivery::delivery::{
    self, DeliveryError, DeliveryFailure, DeliveryOutcome, DeliveryReport, DeliveryRequest,
    FailureKind as WireFailureKind, ParentCausation, PermissionDenial, Source,
};

/// Host-plugin identity for the one guest-to-router bridge.
pub const ROUTER_DELIVERY_ID: &str = "wamn-router-delivery";

// The two series this bridge owns. Both are dashboard contracts that no grep
// from a chart can find, because the Prometheus exporter turns the dots into
// underscores and appends `_total` to a monotonic counter. Pinned by
// `the_bridge_pins_its_two_series_and_records_on_every_driver_outcome`.
const DELIVERY_ATTEMPTS: &str = "wamn.router.delivery.attempts";
const DELIVERY_ERRORS: &str = "wamn.router.delivery.errors";

// Attribute keys. `wamn.source.kind` plus `wamn.source.id` rather than the two
// mutually exclusive keys the older instruments use (`wamn.attachment.id`,
// `wamn.registration`), because one counter covers both ingress kinds and an
// always-empty second key would double the series for nothing.
const SOURCE_KIND: &str = "wamn.source.kind";
const SOURCE_ID: &str = "wamn.source.id";
const WIRING_ID: &str = "wamn.wiring.id";
const WIRING_VERSION: &str = "wamn.wiring.version";
const DELIVERY_ERROR: &str = "wamn.delivery.error";

// The bounded driver refusals a live view can show. Shared with `DeliveryClass`
// rather than respelled, so a dashboard and a run screen never disagree about
// what happened to the same delivery — pinned by
// `a_refusal_reads_the_same_to_a_dashboard_and_to_a_live_view`.
const PERMISSION_DENIED: &str = "permission-denied";
const FRESH_CREDENTIAL_REQUIRED: &str = "fresh-credential-required";
const EXECUTION_FAILED: &str = "execution-failed";

/// The wiring arm of the bridge. A route never reaches it.
///
/// The router driver implements it. `wamn-xs9a.4` moves the driver and its
/// implementation into the workflow layer.
#[async_trait::async_trait]
pub(crate) trait WiringDelivery: Send + Sync {
    /// Resolve and check every wiring that a synchronous attachment targets.
    async fn preload(&self) -> anyhow::Result<WiringPreload>;

    /// Deliver to one wiring target, and settle it through the bridge.
    async fn deliver(
        &self,
        bridge: &RouterDeliveryBridge,
        call: WiringCall<'_>,
    ) -> Result<DeliveryOutcome, DeliveryError>;
}

/// The synchronous wirings one readiness evaluation resolved.
pub(crate) struct WiringPreload {
    pub(crate) wirings: usize,
    /// The release components the wirings name, or `None` when no
    /// synchronous attachment targets a wiring.
    pub(crate) components: Option<Arc<[AdmittedComponent]>>,
}

/// One delivery that the bridge resolved to a wiring target.
pub(crate) struct WiringCall<'a> {
    source: SourceRef<'a>,
    delivery_id: String,
    package_id: &'a str,
    target: &'a AttachmentTarget,
    wiring_id: &'a str,
    wiring_version: u32,
    caller_attached: bool,
    payload: serde_json::Value,
    caller: Option<AuthenticatedCaller>,
    traceparent: Option<String>,
    tracestate: Option<String>,
    causation: Causation,
    attributes: &'a [KeyValue],
    deadline_adjustments: &'a mut Vec<crate::DeadlineAdjustment>,
}

/// The one bridge shared by attachment and registration ingress.
pub struct RouterDeliveryBridge {
    /// `None` on a host with no wiring layer. Such a host refuses a wiring
    /// target.
    wirings: Option<Arc<dyn WiringDelivery>>,
    /// The route path calls this host directly and never enters the driver.
    operations: Arc<OperationHost>,
    release: Arc<LoadedRelease>,
    jetstream: Arc<WamnJetstream>,
    metrics: Option<DeliveryMetrics>,
}

impl RouterDeliveryBridge {
    /// Bind the bridge to the process's operation host, its loaded manifest,
    /// and the router driver that serves wiring targets, if any.
    pub fn new(
        operations: Arc<OperationHost>,
        driver: Option<Arc<RouterDriver>>,
        jetstream: Arc<WamnJetstream>,
        project: &str,
    ) -> anyhow::Result<Self> {
        let release = Arc::clone(&operations.release);
        jetstream.bind_derived_scope(
            ROUTER_DELIVERY_ID,
            &release.manifest().release.tenant_id,
            project,
            &release.manifest().release.environment,
        )?;
        Ok(Self {
            operations,
            wirings: driver.map(|driver| driver as Arc<dyn WiringDelivery>),
            release,
            jetstream,
            metrics: None,
        })
    }

    /// Count every delivery on the supplied meter. The meter is injected rather
    /// than taken from `opentelemetry::global`, so a test owns its own provider
    /// and reads back exactly the series one bridge emitted.
    #[must_use]
    pub fn with_metrics(mut self, meter: &Meter) -> Self {
        self.metrics = Some(DeliveryMetrics::new(meter));
        self
    }

    fn record(&self, attributes: &[KeyValue], class: DeliveryClass) {
        if let Some(metrics) = &self.metrics {
            metrics.record(attributes, class);
        }
    }

    async fn deliver_report(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport {
        let label_eligible = caller.is_some() && matches!(request.source, Source::Attachment(_));
        let mut deadline_adjustments = Vec::new();
        let outcome = self
            .deliver_inner(request, caller, &mut deadline_adjustments)
            .await;
        let mut actor_labels = Vec::new();
        if label_eligible && let Ok(DeliveryOutcome::Respond(payload)) = &outcome {
            let actors = result_actors(payload);
            match self
                .operations
                .postgres
                .record_actor_labels(
                    &self.operations.project,
                    &self.release.manifest().release.tenant_id,
                    &actors,
                )
                .await
            {
                Ok(labels) => actor_labels = labels,
                Err(error) => tracing::warn!(%error, "record actor labels unavailable"),
            }
        }
        DeliveryReport {
            outcome,
            actor_labels,
            deadline_adjustments: deadline_adjustments
                .into_iter()
                .map(|adjustment| delivery::DeadlineAdjustment {
                    node: adjustment.node,
                    requested_ms: adjustment.requested_ms,
                    effective_ms: adjustment.effective_ms,
                })
                .collect(),
        }
    }

    async fn deliver_inner(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
        deadline_adjustments: &mut Vec<crate::DeadlineAdjustment>,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let DeliveryRequest {
            source,
            delivery_id,
            payload,
            caller: _,
            trace,
            parent_causation,
        } = request;
        if delivery_id.is_empty() {
            return Err(DeliveryError::InvalidRequest);
        }
        let payload = serde_json::from_str(&payload).map_err(|_| DeliveryError::InvalidPayload)?;
        let source = match &source {
            Source::Attachment(id) if !id.is_empty() => SourceRef::Attachment(id),
            Source::Registration(id) if !id.is_empty() => SourceRef::Registration(id),
            Source::Attachment(_) | Source::Registration(_) => {
                return Err(DeliveryError::InvalidRequest);
            }
        };
        if parent_causation.is_some() && !matches!(source, SourceRef::Registration(_)) {
            return Err(DeliveryError::InvalidRequest);
        }
        let causation = derived_causation(&delivery_id, parent_causation)?;
        let target = resolve_authorized_target(self.release.manifest(), source, caller.as_ref())?;
        let (traceparent, tracestate) = match trace {
            Some(trace) if trace.traceparent.is_empty() => {
                return Err(DeliveryError::InvalidRequest);
            }
            Some(trace) => (Some(trace.traceparent), trace.tracestate),
            None => (None, None),
        };
        // The live view's first boundary, published while the input payload is
        // still a local: after the request is built it belongs to the driver.
        self.tap(
            source,
            &delivery_id,
            &target.target,
            RouterTapPhase::Accepted,
            &payload,
        )
        .await;
        let attributes = match &self.metrics {
            Some(_) => delivery_attributes(source, &target.target),
            None => Vec::new(),
        };
        let (wiring_id, wiring_version) = match &target.target {
            AttachmentTarget::Wiring {
                wiring_id,
                wiring_version,
            } => (wiring_id.clone(), *wiring_version),
            AttachmentTarget::Route {
                component,
                operation,
            } => {
                let result = invoke_route(
                    &self.operations,
                    RouteCall {
                        package_id: &target.package_id,
                        component,
                        operation,
                        delivery_id: &delivery_id,
                        payload: &payload,
                        caller,
                        traceparent: traceparent.as_deref(),
                        tracestate: tracestate.as_deref(),
                        causation,
                    },
                )
                .await
                .and_then(settle_route);
                return match result {
                    Ok(settled) => {
                        self.record(&attributes, DeliveryClass::Delivered);
                        self.tap(
                            source,
                            &delivery_id,
                            &target.target,
                            RouterTapPhase::Settled(settled.label),
                            &settled.result,
                        )
                        .await;
                        Ok(settled.outcome)
                    }
                    Err(error) => {
                        self.refuse(source, &delivery_id, &target.target, &attributes, &error)
                            .await
                    }
                };
            }
        };

        let Some(wirings) = &self.wirings else {
            let error = anyhow::anyhow!("no wiring layer serves wiring {wiring_id:?}");
            return self
                .refuse(source, &delivery_id, &target.target, &attributes, &error)
                .await;
        };
        wirings
            .deliver(
                self,
                WiringCall {
                    source,
                    delivery_id,
                    package_id: &target.package_id,
                    target: &target.target,
                    wiring_id: &wiring_id,
                    wiring_version,
                    caller_attached: target.caller_attached,
                    payload,
                    caller,
                    traceparent,
                    tracestate,
                    causation,
                    attributes: &attributes,
                    deadline_adjustments,
                },
            )
            .await
    }

    /// Settle a delivery that the route path or the driver refused, or that
    /// failed to execute.
    async fn refuse(
        &self,
        source: SourceRef<'_>,
        delivery_id: &str,
        target: &AttachmentTarget,
        attributes: &[KeyValue],
        error: &anyhow::Error,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        match error {
            error if error.downcast_ref::<OperationRefusal>().is_some() => {
                let denial = error
                    .downcast_ref::<OperationRefusal>()
                    .expect("the guarded branch carries an operation refusal")
                    .clone();
                let (class, literal) = match denial.kind() {
                    OperationRefusalKind::PermissionDenied => {
                        (DeliveryClass::PermissionDenied, PERMISSION_DENIED)
                    }
                    OperationRefusalKind::FreshCredentialRequired => (
                        DeliveryClass::FreshCredentialRequired,
                        FRESH_CREDENTIAL_REQUIRED,
                    ),
                };
                self.record(attributes, class);
                self.tap(
                    source,
                    delivery_id,
                    target,
                    RouterTapPhase::Settled(literal),
                    &serde_json::Value::Null,
                )
                .await;
                Err(lower_operation_refusal(&denial))
            }
            error => {
                self.record(attributes, DeliveryClass::ExecutionFailed);
                self.tap(
                    source,
                    delivery_id,
                    target,
                    RouterTapPhase::Settled(EXECUTION_FAILED),
                    &serde_json::Value::Null,
                )
                .await;
                // The whole chain, not the top context: `invoke wiring node
                // "store"` alone told six cluster runs nothing about WHY
                // (wamn-362o.45).
                tracing::warn!(error = %format_args!("{error:#}"), "router delivery execution failed");
                Err(DeliveryError::ExecutionFailed)
            }
        }
    }

    /// Publish one delivery-boundary preview onto the host's reserved `tap.*`
    /// namespace — the router-edge live view that `wamn-dggp.10`'s run screen
    /// consumes in place of `get-run`.
    ///
    /// The bridge hands over facts and nothing else. Redaction, the payload
    /// ceiling and the subject are all the plugin's: it mints every `tap.*`
    /// subject from its own trusted bind-time claim, which is what makes it the
    /// only writer of a namespace `producer::publish` refuses to every guest.
    ///
    /// Best-effort by contract on that side too — a tap never fails, slows or
    /// reshapes a delivery, and is a no-op on a host with no data-plane NATS.
    /// PER-EDGE previews inside the router walk are the DEMAND-GATED UPGRADE and
    /// are deliberately not built: they would put a publish on every
    /// `Step::Invoke`. Nothing here forecloses them — a per-edge phase is another
    /// variant on a subject that already scopes to one delivery.
    async fn tap(
        &self,
        source: SourceRef<'_>,
        delivery_id: &str,
        target: &AttachmentTarget,
        phase: RouterTapPhase,
        payload: &serde_json::Value,
    ) {
        self.jetstream
            .publish_router_tap(
                ROUTER_DELIVERY_ID,
                RouterTapPreview {
                    delivery_id,
                    target,
                    source_kind: source.kind(),
                    source_id: source.id(),
                    phase,
                    payload,
                },
            )
            .await;
    }
}

/// Only successful result values supply actor IDs. Refusals and input do not.
fn result_actors(payload: &str) -> Vec<String> {
    fn collect(value: &serde_json::Value, actors: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                for (name, value) in fields {
                    if matches!(name.as_str(), "created_by" | "updated_by") {
                        if let Some(actor) = value.as_str().filter(|actor| {
                            actor.parse::<wamn_platform_identity::PrincipalId>().is_ok()
                        }) {
                            actors.insert(actor.to_owned());
                        }
                    } else {
                        collect(value, actors);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    collect(value, actors);
                }
            }
            _ => {}
        }
    }
    let mut actors = std::collections::BTreeSet::new();
    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str(payload) {
        for item in items {
            if item.get("error").is_none()
                && let Some(value) = item.get("value")
            {
                collect(value, &mut actors);
            }
        }
    }
    actors.into_iter().collect()
}

fn derived_causation(
    delivery_id: &str,
    parent: Option<ParentCausation>,
) -> Result<Causation, DeliveryError> {
    match parent {
        Some(parent) if parent.root.is_empty() => Err(DeliveryError::InvalidRequest),
        Some(parent) => Ok(Causation {
            run: delivery_id.to_owned(),
            root: parent.root,
            depth: parent
                .depth
                .checked_add(1)
                .ok_or(DeliveryError::InvalidRequest)?,
        }),
        None => Ok(Causation {
            run: delivery_id.to_owned(),
            root: delivery_id.to_owned(),
            depth: 0,
        }),
    }
}

impl fmt::Debug for RouterDeliveryBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouterDeliveryBridge")
            .field("operations", &self.operations)
            .field("wiring_layer", &self.wirings.is_some())
            .field("release", &self.release.release())
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl HostPlugin for RouterDeliveryBridge {
    fn id(&self) -> &'static str {
        ROUTER_DELIVERY_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:router-delivery/delivery@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "router-delivery", &["delivery"]) {
            return Ok(());
        }
        delivery::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        Ok(())
    }
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<RouterDeliveryBridge>> {
    ctx.try_get_plugin::<RouterDeliveryBridge>(ROUTER_DELIVERY_ID)
}

impl delivery::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> delivery::HostWithStore<T> for SharedCtx {
    async fn deliver(
        accessor: &Accessor<T, Self>,
        mut request: DeliveryRequest,
    ) -> wash_runtime::wasmtime::Result<DeliveryReport> {
        let (plugin, caller) = accessor.with(|mut access| {
            let ctx = access.get();
            let plugin = plugin_of(&ctx)?;
            let caller = request
                .caller
                .take()
                .map(|caller| ctx.table.delete(caller))
                .transpose()?;
            Ok::<_, wash_runtime::wasmtime::Error>((plugin, caller))
        })?;
        Ok(plugin.deliver_report(request, caller).await)
    }
}

#[derive(Debug, Clone, Copy)]
enum SourceRef<'a> {
    Attachment(&'a str),
    Registration(&'a str),
}

impl<'a> SourceRef<'a> {
    /// The bridge's two ingress kinds, as the label a metric attribute and a
    /// delivery preview both carry.
    fn kind(self) -> &'static str {
        match self {
            SourceRef::Attachment(_) => "attachment",
            SourceRef::Registration(_) => "registration",
        }
    }

    fn id(self) -> &'a str {
        match self {
            SourceRef::Attachment(id) | SourceRef::Registration(id) => id,
        }
    }

    /// The platform component that executes a callerless delivery from this
    /// source. A registration delivery stays callerless and executes as
    /// `wamn:materializer`. An anonymous attachment has no executing principal.
    fn platform(self) -> Option<PlatformComponent> {
        match self {
            SourceRef::Attachment(_) => None,
            SourceRef::Registration(_) => Some(PlatformComponent::Materializer),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedTarget {
    package_id: String,
    /// A registration always names a wiring. An attachment names either.
    target: AttachmentTarget,
    caller_attached: bool,
    /// `Some` only for attachment ingress. A callerless attachment is legal
    /// only when its loaded auth policy explicitly names anonymous mode.
    anonymous_caller_permitted: Option<bool>,
    registered_operation: Option<String>,
}

fn resolve_target(manifest: &ServingManifest, source: SourceRef<'_>) -> Option<ResolvedTarget> {
    match source {
        SourceRef::Attachment(id) => {
            let attachment = manifest.attachments.get(id)?;
            Some(ResolvedTarget {
                package_id: attachment.package_id.clone(),
                target: attachment.target.clone(),
                caller_attached: true,
                anonymous_caller_permitted: Some(
                    parse_attachment_auth_policy(&attachment.auth_policy)
                        == Some(AttachmentAuthPolicy::None),
                ),
                registered_operation: attachment.registered_operation.clone(),
            })
        }
        SourceRef::Registration(id) => {
            manifest
                .registrations
                .get(id)
                .map(|registration| ResolvedTarget {
                    package_id: registration.package_id.clone(),
                    target: AttachmentTarget::Wiring {
                        wiring_id: registration.wiring_id.clone(),
                        wiring_version: registration.wiring_version,
                    },
                    caller_attached: false,
                    anonymous_caller_permitted: None,
                    registered_operation: None,
                })
        }
    }
}

fn validate_caller(
    source: SourceRef<'_>,
    target: &ResolvedTarget,
    caller: Option<&AuthenticatedCaller>,
) -> Result<(), DeliveryError> {
    if caller_matches_source(
        source,
        target.anonymous_caller_permitted,
        caller.map(AuthenticatedCaller::attachment_id),
    ) {
        Ok(())
    } else {
        Err(DeliveryError::InvalidRequest)
    }
}

fn resolve_authorized_target(
    manifest: &ServingManifest,
    source: SourceRef<'_>,
    caller: Option<&AuthenticatedCaller>,
) -> Result<ResolvedTarget, DeliveryError> {
    let target = resolve_target(manifest, source).ok_or(DeliveryError::SourceNotFound)?;
    validate_caller(source, &target, caller)?;
    // Attachments do not own freshness. The driver reads each released operation.
    authorize_registered_operation(caller, target.registered_operation.as_deref(), false)
        .map_err(|denial| lower_operation_refusal(&denial))?;
    Ok(target)
}

/// Exercise the exact production attachment resolver and authorization gate.
#[cfg(feature = "test-util")]
pub(crate) fn authorize_attachment_for_test(
    release: &wamn_engine::release_manifest::LoadedRelease,
    attachment_id: &str,
    caller: Option<&AuthenticatedCaller>,
) -> Result<(), Box<str>> {
    resolve_authorized_target(
        release.manifest(),
        SourceRef::Attachment(attachment_id),
        caller,
    )
    .map(|_| ())
    .map_err(|error| match error {
        DeliveryError::PermissionDenied(PermissionDenial { operation }) => operation.into(),
        DeliveryError::FreshCredentialRequired(_) => FRESH_CREDENTIAL_REQUIRED.into(),
        DeliveryError::SourceNotFound => "source-not-found".into(),
        DeliveryError::InvalidRequest => "invalid-request".into(),
        DeliveryError::InvalidPayload => "invalid-payload".into(),
        DeliveryError::ExecutionFailed => "execution-failed".into(),
    })
}

fn caller_matches_source(
    source: SourceRef<'_>,
    anonymous_caller_permitted: Option<bool>,
    caller_attachment_id: Option<&str>,
) -> bool {
    match (source, anonymous_caller_permitted, caller_attachment_id) {
        (SourceRef::Registration(_), None, None) | (SourceRef::Attachment(_), Some(true), None) => {
            true
        }
        (SourceRef::Attachment(attachment_id), Some(false), Some(caller_attachment_id)) => {
            caller_attachment_id == attachment_id
        }
        _ => false,
    }
}

fn lower_operation_refusal(denial: &OperationRefusal) -> DeliveryError {
    let detail = PermissionDenial {
        operation: denial.operation().to_owned(),
    };
    match denial.kind() {
        OperationRefusalKind::PermissionDenied => DeliveryError::PermissionDenied(detail),
        OperationRefusalKind::FreshCredentialRequired => {
            DeliveryError::FreshCredentialRequired(detail)
        }
    }
}

/// How the router driver answered one delivery. The variants are the arms of
/// the driver match in [`RouterDeliveryBridge::deliver`]; the
/// bridge classifies nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryClass {
    Delivered,
    PermissionDenied,
    FreshCredentialRequired,
    ExecutionFailed,
}

impl DeliveryClass {
    /// The `wamn.delivery.error` value, or `None` for the one delivered class.
    fn error(self) -> Option<&'static str> {
        match self {
            DeliveryClass::Delivered => None,
            DeliveryClass::PermissionDenied => Some(PERMISSION_DENIED),
            DeliveryClass::FreshCredentialRequired => Some(FRESH_CREDENTIAL_REQUIRED),
            DeliveryClass::ExecutionFailed => Some(EXECUTION_FAILED),
        }
    }
}

/// The bridge's throughput and error counters.
struct DeliveryMetrics {
    attempts: Counter<u64>,
    errors: Counter<u64>,
}

impl DeliveryMetrics {
    fn new(meter: &Meter) -> Self {
        Self {
            attempts: meter
                .u64_counter(DELIVERY_ATTEMPTS)
                .with_description("deliveries dispatched to the router driver, per wiring source")
                .build(),
            errors: meter
                .u64_counter(DELIVERY_ERRORS)
                .with_description("deliveries the router driver refused, per wiring source")
                .build(),
        }
    }

    /// Every delivery counts as an attempt; a refusal also counts once against
    /// the error series under its own label, so the delivered rate is the
    /// difference and needs no third instrument.
    fn record(&self, attributes: &[KeyValue], class: DeliveryClass) {
        self.attempts.add(1, attributes);
        let Some(error) = class.error() else {
            return;
        };
        let mut attributes = attributes.to_vec();
        attributes.push(KeyValue::new(DELIVERY_ERROR, error));
        self.errors.add(1, &attributes);
    }
}

/// The dimensions `deliver` already holds. Every value is read off the loaded
/// serving manifest, which `resolve_target` refuses to look past, so the series
/// count is fixed for the life of the process at one per manifest attachment
/// and registration. `wamn.wiring.version` is unbounded across releases but
/// constant within a process, so it churns at the release rate, not the
/// delivery rate. A route has no wiring, so its source names it alone.
fn delivery_attributes(source: SourceRef<'_>, target: &AttachmentTarget) -> Vec<KeyValue> {
    let mut attributes = vec![
        KeyValue::new(SOURCE_KIND, source.kind()),
        KeyValue::new(SOURCE_ID, source.id().to_owned()),
    ];
    if let AttachmentTarget::Wiring {
        wiring_id,
        wiring_version,
    } = target
    {
        attributes.push(KeyValue::new(WIRING_ID, wiring_id.clone()));
        attributes.push(KeyValue::new(WIRING_VERSION, i64::from(*wiring_version)));
    }
    attributes
}

/// How one route call settled: what the caller receives, and the live view's
/// label and result for it.
#[derive(Debug)]
struct RouteSettlement {
    outcome: DeliveryOutcome,
    label: &'static str,
    result: serde_json::Value,
}

/// Lower the one export call of a route.
///
/// A route never retries, so a retryable or rate-limited error lowers to the
/// failure that a wiring reports after its last attempt, and the caller sees
/// one shape. The labels and results are the ones [`settled_preview`] gives
/// the same wiring outcome.
fn settle_route(
    outcome: Result<node_types::Emission, node_types::NodeError>,
) -> anyhow::Result<RouteSettlement> {
    let failed = |kind, detail: node_types::ErrorDetail| RouteSettlement {
        label: "failed",
        result: serde_json::json!({"code": detail.code, "message": detail.message}),
        outcome: DeliveryOutcome::Failed(DeliveryFailure {
            kind,
            code: detail.code,
            message: detail.message,
        }),
    };
    Ok(match outcome {
        Ok(emission) => {
            let payload: serde_json::Value = serde_json::from_str(&emission.payload)
                .map_err(|_| anyhow::anyhow!("wamn:node emitted invalid JSON"))?;
            RouteSettlement {
                outcome: DeliveryOutcome::Respond(serde_json::to_string(&payload)?),
                label: "respond",
                result: payload,
            }
        }
        Err(node_types::NodeError::Retryable(detail)) => {
            failed(WireFailureKind::RetryExhausted, detail)
        }
        Err(node_types::NodeError::RateLimited(limited)) => {
            failed(WireFailureKind::RetryExhausted, limited.detail)
        }
        Err(node_types::NodeError::Terminal(detail)) => failed(WireFailureKind::Terminal, detail),
        Err(node_types::NodeError::InvalidInput(detail)) => {
            failed(WireFailureKind::InvalidInput, detail)
        }
        Err(node_types::NodeError::Cancelled) => RouteSettlement {
            outcome: DeliveryOutcome::Cancelled,
            label: "cancelled",
            result: serde_json::Value::Null,
        },
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn labels_only_use_actors_from_successful_result_values() {
        let actor = "01234567-89ab-cdef-0123-456789abcdef";
        let refused = "11234567-89ab-cdef-0123-456789abcdef";
        let payload = serde_json::json!([
            {"request_id": refused, "created_by": refused, "value": {"item": [
                {"created_by": actor, "updated_by": actor, "changed_by": refused},
                {"created_by": "not-an-id"}
            ]}},
            {"error": {"created_by": refused}, "value": {"created_by": refused}}
        ])
        .to_string();
        assert_eq!(super::result_actors(&payload), [actor]);
        assert!(super::result_actors("invalid").is_empty());
    }

    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use wamn_router::{ErrorDetail, Failure, FailureKind, Outcome, Verdict, WalkStatus};

    use super::wiring::{lower_outcome, settled_preview};
    use super::*;

    const MANIFEST: &[u8] = br#"{"attachments":{"orders-http":{"auth-policy":{"modes":["none"]},"definition":{"id":"orders-http","kind":"http","run-deadline-ms":30000},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","kind":"http","package-id":"manifest_mint","wiring-id":"orders","wiring-version":1}},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"},{"component":"transform","digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"}],"format-version":3,"registrations":{"manifest_mint::orders-changed":{"entity":"orders","ops":["insert","update"],"package-id":"manifest_mint","source-package-id":"manifest_mint","wiring-id":"shipping","wiring-version":2}},"release":{"effective-release-id":3,"environment":"prod","packages":[{"package-id":"manifest_mint","package-version":"1.0.0"}],"tenant-id":"manifest-mint-tenant"},"routes":[],"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"manifest_mint","wiring-id":"orders","wiring-version":1},{"graph-hash":"sha256:4444444444444444444444444444444444444444444444444444444444444444","package-id":"manifest_mint","wiring-id":"shipping","wiring-version":2}]}"#;

    fn manifest() -> ServingManifest {
        ServingManifest::from_canonical_bytes(MANIFEST)
            .expect("format-1 fixture is canonical")
            .0
    }

    #[test]
    fn a_registration_delivery_executes_as_the_materializer_and_an_attachment_as_its_caller() {
        assert_eq!(
            SourceRef::Registration("manifest_mint::orders-changed").platform(),
            Some(PlatformComponent::Materializer)
        );
        assert_eq!(SourceRef::Attachment("orders-http").platform(), None);
    }

    #[test]
    fn source_ids_resolve_only_the_manifest_target_and_derive_caller_attachment() {
        assert_eq!(
            resolve_target(&manifest(), SourceRef::Attachment("orders-http")),
            Some(ResolvedTarget {
                package_id: "manifest_mint".into(),
                target: AttachmentTarget::Wiring {
                    wiring_id: "orders".into(),
                    wiring_version: 1,
                },
                caller_attached: true,
                anonymous_caller_permitted: Some(true),
                registered_operation: None,
            })
        );
        assert_eq!(
            resolve_target(
                &manifest(),
                SourceRef::Registration("manifest_mint::orders-changed"),
            ),
            Some(ResolvedTarget {
                package_id: "manifest_mint".into(),
                target: AttachmentTarget::Wiring {
                    wiring_id: "shipping".into(),
                    wiring_version: 2,
                },
                caller_attached: false,
                anonymous_caller_permitted: None,
                registered_operation: None,
            })
        );
        assert_eq!(
            resolve_target(&manifest(), SourceRef::Attachment("shipping")),
            None,
            "a wiring id is not an attachment id and cannot bypass the projection"
        );
    }

    #[test]
    fn caller_handle_must_match_the_loaded_attachment_identity() {
        let anonymous = resolve_target(&manifest(), SourceRef::Attachment("orders-http"))
            .expect("the fixture names the anonymous attachment");
        assert!(caller_matches_source(
            SourceRef::Attachment("orders-http"),
            anonymous.anonymous_caller_permitted,
            None,
        ));

        let mut protected_manifest = manifest();
        protected_manifest
            .attachments
            .get_mut("orders-http")
            .expect("the fixture names the protected attachment")
            .auth_policy = serde_json::json!({"modes": ["pat"]});
        let protected = resolve_target(&protected_manifest, SourceRef::Attachment("orders-http"))
            .expect("the protected attachment still resolves");
        assert!(!caller_matches_source(
            SourceRef::Attachment("orders-http"),
            protected.anonymous_caller_permitted,
            None,
        ));
        assert!(!caller_matches_source(
            SourceRef::Attachment("orders-http"),
            protected.anonymous_caller_permitted,
            Some("other-http"),
        ));
        assert!(caller_matches_source(
            SourceRef::Attachment("orders-http"),
            protected.anonymous_caller_permitted,
            Some("orders-http"),
        ));

        let registration = resolve_target(
            &protected_manifest,
            SourceRef::Registration("manifest_mint::orders-changed"),
        )
        .expect("the fixture names the callerless registration");
        assert!(caller_matches_source(
            SourceRef::Registration("manifest_mint::orders-changed"),
            registration.anonymous_caller_permitted,
            None,
        ));
        assert!(!caller_matches_source(
            SourceRef::Registration("manifest_mint::orders-changed"),
            registration.anonymous_caller_permitted,
            Some("orders-http"),
        ));
    }

    #[test]
    fn permission_denial_lowers_the_exact_registered_operation() {
        let operation = "manifest-mint:order/get@3.0.0";
        let mut registered = manifest();
        registered
            .attachments
            .get_mut("orders-http")
            .expect("the fixture attachment exists")
            .registered_operation = Some(operation.to_owned());
        let target = resolve_target(&registered, SourceRef::Attachment("orders-http"))
            .expect("the registered attachment resolves from the loaded release");
        let denial =
            authorize_registered_operation(None, target.registered_operation.as_deref(), false)
                .expect_err("a callerless registered invocation is denied");

        assert_eq!(denial.operation(), operation);
        assert!(matches!(
            lower_operation_refusal(&denial),
            DeliveryError::PermissionDenied(PermissionDenial { operation: denied })
                if denied == operation
        ));
    }

    #[test]
    fn nested_permission_denial_uses_the_direct_call_wire_contract() {
        let operation = "platform-fixture:widget/record-batch@1.0.0";
        let error = anyhow::Error::new(OperationRefusal::new(
            OperationRefusalKind::PermissionDenied,
            operation,
        ))
        .context("invoke nested operation");
        let denial = error
            .downcast_ref::<OperationRefusal>()
            .expect("context must retain the nested permission denial")
            .clone();

        assert!(matches!(
            lower_operation_refusal(&denial),
            DeliveryError::PermissionDenied(PermissionDenial { operation: denied })
                if denied == operation
        ));
    }

    #[test]
    fn nested_fresh_only_refusal_retains_its_exact_wire_contract() {
        let operation = "platform-fixture:widget/record-batch@1.0.0";
        let error = anyhow::Error::new(OperationRefusal::new(
            OperationRefusalKind::FreshCredentialRequired,
            operation,
        ))
        .context("invoke nested operation");
        let refusal = error
            .downcast_ref::<OperationRefusal>()
            .expect("the nested host boundary must retain the operation refusal")
            .clone();
        assert_eq!(
            refusal.kind(),
            OperationRefusalKind::FreshCredentialRequired
        );
        assert!(matches!(
            lower_operation_refusal(&refusal),
            DeliveryError::FreshCredentialRequired(PermissionDenial { operation: refused })
                if refused == operation
        ));
        assert_eq!(
            DeliveryClass::FreshCredentialRequired.error(),
            Some("fresh-credential-required")
        );
    }

    #[test]
    fn host_mints_current_causation_and_only_inherits_parent_root_depth() {
        assert_eq!(
            derived_causation("delivery-1", None).unwrap(),
            Causation {
                run: "delivery-1".into(),
                root: "delivery-1".into(),
                depth: 0,
            }
        );
        assert_eq!(
            derived_causation(
                "delivery-2",
                Some(ParentCausation {
                    root: "delivery-1".into(),
                    depth: 3,
                })
            )
            .unwrap(),
            Causation {
                run: "delivery-2".into(),
                root: "delivery-1".into(),
                depth: 4,
            }
        );
        assert!(
            derived_causation(
                "delivery-2",
                Some(ParentCausation {
                    root: String::new(),
                    depth: 1,
                })
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_mapping_preserves_each_router_class_without_node_coordinates() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(Failure {
                node: "retired-coordinate".into(),
                kind: FailureKind::InvalidInput,
                detail: ErrorDetail::coded("bad-order", "order is invalid"),
            }),
            hops: 1,
            verdict: None,
        };

        let DeliveryOutcome::Failed(failure) = lower_outcome(outcome).expect("failure maps") else {
            panic!("failed walk must remain a failed delivery")
        };
        assert!(matches!(failure.kind, WireFailureKind::InvalidInput));
        assert_eq!(failure.code.as_deref(), Some("bad-order"));
        assert_eq!(failure.message, "order is invalid");
    }

    /// A route answers each node outcome exactly as its one-node respond
    /// wiring does: the caller's outcome and the live view's label and result
    /// both match. A retryable or rate-limited error reads as the wiring's
    /// failure after its last attempt, because a route never retries.
    #[test]
    fn a_route_settles_each_node_outcome_as_its_one_node_wiring() {
        let detail = || node_types::ErrorDetail {
            message: "order is invalid".into(),
            code: Some("bad-order".into()),
        };
        let failed = |kind| Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(Failure {
                node: "operation".into(),
                kind,
                detail: ErrorDetail::coded("bad-order", "order is invalid"),
            }),
            hops: 1,
            verdict: None,
        };
        let cases = [
            (
                Ok(node_types::Emission {
                    payload: r#"{"accepted":true}"#.into(),
                    port: None,
                }),
                Outcome {
                    status: WalkStatus::Completed,
                    result: serde_json::json!({"accepted": true}),
                    failure: None,
                    hops: 1,
                    verdict: Some(Verdict::Respond {
                        payload: serde_json::json!({"accepted": true}),
                        node_id: "operation".into(),
                    }),
                },
            ),
            (
                Err(node_types::NodeError::Retryable(detail())),
                failed(FailureKind::RetryExhausted),
            ),
            (
                Err(node_types::NodeError::RateLimited(
                    node_types::RateLimitDetail {
                        detail: detail(),
                        retry_after_ms: Some(10),
                    },
                )),
                failed(FailureKind::RetryExhausted),
            ),
            (
                Err(node_types::NodeError::Terminal(detail())),
                failed(FailureKind::Terminal),
            ),
            (
                Err(node_types::NodeError::InvalidInput(detail())),
                failed(FailureKind::InvalidInput),
            ),
            (
                Err(node_types::NodeError::Cancelled),
                outcome_of(WalkStatus::Cancelled, None),
            ),
        ];
        for (route, wiring) in cases {
            let settled = settle_route(route).expect("every node outcome settles");
            let (label, result) = settled_preview(&wiring);
            assert_eq!((settled.label, &settled.result), (label, &*result));
            assert_eq!(
                format!("{:?}", settled.outcome),
                format!(
                    "{:?}",
                    lower_outcome(wiring).expect("the wiring outcome lowers")
                ),
            );
        }

        assert!(
            settle_route(Ok(node_types::Emission {
                payload: "not json".into(),
                port: None,
            }))
            .is_err(),
            "invalid JSON is an execution failure, as on the wiring path"
        );
    }

    #[test]
    fn a_first_verdict_stands_when_later_frontier_work_fails() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(Failure {
                node: "later-terminal".into(),
                kind: FailureKind::SecondVerdict,
                detail: ErrorDetail::coded("second-verdict", "later terminal refused"),
            }),
            hops: 2,
            verdict: Some(Verdict::Respond {
                payload: serde_json::json!({"accepted": true}),
                node_id: "respond".into(),
            }),
        };

        let DeliveryOutcome::Respond(payload) =
            lower_outcome(outcome).expect("the first verdict remains caller truth")
        else {
            panic!("a later failure must not replace the first terminal verdict")
        };
        assert_eq!(payload, r#"{"accepted":true}"#);
    }

    // ---- the instruments ---------------------------------------------------

    /// One meter over an in-memory exporter. The provider is owned by the test,
    /// not by `opentelemetry::global`, so each test reads back exactly the
    /// series its own recorder emitted.
    struct MetricHarness {
        exporter: InMemoryMetricExporter,
        provider: SdkMeterProvider,
    }

    /// One exported series: instrument name, sorted attributes, and value.
    type MetricSeries = (String, Vec<(String, String)>, u64);

    impl MetricHarness {
        fn install() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            Self { exporter, provider }
        }

        fn metrics(&self) -> DeliveryMetrics {
            DeliveryMetrics::new(&self.provider.meter("router-delivery-test"))
        }

        /// Every `(name, sorted attributes, value)` the exporter holds, sorted,
        /// so an assertion names the whole emitted surface and a series that
        /// should not exist cannot hide.
        fn series(&self) -> Vec<MetricSeries> {
            self.provider
                .force_flush()
                .expect("test metrics must flush");
            let mut series = Vec::new();
            for resource in self
                .exporter
                .get_finished_metrics()
                .expect("test metric exporter must remain readable")
            {
                for scope in resource.scope_metrics() {
                    for metric in scope.metrics() {
                        let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
                            panic!("{} must stay a u64 sum", metric.name())
                        };
                        for point in sum.data_points() {
                            let mut attributes: Vec<(String, String)> = point
                                .attributes()
                                .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                                .collect();
                            attributes.sort();
                            series.push((metric.name().to_owned(), attributes, point.value()));
                        }
                    }
                }
            }
            series.sort();
            series
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut labels: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        labels.sort();
        labels
    }

    /// The dimensions are the manifest's own: `resolve_target` refuses an id the
    /// manifest does not name, so nothing outside the manifest can become a
    /// label and the series count is fixed for the process.
    #[test]
    fn a_delivery_is_labelled_by_its_source_kind_id_and_wiring_release() {
        let manifest = manifest();
        let attachment = resolve_target(&manifest, SourceRef::Attachment("orders-http"))
            .expect("the fixture names this attachment");
        assert_eq!(
            delivery_attributes(SourceRef::Attachment("orders-http"), &attachment.target),
            vec![
                KeyValue::new(SOURCE_KIND, "attachment"),
                KeyValue::new(SOURCE_ID, "orders-http"),
                KeyValue::new(WIRING_ID, "orders"),
                KeyValue::new(WIRING_VERSION, 1_i64),
            ]
        );

        let registration = resolve_target(
            &manifest,
            SourceRef::Registration("manifest_mint::orders-changed"),
        )
        .expect("the fixture names this registration");
        assert_eq!(
            delivery_attributes(
                SourceRef::Registration("manifest_mint::orders-changed"),
                &registration.target,
            ),
            vec![
                KeyValue::new(SOURCE_KIND, "registration"),
                KeyValue::new(SOURCE_ID, "manifest_mint::orders-changed"),
                KeyValue::new(WIRING_ID, "shipping"),
                KeyValue::new(WIRING_VERSION, 2_i64),
            ]
        );
    }

    /// A delivered run raises the throughput series and nothing else. If the
    /// error series appeared here, every dashboard's error rate would read 100%.
    #[test]
    fn a_delivered_run_counts_once_and_raises_no_error_series() {
        let harness = MetricHarness::install();
        let attributes = delivery_attributes(
            SourceRef::Attachment("orders-http"),
            &AttachmentTarget::Wiring {
                wiring_id: "orders".to_owned(),
                wiring_version: 1,
            },
        );

        harness
            .metrics()
            .record(&attributes, DeliveryClass::Delivered);

        assert_eq!(
            harness.series(),
            vec![(
                "wamn.router.delivery.attempts".to_owned(),
                labels(&[
                    ("wamn.source.kind", "attachment"),
                    ("wamn.source.id", "orders-http"),
                    ("wamn.wiring.id", "orders"),
                    ("wamn.wiring.version", "1"),
                ]),
                1,
            )]
        );
    }

    /// Every driver refusal counts as an attempt and raises the error series
    /// under its bounded class; the exact operation is never a metric label.
    #[test]
    fn each_driver_refusal_counts_as_an_attempt_and_a_named_error() {
        let harness = MetricHarness::install();
        let metrics = harness.metrics();
        let attributes = delivery_attributes(
            SourceRef::Registration("manifest_mint::orders-changed"),
            &AttachmentTarget::Wiring {
                wiring_id: "shipping".to_owned(),
                wiring_version: 2,
            },
        );

        metrics.record(&attributes, DeliveryClass::PermissionDenied);
        metrics.record(&attributes, DeliveryClass::FreshCredentialRequired);
        metrics.record(&attributes, DeliveryClass::ExecutionFailed);

        let base = [
            ("wamn.source.kind", "registration"),
            ("wamn.source.id", "manifest_mint::orders-changed"),
            ("wamn.wiring.id", "shipping"),
            ("wamn.wiring.version", "2"),
        ];
        let with_error = |error: &str| {
            let mut pairs = base.to_vec();
            pairs.push(("wamn.delivery.error", error));
            labels(&pairs)
        };

        assert_eq!(
            harness.series(),
            vec![
                ("wamn.router.delivery.attempts".to_owned(), labels(&base), 3,),
                (
                    "wamn.router.delivery.errors".to_owned(),
                    with_error("execution-failed"),
                    1,
                ),
                (
                    "wamn.router.delivery.errors".to_owned(),
                    with_error("fresh-credential-required"),
                    1,
                ),
                (
                    "wamn.router.delivery.errors".to_owned(),
                    with_error("permission-denied"),
                    1,
                ),
            ]
        );
    }

    // wamn-hopk R5: the two series were pinned by scanning this file's own
    // implementation half, a technique whose vacuous-match hazard the deleted
    // comment documented. A metric-export contract is a live-probe question.

    fn outcome_of(status: WalkStatus, verdict: Option<Verdict>) -> Outcome {
        Outcome {
            status,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict,
        }
    }

    /// The live view shows the caller's truth, not a second opinion: for every
    /// outcome shape, the preview's label agrees with what `lower_outcome`
    /// actually returns — including the two arms whose ORDER decides the answer.
    #[test]
    fn a_settled_preview_never_contradicts_what_the_caller_received() {
        let respond = Verdict::Respond {
            payload: serde_json::json!({"accepted": true}),
            node_id: "respond".into(),
        };

        let responded = outcome_of(WalkStatus::Completed, Some(respond.clone()));
        let (label, result) = settled_preview(&responded);
        assert_eq!(label, "respond");
        assert_eq!(*result, serde_json::json!({"accepted": true}));

        // A running walk is refused whatever verdict it carries — `lower_outcome`
        // tests `Running` BEFORE the verdict, so a preview that read the verdict
        // first would promise a caller a response it never got.
        let (label, _) = settled_preview(&outcome_of(WalkStatus::Running, Some(respond.clone())));
        assert_eq!(label, EXECUTION_FAILED);
        assert!(matches!(
            lower_outcome(outcome_of(WalkStatus::Running, Some(respond.clone()))),
            Err(DeliveryError::ExecutionFailed)
        ));

        // A first verdict stands over a later frontier failure, so the preview
        // shows the verdict rather than the failure.
        let mut second_verdict = outcome_of(WalkStatus::Failed, Some(respond));
        second_verdict.failure = Some(Failure {
            node: "later-terminal".into(),
            kind: FailureKind::SecondVerdict,
            detail: ErrorDetail::coded("second-verdict", "later terminal refused"),
        });
        assert_eq!(settled_preview(&second_verdict).0, "respond");

        // Verdictless walks read off the status alone.
        assert_eq!(
            settled_preview(&outcome_of(WalkStatus::Cancelled, None)).0,
            "cancelled"
        );
        assert_eq!(
            settled_preview(&outcome_of(WalkStatus::Completed, None)).0,
            EXECUTION_FAILED
        );
        assert_eq!(
            settled_preview(&outcome_of(WalkStatus::Completed, Some(Verdict::Discard))).0,
            "discard"
        );

        // A failure's preview carries the caller's own code and message and
        // nothing the caller did not get.
        let mut failed = outcome_of(WalkStatus::Failed, None);
        failed.failure = Some(Failure {
            node: "retired-coordinate".into(),
            kind: FailureKind::InvalidInput,
            detail: ErrorDetail::coded("bad-order", "order is invalid"),
        });
        let (label, result) = settled_preview(&failed);
        assert_eq!(label, "failed");
        assert_eq!(
            *result,
            serde_json::json!({"code": "bad-order", "message": "order is invalid"})
        );
    }
}

#[cfg(test)]
mod partial_tests {
    use super::bindings::wamn::router_delivery::delivery::{
        EffectOutcome as WireEffectOutcome, FailedOutcome,
    };
    use super::wiring::lower_with_evidence;
    use super::*;
    use crate::router_response::PartialEvidence;
    use serde_json::json;
    use wamn_router::{FailureKind, Outcome, Verdict, WalkStatus};

    fn evidence() -> PartialEvidence {
        PartialEvidence {
            committed_result: json!([{"request_id":"move-1","value":{"movement_id":"movement-1"}}]),
            effect_outcome: Some(wamn_execution_contract::EffectOutcome::ResponseLost),
        }
    }

    #[test]
    fn declared_evidence_crosses_the_bridge_with_the_original_failure() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: json!({"unselected":"must not cross the boundary"}),
            failure: Some(wamn_router::Failure {
                node: "store".to_owned(),
                kind: FailureKind::Terminal,
                detail: wamn_router::ErrorDetail::coded("write_failed", "label store failed"),
            }),
            hops: 3,
            verdict: None,
        };
        let DeliveryOutcome::PartiallyCompleted(partial) =
            lower_with_evidence(outcome, Some(evidence())).unwrap()
        else {
            panic!("a declared committed result must survive the downstream failure")
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&partial.committed_result).unwrap(),
            evidence().committed_result
        );
        assert!(matches!(
            partial.effect_outcome,
            Some(WireEffectOutcome::ResponseLost)
        ));
        let FailedOutcome::Failed(failure) = partial.failed_outcome else {
            panic!("original node failure")
        };
        assert!(matches!(failure.kind, WireFailureKind::Terminal));
        assert_eq!(failure.code.as_deref(), Some("write_failed"));
        assert_eq!(failure.message, "label store failed");
    }

    #[test]
    fn existing_verdict_still_wins_over_later_partial_evidence() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: json!({"later":"ignored"}),
            failure: None,
            hops: 3,
            verdict: Some(Verdict::Respond {
                node_id: "respond".to_owned(),
                payload: json!({"first":true}),
            }),
        };
        let DeliveryOutcome::Respond(payload) =
            lower_with_evidence(outcome, Some(evidence())).unwrap()
        else {
            panic!("first verdict stands")
        };
        assert_eq!(payload, r#"{"first":true}"#);
    }
}
