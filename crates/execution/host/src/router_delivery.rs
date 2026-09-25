//! Guest delivery. A route calls its one operation on the operation host, and
//! a wiring target goes to the wiring layer through `WiringDelivery`.

use std::fmt;
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use wamn_catalog::{AdmittedComponent, AttachmentTarget, OperationKind, ServingRoute};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{
    DeliveryClass, DeliveryError, DeliveryOutcome, DeliveryReport, DeliveryRequest,
    EXECUTION_FAILED, FRESH_CREDENTIAL_REQUIRED, OperationRefusal, OperationRefusalKind,
    PERMISSION_DENIED, ROUTER_DELIVERY_ID, RouteDelivery, RouteSettlement, Source, SourceRef,
    derived_causation, lower_operation_refusal, resolve_authorized_target, settle_route,
};
use wamn_event_wire::Causation;
use wamn_runtime::plugins::wamn_jetstream::{RouterTapPhase, RouterTapPreview, WamnJetstream};

use crate::operation::OperationHost;
use crate::read_cache::{get_tag, list_tag, matches};
use crate::route::{RouteCall, authorize_route, invoke_route};

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

/// The wiring arm of the bridge. A route never reaches it.
///
/// The router driver in `wamn-workflow` implements it. This crate never
/// depends on that crate.
#[async_trait::async_trait]
pub trait WiringDelivery: Send + Sync {
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
#[derive(Debug)]
pub struct WiringPreload {
    pub wirings: usize,
    /// The release components the wirings name, or `None` when no
    /// synchronous attachment targets a wiring.
    pub components: Option<Arc<[AdmittedComponent]>>,
}

/// One delivery that the bridge resolved to a wiring target.
#[derive(Debug)]
pub struct WiringCall<'a> {
    pub source: SourceRef<'a>,
    pub delivery_id: String,
    pub package_id: &'a str,
    pub target: &'a AttachmentTarget,
    pub wiring_id: &'a str,
    pub wiring_version: u32,
    pub caller_attached: bool,
    pub payload: serde_json::Value,
    pub caller: Option<AuthenticatedCaller>,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
    pub causation: Causation,
    pub attributes: &'a [KeyValue],
    /// The bridge reports these to the guest beside the outcome.
    pub deadline_adjustments: &'a mut Vec<DeadlineAdjustment>,
}

