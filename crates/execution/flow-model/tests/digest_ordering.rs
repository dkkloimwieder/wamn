//! W2 digest-ordering proofs for the flow-graph preimage.
//!
//! The graph preimage is [`Flow::canonical_bytes`] (RFC 8785) and its digest is
//! [`Flow::graph_hash`]; `wamn-catalog` embeds those exact bytes as the `graph`
//! frame of the artifact, draft, and validated-draft identities.
//!
//! Object-key and whitespace permutation, and the explicit `version` ordinal
//! moving the digest, are proved by
//! `t0_canonical_graph_bytes_and_hash_ignore_json_key_order_and_whitespace` in
//! `tests/flows.rs` and by `canonical_json_hash_ignores_object_insertion_order`
//! in `src/lib.rs`. This file covers the remaining W2 clauses: collection
//! sequence, canvas layout, and display labels.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wamn_flow::{Flow, FlowPreimage, ResolvedInterfaces};

/// Nodes are deliberately listed out of node-id order and out of topological
/// order: document sequence is an authoring convenience, not an identity.
const GRAPH: &str = r#"{
  "schema-version": "0.1",
  "flow-id": "digest-ordering",
  "version": 3,
  "name": "Digest ordering fixture",
  "nodes": [
    {"id": "zeta", "type": "custom", "label": "Zeta step"},
    {"id": "entry", "type": "request", "config": {"input-schema": true}},
    {"id": "alpha", "type": "custom", "label": "Alpha step"},
    {"id": "done", "type": "respond", "config": {"status": 200}}
  ],
  "edges": [
    {"from": "entry", "to": "alpha"},
    {"from": "alpha", "to": "zeta"},
    {"from": "zeta", "to": "done"}
  ]
}"#;

fn flow() -> Flow {
    Flow::from_json(GRAPH).expect("fixture flow parses")
}

fn interfaces() -> ResolvedInterfaces {
    BTreeMap::from([("custom".to_string(), vec!["main".to_string()])])
}

fn codes(flow: &Flow) -> Vec<&'static str> {
    flow.issues(&interfaces())
        .into_iter()
        .map(|issue| issue.code)
        .collect()
}

#[test]
fn non_semantic_collection_permutation_is_refused_at_validation() {
    let baseline = flow();
    assert!(
        codes(&baseline).is_empty(),
        "{:?}",
        baseline.issues(&interfaces())
    );

    // `connection-requirements` carries the retained sorted-collection refusal;
    // it is proved by
    // `connection_references_are_declared_sorted_and_absent_from_control_nodes`
    // in `src/validate.rs`, which owns the portable-requirement fixture.
}

#[test]
fn canvas_coordinates_cannot_enter_the_graph_preimage() {
    let mut document: Value = serde_json::from_str(GRAPH).expect("fixture parses as JSON");
    document["nodes"][0]["position"] = json!({"x": 12, "y": 40});

    Flow::from_json(&document.to_string())
        .expect_err("`deny_unknown_fields` keeps canvas layout out of the graph document");
}

/// Node identity is `Node::id` and every consumer looks it up by that
/// (`wamn-flow-engine::plan`, `entry_node`, `diff`), so `FlowPreimage` orders
/// node frames by id: permuting the document array is not a new artifact
/// identity (wamn-jvzx.14).
#[test]
fn node_sequence_position_must_not_change_the_graph_digest() {
    let baseline = flow();
    let mut permuted = flow();
    permuted.nodes.swap(0, 2);

    assert!(codes(&permuted).is_empty(), "permutation stays valid");
    assert_eq!(baseline.graph_hash(), permuted.graph_hash());
}

/// Edge fan-out order is load-bearing at runtime (`plan::successors` feeds the
/// engine's frontier in that order), so it is carried by the explicit
/// `Edge::ordinal` and the preimage sorts edges by their stable key. Array
/// position no longer reaches the digest (wamn-jvzx.15).
#[test]
fn edge_sequence_position_must_not_change_the_graph_digest() {
    let baseline = flow();
    let mut permuted = flow();
    permuted.edges.swap(0, 2);

    assert_eq!(baseline.graph_hash(), permuted.graph_hash());
}

