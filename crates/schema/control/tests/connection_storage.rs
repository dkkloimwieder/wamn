use wamn_catalog::ConnectionTypeDescriptor;
use wamn_schema_control::connections::{
    ComponentConnectionRequirement, activate_connection_generation_sql,
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
fn component_storage_sql_uses_component_and_effective_release_grains() {
    assert!(insert_component_connection_requirement_sql().contains("component_digest"));
    assert!(exact_component_connection_requirement_sql().contains("store_alias"));
    let binding = insert_component_connection_binding_sql();
    assert!(binding.contains("effective_release_id"));
    assert!(!binding.contains("catalog_"));
}

/// The instance-update guard refuses any update that does not advance
/// `revision`. Activation is the one update the product issues, so its builder
/// must carry the advance; the live round trip in wamn-ctl's
/// bind_connection_live checks the trigger accepts it, and this pins the
/// property offline so a builder that stops advancing fails here first. The
/// update also compares the expected active generation and revision, so a stale
/// caller matches no row.
#[test]
fn activation_advances_the_instance_revision() {
    let sql = activate_connection_generation_sql();
    assert!(sql.contains("UPDATE catalog.connection_instances"));
    assert!(sql.contains("SET active_generation = $4"));
    assert!(sql.contains("revision = revision + 1"));
    assert!(sql.contains("WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3"));
    assert!(sql.contains("AND active_generation IS NOT DISTINCT FROM $5 AND revision = $6"));
}
