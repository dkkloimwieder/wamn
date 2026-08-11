use sha2::{Digest as _, Sha256};
use wamn_catalog::{
    Artifact, ExecutionBundleIdentity, ExecutionBundleInput, ExecutionBundlePackaging,
    ExecutionPlugManifest, NodeImplementation, PinnedArtifact,
};
use wamn_flow::{Flow, FlowConnectionRequirement};
use wamn_node_manifest::{
    CapabilityClass, ConnectionRecoverySupport, ConnectionRequirement, ConnectionTypeDescriptor,
    ExecutableConnectionRecoveryMode, ExecutableRecoveryClaim, ExecutableRecoveryContract,
    IdempotencyKeyInjection, OccurrenceRecoverySelection, PortableConnectionRequirement,
    PortableRecoveryClaim, RecoveryClass, ResolvedComponent, ResolvedNodeContract,
    ResolvedNodeInterface,
};

fn digest(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn digest_bytes(bytes: &[u8]) -> String {
    let value = Sha256::digest(bytes);
    let hex = value
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

fn artifact_new(
    tenant: &str,
    flow: &Flow,
    mut implementations: Vec<NodeImplementation>,
) -> Result<Artifact, wamn_catalog::CatalogIdentityError> {
    let request = NodeImplementation::platform(
        ResolvedNodeInterface::new(
            "request",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        ExecutableRecoveryContract::pure(),
    );
    let respond = NodeImplementation::platform(
        ResolvedNodeInterface::new(
            "respond",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        ExecutableRecoveryContract::pure(),
    );
    for implementation in [request, respond] {
        let index = implementations
            .iter()
            .position(|candidate| {
                candidate.interface().node_type > implementation.interface().node_type
            })
            .unwrap_or(implementations.len());
        implementations.insert(index, implementation);
    }
    Artifact::new(tenant, flow, implementations)
}

fn artifact_with_selections(
    tenant: &str,
    flow: &Flow,
    mut implementations: Vec<NodeImplementation>,
    mut selections: Vec<OccurrenceRecoverySelection>,
) -> Result<Artifact, wamn_catalog::CatalogIdentityError> {
    let request = NodeImplementation::platform(
        ResolvedNodeInterface::new(
            "request",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        ExecutableRecoveryContract::pure(),
    );
    let respond = NodeImplementation::platform(
        ResolvedNodeInterface::new(
            "respond",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        ExecutableRecoveryContract::pure(),
    );
    if !selections
        .iter()
        .any(|selection| selection.node_id == "request")
    {
        let index = selections
            .iter()
            .position(|selection| selection.node_id.as_str() > "request")
            .unwrap_or(selections.len());
        selections.insert(
            index,
            OccurrenceRecoverySelection::conservative("request", "request", request.contract()),
        );
    }
    if !selections
        .iter()
        .any(|selection| selection.node_id == "response")
    {
        selections.push(OccurrenceRecoverySelection::conservative(
            "response",
            "respond",
            respond.contract(),
        ));
    }
    for implementation in [request, respond] {
        let index = implementations
            .iter()
            .position(|candidate| {
                candidate.interface().node_type > implementation.interface().node_type
            })
            .unwrap_or(implementations.len());
        implementations.insert(index, implementation);
    }
    Artifact::new_with_recovery_selections(tenant, flow, implementations, selections)
}

fn respond_selection() -> OccurrenceRecoverySelection {
    OccurrenceRecoverySelection {
        selection_version: "1".to_string(),
        node_id: "response".to_string(),
        node_type: "respond".to_string(),
        recovery_class: RecoveryClass::Replay,
        portable_connection: None,
    }
}

fn request_selection() -> OccurrenceRecoverySelection {
    OccurrenceRecoverySelection {
        selection_version: "1".to_string(),
        node_id: "request".to_string(),
        node_type: "request".to_string(),
        recovery_class: RecoveryClass::Replay,
        portable_connection: None,
    }
}

fn historical_v1_artifact_hash(flow: &Flow, contract: &str) -> String {
    fn write_frame(output: &mut Vec<u8>, value: &[u8]) {
        output.extend_from_slice(&(value.len() as u64).to_be_bytes());
        output.extend_from_slice(value);
    }

    let mut bytes = Vec::new();
    write_frame(&mut bytes, b"wamn.catalog.identity.v1");
    write_frame(&mut bytes, b"artifact");
    for (tag, value) in [
        (
            b"artifact-id".as_slice(),
            br#"{"flow-id":"connection-identity","flow-version":1,"tenant-id":"tenant-a"}"#
                .as_slice(),
        ),
        (b"schema-version".as_slice(), b"0.1".as_slice()),
        (b"graph".as_slice(), flow.canonical_bytes().as_slice()),
        (b"resolved-node".as_slice(), contract.as_bytes()),
    ] {
        write_frame(&mut bytes, tag);
        write_frame(&mut bytes, value);
    }
    digest_bytes(&bytes)
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
    NodeImplementation::supplied(
        interface,
        digest('1'),
        ExecutableRecoveryContract::effectful(true),
    )?
    .with_connection_recovery_support(vec![support])?
    .with_portable_connections(vec![PortableConnectionRequirement::stable_key_dedup_v1(
        descriptor,
        minimum_retention_ms,
    )])
}

fn repeated_flow() -> Flow {
    let mut flow = Flow::from_json(
        r#"{
          "schema-version":"0.1",
          "flow-id":"repeated-connection-identity",
          "version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":true}},
            {"id":"call-b","type":"http-node"},
            {"id":"call-a","type":"http-node"},
            {"id":"response","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"call-a"},
            {"from":"call-a","to":"call-b"},
            {"from":"call-b","to":"response"}
          ]
        }"#,
    )
    .expect("fixture flow parses");
    let requirement = PortableConnectionRequirement::stable_key_dedup_v1(
        ConnectionTypeDescriptor::http_v1(),
        86_400_000,
    );
    flow.connection_requirements = vec![FlowConnectionRequirement {
        name: "manager".to_string(),
        requirement,
    }];
    flow.nodes
        .iter_mut()
        .find(|node| node.id == "call-a")
        .expect("fixture call-a exists")
        .connection = Some("manager".to_string());
    flow
}

fn http_interface(contract: &str) -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
        "http-node",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Http],
        vec![ConnectionRequirement {
            requirement_type: "http".to_string(),
            contract: contract.to_string(),
        }],
    )
}

