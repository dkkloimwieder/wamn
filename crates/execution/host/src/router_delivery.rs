//! Guest delivery into the single production router driver.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use wamn_catalog::{AttachmentKind, NO_AUTHENTICATION_MODE, ServingManifest};
use wamn_event_wire::Causation;
use wamn_router::{FailureKind, Outcome, Verdict, WalkStatus};
use wamn_runtime::plugins::wamn_jetstream::{
    DerivedPublishRequest, RouterTapPhase, RouterTapPreview, WamnJetstream,
};
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
    self, CallerContext, DeliveryError, DeliveryFailure, DeliveryOutcome, DeliveryRequest,
    Emission, FailureKind as WireFailureKind, ParentCausation, Source,
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

// The two driver refusals a live view can show. Shared with `DeliveryClass`
// rather than respelled, so a dashboard and a run screen never disagree about
// what happened to the same delivery — pinned by
// `a_refusal_reads_the_same_to_a_dashboard_and_to_a_live_view`.
const WIRING_NOT_PRELOADED: &str = "wiring-not-preloaded";
const EXECUTION_FAILED: &str = "execution-failed";

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

        let (role, user_id) = caller_claims(&target, caller)?;
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
            &target.wiring_id,
            target.wiring_version,
            RouterTapPhase::Accepted,
            &payload,
        )
        .await;

        let release = &self.release.manifest().release;
        let request = RouterDriverRequest {
            tenant_id: release.tenant_id.clone(),
            catalog_id: release.catalog_id.clone(),
            environment: release.environment.clone(),
            // Cloned, not moved: the settled preview after the driver call still
            // has to name the delivery it settles, and `request` is gone by then.
            wiring_id: target.wiring_id.clone(),
            wiring_version: target.wiring_version,
            delivery_id: delivery_id.clone(),
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
                let (outcome, result) = settled_preview(&delivery.outcome);
                self.tap(
                    source,
                    &delivery_id,
                    &target.wiring_id,
                    target.wiring_version,
                    RouterTapPhase::Settled(outcome),
                    &result,
                )
                .await;
                self.publish_emit(&delivery.outcome, causation).await?;
                lower_outcome(delivery.outcome)
            }
            Err(error) if error.downcast_ref::<PreloadedWiringMissing>().is_some() => {
                self.record(&attributes, DeliveryClass::WiringNotPreloaded);
                self.tap(
                    source,
                    &delivery_id,
                    &target.wiring_id,
                    target.wiring_version,
                    RouterTapPhase::Settled(WIRING_NOT_PRELOADED),
                    &serde_json::Value::Null,
                )
                .await;
                Err(DeliveryError::WiringNotPreloaded)
            }
            Err(error) => {
                self.record(&attributes, DeliveryClass::ExecutionFailed);
                self.tap(
                    source,
                    &delivery_id,
                    &target.wiring_id,
                    target.wiring_version,
                    RouterTapPhase::Settled(EXECUTION_FAILED),
                    &serde_json::Value::Null,
                )
                .await;
                tracing::warn!(error = %error, "router delivery execution failed");
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
        wiring_id: &str,
        wiring_version: u32,
        phase: RouterTapPhase,
        payload: &serde_json::Value,
    ) {
        self.jetstream
            .publish_router_tap(
                ROUTER_DELIVERY_ID,
                RouterTapPreview {
                    delivery_id,
                    wiring_id,
                    wiring_version,
                    source_kind: source.kind(),
                    source_id: source.id(),
                    phase,
                    payload,
                },
            )
            .await;
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedTarget {
    wiring_id: String,
    wiring_version: u32,
    caller_attached: bool,
    /// `Some` only for attachment ingress. A callerless attachment is legal
    /// only when its welded auth policy explicitly names anonymous mode.
    anonymous_caller_permitted: Option<bool>,
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
                    anonymous_caller_permitted: Some(
                        attachment
                            .auth_policy
                            .get("mode")
                            .and_then(serde_json::Value::as_str)
                            == Some(NO_AUTHENTICATION_MODE),
                    ),
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
                    anonymous_caller_permitted: None,
                    resolution: WiringResolution::Frozen,
                })
        }
    }
}

