//! Workload credential preparation, retirement, and observed role state.

use super::{WorkloadRoleFamily, escape_identifier, escape_literal, stable_surface_sql};

/// Idempotently create or harden one stable workload ACL role as NOLOGIN.
pub fn ensure_workload_acl_role_sql(family: WorkloadRoleFamily) -> String {
    if family == WorkloadRoleFamily::App {
        super::ensure_app_acl_role_sql()
    } else {
        ensure_acl_role_sql(family.acl_role())
    }
}

pub(crate) fn ensure_acl_role_sql(role: &str) -> String {
    format!(
        "DO $workload_acl$ DECLARE role_name text := {role_lit}; BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
           IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
             EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
           ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name \
                         AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                              OR rolinherit OR rolreplication OR rolbypassrls)) THEN \
             RAISE EXCEPTION 'invalid ACL role %', role_name; \
           END IF; \
         END $workload_acl$;",
        role_lit = escape_literal(role),
    )
}

/// Grant database access after CNPG creates this login.
pub fn grant_workload_login_sql(family: WorkloadRoleFamily, database: &str, role: &str) -> String {
    let privileges = if family == WorkloadRoleFamily::Migration {
        "CONNECT, CREATE"
    } else {
        "CONNECT"
    };
    let mut grants = format!(
        "{} GRANT {privileges} ON DATABASE {} TO {};",
        stable_surface_sql(family).unwrap_or_else(|| ensure_workload_acl_role_sql(family)),
        escape_identifier(database),
        escape_identifier(role)
    );
    if family == WorkloadRoleFamily::Migration {
        grants.push_str(&crate::migration::default_privileges_sql(role));
    }
    grants
}

/// Grant the dynamic SQL role reads without inheriting any application writer.
pub fn grant_dynamic_sql_surface_sql() -> String {
    format!(
        "{} DO $read$ DECLARE target record; BEGIN FOR target IN SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' AND nspname NOT IN ('information_schema', 'wamn_history') LOOP EXECUTE format('GRANT USAGE ON SCHEMA %I TO wamn_query', target.nspname); EXECUTE format('GRANT SELECT ON ALL TABLES IN SCHEMA %I TO wamn_query', target.nspname); END LOOP; END $read$; REVOKE ALL ON platform.tx_identity, platform.login_principal FROM wamn_query; GRANT INSERT ON platform.tx_identity TO wamn_query; GRANT EXECUTE ON FUNCTION wamn_authority.current_identity(), wamn_authority.current_principal_id(), wamn_authority.current_role(), wamn_authority.current_application() TO wamn_query; GRANT EXECUTE ON FUNCTION wamn_authority.project_key(text), wamn_authority.current_project_key() TO wamn_query;",
        ensure_acl_role_sql("wamn_query")
    )
}
