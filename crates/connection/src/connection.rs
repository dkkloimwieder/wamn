//! Host-attested connection invocations and application authorization.

use std::sync::Arc;
use wamn_authority::effect_span::{EffectEvidence, EffectSource, EffectWiring};
use wamn_catalog::application_config::{ApplicationConfig, ApplicationConnection};

/// Host-attested identity of one component invocation.
///
/// `component` is the name the catalog admitted the executing component under,
/// and `operation` is the node operation the router driver called on it. Both
/// come from the driver, never from the guest. The operation identifies the
/// authorized export; an effect span records the host function separately.
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
        application: &'a ApplicationConfig,
        alias: &wamn_catalog::canonical_name::CanonicalName,
    ) -> Option<&'a ApplicationConnection> {
        let component = application.components.iter().find(|component| {
            component.package == self.package_id
                && component.digest == self.component_digest
                && component
                    .name
                    .strip_prefix(self.package_id.as_str())
                    .and_then(|suffix| suffix.strip_prefix('-'))
                    == Some(self.component.as_str())
        })?;
        component.operations.get(&self.operation)?;
        let binding = wamn_catalog::application_config::binding(
            application,
            &component.name,
            alias.as_str(),
        )?;
        let selected = match &binding.selector {
            wamn_catalog::ComponentConnectionAlias::Name(name) => name == alias,
            selector => {
                self.entry
                    .wiring()
                    .and_then(|position| selector.resolve(&position.configuration))
                    .as_ref()
                    == Some(alias)
            }
        };
        selected.then_some(binding)
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

}
