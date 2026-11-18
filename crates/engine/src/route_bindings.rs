//! The generated host side of the ingress guest's host imports.
//!
//! One bindgen shares the caller resource between authentication and the
//! operation boundary. The WIT packages live under the repository-root `wit/`.

wash_runtime::wasmtime::component::bindgen!({
    path: [
        "../../wit/wamn-ingress",
        "../../wit/wamn-postgres-0.4",
        "../../wit/wamn-node",
        "../../wit/wamn-operation-0.1",
        "wit",
    ],
    world: "wamn:engine/route-plugins@0.1.0",
    imports: { default: async | trappable | tracing },
    with: {
        "wamn:node/types": crate::operation::node_types,
        "wamn:ingress/routing.authenticated-caller": crate::ingress::AuthenticatedCaller,
    },
    wasmtime_crate: wash_runtime::wasmtime,
});
