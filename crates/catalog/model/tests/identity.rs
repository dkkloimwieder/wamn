use serde_json::json;
use wamn_catalog::{
    Artifact, Attachment, AttachmentActivation, AttachmentDraft, AttachmentId, AttachmentKind,
    CanonicalJson, CatalogHead, CatalogIdentityError, DefinitionHash, ExecutionBundleIdentity,
    ExecutionBundleInput, ExecutionBundlePackaging, ExecutionPlugManifest, InterfaceBundle,
    NodeImplementation, PinnedArtifact, Release, ReleaseId, Source, SourceId, SourceKind,
};
use wamn_flow::Flow;
use wamn_node_manifest::{
    CapabilityClass, ConnectionRecoverySupport, ConnectionRequirement, ConnectionTypeDescriptor,
    ExecutableConnectionRecoveryMode, RecoveryClass, ResolvedNodeInterface, ResolvedPurity,
};

fn request_flow() -> Flow {
    Flow::from_json(
        r#"{
          "schema-version":"0.1",
          "flow-id":"receive-order",
          "version":7,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":true}},
            {"id":"shape","type":"custom-node","config":{"template":"v1"}},
            {"id":"response","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"shape"},
            {"from":"shape","to":"response"}
          ]
        }"#,
    )
    .expect("fixture flow parses")
}

fn interface() -> ResolvedNodeInterface {
    resolved_interface(
        "custom-node",
        vec!["main".to_string()],
        ResolvedPurity::Effectful,
        RecoveryClass::NeverReplay,
    )
}

fn resolved_interface(
    node_type: &str,
    output_ports: Vec<String>,
    purity: ResolvedPurity,
    recovery_class: RecoveryClass,
) -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
        node_type,
        "wamn:node@0.1.0",
        output_ports,
        if purity == ResolvedPurity::Pure {
            vec![CapabilityClass::Pure]
        } else {
            vec![CapabilityClass::Http]
        },
        Vec::new(),
        purity,
        recovery_class,
    )
}

fn supplied(digit: char) -> NodeImplementation {
    NodeImplementation::supplied(
        interface(),
        format!("sha256:{}", digit.to_string().repeat(64)),
    )
    .expect("fixture component digest is valid")
}

fn digest_with(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn bundle_input(identity: &str, digit: char) -> ExecutionBundleInput {
    ExecutionBundleInput::new(identity, digest_with(digit)).expect("fixture input is valid")
}

fn plug(identity: &str, node_types: &[&str], digit: char) -> ExecutionPlugManifest {
    ExecutionPlugManifest::new(
        identity,
        node_types
            .iter()
            .map(|node_type| (*node_type).to_string())
            .collect(),
        digest_with(digit),
    )
    .expect("fixture plug manifest is valid")
}

fn execution_bundle(implementation: NodeImplementation) -> ExecutionBundleIdentity {
    ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::ExactNode,
        bundle_input("runner@1", 'a'),
        bundle_input("wac@0.9.0", 'b'),
    )
    .implementations(vec![implementation])
    .plugs(vec![plug("custom-node", &["custom-node"], 'c')])
    .build()
    .expect("fixture execution bundle is valid")
}

fn artifact() -> Artifact {
    Artifact::new("tenant-a", &request_flow(), vec![supplied('1')])
        .expect("fixture artifact is valid")
}

fn source(id: &str, kind: SourceKind, definition: serde_json::Value) -> Source {
    Source::new(
        SourceId::new(id).expect("fixture source id is valid"),
        kind,
        CanonicalJson::new(definition).expect("fixture source definition is valid"),
    )
}

fn attachment(
    artifact: &Artifact,
    sources: &[Source],
    id: &str,
    kind: AttachmentKind,
    definition: serde_json::Value,
) -> Attachment {
    Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new(id).expect("fixture attachment id is valid"),
            kind,
            artifact_id: artifact.identity().id().clone(),
            source_ids: sources.iter().map(|source| source.id().clone()).collect(),
            definition: CanonicalJson::new(definition)
                .expect("fixture attachment definition is valid"),
        },
        artifact,
        sources,
    )
    .expect("fixture attachment resolves")
}