/// A node deadline changed by the host execution limit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct DeadlineAdjustment {
    pub node: String,
    pub requested_ms: u64,
    pub effective_ms: u64,
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
    /// and the wiring layer that serves wiring targets, if any.
    pub fn new(
        operations: Arc<OperationHost>,
        wirings: Option<Arc<dyn WiringDelivery>>,
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
            wirings,
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

    /// The loaded release this bridge serves.
    pub fn release(&self) -> &Arc<LoadedRelease> {
        &self.release
    }

    /// The JetStream plugin that publishes taps and derived events.
    pub fn jetstream(&self) -> &Arc<WamnJetstream> {
        &self.jetstream
    }

    /// Count one delivery under its class.
    pub fn record(&self, attributes: &[KeyValue], class: DeliveryClass) {
        if let Some(metrics) = &self.metrics {
            metrics.record(attributes, class);
        }
    }

    async fn deliver_inner(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
        deadline_adjustments: &mut Vec<DeadlineAdjustment>,
        etag: &mut Option<String>,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        let DeliveryRequest {
            source,
            delivery_id,
            payload,
            caller: _,
            trace,
            parent_causation,
            if_none_match,
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
                let read = self.release.manifest().routes.iter().find(|route| {
                    route.package_id == target.package_id
                        && route.component == *component
                        && route.operation == *operation
                        && route.kind.is_read()
                });
                let result = self
                    .run_route(
                        RouteCall {
                            attachment_id: source.id(),
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
                        read,
                        if_none_match.as_deref(),
                        etag,
                    )
                    .await;
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

    /// Call one route, with the ETag of a read (`docs/plan/http-reads.md`
    /// section 4.3).
    ///
    /// A list reads the model versions of its relations before it runs, and a
    /// match with If-None-Match answers not-modified without running it. The
    /// caller passes the operation grant first. A `get` runs, and its tag comes
    /// from the revision of the record it returns. A read without a tag, and
    /// every other route, runs as before.
    async fn run_route(
        &self,
        call: RouteCall<'_>,
        read: Option<&ServingRoute>,
        if_none_match: Option<&str>,
        etag: &mut Option<String>,
    ) -> anyhow::Result<RouteSettlement> {
        let release = self
            .operations
            .release_identity()
            .manifest_digest
            .to_string();
        let not_modified = || RouteSettlement {
            outcome: DeliveryOutcome::NotModified,
            label: "not-modified",
            result: serde_json::Value::Null,
        };
        let mut tag = None;
        if let Some(read) =
            read.filter(|read| read.kind != OperationKind::Get && !read.reads.is_empty())
        {
            authorize_route(&self.operations, &call).await?;
            match self.list_tag(read, &release).await {
                Ok(list) if if_none_match.is_some_and(|value| matches(value, &list)) => {
                    *etag = Some(list);
                    return Ok(not_modified());
                }
                Ok(list) => tag = Some(list),
                Err(error) => {
                    tracing::warn!(%error, "model versions unavailable; the list has no ETag");
                }
            }
        }
        let settled = settle_route(invoke_route(&self.operations, call).await?)?;
        if !matches!(settled.outcome, DeliveryOutcome::Respond(_)) {
            return Ok(settled);
        }
        if let Some(revision) = read.and_then(|read| read.revision.as_deref()) {
            tag = get_tag(&release, &settled.result, revision);
        }
        if let Some(tag) = &tag
            && if_none_match.is_some_and(|value| matches(value, tag))
        {
            *etag = Some(tag.clone());
            return Ok(not_modified());
        }
        *etag = tag;
        Ok(settled)
    }

    /// The weak ETag of a list from the current versions of the relations it reads.
    async fn list_tag(&self, read: &ServingRoute, release: &str) -> anyhow::Result<String> {
        let names = read
            .reads
            .iter()
            .map(|relation| (relation.schema.as_str(), relation.relation.as_str()))
            .collect::<Vec<_>>();
        let versions = self
            .operations
            .postgres
            .model_versions(&self.operations.project, &names)
            .await?;
        anyhow::ensure!(
            versions.len() == names.len(),
            "model-version-count-mismatch"
        );
        let versions = read.reads.iter().zip(versions).collect::<Vec<_>>();
        Ok(list_tag(release, &versions))
    }

    /// Settle a delivery that the route path or the driver refused, or that
    /// failed to execute.
    pub async fn refuse(
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
    pub async fn tap(
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

#[async_trait::async_trait]
impl RouteDelivery for RouterDeliveryBridge {
    async fn deliver(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport {
        let label_eligible = caller.is_some() && matches!(request.source, Source::Attachment(_));
        let mut deadline_adjustments = Vec::new();
        let mut etag = None;
        let outcome = self
            .deliver_inner(request, caller, &mut deadline_adjustments, &mut etag)
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
            etag,
            deadline_adjustments: deadline_adjustments
                .into_iter()
                .map(
                    |adjustment| wamn_engine::router_delivery::DeadlineAdjustment {
                        node: adjustment.node,
                        requested_ms: adjustment.requested_ms,
                        effective_ms: adjustment.effective_ms,
                    },
                )
                .collect(),
        }
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

    use super::*;
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use wamn_catalog::ServingManifest;

    const MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"},{"component":"transform","digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"}],"format-version":3,"release":{"effective-release-id":3,"environment":"prod","packages":[{"package-id":"manifest_mint","package-version":"1.0.0"}],"tenant-id":"manifest-mint-tenant"},"routes":[],"workflow":{"attachments":{"orders-http":{"auth-policy":{"modes":["none"]},"definition":{"id":"orders-http","kind":"http","run-deadline-ms":30000},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","kind":"http","package-id":"manifest_mint","wiring-id":"orders","wiring-version":1}},"registrations":{"manifest_mint::orders-changed":{"entity":"orders","ops":["insert","update"],"package-id":"manifest_mint","source-package-id":"manifest_mint","wiring-id":"shipping","wiring-version":2}},"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"manifest_mint","wiring-id":"orders","wiring-version":1},{"graph-hash":"sha256:4444444444444444444444444444444444444444444444444444444444444444","package-id":"manifest_mint","wiring-id":"shipping","wiring-version":2}]}}"#;

    fn manifest() -> ServingManifest {
        ServingManifest::from_canonical_bytes(MANIFEST)
            .expect("format-3 fixture is canonical")
            .0
    }

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
        let attachment =
            resolve_authorized_target(&manifest, SourceRef::Attachment("orders-http"), None)
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

        let registration = resolve_authorized_target(
            &manifest,
            SourceRef::Registration("manifest_mint::orders-changed"),
            None,
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
}
