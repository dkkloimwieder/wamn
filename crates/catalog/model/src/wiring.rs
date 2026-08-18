//! The wiring document — the typed graph a tenant authors over the palette.
//!
//! A wiring is the composition artifact of the component-palette model
//! (`docs/exe-model.md`, R1/R3): nodes name `(component, interface-version)`
//! from the palette, edges connect declared ports, node parameters bind
//! declared params, and the in-draft `cases` array rides the document. It is
//! *data, not code* — versioned, gated against one applied catalog version, and
//! activated by a pointer flip — so its storage is
//! `catalog.wirings` / `catalog.wiring_activation` rather than an OCI artifact.
//!
//! # The cases array attaches here (wamn-0h0g.18.4)
//!
//! The ruling put the in-draft test contract on the *wiring*, not on a
//! component: a golden input plus a required observable is an end-to-end shape,
//! and the flow document's successor as the composition artifact is this
//! document. So [`WiringDocument::cases`] is [`wamn_flow::TestSetCase`] itself —
//! the same `case-id` / `input` / `expect` shape, the same
//! [`wamn_flow::MAX_TEST_SET_CASES`] bound, checked by the same
//! [`wamn_flow::validate_cases`]. A second declaration of that contract would be
//! a second contract; when `wamn-flow` retires (wamn-0h0g.26.5) the cases module
//! moves, and this carrier keeps working against the moved types.
//!
//! # Identity
//!
//! [`WiringDocument::wiring_hash`] is the SHA-256 of the document's RFC 8785
//! canonical JSON, taken through the workspace's single canonicalizer
//! ([`wamn_flow::canonical_json_sha256`]) exactly as
//! [`ServingManifest`](crate::ServingManifest) does. It is the value stored in
//! `catalog.wirings.wiring_hash` and confirmed by
//! `catalog.wiring_activation.confirmed_definition_hash`.
//!
//! Nodes are a `BTreeMap` keyed by node id, so map order cannot reach the
//! digest and a duplicate id is not a representable state. Edges are a `Vec`
//! because fan-out order is author-meaningful; since the digest is taken over
//! this document rather than a reordered preimage, array position carries that
//! order and no explicit ordinal is needed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CatalogIdentityError, DefinitionHash, validate_text};

/// The only wiring-document format shipped by the MVP.
pub const WIRING_DOCUMENT_FORMAT_VERSION: &str = "0.1";

/// One graph step: an operation of one palette component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringNode {
    /// The palette component this node runs. It is the instance-pool key —
    /// pools are per component, shared across every wiring using it — and half
    /// of the `(component, interface-version)` pair promotion checks the target
    /// library covers.
    pub component: String,
    /// The WIT interface version this node was wired against. The other half of
    /// the promotion pair, and what late binding holds within.
    pub interface_version: String,
    /// The operation the router invokes over the typed WIT seam once it holds a
    /// pooled instance (`entity.create` is this field's `create`).
    pub operation: String,
    /// Values bound to the component's declared params — a generic `entity`
    /// node's relation and filter spec, for instance. Typed by the component's
    /// WIT declaration and checked by the gate (wamn-0h0g.18.3), not by this
    /// crate: the palette is tenant data, so a shape written here would be a
    /// second, staler copy of it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// A wire from one node's output port to a downstream node's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringEdge {
    /// Source node id — a key of [`WiringDocument::nodes`].
    pub from: String,
    /// Source output port. Omitted means [`wamn_flow::MAIN_PORT`];
    /// [`wamn_flow::ERROR_PORT`] is the error path the walk routes failures
    /// along without ending the delivery.
    #[serde(default = "main_port", skip_serializing_if = "is_main_port")]
    pub from_port: String,
    /// Target node id — a key of [`WiringDocument::nodes`].
    pub to: String,
    /// Target input port. Absent means the component's single declared input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_port: Option<String>,
}