#[test]
fn artifact_identity_pins_every_graph_interface_and_component_input() {
    let baseline = artifact();
    let baseline_hash = baseline.identity().artifact_hash().as_str();
    assert!(baseline.graph_hash().starts_with("sha256:"));

    let tenant_mutant = Artifact::new("tenant-b", &request_flow(), vec![supplied('1')]).unwrap();

    let mut flow_id = request_flow();
    flow_id.flow_id = "receive-order-v2".to_string();
    let flow_id_mutant = Artifact::new("tenant-a", &flow_id, vec![supplied('1')]).unwrap();

    let mut flow_version = request_flow();
    flow_version.version = 8;
    let flow_version_mutant =
        Artifact::new("tenant-a", &flow_version, vec![supplied('1')]).unwrap();

    let mut graph = request_flow();
    graph.nodes[1].config = json!({"template": "v2"});
    let graph_mutant = Artifact::new("tenant-a", &graph, vec![supplied('1')]).unwrap();

    let mut changed_ports = interface();
    changed_ports.output_ports = vec!["alternate".to_string()];
    let mut changed_ports_flow = request_flow();
    changed_ports_flow.edges[1].from_port = "alternate".to_string();
    let interface_ports_mutant = Artifact::new(
        "tenant-a",
        &changed_ports_flow,
        vec![
            NodeImplementation::supplied(changed_ports, format!("sha256:{}", "1".repeat(64)))
                .unwrap(),
        ],
    )
    .unwrap();

    let mut changed_semantics = interface();
    changed_semantics.purity = ResolvedPurity::Pure;
    changed_semantics.recovery_class = RecoveryClass::Replay;
    let interface_semantics_mutant = Artifact::new(
        "tenant-a",
        &request_flow(),
        vec![
            NodeImplementation::supplied(changed_semantics, format!("sha256:{}", "1".repeat(64)))
                .unwrap(),
        ],
    )
    .unwrap();

    let component_mutant = Artifact::new("tenant-a", &request_flow(), vec![supplied('2')]).unwrap();
    let implementation_class_mutant = Artifact::new(
        "tenant-a",
        &request_flow(),
        vec![NodeImplementation::platform(interface())],
    )
    .unwrap();

    for (mutation, mutant) in [
        ("artifact-tenant-id", tenant_mutant),
        ("artifact-flow-id", flow_id_mutant),
        ("artifact-flow-version", flow_version_mutant),
        ("artifact-graph", graph_mutant),
        ("artifact-interface-ports", interface_ports_mutant),
        ("artifact-interface-semantics", interface_semantics_mutant),
        ("artifact-supplied-component-digest", component_mutant),
        ("artifact-implementation-class", implementation_class_mutant),
    ] {
        assert_ne!(
            baseline_hash,
            mutant.identity().artifact_hash().as_str(),
            "named mutation {mutation} survived"
        );
    }

    // Golden bytes kill removal or reordering of any domain-separated frame.
    assert_eq!(
        baseline_hash, "sha256:dab341e733e7f0cc25cabe40a5832794f52bb9c409fb04c9471ea071f2c0d940",
        "artifact frame sequence changed"
    );
}

