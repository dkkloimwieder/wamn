use serde_json::json;
use wamn_catalog::{
    ArtifactHash, ArtifactId, ArtifactIdentity, Attachment, AttachmentDraft, AttachmentId,
    AttachmentKind, CanonicalJson, CatalogIdentityError, DefinitionHash, Source, SourceId,
    SourceKind,
};

fn artifact_identity(tenant: &str, flow_version: u32, digit: char) -> ArtifactIdentity {
    ArtifactIdentity::new(
        ArtifactId::new(tenant, "receive-order", flow_version).expect("fixture artifact id"),
        ArtifactHash::parse(format!("sha256:{}", digit.to_string().repeat(64)))
            .expect("fixture artifact hash"),
    )
}

fn artifact() -> ArtifactIdentity {
    artifact_identity("tenant-a", 7, '1')
}

fn source(id: &str, kind: SourceKind, definition: serde_json::Value) -> Source {
    Source::new(
        SourceId::new(id).expect("fixture source id is valid"),
        kind,
        CanonicalJson::new(definition).expect("fixture source definition is valid"),
    )
}

fn attachment(
    artifact: &ArtifactIdentity,
    sources: &[Source],
    id: &str,
    kind: AttachmentKind,
    definition: serde_json::Value,
) -> Attachment {
    Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new(id).expect("fixture attachment id is valid"),
            kind,
            artifact_id: artifact.id().clone(),
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

    let changed_artifact = artifact_identity("tenant-a", 7, '2');
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
        baseline_hash, "sha256:a462df2c4cea6093754704bde3f8c5433555e463f6b58fd99c54cfc13d2aa604",
        "definition frame sequence changed"
    );
}

#[test]
fn omitted_unresolved_mutable_and_noncanonical_inputs_are_rejected() {
    let artifact = artifact();
    let missing_source = Attachment::resolve(
        AttachmentDraft {
            id: AttachmentId::new("public-api").unwrap(),
            kind: AttachmentKind::Http,
            artifact_id: artifact.id().clone(),
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
    // Whitespace, key order and duplicate keys are still refused: the parse
    // re-serializes and compares bytes, and `serde_json::Map` is a `BTreeMap`,
    // so a canonical text is the only text that reproduces itself.
    for noncanonical in [r#"{ "a":1}"#, r#"{"b":2,"a":1}"#, "{\n\"a\":1\n}"] {
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

    // MEASURED NARROWING (wamn-0h0g.26.5). The hand-rolled RFC 8785
    // canonicalizer rendered every number through the ECMAScript algorithm, so
    // `1.0` re-serialized as `1` and this text was refused. `serde_json` with
    // `float_roundtrip` reproduces the authored spelling instead, so `1.0` now
    // round-trips and is admitted -- and `{"a":1}` and `{"a":1.0}` are two
    // canonical texts with two different definition hashes.
    //
    // This is bounded, not a hole: every model that enters a digest is a typed
    // struct whose numeric fields serialize one way (a `u16` status is always
    // `200`), and free-form JSON reaches identity only here, where the author's
    // own bytes are what gets hashed and re-checked. Nothing outside Rust
    // verifies these digests.
    assert_eq!(
        CanonicalJson::parse(r#"{"a":1.0}"#).unwrap().as_bytes(),
        br#"{"a":1.0}"#
    );
    assert_ne!(
        CanonicalJson::parse(r#"{"a":1.0}"#).unwrap().as_bytes(),
        CanonicalJson::parse(r#"{"a":1}"#).unwrap().as_bytes()
    );
}

#[test]
fn hash_parser_preserves_canonical_digest_invariant() {
    assert!(DefinitionHash::parse(format!("sha256:{}", "A".repeat(64))).is_err());
}
