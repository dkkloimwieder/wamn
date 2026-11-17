//! Workload credential preparation, retirement, and observed role state.

use super::{WorkloadRoleFamily, escape_identifier, escape_literal, stable_surface_sql};

/// Idempotently create or harden one stable workload ACL role as NOLOGIN.
pub fn ensure_workload_acl_role_sql(family: WorkloadRoleFamily) -> String {
    if family == WorkloadRoleFamily::Migration {
        String::new()
    } else {
        super::ensure_project_roles_sql().to_owned()
    }
}

/// Grant database access after CNPG creates this login.
pub fn grant_workload_login_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &wamn_identity_protocol::LoginName,
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
        grants.push_str(&format!(
            "INSERT INTO platform.login_principal(login,principal_id) VALUES ({login}, '{principal}') ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id;",
            login = escape_literal(role.as_str()),
            principal = crate::PlatformComponent::Executor.principal_id(),
        ));
    }
    if family == WorkloadRoleFamily::Migration {
        grants.push_str(&format!(
            "ALTER DEFAULT PRIVILEGES FOR ROLE {} REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;",
            escape_identifier(role.as_str())
        ));
    }
    grants
}

/// Grant the dynamic SQL role reads without inheriting any application writer.
pub fn grant_dynamic_sql_surface_sql() -> String {
    format!(
        "{} DO $read$ DECLARE target record; BEGIN FOR target IN SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' AND nspname NOT IN ('information_schema', 'wamn_history') LOOP EXECUTE format('GRANT USAGE ON SCHEMA %I TO wamn_query', target.nspname); EXECUTE format('GRANT SELECT ON ALL TABLES IN SCHEMA %I TO wamn_query', target.nspname); END LOOP; FOR target IN SELECT c.oid::regclass AS relation FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='platform' AND c.relname IN ('credential','oauth_token','oauth_authorization') LOOP EXECUTE format('REVOKE ALL ON %s FROM wamn_query', target.relation); END LOOP; END $read$; REVOKE ALL ON platform.tx_identity, platform.login_principal FROM wamn_query; GRANT INSERT ON platform.tx_identity TO wamn_query; GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(), wamn_authority.current_role(), wamn_authority.current_application() TO wamn_query;",
        super::ensure_project_roles_sql()
    )
}

/// Grant the host's platform work and retention within one project database.
pub fn grant_platform_surface_sql() -> String {
    let role = escape_identifier(wamn_role::PLATFORM_ROLE);
    format!(
        "GRANT USAGE ON SCHEMA platform, catalog, wamn_run, wamn_history, wamn_authority TO {role}; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA platform, catalog, wamn_run TO {role}; \
         GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA platform, catalog, wamn_run TO {role}; \
         GRANT SELECT, INSERT, UPDATE ON wamn_history.pruning_watermarks TO {role}; \
         REVOKE ALL ON platform.login_principal FROM {role}; \
         GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA wamn_authority TO {role}; \
         GRANT EXECUTE ON FUNCTION wamn_history.row_image(record), \
           wamn_history.timestamptz_image(timestamptz) TO {role};",
    )
}
