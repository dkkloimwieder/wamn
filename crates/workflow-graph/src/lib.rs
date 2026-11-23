//! Authored workflow graphs and their canonical JSON identity.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};
use wamn_package::{RowOperation, canonical_name::CanonicalName};

/// The only accepted workflow graph format.
pub const GRAPH_FORMAT_VERSION: &str = "0.3";

/// Default node output port.
pub const MAIN_PORT: &str = "main";

/// Node output port for failures.
pub const ERROR_PORT: &str = "error";

/// The trigger declared by a workflow's entry node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum GraphTrigger {
    Request {
        path: String,
    },
    Schedule {
        cron: String,
    },
    DataChange {
        package: wamn_package::PackageName,
        entity: CanonicalName,
        ops: Vec<RowOperation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
    },
}

/// A workflow entry, exact sibling call, or HTTP response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum GraphNode {
    Entry(GraphTrigger),
    Call {
        sibling: CanonicalName,
        export: CanonicalName,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        params: BTreeMap<CanonicalName, Value>,
    },
    Response {
        status: u16,
    },
}

/// A connection from one node output to another node input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GraphEdge {
    pub from: CanonicalName,
    #[serde(default = "main_port", skip_serializing_if = "is_main_port")]
    pub from_port: CanonicalName,
    pub to: CanonicalName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_port: Option<CanonicalName>,
}

/// An immutable authored version of a workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WorkflowGraph {
    pub format_version: String,
    pub graph_id: CanonicalName,
    pub version: u32,
    pub nodes: BTreeMap<CanonicalName, GraphNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<GraphEdge>,
}

impl WorkflowGraph {
    /// Decode the graph and verify its format and structural constraints.
    pub fn parse(value: &Value) -> Result<Self, GraphError> {
        let document = Self::deserialize(value)
            .map_err(|error| invalid(format!("invalid workflow graph JSON: {error}")))?;
        if document.format_version != GRAPH_FORMAT_VERSION {
            return Err(invalid(format!(
                "workflow graph format-version must be {GRAPH_FORMAT_VERSION}"
            )));
        }
        if document.version == 0 {
            return Err(GraphError::ZeroVersion { field: "version" });
        }
        if contains_secret_value(value) {
            return Err(invalid(
                "workflow graph contains a secret-shaped value; use an environment connection",
            ));
        }
        for node in document.nodes.values() {
            if let GraphNode::Response { status } = node
                && !(200..=599).contains(status)
            {
                return Err(invalid(
                    "workflow response status must be between 200 and 599",
                ));
            }
        }
        for edge in &document.edges {
            for endpoint in [&edge.from, &edge.to] {
                if !document.nodes.contains_key(endpoint) {
                    return Err(GraphError::UnresolvedNode {
                        node_id: endpoint.clone(),
                    });
                }
            }
            if matches!(document.nodes[&edge.from], GraphNode::Response { .. })
                || matches!(document.nodes[&edge.to], GraphNode::Entry(_))
            {
                return Err(invalid("edges may not leave a response or enter an entry"));
            }
        }
        Ok(document)
    }

    /// The SHA-256 of the graph's canonical JSON.
    pub fn graph_hash(&self) -> wamn_digest::Digest {
        wamn_digest::canonical_json_sha256(
            &serde_json::to_value(self).expect("workflow graph serializes"),
        )
    }
}

fn contains_secret_value(value: &Value) -> bool {
    match value {
        Value::String(value) => ["Bearer ", "-----BEGIN", "AKIA"]
            .iter()
            .any(|prefix| value.starts_with(prefix)),
        Value::Array(values) => values.iter().any(contains_secret_value),
        Value::Object(values) => values.values().any(contains_secret_value),
        _ => false,
    }
}

fn invalid(message: impl Into<String>) -> GraphError {
    GraphError::InvalidDefinition {
        message: message.into(),
    }
}

fn main_port() -> CanonicalName {
    MAIN_PORT.parse().expect("fixed main port")
}

fn is_main_port(port: &CanonicalName) -> bool {
    port == MAIN_PORT
}

/// A refused workflow definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    ZeroVersion { field: &'static str },
    InvalidDefinition { message: String },
    UnresolvedNode { node_id: CanonicalName },
}

impl fmt::Display for GraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroVersion { field } => write!(formatter, "{field} must be greater than zero"),
            Self::InvalidDefinition { message } => write!(formatter, "{message}"),
            Self::UnresolvedNode { node_id } => {
                write!(
                    formatter,
                    "graph edge names node {node_id:?}, which the document does not declare"
                )
            }
        }
    }
}

impl std::error::Error for GraphError {}

#[cfg(test)]
mod tests {
    use super::{GRAPH_FORMAT_VERSION, GraphError, WorkflowGraph};
    use serde_json::json;

    #[test]
    fn prior_graph_format_is_refused() {
        let mut graph = json!({
            "format-version": GRAPH_FORMAT_VERSION,
            "graph-id": "orders",
            "version": 1,
            "nodes": {"start": {"entry": {"request": {"path": "/orders"}}}},
        });
        assert!(WorkflowGraph::parse(&graph).is_ok());

        graph["format-version"] = json!("0.2");
        assert!(matches!(
            WorkflowGraph::parse(&graph),
            Err(GraphError::InvalidDefinition { .. })
        ));
    }
}