#[test]
fn canonical_resolution_drives_artifact_replay_and_execution_bundle_identity() {
    let baseline_implementation = supplied('1');
    let baseline_artifact = Artifact::new(
        "tenant-a",
        &request_flow(),
        vec![baseline_implementation.clone()],
    )
    .unwrap();
    let baseline_bundle = execution_bundle(baseline_implementation.clone());
    assert_eq!(
        baseline_implementation.contract().recovery_class(),
        RecoveryClass::NeverReplay
    );

    let mut mutations = Vec::new();
    let mut contract_version = interface();
    contract_version.contract_version = "2".to_string();
    mutations.push(("contract-version", contract_version));
    let mut strict_interface = interface();
    strict_interface.interface_contract = "wamn:node@0.2.0".to_string();
    mutations.push(("interface-contract", strict_interface));
    let mut capabilities = interface();
    capabilities.capability_classes = vec![CapabilityClass::Postgres];
    mutations.push(("capability-class", capabilities));
    let mut recovery = interface();
    recovery.recovery_class = RecoveryClass::IdempotentWithKey;
    mutations.push(("recovery", recovery));
    let mut connection = interface();
    connection.connection_requirements = vec![ConnectionRequirement {
        requirement_type: "http".to_string(),
        contract: "wamn:connection/http@0.1.0".to_string(),
    }];
    mutations.push(("connection-requirement", connection));

    for (name, interface) in mutations {
        let mut implementation =
            NodeImplementation::supplied(interface, format!("sha256:{}", "1".repeat(64))).unwrap();
        if name == "connection-requirement" {
            implementation = implementation
                .with_connection_recovery_support(vec![ConnectionRecoverySupport {
                    descriptor: ConnectionTypeDescriptor::http_v1(),
                    supported_modes: vec![ExecutableConnectionRecoveryMode::NeverReplay],
                }])
                .unwrap();
        }
        let artifact =
            Artifact::new("tenant-a", &request_flow(), vec![implementation.clone()]).unwrap();
        let bundle = execution_bundle(implementation);
        assert_ne!(
            baseline_artifact.identity().artifact_hash(),
            artifact.identity().artifact_hash(),
            "{name} did not invalidate artifact identity"
        );
        assert_ne!(
            baseline_bundle.hash(),
            bundle.hash(),
            "{name} did not invalidate execution-bundle identity"
        );
    }

    let changed_executable = supplied('2');
    let changed_bundle = execution_bundle(changed_executable);
    assert_ne!(baseline_bundle.hash(), changed_bundle.hash());
}

#[test]
fn execution_bundle_identity_pins_every_composition_input() {
    fn build(
        packaging: ExecutionBundlePackaging,
        runner: ExecutionBundleInput,
        implementation: NodeImplementation,
        plugs: Vec<ExecutionPlugManifest>,
        adapters: Vec<ExecutionBundleInput>,
        tool: ExecutionBundleInput,
    ) -> ExecutionBundleIdentity {
        ExecutionBundleIdentity::builder(packaging, runner, tool)
            .implementations(vec![implementation])
            .plugs(plugs)
            .adapters(adapters)
            .build()
            .unwrap()
    }

    let baseline = build(
        ExecutionBundlePackaging::ExactNode,
        bundle_input("runner@1", 'a'),
        supplied('1'),
        vec![plug("custom-node", &["custom-node"], 'c')],
        vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
        bundle_input("wac@0.9.0", 'b'),
    );
    let baseline_hash = baseline.hash();
    assert_eq!(
        baseline_hash, "sha256:bd7ba335344dfeb64d2a5a33efcc923b781134da92133b237bb2929914be2350",
        "execution-bundle frame sequence changed"
    );

    let mutants = [
        build(
            ExecutionBundlePackaging::CapabilityClass,
            bundle_input("runner@1", 'a'),
            supplied('1'),
            vec![plug("http", &["custom-node", "http-request"], 'c')],
            vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
            bundle_input("wac@0.9.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@2", 'a'),
            supplied('1'),
            vec![plug("custom-node", &["custom-node"], 'c')],
            vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
            bundle_input("wac@0.9.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@1", '9'),
            supplied('1'),
            vec![plug("custom-node", &["custom-node"], 'c')],
            vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
            bundle_input("wac@0.9.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@1", 'a'),
            supplied('1'),
            vec![plug("custom-node-v2", &["custom-node"], 'c')],
            vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
            bundle_input("wac@0.9.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@1", 'a'),
            supplied('1'),
            vec![plug("custom-node", &["custom-node"], '8')],
            vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
            bundle_input("wac@0.9.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@1", 'a'),
            supplied('1'),
            vec![plug("custom-node", &["custom-node"], 'c')],
            vec![bundle_input("wamn:connection/http@0.2.0", 'd')],
            bundle_input("wac@0.9.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@1", 'a'),
            supplied('1'),
            vec![plug("custom-node", &["custom-node"], 'c')],
            vec![bundle_input("wamn:connection/http@0.1.0", '7')],
            bundle_input("wac@0.9.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@1", 'a'),
            supplied('1'),
            vec![plug("custom-node", &["custom-node"], 'c')],
            vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
            bundle_input("wac@0.10.0", 'b'),
        ),
        build(
            ExecutionBundlePackaging::ExactNode,
            bundle_input("runner@1", 'a'),
            supplied('1'),
            vec![plug("custom-node", &["custom-node"], 'c')],
            vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
            bundle_input("wac@0.9.0", '6'),
        ),
    ];
    for mutant in mutants {
        assert_ne!(baseline_hash, mutant.hash());
    }

    let rebuilt = build(
        ExecutionBundlePackaging::ExactNode,
        bundle_input("runner@1", 'a'),
        supplied('1'),
        vec![plug("custom-node", &["custom-node"], 'c')],
        vec![bundle_input("wamn:connection/http@0.1.0", 'd')],
        bundle_input("wac@0.9.0", 'b'),
    );
    assert_eq!(baseline, rebuilt);
}

