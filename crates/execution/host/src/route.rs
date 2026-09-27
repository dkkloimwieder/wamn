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
    InvocationSite, NativeFacts, NodeAcquisition, OperationHost, component_invocation,
    invocation_span, node_trace_context, remote_trace_context,
};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::operation::native_call::RowBatches;
use wamn_engine::operation::{
    OperationCall, OperationClosure, invoke_operation, invoke_operation_stream, node_types,
};
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
pub(crate) async fn invoke_route(
    host: &OperationHost,
    route: RouteCall<'_>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    match call_route(host, route, None).await? {
        RouteReturn::Whole(returned) => Ok(returned),
        RouteReturn::Streamed(_) => unreachable!("a whole call returns a whole outcome"),
    }
}

/// Call the query of one route once, and hand its rows to `rows` as the
/// component writes them. The result is the outcome JSON after the last row.
pub(crate) async fn invoke_route_stream(
    host: &OperationHost,
    route: RouteCall<'_>,
    rows: RowBatches,
) -> anyhow::Result<Result<String, node_types::NodeError>> {
    match call_route(host, route, Some(rows)).await? {
        RouteReturn::Streamed(outcome) => Ok(outcome),
        RouteReturn::Whole(_) => unreachable!("a streamed call returns its outcome JSON"),
    }
}

enum RouteReturn {
    Whole(Result<node_types::Emission, node_types::NodeError>),
    Streamed(Result<String, node_types::NodeError>),
}

/// Call one route's export, streamed when `rows` is given.
///
/// A route has no wiring position, so its node context names the empty
/// wiring, version 0 and the empty node, and its config is JSON `null`.
async fn call_route(
    host: &OperationHost,
    route: RouteCall<'_>,
    rows: Option<RowBatches>,
) -> anyhow::Result<RouteReturn> {
    let components = host.release_components().await?;
    let component = authorized_component(&components, &route)?;
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
        let call = OperationCall {
            closure: OperationClosure::Released(&components),
            component,
            operation: route.operation,
            context,
            input: route.payload,
            deadline_ms,
            facts: NativeFacts::entry(acquisition, route.caller),
        };
        Ok(match rows {
            Some(rows) => RouteReturn::Streamed(invoke_operation_stream(host, call, rows).await?),
            None => RouteReturn::Whole(invoke_operation(host, call, None).await?),
        })
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
    authorize_registered_operation(
        route.caller.as_ref(),
        fact.registered_operation.as_deref(),
        fact.fresh_only,
    )?;
    Ok(component)
}
