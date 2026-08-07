//! W2 digest-ordering proofs for the catalog definition preimages.
//!
//! Every catalog identity is a domain-separated frame sequence, so frame order
//! is itself part of the preimage (`named_removal_and_reordering_mutants_change_
//! every_hash_frame` in `src/lib.rs`). These fixtures pin the complementary
//! claim: the repeated frames inside a preimage are ordered by the member's
//! stable id, and a caller that supplies another sequence is refused rather
//! than silently hashed.

use serde_json::json;
use wamn_catalog::{
    Artifact, Attachment, AttachmentDraft, AttachmentId, AttachmentKind, CanonicalJson,
    CatalogIdentityError, ExecutionBundleIdentity, ExecutionBundleInput, ExecutionBundlePackaging,
    ExecutionPlugManifest, InterfaceBundle, NodeImplementation, Release, ReleaseId, Source,
    SourceId, SourceKind,
};
use wamn_flow::Flow;
use wamn_node_manifest::{
    CapabilityClass, ExecutableRecoveryContract, ResolvedNodeInterface, ResolvedPurity,
};

/// Nodes are deliberately listed out of node-id order: document sequence is an
/// authoring convenience, not an identity.
const GRAPH: &str = r#"{
  "schema-version": "0.1",
  "flow-id": "digest-ordering",
  "version": 2,
  "nodes": [
    {"id": "zeta", "type": "custom-node", "config": {"template": "v1"}},
    {"id": "entry", "type": "request", "config": {"input-schema": true}},
    {"id": "alpha", "type": "custom-node", "config": {"template": "v1"}},
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

fn digest_with(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn interface(node_type: &str, purity: ResolvedPurity) -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
        node_type,
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        if purity == ResolvedPurity::Pure {
            vec![CapabilityClass::Pure]
        } else {
            vec![CapabilityClass::Http]
        },
        Vec::new(),
    )
}

fn platform(node_type: &str) -> NodeImplementation {
    NodeImplementation::platform(
        interface(node_type, ResolvedPurity::Pure),
        ExecutableRecoveryContract::pure(),
    )
}

fn supplied(node_type: &str, digit: char) -> NodeImplementation {
    NodeImplementation::supplied(
        interface(node_type, ResolvedPurity::Effectful),
        digest_with(digit),
        ExecutableRecoveryContract::effectful(false),
    )
    .expect("fixture component digest is valid")
}

/// Sorted by node type, as `validate_interface_order` requires.
fn implementations() -> Vec<NodeImplementation> {
    vec![
        supplied("custom-node", '1'),
        platform("request"),
        platform("respond"),
    ]
}

fn artifact_of(flow: &Flow) -> Artifact {
    Artifact::new("tenant-a", flow, implementations()).expect("fixture artifact is valid")
}

fn source(id: &str) -> Source {
    Source::new(
        SourceId::new(id).expect("fixture source id is valid"),
        SourceKind::Auth,
        CanonicalJson::new(json!({"kind": "bearer"})).expect("fixture definition is canonical"),
    )
}

fn attachment_of(
    artifact: &Artifact,
    source_ids: Vec<SourceId>,
    sources: &[Source],
    definition: serde_json::Value,
) -> Result<Attachment, CatalogIdentityError> {
    Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new("expose").expect("fixture attachment id is valid"),
            kind: AttachmentKind::Http,
            artifact_id: artifact.identity().id().clone(),
            source_ids,
            definition: CanonicalJson::new(definition).expect("fixture definition is canonical"),
        },
        artifact,
        sources,
    )
}

