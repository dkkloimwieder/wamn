//! Host plugin for `wamn:router-delivery@0.4.0`, the one guest-to-host
//! delivery import that attachment and registration ingress share.
//!
//! The plugin takes the caller handle back from the resource table and hands
//! the request to the host's [`RouteDelivery`]. Every host serves a delivery
//! with the pieces that live here: source and target resolution, the caller
//! and operation grant checks, the causation that a delivery derives, the
//! settlement of a route's one export call, and the refusal literals.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use anyhow::Context as _;
use tracing::Instrument as _;
use wamn_catalog::{
    AdmittedComponent, AttachmentAuthPolicy, AttachmentTarget, HostAttachment,
    ServingComponentOperation, ServingManifest, parse_attachment_auth_policy,
};
use wamn_event_wire::Causation;
use wamn_project_state::PlatformComponent;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Accessor;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::engine::MAX_HOST_CALL_DURATION;
use crate::flow_http_routing::AuthenticatedCaller;
use crate::operation::node_types;
use crate::route_bindings::wamn::router_delivery::delivery;
/// The wire types of `wamn:router-delivery` that a host and the wiring layer
/// settle into.
pub use crate::route_bindings::wamn::router_delivery::delivery::{
    DeadlineAdjustment, DeliveryError, DeliveryFailure, DeliveryOutcome, DeliveryReport,
    DeliveryRequest, EffectOutcome, Emission, FailedOutcome, FailureType, ParentCausation,
    PartialCompletion, PermissionDenial, Source,
};

/// Host-plugin identity for the one guest-to-router bridge.
pub const ROUTER_DELIVERY_ID: &str = "wamn-router-delivery";

// The bounded driver refusals a live view can show. Shared with `DeliveryClass`
// rather than respelled, so a dashboard and a run screen never disagree about
// what happened to the same delivery — pinned by
// `a_refusal_reads_the_same_to_a_dashboard_and_to_a_live_view`.
pub const PERMISSION_DENIED: &str = "permission-denied";
pub const FRESH_CREDENTIAL_REQUIRED: &str = "fresh-credential-required";
pub const EXECUTION_FAILED: &str = "execution-failed";

/// The host's delivery: what one request does once the plugin has the caller
/// handle back.
#[async_trait::async_trait]
pub trait RouteDelivery: Send + Sync + fmt::Debug {
    /// Serve one delivery request and report its outcome.
    async fn deliver(
        &self,
        request: DeliveryRequest,
        caller: Option<AuthenticatedCaller>,
    ) -> DeliveryReport;
}

/// The one guest-to-host delivery plugin, over the host's [`RouteDelivery`].
#[derive(Debug)]
pub struct RouterDelivery {
    delivery: Arc<dyn RouteDelivery>,
}

impl RouterDelivery {
    /// Serve the delivery import with the host's delivery.
    pub fn new(delivery: Arc<dyn RouteDelivery>) -> Self {
        Self { delivery }
    }
}

#[async_trait::async_trait]
impl HostPlugin for RouterDelivery {
    fn id(&self) -> &'static str {
        ROUTER_DELIVERY_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:router-delivery/delivery@0.4.0")]),
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
        add_to_linker(item.linker())?;
        Ok(())
    }
}

/// Link the delivery import. A store without this plugin traps on each call.
pub fn add_to_linker(
    linker: &mut wash_runtime::wasmtime::component::Linker<SharedCtx>,
) -> wash_runtime::wasmtime::Result<()> {
    delivery::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<RouterDelivery>> {
    ctx.try_get_plugin::<RouterDelivery>(ROUTER_DELIVERY_ID)
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
        Ok(plugin
            .delivery
            .deliver(request, caller)
            .instrument(tracing::info_span!(target: "wamn::route", "wamn.route.deliver"))
            .await)
    }
}

/// The causation of one delivery. A registration delivery inherits its
/// parent's root and depth, and every delivery names itself as the run.
pub fn derived_causation(
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

/// The ingress source of one delivery.
#[derive(Debug, Clone, Copy)]
pub enum SourceRef<'a> {
    Attachment(&'a str),
    Registration(&'a str),
}

impl<'a> SourceRef<'a> {
    /// The bridge's two ingress kinds, as the label a metric attribute and a
    /// delivery preview both carry.
    pub fn source_type(self) -> &'static str {
        match self {
            SourceRef::Attachment(_) => "attachment",
            SourceRef::Registration(_) => "registration",
        }
    }

    pub fn id(self) -> &'a str {
        match self {
            SourceRef::Attachment(id) | SourceRef::Registration(id) => id,
        }
    }

    /// The platform component that executes a callerless delivery from this
    /// source. A registration delivery stays callerless and executes as
    /// `wamn:materializer`. An anonymous attachment has no executing principal.
    pub fn platform(self) -> Option<PlatformComponent> {
        match self {
            SourceRef::Attachment(_) => None,
            SourceRef::Registration(_) => Some(PlatformComponent::Materializer),
        }
    }
}

