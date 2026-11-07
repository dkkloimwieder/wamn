//! The privileged provisioning statements permit only grants on system tables.

use serde_json::Value;
use wamn_control_provision::{host_credentials::HOST_CREDENTIALS, sql};

fn inspect(tree: &Value) {
    match tree {
        Value::Object(fields) => {
            if fields.contains_key("GrantStmt") {
                return;
            }
            if let Some(relation) = fields.get("RangeVar") {
                assert!(
                    !matches!(
                        relation["schemaname"].as_str(),
                        Some("registry" | "identity")
                    ),
                    "admin statement accesses a system table: {relation}"
                );
            }
            for (name, value) in fields {
                if matches!(name.as_str(), "query" | "sval")
                    && let Some(query) = value.as_str()
                    && let Ok(parsed) = pg_query::parse(query)
                {
                    inspect(&serde_json::to_value(parsed.protobuf).unwrap());
                }
                inspect(value);
            }
        }
        Value::Array(items) => items.iter().for_each(inspect),
        _ => {}
    }
}

fn inspect_sql(sql: &str) {
    let parsed = pg_query::parse(sql).unwrap();
    inspect(&serde_json::to_value(parsed.protobuf).unwrap());
    if let Ok(blocks) = pg_query::parse_plpgsql(sql) {
        inspect(&blocks);
    }
}

#[test]
fn ensure_admin_statements_access_system_tables_only_through_grants() {
    for statement in [
        crate::provision_project_env::role_posture_sql(),
        crate::provision_project_env::privilege_sql("project"),
        sql::install_platform_extensions_sql(),
        sql::ensure_replication_role_sql("replication_probe", "password"),
        sql::create_failover_slot_sql("replication_probe"),
        sql::ensure_package_owner_role_sql("probe"),
        sql::ensure_workload_acl_role_sql(wamn_control_provision::WorkloadRoleFamily::Retention),
        sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::AuditRetention,
        ),
        sql::revoke_public_connect_floor_sql().into(),
        sql::workload_generation_state_sql().into(),
        sql::role_database_grants_sql().into(),
        sql::public_connect_databases_sql().into(),
        sql::public_temporary_on_current_database_sql().into(),
        sql::non_template_databases_sql().into(),
    ] {
        inspect_sql(&statement);
    }
    for credential in HOST_CREDENTIALS {
        let role = "generation_probe";
        for statement in [
            sql::prepare_workload_generation_sql(
                credential.family,
                "project",
                role,
                "password",
                "2030-01-01",
            ),
            sql::normalize_workload_generation_membership_sql(credential.family, role, true),
            sql::retire_workload_generation_sql(credential.family, "project", role),
            sql::terminate_workload_generation_sessions_sql(role),
        ] {
            inspect_sql(&statement);
        }
    }
}