#[test]
fn bundle_provenance_is_reproducible_and_rejects_cache_poisoning() {
    struct EnvironmentInstance<'a> {
        environment: &'a str,
        endpoint: &'a str,
        credential_generation: u32,
    }

    let dev = EnvironmentInstance {
        environment: "dev",
        endpoint: "https://sandbox.example.test",
        credential_generation: 3,
    };
    let prod = EnvironmentInstance {
        environment: "prod",
        endpoint: "https://api.example.test",
        credential_generation: 91,
    };
    assert_ne!(dev.environment, prod.environment);
    assert_ne!(dev.endpoint, prod.endpoint);
    assert_ne!(dev.credential_generation, prod.credential_generation);

    let dev_identity = execution_bundle(supplied('1'));
    let prod_identity = execution_bundle(supplied('1'));
    assert_eq!(
        dev_identity, prod_identity,
        "environment instances are excluded"
    );

    let output = b"deterministic composed component";
    let composition_log = b"wac plug: deterministic fixture";
    let provenance = dev_identity.provenance(output, composition_log);
    let rebuilt = prod_identity.provenance(output, composition_log);
    assert_eq!(provenance, rebuilt);
    assert_eq!(provenance.identity_hash(), dev_identity.hash());
    assert_eq!(provenance.identity_bytes(), dev_identity.canonical_bytes());
    assert!(provenance.output_digest().starts_with("sha256:"));
    assert_eq!(provenance.composition_log(), composition_log);
    assert!(!provenance.canonical_bytes().is_empty());
    assert_eq!(provenance.verify_rebuild(&prod_identity, output), Ok(()));

    assert_eq!(
        provenance.verify_rebuild(&execution_bundle(supplied('2')), output),
        Err(CatalogIdentityError::ExecutionBundleIdentityMismatch)
    );
    assert_eq!(
        provenance.verify_rebuild(&prod_identity, b"poisoned cached component"),
        Err(CatalogIdentityError::ExecutionBundleOutputMismatch)
    );
}

#[test]
fn bundle_builder_refuses_ambiguous_layouts_and_order() {
    let runner = bundle_input("runner@1", 'a');
    let tool = bundle_input("wac@0.9.0", 'b');

    let multi_node_exact = ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::ExactNode,
        runner.clone(),
        tool.clone(),
    )
    .implementations(vec![supplied('1')])
    .plugs(vec![plug("combined", &["custom-node", "other-node"], 'c')])
    .build();
    assert!(multi_node_exact.is_err());

    let extra_exact_plug = ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::ExactNode,
        runner.clone(),
        tool.clone(),
    )
    .implementations(vec![supplied('1')])
    .plugs(vec![
        plug("custom-node", &["custom-node"], 'c'),
        plug("other-node", &["other-node"], 'd'),
    ])
    .build();
    assert!(extra_exact_plug.is_err());

    let missing_node = ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::CapabilityClass,
        runner.clone(),
        tool.clone(),
    )
    .implementations(vec![supplied('1')])
    .plugs(vec![plug("pure", &["other-node"], 'c')])
    .build();
    assert!(missing_node.is_err());

    let duplicate_node = ExecutionBundleIdentity::builder(
        ExecutionBundlePackaging::CapabilityClass,
        runner.clone(),
        tool.clone(),
    )
    .implementations(vec![supplied('1')])
    .plugs(vec![
        plug("http-a", &["custom-node"], 'c'),
        plug("http-b", &["custom-node"], 'd'),
    ])
    .build();
    assert!(duplicate_node.is_err());

    let unsorted_adapters =
        ExecutionBundleIdentity::builder(ExecutionBundlePackaging::ExactNode, runner, tool)
            .implementations(vec![supplied('1')])
            .plugs(vec![plug("custom-node", &["custom-node"], 'c')])
            .adapters(vec![
                bundle_input("z-adapter", 'd'),
                bundle_input("a-adapter", 'e'),
            ])
            .build();
    assert!(unsorted_adapters.is_err());
}

