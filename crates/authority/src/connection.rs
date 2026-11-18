//! Shared invocation identity and connection authorization rules.

#[cfg(any(test, feature = "test-util"))]
use std::sync::Arc;
#[cfg(any(test, feature = "test-util"))]
use wamn_catalog::ServingManifest;
#[cfg(any(test, feature = "test-util"))]
use wamn_catalog::serving_context::ServingContext;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: ["../../wit/wamn-connection", "../../wit/wamn-ingress", "../../wit/wamn-postgres-0.4", "../../wit/wamn-node", "../../wit/wamn-operation-0.1", "../../wit/wasmcloud-blobstore", "wit"],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}
pub use bindings::wamn::connection::http::ConnectionError;
/// A package release for host service fixtures that execute no released component.
#[cfg(any(test, feature = "test-util"))]
pub fn test_release(project_env_key: &wamn_registry::ProjectEnvKey) -> Arc<ServingContext> {
    let manifest = ServingManifest::new(
        wamn_catalog::ServingRelease {
            packages: [wamn_catalog::PackageCoordinate::new(
                "host_service_fixture".parse().unwrap(),
                "1.0.0".parse().unwrap(),
            )
            .expect("fixture package coordinate")]
            .into(),
        },
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .expect("host service fixture manifest");
    Arc::new(
        ServingContext::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "host service fixture",
            wamn_catalog::serving_context::ReleaseTarget::new(
                project_env_key.clone(),
                "dev".parse().expect("valid fixture coordinate"),
            ),
        )
        .expect("host service fixture release"),
    )
}
