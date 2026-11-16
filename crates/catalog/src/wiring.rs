//! The wiring document — the typed graph a project authors over the palette.
//!
//! A wiring is the composition artifact of the component-palette model
//! (`docs/architecture/execution.md#results-and-effects`): nodes name `(component, interface-version)`
//! from the palette, edges connect declared ports, node parameters bind
//! declared params. It is
//! *data, not code* — versioned, conditioned against one environment effective
//! release, and read from its environment workflow row in `catalog.workflows`.
//!
//! # Identity
//!
//! [`WiringDocument::wiring_hash`] is the SHA-256 of the document's RFC 8785
//! canonical JSON, taken through the workspace's single canonicalizer
//! ([`wamn_digest::canonical_json_sha256`]) exactly as
//! [`ServingManifest`](crate::ServingManifest) does. It is the value stored in
//! `catalog.workflows.wiring_hash`.
//!
//! Nodes are a `BTreeMap` keyed by node id, so map order cannot reach the
//! digest and a duplicate id is not a representable state. Edges are a `Vec`
//! because fan-out order is author-meaningful; since the digest is taken over
//! this document rather than a reordered preimage, array position carries that
//! order and no explicit ordinal is needed.

use crate::canonical_name::CanonicalName;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The row operation carried by events and workflow Emit selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WiringEventOperation {
    Insert,
    Update,
    Delete,
}

impl WiringEventOperation {
    /// The serialized operation spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            WiringEventOperation::Insert => "insert",
            WiringEventOperation::Update => "update",
            WiringEventOperation::Delete => "delete",
        }
    }
}

use crate::CatalogIdentityError;

/// One authored invocation through a package-manifest base dependency alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringOperationDependency {
    /// Alias keyed by the owning package's `base_dependencies` declaration.
    pub alias: CanonicalName,
    /// Canonical package-local operation admitted by that dependency.
    pub operation: CanonicalName,
}

/// The only wiring-document format shipped by the MVP.
pub const WIRING_DOCUMENT_FORMAT_VERSION: &str = "0.1";

/// How a node ends its delivery, when it is a terminal.
///
/// The workflow walk reads this declaration directly. It records discard only
/// when the frontier ends without a declared terminal or a waiting caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WiringTerminal {
    /// Release the synchronous caller waiting on this delivery, answering with
    /// this node's emitted payload.
    Respond,
    /// Publish this node's emitted payload as a derived event, keyed by the
    /// author-supplied dedup id the component put in it. The selector is exact
    /// admitted wiring data; the guest payload can never choose its subject.
    Emit {
        /// Stable catalog entity id used in the event subject and registration
        /// match, never a physical relation name inferred at execution time.
        entity: CanonicalName,
        /// The exact event operation admitted for this terminal occurrence.
        operation: WiringEventOperation,
    },
}

impl WiringTerminal {
    /// Construct an emit terminal from its exact admitted selector.
    pub fn emit(entity: CanonicalName, operation: WiringEventOperation) -> Self {
        Self::Emit { entity, operation }
    }
}

/// One graph step: an operation of one palette component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringNode {
    /// The palette component this node runs. It is the instance-pool key —
    /// pools are per component, shared across every wiring using it — and half
    /// of the `(component, interface-version)` pair promotion tests the target
    /// library covers.
    pub component: CanonicalName,
    /// The WIT interface version this node was wired against. The other half of
    /// the promotion pair, and what late binding holds within.
    pub interface_version: CanonicalName,
    /// The operation the router invokes over the typed WIT seam once it holds a
    /// pooled instance (`entity.create` is this field's `create`).
    pub operation: CanonicalName,
    /// Cross-package registered operation this node resolves through, if any.
    /// The package manifest owns the alias-to-exact-target requirement; wiring
    /// bytes name only the alias and one operation from its permitted set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_dependency: Option<WiringOperationDependency>,
    /// Values bound to the component's declared params — a generic `entity`
    /// node's relation and filter spec, for instance. Typed by the component's
    /// WIT declaration and validated by the condition (wamn-0h0g.18.3), not by this
    /// crate: the palette is project data, so a shape written here would be a
    /// second, staler copy of it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<CanonicalName, Value>,
    /// The [`WiringTerminal`] this node ends the delivery with, if it is one.
    /// Absent on ordinary nodes — most of a graph is intermediate work — and
    /// absent from the serialized bytes when it is, so an ordinary node's
    /// contribution to [`WiringDocument::wiring_hash`] is unchanged by this
    /// field existing.
    ///
    /// Several nodes may declare one. Exclusive branches each ending the
    /// delivery their own way is a legitimate graph; *two verdicts in one
    /// delivery* is a refusal the walk makes on the path actually taken
    /// (a second verdict), which a document cannot
    /// decide by shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<WiringTerminal>,
}

