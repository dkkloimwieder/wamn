//! Repository-level conformance for immutable catalog definition identity.

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        Artifact, Attachment, AttachmentDraft, AttachmentId, AttachmentKind, CanonicalJson,
        CatalogHead, Release, ReleaseId, Source, SourceId, SourceKind,
    };
    use wamn_flow::Flow;
    use wamn_flow::node_contract::{Capability, EffectPolicy, NodeInterface};

    fn flow() -> Flow {
        Flow::from_json(
            r#"{
              "schema-version":"0.1",
              "flow-id":"catalog-proof",
              "version":1,
              "nodes":[
                {"id":"request","type":"request","config":{"input-schema":true}},
                {"id":"custom","type":"custom-step"},
                {"id":"response","type":"respond","config":{"status":200}}
              ],
              "edges":[
                {"from":"request","to":"custom"},
                {"from":"custom","to":"response"}
              ]
            }"#,
        )
        .expect("proof flow parses")
    }

    fn artifact() -> Artifact {
        let interface = NodeInterface {
            node_type: "custom-step".to_string(),
            output_ports: vec!["main".to_string()],
            capabilities: vec![Capability::HttpEgress],
            connection_requirements: Vec::new(),
            effect_policy: EffectPolicy::Effectful,
        };
        Artifact::new(
            "tenant-a",
            &flow(),
            vec![
                interface,
                NodeInterface {
                    node_type: "request".to_string(),
                    output_ports: vec!["main".to_string()],
                    capabilities: Vec::new(),
                    connection_requirements: Vec::new(),
                    effect_policy: EffectPolicy::Pure,
                },
                NodeInterface {
                    node_type: "respond".to_string(),
                    output_ports: vec!["main".to_string()],
                    capabilities: Vec::new(),
                    connection_requirements: Vec::new(),
                    effect_policy: EffectPolicy::Pure,
                },
            ],
        )
        .expect("artifact is canonical")
    }

    #[test]
    fn catalog_artifact_and_resolved_attachment_identity_is_complete() {
        let artifact = artifact();
        let source = Source::new(
            SourceId::new("auth").unwrap(),
            SourceKind::Auth,
            CanonicalJson::new(json!({"policy": "partner"})).unwrap(),
        );
        let attachment = Attachment::resolve(
            AttachmentDraft {
                id: AttachmentId::new("public").unwrap(),
                kind: AttachmentKind::Http,
                artifact_id: artifact.identity().id().clone(),
                source_ids: vec![source.id().clone()],
                definition: CanonicalJson::new(json!({"method": "POST", "path": "/v1/run"}))
                    .unwrap(),
            },
            &artifact,
            std::slice::from_ref(&source),
        )
        .expect("source resolution completes the definition");
        let release = Release::new(
            ReleaseId::new("tenant-a", "main", 1).unwrap(),
            vec![artifact.identity().clone()],
            vec![source],
            vec![attachment.clone()],
        )
        .expect("release members are canonical");

        assert_eq!(
            release.attachments()[0].definition_hash(),
            attachment.definition_hash()
        );
        assert_ne!(
            artifact.identity().artifact_hash().as_str(),
            attachment.definition_hash().as_str()
        );
    }

    #[test]
    fn catalog_refuses_unresolved_or_noncanonical_definition_inputs() {
        let artifact = artifact();
        let error = Attachment::resolve(
            AttachmentDraft {
                id: AttachmentId::new("public").unwrap(),
                kind: AttachmentKind::Http,
                artifact_id: artifact.identity().id().clone(),
                source_ids: vec![SourceId::new("missing").unwrap()],
                definition: CanonicalJson::new(json!({"path": "/"})).unwrap(),
            },
            &artifact,
            &[],
        )
        .expect_err("an unresolved source cannot be hashed");
        assert!(error.to_string().contains("unresolved"));
        assert!(CanonicalJson::parse(r#"{ "path": "/" }"#).is_err());
        assert!(CanonicalJson::new(json!({"enabled": true})).is_err());
    }

    #[test]
    fn catalog_activation_and_head_are_typed_without_entering_definition_hashes() {
        let artifact = artifact();
        let source = Source::new(
            SourceId::new("auth").unwrap(),
            SourceKind::Auth,
            CanonicalJson::new(json!({"policy": "partner"})).unwrap(),
        );
        let attachment = Attachment::resolve(
            AttachmentDraft {
                id: AttachmentId::new("public").unwrap(),
                kind: AttachmentKind::Http,
                artifact_id: artifact.identity().id().clone(),
                source_ids: vec![source.id().clone()],
                definition: CanonicalJson::new(json!({"path": "/"})).unwrap(),
            },
            &artifact,
            std::slice::from_ref(&source),
        )
        .unwrap();
        let activation = wamn_catalog::AttachmentActivation::new(
            "tenant-a",
            "main",
            "dev",
            attachment.id().clone(),
            attachment.definition_hash().clone(),
            true,
        )
        .unwrap();
        let head = CatalogHead::new("tenant-a", "main", "dev", 1).unwrap();

        assert!(activation.definition_is_live(attachment.definition_hash()));
        assert_eq!(head.applied_version(), 1);
    }
}
