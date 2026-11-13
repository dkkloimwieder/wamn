//! Test databases with the control store installed.

use crate::CONTROL_PORTABLE_STORE_SQL;
use crate::SYSTEM_SCHEMA_SQL;
use crate::sql;

/// Create the control owner role with the attributes of the production bootstrap.
const SYSTEM_ROLE_SQL: &str = "DO $system_role$ BEGIN \
       PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_system') THEN \
         CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS; \
       END IF; \
     END $system_role$;";

/// Create a test database with the system base grants: the control store that a
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
        .expect("install the system control store minimum");
    database
}

/// Seed one named organization, project and environment for authority tests.
pub fn seed_org_project_env_sql(org: &str, project: &str, env: &str) -> String {
    use postgres_protocol::escape::escape_literal;
    let org = escape_literal(org);
    let project = escape_literal(project);
    let env = escape_literal(env);
    let actor = crate::PlatformComponent::Provisioning.principal_id();
    format!(
        "SELECT set_config('app.user_id', '{actor}', true); \
         INSERT INTO registry.orgs(id,placement_type) VALUES ({org},'dedicated') ON CONFLICT DO NOTHING; \
         INSERT INTO registry.projects(org,id) VALUES ({org},{project}) ON CONFLICT DO NOTHING; \
         INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) \
         VALUES ({org},{env},'\"own\"',0,1,'1Gi','1','1Gi','postgres:18') ON CONFLICT DO NOTHING; \
         WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ({org},{env},{env}) ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ({org},{project},{env},'s1','aaaaaaa1') ON CONFLICT DO NOTHING;"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_login_reads_and_writes_only_its_own_org() {
        let _lock = wamn_test_postgres::lock();
        let database = system();
        database
            .execute(&[
                &seed_org_project_env_sql("acme", "orders", "dev"),
                &seed_org_project_env_sql("other", "orders", "dev"),
            ])
            .unwrap();
        let login = crate::workload_generation_role(
            crate::WorkloadRoleFamily::Control,
            crate::WorkloadRoleIdentity::Org {
                org: "acme",
                database: database.name(),
            },
            crate::CredentialGeneration::A,
        )
        .unwrap();
        database.execute(&[&format!(
            "CREATE ROLE {login} LOGIN INHERIT NOSUPERUSER NOBYPASSRLS; \
             GRANT wamn_control TO {login}; SET SESSION AUTHORIZATION {login}; \
             SET ROLE wamn_control; \
             SELECT set_config('app.user_id','770df186-ac15-579e-b46b-c297cae2011b',false); \
             SELECT set_config('app.project_env_key','other--dev--orders',false); \
             DO $isolation$ BEGIN \
                 ASSERT EXISTS (SELECT FROM registry.projects WHERE org='acme'); \
                 ASSERT NOT EXISTS (SELECT FROM registry.projects WHERE org<>'acme'); \
                 INSERT INTO registry.projects(org,id) VALUES ('acme','allowed'); \
                 BEGIN \
                     INSERT INTO registry.projects(org,id) VALUES ('other','refused'); \
                     RAISE EXCEPTION 'cross-org write succeeded'; \
                 EXCEPTION WHEN insufficient_privilege THEN NULL; END; \
                 BEGIN \
                     PERFORM registry.configure_external_system('other','external_system.list','{{\"project\":\"orders\"}}'::jsonb); \
                     RAISE EXCEPTION 'cross-org function succeeded'; \
                 EXCEPTION WHEN insufficient_privilege THEN NULL; END; \
             END $isolation$; \
             RESET SESSION AUTHORIZATION; DROP ROLE {login};"
        )]).unwrap();
    }
}
