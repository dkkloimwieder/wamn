//! Shared invocation identity and connection authorization rules.

use crate::effect_span::{EffectEvidence, EffectWiring};
use crate::identity::{
    CandidateBindingWorld, ConnectionEffectSnapshot, ConnectionEntryLookup, WiringLookup,
};
use std::sync::Arc;
use wamn_catalog::{ArtifactHash, ConnectionTypeDescriptor, ServingComponent, ServingManifest};
#[cfg(any(test, feature = "test-util"))]
use wamn_engine::release_manifest::LoadedRelease;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: ["../../../wit/wamn-connection", "../../../wit/wamn-flow-http-routing", "../../../wit/wamn-postgres-0.4", "../../../wit/wamn-node", "../../../wit/wamn-operation-0.1", "../../../wit/wasmcloud-blobstore", "wit"],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}
pub use bindings::wamn::connection::http::ConnectionError;
/// Host-attested identity of one component invocation.
///
/// `component` is the name the catalog admitted the executing component under,
/// and `operation` is the node operation the router driver called on it. Both
/// come from the driver, never from the guest. They are here because
/// `component_digest` is a manifest key and no reader looks a component up by
/// it, and because the capability method an effect span records says which host
/// function ran, not which node operation raised it (`wamn-b2m6.7`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInvocation {
    /// The root component, unchanged by nested calls.
    pub origin: ConnectionOrigin,
    /// Where the invocation entered the release, unchanged by nested calls.
    pub entry: InvocationEntry,
    pub package_id: String,
    pub component_digest: String,
    pub component: String,
    pub operation: String,
    pub closure: ConnectionExecutionClosure,
    /// Optional bounded failure evidence owned by this invocation.
    pub effects: Option<EffectEvidence>,
}

impl ConnectionInvocation {
    /// Resolve an external connection only from this operation's declarations.
    pub fn connection<'a>(
        &self,
        manifest: &'a ServingManifest,
        alias: &str,
    ) -> Option<&'a wamn_catalog::OperationConnection> {
        self.declaration(manifest)?
            .connections
            .iter()
            .find(|connection| {
                let configuration = self
                    .entry
                    .wiring()
                    .map(|position| position.configuration.as_ref());
                match connection {
                    wamn_catalog::OperationConnection::Import(name) => name == alias,
                    wamn_catalog::OperationConnection::Host(connection) => {
                        connection.store_alias.name() == Some(alias)
                            || configuration.and_then(|configuration| {
                                connection.store_alias.resolve(configuration)
                            }) == Some(alias)
                    }
                }
            })
    }

    pub fn declaration<'a>(
        &self,
        manifest: &'a ServingManifest,
    ) -> Option<&'a wamn_catalog::AdmittedComponentOperation> {
        manifest
            .components
            .iter()
            .find(|component| {
                component.package_id == self.package_id
                    && component.descriptor.component_digest == self.component_digest
                    && component.descriptor.component == self.component
            })?
            .descriptor
            .operations
            .get(&self.operation)
    }

    /// The span coordinates of this invocation. A route has no wiring
    /// position, so it records the wiring keys empty.
    pub fn effect_wiring(&self) -> EffectWiring<'_> {
        let (wiring_id, wiring_version, node_id, occurrence) = match self.entry.wiring() {
            None => ("", 0, "", 0),
            Some(position) => (
                position.wiring_id.as_str(),
                position.wiring_version,
                position.node_id.as_str(),
                position.occurrence,
            ),
        };
        EffectWiring {
            package_id: &self.package_id,
            wiring_id,
            wiring_version,
            node_id,
            occurrence,
            component_digest: &self.component_digest,
            component_name: &self.component,
            operation: &self.operation,
        }
    }
}

/// Where one invocation entered the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationEntry {
    /// A route called the origin export once. The origin names the route.
    Route,
    /// One node occurrence of an exact wiring version.
    Wiring(WiringPosition),
}

impl InvocationEntry {
    /// The wiring position, or `None` for a route.
    pub fn wiring(&self) -> Option<&WiringPosition> {
        match self {
            Self::Route => None,
            Self::Wiring(position) => Some(position),
        }
    }
}