#[test]
fn interface_bundle_round_trips_exact_canonical_bytes_and_typed_recovery() {
    let bundle = InterfaceBundle::new(vec![interface()]).unwrap();
    let canonical = std::str::from_utf8(bundle.canonical_bytes()).unwrap();
    assert_eq!(
        canonical,
        r#"[{"executable":{"kind":"platform","revision":"wamn-standard-nodes@0.1.0"},"interface":{"capability-classes":["http"],"connection-requirements":[],"contract-version":"1","interface-contract":"wamn:node@0.1.0","node-type":"custom-node","output-ports":["main"],"purity":"effectful","recovery-class":"never-replay"}}]"#
    );
    assert!(bundle.hash().starts_with("sha256:"));
    assert_eq!(
        bundle.interface("custom-node").unwrap().recovery_class,
        RecoveryClass::NeverReplay
    );
    assert_eq!(
        InterfaceBundle::from_canonical_json(canonical).unwrap(),
        bundle
    );
}

#[test]
fn interface_bundle_refuses_shape_canonicality_hash_and_order_mutations() {
    let artifact = artifact();
    let canonical = std::str::from_utf8(artifact.interface_bundle().canonical_bytes()).unwrap();
    for mutant in [
        format!(" {canonical}"),
        canonical.replace(r#""purity":"effectful""#, r#""purity":"pure""#),
        canonical.replace(r#","recovery-class":"never-replay""#, ""),
        canonical.replace(r#""node-type":"custom-node""#, r#""unknown":"custom-node""#),
    ] {
        assert!(
            InterfaceBundle::from_canonical_json(&mutant).is_err(),
            "mutation survived: {mutant}"
        );
    }

    let reversed = InterfaceBundle::new(vec![
        resolved_interface(
            "z-node",
            vec!["main".into()],
            ResolvedPurity::Effectful,
            RecoveryClass::NeverReplay,
        ),
        interface(),
    ]);
    assert!(matches!(
        reversed,
        Err(CatalogIdentityError::NonCanonicalInterfaceOrder { .. })
    ));
}

#[test]
fn pinned_artifact_verifies_graph_bundle_and_artifact_key_as_one_unit() {
    let flow = request_flow();
    let artifact = artifact();
    let graph = flow.to_json();
    let bundle = std::str::from_utf8(artifact.interface_bundle().canonical_bytes()).unwrap();
    let verified = PinnedArtifact::from_storage(
        "tenant-a",
        &flow.flow_id,
        flow.version,
        &graph,
        artifact.graph_hash(),
        artifact.identity().artifact_hash().as_str(),
        bundle,
        artifact.interface_bundle().hash(),
        &serde_json::to_string(artifact.supplied_components()).unwrap(),
    )
    .unwrap();
    assert_eq!(verified.flow(), &flow);
    assert_eq!(
        verified
            .interface_bundle()
            .interface("custom-node")
            .unwrap()
            .purity,
        ResolvedPurity::Effectful
    );

    let bad_graph_hash = format!("sha256:{}", "0".repeat(64));
    let bad_bundle_hash = format!("sha256:{}", "2".repeat(64));
    let bad_artifact_hash = format!("sha256:{}", "3".repeat(64));
    assert!(matches!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            artifact.graph_hash(),
            &bad_artifact_hash,
            bundle,
            artifact.interface_bundle().hash(),
            &serde_json::to_string(artifact.supplied_components()).unwrap(),
        ),
        Err(CatalogIdentityError::ArtifactHashMismatch)
    ));
    assert!(matches!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            artifact.graph_hash(),
            artifact.identity().artifact_hash().as_str(),
            bundle,
            artifact.interface_bundle().hash(),
            "[]",
        ),
        Err(CatalogIdentityError::ArtifactHashMismatch)
    ));
    assert!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            artifact.graph_hash(),
            artifact.identity().artifact_hash().as_str(),
            bundle,
            artifact.interface_bundle().hash(),
            "not-json",
        )
        .is_err()
    );
    assert!(matches!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            &bad_graph_hash,
            artifact.identity().artifact_hash().as_str(),
            bundle,
            artifact.interface_bundle().hash(),
            &serde_json::to_string(artifact.supplied_components()).unwrap(),
        ),
        Err(CatalogIdentityError::GraphHashMismatch)
    ));
    assert!(matches!(
        PinnedArtifact::from_storage(
            "tenant-a",
            &flow.flow_id,
            flow.version,
            &graph,
            artifact.graph_hash(),
            artifact.identity().artifact_hash().as_str(),
            bundle,
            &bad_bundle_hash,
            &serde_json::to_string(artifact.supplied_components()).unwrap(),
        ),
        Err(CatalogIdentityError::InterfaceBundleHashMismatch)
    ));
    assert!(matches!(
        PinnedArtifact::from_storage(
            "tenant-a",
            "different-flow",
            flow.version,
            &graph,
            artifact.graph_hash(),
            artifact.identity().artifact_hash().as_str(),
            bundle,
            artifact.interface_bundle().hash(),
            &serde_json::to_string(artifact.supplied_components()).unwrap(),
        ),
        Err(CatalogIdentityError::ArtifactIdMismatch)
    ));
}

