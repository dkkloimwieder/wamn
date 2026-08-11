use serde_json::json;
use wamn_catalog::{
    Artifact, Attachment, AttachmentActivation, AttachmentDraft, AttachmentId, AttachmentKind,
    CanonicalJson, CatalogHead, CatalogIdentityError, DefinitionHash, Release, ReleaseId, Source,
    SourceId, SourceKind,
};
use wamn_flow::Flow;
use wamn_node_manifest::{CapabilityClass, ResolvedNodeInterface};

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

fn artifact_new(
    tenant: &str,
    flow: &Flow,
    mut implementations: Vec<ResolvedNodeInterface>,
) -> Result<Artifact, CatalogIdentityError> {
    let request = ResolvedNodeInterface::new(
        "request",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Pure],
        Vec::new(),
    );
    let respond = ResolvedNodeInterface::new(
        "respond",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Pure],
        Vec::new(),
    );
    for implementation in [request, respond] {
        let index = implementations
            .iter()
            .position(|candidate| candidate.node_type > implementation.node_type)
            .unwrap_or(implementations.len());
        implementations.insert(index, implementation);
    }
    Artifact::new(tenant, flow, implementations)
}

fn interface() -> ResolvedNodeInterface {
    ResolvedNodeInterface::new(
        "custom-node",
        "wamn:node/node@0.1.0",
        vec!["main".to_string()],
        vec![CapabilityClass::Http],
        Vec::new(),
    )
}

fn supplied(_digit: char) -> ResolvedNodeInterface {
    interface()
}

fn artifact() -> Artifact {
    artifact_new("tenant-a", &request_flow(), vec![supplied('1')])
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

    let mut changed_flow = request_flow();
    changed_flow.nodes[1].config = json!({"template": "v2"});
    let changed_artifact = artifact_new("tenant-a", &changed_flow, vec![supplied('2')]).unwrap();
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
        baseline_hash, "sha256:92a76c0504a1142aa086b0b135e611d59c036384d70064424b2994a67c564e9e",
        "definition frame sequence changed"
    );
}

#[test]
fn omitted_unresolved_mutable_and_noncanonical_inputs_are_rejected() {
    let missing_interface = artifact_new("tenant-a", &request_flow(), vec![])
        .expect_err("an ordinary graph node needs an interface pin");
    assert!(matches!(
        missing_interface,
        CatalogIdentityError::UnresolvedInterface { .. }
    ));
    assert!(!interface().node_type.is_empty());

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
