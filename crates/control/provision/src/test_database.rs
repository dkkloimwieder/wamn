//! Test databases with the control store installed.

use crate::{CONTROL_PORTABLE_STORE_SQL, SYSTEM_SCHEMA_SQL, sql};

/// Create the control owner role with the attributes of the production bootstrap.
const SYSTEM_ROLE_SQL: &str = "DO $system_role$ BEGIN \
       PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_system') THEN \
         CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS; \
       END IF; \
     END $system_role$;";

/// Create a test database with the system floor: the control store that a
/// fresh control database has.
///
/// The database is owned by `wamn_system`, and [`SYSTEM_SCHEMA_SQL`] and
/// [`CONTROL_PORTABLE_STORE_SQL`] are applied as `wamn_system`, as the control
/// bootstrap applies them.
///
/// # Panics
///
/// Panics when the test server cannot start or the control store does not install.
pub fn system() -> wamn_test_postgres::Database {
    let database = wamn_test_postgres::database();
    database
        .execute(&[
            SYSTEM_ROLE_SQL,
            &sql::ensure_control_author_acl_role_sql(),
            sql::ensure_db_owner_role_sql(),
            &format!(
                "ALTER DATABASE \"{}\" OWNER TO wamn_system",
                database.name()
            ),
            "SET ROLE wamn_system",
            SYSTEM_SCHEMA_SQL,
            CONTROL_PORTABLE_STORE_SQL,
            "RESET ROLE",
            &sql::grant_control_surface_sql(),
        ])
        .expect("install the system control store floor");
    database
}

/// Seed one named organization, project and environment for authority tests.
pub fn seed_org_project_env_sql(org: &str, project: &str, env: &str) -> String {
    use wamn_pg_core::quote_literal;
    let org = quote_literal(org);
    let project = quote_literal(project);
    let env = quote_literal(env);
    let actor = crate::PlatformComponent::Provisioning.principal_id();
    format!(
        "SELECT set_config('app.user_id', '{actor}', true); \
         INSERT INTO registry.orgs(id,placement_type) VALUES ({org},'dedicated') ON CONFLICT DO NOTHING; \
         INSERT INTO registry.projects(org,id) VALUES ({org},{project}) ON CONFLICT DO NOTHING; \
         INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) \
         VALUES ({org},{env},'\"own\"',0,1,'1Gi','1','1Gi','postgres:18') ON CONFLICT DO NOTHING; \
         INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) \
         VALUES ({org},{project},{env},'s1','aaaaaaa1',{env}) ON CONFLICT DO NOTHING;"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn system_floor_installs_the_control_store_as_its_owner() {
        let database = super::system();
        assert_eq!(
            database
                .execute(&[
                    "SELECT pg_catalog.pg_get_userbyid(nspowner) FROM pg_catalog.pg_namespace \
                     WHERE nspname IN ('registry', 'catalog') ORDER BY nspname",
                ])
                .unwrap()
                .trim(),
            "wamn_system\nwamn_system"
        );
    }
}