/// One version of a wiring: the document `catalog.wirings.graph_json` stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringDocument {
    /// The document format version. A foreign version fails closed at
    /// [`WiringDocument::parse`] rather than being partially understood.
    pub format_version: String,
    /// Stable identifier shared across every version of this wiring. It is the
    /// activation pointer's own key: `catalog.wiring_activation` is keyed by
    /// `(tenant, catalog, environment, wiring_id)`, which is what makes "exactly
    /// one enabled definition hash" structural.
    pub wiring_id: String,
    /// Monotonic version of this wiring — `catalog.wirings.version`, the row a
    /// rollback flips back to.
    pub version: u32,
    /// The graph's nodes, keyed by node id. The router resolves the active
    /// wiring and walks these in frontier order (wamn-0h0g.16.5).
    pub nodes: BTreeMap<String, WiringNode>,
    /// The wires between node ports, in fan-out order within each
    /// `(from, from-port)` group — the sequence the walk dispatches a branch's
    /// targets in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<WiringEdge>,
    /// The in-draft test cases (wamn-0h0g.18.4). Run by the gate against the
    /// real applied catalog at authoring and again at promote-time re-gate;
    /// `generate crud` writes them by construction.
    ///
    /// Bounded by [`wamn_flow::validate_cases`], and only when the document
    /// carries at least one: an absent and an empty array are the same bytes,
    /// which is the rule [`wamn_flow::Flow`] already applies to the same field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cases: Vec<wamn_flow::TestSetCase>,
}

impl WiringDocument {
    /// Build and validate one wiring document from its parts.
    ///
    /// Unlike [`ServingManifest::new`](crate::ServingManifest), this is not
    /// fenced behind `test-util`: authoring emits ordinary gated wirings —
    /// `generate crud` (wamn-0h0g.23.3) is a production producer — so a fence
    /// here would block the emitter rather than a misuse.
    pub fn new(
        wiring_id: impl Into<String>,
        version: u32,
        nodes: BTreeMap<String, WiringNode>,
        edges: Vec<WiringEdge>,
        cases: Vec<wamn_flow::TestSetCase>,
    ) -> Result<Self, CatalogIdentityError> {
        let document = Self {
            format_version: WIRING_DOCUMENT_FORMAT_VERSION.to_string(),
            wiring_id: wiring_id.into(),
            version,
            nodes,
            edges,
            cases,
        };
        document.validate()?;
        Ok(document)
    }

    /// Parse and validate a stored document — the `catalog.wirings.graph_json`
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
    pub fn wiring_hash(&self) -> DefinitionHash {
        DefinitionHash::parse(wamn_flow::canonical_json_sha256(&self.as_value()))
            .expect("the shared canonicalizer emits a canonical sha256 digest")
    }

    fn as_value(&self) -> Value {
        serde_json::to_value(self).expect("wiring document serializes")
    }

    fn validate(&self) -> Result<(), CatalogIdentityError> {
        if self.format_version != WIRING_DOCUMENT_FORMAT_VERSION {
            return invalid("wiring document format-version must be textual 0.1");
        }
        validate_text(&self.wiring_id, "wiring-id")?;
        if self.version == 0 {
            return Err(CatalogIdentityError::ZeroVersion { field: "version" });
        }
        if self.nodes.is_empty() {
            return invalid("a wiring with no nodes has nothing for the router to walk");
        }

        for (node_id, node) in &self.nodes {
            validate_text(node_id, "node-id")?;
            validate_text(&node.component, "component")?;
            validate_text(&node.interface_version, "interface-version")?;
            validate_text(&node.operation, "operation")?;
        }

        for edge in &self.edges {
            for endpoint in [&edge.from, &edge.to] {
                if !self.nodes.contains_key(endpoint) {
                    return Err(CatalogIdentityError::UnresolvedWiringNode {
                        node_id: endpoint.clone(),
                    });
                }
            }
            validate_text(&edge.from_port, "from-port")?;
            if let Some(to_port) = &edge.to_port {
                validate_text(to_port, "to-port")?;
            }
        }

        if !self.cases.is_empty() {
            wamn_flow::validate_cases(&self.cases).map_err(|error| {
                CatalogIdentityError::InvalidDefinition {
                    message: format!("wiring document cases are invalid: {error}"),
                }
            })?;
        }
        Ok(())
    }
}

fn invalid(message: &str) -> Result<(), CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.to_string(),
    })
}

fn main_port() -> String {
    wamn_flow::MAIN_PORT.to_string()
}

