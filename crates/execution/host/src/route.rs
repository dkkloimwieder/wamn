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
use wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller;

use crate::operation::{
    InvocationSite, NativeFacts, NodeAcquisition, OperationCall, OperationClosure, OperationHost,
    authorize_registered_operation, bounded_node_deadline_ms, component_invocation,
    invocation_span, invoke_operation, node_trace_context, node_types, remote_trace_context,
};

/// One request to one route.
pub(crate) struct RouteCall<'a> {
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
    let component = route_component(
        &components,
        route.package_id,
        route.component,
        route.operation,
    )?;
    let fact = component
        .operation(route.operation)
        .context("route-operation-fact-missing")?;
    authorize_registered_operation(
        route.caller.as_ref(),
        fact.registered_operation.as_deref(),
        fact.fresh_only,
    )?;
    let release = &host.release.manifest().release;
    let span = invocation_span(
        &InvocationSite {
            tenant_id: &release.tenant_id,
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
            claims: host.claims(&release.tenant_id, Some(host.release_identity())),
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
        invoke_operation(
            host,
            OperationCall {
                closure: OperationClosure::Released(&components),
                component,
                operation: route.operation,
                context,
                input: route.payload,
                deadline_ms,
                facts: NativeFacts::entry(acquisition, route.caller),
            },
            None,
        )
        .await
    }
    .instrument(span)
    .await
}

/// The one admitted component of the package that exports the route operation.
fn route_component<'a>(
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