/// The manifest target of one delivery source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub package_id: String,
    /// A registration always names a wiring. An attachment names either.
    pub target: AttachmentTarget,
    pub caller_attached: bool,
    /// `Some` only for attachment ingress. A callerless attachment is legal
    /// only when its loaded auth policy permits no caller principal.
    anonymous_caller_permitted: Option<bool>,
    registered_operation: Option<String>,
}

fn resolve_target(manifest: &ServingManifest, source: SourceRef<'_>) -> Option<ResolvedTarget> {
    match source {
        SourceRef::Attachment(id) => {
            let attachment = manifest.attachment(id)?;
            Some(ResolvedTarget {
                package_id: attachment.package_id().to_owned(),
                target: attachment.target(),
                caller_attached: true,
                anonymous_caller_permitted: Some(
                    parse_attachment_auth_policy(attachment.auth_policy())
                        .is_some_and(AttachmentAuthPolicy::has_no_caller),
                ),
                registered_operation: attachment.registered_operation().map(str::to_owned),
            })
        }
        SourceRef::Registration(id) => {
            manifest
                .workflow
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

/// Resolve a delivery source to its target, and check its caller and the
/// registered operation grant.
pub fn resolve_authorized_target(
    manifest: &ServingManifest,
    source: SourceRef<'_>,
    caller: Option<&AuthenticatedCaller>,
) -> Result<ResolvedTarget, DeliveryError> {
    let target = resolve_target(manifest, source).ok_or(DeliveryError::SourceNotFound)?;
    validate_caller(source, &target, caller)?;
    // Attachments do not own freshness. The driver reads each released operation.
    authorize_registered_operation(caller, target.registered_operation.as_deref())
        .map_err(|denial| lower_operation_refusal(&denial))?;
    Ok(target)
}

/// Resolve an attachment source to a host route of the release, and check
/// its caller and the grant of an `admin`-only route. A host route is never
/// anonymous. `None` when the source is not a host route.
pub fn resolve_authorized_host_route(
    manifest: &ServingManifest,
    source: SourceRef<'_>,
    caller: Option<&AuthenticatedCaller>,
) -> Option<Result<&'static HostAttachment, DeliveryError>> {
    let SourceRef::Attachment(id) = source else {
        return None;
    };
    let attachment = manifest.host_attachment(id)?;
    if !caller_matches_source(
        source,
        Some(false),
        caller.map(AuthenticatedCaller::attachment_id),
    ) {
        return Some(Err(DeliveryError::InvalidRequest));
    }
    Some(
        authorize_registered_operation(caller, attachment.registered_operation())
            .map(|()| attachment)
            .map_err(|denial| lower_operation_refusal(&denial)),
    )
}

/// Exercise the exact production attachment resolver and authorization gate.
#[cfg(feature = "test-util")]
pub fn authorize_attachment_for_test(
    release: &crate::release_manifest::LoadedRelease,
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

/// The wire refusal for an operation the caller may not invoke.
pub fn lower_operation_refusal(denial: &OperationRefusal) -> DeliveryError {
    let detail = PermissionDenial {
        operation: denial.operation().to_owned(),
    };
    match denial.refusal_type() {
        OperationRefusalType::PermissionDenied => DeliveryError::PermissionDenied(detail),
    }
}

/// How the router driver answered one delivery. The variants are the arms of
/// the driver match in the host's delivery. The host classifies nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryClass {
    Delivered,
    PermissionDenied,
    ExecutionFailed,
}

impl DeliveryClass {
    /// The `wamn.delivery.error` value, or `None` for the one delivered class.
    pub fn error(self) -> Option<&'static str> {
        match self {
            DeliveryClass::Delivered => None,
            DeliveryClass::PermissionDenied => Some(PERMISSION_DENIED),
            DeliveryClass::ExecutionFailed => Some(EXECUTION_FAILED),
        }
    }
}

/// How one route call settled: what the caller receives, and the payload of a
/// respond, which a get's ETag reads. Every other outcome carries `Null`.
#[derive(Debug)]
pub struct RouteSettlement {
    pub outcome: DeliveryOutcome,
    pub result: serde_json::Value,
}

/// Lower the one export call of a route.
///
/// A route never retries, so a retryable or rate-limited error lowers to the
/// failure that a wiring reports after its last attempt, and the caller sees
/// one shape.
pub fn settle_route(
    outcome: Result<node_types::Emission, node_types::NodeError>,
) -> anyhow::Result<RouteSettlement> {
    let failed = |failure_type, detail: node_types::ErrorDetail| RouteSettlement {
        result: serde_json::Value::Null,
        outcome: DeliveryOutcome::Failed(DeliveryFailure {
            failure_type,
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
                result: payload,
            }
        }
        Err(node_types::NodeError::Retryable(detail)) => {
            failed(FailureType::RetryExhausted, detail)
        }
        Err(node_types::NodeError::RateLimited(limited)) => {
            failed(FailureType::RetryExhausted, limited.detail)
        }
        Err(node_types::NodeError::Terminal(detail)) => failed(FailureType::Terminal, detail),
        Err(node_types::NodeError::InvalidInput(detail)) => {
            failed(FailureType::InvalidInput, detail)
        }
        Err(node_types::NodeError::Cancelled) => RouteSettlement {
            outcome: DeliveryOutcome::Cancelled,
            result: serde_json::Value::Null,
        },
    })
}

