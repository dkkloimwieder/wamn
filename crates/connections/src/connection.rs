//! Host-attested connection invocations and release authorization.

use std::sync::Arc;
use wamn_authority::connection::ConnectionError;
use wamn_authority::effect_span::{EffectEvidence, EffectSource, EffectWiring};
use wamn_authority::identity::{
    CandidateBindingWorld, ConnectionEffectSnapshot, ConnectionEntryLookup, WiringLookup,
};
use wamn_catalog::{ServingComponent, ServingManifest};

/// Host-attested identity of one component invocation.
///
/// `component` is the name the catalog admitted the executing component under,
/// and `operation` is the node operation the router driver called on it. Both
/// come from the driver, never from the guest. They are here because
/// `component_digest` is a manifest key and no reader looks a component up by
/// it, and because the host service method an effect span records says which host
/// function ran, not which node operation raised it (`wamn-b2m6.7`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInvocation {
    /// The root component, unchanged by nested calls.
    pub origin: ConnectionOrigin,
    /// Where the invocation entered the release, unchanged by nested calls.
    pub entry: InvocationEntry,
    pub package_id: wamn_catalog::PackageName,
    pub component_digest: wamn_digest::Digest,
    pub component: wamn_catalog::canonical_name::CanonicalName,
    pub operation: wamn_catalog::canonical_name::CanonicalName,
    pub closure: ConnectionExecutionClosure,
    /// Optional bounded failure evidence owned by this invocation.
    pub effects: Option<EffectEvidence>,
}

impl ConnectionInvocation {
    /// Resolve a host connection declared by the executing component.
    pub fn connection<'a>(
        &self,
        manifest: &'a ServingManifest,
        alias: &wamn_catalog::canonical_name::CanonicalName,
    ) -> Option<&'a wamn_catalog::ComponentConnection> {
        let component = manifest.components.iter().find(|component| {
            component.package_id == self.package_id
                && component.descriptor.component_digest == self.component_digest
                && component.descriptor.component == self.component
        })?;
        component.descriptor.operations.get(&self.operation)?;
        let configuration = self
            .entry
            .wiring()
            .map(|position| position.configuration.as_ref());
        component.descriptor.connections.iter().find(|connection| {
            connection.store_alias.name() == Some(alias)
                || configuration
                    .and_then(|configuration| connection.store_alias.resolve(configuration))
                    .as_ref()
                    == Some(alias)
        })
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
            source: EffectSource::Package(&self.package_id),
            wiring_id,
            wiring_version,
            node_id,
            occurrence,
            component_digest: Some(&self.component_digest),
            component_name: self.component.as_str(),
            operation: self.operation.as_str(),
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
    pub wiring_hash: wamn_digest::Digest,
    /// The application that owns the wiring.
    pub application: wamn_catalog::ApplicationName,
    pub wiring_id: wamn_catalog::canonical_name::CanonicalName,
    pub wiring_version: u32,
    pub node_id: wamn_catalog::canonical_name::CanonicalName,
    pub occurrence: u32,
    pub configuration: Arc<
        std::collections::BTreeMap<wamn_catalog::canonical_name::CanonicalName, serde_json::Value>,
    >,
}

/// Host-attested root identity for an effect's exact dependency path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionOrigin {
    pub package_id: wamn_catalog::PackageName,
    pub component_digest: wamn_digest::Digest,
    pub component: wamn_catalog::canonical_name::CanonicalName,
    pub interface_version: wamn_catalog::canonical_name::CanonicalName,
    pub operation: wamn_catalog::canonical_name::CanonicalName,
}

/// Host-owned authority closure for one component invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionExecutionClosure {
    /// A digest-verified serving manifest owns the component and wiring.
    Released,
    /// Private admission froze an exact candidate wiring and binding world.
    Candidate {
        /// The release the candidate resolves under, as its manifest digest.
        manifest_digest: wamn_digest::Digest,
        environment: wamn_registry::EnvName,
        wiring_hash: wamn_digest::Digest,
        component: wamn_catalog::canonical_name::CanonicalName,
        interface_version: wamn_catalog::canonical_name::CanonicalName,
        binding_world: Arc<CandidateBindingWorld>,
    },
}

