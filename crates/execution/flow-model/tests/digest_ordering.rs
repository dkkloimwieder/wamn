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
use wamn_flow::{Flow, ResolvedInterfaces};

/// Nodes are deliberately listed out of node-id order and out of topological
/// order: document sequence is an authoring convenience, not an identity.
const GRAPH: &str = r#"{
  "schema-version": "0.1",
  "flow-id": "digest-ordering",
  "version": 3,
  "name": "Digest ordering fixture",
  "nodes": [
    {"id": "zeta", "type": "custom", "label": "Zeta step", "credential": "beta-key"},
    {"id": "entry", "type": "request", "config": {"input-schema": true}},
    {"id": "alpha", "type": "custom", "label": "Alpha step", "credential": "alpha-key"},
    {"id": "done", "type": "respond", "config": {"status": 200}}
  ],
  "edges": [
    {"from": "entry", "to": "alpha"},
    {"from": "alpha", "to": "zeta"},
    {"from": "zeta", "to": "done"}
  ],
  "credentials": [
    {"name": "alpha-key", "description": "Alpha credential"},
    {"name": "beta-key", "description": "Beta credential"}
  ],
  "allowed-hosts": ["a.example.com", "b.example.com"]
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

    let mut credentials = flow();
    credentials.credentials.swap(0, 1);
    assert!(codes(&credentials).contains(&"unsorted-credentials"));

    let mut hosts = flow();
    hosts.allowed_hosts.swap(0, 1);
    assert!(codes(&hosts).contains(&"unsorted-allowed-hosts"));

    // `connection-requirements` carries the same refusal; it is proved by
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

// FOLLOW-UP (child bead of wamn-jvzx, to file: "digest preimage must order flow
// nodes by node id"). `Flow::canonical_bytes` serializes `nodes` in document
// sequence, so permuting the array moves `graph_hash`, `artifact_hash`, and
// `draft_content_hash` without any semantic change. Node identity is `Node::id`
// and every consumer looks up by it (`wamn-flow-engine::plan`, `entry_node`,
// `diff`). The fix is to sort by node id at preimage build, which changes the
// digest of every already-persisted artifact — so it is filed, not applied here.
#[test]
#[ignore = "known W2 defect: node document sequence still enters the graph preimage"]
fn node_sequence_position_must_not_change_the_graph_digest() {
    let baseline = flow();
    let mut permuted = flow();
    permuted.nodes.swap(0, 2);

    assert!(codes(&permuted).is_empty(), "permutation stays valid");
    assert_eq!(baseline.graph_hash(), permuted.graph_hash());
}

// FOLLOW-UP (child bead of wamn-jvzx, to file: "flow edges need an explicit
// fan-out order field"). Edge sequence position is load-bearing at runtime —
// `wamn-flow-engine::plan::successors` documents "Order preserves the flow's
// edge order, so fan-out to several targets is deterministic" — but the schema
// carries no explicit order field, so author-meaningful order rides sequence
// position. W2 requires the explicit field first; only then can the preimage
// sort edges by their stable key. Both steps change persisted digests.
#[test]
#[ignore = "known W2 defect: edge fan-out order is sequence position, not an explicit order field"]
fn edge_sequence_position_must_not_change_the_graph_digest() {
    let baseline = flow();
    let mut permuted = flow();
    permuted.edges.swap(0, 2);

    assert_eq!(baseline.graph_hash(), permuted.graph_hash());
}

// FOLLOW-UP (child bead of wamn-jvzx, to file: "editor labels must leave the
// graph preimage"). `Flow::name`, `Node::label`, and `CredentialRef::description`
// are documented as editor display text yet are serialized into
// `Flow::canonical_bytes`, so renaming a node mints a new artifact identity.
// Excluding them changes every persisted digest, so it is filed, not applied.
#[test]
#[ignore = "known W2 defect: editor labels still enter the graph preimage"]
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
    for credential in &mut relabelled.credentials {
        credential.description = Some("renamed".to_string());
    }

    assert_eq!(baseline.graph_hash(), relabelled.graph_hash());
}