/// The other half of rule 2: an explicit order value *is* identity. Moving one
/// edge's `ordinal` within its group changes the digest, and `Flow::from_json`
/// is where an absent ordinal becomes the edge's position in its group — read
/// exactly once, at parse.
#[test]
fn explicit_edge_ordinals_are_materialized_at_parse_and_do_change_the_digest() {
    let fanned = Flow::from_json(
        r#"{
          "schema-version": "0.1",
          "flow-id": "fan-out",
          "version": 1,
          "nodes": [
            {"id": "entry", "type": "request", "config": {"input-schema": true}},
            {"id": "a", "type": "custom"},
            {"id": "b", "type": "custom"}
          ],
          "edges": [
            {"from": "entry", "to": "b"},
            {"from": "entry", "to": "a"}
          ]
        }"#,
    )
    .expect("fan-out fixture parses");

    assert_eq!(
        fanned
            .edges
            .iter()
            .map(|edge| edge.ordinal)
            .collect::<Vec<_>>(),
        [Some(0), Some(1)],
        "an absent ordinal becomes the edge's position within its group"
    );
    assert_eq!(
        Flow::from_json(&fanned.to_json())
            .expect("an exported flow re-parses")
            .edges,
        fanned.edges,
        "materialization is idempotent across an export/import round trip"
    );

    // The ordinal is part of the edge sort key, not only of the hashed frame:
    // this group's ordinal order (b, a) is deliberately the reverse of its
    // target-id order, so dropping the ordinal from the key would reorder the
    // frames here even though the frames themselves are unchanged.
    let preimage: Value =
        serde_json::from_slice(&fanned.canonical_bytes()).expect("the preimage is JSON");
    let targets: Vec<&str> = preimage["edges"]
        .as_array()
        .expect("the preimage carries edges")
        .iter()
        .map(|edge| edge["to"].as_str().expect("an edge target"))
        .collect();
    assert_eq!(
        targets,
        ["b", "a"],
        "edge frames follow the fan-out ordinal, not the target id"
    );

    let mut reordered = fanned.clone();
    reordered.edges[0].ordinal = Some(1);
    reordered.edges[1].ordinal = Some(0);
    assert_ne!(
        fanned.graph_hash(),
        reordered.graph_hash(),
        "changing an explicit order value changes the digest"
    );

    let mut repeated = fanned.clone();
    repeated.edges.push(repeated.edges[0].clone());
    assert!(
        codes(&repeated).contains(&"duplicate-edge"),
        "a repeated wire would make the edge sort key ambiguous"
    );
}

/// `Flow::name` and `Node::label` are editor display text with no reader
/// anywhere in the platform, so `FlowPreimage` omits both and relabelling a
/// graph does not mint a new artifact identity (wamn-jvzx.16). They stay in
/// the document.
#[test]
fn editor_labels_must_not_enter_the_graph_preimage() {
    let baseline = flow();
    let mut relabelled = flow();
    relabelled.name = Some("Renamed by the editor".to_string());
    for node in &mut relabelled.nodes {
        node.label = node
            .label
            .as_ref()
            .map(|label| format!("{label} (renamed)"));
    }

    assert_eq!(baseline.graph_hash(), relabelled.graph_hash());
    assert_ne!(
        baseline.to_json(),
        relabelled.to_json(),
        "the document still carries the display text it is not identified by"
    );
}

/// Drift guard: the exact RFC 8785 preimage bytes for `GRAPH`. Any silent change
/// to the projection — a field entering or leaving it, an ordering rule moving —
/// fails here with a readable diff. The digest of every already-persisted
/// artifact moves with these bytes; a pre-change row then fails closed in
/// `PinnedArtifact::from_storage` (`GraphHashMismatch`) and from-zero
/// reprovisioning is the migration story, per FLOW-SPEC's greenfield rule.
#[test]
fn graph_preimage_bytes_are_pinned() {
    assert_eq!(
        String::from_utf8(flow().canonical_bytes()).expect("the preimage is UTF-8"),
        concat!(
            r#"{"edges":[{"from":"alpha","ordinal":0,"to":"zeta"},"#,
            r#"{"from":"entry","ordinal":0,"to":"alpha"},"#,
            r#"{"from":"zeta","ordinal":0,"to":"done"}],"#,
            r#""flow-id":"digest-ordering","#,
            r#""nodes":[{"id":"alpha","type":"custom"},"#,
            r#"{"config":{"status":200},"id":"done","type":"respond"},"#,
            r#"{"config":{"input-schema":true},"id":"entry","type":"request"},"#,
            r#"{"id":"zeta","type":"custom"}],"#,
            r#""schema-version":"0.1","version":3}"#,
        ),
        "node frames are ordered by node id and display text is absent"
    );
}

