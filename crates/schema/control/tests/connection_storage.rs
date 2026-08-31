use wamn_catalog::ConnectionTypeDescriptor;
use wamn_schema_control::connections::{
    ComponentConnectionRequirement, ConnectionGenerationDefinition,
    exact_component_connection_requirement_sql, insert_component_connection_binding_sql,
    insert_component_connection_requirement_sql,
};

fn requirement() -> ComponentConnectionRequirement {
    ComponentConnectionRequirement::new(
        "sha256:component-a",
        "erp",
        ConnectionTypeDescriptor::http_v1(),
    )
}

#[test]
fn component_requirement_identity_is_environment_independent() {
    let requirement = requirement();
    assert_eq!(requirement.component_digest(), "sha256:component-a");
    assert_eq!(requirement.store_alias(), "erp");
    assert_eq!(requirement.requirement_hash().len(), 71);
    assert!(requirement.requirement_hash().starts_with("sha256:"));
    for forbidden in ["prod", "credential-secret", "instance-id"] {
        assert!(
            !requirement
                .canonical_bytes()
                .windows(forbidden.len())
                .any(|window| window == forbidden.as_bytes())
        );
    }
}

#[test]
fn generation_hash_covers_every_non_secret_field() {
    let baseline = ConnectionGenerationDefinition {
        primary_authority: "https://erp.example".into(),
        failover_authorities: vec!["https://backup.example".into()],
        tls_policy: "verify-authority".into(),
        redirect_policy: "same-authority".into(),
        proxy_reference: Some("proxy-a".into()),
    };
    let mut changed = baseline.clone();
    changed.proxy_reference = None;
    assert!(baseline.definition_hash().starts_with("sha256:"));
    assert_ne!(baseline.definition_hash(), changed.definition_hash());
}

#[test]
fn component_storage_sql_uses_component_and_effective_release_grains() {
    assert!(insert_component_connection_requirement_sql().contains("component_digest"));
    assert!(exact_component_connection_requirement_sql().contains("store_alias"));
    let binding = insert_component_connection_binding_sql();
    assert!(binding.contains("effective_release_id"));
    assert!(!binding.contains("catalog_"));
}
