use serde_json::json;
use wamn_catalog::{
    Artifact, Attachment, AttachmentActivation, AttachmentDraft, AttachmentId, AttachmentKind,
    CanonicalJson, CatalogHead, CatalogIdentityError, DefinitionHash, InterfaceBundle,
    NodeImplementation, PinnedArtifact, Release, ReleaseId, Source, SourceId, SourceKind,
};
use wamn_flow::Flow;
use wamn_node_manifest::{RecoveryClass, ResolvedNodeInterface, ResolvedPurity};

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
    ResolvedNodeInterface {
        node_type: "custom-node".to_string(),
        output_ports: vec!["main".to_string()],
        purity: ResolvedPurity::Effectful,
        recovery_class: RecoveryClass::NeverReplay,
    }
}

fn supplied(digit: char) -> NodeImplementation {
    NodeImplementation::supplied(
        interface(),
        format!("sha256:{}", digit.to_string().repeat(64)),
    )
    .expect("fixture component digest is valid")
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
        baseline_hash, "sha256:7ffb85fc00483d38f78c09969dd26da8a07a5b43842df2040f28f843a0037f7c",
        "artifact frame sequence changed"
    );
}

#[test]
fn interface_bundle_round_trips_exact_canonical_bytes_and_typed_recovery() {
    let bundle = InterfaceBundle::new(vec![interface()]).unwrap();
    let canonical = std::str::from_utf8(bundle.canonical_bytes()).unwrap();
    assert_eq!(
        canonical,
        r#"[{"node-type":"custom-node","output-ports":["main"],"purity":"effectful","recovery-class":"never-replay"}]"#
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
        ResolvedNodeInterface {
            node_type: "z-node".into(),
            output_ports: vec!["main".into()],
            purity: ResolvedPurity::Effectful,
            recovery_class: RecoveryClass::NeverReplay,
        },
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
        baseline_hash, "sha256:6a6fe2a9a01897295cafa3e3c6e431a013c35c8d09278e2a5f11b6c0c4d3dd5d",
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
    let z_interface = ResolvedNodeInterface {
        node_type: "z-node".to_string(),
        output_ports: vec!["main".to_string()],
        purity: ResolvedPurity::Effectful,
        recovery_class: RecoveryClass::NeverReplay,
    };
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
