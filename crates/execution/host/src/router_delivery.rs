//! Guest delivery. A route calls its one operation on the operation host.

use std::fmt;
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use wamn_catalog::{AttachmentTarget, OperationType, ServingRoute};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{
    DeliveryClass, DeliveryError, DeliveryOutcome, DeliveryReport, DeliveryRequest,
    OperationRefusal, OperationRefusalType, ROUTER_DELIVERY_ID, RouteDelivery, RouteSettlement,
    Source, SourceRef, derived_causation, lower_operation_refusal, resolve_authorized_target,
    settle_route,
};
use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;

use crate::operation::OperationHost;
use crate::read_cache::{changes_tag, get_tag, is_feed_read, list_tag, matches};
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
const DELIVERY_ERROR: &str = "wamn.delivery.error";

/// A node deadline changed by the host execution limit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct DeadlineAdjustment {
    pub node: String,
    pub requested_ms: u64,
    pub effective_ms: u64,
}

/// The one bridge shared by attachment and registration ingress.
#[derive(Clone)]
pub struct RouterDeliveryBridge {
    /// The route path calls this host directly and never enters the driver.
    operations: Arc<OperationHost>,
    release: Arc<LoadedRelease>,
    jetstream: Arc<WamnJetstream>,
    metrics: Option<DeliveryMetrics>,
}