fn supplied_effectful(
    interface: ResolvedNodeInterface,
) -> Result<NodeImplementation, wamn_catalog::CatalogIdentityError> {
    NodeImplementation::supplied(
        interface,
        digest('1'),
        ExecutableRecoveryContract::effectful(true),
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
    let baseline_artifact = artifact_new("tenant-a", &flow(), vec![baseline.clone()]).unwrap();
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
        let artifact = artifact_new("tenant-a", &flow(), vec![mutant.clone()]).unwrap();
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

    let standard = NodeImplementation::platform(
        http_interface("wamn:connection/http@0.1.0"),
        ExecutableRecoveryContract::effectful(true),
    )
    .with_connection_recovery_support(vec![support.clone()])
    .unwrap()
    .with_portable_connections(vec![requirement.clone()])
    .unwrap();
    let custom = supplied_effectful(http_interface("wamn:connection/http@0.1.0"))
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
    let error = supplied_effectful(http_interface("wamn:connection/http@0.1.0"))
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
    let error = supplied_effectful(multi_contract_interface)
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
    let error = supplied_effectful(http_interface("wamn:connection/http@0.1.0"))
        .unwrap()
        .with_connection_recovery_support(vec![stable_only])
        .expect_err("support must include the descriptor's conservative mode");
    assert!(error.to_string().contains("omit its conservative"));

    let mut reversed_modes = full_recovery_support(descriptor.clone());
    reversed_modes.supported_modes.reverse();
    let error = supplied_effectful(http_interface("wamn:connection/http@0.1.0"))
        .unwrap()
        .with_connection_recovery_support(vec![reversed_modes])
        .expect_err("mode order must be canonical");
    assert!(error.to_string().contains("sorted, and unique"));

    let support = full_recovery_support(descriptor);
    let error = supplied_effectful(http_interface("wamn:connection/http@0.1.0"))
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
        supplied_effectful(http_interface("wamn:connection/http@0.1.0"))
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
        artifact_new("tenant-a", &flow(), vec![baseline.clone()])
            .unwrap()
            .identity()
            .artifact_hash(),
        artifact_new("tenant-a", &flow(), vec![declaration_mutant.clone()])
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
    let error = supplied_effectful(baseline.interface().clone())
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
    let error = supplied_effectful(baseline.interface().clone())
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
    let duplicates = supplied_effectful(baseline.interface().clone())
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
    let error = supplied_effectful(interface)
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
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Pure],
        Vec::new(),
    );
    let implementation =
        NodeImplementation::supplied(interface, digest('1'), ExecutableRecoveryContract::pure())
            .unwrap();
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

