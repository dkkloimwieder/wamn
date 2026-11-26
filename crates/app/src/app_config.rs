//! Application facts supplied to host plugins through workload configuration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The route component's application configuration key.
pub const APP_CONFIG: &str = "wamn.app";

/// Deploy facts shared by an application's host plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AppConfig {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,
    pub app: wamn_registry::AppName,
    /// One positive grant cache lifetime for the project, in seconds.
    pub grant_cache_seconds: u32,
    pub components: Vec<AppComponent>,
}

/// One component's dispatch identity, SQL schema and operation policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AppComponent {
    /// The sibling component name in the WorkloadDeployment.
    pub name: String,
    pub package: wamn_package::PackageName,
    pub digest: wamn_digest::Digest,
    pub connections: Vec<AppConnection>,
    /// Absent for a component that declares no SQL.
    pub schema: Option<String>,
    pub operations: BTreeMap<wamn_package::canonical_name::CanonicalName, AppOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_change: Option<AppDataChange>,
}

/// The role, SQL transaction policy and resolved models of a handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AppDataChange {
    pub role: wamn_role::RoleName,
    pub host_transaction: bool,
    pub models: Vec<DataChangeModel>,
}

/// The source package and entity that identify one model in a CDC message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DataChangeModel {
    pub owner_package: wamn_package::PackageName,
    pub model: wamn_package::manifest::source_name::SourceName,
    pub entity: wamn_package::manifest::source_name::SourceName,
    pub ops: Vec<wamn_package::RowOperation>,
}

/// The host policy of one component export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AppOperation {
    /// The sibling component name used for dispatch in the WorkloadDeployment.
    pub component: String,
    pub public: bool,
    pub host_transaction: bool,
    /// SQL admission mode, preserving delete as a distinct operation type.
    /// Absent for exports without a package operation contract (such as palette nodes).
    #[serde(rename = "type")]
    pub type_: Option<AppOperationType>,
    pub idempotency_key: Option<String>,
    pub signed_result: Option<wamn_package::SignedFileReads>,
}

/// The SQL admission type of an application operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppOperationType {
    Read,
    Write,
    Delete,
}

impl AppOperationType {
    /// The SQL mode for a routed package operation.
    pub fn from_operation(operation: wamn_package::OperationType) -> Option<Self> {
        use wamn_package::OperationType;
        match operation {
            OperationType::EventHandler => None,
            OperationType::Delete => Some(Self::Delete),
            read if read.is_read() => Some(Self::Read),
            _ => Some(Self::Write),
        }
    }
}

/// One component declaration resolved by deploy, without runtime storage or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AppConnection {
    pub name: wamn_package::canonical_name::CanonicalName,
    pub selector: wamn_package::ComponentConnectionAlias,
    pub connection_id: String,
    pub mode: wamn_package::ConnectionMode,
    pub interface: String,
    pub integration_type: wamn_registry::integration_code::IntegrationCode,
    pub runtime_type: wamn_connection_type::RuntimeType,
    pub exclusive: bool,
    pub target_type: String,
    /// Full repository@digest of a wasm integration.
    pub integration: Option<String>,
}

/// Look up one declared name on the exact workload sibling component.
pub fn binding<'a>(app: &'a AppConfig, component: &str, name: &str) -> Option<&'a AppConnection> {
    app.components
        .iter()
        .find(|entry| entry.name == component)?
        .connections
        .iter()
        .find(|binding| binding.name.as_str() == name)
}

/// Find the declared export on an exact workload sibling.
pub fn workflow_call<'a>(
    app: &'a AppConfig,
    sibling: &wamn_package::canonical_name::CanonicalName,
    export: &wamn_package::canonical_name::CanonicalName,
) -> Option<(&'a AppComponent, &'a AppOperation)> {
    let component = app
        .components
        .iter()
        .find(|item| item.name == sibling.as_str())?;
    Some((component, component.operations.get(export)?))
}

/// Validate an authored graph against this workload's available exports.
pub fn check_authored_workflow<'a>(
    app: &AppConfig,
    graph: &'a wamn_workflow_graph::WorkflowGraph,
    role: Option<&str>,
) -> Result<wamn_workflow_graph::authoring::Checked<'a>, wamn_workflow_graph::GraphError> {
    wamn_workflow_graph::authoring::check(graph, app.app.as_str(), role, |sibling, export| {
        let (component, operation) = workflow_call(app, sibling, export)?;
        let palette = component.package.as_str() == wamn_package::PALETTE_PACKAGE_ID;
        Some(match operation.type_ {
            Some(AppOperationType::Write | AppOperationType::Delete) => true,
            type_ if !palette => type_.is_none(),
            _ => !matches!(
                component.name.as_str(),
                "wamn_palette-jsonata" | "wamn_palette-transform" | "wamn_palette-label-render"
            ),
        })
    })
}

/// Verify that every call in an enabled graph is available in the candidate workload.
pub fn check_workflow_references(
    app: &AppConfig,
    graph: &wamn_workflow_graph::WorkflowGraph,
) -> Result<(), wamn_workflow_graph::GraphError> {
    for (id, node) in &graph.nodes {
        if let wamn_workflow_graph::GraphNode::Call {
            sibling, export, ..
        } = node
            && workflow_call(app, sibling, export).is_none()
        {
            return Err(wamn_workflow_graph::GraphError::InvalidDefinition {
                message: format!("node {id} names an unavailable sibling export"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIBLING: &str = "shop-orders";
    const EXPORT: &str = "shop:orders/place@1.0.0";

    /// A candidate release whose `exports` lists each sibling and its exports.
    fn release(exports: &[(&str, &[&str])]) -> AppConfig {
        AppConfig {
            org: "acme".parse().unwrap(),
            project: "shop".parse().unwrap(),
            env: "dev".parse().unwrap(),
            app: "app".parse().unwrap(),
            grant_cache_seconds: 60,
            components: exports
                .iter()
                .map(|(sibling, operations)| AppComponent {
                    name: (*sibling).to_owned(),
                    package: "shop".parse().unwrap(),
                    digest: wamn_digest::bytes_sha256(sibling.as_bytes()),
                    connections: Vec::new(),
                    schema: None,
                    operations: operations
                        .iter()
                        .map(|export| {
                            let operation = AppOperation {
                                component: (*sibling).to_owned(),
                                public: true,
                                host_transaction: true,
                                type_: Some(AppOperationType::Write),
                                idempotency_key: None,
                                signed_result: None,
                            };
                            (export.parse().unwrap(), operation)
                        })
                        .collect(),
                    data_change: None,
                })
                .collect(),
        }
    }

    #[test]
    fn an_enabled_graph_refuses_a_release_without_its_sibling_or_export() {
        let graph = wamn_workflow_graph::WorkflowGraph::parse(&serde_json::json!({
            "format-version": "0.3", "graph-id": "place", "version": 1,
            "nodes": {
                "entry": {"entry": {"request": {"path": "/place"}}},
                "call": {"call": {"sibling": SIBLING, "export": EXPORT}},
                "response": {"response": {"status": 200}},
            },
            "edges": [{"from": "entry", "to": "call"}, {"from": "call", "to": "response"}],
        }))
        .unwrap();
        check_workflow_references(&release(&[(SIBLING, &[EXPORT])]), &graph).unwrap();
        for removed in [
            release(&[]),
            release(&[("shop-other", &[EXPORT])]),
            release(&[(SIBLING, &["shop:orders/cancel@1.0.0"])]),
        ] {
            assert!(check_workflow_references(&removed, &graph).is_err());
        }
    }
}