fn is_main_port(port: &str) -> bool {
    port == wamn_flow::MAIN_PORT
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};
    use wamn_flow::{Expect, ExpectedOutcome, MAX_TEST_SET_CASES, TestSetCase};

    use super::{WiringDocument, WiringEdge, WiringNode};
    use crate::CatalogIdentityError;

    fn node(component: &str) -> WiringNode {
        WiringNode {
            component: component.to_owned(),
            interface_version: "0.1.0".to_owned(),
            operation: "call".to_owned(),
            params: BTreeMap::new(),
        }
    }

    fn case(case_id: &str) -> TestSetCase {
        TestSetCase {
            case_id: case_id.to_owned(),
            input: json!({"value": 1}),
            expect: Expect {
                outcome: ExpectedOutcome::Responded,
                status: Some(200),
                body_subset: None,
                failure_code: None,
            },
        }
    }

    fn crud_wiring(cases: Vec<TestSetCase>) -> WiringDocument {
        let nodes = BTreeMap::from([
            ("in".to_owned(), node("http-entry")),
            ("write".to_owned(), node("entity")),
            ("out".to_owned(), node("respond")),
        ]);
        let edges = vec![
            WiringEdge {
                from: "in".to_owned(),
                from_port: wamn_flow::MAIN_PORT.to_owned(),
                to: "write".to_owned(),
                to_port: None,
            },
            WiringEdge {
                from: "write".to_owned(),
                from_port: wamn_flow::MAIN_PORT.to_owned(),
                to: "out".to_owned(),
                to_port: None,
            },
        ];
        WiringDocument::new("orders-create", 1, nodes, edges, cases)
            .expect("the generated crud shape is a valid wiring")
    }

    #[test]
    fn the_document_wire_shape_is_closed() {
        let wiring = crud_wiring(vec![case("roundtrip")]);
        let wire = serde_json::to_value(&wiring).expect("serializes");
        assert_eq!(
            wire["nodes"]["write"],
            json!({"component": "entity", "interface-version": "0.1.0", "operation": "call"})
        );
        // An omitted `from-port` is the main port, so the default never reaches
        // the wire and cannot fork the hash of one authored graph.
        assert_eq!(
            wire["edges"][0],
            json!({"from": "in", "to": "write"}),
            "main-port edges serialize without the port"
        );
        assert_eq!(wire["cases"][0]["case-id"], json!("roundtrip"));
        assert_eq!(
            WiringDocument::parse(&wire).expect("the exact document parses"),
            wiring
        );

        for foreign in [
            json!({"format-version": "0.2", "wiring-id": "w", "version": 1,
                   "nodes": {"a": {"component": "c", "interface-version": "0.1.0",
                                   "operation": "call"}}}),
            json!({"format-version": "0.1", "wiring-id": "w", "version": 1,
                   "nodes": {"a": {"component": "c", "interface-version": "0.1.0",
                                   "operation": "call"}},
                   "test-set": []}),
            json!({"format-version": "0.1", "wiring-id": "w", "version": 1, "nodes": {}}),
        ] {
            assert!(WiringDocument::parse(&foreign).is_err());
        }
    }

    #[test]
    fn an_edge_endpoint_must_name_a_declared_node() {
        let mut wiring = crud_wiring(Vec::new());
        wiring.edges[1].to = "missing".to_owned();
        let wire = serde_json::to_value(&wiring).expect("serializes");

        assert_eq!(
            WiringDocument::parse(&wire).unwrap_err(),
            CatalogIdentityError::UnresolvedWiringNode {
                node_id: "missing".to_owned()
            }
        );
    }

    #[test]
    fn the_cases_array_carries_the_flow_documents_bounds() {
        // The wiring is the carrier now (wamn-0h0g.18.4), so the bounds are
        // wamn-flow's own — not a second set that could drift from them. Each
        // refusal is reached by storing an out-of-bounds array and reading it
        // back, which is the path a hand-edited row takes.
        let refuse = |cases: Vec<TestSetCase>| {
            let mut stored = serde_json::to_value(crud_wiring(Vec::new())).expect("serializes");
            stored["cases"] = serde_json::to_value(cases).expect("cases serialize");
            WiringDocument::parse(&stored)
                .expect_err("out-of-bounds cases are refused")
                .to_string()
        };

        assert!(refuse(vec![case("same"), case("same")]).contains("duplicate case-id"));
        assert!(
            refuse(
                (0..=MAX_TEST_SET_CASES)
                    .map(|ordinal| case(&format!("case-{ordinal}")))
                    .collect()
            )
            .contains("maximum is 256")
        );

        // A document carrying no cases is a legal document, exactly as an
        // absent and an empty array are the same bytes.
        let none = crud_wiring(Vec::new());
        assert!(
            !serde_json::to_value(&none)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("cases")
        );
    }

    #[test]
    fn the_wiring_hash_covers_the_graph_and_ignores_key_order() {
        let wiring = crud_wiring(vec![case("roundtrip")]);
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
            .insert("relation".to_owned(), json!("orders"));
        assert_ne!(reparameterized.wiring_hash(), hash);

        let mut recased = wiring;
        recased.cases[0].expect.status = Some(201);
        assert_ne!(recased.wiring_hash(), hash);
    }
}
