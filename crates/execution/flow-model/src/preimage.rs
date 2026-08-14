//! The digest preimage projection of a flow (W2 digest ordering).
//!
//! Every definition digest that covers a graph — `graph_hash`, the catalog
//! artifact's `graph` frame, and `draft-content-hash` — hashes this projection
//! rather than the [`Flow`] document itself. Routing all of them through one
//! `#[derive(Serialize)]` view makes each field a **compile-time** decision:
//! a field added to [`Flow`] does not reach a digest until it is added here,
//! and a field listed here cannot be dropped from a digest by accident.
//!
//! The projection differs from the document in exactly the ways W2 requires
//! (see `docs/archive/authoring/authoring-surface.md` §Digest ordering):
//!
//! - **nodes are ordered by [`Node::id`]**, the stable identity every consumer
//!   looks up by, so a pure reordering of the document's `nodes` array is not a
//!   new artifact identity;
//! - **edges are ordered by their stable key**, `(from, from-port, ordinal, to,
//!   to-port)`. Fan-out order is semantic, so it is carried by the explicit
//!   [`Edge::ordinal`] and hashed; the array position that used to imply it is
//!   not. `Flow::validate`'s `duplicate-edge` refusal is what makes the key
//!   total;
//! - **display text is absent.** [`Flow::name`], [`Node::label`],
//!   [`crate::CredentialRef::description`], and [`crate::CredentialRef::kind`] are editor
//!   text with no reader anywhere in the platform — the vault resolves a
//!   credential by [`crate::CredentialRef::name`] and `diff` compares credentials by
//!   name alone — so renaming does not mint a new artifact identity. They stay
//!   in the document; they simply never enter a digest.
//!
//! Object-key order and whitespace never reach a digest at all: the projection
//! is serialized through [`crate::canonical`] (RFC 8785).

use serde::Serialize;
use serde_json::Value;

use crate::types::{Edge, Flow, FlowConnectionRequirement, Node};

/// The canonical digest preimage of a [`Flow`].
///
/// Build it with [`FlowPreimage::of`] for the ordinary graph preimage, or with
/// [`FlowPreimage::version_independent`] for the content address that must not
/// move when only the flow's publish version changes.
///
/// # Examples
///
/// ```
/// let flow = wamn_flow::Flow::from_json(
///     r#"{"schema-version":"0.1","flow-id":"f","version":1,
///         "nodes":[{"id":"b","type":"respond","config":{"status":200}},
///                  {"id":"a","type":"request","config":{"input-schema":true}}]}"#,
/// )
/// .unwrap();
/// let preimage = serde_json::to_value(wamn_flow::FlowPreimage::of(&flow)).unwrap();
/// let ids: Vec<&str> = preimage["nodes"]
///     .as_array()
///     .unwrap()
///     .iter()
///     .map(|node| node["id"].as_str().unwrap())
///     .collect();
/// assert_eq!(ids, ["a", "b"], "preimage nodes are ordered by node id");
/// ```
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct FlowPreimage<'a> {
    schema_version: &'a str,
    flow_id: &'a str,
    /// Absent for [`FlowPreimage::version_independent`].
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
    /// Ordered by [`Node::id`], never by document sequence.
    nodes: Vec<NodePreimage<'a>>,
    /// Ordered by the stable edge key, never by document sequence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    edges: Vec<&'a Edge>,
    #[serde(skip_serializing_if = "is_empty")]
    connection_requirements: &'a [FlowConnectionRequirement],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    credentials: Vec<CredentialPreimage<'a>>,
    #[serde(skip_serializing_if = "is_empty")]
    allowed_hosts: &'a [String],
}

/// A graph node's identity: [`Node`] without its editor [`Node::label`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct NodePreimage<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    node_type: &'a str,
    #[serde(skip_serializing_if = "is_empty_object")]
    config: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential: Option<&'a str>,
}

/// A credential declaration's identity: the logical name the vault resolves and
/// [`Node::credential`] points at. `kind` is an editor picker hint and
/// `description` is prose; neither is read by anything, so neither is identity.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct CredentialPreimage<'a> {
    name: &'a str,
}

impl<'a> FlowPreimage<'a> {
    /// The graph preimage hashed by [`Flow::graph_hash`] and embedded as the
    /// catalog artifact's `graph` frame.
    pub fn of(flow: &'a Flow) -> FlowPreimage<'a> {
        FlowPreimage {
            version: Some(flow.version),
            ..FlowPreimage::shared(flow)
        }
    }

    /// The version-independent preimage behind `draft-content-hash`: identical
    /// to [`FlowPreimage::of`] with `version` omitted, so two revisions of one
    /// document that differ only in their proposed publish version share a
    /// content address.
    pub fn version_independent(flow: &'a Flow) -> FlowPreimage<'a> {
        FlowPreimage {
            version: None,
            ..FlowPreimage::shared(flow)
        }
    }

    /// Everything both projections agree on. `version` is the only field the
    /// two constructors disagree about, so it is left at its `None` default
    /// here and set by [`FlowPreimage::of`].
    fn shared(flow: &'a Flow) -> FlowPreimage<'a> {
        let mut sorted: Vec<&Node> = flow.nodes.iter().collect();
        // Stable, so a flow that has not been validated (duplicate node ids are
        // an `Flow::validate` error) still hashes deterministically.
        sorted.sort_by(|left, right| left.id.cmp(&right.id));
        let nodes = sorted.into_iter().map(node_preimage).collect();
        let mut edges: Vec<&Edge> = flow.edges.iter().collect();
        edges.sort_by(|left, right| edge_key(left).cmp(&edge_key(right)));
        FlowPreimage {
            schema_version: &flow.schema_version,
            flow_id: &flow.flow_id,
            version: None,
            nodes,
            edges,
            connection_requirements: &flow.connection_requirements,
            credentials: flow
                .credentials
                .iter()
                .map(|credential| CredentialPreimage {
                    name: &credential.name,
                })
                .collect(),
            allowed_hosts: &flow.allowed_hosts,
        }
    }
}

fn node_preimage(node: &Node) -> NodePreimage<'_> {
    NodePreimage {
        id: &node.id,
        node_type: &node.node_type,
        config: &node.config,
        connection: node.connection.as_deref(),
        credential: node.credential.as_deref(),
    }
}

/// Matches [`Node::config`]'s own `skip_serializing_if`, so an absent config is
/// absent from the preimage exactly as it is from the document.
fn is_empty_object(value: &&Value) -> bool {
    match value {
        Value::Object(map) => map.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

/// The stable edge key: the source endpoint, the explicit fan-out
/// [`Edge::ordinal`] within that endpoint's group, then the target endpoint.
/// Total for a valid flow, because `duplicate-edge` refuses a repeated
/// `(from, from-port, to, to-port)`.
fn edge_key(edge: &Edge) -> (&str, &str, u32, &str, Option<&str>) {
    (
        edge.from.as_str(),
        edge.from_port.as_str(),
        edge.ordinal.unwrap_or(0),
        edge.to.as_str(),
        edge.to_port.as_deref(),
    )
}

fn is_empty<T>(items: &&[T]) -> bool {
    items.is_empty()
}
