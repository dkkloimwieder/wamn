//! Host-attested connection invocations and application authorization.

use std::sync::Arc;
use wamn_authority::effect_span::{EffectEvidence, EffectGraph, EffectSource};
use wamn_engine::application_config::{ApplicationConfig, ApplicationConnection};

/// Host-attested identity of one component invocation.
///
/// `component` is the name the catalog admitted the executing component under,
/// and `operation`, when present, is the operation called on it. Both
/// come from the driver, never from the guest. The operation identifies the
/// authorized export; an effect span records the host function separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInvocation {
    /// Where the invocation entered the release, unchanged by nested calls.
    pub entry: InvocationEntry,
    pub package_id: wamn_package::PackageName,
    pub component_digest: wamn_digest::Digest,
    pub component: wamn_package::canonical_name::CanonicalName,
    pub operation: Option<wamn_package::canonical_name::CanonicalName>,
    pub closure: ConnectionExecutionClosure,
    /// Optional bounded failure evidence owned by this invocation.
    pub effects: Option<EffectEvidence>,
}

impl ConnectionInvocation {
    /// Resolve a host connection declared by the executing component.
    pub fn connection<'a>(
        &self,
        application: &'a ApplicationConfig,
        alias: &wamn_package::canonical_name::CanonicalName,
    ) -> Option<&'a ApplicationConnection> {
        let component = application.components.iter().find(|component| {
            component.package == self.package_id
                && component.digest == self.component_digest
                && component.name == self.component.as_str()
        })?;
        match (&self.entry, &self.operation) {
            (InvocationEntry::DataChange, None) => {
                component.data_change.as_ref()?;
            }
            (InvocationEntry::Route | InvocationEntry::Graph(_), Some(operation)) => {
                component.operations.get(operation)?;
            }
            _ => return None,
        }
        wamn_engine::application_config::binding(application, &component.name, alias.as_str())
    }

    /// Build call authority from the configured operation and its exact sibling.
    pub fn for_operation(
        application: &ApplicationConfig,
        operation: &wamn_package::canonical_name::CanonicalName,
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
        let name: wamn_package::canonical_name::CanonicalName = component.name.parse()?;
        Ok(Self {
            entry: InvocationEntry::Route,
            package_id: component.package.clone(),
            component_digest: component.digest,
            component: name,
            operation: Some(operation.clone()),
            closure: ConnectionExecutionClosure::Released,
            effects: None,
        })
    }

    /// The span coordinates of this invocation. A route has no graph
    /// position, so it records the graph keys empty.
    pub fn effect_graph(&self) -> EffectGraph<'_> {
        let (graph_id, graph_version, node_id, occurrence) = match self.entry.graph() {
            None => ("", 0, "", 0),
            Some(position) => (
                position.graph_id.as_str(),
                position.graph_version,
                position.node_id.as_str(),
                position.occurrence,
            ),
        };
        EffectGraph {
            source: EffectSource::Package(&self.package_id),
            graph_id,
            graph_version,
            node_id,
            occurrence,
            component_digest: Some(&self.component_digest),
            component_name: self.component.as_str(),
            operation: self
                .operation
                .as_ref()
                .map_or("", |operation| operation.as_str()),
        }
    }
}

/// Where one invocation entered the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationEntry {
    /// A route called the origin export once. The origin names the route.
    Route,
    /// A JetStream delivery to a declared data change handler.
    DataChange,
    /// One node occurrence of an exact graph version.
    Graph(GraphPosition),
}

impl InvocationEntry {
    /// The graph position, or `None` for a route.
    pub fn graph(&self) -> Option<&GraphPosition> {
        match self {
            Self::Route | Self::DataChange => None,
            Self::Graph(position) => Some(position),
        }
    }
}

/// One node occurrence of an exact graph version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphPosition {
    /// The immutable graph hash frozen on the run.
    pub graph_hash: wamn_digest::Digest,
    /// The application that owns the graph.
    pub application: wamn_registry::ApplicationName,
    pub graph_id: wamn_package::canonical_name::CanonicalName,
    pub graph_version: u32,
    pub node_id: wamn_package::canonical_name::CanonicalName,
    pub occurrence: u32,
    pub configuration: Arc<
        std::collections::BTreeMap<wamn_package::canonical_name::CanonicalName, serde_json::Value>,
    >,
}

/// Host-owned authority closure for one component invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionExecutionClosure {
    /// Workload configuration owns the declared component and connection bindings.
    Released,
}
