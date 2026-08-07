use wamn_catalog::{
    Artifact, DraftArtifact, ExecutionBundleIdentity, ExecutionBundleInput,
    ExecutionBundlePackaging, ExecutionPlugManifest, NodeImplementation, PinnedDraftArtifact,
    StoredValidatedDraftContext, ValidatedDraftIdentity, ValidatedDraftIdentityInput,
};
use wamn_flow::Flow;
use wamn_node_manifest::{CapabilityClass, ExecutableRecoveryContract, ResolvedNodeInterface};

fn digest(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn flow(version: u32, status: u16) -> Flow {
    Flow::from_json(&format!(
        r#"{{
          "schema-version":"0.1","flow-id":"draft-flow","version":{version},
          "nodes":[
            {{"id":"request","type":"request","config":{{"input-schema":true}}}},
            {{"id":"respond","type":"respond","config":{{"status":{status}}}}}
          ],
          "edges":[{{"from":"request","to":"respond"}}]
        }}"#
    ))
    .expect("draft flow parses")
}

fn implementation(node_type: &str) -> NodeImplementation {
    NodeImplementation::platform(
        ResolvedNodeInterface::new(
            node_type,
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        ExecutableRecoveryContract::pure(),
    )
}

fn implementations() -> Vec<NodeImplementation> {
    vec![implementation("request"), implementation("respond")]
}

fn bundle(implementations: Vec<NodeImplementation>) -> ExecutionBundleIdentity {
    bundle_with_plug(implementations, 'c')
}

fn bundle_with_plug(
    implementations: Vec<NodeImplementation>,
    plug_digest: char,
) -> ExecutionBundleIdentity {
    ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::CapabilityClass,
        ExecutionBundleInput::new("flowrunner@fixture", digest('a')).unwrap(),
        ExecutionBundleInput::new("wac@fixture", digest('b')).unwrap(),
    )
    .implementations(implementations)
    .plugs(vec![
        ExecutionPlugManifest::new(
            "pure-standard-nodes",
            vec!["request".to_string(), "respond".to_string()],
            digest(plug_digest),
        )
        .expect("fixture plug manifest is valid"),
    ])
    .build()
    .expect("bundle identity is valid")
}

#[test]
fn execution_bundle_exposes_the_exact_runner_digest_for_instantiate_guarding() {
    let bundle = bundle(implementations());
    let runner = bundle.runner_input().expect("runner frame verifies");

    assert_eq!(runner.identity(), "flowrunner@fixture");
    assert_eq!(runner.digest(), digest('a'));
}