#[test]
fn pinned_artifact_verifies_and_projects_the_legacy_persisted_shape() {
    const LEGACY_BUNDLE: &str = r#"[{"node-type":"custom-node","output-ports":["main"],"purity":"effectful","recovery-class":"never-replay"}]"#;
    const LEGACY_BUNDLE_HASH: &str =
        "sha256:6dedf8035e4ed1bb053b9701f5b5a9620e340111fcba07e71bcb3a8897a03201";
    const LEGACY_COMPONENTS: &str = r#"[{"interface":{"node-type":"custom-node","output-ports":["main"],"purity":"effectful","recovery-class":"never-replay"},"component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111"}]"#;
    const LEGACY_ARTIFACT_HASH: &str =
        "sha256:7ffb85fc00483d38f78c09969dd26da8a07a5b43842df2040f28f843a0037f7c";

    let flow = request_flow();
    let graph = flow.to_json();
    let graph_hash = Artifact::new("tenant-a", &flow, vec![supplied('1')])
        .unwrap()
        .graph_hash()
        .to_string();
    let pinned = PinnedArtifact::from_storage(
        "tenant-a",
        &flow.flow_id,
        flow.version,
        &graph,
        &graph_hash,
        LEGACY_ARTIFACT_HASH,
        LEGACY_BUNDLE,
        LEGACY_BUNDLE_HASH,
        LEGACY_COMPONENTS,
    )
    .expect("a pre-contract-version persisted artifact remains recoverable");

    let contract = pinned
        .interface_bundle()
        .contract("custom-node")
        .expect("legacy interface projects into a runtime contract");
    assert_eq!(contract.interface.contract_version, "legacy-v0");
    assert_eq!(contract.recovery_class(), RecoveryClass::NeverReplay);
    assert!(matches!(
        contract.executable,
        wamn_node_manifest::ExecutableIdentity::Component { .. }
    ));
    assert_eq!(pinned.interface_bundle().hash(), LEGACY_BUNDLE_HASH);
}

