//! Shared invocation identity and connection authorization rules.

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: ["../../wit/wamn-connection", "../../wit/wamn-ingress", "../../wit/wamn-postgres-0.4", "../../wit/wamn-node", "../../wit/wamn-operation-0.1", "../../wit/wasmcloud-blobstore", "wit"],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}
pub use bindings::wamn::connection::http::ConnectionError;
