use wamn_schema_control::connections::{
    ArtifactConnectionRequirement, ComponentConnectionRequirement, ConnectionGenerationDefinition,
    ConnectionInstanceStatus, GenerationRetentionKind, insert_component_connection_binding_sql,
    insert_component_connection_requirement_sql, insert_connection_binding_sql,
    insert_connection_generation_sql, insert_connection_instance_sql,
    insert_connection_requirement_sql, insert_generation_retention_sql,
};
use wamn_schema_model::ConnectionTypeDescriptor;

const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");

fn artifact_requirement() -> ArtifactConnectionRequirement {
    ArtifactConnectionRequirement::new("artifact-a", "erp", ConnectionTypeDescriptor::http_v1())
}

fn component_requirement() -> ComponentConnectionRequirement {
    ComponentConnectionRequirement::new(
        "sha256:component-a",
        "erp",
        ConnectionTypeDescriptor::http_v1(),
    )
}

#[test]
fn portable_requirement_identity_excludes_environment_definition_fields() {
    let requirement = artifact_requirement();
    let bytes = requirement.canonical_bytes();
    assert_eq!(requirement.artifact_hash(), "artifact-a");
    assert_eq!(requirement.requirement_name(), "erp");
    assert!(!requirement.requirement().identity_bytes().is_empty());

    for forbidden in [
        "prod-environment-id",
        "https://prod.example",
        "credential-secret",
        "credential-generation",
        "instance-generation",
        "instance-id",
        "definition-hash",
    ] {
        assert!(
            !bytes
                .windows(forbidden.len())
                .any(|window| window == forbidden.as_bytes()),
            "portable bytes contain environment field {forbidden:?}"
        );
    }
    assert_eq!(requirement.requirement_hash().len(), 64);
}

#[test]
fn identical_artifact_requirement_binds_differently_without_identity_drift() {
    let requirement = artifact_requirement();
    let artifact_bytes = requirement.canonical_bytes();
    let dev_binding = ("dev", "erp-dev");
    let prod_binding = ("prod", "erp-prod");

    assert_ne!(dev_binding, prod_binding);
    assert_eq!(requirement.canonical_bytes(), artifact_bytes);
    assert_eq!(
        requirement.requirement_hash(),
        requirement.requirement_hash()
    );
}

#[test]
fn component_requirement_identity_uses_digest_and_store_alias() {
    let requirement = component_requirement();
    assert_eq!(requirement.component_digest(), "sha256:component-a");
    assert_eq!(requirement.store_alias(), "erp");
    assert_eq!(
        requirement.requirement(),
        &ConnectionTypeDescriptor::http_v1()
    );
    assert_eq!(requirement.requirement_hash().len(), 64);
}

#[test]
fn generation_definition_hash_covers_every_non_secret_field() {
    let baseline = ConnectionGenerationDefinition {
        primary_authority: "https://erp.example".into(),
        failover_authorities: vec!["https://erp-backup.example".into()],
        tls_policy: "verify-authority".into(),
        redirect_policy: "same-authority".into(),
        proxy_reference: Some("proxy-a".into()),
    };
    let baseline_hash = baseline.definition_hash();

    let mut mutations = Vec::new();
    let mut primary = baseline.clone();
    primary.primary_authority = "https://other.example".into();
    mutations.push(primary);
    let mut failover = baseline.clone();
    failover.failover_authorities.clear();
    mutations.push(failover);
    let mut tls = baseline.clone();
    tls.tls_policy = "disabled".into();
    mutations.push(tls);
    let mut redirect = baseline.clone();
    redirect.redirect_policy = "deny".into();
    mutations.push(redirect);
    let mut proxy = baseline.clone();
    proxy.proxy_reference = None;
    mutations.push(proxy);

    for mutation in mutations {
        assert_ne!(mutation.definition_hash(), baseline_hash);
    }
}