#[test]
fn conservative_only_connection_requirement_needs_no_stronger_claim_descriptor() {
    let interface = ResolvedNodeInterface::new(
        "http-node",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Postgres],
        vec![ConnectionRequirement {
            requirement_type: "postgres".to_string(),
            contract: "wamn:connection/postgres@0.1.0".to_string(),
        }],
    );
    let implementation = NodeImplementation::supplied(
        interface,
        digest('1'),
        ExecutableRecoveryContract::effectful(false),
    )
    .unwrap();
    artifact_new("tenant-a", &flow(), vec![implementation])
        .expect("an exact WIT requirement may remain conservative without stronger claims");
}

#[test]
fn ordered_occurrence_selections_pin_connection_recovery_for_exact_occurrence() {
    let implementation = implementation("wamn:connection/http@0.1.0", 86_400_000).unwrap();
    let portable = implementation.portable_connections()[0].clone();
    let selections = vec![
        OccurrenceRecoverySelection {
            selection_version: "1".to_string(),
            node_id: "call-a".to_string(),
            node_type: "http-node".to_string(),
            recovery_class: RecoveryClass::IdempotentWithKey,
            portable_connection: Some(portable),
        },
        OccurrenceRecoverySelection {
            selection_version: "1".to_string(),
            node_id: "call-b".to_string(),
            node_type: "http-node".to_string(),
            recovery_class: RecoveryClass::NeverReplay,
            portable_connection: None,
        },
        request_selection(),
        respond_selection(),
    ];
    let selected = artifact_with_selections(
        "tenant-a",
        &repeated_flow(),
        vec![implementation.clone()],
        selections.clone(),
    )
    .unwrap();
    assert_eq!(selected.occurrence_recovery(), selections);
    assert_eq!(
        selected.identity().artifact_hash().as_str(),
        "sha256:01f6b56272f3f9c5a7b660759e3271b8886c703bc6421517838341f87f6167d0",
        "occurrence recovery frame sequence changed"
    );

    let mut claim_mutant = selections;
    let Some(requirement) = claim_mutant[0].portable_connection.as_mut() else {
        panic!("fixture carries a portable claim");
    };
    let PortableRecoveryClaim::StableKeyDedupV1 {
        minimum_retention_ms,
    } = &mut requirement.recovery
    else {
        panic!("fixture carries stable-key recovery");
    };
    *minimum_retention_ms += 1;
    assert!(
        artifact_with_selections(
            "tenant-a",
            &repeated_flow(),
            vec![implementation],
            claim_mutant,
        )
        .is_err(),
        "an unpinned claim mutation must fail closed"
    );
}

#[test]
fn conservative_selection_pins_the_connection_requirement_used_by_the_occurrence() {
    let implementation = implementation("wamn:connection/http@0.1.0", 86_400_000).unwrap();
    let artifact = artifact_new("tenant-a", &repeated_flow(), vec![implementation.clone()])
        .expect("connected artifact resolves");
    let call_a = artifact
        .occurrence_recovery()
        .iter()
        .find(|selection| selection.node_id == "call-a")
        .expect("call-a recovery is pinned");
    assert_eq!(call_a.recovery_class, RecoveryClass::IdempotentWithKey);
    assert_eq!(
        call_a.portable_connection.as_ref(),
        implementation.portable_connections().first()
    );

    let call_b = artifact
        .occurrence_recovery()
        .iter()
        .find(|selection| selection.node_id == "call-b")
        .expect("call-b recovery is pinned");
    assert_eq!(call_b.recovery_class, RecoveryClass::NeverReplay);
    assert!(call_b.portable_connection.is_none());
}

