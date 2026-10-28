//! A request to a route: one export call of one component, with no wiring.
//!
//! A route never enters the router driver. It resolves its component from the
//! carried release, applies the same operation grant as a wiring node, and
//! calls [`invoke_operation`] once. A route never retries: the caller decides
//! whether to send a new request.

use anyhow::Context as _;
use tracing::Instrument as _;
use wamn_catalog::AdmittedComponent;
use wamn_event_wire::Causation;
use wamn_runtime::plugins::connection_http::{ConnectionExecutionClosure, InvocationEntry};

use crate::operation::{
    CallIntents, InvocationSite, NativeFacts, NodeAcquisition, OperationHost, component_invocation,
    invocation_span, node_trace_context, remote_trace_context,
};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wamn_engine::router_delivery::{
    authorize_registered_operation, bounded_node_deadline_ms, route_component,
};

/// One request to one route.
pub(crate) struct RouteCall<'a> {
    /// The attachment that entered the route; its invocation span names it.
    pub(crate) attachment_id: &'a str,
    pub(crate) package_id: &'a str,
    pub(crate) component: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) delivery_id: &'a str,
    pub(crate) payload: &'a serde_json::Value,
    pub(crate) caller: Option<AuthenticatedCaller>,
    pub(crate) traceparent: Option<&'a str>,
    pub(crate) tracestate: Option<&'a str>,
    pub(crate) causation: Causation,
}

/// Call the export of one route once and return what the component returned.
///
/// A route has no wiring position, so its node context names the empty
/// wiring, version 0 and the empty node, and its config is JSON `null`.
pub(crate) async fn invoke_route(
    host: &OperationHost,
    route: RouteCall<'_>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    let components = host.release_components().await?;
    let component = authorized_component(&components, &route)?;
    let release = host.release.scope();
    let span = invocation_span(
        &InvocationSite {
            env_key: &release.env_key,
            project: &host.project,
            environment: &release.environment,
            wiring_id: "",
            wiring_version: 0,
            node_id: "",
            input_port: None,
            operation: route.operation,
            component_digest: &component.component_digest,
            caller: route.caller.as_ref(),
        },
        remote_trace_context(route.traceparent, route.tracestate).as_ref(),
    );
    span.record("wamn.attachment_id", route.attachment_id);
    let deadline_ms = bounded_node_deadline_ms(None);
    async {
        // Read inside the span: the guest parents to `wamn.component.invoke`.
        let trace = node_trace_context(route.traceparent, route.tracestate);
        let context = node_types::NodeContext {
            wiring_id: String::new(),
            wiring_version: 0,
            node_id: String::new(),
            delivery_id: route.delivery_id.to_owned(),
            input_port: None,
            occurrence: 0,
            traceparent: trace.traceparent,
            tracestate: trace.tracestate,
            deadline_ms: Some(deadline_ms),
            config: "null".to_owned(),
        };
        let acquisition = NodeAcquisition {
            claims: host.claims(&release.env_key, Some(host.release_identity())),
            invocation: component_invocation(
                component,
                route.operation,
                InvocationEntry::Route,
                ConnectionExecutionClosure::Released,
                None,
            ),
            causation: Some(route.causation),
            // Only an attachment enters a route, and an attachment executes
            // as its caller.
            platform: None,
        };
        let call = OperationCall {
            closure: OperationClosure::Released(&components),
            component,
            operation: route.operation,
            context,
            input: route.payload,
            deadline_ms,
            facts: NativeFacts::entry(acquisition, route.caller),
        };
        let intents = host.call_intents(component, route.operation)?;
        let intent = intents.as_ref().map(CallIntents::context);
        invoke_operation(host, call, intent).await
    }
    .instrument(span)
    .await
}

/// Apply the operation grant of one route without calling it.
///
/// A conditional read calls it before it answers not-modified, so a 304 never
/// answers a caller that the read itself would refuse.
pub(crate) async fn authorize_route(
    host: &OperationHost,
    route: &RouteCall<'_>,
) -> anyhow::Result<()> {
    let components = host.release_components().await?;
    authorized_component(&components, route).map(|_| ())
}

/// The component of one route, after the caller passed its operation grant.
fn authorized_component<'a>(
    components: &'a [AdmittedComponent],
    route: &RouteCall<'_>,
) -> anyhow::Result<&'a AdmittedComponent> {
    let component = route_component(
        components,
        route.package_id,
        route.component,
        route.operation,
    )?;
    let fact = component
        .operation(route.operation)
        .context("route-operation-fact-missing")?;
    authorize_registered_operation(route.caller.as_ref(), fact.registered_operation.as_deref())?;
    Ok(component)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use wamn_catalog::{AdmittedComponentOperation, ComponentPackageScope};
    use wamn_engine::flow_http_routing::CredentialType;
    use wamn_engine::router_delivery::{OperationRefusal, OperationRefusalType};

    use super::*;

    const OPERATION: &str = "fixture:widget/create@1.0.0";

    fn component() -> AdmittedComponent {
        AdmittedComponent {
            scope: ComponentPackageScope {
                env_key: "tenant-a".into(),
                package_id: "fixture".into(),
                package_version: "1.0.0".into(),
            },
            component: "widget".into(),
            interface_version: "1.0.0".into(),
            operations: BTreeMap::from([(
                OPERATION.into(),
                AdmittedComponentOperation {
                    registered_operation: Some(OPERATION.into()),
                    committed_result_schema: None,
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: String::new(),
            effects: Vec::new(),
        }
    }

    fn route(payload: &serde_json::Value) -> RouteCall<'_> {
        RouteCall {
            attachment_id: "widget-http",
            package_id: "fixture",
            component: "widget",
            operation: OPERATION,
            delivery_id: "delivery-1",
            payload,
            caller: Some(AuthenticatedCaller::new(
                "widget-http",
                "user-a",
                CredentialType::Pat,
                false,
                HashSet::from(["fixture:widget/create".into()]),
                None,
            )),
            traceparent: None,
            tracestate: None,
            causation: Causation {
                run: "attachment:delivery-1".into(),
                root: "attachment:delivery-1".into(),
                depth: 0,
            },
        }
    }

    #[test]
    fn a_route_without_a_caller_refuses_its_registered_operation() {
        let components = [component()];
        let payload = serde_json::Value::Null;
        let mut route = route(&payload);
        assert!(authorized_component(&components, &route).is_ok());
        route.caller = None;
        let error = authorized_component(&components, &route).unwrap_err();
        let refusal = error.downcast_ref::<OperationRefusal>().unwrap();
        assert_eq!(
            refusal.refusal_type(),
            OperationRefusalType::PermissionDenied
        );
        assert_eq!(refusal.operation(), OPERATION);
    }

    #[test]
    fn a_route_whose_component_lacks_the_operation_is_refused() {
        let mut components = [component()];
        let payload = serde_json::Value::Null;
        let route = route(&payload);
        assert!(authorized_component(&components, &route).is_ok());
        components[0].operations.clear();
        let error = authorized_component(&components, &route).unwrap_err();
        // The later route-operation-fact-missing arm is unreachable.
        assert_eq!(error.to_string(), "route-component-missing");
    }

    #[test]
    fn a_route_cannot_select_another_packages_component() {
        let components = [component()];
        let payload = serde_json::Value::Null;
        let mut route = route(&payload);
        assert!(authorized_component(&components, &route).is_ok());
        route.package_id = "other";
        let error = authorized_component(&components, &route).unwrap_err();
        assert_eq!(error.to_string(), "route-component-missing");
    }
}
