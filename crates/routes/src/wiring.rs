//! Authored workflow graphs and their canonical JSON identity.

use crate::WiringError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use wamn_package::{WiringEventOperation, canonical_name::CanonicalName};
use wamn_registry::ApplicationName;

/// The only accepted workflow graph format.
pub const WIRING_DOCUMENT_FORMAT_VERSION: &str = "0.2";

/// The trigger declared by a workflow's entry node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum WiringEntry {
    Request {
        path: String,
    },
    Schedule {
        cron: String,
    },
    DataChange {
        package: wamn_package::PackageName,
        entity: CanonicalName,
        ops: Vec<WiringEventOperation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
    },
}

/// A workflow entry, exact sibling call, or HTTP response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum WiringNode {
    Entry(WiringEntry),
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
pub struct WiringEdge {
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
pub struct WiringDocument {
    pub format_version: String,
    pub wiring_id: CanonicalName,
    pub version: u32,
    pub nodes: BTreeMap<CanonicalName, WiringNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<WiringEdge>,
}

impl WiringDocument {
    /// Decode the graph and verify its format and structural constraints.
    pub fn parse(value: &Value) -> Result<Self, WiringError> {
        let document = Self::deserialize(value)
            .map_err(|error| invalid(format!("invalid workflow graph JSON: {error}")))?;
        if document.format_version != WIRING_DOCUMENT_FORMAT_VERSION {
            return Err(invalid("workflow graph format-version must be 0.2"));
        }
        if document.version == 0 {
            return Err(WiringError::ZeroVersion { field: "version" });
        }
        if contains_secret_value(value) {
            return Err(invalid(
                "workflow graph contains a secret-shaped value; use an environment connection",
            ));
        }
        for node in document.nodes.values() {
            if let WiringNode::Response { status } = node
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
                    return Err(WiringError::UnresolvedWiringNode {
                        node_id: endpoint.clone(),
                    });
                }
            }
            if matches!(document.nodes[&edge.from], WiringNode::Response { .. })
                || matches!(document.nodes[&edge.to], WiringNode::Entry(_))
            {
                return Err(invalid("edges may not leave a response or enter an entry"));
            }
        }
        Ok(document)
    }

    /// The SHA-256 of the graph's canonical JSON.
    pub fn wiring_hash(&self) -> wamn_digest::Digest {
        wamn_digest::canonical_json_sha256(
            &serde_json::to_value(self).expect("workflow graph serializes"),
        )
    }
}

/// The native ingress path reserved for an application's Request workflows.
pub fn workflow_http_prefix(application: &ApplicationName) -> String {
    format!("/api/{application}/flow")
}

/// The grant reference that persists across versions and Request paths.
pub fn workflow_permission(application: &ApplicationName, id: &str) -> String {
    format!("wamn-workflow:{application}/{id}")
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

fn invalid(message: impl Into<String>) -> WiringError {
    WiringError::InvalidDefinition {
        message: message.into(),
    }
}

fn main_port() -> CanonicalName {
    crate::MAIN_PORT.parse().expect("fixed main port")
}

fn is_main_port(port: &CanonicalName) -> bool {
    port == crate::MAIN_PORT
}
