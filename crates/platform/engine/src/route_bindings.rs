//! The generated host side of the http-route guest's three host imports.
//!
//! One bindgen covers all three interfaces, so the caller resource that routing
//! returns is the same Rust type that delivery and the operation take back. The WIT packages
//! stay beside their guests' other copies, and this world only names them.

wash_runtime::wasmtime::component::bindgen!({
    path: [
        "../runtime/wit/deps/wamn-flow-http-routing",
        "../../execution/host/wit/deps/wamn-router-delivery-0.4",
        "../runtime/wit/deps/wamn-postgres-0.4",
        "../runtime/wit/deps/wamn-operation-0.1",
        "wit",
    ],
    world: "wamn:engine/route-plugins@0.1.0",
    imports: { default: async | trappable | tracing },
    with: {
        "wamn:flow-http-routing/routing.route-permit": crate::flow_http_routing::RoutePermit,
        "wamn:flow-http-routing/routing.authenticated-caller": crate::flow_http_routing::AuthenticatedCaller,
    },
    wasmtime_crate: wash_runtime::wasmtime,
});
