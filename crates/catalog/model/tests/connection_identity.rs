use wamn_catalog::{
    Artifact, ExecutionBundleIdentity, ExecutionBundleInput, ExecutionBundlePackaging,
    ExecutionPlugManifest, NodeImplementation,
};
use wamn_flow::Flow;
use wamn_node_manifest::{
    CapabilityClass, ConnectionRecoverySupport, ConnectionRequirement, ConnectionTypeDescriptor,
    ExecutableConnectionRecoveryMode, ExecutableRecoveryClaim, IdempotencyKeyInjection,
    PortableConnectionRequirement, RecoveryClass, ResolvedNodeContract, ResolvedNodeInterface,
    ResolvedPurity,
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
    let interface = http_interface(contract);
    let support = full_recovery_support(descriptor.clone());
    NodeImplementation::supplied(interface, digest('1'))?
        .with_connection_recovery_support(vec![support])?
        .with_portable_connections(vec![PortableConnectionRequirement::stable_key_dedup_v1(
            descriptor,
            minimum_retention_ms,
        )])
}

fn http_interface(contract: &str) -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
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
    )
}

fn full_recovery_support(descriptor: ConnectionTypeDescriptor) -> ConnectionRecoverySupport {
    ConnectionRecoverySupport {
        descriptor,
        supported_modes: vec![
            ExecutableConnectionRecoveryMode::NeverReplay,
            ExecutableConnectionRecoveryMode::IdempotentWithKey {
                claim: ExecutableRecoveryClaim::StableKeyDedupV1,
                key_propagation: IdempotencyKeyInjection::HttpIdempotencyKeyHeader,
            },
        ],
    }
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
fn standard_and_custom_resolutions_share_typed_recovery_support() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    let support = full_recovery_support(descriptor.clone());
    let requirement = PortableConnectionRequirement::stable_key_dedup_v1(descriptor, 86_400_000);

    let standard = NodeImplementation::platform(http_interface("wamn:connection/http@0.1.0"))
        .with_connection_recovery_support(vec![support.clone()])
        .unwrap()
        .with_portable_connections(vec![requirement.clone()])
        .unwrap();
    let custom =
        NodeImplementation::supplied(http_interface("wamn:connection/http@0.1.0"), digest('1'))
            .unwrap()
            .with_connection_recovery_support(vec![support])
            .unwrap()
            .with_portable_connections(vec![requirement])
            .unwrap();

    assert_eq!(
        standard.connection_recovery_support(),
        custom.connection_recovery_support()
    );
    assert_eq!(
        standard.portable_connections(),
        custom.portable_connections()
    );
}

#[test]
fn unsupported_claim_and_missing_key_propagation_fail_closed() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    let never_replay_only = ConnectionRecoverySupport {
        descriptor: descriptor.clone(),
        supported_modes: vec![ExecutableConnectionRecoveryMode::NeverReplay],
    };
    let error =
        NodeImplementation::supplied(http_interface("wamn:connection/http@0.1.0"), digest('1'))
            .unwrap()
            .with_connection_recovery_support(vec![never_replay_only])
            .unwrap()
            .with_portable_connections(vec![PortableConnectionRequirement::stable_key_dedup_v1(
                descriptor, 1,
            )])
            .expect_err("an executable must explicitly support the portable recovery claim");
    assert!(
        error
            .to_string()
            .contains("unsupported for exact connection contract")
    );

    let mut second_descriptor = ConnectionTypeDescriptor::http_v1();
    second_descriptor.contract = "wamn:connection/http@0.2.0".to_string();
    let mut multi_contract_interface = http_interface("wamn:connection/http@0.1.0");
    multi_contract_interface
        .connection_requirements
        .push(ConnectionRequirement {
            requirement_type: "http".to_string(),
            contract: second_descriptor.contract.clone(),
        });
    let error = NodeImplementation::supplied(multi_contract_interface, digest('1'))
        .unwrap()
        .with_connection_recovery_support(vec![full_recovery_support(
            ConnectionTypeDescriptor::http_v1(),
        )])
        .expect_err("every exact connection contract must have its own recovery declaration");
    assert!(
        error
            .to_string()
            .contains("has no executable recovery declaration")
    );

    let implementation = implementation("wamn:connection/http@0.1.0", 1).unwrap();
    let mut missing_key =
        serde_json::to_value(implementation.contract()).expect("contract serializes");
    missing_key["connection-recovery-support"][0]["supported-modes"][1]
        .as_object_mut()
        .expect("mode is an object")
        .remove("key-propagation");
    assert!(serde_json::from_value::<ResolvedNodeContract>(missing_key).is_err());

    let mut unknown_claim =
        serde_json::to_value(implementation.contract()).expect("contract serializes");
    unknown_claim["connection-recovery-support"][0]["supported-modes"][1]["claim"] =
        serde_json::Value::String("header-was-present".to_string());
    assert!(serde_json::from_value::<ResolvedNodeContract>(unknown_claim).is_err());
}