/// A wire from one node's output port to a downstream node's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringEdge {
    /// Source node id — a key of [`WiringDocument::nodes`].
    pub from: CanonicalName,
    /// Source output port. Omitted means [`crate::MAIN_PORT`];
    /// [`crate::ERROR_PORT`] is the error path the walk routes failures
    /// along without ending the delivery.
    #[serde(default = "main_port", skip_serializing_if = "is_main_port")]
    pub from_port: CanonicalName,
    /// Target node id — a key of [`WiringDocument::nodes`].
    pub to: CanonicalName,
    /// Target input port. Absent means the component's single declared input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_port: Option<CanonicalName>,
}

/// The normal HTTP response and optional source of committed command evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringResponse {
    /// The Respond terminal whose whole HTTP envelope matches the schema.
    pub node: CanonicalName,
    /// The whole normal HTTP envelope, independent of schemas on wiring edges.
    pub schema: Value,
    /// The node whose admitted operation declares a committed result schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_result: Option<CanonicalName>,
}

/// One version of a wiring: the document `catalog.workflows.graph_json` stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringDocument {
    /// The document format version. A foreign version fails closed at
    /// [`WiringDocument::parse`] rather than being partially understood.
    pub format_version: String,
    /// Stable identifier shared across every version of this wiring.
    pub wiring_id: CanonicalName,
    /// Monotonic version of this wiring — `catalog.workflows.version`, the row a
    /// rollback flips back to.
    pub version: u32,
    /// The node a delivery enters at: where the walk puts the admitted payload
    /// and the only node it can reach without an edge.
    ///
    /// Required, and validated against [`WiringDocument::nodes`] here, so a graph
    /// the router could not enter is refused at authoring rather than in the
    /// serving path — where the same refusal is
    /// the workflow walk, one activation and one
    /// cache miss too late.
    pub entry: CanonicalName,
    /// The graph's nodes, keyed by node id. The router resolves the active
    /// wiring and walks these in frontier order (wamn-0h0g.16.5).
    pub nodes: BTreeMap<CanonicalName, WiringNode>,
    /// The wires between node ports, in fan-out order within each
    /// `(from, from-port)` group — the sequence the walk dispatches a branch's
    /// targets in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<WiringEdge>,
    /// Explicit contracts for normal responses and selected committed results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<WiringResponse>,
}

/// A valid one-node graph of `wiring_id` at `version`, for tests.
#[cfg(any(test, feature = "test-util"))]
pub fn fixture_graph(wiring_id: &CanonicalName, version: u32) -> WiringDocument {
    let node = WiringNode {
        component: "transform".parse().unwrap(),
        interface_version: "0.1".parse().unwrap(),
        operation: "map".parse().unwrap(),
        operation_dependency: None,
        params: BTreeMap::new(),
        terminal: None,
    };
    WiringDocument::new(
        wiring_id.clone(),
        version,
        "in".parse().unwrap(),
        BTreeMap::from([("in".parse().unwrap(), node)]),
        Vec::new(),
    )
    .expect("the fixture graph is valid")
}

