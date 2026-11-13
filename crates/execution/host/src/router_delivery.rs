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

// Attribute keys. `wamn.source.type` plus `wamn.source.id` rather than the two
// mutually exclusive keys the older instruments use (`wamn.attachment.id`,
// `wamn.registration`), because one counter covers both ingress types and an
// always-empty second key would double the series for nothing.
const SOURCE_TYPE: &str = "wamn.source.type";
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
        jetstream.bind_derived_invocation(
            ROUTER_DELIVERY_ID,
            &release.target().project_env_key,
            project,
            &release.target().environment,
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
        KeyValue::new(SOURCE_TYPE, source.source_type()),
        KeyValue::new(SOURCE_ID, source.id().to_owned()),
    ]
}