#[test]
fn recovery_support_is_complete_sorted_and_unique() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    let stable_only = ConnectionRecoverySupport {
        descriptor: descriptor.clone(),
        supported_modes: vec![ExecutableConnectionRecoveryMode::IdempotentWithKey {
            claim: ExecutableRecoveryClaim::StableKeyDedupV1,
            key_propagation: IdempotencyKeyInjection::HttpIdempotencyKeyHeader,
        }],
    };
    let error =
        NodeImplementation::supplied(http_interface("wamn:connection/http@0.1.0"), digest('1'))
            .unwrap()
            .with_connection_recovery_support(vec![stable_only])
            .expect_err("support must include the descriptor's conservative mode");
    assert!(error.to_string().contains("omit its conservative"));

    let mut reversed_modes = full_recovery_support(descriptor.clone());
    reversed_modes.supported_modes.reverse();
    let error =
        NodeImplementation::supplied(http_interface("wamn:connection/http@0.1.0"), digest('1'))
            .unwrap()
            .with_connection_recovery_support(vec![reversed_modes])
            .expect_err("mode order must be canonical");
    assert!(error.to_string().contains("sorted, and unique"));

    let support = full_recovery_support(descriptor);
    let error =
        NodeImplementation::supplied(http_interface("wamn:connection/http@0.1.0"), digest('1'))
            .unwrap()
            .with_connection_recovery_support(vec![support.clone(), support])
            .expect_err("duplicate connection recovery support must fail");
    assert!(error.to_string().contains("sorted and unique"));
}

#[test]
fn recovery_support_declaration_mutates_artifact_and_bundle_identity() {
    let descriptor = ConnectionTypeDescriptor::http_v1();
    let requirement = PortableConnectionRequirement::never_replay(descriptor.clone());
    let baseline_support = ConnectionRecoverySupport {
        descriptor: descriptor.clone(),
        supported_modes: vec![ExecutableConnectionRecoveryMode::NeverReplay],
    };
    let expanded_support = full_recovery_support(descriptor);
    let make_implementation = |support| {
        NodeImplementation::supplied(http_interface("wamn:connection/http@0.1.0"), digest('1'))
            .unwrap()
            .with_connection_recovery_support(vec![support])
            .unwrap()
            .with_portable_connections(vec![requirement.clone()])
            .unwrap()
    };
    let baseline = make_implementation(baseline_support);
    let declaration_mutant = make_implementation(expanded_support);

    assert_ne!(
        baseline.contract().identity_hash(),
        declaration_mutant.contract().identity_hash()
    );
    assert_ne!(
        Artifact::new("tenant-a", &flow(), vec![baseline.clone()])
            .unwrap()
            .identity()
            .artifact_hash(),
        Artifact::new("tenant-a", &flow(), vec![declaration_mutant.clone()])
            .unwrap()
            .identity()
            .artifact_hash()
    );
    assert_ne!(bundle(baseline).hash(), bundle(declaration_mutant).hash());
}

#[test]
fn recovery_support_bytes_exclude_environment_facts() {
    let implementation = implementation("wamn:connection/http@0.1.0", 1).unwrap();
    let bytes = implementation.contract().identity_bytes();
    for forbidden in [
        b"endpoint".as_slice(),
        b"environment-id".as_slice(),
        b"instance-generation".as_slice(),
        b"attestation".as_slice(),
        b"evidence".as_slice(),
    ] {
        assert!(
            !bytes
                .windows(forbidden.len())
                .any(|window| window == forbidden),
            "resolved recovery support contains environment fact {:?}",
            String::from_utf8_lossy(forbidden)
        );
    }

    for field in ["endpoint", "instance-generation", "attestation", "evidence"] {
        let mut value =
            serde_json::to_value(implementation.contract()).expect("contract serializes");
        value["connection-recovery-support"][0][field] =
            serde_json::Value::String("forbidden".to_string());
        assert!(
            serde_json::from_value::<ResolvedNodeContract>(value).is_err(),
            "resolved recovery support admitted environment field {field:?}"
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
        .with_connection_recovery_support(baseline.connection_recovery_support().to_vec())
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
        .with_connection_recovery_support(baseline.connection_recovery_support().to_vec())
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
        .with_connection_recovery_support(baseline.connection_recovery_support().to_vec())
        .unwrap()
        .with_portable_connections(vec![requirement.clone(), requirement])
        .expect_err("duplicate portable requirements must fail closed");
    assert!(duplicates.to_string().contains("sorted and unique"));
}

#[test]
fn recovery_support_must_be_declared_by_the_resolved_interface() {
    let baseline = implementation("wamn:connection/http@0.1.0", 1).unwrap();
    let mut interface = baseline.interface().clone();
    interface.connection_requirements.clear();
    let error = NodeImplementation::supplied(interface, digest('1'))
        .unwrap()
        .with_connection_recovery_support(baseline.connection_recovery_support().to_vec())
        .expect_err("recovery support without executable capability must fail");
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
    assert!(
        !bytes
            .windows(b"connection-recovery-support".len())
            .any(|window| window == b"connection-recovery-support")
    );
}