/// Why an originating caller cannot invoke a registered operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationRefusalType {
    PermissionDenied,
}

/// Exact operation authority missing from the originating caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRefusal {
    type_: OperationRefusalType,
    operation: Box<str>,
}

impl OperationRefusal {
    pub fn new(type_: OperationRefusalType, operation: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            operation: operation.into(),
        }
    }

    pub fn refusal_type(&self) -> OperationRefusalType {
        self.type_
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }
}

impl fmt::Display for OperationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.type_ {
            OperationRefusalType::PermissionDenied => "permission denied",
        };
        write!(formatter, "{reason} for operation {}", self.operation)
    }
}

impl std::error::Error for OperationRefusal {}

/// Check the grant of a released operation before its entry runs: the caller
/// must hold the entry's own registered operation.
pub fn authorize_released_operation(
    caller: Option<&AuthenticatedCaller>,
    released: &ServingComponentOperation,
) -> Result<(), OperationRefusal> {
    authorize_registered_operation(caller, released.registered_operation.as_deref())
}

/// Check that the originating caller holds the registered operation grant.
pub fn authorize_registered_operation(
    caller: Option<&AuthenticatedCaller>,
    operation: Option<&str>,
) -> Result<(), OperationRefusal> {
    let Some(operation) = operation else {
        return Ok(());
    };
    caller
        .filter(|caller| caller.permits(operation))
        .map(|_| ())
        .ok_or_else(|| OperationRefusal::new(OperationRefusalType::PermissionDenied, operation))
}

/// The host call ceiling in milliseconds. The constant is well under an hour,
/// so it fits every width this bound converts to.
fn max_host_call_ms() -> u64 {
    u64::try_from(MAX_HOST_CALL_DURATION.as_millis()).unwrap_or(u64::MAX)
}

/// A node deadline bounded to the host call ceiling.
pub fn bounded_node_deadline_ms(deadline_ms: Option<u64>) -> u64 {
    deadline_ms
        .unwrap_or(max_host_call_ms())
        .clamp(1, max_host_call_ms())
}