impl WiringDocument {
    /// Build and validate one wiring document from its parts.
    ///
    /// Unlike [`ServingManifest::new`](crate::ServingManifest), this is not
    /// fenced behind `test-util`: authoring emits ordinary conditioned wirings —
    /// `generate crud` (wamn-0h0g.23.3) is a production producer — so a fence
    /// here would block the emitter rather than a misuse.
    pub fn new(
        wiring_id: CanonicalName,
        version: u32,
        entry: CanonicalName,
        nodes: BTreeMap<CanonicalName, WiringNode>,
        edges: Vec<WiringEdge>,
    ) -> Result<Self, CatalogIdentityError> {
        let document = Self {
            format_version: WIRING_DOCUMENT_FORMAT_VERSION.to_string(),
            wiring_id,
            version,
            entry,
            nodes,
            edges,
            response: None,
        };
        document.validate()?;
        Ok(document)
    }

    /// Parse and validate a stored document — the `catalog.workflows.graph_json`
    /// reader.
    ///
    /// The column holds a parsed document rather than exact bytes, so identity
    /// is *derived* here and compared by the caller against the stored
    /// `wiring_hash`, never asserted into the document.
    pub fn parse(document: &Value) -> Result<Self, CatalogIdentityError> {
        let document = serde_json::from_value::<Self>(document.clone()).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("wiring document JSON is invalid: {error}"),
            }
        })?;
        document.validate()?;
        Ok(document)
    }

    /// The definition hash over this document's RFC 8785 canonical JSON.
    pub fn wiring_hash(&self) -> wamn_digest::Digest {
        wamn_digest::canonical_json_sha256(&self.as_value())
    }

    fn as_value(&self) -> Value {
        serde_json::to_value(self).expect("wiring document serializes")
    }

    pub(crate) fn validate(&self) -> Result<(), CatalogIdentityError> {
        if contains_secret_value(&self.as_value()) {
            return invalid(
                "wiring document contains a secret-shaped value; supply credentials through an environment-owned connection",
            );
        }
        if self.format_version != WIRING_DOCUMENT_FORMAT_VERSION {
            return invalid("wiring document format-version must be textual 0.1");
        }

        if self.version == 0 {
            return Err(CatalogIdentityError::ZeroVersion { field: "version" });
        }
        if self.nodes.is_empty() {
            return invalid("a wiring with no nodes has nothing for the router to walk");
        }

        if !self.nodes.contains_key(&self.entry) {
            return Err(CatalogIdentityError::UnresolvedWiringEntry {
                node_id: self.entry.clone(),
            });
        }

        for edge in &self.edges {
            for endpoint in [&edge.from, &edge.to] {
                if !self.nodes.contains_key(endpoint) {
                    return Err(CatalogIdentityError::UnresolvedWiringNode {
                        node_id: endpoint.clone(),
                    });
                }
            }
        }

        if let Some(response) = &self.response {
            let node = self.nodes.get(&response.node).ok_or_else(|| {
                CatalogIdentityError::UnresolvedWiringNode {
                    node_id: response.node.clone(),
                }
            })?;
            if node.terminal != Some(WiringTerminal::Respond) {
                return invalid("response node must be a Respond terminal");
            }
            crate::component_facts::normalize_schema(response.schema.clone(), "response").map_err(
                |error| CatalogIdentityError::InvalidDefinition {
                    message: format!("wiring response schema is invalid: {error}"),
                },
            )?;
            if let Some(committed) = &response.committed_result
                && !self.nodes.contains_key(committed)
            {
                return Err(CatalogIdentityError::UnresolvedWiringNode {
                    node_id: committed.clone(),
                });
            }
        }

        Ok(())
    }
}

/// Inspect values, not reference names or object keys. These recognizable
/// prefixes match the existing payload-redaction minimum; arbitrary secrets
/// cannot be identified by their spelling.
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

