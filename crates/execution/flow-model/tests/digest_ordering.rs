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
            r#"{"allowed-hosts":["a.example.com","b.example.com"],"#,
            r#""credentials":[{"description":"Alpha credential","name":"alpha-key"},"#,
            r#"{"description":"Beta credential","name":"beta-key"}],"#,
            r#""edges":[{"from":"entry","to":"alpha"},{"from":"alpha","to":"zeta"},"#,
            r#"{"from":"zeta","to":"done"}],"#,
            r#""flow-id":"digest-ordering","name":"Digest ordering fixture","#,
            r#""nodes":[{"credential":"alpha-key","id":"alpha","label":"Alpha step","type":"custom"},"#,
            r#"{"config":{"status":200},"id":"done","type":"respond"},"#,
            r#"{"config":{"input-schema":true},"id":"entry","type":"request"},"#,
            r#"{"credential":"beta-key","id":"zeta","label":"Zeta step","type":"custom"}],"#,
            r#""schema-version":"0.1","version":3}"#,
        ),
        "node frames are ordered by node id"
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
     "connection": "erp-callback", "credential": "api-key"}
  ],
  "edges": [{"from": "entry", "from-port": "alt", "to": "call", "to-port": "in"}],
  "connection-requirements": [{"name":"erp-callback","requirement":{"requirement-version":"1","descriptor":{"descriptor-version":"1","requirement-type":"http","contract":"wamn:connection/http@0.1.0","authority-model":"http-origin","field-ownership":[{"field":"method","owner":"author"},{"field":"relative-target","owner":"author"},{"field":"headers","owner":"author"},{"field":"body","owner":"author"},{"field":"authority","owner":"environment"},{"field":"tls","owner":"environment"},{"field":"redirect","owner":"environment"},{"field":"proxy","owner":"environment"},{"field":"credential","owner":"environment"},{"field":"idempotency-key","owner":"system"}],"credential-injection":"environment-selected-http-header","idempotency-key-injection":"http-idempotency-key-header","conservative-recovery":"never-replay","recovery-claims":[{"claim":"stable-key-dedup-v1","parameters":["minimum-retention-ms"],"operation_fingerprint":["method","relative-target","semantic-headers","body-digest"]}]},"recovery":{"claim":"never-replay"}}}],
  "credentials": [{"name": "api-key", "kind": "api-key", "description": "The key"}],
  "allowed-hosts": ["a.example.com"],
  "partition-policy": "leapfrog",
  "ordering": {"mode": "partitioned", "partition-key": "id"},
  "capture": {"mode": "scrubbed", "max-bytes": 8}
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
/// identity and which are display, at the flow, node, and credential level.
#[test]
fn every_flow_field_is_classified_as_identity_or_display() {
    let flow = Flow::from_json(MAXIMAL_GRAPH).expect("maximal fixture parses");
    let document = serde_json::to_value(&flow).expect("the document serializes");
    let preimage = serde_json::to_value(FlowPreimage::of(&flow)).expect("the preimage serializes");

    assert_eq!(
        sorted_keys(&document),
        [
            "allowed-hosts",
            "capture",
            "connection-requirements",
            "credentials",
            "edges",
            "flow-id",
            "name",
            "nodes",
            "ordering",
            "partition-policy",
            "schema-version",
            "version",
        ],
        "a new `Flow` field must be classified as identity or display here"
    );
    assert_eq!(
        sorted_keys(&preimage),
        sorted_keys(&document),
        "no flow-level field is display yet"
    );
    assert_eq!(
        sorted_keys(&preimage["nodes"][0]),
        ["config", "connection", "credential", "id", "label", "type"],
        "no node-level field is display yet"
    );
    assert_eq!(
        sorted_keys(&preimage["credentials"][0]),
        ["description", "kind", "name"],
        "no credential-level field is display yet"
    );
}
