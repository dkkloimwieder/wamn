//! Private format 0.3 graph and its ordered outgoing edges.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::Value;

use crate::bindings::wamn::workflow::run::{FailureType, Workflow};
use crate::walk::WalkError;

#[path = "../../../crates/package/src/row_operation.rs"]
mod row_operation;
use row_operation::RowOperation;

pub(crate) const MAIN: &str = "main";
pub(crate) const ERROR: &str = "error";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Document {
    format_version: String,
    graph_id: String,
    version: u32,
    nodes: BTreeMap<String, Node>,
    #[serde(default)]
    edges: Vec<Edge>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Node {
    Entry(Trigger),
    Call(Call),
    Response(Response),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Trigger {
    Request {
        #[serde(rename = "path")]
        _path: String,
    },
    Schedule {
        #[serde(rename = "cron")]
        _cron: String,
    },
    DataChange {
        #[serde(rename = "package")]
        _package: String,
        #[serde(rename = "entity")]
        _entity: String,
        #[serde(rename = "ops")]
        _ops: Vec<RowOperation>,
        #[serde(rename = "condition")]
        _condition: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Call {
    pub(crate) sibling: String,
    pub(crate) export: String,
    #[serde(default)]
    pub(crate) params: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub(crate) status: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Edge {
    from: String,
    #[serde(default = "main_port")]
    from_port: String,
    to: String,
    to_port: Option<String>,
}

fn main_port() -> String {
    MAIN.into()
}

#[derive(Debug)]
pub(crate) struct Target {
    pub(crate) node: String,
    pub(crate) input_port: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Graph {
    pub(crate) entry: String,
    pub(crate) request: bool,
    pub(crate) nodes: BTreeMap<String, Node>,
    outgoing: HashMap<String, HashMap<String, Vec<Target>>>,
}

impl Graph {
    pub(crate) fn parse(definition: &Workflow) -> Result<Self, WalkError> {
        let doc: Document = serde_json::from_str(&definition.graph_json)
            .map_err(|source| WalkError::invalid_json("parse workflow graph", source))?;
        if doc.format_version != "0.3" {
            return Err(WalkError::failed(
                FailureType::ForeignRevision,
                "foreign-revision",
                format!("unsupported workflow format {}", doc.format_version),
            ));
        }
        if doc.graph_id != definition.id || doc.version != definition.version || doc.version == 0 {
            return Err(invalid("graph identity differs from the admitted workflow"));
        }
        let mut entries = doc.nodes.iter().filter_map(|(id, node)| match node {
            Node::Entry(trigger) => Some((id, trigger)),
            _ => None,
        });
        let Some((entry, trigger)) = entries.next() else {
            return Err(invalid("graph has no entry"));
        };
        if entries.next().is_some() {
            return Err(invalid("graph has multiple entries"));
        }
        let request = matches!(trigger, Trigger::Request { .. });
        if doc.nodes.values().any(|node| {
            matches!(node, Node::Response(response) if !request || !(200..=599).contains(&response.status))
        }) {
            return Err(invalid("invalid Response node"));
        }
        let mut outgoing: HashMap<String, HashMap<String, Vec<Target>>> = HashMap::new();
        for edge in doc.edges {
            let Some(source) = doc.nodes.get(&edge.from) else {
                return Err(invalid("edge source is absent"));
            };
            if matches!(source, Node::Response(_)) {
                return Err(invalid("Response node has an outgoing edge"));
            }
            if !doc.nodes.contains_key(&edge.to) {
                return Err(invalid("edge target is absent"));
            }
            outgoing
                .entry(edge.from)
                .or_default()
                .entry(edge.from_port)
                .or_default()
                .push(Target {
                    node: edge.to,
                    input_port: edge.to_port,
                });
        }
        Ok(Self {
            entry: entry.clone(),
            request,
            nodes: doc.nodes,
            outgoing,
        })
    }

    pub(crate) fn successors(&self, node: &str, port: &str) -> &[Target] {
        self.outgoing
            .get(node)
            .and_then(|ports| ports.get(port))
            .map_or(&[], Vec::as_slice)
    }
}

fn invalid(message: &str) -> WalkError {
    WalkError::failed(FailureType::InvalidInput, "invalid-graph", message)
}