fn caller_claims(
    target: &ResolvedTarget,
    caller: Option<CallerContext>,
) -> Result<(Option<String>, Option<String>), DeliveryError> {
    if (!target.caller_attached && caller.is_some())
        || (target.anonymous_caller_permitted == Some(false)
            && caller
                .as_ref()
                .and_then(|caller| caller.user_id.as_deref())
                .is_none())
    {
        return Err(DeliveryError::InvalidRequest);
    }
    match caller {
        Some(caller)
            if caller.role.as_deref() == Some("") || caller.user_id.as_deref() == Some("") =>
        {
            Err(DeliveryError::InvalidRequest)
        }
        Some(caller) => Ok((caller.role, caller.user_id)),
        None => Ok((None, None)),
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
            DeliveryClass::WiringNotPreloaded => Some(WIRING_NOT_PRELOADED),
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
    vec![
        KeyValue::new(SOURCE_KIND, source.kind()),
        KeyValue::new(SOURCE_ID, source.id().to_owned()),
        KeyValue::new(WIRING_ID, wiring_id.to_owned()),
        KeyValue::new(WIRING_VERSION, i64::from(wiring_version)),
    ]
}

/// How one settled delivery reads in the live view: the outcome label, and the
/// result the caller was given.
///
/// This mirrors [`lower_outcome`] arm for arm, INCLUDING its two order-sensitive
/// rulings — a running walk is a failure whatever verdict it carries, and a
/// first verdict stands over a later frontier failure. A live view that
/// disagreed with what the caller actually received would be worse than none,
/// because it would be believed.
///
/// The verdict payloads are BORROWED. The plugin copies only if it will publish,
/// so a host with no data-plane NATS pays nothing for a preview it drops.
fn settled_preview(outcome: &Outcome) -> (&'static str, Cow<'_, serde_json::Value>) {
    if matches!(outcome.status, WalkStatus::Running) {
        return (EXECUTION_FAILED, Cow::Owned(serde_json::Value::Null));
    }
    match outcome.verdict.as_ref() {
        Some(Verdict::Respond { payload, .. }) => ("respond", Cow::Borrowed(payload)),
        Some(Verdict::Emit { event, .. }) => ("emit", Cow::Borrowed(event)),
        Some(Verdict::Discard) => ("discard", Cow::Owned(serde_json::Value::Null)),
        None => match outcome.status {
            WalkStatus::Cancelled => ("cancelled", Cow::Owned(serde_json::Value::Null)),
            WalkStatus::Failed => (
                "failed",
                Cow::Owned(match outcome.failure.as_ref() {
                    // The kind is deliberately absent: it has no wire spelling
                    // of its own, and a `Debug` rendering on a subject a console
                    // parses would drift the moment the enum is edited.
                    Some(failure) => serde_json::json!({
                        "code": failure.detail.code,
                        "message": failure.detail.message,
                    }),
                    None => serde_json::Value::Null,
                }),
            ),
            // A completed walk with no verdict never settled anything, and
            // `lower_outcome` refuses both of these the same way.
            WalkStatus::Completed | WalkStatus::Running => {
                (EXECUTION_FAILED, Cow::Owned(serde_json::Value::Null))
            }
        },
    }
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
                anonymous_caller_permitted: Some(true),
                resolution: WiringResolution::Preloaded,
            })
        );
        assert_eq!(
            resolve_target(&manifest(), SourceRef::Registration("orders-changed")),
            Some(ResolvedTarget {
                wiring_id: "shipping".into(),
                wiring_version: 2,
                caller_attached: false,
                anonymous_caller_permitted: None,
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
    fn attachment_caller_identity_follows_the_welded_auth_policy() {
        const USER_ID: &str = "11111111-1111-4111-8111-111111111111";

        let anonymous = resolve_target(&manifest(), SourceRef::Attachment("orders-http"))
            .expect("the fixture names the anonymous attachment");
        assert_eq!(caller_claims(&anonymous, None), Ok((None, None)));

        let mut protected_manifest = manifest();
        protected_manifest
            .attachments
            .get_mut("orders-http")
            .expect("the fixture names the protected attachment")
            .auth_policy = serde_json::json!({"mode": "bearer"});
        let protected = resolve_target(&protected_manifest, SourceRef::Attachment("orders-http"))
            .expect("the protected attachment still resolves");
        assert!(matches!(
            caller_claims(&protected, None),
            Err(DeliveryError::InvalidRequest)
        ));
        assert!(matches!(
            caller_claims(
                &protected,
                Some(CallerContext {
                    role: None,
                    user_id: None,
                })
            ),
            Err(DeliveryError::InvalidRequest)
        ));
        let authenticated = CallerContext {
            role: None,
            user_id: Some(USER_ID.to_owned()),
        };
        assert_eq!(
            caller_claims(&protected, Some(authenticated)),
            Ok((None, Some(USER_ID.to_owned()))),
            "an authenticated UUID remains unchanged"
        );

        let registration = resolve_target(
            &protected_manifest,
            SourceRef::Registration("orders-changed"),
        )
        .expect("the fixture names the callerless registration");
        assert_eq!(
            caller_claims(&registration, None),
            Ok((None, None)),
            "attachment auth policy does not apply to registrations"
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