#[test]
fn member_collections_refuse_a_non_canonical_sequence() {
    let mut reversed_interfaces = implementations();
    reversed_interfaces.reverse();
    assert!(matches!(
        InterfaceBundle::new(reversed_interfaces),
        Err(CatalogIdentityError::NonCanonicalInterfaceOrder { .. })
    ));

    let artifact = artifact_of(&flow());
    let sources = vec![source("auth-a"), source("auth-b")];
    let reversed_ids = vec![sources[1].id().clone(), sources[0].id().clone()];
    assert!(matches!(
        attachment_of(
            &artifact,
            reversed_ids,
            &sources,
            json!({"route": "/orders"})
        ),
        Err(CatalogIdentityError::NonCanonicalMemberOrder {
            field: "source-ids",
            ..
        })
    ));

    let sorted_ids = sources.iter().map(|source| source.id().clone()).collect();
    let attachment = attachment_of(&artifact, sorted_ids, &sources, json!({"route": "/orders"}))
        .expect("sorted source ids resolve");
    let mut reversed_sources = sources.clone();
    reversed_sources.reverse();
    assert!(matches!(
        Release::new(
            ReleaseId::new("tenant-a", "catalog-a", 1).expect("fixture release id is valid"),
            vec![artifact.identity().clone()],
            reversed_sources,
            vec![attachment],
        ),
        Err(CatalogIdentityError::NonCanonicalMemberOrder {
            field: "sources",
            ..
        })
    ));

    let bundle = ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::ExactNode,
        ExecutionBundleInput::new("runner@1", digest_with('a')).expect("fixture input is valid"),
        ExecutionBundleInput::new("wac@0.9.0", digest_with('b')).expect("fixture input is valid"),
    )
    .implementations(vec![
        supplied("alpha-node", '1'),
        supplied("beta-node", '2'),
    ])
    .plugs(vec![
        plug("beta-plug", "beta-node", 'd'),
        plug("alpha-plug", "alpha-node", 'c'),
    ])
    .build();
    assert!(matches!(
        bundle,
        Err(CatalogIdentityError::InvalidDefinition { ref message })
            if message == "execution-plug values must be sorted and unique"
    ));
}

fn plug(identity: &str, node_type: &str, digit: char) -> ExecutionPlugManifest {
    ExecutionPlugManifest::new(identity, vec![node_type.to_string()], digest_with(digit))
        .expect("fixture plug manifest is valid")
}

#[test]
fn definition_key_order_and_whitespace_never_reach_a_definition_hash() {
    let artifact = artifact_of(&flow());
    let baseline = attachment_of(
        &artifact,
        Vec::new(),
        &[],
        json!({"route": "/orders", "method": "POST", "nested": {"b": 2, "a": 1}}),
    )
    .expect("baseline attachment resolves");
    let permuted = attachment_of(
        &artifact,
        Vec::new(),
        &[],
        json!({"nested": {"a": 1, "b": 2}, "method": "POST", "route": "/orders"}),
    )
    .expect("key-permuted attachment resolves");

    assert_eq!(baseline.definition_hash(), permuted.definition_hash());

    // Whitespace cannot reach a digest at all: persisted definitions must
    // already be RFC 8785 canonical.
    assert!(matches!(
        CanonicalJson::parse("{ \"route\": \"/orders\" }"),
        Err(CatalogIdentityError::NonCanonicalJson)
    ));
}

#[test]
fn occurrence_recovery_frames_follow_node_id_not_document_sequence() {
    let baseline = artifact_of(&flow());
    let node_ids: Vec<&str> = baseline
        .occurrence_recovery()
        .iter()
        .map(|selection| selection.node_id.as_str())
        .collect();
    assert_eq!(node_ids, ["alpha", "done", "entry", "zeta"]);

    let mut permuted_flow = flow();
    permuted_flow.nodes.swap(0, 2);
    let permuted = artifact_of(&permuted_flow);

    assert_eq!(
        baseline.occurrence_recovery_hash(),
        permuted.occurrence_recovery_hash()
    );
}

// FOLLOW-UP (child bead of wamn-jvzx, to file: "digest preimage must order flow
// nodes by node id"). The artifact preimage's `graph` frame is
// `Flow::canonical_bytes`, which serializes `nodes` in document sequence, so a
// pure reordering mints a different `artifact_hash`. The sibling fixture
// `node_sequence_position_must_not_change_the_graph_digest` in `wamn-flow` owns
// the source-level defect; this one pins the persisted digest that inherits it.
#[test]
#[ignore = "known W2 defect: node document sequence still enters the artifact preimage"]
fn artifact_hash_must_not_depend_on_node_document_sequence() {
    let baseline = artifact_of(&flow());
    let mut permuted_flow = flow();
    permuted_flow.nodes.swap(0, 2);
    let permuted = artifact_of(&permuted_flow);

    assert_eq!(
        baseline.identity().artifact_hash(),
        permuted.identity().artifact_hash()
    );
}