#[test]
fn definition_hash_pins_attachment_artifact_and_complete_resolved_sources() {
    let baseline_artifact = artifact();
    let baseline_sources = vec![
        source("a-auth", SourceKind::Auth, json!({"policy": "partner"})),
        source(
            "b-caller",
            SourceKind::CallerPolicy,
            json!({"roles": ["writer"]}),
        ),
    ];
    let baseline = attachment(
        &baseline_artifact,
        &baseline_sources,
        "public-api",
        AttachmentKind::Http,
        json!({"method": "POST", "path": "/v1/orders"}),
    );
    let baseline_hash = baseline.definition_hash().as_str();

    let id_mutant = attachment(
        &baseline_artifact,
        &baseline_sources,
        "partner-api",
        AttachmentKind::Http,
        json!({"method": "POST", "path": "/v1/orders"}),
    );
    let kind_mutant = attachment(
        &baseline_artifact,
        &baseline_sources,
        "public-api",
        AttachmentKind::Internal,
        json!({"method": "POST", "path": "/v1/orders"}),
    );
    let definition_mutant = attachment(
        &baseline_artifact,
        &baseline_sources,
        "public-api",
        AttachmentKind::Http,
        json!({"method": "PUT", "path": "/v1/orders"}),
    );

    let changed_artifact = Artifact::new("tenant-a", &request_flow(), vec![supplied('2')]).unwrap();
    let artifact_mutant = attachment(
        &changed_artifact,
        &baseline_sources,
        "public-api",
        AttachmentKind::Http,
        json!({"method": "POST", "path": "/v1/orders"}),
    );

    let source_id_mutant_sources = vec![
        source("a-auth-v2", SourceKind::Auth, json!({"policy": "partner"})),
        baseline_sources[1].clone(),
    ];
    let source_id_mutant = attachment(
        &baseline_artifact,
        &source_id_mutant_sources,
        "public-api",
        AttachmentKind::Http,
        json!({"method": "POST", "path": "/v1/orders"}),
    );
    let source_kind_mutant_sources = vec![
        source(
            "a-auth",
            SourceKind::CallerPolicy,
            json!({"policy": "partner"}),
        ),
        baseline_sources[1].clone(),
    ];
    let source_kind_mutant = attachment(
        &baseline_artifact,
        &source_kind_mutant_sources,
        "public-api",
        AttachmentKind::Http,
        json!({"method": "POST", "path": "/v1/orders"}),
    );
    let source_definition_mutant_sources = vec![
        source("a-auth", SourceKind::Auth, json!({"policy": "employee"})),
        baseline_sources[1].clone(),
    ];
    let source_definition_mutant = attachment(
        &baseline_artifact,
        &source_definition_mutant_sources,
        "public-api",
        AttachmentKind::Http,
        json!({"method": "POST", "path": "/v1/orders"}),
    );

    for (mutation, mutant) in [
        ("definition-attachment-id", id_mutant),
        ("definition-attachment-kind", kind_mutant),
        ("definition-attachment-body", definition_mutant),
        ("definition-artifact-identity", artifact_mutant),
        ("definition-source-id", source_id_mutant),
        ("definition-source-kind", source_kind_mutant),
        ("definition-source-body", source_definition_mutant),
    ] {
        assert_ne!(
            baseline_hash,
            mutant.definition_hash().as_str(),
            "named mutation {mutation} survived"
        );
    }

    assert_eq!(
        baseline_hash, "sha256:e64b7840deb81287f1d2268f350fd9c75082d98d535ca7f41b97adfb453f93cc",
        "definition frame sequence changed"
    );
}

#[test]
fn omitted_unresolved_mutable_and_noncanonical_inputs_are_rejected() {
    let missing_interface = Artifact::new("tenant-a", &request_flow(), vec![])
        .expect_err("an ordinary graph node needs an interface pin");
    assert!(matches!(
        missing_interface,
        CatalogIdentityError::UnresolvedInterface { .. }
    ));
    assert!(NodeImplementation::supplied(interface(), "").is_err());

    let artifact = artifact();
    let missing_source = Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new("public-api").unwrap(),
            kind: AttachmentKind::Http,
            artifact_id: artifact.identity().id().clone(),
            source_ids: vec![SourceId::new("missing").unwrap()],
            definition: CanonicalJson::new(json!({"path": "/"})).unwrap(),
        },
        &artifact,
        &[],
    )
    .expect_err("unresolved sources cannot enter a definition hash");
    assert!(matches!(
        missing_source,
        CatalogIdentityError::UnresolvedSource { .. }
    ));

    for field in [
        "active",
        "applied-version",
        "changed-at",
        "confirmed-definition-hash",
        "created-at",
        "enabled",
        "updated-at",
    ] {
        assert!(
            matches!(
                CanonicalJson::new(json!({field: true})),
                Err(CatalogIdentityError::MutableIdentityInput { .. })
            ),
            "mutable field {field} was accepted"
        );
    }
    for noncanonical in [
        r#"{ "a":1}"#,
        r#"{"b":2,"a":1}"#,
        r#"{"a":1.0}"#,
        "{\n\"a\":1\n}",
    ] {
        assert!(
            matches!(
                CanonicalJson::parse(noncanonical),
                Err(CatalogIdentityError::NonCanonicalJson)
            ),
            "noncanonical input {noncanonical:?} was accepted"
        );
    }
    assert_eq!(
        CanonicalJson::parse(r#"{"a":1,"b":2}"#).unwrap().as_bytes(),
        br#"{"a":1,"b":2}"#
    );
}

