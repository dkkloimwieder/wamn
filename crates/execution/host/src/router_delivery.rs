//! Guest delivery into the single production router driver.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

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

/// The one bridge shared by attachment and registration ingress.
pub struct RouterDeliveryBridge {
    driver: Arc<RouterDriver>,
    release: Arc<ReleaseManifestWeld>,
    jetstream: Arc<WamnJetstream>,
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
        })
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
        match self.driver.execute(request).await {
            Ok(delivery) => {
                self.publish_emit(&delivery.outcome, causation).await?;
                lower_outcome(delivery.outcome)
            }
            Err(error) if error.downcast_ref::<PreloadedWiringMissing>().is_some() => {
                Err(DeliveryError::WiringNotPreloaded)
            }
            Err(error) => {
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
    use super::*;
    use wamn_router::{ErrorDetail, Failure};

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
}