/// One node occurrence of an exact wiring version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringPosition {
    /// The immutable graph hash frozen on the run.
    pub wiring_hash: String,
    /// The wiring can belong to a package that imports the root component.
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub node_id: String,
    pub occurrence: u32,
    pub configuration: Arc<std::collections::BTreeMap<String, serde_json::Value>>,
}

/// Host-attested root identity for an effect's exact dependency path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionOrigin {
    pub package_id: String,
    pub component_digest: String,
    pub component: String,
    pub interface_version: String,
    pub operation: String,
}

/// Host-owned authority closure for one component invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionExecutionClosure {
    /// A digest-verified serving manifest owns the component and wiring.
    Released,
    /// Private admission froze an exact candidate wiring and binding world.
    Candidate {
        /// The release the candidate resolves under, as its manifest digest.
        manifest_digest: String,
        environment: String,
        wiring_hash: String,
        component: String,
        interface_version: String,
        binding_world: Arc<CandidateBindingWorld>,
    },
}

/// A package release for host capability fixtures that execute no released component.
#[cfg(any(test, feature = "test-util"))]
pub fn test_release(project_env_key: &str) -> Arc<LoadedRelease> {
    let manifest = ServingManifest::new(
        wamn_catalog::ServingRelease {
            packages: [
                wamn_catalog::PackageCoordinate::new("capability_fixture", "1.0.0")
                    .expect("fixture package coordinate"),
            ]
            .into(),
        },
        Default::default(),
        Default::default(),
        Default::default(),
    )
    .expect("capability fixture manifest");
    Arc::new(
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "capability fixture",
            wamn_engine::release_manifest::ReleaseTarget::new(project_env_key, "dev"),
        )
        .expect("capability fixture release"),
    )
}

/// Find the application authorized to execute this exact released component.
pub fn released_invocation_application<'a>(
    manifest: &ServingManifest,
    target: &'a wamn_engine::release_manifest::ReleaseTarget,
    invocation: &ConnectionInvocation,
) -> Option<&'a str> {
    if !manifest.components.iter().any(|component| {
        component.package_id == invocation.package_id
            && component.descriptor.component == invocation.component
            && component.descriptor.component_digest == invocation.component_digest
            && component
                .descriptor
                .operations
                .contains_key(&invocation.operation)
    }) {
        return None;
    }
    if invocation.package_id == wamn_catalog::PALETTE_PACKAGE_ID {
        let InvocationEntry::Wiring(position) = &invocation.entry else {
            return None;
        };
        target
            .applications
            .get_key_value(&position.package_id)
            .map(|(name, _)| name.as_str())
    } else {
        if !manifest
            .release
            .packages
            .iter()
            .any(|package| package.package_id() == invocation.package_id)
        {
            return None;
        }
        let application =
            wamn_catalog::application_for_package(&target.applications, &invocation.package_id)?;
        if invocation
            .entry
            .wiring()
            .is_some_and(|position| position.package_id != application)
        {
            return None;
        }
        Some(application)
    }
}

/// Require the executing operation to be reachable from the attested wiring root.
///
/// Shared with the blobstore capability rather than reimplemented there: two
/// spellings of "does this component belong to this release" could disagree,
/// and the one that was wrong would authorize an effect.
/// The snapshot entry for one bound invocation: its route, or its wiring node.
///
/// Shared with the blobstore capability for the reason
/// [`authorize_release_closure`] gives.
pub fn entry_lookup(
    invocation: &ConnectionInvocation,
) -> Result<ConnectionEntryLookup<'_>, ConnectionError> {
    Ok(match &invocation.entry {
        InvocationEntry::Route => ConnectionEntryLookup {
            package_id: &invocation.origin.package_id,
            wiring: None,
        },
        InvocationEntry::Wiring(position) => ConnectionEntryLookup {
            package_id: &position.package_id,
            wiring: Some(WiringLookup {
                wiring_id: &position.wiring_id,
                wiring_version: i32::try_from(position.wiring_version)
                    .map_err(|_| ConnectionError::AttestationInvalid)?,
                node_id: &position.node_id,
            }),
        },
    })
}