#[test]
fn schema_pins_stable_keys_tenant_isolation_immutability_and_retention() {
    for table in [
        "connection_requirements",
        "connection_instances",
        "connection_generations",
        "connection_bindings",
        "connection_generation_retention",
    ] {
        assert!(CATALOG_SCHEMA.contains(&format!("CREATE TABLE catalog.{table}")));
        assert!(CATALOG_SCHEMA.contains(&format!(
            "ALTER TABLE catalog.{table} FORCE ROW LEVEL SECURITY"
        )));
        assert!(CATALOG_SCHEMA.contains(&format!("CREATE POLICY {table}_tenant")));
    }
    for required in [
        "connection_requirements_complete_grain",
        "connection_requirements_legacy_key",
        "connection_requirements_component_key",
        "PRIMARY KEY (tenant_id, environment, instance_id)",
        "PRIMARY KEY (tenant_id, environment, instance_id, generation)",
        "connection_bindings_complete_grain",
        "connection_bindings_legacy_key",
        "connection_bindings_component_key",
        "connection_bindings_require_requirement",
        "connection_requirements_immutable",
        "connection_generations_update_immutable",
        "connection_bindings_immutable",
        "reject_referenced_connection_generation_delete",
        "connection_instance_controlled_update",
        "credential_set_handle",
        "definition_hash",
        "binding_status IN ('active', 'disabled')",
        "validation_status IN ('valid', 'invalid')",
        "retained_until",
    ] {
        assert!(
            CATALOG_SCHEMA.contains(required),
            "schema omitted {required:?}"
        );
    }
    assert!(!CATALOG_SCHEMA.contains("CREATE FUNCTION catalog.require_connection_artifact"));
    assert!(!CATALOG_SCHEMA.contains("CREATE TRIGGER connection_requirements_require_artifact"));
    assert!(!CATALOG_SCHEMA.contains("credential_secret"));
}

#[test]
fn schema_pins_exactly_one_complete_connection_grain() {
    let requirements = CATALOG_SCHEMA
        .split_once("CREATE TABLE catalog.connection_requirements (")
        .expect("connection requirements table")
        .1
        .split_once("ALTER TABLE catalog.connection_requirements")
        .expect("connection requirements table end")
        .0;
    let bindings = CATALOG_SCHEMA
        .split_once("CREATE TABLE catalog.connection_bindings (")
        .expect("connection bindings table")
        .1
        .split_once("ALTER TABLE catalog.connection_bindings")
        .expect("connection bindings table end")
        .0;

    for table in [requirements, bindings] {
        assert!(table.contains("artifact_hash IS NOT NULL AND requirement_name IS NOT NULL"));
        assert!(table.contains("component_digest IS NULL AND store_alias IS NULL"));
        assert!(table.contains("artifact_hash IS NULL AND requirement_name IS NULL"));
        assert!(table.contains("component_digest IS NOT NULL AND store_alias IS NOT NULL"));
    }
}

#[test]
fn sql_builders_pin_values_and_keep_owners_separate() {
    assert!(insert_connection_requirement_sql().contains("$4::text::jsonb"));
    assert!(!insert_connection_requirement_sql().contains("environment"));
    assert!(insert_connection_instance_sql().contains("environment"));
    assert!(insert_connection_generation_sql().contains("credential_set_handle"));
    assert!(insert_connection_binding_sql().contains("catalog_version"));
    assert!(
        insert_component_connection_requirement_sql().contains("component_digest, store_alias")
    );
    assert!(insert_component_connection_binding_sql().contains("component_digest, store_alias"));
    assert!(insert_generation_retention_sql().contains("retained_until"));
    assert_eq!(ConnectionInstanceStatus::Enabled.as_sql(), "enabled");
    assert_eq!(ConnectionInstanceStatus::Disabled.as_sql(), "disabled");
    assert_eq!(
        GenerationRetentionKind::ActiveAttempt.as_sql(),
        "active-attempt"
    );
    assert_eq!(
        GenerationRetentionKind::DeployedRelease.as_sql(),
        "deployed-release"
    );
}

#[test]
fn project_generation_retention_kinds_are_plane_local_and_exact() {
    fn asserted_sql(kind: GenerationRetentionKind) -> &'static str {
        match kind {
            GenerationRetentionKind::ActiveAttempt => "active-attempt",
            GenerationRetentionKind::DeployedRelease => "deployed-release",
        }
    }

    for kind in [
        GenerationRetentionKind::ActiveAttempt,
        GenerationRetentionKind::DeployedRelease,
    ] {
        assert_eq!(kind.as_sql(), asserted_sql(kind));
        assert_eq!(
            serde_json::to_string(&kind).expect("serialize retention kind"),
            format!("\"{}\"", asserted_sql(kind))
        );
    }

    let retention_table = CATALOG_SCHEMA
        .split_once("CREATE TABLE catalog.connection_generation_retention (")
        .expect("connection generation retention table")
        .1
        .split_once("ALTER TABLE catalog.connection_generation_retention")
        .expect("end of connection generation retention table")
        .0;
    assert!(
        retention_table
            .contains("CHECK (reference_kind IN ('active-attempt', 'deployed-release'))")
    );
    assert_eq!(retention_table.matches("REFERENCES catalog.").count(), 1);
    assert!(retention_table.contains("REFERENCES catalog.connection_generations"));
    assert!(!retention_table.contains("credential_set_handle"));

    for retired_kind in ["replay-seed", "audit-seed", "release-evidence"] {
        assert!(!retention_table.contains(retired_kind));
    }
}
