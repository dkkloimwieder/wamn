//! Shared invocation identity and connection authorization rules.

use crate::identity::ConnectionEffectSnapshot;
#[cfg(any(test, feature = "test-util"))]
use std::sync::Arc;
use wamn_catalog::ConnectionTypeDescriptor;
#[cfg(any(test, feature = "test-util"))]
use wamn_catalog::ServingManifest;
#[cfg(any(test, feature = "test-util"))]
use wamn_engine::release_manifest::LoadedRelease;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: ["../../wit/wamn-connection", "../../wit/wamn-flow-http-routing", "../../wit/wamn-postgres-0.4", "../../wit/wamn-node", "../../wit/wamn-operation-0.1", "../../wit/wasmcloud-blobstore", "wit"],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}
pub use bindings::wamn::connection::http::ConnectionError;
/// A package release for host service fixtures that execute no released component.
#[cfg(any(test, feature = "test-util"))]
pub fn test_release(project_env_key: &wamn_registry::ProjectEnvKey) -> Arc<LoadedRelease> {
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
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "host service fixture",
            wamn_engine::release_manifest::ReleaseTarget::new(
                project_env_key.clone(),
                "dev".parse().expect("valid fixture coordinate"),
            ),
        )
        .expect("host service fixture release"),
    )
}

/// Require an active binding, the admitted generation and the selected service's contract.
pub fn authorize_snapshot(
    snapshot: &ConnectionEffectSnapshot,
    descriptor: &ConnectionTypeDescriptor,
) -> Result<(), ConnectionError> {
    if !snapshot.node_permitted {
        return Err(ConnectionError::AttestationInvalid);
    }
    let Some(requirement) = snapshot.requirement_json.as_ref() else {
        return Err(ConnectionError::Incompatible);
    };
    if !snapshot.binding_active || !snapshot.binding_valid || snapshot.instance_id.is_none() {
        return Err(ConnectionError::Unbound);
    }
    if !snapshot.instance_enabled
        || snapshot.active_generation.is_none()
        || snapshot.pinned_generation.or(snapshot.active_generation) != snapshot.generation
    {
        return Err(ConnectionError::CredentialUnavailable);
    }
    // `requirement_json` is the WHOLE portable record component admission
    // minted — `{component-digest, store-alias, requirement}` — because that is
    // the value `requirement_hash` is the SHA-256 of. The connection SEMANTICS
    // therefore sit one level down, under `requirement`.
    let expected_type = descriptor.requirement_type.as_str();
    let expected_contract = descriptor.contract.as_str();
    if snapshot.requirement_type.as_deref() != Some(expected_type)
        || snapshot.contract.as_deref() != Some(expected_contract)
        || requirement
            .pointer("/requirement/requirement-type")
            .and_then(serde_json::Value::as_str)
            != Some(expected_type)
        || requirement
            .pointer("/requirement/contract")
            .and_then(serde_json::Value::as_str)
            != Some(expected_contract)
    {
        return Err(ConnectionError::Incompatible);
    }
    Ok(())
}