pub fn authorize_release_closure(
    manifest: &ServingManifest,
    invocation: &ConnectionInvocation,
    snapshot: &ConnectionEffectSnapshot,
) -> Result<(), ConnectionError> {
    let digest = ArtifactHash::parse(invocation.component_digest.clone())
        .map_err(|_| ConnectionError::AttestationInvalid)?;
    let Some(((component, interface_version), operation)) = snapshot
        .component
        .as_ref()
        .zip(snapshot.interface_version.as_ref())
        .zip(snapshot.operation.as_ref())
    else {
        return Err(ConnectionError::AttestationInvalid);
    };
    if component != &invocation.component || operation != &invocation.operation {
        return Err(ConnectionError::AttestationInvalid);
    }
    let component = manifest.components.iter().find(|candidate| {
        candidate.package_id == invocation.package_id
            && candidate.descriptor.component == *component
            && candidate.descriptor.interface_version == *interface_version
            && candidate.descriptor.component_digest == digest.as_str()
    });
    let operation_admitted = component
        .and_then(|component| component.descriptor.operations.get(operation))
        .is_some_and(|operation| operation.registered_operation == snapshot.registered_operation);
    let origin = &invocation.origin;
    let entry_released = match &invocation.entry {
        InvocationEntry::Route => {
            snapshot.wiring_hash.is_none()
                && manifest.routes.iter().any(|route| {
                    route.package_id == origin.package_id
                        && route.component == origin.component
                        && route.operation == origin.operation
                })
        }
        InvocationEntry::Wiring(position) => {
            snapshot.node_permitted
                && snapshot.wiring_hash.as_deref() == Some(position.wiring_hash.as_str())
        }
    };
    let root = manifest.components.iter().find(|component| {
        component.package_id == origin.package_id
            && component.descriptor.component == origin.component
            && component.descriptor.interface_version == origin.interface_version
            && component.descriptor.component_digest.as_str() == origin.component_digest
    });
    if !operation_admitted
        || !entry_released
        || !root
            .is_some_and(|root| released_operation_reachable(root, &origin.operation, invocation))
    {
        return Err(ConnectionError::AttestationInvalid);
    }
    Ok(())
}

/// The invocation must be the released entry itself. An application's
/// components compose at build into one component, so a call inside it keeps
/// the entry's invocation identity.
fn released_operation_reachable(
    root: &ServingComponent,
    root_operation: &str,
    invocation: &ConnectionInvocation,
) -> bool {
    root.descriptor.operations.contains_key(root_operation)
        && root.package_id == invocation.package_id
        && root.descriptor.component == invocation.component
        && root.descriptor.component_digest.as_str() == invocation.component_digest
        && root_operation == invocation.operation
}

/// Require the snapshot to carry the wiring hash, component and interface
/// version frozen at candidate admission, and to equal the frozen binding row.
///
/// Shared with the blobstore capability for the reason
/// [`authorize_release_closure`] gives. Two spellings of the candidate rule can
/// disagree, and the wrong one authorizes an effect.
pub fn authorize_candidate_closure(
    invocation: &ConnectionInvocation,
    snapshot: &ConnectionEffectSnapshot,
    binding: &crate::identity::CandidateConnectionBinding,
) -> Result<(), ConnectionError> {
    let ConnectionExecutionClosure::Candidate {
        wiring_hash,
        component,
        interface_version,
        ..
    } = &invocation.closure
    else {
        return Err(ConnectionError::AttestationInvalid);
    };
    if invocation.package_id != invocation.origin.package_id
        || invocation.component_digest != invocation.origin.component_digest
        || invocation.component != invocation.origin.component
        || invocation.operation != invocation.origin.operation
        || snapshot.operation.as_deref() != Some(invocation.operation.as_str())
        || snapshot.wiring_hash.as_ref() != Some(wiring_hash)
        || snapshot.component.as_deref() != Some(component.as_str())
        || snapshot.interface_version.as_deref() != Some(interface_version.as_str())
        || !binding.matches_snapshot(snapshot)
    {
        return Err(ConnectionError::AttestationInvalid);
    }
    Ok(())
}

/// Require an active binding, the admitted generation and the selected capability's contract.
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
