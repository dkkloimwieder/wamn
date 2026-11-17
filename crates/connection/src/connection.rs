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
                && component.name == self.component.as_str()
        })?;
        component.operations.get(&self.operation)?;
        wamn_catalog::application_config::binding(application, &component.name, alias.as_str())
    }

    /// Build call authority from the configured operation and its exact sibling.
    pub fn for_operation(
        application: &ApplicationConfig,
        operation: &wamn_catalog::canonical_name::CanonicalName,
    ) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let (component, policy) = application
            .components
            .iter()
            .find_map(|component| {
                component
                    .operations
                    .get(operation)
                    .map(|policy| (component, policy))
            })
            .context("operation component missing")?;
        anyhow::ensure!(
            component.name == policy.component,
            "operation component disagrees"
        );
        let name: wamn_catalog::canonical_name::CanonicalName = component.name.parse()?;
        Ok(Self {
            entry: InvocationEntry::Route,
            package_id: component.package.clone(),
            component_digest: component.digest,
            component: name,
            operation: operation.clone(),
            closure: ConnectionExecutionClosure::Released,
            effects: None,
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

/// Host-owned authority closure for one component invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionExecutionClosure {
    /// Workload configuration owns the declared component and connection bindings.
    Released,
}
