use wamn_catalog::{
    Artifact, ExecutionBundleIdentity, ExecutionBundleInput, ExecutionBundlePackaging,
    ExecutionPlugManifest, NodeImplementation,
};
use wamn_flow::Flow;
use wamn_node_manifest::{
    CapabilityClass, ConnectionRequirement, ConnectionTypeDescriptor,
    PortableConnectionRequirement, RecoveryClass, ResolvedNodeInterface, ResolvedPurity,
};

fn digest(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn flow() -> Flow {
    Flow::from_json(
        r#"{
          "schema-version":"0.1",
          "flow-id":"connection-identity",
          "version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":true}},
            {"id":"call","type":"http-node"},
            {"id":"response","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"call"},
            {"from":"call","to":"response"}
          ]
        }"#,
    )
    .expect("fixture flow parses")
}

fn implementation(
    contract: &str,
    minimum_retention_ms: u64,
) -> Result<NodeImplementation, wamn_catalog::CatalogIdentityError> {
    let mut descriptor = ConnectionTypeDescriptor::http_v1();
    descriptor.contract = contract.to_string();
    let interface = ResolvedNodeInterface::new(
        "http-node",
        "wamn:node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Http],
        vec![ConnectionRequirement {
            requirement_type: "http".to_string(),
            contract: contract.to_string(),
        }],
        ResolvedPurity::Effectful,
        RecoveryClass::NeverReplay,
    );
    NodeImplementation::supplied(interface, digest('1'))?.with_portable_connections(vec![
        PortableConnectionRequirement::stable_key_dedup_v1(descriptor, minimum_retention_ms),
    ])
}

fn bundle(implementation: NodeImplementation) -> ExecutionBundleIdentity {
    ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::ExactNode,
        ExecutionBundleInput::new("runner@1", digest('2')).unwrap(),
        ExecutionBundleInput::new("wac@0.9.0", digest('3')).unwrap(),
    )
    .implementations(vec![implementation])
    .plugs(vec![
        ExecutionPlugManifest::new("http-node", vec!["http-node".to_string()], digest('4'))
            .unwrap(),
    ])
    .build()
    .expect("execution bundle builds")
}

#[test]
fn descriptor_and_claim_mutations_invalidate_every_consuming_identity() {
    let baseline = implementation("wamn:connection/http@0.1.0", 86_400_000).unwrap();
    let descriptor_mutant = implementation("wamn:connection/http@0.2.0", 86_400_000).unwrap();
    let claim_mutant = implementation("wamn:connection/http@0.1.0", 172_800_000).unwrap();

    let baseline_node_hash = baseline.contract().identity_hash();
    let baseline_artifact = Artifact::new("tenant-a", &flow(), vec![baseline.clone()]).unwrap();
    let baseline_bundle = bundle(baseline);

    for (name, mutant) in [
        ("descriptor-contract", descriptor_mutant),
        ("portable-claim-parameter", claim_mutant),
    ] {
        assert_ne!(
            baseline_node_hash,
            mutant.contract().identity_hash(),
            "{name} survived resolved-node identity"
        );
        let artifact = Artifact::new("tenant-a", &flow(), vec![mutant.clone()]).unwrap();
        assert_ne!(
            baseline_artifact.identity().artifact_hash(),
            artifact.identity().artifact_hash(),
            "{name} survived artifact identity"
        );
        assert_ne!(
            baseline_bundle.hash(),
            bundle(mutant).hash(),
            "{name} survived execution-bundle identity"
        );
    }
}

#[test]
fn unsupported_or_noncanonical_portable_contracts_are_rejected() {
    let baseline = implementation("wamn:connection/http@0.1.0", 1).unwrap();

    let mut unknown_descriptor = baseline.portable_connections()[0].clone();
    unknown_descriptor.descriptor.descriptor_version = "2".to_string();
    let error = NodeImplementation::supplied(baseline.interface().clone(), digest('1'))
        .unwrap()
        .with_portable_connections(vec![unknown_descriptor])
        .expect_err("unknown descriptor version must fail closed");
    assert!(
        error
            .to_string()
            .contains("unsupported connection descriptor version")
    );

    let mut unknown_requirement = baseline.portable_connections()[0].clone();
    unknown_requirement.requirement_version = "2".to_string();
    let error = NodeImplementation::supplied(baseline.interface().clone(), digest('1'))
        .unwrap()
        .with_portable_connections(vec![unknown_requirement])
        .expect_err("unknown requirement version must fail closed");
    assert!(
        error
            .to_string()
            .contains("unsupported portable connection requirement version")
    );

    let zero_retention = implementation("wamn:connection/http@0.1.0", 0)
        .expect_err("zero retention must fail closed");
    assert!(zero_retention.to_string().contains("must be positive"));

    let requirement = baseline.portable_connections()[0].clone();
    let duplicates = NodeImplementation::supplied(baseline.interface().clone(), digest('1'))
        .unwrap()
        .with_portable_connections(vec![requirement.clone(), requirement])
        .expect_err("duplicate portable requirements must fail closed");
    assert!(duplicates.to_string().contains("sorted and unique"));
}

#[test]
fn portable_contract_must_be_declared_by_the_resolved_interface() {
    let baseline = implementation("wamn:connection/http@0.1.0", 1).unwrap();
    let mut interface = baseline.interface().clone();
    interface.connection_requirements.clear();
    let error = NodeImplementation::supplied(interface, digest('1'))
        .unwrap()
        .with_portable_connections(baseline.portable_connections().to_vec())
        .expect_err("portable connection without executable capability must fail");
    assert!(
        error
            .to_string()
            .contains("absent from the resolved interface")
    );
}

#[test]
fn empty_portable_model_preserves_the_existing_resolved_contract_shape() {
    let interface = ResolvedNodeInterface::new(
        "pure-node",
        "wamn:node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Pure],
        Vec::new(),
        ResolvedPurity::Pure,
        RecoveryClass::Replay,
    );
    let implementation = NodeImplementation::supplied(interface, digest('1')).unwrap();
    let bytes = implementation.contract().identity_bytes();
    assert!(
        !bytes
            .windows(b"portable-connections".len())
            .any(|window| window == b"portable-connections")
    );
}