#[test]
fn draft_content_identity_excludes_release_version_but_not_behavior() {
    let v7 = DraftArtifact::new(
        "tenant-a",
        &flow(7, 200),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();
    let v8 = DraftArtifact::new(
        "tenant-a",
        &flow(8, 200),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();
    let changed = DraftArtifact::new(
        "tenant-a",
        &flow(8, 201),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();

    assert_eq!(v7.content_hash(), v8.content_hash());
    assert_ne!(v8.content_hash(), changed.content_hash());
    assert_ne!(
        v7.artifact().identity().artifact_hash(),
        v8.artifact().identity().artifact_hash(),
        "the published artifact identity remains versioned"
    );
}

#[test]
fn publication_must_reuse_the_tested_ordinary_artifact_version() {
    let draft = DraftArtifact::new(
        "tenant-a",
        &flow(8, 200),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();
    let publishable = Artifact::new("tenant-a", &flow(8, 200), implementations()).unwrap();
    let silently_reversioned = Artifact::new("tenant-a", &flow(9, 200), implementations()).unwrap();

    assert_eq!(
        draft.artifact().identity().artifact_hash(),
        publishable.identity().artifact_hash()
    );
    assert_ne!(
        draft.artifact().identity().artifact_hash(),
        silently_reversioned.identity().artifact_hash(),
        "publication must add release membership around the exact tested artifact"
    );
}

#[test]
fn validated_identity_rejects_version_and_document_provenance_aliases() {
    let v7 = DraftArtifact::new(
        "tenant-a",
        &flow(7, 200),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();
    let v8 = DraftArtifact::new(
        "tenant-a",
        &flow(8, 200),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();
    let base_artifact_hash = digest('d');
    let input = ValidatedDraftIdentityInput {
        tenant_id: "tenant-a",
        draft_id: "workspace-a",
        draft_revision: 3,
        flow_id: "draft-flow",
        runtime_flow_version: 7,
        draft_content_hash: v7.content_hash().as_str(),
        draft_artifact_hash: v7.artifact().identity().artifact_hash().as_str(),
        execution_bundle_hash: v7.execution_bundle().hash(),
        catalog_id: "catalog-a",
        catalog_version: 4,
        environment: "dev",
        suite_flow_version: 6,
        binding_base_artifact_hash: &base_artifact_hash,
    };
    let exact = ValidatedDraftIdentity::new(input).unwrap();
    let sibling = ValidatedDraftIdentity::new(ValidatedDraftIdentityInput {
        draft_id: "workspace-b",
        ..input
    })
    .unwrap();
    let later_revision = ValidatedDraftIdentity::new(ValidatedDraftIdentityInput {
        draft_revision: 4,
        ..input
    })
    .unwrap();
    let silently_reversioned = ValidatedDraftIdentity::new(ValidatedDraftIdentityInput {
        runtime_flow_version: 8,
        draft_content_hash: v8.content_hash().as_str(),
        draft_artifact_hash: v8.artifact().identity().artifact_hash().as_str(),
        execution_bundle_hash: v8.execution_bundle().hash(),
        ..input
    })
    .unwrap();

    assert_ne!(exact, sibling);
    assert_ne!(exact, later_revision);
    assert_ne!(exact, silently_reversioned);
}

#[test]
fn persisted_draft_reverifies_content_resolution_and_bundle() {
    let draft = DraftArtifact::new(
        "tenant-a",
        &flow(7, 200),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();
    let artifact = draft.artifact();
    let bundle_bytes = draft.execution_bundle().canonical_bytes().to_vec();
    let base_artifact_hash = digest('d');
    let validated_identity = ValidatedDraftIdentity::new(ValidatedDraftIdentityInput {
        tenant_id: "tenant-a",
        draft_id: "workspace-a",
        draft_revision: 3,
        flow_id: "draft-flow",
        runtime_flow_version: 7,
        draft_content_hash: draft.content_hash().as_str(),
        draft_artifact_hash: artifact.identity().artifact_hash().as_str(),
        execution_bundle_hash: draft.execution_bundle().hash(),
        catalog_id: "catalog-a",
        catalog_version: 4,
        environment: "dev",
        suite_flow_version: 6,
        binding_base_artifact_hash: &base_artifact_hash,
    })
    .unwrap();
    let context = StoredValidatedDraftContext {
        expected_identity_hash: validated_identity.as_str(),
        draft_id: "workspace-a",
        draft_revision: 3,
        catalog_id: "catalog-a",
        catalog_version: 4,
        environment: "dev",
        suite_flow_version: 6,
        binding_base_artifact_hash: &base_artifact_hash,
    };
    let pinned = PinnedDraftArtifact::from_storage(
        "tenant-a",
        "draft-flow",
        7,
        draft.content_hash().as_str(),
        &flow(7, 200).to_json(),
        artifact.graph_hash(),
        artifact.identity().artifact_hash().as_str(),
        &String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec()).unwrap(),
        artifact.interface_bundle().hash(),
        &serde_json::to_string(artifact.supplied_components()).unwrap(),
        Some(&String::from_utf8(artifact.occurrence_recovery_bytes().to_vec()).unwrap()),
        Some(artifact.occurrence_recovery_hash()),
        bundle_bytes.clone(),
        draft.execution_bundle().hash(),
        context,
    )
    .unwrap();

    assert_eq!(pinned.content_hash(), draft.content_hash());
    assert_eq!(
        pinned.execution_bundle().hash(),
        draft.execution_bundle().hash()
    );

    let wrong_draft = PinnedDraftArtifact::from_storage(
        "tenant-a",
        "draft-flow",
        7,
        &digest('f'),
        &flow(7, 200).to_json(),
        artifact.graph_hash(),
        artifact.identity().artifact_hash().as_str(),
        &String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec()).unwrap(),
        artifact.interface_bundle().hash(),
        &serde_json::to_string(artifact.supplied_components()).unwrap(),
        Some(&String::from_utf8(artifact.occurrence_recovery_bytes().to_vec()).unwrap()),
        Some(artifact.occurrence_recovery_hash()),
        bundle_bytes,
        draft.execution_bundle().hash(),
        context,
    );
    assert!(wrong_draft.is_err());
}

#[test]
fn a_valid_bundle_from_another_draft_cannot_be_transplanted() {
    let draft = DraftArtifact::new(
        "tenant-a",
        &flow(7, 200),
        implementations(),
        bundle(implementations()),
    )
    .unwrap();
    let other_bundle = bundle_with_plug(implementations(), 'e');
    let artifact = draft.artifact();
    let base_artifact_hash = digest('d');
    let validated_identity = ValidatedDraftIdentity::new(ValidatedDraftIdentityInput {
        tenant_id: "tenant-a",
        draft_id: "workspace-a",
        draft_revision: 3,
        flow_id: "draft-flow",
        runtime_flow_version: 7,
        draft_content_hash: draft.content_hash().as_str(),
        draft_artifact_hash: artifact.identity().artifact_hash().as_str(),
        execution_bundle_hash: draft.execution_bundle().hash(),
        catalog_id: "catalog-a",
        catalog_version: 4,
        environment: "dev",
        suite_flow_version: 6,
        binding_base_artifact_hash: &base_artifact_hash,
    })
    .unwrap();

    let transplanted = PinnedDraftArtifact::from_storage(
        "tenant-a",
        "draft-flow",
        7,
        draft.content_hash().as_str(),
        &flow(7, 200).to_json(),
        artifact.graph_hash(),
        artifact.identity().artifact_hash().as_str(),
        &String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec()).unwrap(),
        artifact.interface_bundle().hash(),
        &serde_json::to_string(artifact.supplied_components()).unwrap(),
        Some(&String::from_utf8(artifact.occurrence_recovery_bytes().to_vec()).unwrap()),
        Some(artifact.occurrence_recovery_hash()),
        other_bundle.canonical_bytes().to_vec(),
        other_bundle.hash(),
        StoredValidatedDraftContext {
            expected_identity_hash: validated_identity.as_str(),
            draft_id: "workspace-a",
            draft_revision: 3,
            catalog_id: "catalog-a",
            catalog_version: 4,
            environment: "dev",
            suite_flow_version: 6,
            binding_base_artifact_hash: &base_artifact_hash,
        },
    );

    assert!(matches!(
        transplanted,
        Err(wamn_catalog::CatalogIdentityError::ValidatedDraftIdentityMismatch)
    ));
}
