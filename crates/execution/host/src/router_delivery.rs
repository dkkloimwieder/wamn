//! Guest delivery into the single production router driver.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use wamn_catalog::{AttachmentKind, ServingManifest};
use wamn_event_wire::Causation;
use wamn_router::{FailureKind, Outcome, Verdict, WalkStatus};
use wamn_runtime::plugins::wamn_jetstream::{DerivedPublishRequest, WamnJetstream};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::{PreloadedWiringMissing, RouterDriver, RouterDriverRequest, WiringResolution};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "wit",
        world: "router-delivery-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::router_delivery::delivery::{
    self, DeliveryError, DeliveryFailure, DeliveryOutcome, DeliveryRequest, Emission,
    FailureKind as WireFailureKind, ParentCausation, Source,
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

/// The one bridge shared by attachment and registration ingress.
pub struct RouterDeliveryBridge {
    driver: Arc<RouterDriver>,
    release: Arc<ReleaseManifestWeld>,
    jetstream: Arc<WamnJetstream>,
    metrics: Option<DeliveryMetrics>,
}

impl RouterDeliveryBridge {
    /// Bind the bridge to the process's existing driver and welded manifest.
    pub fn new(
        driver: Arc<RouterDriver>,
        release: Arc<ReleaseManifestWeld>,
        jetstream: Arc<WamnJetstream>,
        project: &str,
    ) -> anyhow::Result<Self> {
        jetstream.bind_derived_scope(
            ROUTER_DELIVERY_ID,
            &release.manifest().release.tenant_id,
            project,
            &release.manifest().release.environment,
        )?;
        Ok(Self {
            driver,
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

    async fn deliver(&self, request: DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError> {
        let DeliveryRequest {
            source,
            delivery_id,
            payload,
            caller,
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
        let target =
            resolve_target(self.release.manifest(), source).ok_or(DeliveryError::SourceNotFound)?;

        if !target.caller_attached && caller.is_some() {
            return Err(DeliveryError::InvalidRequest);
        }
        let (role, user_id) = match caller {
            Some(caller)
                if caller.role.as_deref() == Some("") || caller.user_id.as_deref() == Some("") =>
            {
                return Err(DeliveryError::InvalidRequest);
            }
            Some(caller) => (caller.role, caller.user_id),
            None => (None, None),
        };
        let (traceparent, tracestate) = match trace {
            Some(trace) if trace.traceparent.is_empty() => {
                return Err(DeliveryError::InvalidRequest);
            }
            Some(trace) => (Some(trace.traceparent), trace.tracestate),
            None => (None, None),
        };
        let release = &self.release.manifest().release;
        let request = RouterDriverRequest {
            tenant_id: release.tenant_id.clone(),
            catalog_id: release.catalog_id.clone(),
            environment: release.environment.clone(),
            wiring_id: target.wiring_id,
            wiring_version: target.wiring_version,
            delivery_id,
            payload,
            caller_attached: target.caller_attached,
            resolution: target.resolution,
            role,
            user_id,
            traceparent,
            tracestate,
        };
        // The last point that still holds every dimension: the request moves
        // into the driver on the next line.
        let attributes = match &self.metrics {
            Some(_) => delivery_attributes(source, &request.wiring_id, request.wiring_version),
            None => Vec::new(),
        };
        match self.driver.execute(request).await {
            Ok(delivery) => {
                self.record(&attributes, DeliveryClass::Delivered);
                self.publish_emit(&delivery.outcome, causation).await?;
                lower_outcome(delivery.outcome)
            }
            Err(error) if error.downcast_ref::<PreloadedWiringMissing>().is_some() => {
                self.record(&attributes, DeliveryClass::WiringNotPreloaded);
                Err(DeliveryError::WiringNotPreloaded)
            }
            Err(error) => {
                self.record(&attributes, DeliveryClass::ExecutionFailed);
                tracing::warn!(error = %error, "router delivery execution failed");
                Err(DeliveryError::ExecutionFailed)
            }
        }
    }

    async fn publish_emit(
        &self,
        outcome: &Outcome,
        causation: Causation,
    ) -> Result<(), DeliveryError> {
        let Some(Verdict::Emit {
            event,
            dedup_id,
            entity,
            operation,
        }) = outcome.verdict.as_ref()
        else {
            return Ok(());
        };
        self.jetstream
            .publish_derived(DerivedPublishRequest {
                component_id: ROUTER_DELIVERY_ID.to_owned(),
                entity: entity.clone(),
                operation: *operation,
                payload: event.clone(),
                dedup_id: dedup_id.clone(),
                causation,
            })
            .await
            .map(|_| ())
            .map_err(|error| {
                tracing::warn!(
                    error = %error,
                    error_kind = ?error.kind(),
                    "derived-event publication did not receive a server ACK"
                );
                DeliveryError::ExecutionFailed
            })
    }
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
            .field("driver", &self.driver)
            .field("release", &self.release.release())
            .finish()
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

impl delivery::Host for ActiveCtx<'_> {
    async fn deliver(
        &mut self,
        request: DeliveryRequest,
    ) -> wash_runtime::wasmtime::Result<Result<DeliveryOutcome, DeliveryError>> {
        let plugin = plugin_of(self)?;
        Ok(plugin.deliver(request).await)
    }
}

#[derive(Debug, Clone, Copy)]
enum SourceRef<'a> {
    Attachment(&'a str),
    Registration(&'a str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedTarget {
    wiring_id: String,
    wiring_version: u32,
    caller_attached: bool,
    resolution: WiringResolution,
}

fn resolve_target(manifest: &ServingManifest, source: SourceRef<'_>) -> Option<ResolvedTarget> {
    match source {
        SourceRef::Attachment(id) => {
            manifest
                .attachments
                .get(id)
                .map(|attachment| ResolvedTarget {
                    wiring_id: attachment.wiring_id.clone(),
                    wiring_version: attachment.wiring_version,
                    caller_attached: true,
                    resolution: if matches!(
                        attachment.kind,
                        AttachmentKind::Http | AttachmentKind::Internal | AttachmentKind::Studio
                    ) {
                        WiringResolution::Preloaded
                    } else {
                        WiringResolution::Frozen
                    },
                })
        }
        SourceRef::Registration(id) => {
            manifest
                .registrations
                .get(id)
                .map(|registration| ResolvedTarget {
                    wiring_id: registration.wiring_id.clone(),
                    wiring_version: registration.wiring_version,
                    caller_attached: false,
                    resolution: WiringResolution::Frozen,
                })
        }
    }
}

/// How the router driver answered one delivery. The three variants are the
/// three arms of the driver match in [`RouterDeliveryBridge::deliver`]; the
/// bridge classifies nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryClass {
    Delivered,
    WiringNotPreloaded,
    ExecutionFailed,
}

impl DeliveryClass {
    /// The `wamn.delivery.error` value, or `None` for the one delivered class.
    fn error(self) -> Option<&'static str> {
        match self {
            DeliveryClass::Delivered => None,
            DeliveryClass::WiringNotPreloaded => Some("wiring-not-preloaded"),
            DeliveryClass::ExecutionFailed => Some("execution-failed"),
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

/// The dimensions `deliver` already holds. Every value is read off the welded
/// serving manifest, which `resolve_target` refuses to look past, so the series
/// count is fixed for the life of the process at one per manifest attachment
/// and registration. `wamn.wiring.version` is unbounded across releases but
/// constant within a process, so it churns at the release rate, not the
/// delivery rate.
fn delivery_attributes(
    source: SourceRef<'_>,
    wiring_id: &str,
    wiring_version: u32,
) -> Vec<KeyValue> {
    let (kind, id) = match source {
        SourceRef::Attachment(id) => ("attachment", id),
        SourceRef::Registration(id) => ("registration", id),
    };
    vec![
        KeyValue::new(SOURCE_KIND, kind),
        KeyValue::new(SOURCE_ID, id.to_owned()),
        KeyValue::new(WIRING_ID, wiring_id.to_owned()),
        KeyValue::new(WIRING_VERSION, i64::from(wiring_version)),
    ]
}

fn lower_outcome(outcome: Outcome) -> Result<DeliveryOutcome, DeliveryError> {
    if matches!(outcome.status, WalkStatus::Running) {
        return Err(DeliveryError::ExecutionFailed);
    }
    if let Some(verdict) = outcome.verdict {
        // A terminal may settle the delivery before the rest of the frontier
        // drains. If later work fails (including SecondVerdict), the router's
        // explicit invariant is that the first verdict stands. Preserve that
        // caller truth and keep the later failure observable host-side.
        if let Some(failure) = outcome.failure {
            tracing::warn!(
                failure_kind = ?failure.kind,
                failure_code = failure.detail.code.as_deref(),
                failure_message = %failure.detail.message,
                "router delivery failed after its terminal verdict; first verdict stands"
            );
        }
        return lower_verdict(verdict);
    }
    match outcome.status {
        WalkStatus::Completed => Err(DeliveryError::ExecutionFailed),
        WalkStatus::Failed => outcome
            .failure
            .map(|failure| {
                DeliveryOutcome::Failed(DeliveryFailure {
                    kind: lower_failure_kind(failure.kind),
                    code: failure.detail.code,
                    message: failure.detail.message,
                })
            })
            .ok_or(DeliveryError::ExecutionFailed),
        WalkStatus::Cancelled => Ok(DeliveryOutcome::Cancelled),
        WalkStatus::Running => Err(DeliveryError::ExecutionFailed),
    }
}

fn lower_verdict(verdict: Verdict) -> Result<DeliveryOutcome, DeliveryError> {
    match verdict {
        Verdict::Respond { payload, .. } => serde_json::to_string(&payload)
            .map(DeliveryOutcome::Respond)
            .map_err(|_| DeliveryError::ExecutionFailed),
        Verdict::Emit {
            event, dedup_id, ..
        } => serde_json::to_string(&event)
            .map(|event| DeliveryOutcome::Emit(Emission { event, dedup_id }))
            .map_err(|_| DeliveryError::ExecutionFailed),
        Verdict::Discard => Ok(DeliveryOutcome::Discard),
    }
}

fn lower_failure_kind(kind: FailureKind) -> WireFailureKind {
    match kind {
        FailureKind::Terminal => WireFailureKind::Terminal,
        FailureKind::RetryExhausted => WireFailureKind::RetryExhausted,
        FailureKind::InvalidInput => WireFailureKind::InvalidInput,
        FailureKind::HopLimit => WireFailureKind::HopLimit,
        FailureKind::UnreleasedCaller => WireFailureKind::UnreleasedCaller,
        FailureKind::MissingDedupId => WireFailureKind::MissingDedupId,
        FailureKind::RespondWithoutCaller => WireFailureKind::RespondWithoutCaller,
        FailureKind::SecondVerdict => WireFailureKind::SecondVerdict,
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use wamn_router::{ErrorDetail, Failure};

    use super::*;

    const MANIFEST: &[u8] = br#"{"attachments":{"orders-http":{"auth-policy":{"mode":"none"},"definition":{"id":"orders-http","kind":"http","run-deadline-ms":30000},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","kind":"http","wiring-id":"orders","wiring-version":1}},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1"},{"component":"transform","digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","interface-version":"0.1"}],"format-version":2,"registrations":{"orders-changed":{"entity":"orders","ops":["insert","update"],"wiring-id":"shipping","wiring-version":2}},"release":{"catalog-id":"manifest-mint-catalog","catalog-version":3,"environment":"prod","tenant-id":"manifest-mint-tenant"},"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","wiring-id":"orders","wiring-version":1},{"graph-hash":"sha256:4444444444444444444444444444444444444444444444444444444444444444","wiring-id":"shipping","wiring-version":2}]}"#;

    fn manifest() -> ServingManifest {
        ServingManifest::from_canonical_bytes(MANIFEST)
            .expect("format-2 fixture is canonical")
            .0
    }

    #[test]
    fn source_ids_resolve_only_the_manifest_target_and_derive_caller_attachment() {
        assert_eq!(
            resolve_target(&manifest(), SourceRef::Attachment("orders-http")),
            Some(ResolvedTarget {
                wiring_id: "orders".into(),
                wiring_version: 1,
                caller_attached: true,
                resolution: WiringResolution::Preloaded,
            })
        );
        assert_eq!(
            resolve_target(&manifest(), SourceRef::Registration("orders-changed")),
            Some(ResolvedTarget {
                wiring_id: "shipping".into(),
                wiring_version: 2,
                caller_attached: false,
                resolution: WiringResolution::Frozen,
            })
        );
        assert_eq!(
            resolve_target(&manifest(), SourceRef::Attachment("shipping")),
            None,
            "a wiring id is not an attachment id and cannot bypass the projection"
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
        fn series(&self) -> Vec<(String, Vec<(String, String)>, u64)> {
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
            delivery_attributes(
                SourceRef::Attachment("orders-http"),
                &attachment.wiring_id,
                attachment.wiring_version,
            ),
            vec![
                KeyValue::new(SOURCE_KIND, "attachment"),
                KeyValue::new(SOURCE_ID, "orders-http"),
                KeyValue::new(WIRING_ID, "orders"),
                KeyValue::new(WIRING_VERSION, 1_i64),
            ]
        );

        let registration = resolve_target(&manifest, SourceRef::Registration("orders-changed"))
            .expect("the fixture names this registration");
        assert_eq!(
            delivery_attributes(
                SourceRef::Registration("orders-changed"),
                &registration.wiring_id,
                registration.wiring_version,
            ),
            vec![
                KeyValue::new(SOURCE_KIND, "registration"),
                KeyValue::new(SOURCE_ID, "orders-changed"),
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
        let attributes = delivery_attributes(SourceRef::Attachment("orders-http"), "orders", 1);

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

    /// Both driver refusals count as attempts and both raise the error series,
    /// under labels that tell a missing preloaded wiring apart from any other
    /// execution failure — the one distinction the error series exists for.
    #[test]
    fn each_driver_refusal_counts_as_an_attempt_and_a_named_error() {
        let harness = MetricHarness::install();
        let metrics = harness.metrics();
        let attributes =
            delivery_attributes(SourceRef::Registration("orders-changed"), "shipping", 2);

        metrics.record(&attributes, DeliveryClass::WiringNotPreloaded);
        metrics.record(&attributes, DeliveryClass::ExecutionFailed);

        let base = [
            ("wamn.source.kind", "registration"),
            ("wamn.source.id", "orders-changed"),
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
                ("wamn.router.delivery.attempts".to_owned(), labels(&base), 2,),
                (
                    "wamn.router.delivery.errors".to_owned(),
                    with_error("execution-failed"),
                    1,
                ),
                (
                    "wamn.router.delivery.errors".to_owned(),
                    with_error("wiring-not-preloaded"),
                    1,
                ),
            ]
        );
    }

    /// The two series are dashboard contracts the Prometheus exporter renames,
    /// and the driver match is the only site that can raise them — a driver
    /// arm that stops recording would emit nothing and fail no other test,
    /// because the driver itself needs an engine and a database to run. Pinned
    /// the way `crates/execution/router` pins its cache-hit series.
    #[test]
    fn the_bridge_pins_its_two_series_and_records_on_every_driver_outcome() {
        let source = include_str!("router_delivery.rs");
        for name in [
            "wamn.router.delivery.attempts",
            "wamn.router.delivery.errors",
        ] {
            assert!(source.contains(name), "the {name} series was renamed");
        }
        for class in [
            "DeliveryClass::Delivered",
            "DeliveryClass::WiringNotPreloaded",
            "DeliveryClass::ExecutionFailed",
        ] {
            assert!(
                source.contains(&format!("self.record(&attributes, {class})")),
                "the driver arm recording {class} disappeared"
            );
        }
    }
}