/// Build the narrow HTTP error schema for one declared committed command result.
pub fn partial_response_schema(committed_schema: &Value) -> Value {
    let mut committed_schema = committed_schema.clone();
    if let Some(schema) = committed_schema.as_object_mut() {
        // Keep document-local references targeted to the original success schema.
        schema
            .entry("$id")
            .or_insert_with(|| Value::String("urn:wamn:committed-result".to_owned()));
    }
    serde_json::json!({
        "type": "object",
        "required": ["committed_result", "failed_outcome"],
        "additionalProperties": false,
        "properties": {
            "committed_result": committed_schema,
            "failed_outcome": {
                "type": "object",
                "required": ["code"],
                "additionalProperties": false,
                "properties": {
                    "code": {"type": "string", "minLength": 1},
                    "message": {"type": "string"},
                    "operation": {"type": "string"},
                    "effect_outcome": {"enum": ["refused-before-dispatch", "responded", "timeout", "cancelled", "effect-uncertain", "response-lost"]}
                }
            }
        }
    })
}

fn invalid(message: &str) -> Result<(), CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.to_string(),
    })
}

fn main_port() -> CanonicalName {
    crate::MAIN_PORT.parse().expect("fixed main port")
}

fn is_main_port(port: &CanonicalName) -> bool {
    port == crate::MAIN_PORT
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::WiringEventOperation as Op;
    use serde_json::{Value, json};

    use super::{
        WiringDocument, WiringEdge, WiringNode, WiringOperationDependency, WiringTerminal,
    };
    use crate::CatalogIdentityError;

    fn node(component: &str) -> WiringNode {
        WiringNode {
            component: component.parse().unwrap(),
            interface_version: "0.1.0".parse().unwrap(),
            operation: "call".parse().unwrap(),
            operation_dependency: None,
            params: BTreeMap::new(),
            terminal: None,
        }
    }

    fn terminal_node(component: &str, terminal: WiringTerminal) -> WiringNode {
        WiringNode {
            terminal: Some(terminal),
            ..node(component)
        }
    }

    fn crud_wiring() -> WiringDocument {
        let nodes: BTreeMap<crate::canonical_name::CanonicalName, WiringNode> = BTreeMap::from([
            ("in".parse().unwrap(), node("http-entry")),
            ("write".parse().unwrap(), node("entity")),
            (
                "out".parse().unwrap(),
                terminal_node("respond", WiringTerminal::Respond),
            ),
        ]);
        let edges = vec![
            WiringEdge {
                from: "in".parse().unwrap(),
                from_port: crate::MAIN_PORT.parse().unwrap(),
                to: "write".parse().unwrap(),
                to_port: None,
            },
            WiringEdge {
                from: "write".parse().unwrap(),
                from_port: crate::MAIN_PORT.parse().unwrap(),
                to: "out".parse().unwrap(),
                to_port: None,
            },
        ];
        WiringDocument::new(
            "orders-create".parse().unwrap(),
            1,
            "in".parse().unwrap(),
            nodes,
            edges,
        )
        .expect("the generated crud shape is a valid wiring")
    }

    #[test]
    fn secret_values_refuse_but_reference_names_remain_portable() {
        let mut wiring = crud_wiring();
        wiring.nodes.get_mut("write").unwrap().params.insert(
            "credential".parse().unwrap(),
            json!({"secret_ref": "fixture-api-token", "environment": "prod"}),
        );
        let portable = serde_json::to_value(&wiring).unwrap();
        WiringDocument::parse(&portable).expect("names are not secret values");
        for secret in [
            "Bearer fixture-token",
            "-----BEGIN RSA PRIVATE KEY-----",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            let mut document = portable.clone();
            *document.pointer_mut("/nodes/write/params").unwrap() = json!({"nested": [secret]});
            let error = WiringDocument::parse(&document).unwrap_err();
            assert!(matches!(
                error,
                CatalogIdentityError::InvalidDefinition { .. }
            ));
            assert!(!error.to_string().contains(secret));
        }
        wiring
            .nodes
            .get_mut("write")
            .unwrap()
            .params
            .insert("header".parse().unwrap(), json!("Bearer fixture-token"));
        WiringDocument::new(
            wiring.wiring_id,
            wiring.version,
            wiring.entry,
            wiring.nodes,
            wiring.edges,
        )
        .expect_err("construction also refuses secret values");
    }

    #[test]
    fn the_document_wire_shape_is_closed() {
        let wiring = crud_wiring();
        let wire = serde_json::to_value(&wiring).expect("serializes");
        {
            let observed = &(wire["nodes"]["write"]);
            assert_eq!(observed["component"], serde_json::json!("entity"));
            assert_eq!(observed["interface-version"], serde_json::json!("0.1.0"));
            assert_eq!(observed["operation"], serde_json::json!("call"));
        };
        // An omitted `from-port` is the main port, so the default never reaches
        // the wire and cannot fork the hash of one authored graph.
        {
            let observed = &(wire["edges"][0]);
            assert_eq!(observed["from"], serde_json::json!("in"));
            assert_eq!(observed["to"], serde_json::json!("write"));
            assert!(observed.get("from-port").is_none());
        };
        assert_eq!(
            WiringDocument::parse(&wire).expect("the exact document parses"),
            wiring
        );

        for foreign in [
            json!({"format-version": "0.2", "wiring-id": "w", "version": 1, "entry": "a",
                   "nodes": {"a": {"component": "c", "interface-version": "0.1.0",
                                   "operation": "call"}}}),
            json!({"format-version": "0.1", "wiring-id": "w", "version": 1, "entry": "a",
                   "nodes": {"a": {"component": "c", "interface-version": "0.1.0",
                                   "operation": "call"}},
                   "test-set": []}),
            json!({"format-version": "0.1", "wiring-id": "w", "version": 1, "entry": "a",
                   "nodes": {}}),
            // `discard` is a verdict the walk settles on, never a declaration.
            json!({"format-version": "0.1", "wiring-id": "w", "version": 1, "entry": "a",
                   "nodes": {"a": {"component": "c", "interface-version": "0.1.0",
                                   "operation": "call", "terminal": "discard"}}}),
        ] {
            assert!(WiringDocument::parse(&foreign).is_err(), "{foreign}");
        }
    }

    #[test]
    fn a_cross_package_node_carries_one_closed_alias_operation_coordinate() {
        let mut stored = serde_json::to_value(crud_wiring()).expect("serializes");
        stored["nodes"]["write"]["operation-dependency"] = json!({
            "alias": "fixture",
            "operation": "widget.record_batch"
        });

        let parsed = WiringDocument::parse(&stored).expect("dependency coordinate parses");
        assert_eq!(
            parsed.nodes["write"].operation_dependency,
            Some(WiringOperationDependency {
                alias: "fixture".parse().unwrap(),
                operation: "widget.record_batch".parse().unwrap(),
            })
        );
        assert_eq!(
            serde_json::to_value(parsed).unwrap()["nodes"]["write"]["operation-dependency"],
            stored["nodes"]["write"]["operation-dependency"]
        );

        stored["nodes"]["write"]["operation-dependency"]["package"] = json!("base");
        assert!(
            WiringDocument::parse(&stored).is_err(),
            "wiring bytes cannot author a parallel package coordinate"
        );
    }

    #[test]
    fn response_contracts_select_existing_nodes_and_validate_the_normal_schema() {
        let original = crud_wiring();
        let mut wire = serde_json::to_value(&original).unwrap();
        assert!(wire.get("response").is_none());
        wire["response"] = json!({
            "node": "out",
            "schema": {"type": "array"},
            "committed-result": "write"
        });
        let declared = WiringDocument::parse(&wire).expect("explicit response contract parses");
        assert_ne!(original.wiring_hash(), declared.wiring_hash());
        assert_eq!(serde_json::to_value(&declared).unwrap(), wire);
        assert_eq!(declared.edges, original.edges);

        for response in [
            json!({"node": "missing", "schema": {}}),
            json!({"node": "write", "schema": {}}),
            json!({"node": "out", "schema": {"type": "unknown"}}),
            json!({"node": "out", "schema": {"$ref": "https://example.invalid/schema"}}),
            json!({"node": "out", "schema": {}, "committed-result": "missing"}),
            json!({"node": "out", "schema": {}, "committed-result": " "}),
        ] {
            wire["response"] = response;
            assert!(WiringDocument::parse(&wire).is_err(), "{wire}");
        }
    }

    #[test]
    fn partial_response_schema_preserves_success_references_and_refuses_extra_results() {
        let success = json!({
            "$defs": {"widget": {"type": "string"}},
            "type": "object",
            "required": ["widget_id"],
            "properties": {"widget_id": {"$ref": "#/$defs/widget"}},
            "additionalProperties": false
        });
        let schema = super::partial_response_schema(&success);
        let mut compiler = boon::Compiler::new();
        compiler.set_default_draft(boon::Draft::V2020_12);
        compiler
            .add_resource("mem://partial-response.json", schema)
            .unwrap();
        let mut schemas = boon::Schemas::new();
        let schema_index = compiler
            .compile("mem://partial-response.json", &mut schemas)
            .unwrap();
        let accepted = json!({
            "committed_result": {"widget_id": "widget-1"},
            "failed_outcome": {"code": "write_failed", "operation": "store", "effect_outcome": "response-lost"}
        });
        schemas
            .validate(&accepted, schema_index)
            .expect("declared evidence validates");
        for (pointer, value) in [
            ("/all_results", json!([])),
            ("/committed_result/widget_id", json!(7)),
            ("/failed_outcome/code", json!("")),
            ("/failed_outcome/effect_outcome", json!("rolled-back")),
            ("/failed_outcome/extra", json!(true)),
        ] {
            let mut invalid = accepted.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            invalid
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.to_owned(), value);
            assert!(
                schemas.validate(&invalid, schema_index).is_err(),
                "{invalid}"
            );
        }
        let no_commit = json!({"failed_outcome": {"code": "write_failed"}});
        assert!(schemas.validate(&no_commit, schema_index).is_err());
        assert!(
            schemas
                .validate(
                    &json!({"committed_result": {"widget_id": "widget-1"}}),
                    schema_index
                )
                .is_err()
        );
    }

    #[test]
    fn an_edge_endpoint_must_name_a_declared_node() {
        let mut wiring = crud_wiring();
        wiring.edges[1].to = "missing".parse().unwrap();
        let wire = serde_json::to_value(&wiring).expect("serializes");

        assert_eq!(
            WiringDocument::parse(&wire).unwrap_err(),
            CatalogIdentityError::UnresolvedWiringNode {
                node_id: "missing".parse().unwrap()
            }
        );
    }

    /// The gap wamn-0h0g.18.5 closes: before it, `WiringNode` carried only
    /// `component`/`interface-version`/`operation`/`params` and the document
    /// carried no entry, so the terminal declaration had no authoring
    /// source and the graph index had no entry to be given. Both
    /// now survive the store round trip a resolved wiring actually takes:
    /// document -> `graph_json` bytes -> `WiringDocument::parse`.
    #[test]
    fn a_node_declares_its_terminal_and_the_document_declares_its_entry() {
        let mut wiring = crud_wiring();
        wiring.nodes.insert(
            "publish".parse().unwrap(),
            terminal_node(
                "bus",
                WiringTerminal::emit("orders".parse().unwrap(), Op::Insert),
            ),
        );
        wiring.edges.push(WiringEdge {
            from: "write".parse().unwrap(),
            from_port: crate::MAIN_PORT.parse().unwrap(),
            to: "publish".parse().unwrap(),
            to_port: None,
        });
        let stored = serde_json::to_value(&wiring).expect("serializes");

        // The two tokens are the router's own `Terminal` variants; a third
        // spelling here would be a second contract for the same decision.
        assert_eq!(stored["entry"], json!("in"));
        assert_eq!(stored["nodes"]["out"]["terminal"], json!("respond"));
        {
            let observed = &(stored["nodes"]["publish"]["terminal"]);
            assert_eq!(observed["emit"]["entity"], serde_json::json!("orders"));
            assert_eq!(observed["emit"]["operation"], serde_json::json!("insert"));
        };
        assert!(
            !stored["nodes"]["write"]
                .as_object()
                .expect("an object")
                .contains_key("terminal"),
            "an ordinary node carries no terminal key, so its stored bytes — and \
             the hash over them — are what they were before this field existed"
        );

        let read_back = WiringDocument::parse(&stored).expect("the stored document parses");
        assert_eq!(read_back.entry, "in");
        assert_eq!(
            read_back.nodes["out"].terminal,
            Some(WiringTerminal::Respond)
        );
        assert_eq!(
            read_back.nodes["publish"].terminal,
            Some(WiringTerminal::emit("orders".parse().unwrap(), Op::Insert))
        );
        assert_eq!(read_back.nodes["write"].terminal, None);
        assert_eq!(read_back, wiring);
    }

    /// An entry that names nothing is refused here rather than at
    /// the graph index, which would be one activation and one
    /// cache miss too late.
    #[test]
    fn the_entry_must_name_a_declared_node() {
        let mut wiring = crud_wiring();
        wiring.entry = "missing".parse().unwrap();
        let stored = serde_json::to_value(&wiring).expect("serializes");

        assert_eq!(
            WiringDocument::parse(&stored).unwrap_err(),
            CatalogIdentityError::UnresolvedWiringEntry {
                node_id: "missing".parse().unwrap()
            }
        );

        // The field is required at the WIRE, never defaulted and then rejected.
        // That is the whole backward-compatibility statement: a graph_json
        // written before wamn-0h0g.18.5 is refused as a *document*, so it can
        // never be read back as a wiring that happens to enter nowhere.
        let mut absent = serde_json::to_value(crud_wiring()).expect("serializes");
        absent
            .as_object_mut()
            .expect("an object")
            .remove("entry")
            .expect("the entry the document serialized");
        let _refusal =
            WiringDocument::parse(&absent).expect_err("an entry-less document is not a wiring");
    }

    #[test]
    fn the_wiring_hash_covers_the_graph_and_ignores_key_order() {
        let wiring = crud_wiring();
        let hash = wiring.wiring_hash();

        let mut permuted = serde_json::Map::new();
        let wire = serde_json::to_value(&wiring).expect("serializes");
        for key in wire.as_object().expect("an object").keys().rev() {
            permuted.insert(key.clone(), wire[key].clone());
        }
        assert_eq!(
            WiringDocument::parse(&Value::Object(permuted))
                .expect("parses")
                .wiring_hash(),
            hash,
            "canonical JSON identity cannot depend on document key order"
        );

        let mut reparameterized = wiring.clone();
        reparameterized
            .nodes
            .get_mut("write")
            .expect("the entity node")
            .params
            .insert("relation".parse().unwrap(), json!("orders"));
        assert_ne!(reparameterized.wiring_hash(), hash);

        // The terminal and the entry are conditioned definition content, not runtime
        // configuration: re-terminalling or re-entering a graph is a new
        // `catalog.workflows` row to condition and flip onto, never an edit to the
        // definition an environment already confirmed.
        let mut reterminaled = wiring.clone();
        reterminaled
            .nodes
            .get_mut("out")
            .expect("the respond node")
            .terminal = Some(WiringTerminal::emit("orders".parse().unwrap(), Op::Update));
        assert_ne!(reterminaled.wiring_hash(), hash);

        let mut reselected = reterminaled;
        reselected
            .nodes
            .get_mut("out")
            .expect("the emit node")
            .terminal = Some(WiringTerminal::emit(
            "shipments".parse().unwrap(),
            Op::Update,
        ));
        assert_ne!(reselected.wiring_hash(), hash);

        let mut reentered = wiring;
        reentered.entry = "write".parse().unwrap();
        assert_ne!(reentered.wiring_hash(), hash);
    }

    #[test]
    fn an_emit_terminal_requires_a_complete_exact_selector() {
        let base = serde_json::to_value(crud_wiring()).expect("serializes");
        for terminal in [
            json!("emit"),
            json!({"emit": {"operation": "insert"}}),
            json!({"emit": {"entity": "orders"}}),
            json!({"emit": {"entity": "orders", "operation": "truncate"}}),
            json!({"emit": {"entity": "", "operation": "insert"}}),
        ] {
            let mut stored = base.clone();
            stored["nodes"]["out"]["terminal"] = terminal.clone();
            assert!(
                WiringDocument::parse(&stored).is_err(),
                "partial or foreign selector admitted: {terminal}"
            );
        }
    }
}