#[test]
fn explicit_occurrence_selections_round_trip_from_canonical_storage() {
    let implementation = implementation("wamn:connection/http@0.1.0", 86_400_000).unwrap();
    let selections = vec![
        OccurrenceRecoverySelection {
            selection_version: "1".to_string(),
            node_id: "call-a".to_string(),
            node_type: "http-node".to_string(),
            recovery_class: RecoveryClass::IdempotentWithKey,
            portable_connection: Some(implementation.portable_connections()[0].clone()),
        },
        OccurrenceRecoverySelection {
            selection_version: "1".to_string(),
            node_id: "call-b".to_string(),
            node_type: "http-node".to_string(),
            recovery_class: RecoveryClass::NeverReplay,
            portable_connection: None,
        },
        request_selection(),
        respond_selection(),
    ];
    let flow = repeated_flow();
    let artifact =
        artifact_with_selections("tenant-a", &flow, vec![implementation], selections.clone())
            .unwrap();
    let graph = flow.to_json();
    let interfaces = std::str::from_utf8(artifact.interface_bundle().canonical_bytes()).unwrap();
    let components = serde_json::to_string(artifact.supplied_components()).unwrap();
    let occurrence_recovery = std::str::from_utf8(artifact.occurrence_recovery_bytes()).unwrap();

    let pinned = PinnedArtifact::from_storage(
        "tenant-a",
        &flow.flow_id,
        flow.version,
        &graph,
        artifact.graph_hash(),
        artifact.identity().artifact_hash().as_str(),
        interfaces,
        artifact.interface_bundle().hash(),
        &components,
        Some(occurrence_recovery),
        Some(artifact.occurrence_recovery_hash()),
    )
    .unwrap();
    assert_eq!(pinned.occurrence_recovery(), selections);
    assert_eq!(
        artifact.occurrence_recovery_hash(),
        "sha256:17655bec92539fd91c4723f2f1113dad8199798e1d7dc21638420efe167415ed",
        "canonical occurrence-selection ordering changed"
    );

    assert!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            artifact.graph_hash(),
            artifact.identity().artifact_hash().as_str(),
            interfaces,
            artifact.interface_bundle().hash(),
            &components,
            None,
            None,
        )
        .is_err(),
        "current artifacts must not re-resolve omitted selections"
    );
    assert!(matches!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            artifact.graph_hash(),
            artifact.identity().artifact_hash().as_str(),
            interfaces,
            artifact.interface_bundle().hash(),
            &components,
            Some("[]"),
            Some(artifact.occurrence_recovery_hash()),
        ),
        Err(wamn_catalog::CatalogIdentityError::OccurrenceRecoveryHashMismatch)
    ));

    let mut mutated: serde_json::Value = serde_json::from_str(occurrence_recovery).unwrap();
    mutated.as_array_mut().unwrap().swap(0, 1);
    let mutated = serde_json::to_string(&mutated).unwrap();
    let mutated_hash = digest_bytes(mutated.as_bytes());
    assert!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            artifact.graph_hash(),
            artifact.identity().artifact_hash().as_str(),
            interfaces,
            artifact.interface_bundle().hash(),
            &components,
            Some(&mutated),
            Some(&mutated_hash),
        )
        .is_err(),
        "internally consistent mutated selection bytes must still fail artifact verification"
    );
}

#[test]
fn only_historical_v1_may_project_conservative_occurrence_selections() {
    const CONTRACT: &str = r#"{"executable":{"digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","kind":"component"},"interface":{"capability-classes":["http"],"connection-requirements":[],"contract-version":"1","interface-contract":"wamn:node/node@0.1.0","node-type":"http-node","output-ports":["main"],"purity":"effectful","recovery-class":"never-replay"}}"#;
    let interfaces = format!("[{CONTRACT}]");
    let components = format!(r#"[{{"contract":{CONTRACT}}}]"#);
    let flow = flow();
    let graph = flow.to_json();
    let graph_hash = digest_bytes(&flow.canonical_bytes());
    let artifact_hash = historical_v1_artifact_hash(&flow, CONTRACT);
    let interfaces_hash = digest_bytes(interfaces.as_bytes());
    let pinned = PinnedArtifact::from_storage(
        "tenant-a",
        &flow.flow_id,
        flow.version,
        &graph,
        &graph_hash,
        &artifact_hash,
        &interfaces,
        &interfaces_hash,
        &components,
        None,
        None,
    )
    .unwrap();
    assert_eq!(pinned.occurrence_recovery().len(), 1);
    assert_eq!(
        pinned.occurrence_recovery()[0].recovery_class,
        RecoveryClass::NeverReplay
    );
    assert!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            &graph_hash,
            &artifact_hash,
            &interfaces,
            &interfaces_hash,
            &components,
            Some("[]"),
            Some(&digest_bytes(b"[]")),
        )
        .is_err(),
        "historical projection must not accept unauthenticated selection bytes"
    );
}