/// Find the application authorized to execute this exact released component.
pub fn released_invocation_application<'a>(
    manifest: &ServingManifest,
    target: &'a wamn_engine::release_manifest::ReleaseTarget,
    invocation: &ConnectionInvocation,
) -> Option<&'a wamn_catalog::ApplicationName> {
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
    if invocation.package_id.as_str() == wamn_catalog::PALETTE_PACKAGE_ID {
        let InvocationEntry::Wiring(position) = &invocation.entry else {
            return None;
        };
        target
            .applications
            .get_key_value(&position.application)
            .map(|(name, _)| name)
    } else {
        if !manifest
            .release
            .packages
            .iter()
            .any(|package| package.package_id() == &invocation.package_id)
        {
            return None;
        }
        let application =
            wamn_catalog::application_for_package(&target.applications, &invocation.package_id)?;
        if invocation
            .entry
            .wiring()
            .is_some_and(|position| &position.application != application)
        {
            return None;
        }
        Some(application)
    }
}

/// Require the executing operation to be reachable from the attested wiring root.
///
/// Shared with the blobstore host service rather than reimplemented there: two
/// spellings of "does this component belong to this release" could disagree,
/// and the one that was wrong would authorize an effect.
/// The snapshot entry for one bound invocation: its route, or its wiring node.
///
/// Shared with the blobstore host service for the reason
/// [`authorize_release_closure`] gives.
pub fn entry_lookup(
    invocation: &ConnectionInvocation,
) -> Result<ConnectionEntryLookup<'_>, ConnectionError> {
    Ok(match &invocation.entry {
        InvocationEntry::Route => ConnectionEntryLookup::Route {
            package_id: &invocation.origin.package_id,
        },
        InvocationEntry::Wiring(position) => ConnectionEntryLookup::Wiring {
            application: &position.application,
            wiring: WiringLookup {
                wiring_id: &position.wiring_id,
                wiring_version: i32::try_from(position.wiring_version)
                    .map_err(|_| ConnectionError::AttestationInvalid)?,
                node_id: &position.node_id,
            },
        },
    })
}

pub fn authorize_release_closure(
    manifest: &ServingManifest,
    invocation: &ConnectionInvocation,
    snapshot: &ConnectionEffectSnapshot,
) -> Result<(), ConnectionError> {
    let digest = invocation.component_digest;
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
            && candidate.descriptor.component_digest == digest
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
            snapshot.node_permitted && snapshot.wiring_hash.as_ref() == Some(&position.wiring_hash)
        }
    };
    let root = manifest.components.iter().find(|component| {
        component.package_id == origin.package_id
            && component.descriptor.component == origin.component
            && component.descriptor.interface_version == origin.interface_version
            && component.descriptor.component_digest == origin.component_digest
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
    root_operation: &wamn_catalog::canonical_name::CanonicalName,
    invocation: &ConnectionInvocation,
) -> bool {
    root.descriptor.operations.contains_key(root_operation)
        && root.package_id == invocation.package_id
        && root.descriptor.component == invocation.component
        && root.descriptor.component_digest == invocation.component_digest
        && root_operation == &invocation.operation
}

/// Require the snapshot to carry the wiring hash, component and interface
/// version frozen at candidate admission, and to equal the frozen binding row.
///
/// Shared with the blobstore host service for the reason
/// [`authorize_release_closure`] gives. Two spellings of the candidate rule can
/// disagree, and the wrong one authorizes an effect.
pub fn authorize_candidate_closure(
    invocation: &ConnectionInvocation,
    snapshot: &ConnectionEffectSnapshot,
    binding: &wamn_authority::identity::CandidateConnectionBinding,
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
        || snapshot.operation.as_ref() != Some(&invocation.operation)
        || snapshot.wiring_hash.as_ref() != Some(wiring_hash)
        || snapshot.component.as_ref() != Some(component)
        || snapshot.interface_version.as_ref() != Some(interface_version)
        || !binding.matches_snapshot(snapshot)
    {
        return Err(ConnectionError::AttestationInvalid);
    }
    Ok(())
}