#[test]
fn noncanonical_interface_and_member_reordering_is_rejected() {
    let mut flow = request_flow();
    flow.nodes.insert(
        2,
        wamn_flow::Node {
            id: "second".to_string(),
            node_type: "z-node".to_string(),
            label: None,
            config: json!({}),
            credential: None,
        },
    );
    flow.edges = vec![
        wamn_flow::Edge {
            from: "request".to_string(),
            from_port: "main".to_string(),
            to: "shape".to_string(),
            to_port: None,
        },
        wamn_flow::Edge {
            from: "shape".to_string(),
            from_port: "main".to_string(),
            to: "second".to_string(),
            to_port: None,
        },
        wamn_flow::Edge {
            from: "second".to_string(),
            from_port: "main".to_string(),
            to: "response".to_string(),
            to_port: None,
        },
    ];
    let z_interface = resolved_interface(
        "z-node",
        vec!["main".to_string()],
        ResolvedPurity::Effectful,
        RecoveryClass::NeverReplay,
    );
    let reordered = Artifact::new(
        "tenant-a",
        &flow,
        vec![
            NodeImplementation::platform(z_interface),
            NodeImplementation::platform(interface()),
        ],
    )
    .expect_err("interface bundle order is canonical");
    assert!(matches!(
        reordered,
        CatalogIdentityError::NonCanonicalInterfaceOrder { .. }
    ));

    let artifact = artifact();
    let auth = source("a-auth", SourceKind::Auth, json!({"policy": "partner"}));
    let caller = source(
        "b-caller",
        SourceKind::CallerPolicy,
        json!({"roles": ["writer"]}),
    );
    let source_order = Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new("public-api").unwrap(),
            kind: AttachmentKind::Http,
            artifact_id: artifact.identity().id().clone(),
            source_ids: vec![caller.id().clone(), auth.id().clone()],
            definition: CanonicalJson::new(json!({"path": "/"})).unwrap(),
        },
        &artifact,
        &[auth, caller],
    )
    .expect_err("source references have one canonical order");
    assert!(matches!(
        source_order,
        CatalogIdentityError::NonCanonicalMemberOrder { .. }
    ));
}

#[test]
fn release_activation_head_and_hash_parser_preserve_scope_invariants() {
    let artifact = artifact();
    let sources = vec![source(
        "auth",
        SourceKind::Auth,
        json!({"policy": "partner"}),
    )];
    let attachment = attachment(
        &artifact,
        &sources,
        "public-api",
        AttachmentKind::Http,
        json!({"path": "/"}),
    );
    let release = Release::new(
        ReleaseId::new("tenant-a", "main", 4).unwrap(),
        vec![artifact.identity().clone()],
        sources,
        vec![attachment.clone()],
    )
    .expect("release is canonical");
    assert!(!release.canonical_bytes().is_empty());

    let activation = AttachmentActivation::new(
        "tenant-a",
        "main",
        "prod",
        attachment.id().clone(),
        attachment.definition_hash().clone(),
        true,
    )
    .unwrap();
    assert!(activation.definition_is_live(attachment.definition_hash()));
    let other = DefinitionHash::parse(format!("sha256:{}", "f".repeat(64))).unwrap();
    assert!(!activation.definition_is_live(&other));

    let head = CatalogHead::new("tenant-a", "main", "prod", 4).unwrap();
    assert_eq!(head.applied_version(), 4);
    assert!(CatalogHead::new("tenant-a", "main", "prod", 0).is_err());
    assert!(DefinitionHash::parse(format!("sha256:{}", "A".repeat(64))).is_err());
}