#[test]
fn occurrence_selection_rejects_missing_reordered_unknown_and_weaker_inputs() {
    let http_implementation = implementation("wamn:connection/http@0.1.0", 86_400_000).unwrap();
    let baseline = artifact_new(
        "tenant-a",
        &repeated_flow(),
        vec![http_implementation.clone()],
    )
    .unwrap()
    .occurrence_recovery()
    .to_vec();

    assert!(
        artifact_with_selections(
            "tenant-a",
            &repeated_flow(),
            vec![http_implementation.clone()],
            baseline[..1].to_vec(),
        )
        .is_err()
    );
    let mut reordered = baseline.clone();
    reordered.reverse();
    assert!(
        artifact_with_selections(
            "tenant-a",
            &repeated_flow(),
            vec![http_implementation.clone()],
            reordered,
        )
        .is_err()
    );
    let mut unknown = baseline;
    unknown[0].selection_version = "99".to_string();
    assert!(
        artifact_with_selections(
            "tenant-a",
            &repeated_flow(),
            vec![http_implementation],
            unknown,
        )
        .is_err()
    );

    let mut unpinned = artifact_new(
        "tenant-a",
        &repeated_flow(),
        vec![implementation("wamn:connection/http@0.1.0", 86_400_000).unwrap()],
    )
    .unwrap()
    .occurrence_recovery()
    .to_vec();
    unpinned
        .iter_mut()
        .find(|selection| selection.node_id == "call-a")
        .expect("call-a selection exists")
        .portable_connection = None;
    assert!(
        artifact_with_selections(
            "tenant-a",
            &repeated_flow(),
            vec![implementation("wamn:connection/http@0.1.0", 86_400_000).unwrap()],
            unpinned,
        )
        .is_err(),
        "a connected occurrence cannot drop its exact portable requirement"
    );

    let pure_interface = ResolvedNodeInterface::new(
        "http-node",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Pure],
        Vec::new(),
    );
    let pure = NodeImplementation::supplied(
        pure_interface,
        digest('2'),
        ExecutableRecoveryContract::pure(),
    )
    .unwrap();
    let weaker = vec![OccurrenceRecoverySelection {
        selection_version: "1".to_string(),
        node_id: "call".to_string(),
        node_type: "http-node".to_string(),
        recovery_class: RecoveryClass::NeverReplay,
        portable_connection: None,
    }];
    assert!(
        artifact_with_selections("tenant-a", &flow(), vec![pure], weaker).is_err(),
        "a selection cannot weaken the executable's conservative default"
    );
}

#[test]
fn recovery_contract_versions_and_current_fallback_mutations_fail_closed() {
    let implementation = implementation("wamn:connection/http@0.1.0", 1).unwrap();
    let mut contract = implementation.contract().clone();
    contract.executable_recovery.conservative_class = RecoveryClass::IdempotentWithKey;
    let mismatch = NodeImplementation::from_resolved_component(ResolvedComponent { contract })
        .expect("component identity remains structurally valid");
    assert!(artifact_new("tenant-a", &flow(), vec![mismatch]).is_err());

    let mut interface = http_interface("wamn:connection/http@0.1.0");
    interface.contract_version = "99".to_string();
    let unknown = NodeImplementation::supplied(
        interface,
        digest('1'),
        ExecutableRecoveryContract::effectful(true),
    )
    .unwrap();
    assert!(artifact_new("tenant-a", &flow(), vec![unknown]).is_err());

    let mut recovery = ExecutableRecoveryContract::effectful(true);
    recovery.contract_version = "99".to_string();
    assert!(implementation.with_executable_recovery(recovery).is_err());
}