/// A flow with every optional field populated, so serializing it skips nothing.
const MAXIMAL_GRAPH: &str = r#"{
  "schema-version": "0.1",
  "flow-id": "maximal",
  "version": 4,
  "name": "Maximal fixture",
  "nodes": [
    {"id": "entry", "type": "request", "label": "Entry", "config": {"input-schema": true}},
    {"id": "call", "type": "custom", "label": "Call", "config": {"k": 1},
     "connection": "erp-callback"}
  ],
  "edges": [{"from": "entry", "from-port": "alt", "to": "call", "to-port": "in"}],
  "connection-requirements": [{"name":"erp-callback","requirement":{"descriptor-version":"1","requirement-type":"http","contract":"wamn:connection/http@0.1.0","authority-model":"http-origin","field-ownership":[{"field":"method","owner":"author"},{"field":"relative-target","owner":"author"},{"field":"headers","owner":"author"},{"field":"body","owner":"author"},{"field":"authority","owner":"environment"},{"field":"tls","owner":"environment"},{"field":"redirect","owner":"environment"},{"field":"proxy","owner":"environment"},{"field":"credential","owner":"environment"}],"credential-injection":"environment-selected-http-header"}}]
}"#;

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// The classification guard behind the preimage view. `FlowPreimage` is a
/// hand-written projection, so a field added to `Flow` still compiles while
/// reaching no digest — this test is what makes that a decision rather than an
/// omission. `MAXIMAL_GRAPH` populates every optional field so nothing is
/// skipped, and the pinned key sets then state exactly which document fields are
/// identity and which are display, at the flow and node levels.
#[test]
fn every_flow_field_is_classified_as_identity_or_display() {
    let flow = Flow::from_json(MAXIMAL_GRAPH).expect("maximal fixture parses");
    let document = serde_json::to_value(&flow).expect("the document serializes");
    let preimage = serde_json::to_value(FlowPreimage::of(&flow)).expect("the preimage serializes");

    assert_eq!(
        sorted_keys(&document),
        [
            "connection-requirements",
            "edges",
            "flow-id",
            "name",
            "nodes",
            "schema-version",
            "version",
        ],
        "a new `Flow` field must be classified as identity or display here"
    );
    assert_eq!(
        sorted_keys(&preimage["edges"][0]),
        ["from", "from-port", "ordinal", "to", "to-port"],
        "no edge-level field is display yet"
    );
    let mut identity_keys = sorted_keys(&document);
    identity_keys.retain(|key| key != "name");
    assert_eq!(
        sorted_keys(&preimage),
        identity_keys,
        "`name` is the only display field at the flow level"
    );
    assert_eq!(
        sorted_keys(&preimage["nodes"][0]),
        ["config", "connection", "id", "type"],
        "`label` is the only display field at the node level"
    );
    assert_eq!(
        sorted_keys(&document["nodes"][0]),
        ["config", "id", "label", "type"],
        "the document keeps the display text the preimage drops"
    );
}

#[test]
fn retired_ordering_fields_are_refused_instead_of_compatibly_ignored() {
    let base: Value = serde_json::from_str(MAXIMAL_GRAPH).expect("maximal fixture parses as JSON");
    for (key, value) in [
        ("partition-policy", serde_json::json!("leapfrog")),
        (
            "ordering",
            serde_json::json!({"mode": "partitioned", "partition-key": "id"}),
        ),
    ] {
        let mut document = base.clone();
        document
            .as_object_mut()
            .expect("flow document is an object")
            .insert(key.to_string(), value);
        assert!(
            Flow::from_json(&document.to_string()).is_err(),
            "retired field {key} must fail closed; immutable old artifacts are reprovisioned"
        );
    }
}
