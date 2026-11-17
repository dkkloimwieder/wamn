//! One system database reader login per environment.

use postgres_protocol::escape::escape_identifier;

/// Declare one environment's reader, shared by all its projects.
pub fn managed_role(
    org: &wamn_registry::OrgId,
    environment: &wamn_registry::EnvName,
    database: &str,
) -> crate::connection::ManagedRole {
    let digest = wamn_digest::canonical_json_sha256(&serde_json::json!([
        wamn_role::SYSTEM_READER_ROLE,
        org,
        environment,
        database
    ]));
    let login =
        wamn_registry::name::credential_login(wamn_role::SYSTEM_READER_ROLE, &digest.hex()[..40]);
    let secret = login.as_str().replace('_', "-");
    crate::connection::managed_role(login, wamn_role::SYSTEM_READER_ROLE, secret)
}

/// Permit host authentication and integration type reads.
pub fn grants_sql() -> String {
    let role = escape_identifier(wamn_role::SYSTEM_READER_ROLE);
    let mut sql = format!(
        "DO $connect$ BEGIN EXECUTE format('GRANT CONNECT ON DATABASE %I TO {role}', current_database()); END $connect$; \
         GRANT USAGE ON SCHEMA identity, registry, platform, wamn_authority TO {role}; \
         GRANT INSERT ON platform.tx_identity TO {role}; \
         GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), \
           wamn_authority.current_principal_id(), wamn_authority.current_role(), \
           wamn_authority.current_application(), identity.is_project_admin(uuid,text,text), \
           identity.has_project_access(uuid,text,text,text) TO {role}; \
         GRANT SELECT ON registry.integration_type, registry.integration_type_version, \
           registry.auth_type, registry.environment_projects, registry.target, \
           registry.connection_principal TO {role};"
    );
    for relation in crate::sql::IDENTITY_READER_RELATIONS {
        sql.push_str(&format!(
            " GRANT SELECT ON identity.{} TO {role};",
            escape_identifier(relation),
        ));
    }
    for (relation, columns) in crate::sql::IDENTITY_READER_COLUMNS {
        let columns = columns
            .iter()
            .map(|column| escape_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(
            " GRANT SELECT ({columns}) ON identity.{} TO {role};",
            escape_identifier(relation),
        ));
    }
    sql
}
