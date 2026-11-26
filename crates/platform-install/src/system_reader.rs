//! Grants for the system database reader role.

use std::fmt::Write as _;

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
           wamn_authority.current_app(), identity.is_project_admin(uuid,text,text), \
           identity.has_project_access(uuid,text,text,text) TO {role}; \
         GRANT SELECT ON registry.integration_type, registry.integration_type_version, \
           registry.auth_type, registry.project_env, registry.target TO {role};"
    );
    for relation in crate::sql::IDENTITY_READER_RELATIONS {
        write!(
            sql,
            " GRANT SELECT ON identity.{} TO {role};",
            escape_identifier(relation),
        )
        .expect("string write");
    }
    for (relation, columns) in crate::sql::IDENTITY_READER_COLUMNS {
        let columns = columns
            .iter()
            .map(|column| escape_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            sql,
            " GRANT SELECT ({columns}) ON identity.{} TO {role};",
            escape_identifier(relation),
        )
        .expect("string write");
    }
    sql
}