/// The one admitted component of the package that exports the route operation.
pub fn route_component<'a>(
    components: &'a [AdmittedComponent],
    package_id: &str,
    component: &str,
    operation: &str,
) -> anyhow::Result<&'a AdmittedComponent> {
    let mut providers = components.iter().filter(|fact| {
        fact.scope.package_id == package_id
            && fact.component == component
            && fact.operations.contains_key(operation)
    });
    let provider = providers.next().context("route-component-missing")?;
    anyhow::ensure!(providers.next().is_none(), "route-component-ambiguous");
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_http_routing::CredentialType;

    const MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"},{"descriptor":{"component":"transform","component-digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"manifest_mint"}],"format-version":11,"release":{"packages":[{"package-id":"manifest_mint","package-version":"1.0.0"}]},"routes":[],"workflow":{"attachments":{"orders-http":{"auth-policy":{"modes":["none"]},"definition":{"id":"orders-http","run-deadline-ms":30000,"type":"http"},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","package-id":"manifest_mint","type":"http","wiring-id":"orders","wiring-version":1}},"registrations":{"manifest_mint::orders-changed":{"entity":"orders","ops":["insert","update"],"package-id":"manifest_mint","source-package-id":"manifest_mint","wiring-id":"shipping","wiring-version":2}},"wirings":[{"graph":{"entry":"in","format-version":"0.1","nodes":{"in":{"component":"transform","interface-version":"0.1","operation":"map"}},"version":1,"wiring-id":"orders"},"package-id":"manifest_mint"},{"graph":{"entry":"in","format-version":"0.1","nodes":{"in":{"component":"transform","interface-version":"0.1","operation":"map"}},"version":2,"wiring-id":"shipping"},"package-id":"manifest_mint"}]}}"#;

    fn manifest() -> ServingManifest {
        ServingManifest::from_canonical_bytes(MANIFEST)
            .expect("format-7 fixture is canonical")
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
    fn a_host_route_resolves_only_for_its_own_authenticated_caller() {
        let mut manifest = manifest();
        assert!(
            resolve_authorized_host_route(
                &manifest,
                SourceRef::Attachment("wamn-control-permission-mine-http"),
                None,
            )
            .is_none(),
            "a release without the set serves none of its routes"
        );
        manifest
            .host_routes
            .insert(wamn_catalog::HostRouteSet::Application);
        let id = "wamn-control-permission-mine-http";
        let caller = |attachment: &str| {
            AuthenticatedCaller::new(
                attachment,
                "principal",
                CredentialType::Session,
                false,
                HashSet::new(),
                None,
            )
        };
        assert!(matches!(
            resolve_authorized_host_route(&manifest, SourceRef::Attachment(id), None),
            Some(Err(DeliveryError::InvalidRequest))
        ));
        assert!(matches!(
            resolve_authorized_host_route(
                &manifest,
                SourceRef::Attachment(id),
                Some(&caller("orders-http")),
            ),
            Some(Err(DeliveryError::InvalidRequest))
        ));
        let attachment =
            resolve_authorized_host_route(&manifest, SourceRef::Attachment(id), Some(&caller(id)))
                .expect("a host route")
                .expect("a member route admits every authenticated caller");
        assert_eq!(attachment.reference, "wamn-control:permission/mine");
        assert!(
            resolve_authorized_host_route(&manifest, SourceRef::Attachment("orders-http"), None)
                .is_none(),
            "an authored attachment is not a host route"
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
        let mut signed_manifest = manifest();
        signed_manifest
            .workflow
            .attachments
            .get_mut("orders-http")
            .unwrap()
            .auth_policy = serde_json::json!({"modes":["signature"],"header":"X-Signature","credential":"webhook"});
        let signed = resolve_target(&signed_manifest, SourceRef::Attachment("orders-http"))
            .expect("the signed attachment resolves");
        assert!(caller_matches_source(
            SourceRef::Attachment("orders-http"),
            signed.anonymous_caller_permitted,
            None,
        ));

        let mut protected_manifest = manifest();
        protected_manifest
            .workflow
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
            .workflow
            .attachments
            .get_mut("orders-http")
            .expect("the fixture attachment exists")
            .registered_operation = Some(operation.to_owned());
        let target = resolve_target(&registered, SourceRef::Attachment("orders-http"))
            .expect("the registered attachment resolves from the loaded release");
        let denial = authorize_registered_operation(None, target.registered_operation.as_deref())
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
            OperationRefusalType::PermissionDenied,
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
    fn node_deadline_is_nonzero_and_host_bounded() {
        let ceiling = max_host_call_ms();

        assert_eq!(bounded_node_deadline_ms(None), ceiling);
        assert_eq!(bounded_node_deadline_ms(Some(0)), 1);
        assert_eq!(bounded_node_deadline_ms(Some(ceiling + 1)), ceiling);
        assert_eq!(bounded_node_deadline_ms(Some(17)), 17);
    }

    #[test]
    fn every_registered_invocation_requires_the_exact_operation_grant() {
        let operation = "orders:widget/get@7.0.0";

        assert!(authorize_registered_operation(None, None).is_ok());
        let denial = authorize_registered_operation(None, Some(operation))
            .expect_err("a registered invocation without an originating caller is denied");
        assert_eq!(denial.operation(), operation);
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
}