impl RouterDeliveryBridge {
    /// Bind the bridge to the process's operation host, its loaded manifest,
    /// and the event transport.
    pub fn new(
        operations: Arc<OperationHost>,
        jetstream: Arc<WamnJetstream>,
        project: &str,
    ) -> anyhow::Result<Self> {
        let release = Arc::clone(&operations.release);
        jetstream.bind_derived_scope(
            ROUTER_DELIVERY_ID,
            &release.scope().tenant_id,
            project,
            &release.scope().environment,
        )?;
        Ok(Self {
            operations,
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

    /// The JetStream plugin that publishes derived events.
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
        _deadline_adjustments: &mut Vec<DeadlineAdjustment>,
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
        let attributes = match &self.metrics {
            Some(_) => delivery_attributes(source, &target.target),
            None => Vec::new(),
        };
        let AttachmentTarget::Route {
            component,
            operation,
        } = &target.target;
        let read = self.release.manifest().routes.iter().find(|route| {
            route.package_id == target.package_id
                && route.component == *component
                && route.operation == *operation
                && route.type_.is_read()
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
        match result {
            Ok(settled) => {
                self.record(&attributes, DeliveryClass::Delivered);
                Ok(settled.outcome)
            }
            Err(error) => self.refuse(&attributes, &error),
        }
    }

    /// Call one route, with the ETag of a read (`docs/architecture/execution.md`).
    ///
    /// A list reads the WAL position before it runs, and a
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
            .manifest_digest()
            .to_string();
        let not_modified = || RouteSettlement {
            outcome: DeliveryOutcome::NotModified,
            result: serde_json::Value::Null,
        };
        // A fresh signed URL is a delivery result even when its row is unchanged.
        // Never answer 304 for a result that holds an expiring capability.
        let signs_files = self.release.manifest().signs_file_results(
            call.package_id,
            call.component,
            call.operation,
        );
        let read = read.filter(|_| !signs_files);
        let mut tag = None;
        // A change feed read takes its tag from the cursor it returns, so it
        // runs before the tag is known.
        let feed = is_feed_read(call.payload);
        if let Some(read) =
            read.filter(|read| read.type_ != OperationType::Get && !read.reads.is_empty() && !feed)
        {
            authorize_route(&self.operations, &call).await?;
            match self.list_tag(read, &release).await {
                Ok(list) if if_none_match.is_some_and(|value| matches(value, &list)) => {
                    *etag = Some(list);
                    return Ok(not_modified());
                }
                Ok(list) => tag = Some(list),
                Err(error) => {
                    tracing::warn!(%error, "model version unavailable; the list has no ETag");
                }
            }
        }
        let settled = settle_route(invoke_route(&self.operations, call).await?)?;
        if !matches!(settled.outcome, DeliveryOutcome::Respond(_)) {
            return Ok(settled);
        }
        if feed {
            tag = changes_tag(&release, &settled.result);
        } else if let Some(revision) = read.and_then(|read| read.revision.as_deref()) {
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

    /// The weak ETag of a list from its relations and the current WAL position.
    async fn list_tag(&self, read: &ServingRoute, release: &str) -> anyhow::Result<String> {
        let version = self
            .operations
            .postgres
            .model_version(&self.operations.project)
            .await?;
        Ok(list_tag(release, &read.reads, &version))
    }

    /// Settle a delivery that the route path or the driver refused, or that
    /// failed to execute.
    pub fn refuse(
        &self,
        attributes: &[KeyValue],
        error: &anyhow::Error,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        match error {
            error if error.downcast_ref::<OperationRefusal>().is_some() => {
                let denial = error
                    .downcast_ref::<OperationRefusal>()
                    .expect("the guarded branch carries an operation refusal")
                    .clone();
                let class = match denial.refusal_type() {
                    OperationRefusalType::PermissionDenied => DeliveryClass::PermissionDenied,
                };
                self.record(attributes, class);
                Err(lower_operation_refusal(&denial))
            }
            error => {
                self.record(attributes, DeliveryClass::ExecutionFailed);
                // The whole chain, not the top context: `invoke wiring node
                // "store"` alone told six cluster runs nothing about WHY
                // (wamn-362o.45).
                tracing::warn!(error = %format_args!("{error:#}"), "router delivery execution failed");
                Err(DeliveryError::ExecutionFailed)
            }
        }
    }
}

#[async_trait::async_trait]
impl RouteDelivery for RouterDeliveryBridge {
    async fn deliver(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport {
        let mut deadline_adjustments = Vec::new();
        let mut etag = None;
        let outcome = self
            .deliver_inner(request, caller, &mut deadline_adjustments, &mut etag)
            .await;
        DeliveryReport {
            outcome,
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

impl fmt::Debug for RouterDeliveryBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouterDeliveryBridge")
            .field("operations", &self.operations)
            .field("release", &self.release.release())
            .finish_non_exhaustive()
    }
}

/// The bridge's throughput and error counters.
#[derive(Clone)]
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
fn delivery_attributes(source: SourceRef<'_>, _target: &AttachmentTarget) -> Vec<KeyValue> {
    vec![
        KeyValue::new(SOURCE_KIND, source.source_type()),
        KeyValue::new(SOURCE_ID, source.id().to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use wamn_catalog::ServingManifest;

    const MANIFEST: &[u8] = br#"{"attachments":{"orders-http":{"auth-policy":{"modes":["none"]},"component":"http-request","definition":{"id":"orders-http","run-deadline-ms":30000,"type":"http"},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","operation":"wamn:node/handler@0.1.0","package-id":"manifest_mint","type":"http"}},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"},{"descriptor":{"component":"transform","component-digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"}],"format-version":11,"release":{"packages":[{"package-id":"manifest_mint","package-version":"1.0.0"}]},"routes":[{"component":"http-request","operation":"wamn:node/handler@0.1.0","package-id":"manifest_mint","type":"command"}]}"#;

    fn manifest() -> ServingManifest {
        ServingManifest::from_canonical_bytes(MANIFEST)
            .expect("format-7 fixture is canonical")
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
    fn a_delivery_is_labelled_by_its_source_type_and_id() {
        let manifest = manifest();
        let attachment =
            resolve_authorized_target(&manifest, SourceRef::Attachment("orders-http"), None)
                .expect("the fixture names this attachment");
        assert_eq!(
            delivery_attributes(SourceRef::Attachment("orders-http"), &attachment.target),
            vec![
                KeyValue::new(SOURCE_KIND, "attachment"),
                KeyValue::new(SOURCE_ID, "orders-http"),
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
            &AttachmentTarget::Route {
                component: "http-request".to_owned(),
                operation: "wamn:node/handler@0.1.0".to_owned(),
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
            SourceRef::Attachment("orders-http"),
            &AttachmentTarget::Route {
                component: "http-request".to_owned(),
                operation: "wamn:node/handler@0.1.0".to_owned(),
            },
        );

        metrics.record(&attributes, DeliveryClass::PermissionDenied);
        metrics.record(&attributes, DeliveryClass::ExecutionFailed);

        let base = [
            ("wamn.source.kind", "attachment"),
            ("wamn.source.id", "orders-http"),
        ];
        let with_error = |error: &str| {
            let mut pairs = base.to_vec();
            pairs.push(("wamn.delivery.error", error));
            labels(&pairs)
        };

        assert_eq!(
            harness.series(),
            vec![
                ("wamn.router.delivery.attempts".to_owned(), labels(&base), 2,),
                (
                    "wamn.router.delivery.errors".to_owned(),
                    with_error("execution-failed"),
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
