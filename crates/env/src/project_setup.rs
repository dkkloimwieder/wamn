//! Workload credential preparation, retirement, and observed role state.

use std::fmt::Write as _;

use postgres_protocol::escape::{escape_identifier, escape_literal};
use wamn_identity_protocol::workload_role::WorkloadRoleFamily;

/// Idempotently create or harden one stable workload ACL role as NOLOGIN.
pub fn ensure_workload_acl_role_sql(family: WorkloadRoleFamily) -> String {
    if family == WorkloadRoleFamily::Migration {
        String::new()
    } else {
        ensure_project_roles_sql().to_owned()
    }
}

/// Grant database access after CNPG creates this login.
pub fn grant_workload_login_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &wamn_role::LoginName,
) -> String {
    let privileges = if family == WorkloadRoleFamily::Migration {
        "CONNECT, CREATE"
    } else {
        "CONNECT"
    };
    let mut grants = format!(
        "{} GRANT {privileges} ON DATABASE {} TO {};",
        stable_surface_sql(family).unwrap_or_else(|| ensure_workload_acl_role_sql(family)),
        escape_identifier(database),
        escape_identifier(role.as_str())
    );
    if family == WorkloadRoleFamily::Platform {
        write!(
            grants,
            "INSERT INTO platform.login_principal(login,principal_id) VALUES ({login}, '{principal}') ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id;",
            login = escape_literal(role.as_str()),
            principal = wamn_identity_protocol::PlatformComponent::Executor.principal_id(),
        )
        .expect("string write");
    }
    if family == WorkloadRoleFamily::Migration {
        write!(
            grants,
            "ALTER DEFAULT PRIVILEGES FOR ROLE {} REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;",
            escape_identifier(role.as_str())
        )
        .expect("string write");
    }
    grants
}

/// Grant the dynamic SQL role reads without inheriting any application writer.
pub fn grant_dynamic_sql_surface_sql() -> String {
    let role = escape_identifier(wamn_role::QUERY_ROLE);
    format!(
        "{} DO $read$ DECLARE target record; relation record; \
         private_relations CONSTANT name[] := ARRAY[ \
           'credential', 'credential_history', 'oauth_token', 'oauth_authorization', 'pat', 'pat_history']; BEGIN \
         FOR target IN SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' AND nspname NOT IN ('information_schema', 'wamn_history') LOOP \
           EXECUTE format('GRANT USAGE ON SCHEMA %I TO {role}', target.nspname); \
           FOR relation IN SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
             WHERE n.nspname=target.nspname AND c.relkind IN ('r','p','v','m','f') \
             AND NOT (n.nspname='platform' AND c.relname = ANY(private_relations)) LOOP \
             EXECUTE format('GRANT SELECT ON %I.%I TO {role}', target.nspname, relation.relname); \
           END LOOP; \
         END LOOP; \
         FOR target IN SELECT c.oid::regclass AS relation FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
           WHERE n.nspname='platform' AND c.relname = ANY(private_relations) LOOP \
           EXECUTE format('REVOKE ALL ON %s FROM {role}', target.relation); \
         END LOOP; END $read$; \
         REVOKE ALL ON platform.tx_identity, platform.login_principal FROM {role}; \
         GRANT INSERT ON platform.tx_identity TO {role}; \
         GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(), \
           wamn_authority.current_role(), wamn_authority.current_app() TO {role};",
        ensure_project_roles_sql()
    )
}

/// Grant the host's platform work and retention within one project database.
pub fn grant_platform_surface_sql() -> String {
    let role = escape_identifier(wamn_role::PLATFORM_ROLE);
    format!(
        "GRANT USAGE ON SCHEMA platform, catalog, wamn_run, wamn_history, wamn_authority TO {role}; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA platform, catalog, wamn_run TO {role}; \
         GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA platform, catalog, wamn_run TO {role}; \
         GRANT SELECT, INSERT, UPDATE ON wamn_history.pruning_watermark TO {role}; \
         REVOKE ALL ON platform.login_principal FROM {role}; \
         GRANT EXECUTE ON FUNCTION platform.prune_tx_identity() TO {role}; \
         REVOKE DELETE ON platform.pat, platform.role FROM {role}; \
         REVOKE DELETE ON wamn_run.run FROM {role}; \
         GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(), \
           wamn_authority.current_role(), wamn_authority.current_app() TO {role}; \
         GRANT EXECUTE ON FUNCTION wamn_history.row_image(record), \
           wamn_history.timestamptz_image(timestamptz) TO {role};",
    )
}

/// The per-project authorization and configuration schema, applied to a project
/// database after its catalog schema.
///
/// Every relation it creates carries a stamp trigger that calls
/// `wamn_history.stamp_row()`, so the applier installs
/// `deploy/sql/record-history.sql` first. `CATALOG_SCHEMA_SQL` carries that
/// file, so an applier that applied the catalog schema already holds it
/// (`wamn-0h0g.9.15`).
pub const APP_SCHEMA_SQL: &str = concat!(
    include_str!("../../../deploy/sql/app-schema.sql"),
    include_str!("../../../deploy/sql/definer-privileges.sql"),
);

/// Complete catalog bootstrap for a fresh project database.
pub const CATALOG_SCHEMA_SQL: &str = concat!(
    include_str!("../../../deploy/sql/catalog-schema-prefix.sql"),
    include_str!("../../../deploy/sql/reject-immutable-row-change.sql"),
    include_str!("../../../deploy/sql/record-history.sql"),
    include_str!("../../../deploy/sql/record-history-app-grants.sql"),
    include_str!("../../../deploy/sql/catalog-schema.sql"),
    include_str!("../../../deploy/sql/definer-privileges.sql"),
);

/// Create shared project roles before selecting the database owner.
pub fn ensure_project_roles_sql() -> &'static str {
    include_str!("../../../deploy/sql/project-roles.sql")
}

pub fn stable_surface_sql(family: WorkloadRoleFamily) -> Option<String> {
    match family {
        WorkloadRoleFamily::Query => Some(grant_dynamic_sql_surface_sql()),
        WorkloadRoleFamily::Platform => Some(grant_platform_surface_sql()),
        _ => None,
    }
}

use wamn_role::APP_ROLE;

pub fn role_posture_sql() -> String {
    ensure_project_roles_sql().to_owned()
}

/// Keep database CONNECT privileges on individual logins. Shared ACL roles cannot connect.
pub fn privilege_sql(database: &str) -> String {
    let db = escape_identifier(database);
    format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE {db} FROM PUBLIC; \
         REVOKE CONNECT ON DATABASE {db} FROM {app};\n",
        app = escape_identifier(APP_ROLE),
    )
}

/// Grant each project login its database surface once CloudNativePG has created it.
pub async fn grant(
    project: &mut sqlx::PgConnection,
    connections: &[(
        WorkloadRoleFamily,
        wamn_postgres::database::ConnectionConfig,
    )],
) -> anyhow::Result<()> {
    for (family, connection) in connections {
        wamn_postgres::connection_readiness::wait_role(project, &connection.username).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            crate::project_setup::grant_workload_login_sql(
                *family,
                &connection.database,
                &connection.username,
            ),
        ))
        .execute(&mut *project)
        .await?;
    }
    Ok(())
}
