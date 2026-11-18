//! Grants for the system database reader role.

use postgres_protocol::escape::escape_identifier;

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
           registry.auth_type, registry.environment_projects, registry.target TO {role};"
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
