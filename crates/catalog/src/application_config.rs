//! Application facts supplied to host plugins through workload configuration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The route component's application configuration key.
pub const APPLICATION_CONFIG: &str = "wamn.application";

/// Deploy facts shared by an application's host plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationConfig {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
    pub application: crate::ApplicationName,
    /// One positive grant cache lifetime for the project, in seconds.
    pub grant_cache_seconds: u32,
    pub components: Vec<ApplicationComponent>,
}

/// One component's dispatch identity, SQL schema and operation policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationComponent {
    /// The sibling component name in the WorkloadDeployment.
    pub name: String,
    pub package: crate::PackageName,
    pub digest: wamn_digest::Digest,
    pub connections: Vec<ApplicationConnection>,
    /// Absent for a component that declares no SQL.
    pub schema: Option<String>,
    pub operations: BTreeMap<crate::canonical_name::CanonicalName, ApplicationOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_change: Option<ApplicationDataChange>,
}

/// The role, SQL transaction policy and resolved models of a handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationDataChange {
    pub role: wamn_identity_protocol::RoleName,
    pub host_transaction: bool,
    pub models: Vec<DataChangeModel>,
}

/// The source package and entity that identify one model in a CDC message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DataChangeModel {
    pub owner_package: crate::PackageName,
    pub model: crate::manifest::source_name::SourceName,
    pub entity: crate::manifest::source_name::SourceName,
    pub ops: Vec<crate::WiringEventOperation>,
}

/// The host policy of one component export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationOperation {
    /// The sibling component name used for dispatch in the WorkloadDeployment.
    pub component: String,
    pub public: bool,
    pub host_transaction: bool,
    /// SQL admission mode, preserving delete as a distinct operation type.
    /// Absent for exports without a package operation contract (such as palette nodes).
    #[serde(rename = "type")]
    pub type_: Option<ApplicationOperationType>,
    pub idempotency_key: Option<String>,
    pub signed_result: Option<crate::SignedFileReads>,
}

/// The SQL admission type of an application operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplicationOperationType {
    Read,
    Write,
    Delete,
}

/// One component declaration resolved by deploy, without runtime storage or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ApplicationConnection {
    pub name: crate::canonical_name::CanonicalName,
    pub selector: crate::ComponentConnectionAlias,
    pub connection_id: String,
    pub mode: crate::ConnectionMode,
    pub interface: String,
    pub integration_type: wamn_registry::integration_code::IntegrationCode,
    pub runtime_type: crate::RuntimeType,
    pub exclusive: bool,
    pub target_type: String,
    /// Full repository@digest of a wasm integration.
    pub integration: Option<String>,
}

/// Look up one declared name on the exact workload sibling component.
pub fn binding<'a>(
    application: &'a ApplicationConfig,
    component: &str,
    name: &str,
) -> Option<&'a ApplicationConnection> {
    application
        .components
        .iter()
        .find(|entry| entry.name == component)?
        .connections
        .iter()
        .find(|binding| binding.name.as_str() == name)
}

/// Find the declared export on an exact workload sibling.
pub fn workflow_call<'a>(
    application: &'a ApplicationConfig,
    sibling: &crate::canonical_name::CanonicalName,
    export: &crate::canonical_name::CanonicalName,
) -> Option<(&'a ApplicationComponent, &'a ApplicationOperation)> {
    let component = application
        .components
        .iter()
        .find(|item| item.name == sibling.as_str())?;
    Some((component, component.operations.get(export)?))
}

/// Verify that every call in an enabled graph is available in the candidate workload.
pub fn check_workflow_references(
    application: &ApplicationConfig,
    graph: &crate::WiringDocument,
) -> Result<(), crate::CatalogIdentityError> {
    for (id, node) in &graph.nodes {
        if let crate::WiringNode::Call {
            sibling, export, ..
        } = node
            && workflow_call(application, sibling, export).is_none()
        {
            return Err(crate::CatalogIdentityError::InvalidDefinition {
                message: format!("node {id} names an unavailable sibling export"),
            });
        }
    }
    Ok(())
}
