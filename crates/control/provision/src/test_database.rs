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

/// Arrange a restricted package login for a local PostgreSQL fixture.
pub async fn package_migration_input(
    input: impl AsRef<str>,
    org: &str,
    project: &str,
    environment: &str,
) -> anyhow::Result<String> {
    let input = input.as_ref();
    let options = wamn_postgres::connect_options(input).await?;
    let database = options
        .get_database()
        .ok_or_else(|| anyhow::anyhow!("fixture database is absent"))?;
    let key = wamn_registry::project_env_key(org, project, environment);
    let login = crate::migration::login(&key, database)?;
    let mut admin = wamn_postgres::connect(input).await?;
    let role = postgres_protocol::escape::escape_identifier(&login);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname={}) THEN CREATE ROLE {role} LOGIN PASSWORD 'migration-fixture' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$; GRANT CONNECT ON DATABASE {} TO {role}; GRANT {role} TO wamn_db_owner WITH INHERIT TRUE, SET TRUE",
        postgres_protocol::escape::escape_literal(&login), postgres_protocol::escape::escape_identifier(database))))
        .execute(&mut admin).await?;
    let protected: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT FROM pg_event_trigger WHERE evtname='wamn_migration_privileges')",
    )
    .fetch_one(&mut admin)
    .await?;
    if !protected {
        sqlx::raw_sql(crate::MIGRATION_PRIVILEGES_SQL)
            .execute(&mut admin)
            .await?;
    }
    let mut url = url::Url::parse(input)?;
    url.set_username(&login)
        .map_err(|()| anyhow::anyhow!("fixture login"))?;
    url.set_password(Some("migration-fixture"))
        .map_err(|()| anyhow::anyhow!("fixture password"))?;
    Ok(url.into())
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
        let login = crate::workload_login(
            crate::WorkloadRoleFamily::Control,
            crate::WorkloadRoleIdentity::Org {
                org: "acme",
                database: database.name(),
            },
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

/// Install a role for an isolated database interface test. Production roles belong to CNPG.
pub fn login_sql(
    family: crate::WorkloadRoleFamily,
    database: &str,
    role: &str,
    password: &str,
) -> String {
    use postgres_protocol::escape::{escape_identifier, escape_literal};
    format!(
        "{} CREATE ROLE {} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD {}; {} {}",
        crate::sql::ensure_workload_acl_role_sql(family),
        escape_identifier(role),
        escape_literal(password),
        format!(
            "{} {}",
            membership_sql(family, role, true),
            sql::platform_group_membership_sql(family)
        ),
        crate::sql::grant_workload_login_sql(family, database, role)
    )
}

/// Set one role membership for a database authority test.
pub fn membership_sql(family: crate::WorkloadRoleFamily, role: &str, active: bool) -> String {
    use postgres_protocol::escape::escape_identifier;
    let role = escape_identifier(role);
    let acl = escape_identifier(family.acl_role());
    if active {
        format!("GRANT {acl} TO {role} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE")
    } else {
        format!("REVOKE {acl} FROM {role}")
    }
}

/// Install the issuer role for a service interface test.
pub fn issuer_login_sql(issuer: &str, password: &str) -> anyhow::Result<String> {
    use postgres_protocol::escape::{escape_identifier, escape_literal};
    let role = crate::identity_role::identity_issuer_login(issuer)?;
    Ok(format!(
        "{} CREATE ROLE {} LOGIN INHERIT PASSWORD {}; GRANT wamn_identity_issuer TO {} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; GRANT CONNECT ON DATABASE wamn_system TO {};",
        crate::identity_role::grant_identity_issuer_surface_sql(),
        escape_identifier(&role),
        escape_literal(password),
        escape_identifier(&role),
        escape_identifier(&role)
    ))
}

/// Install an isolated replication fixture. CNPG owns production logins.
pub fn replication_login_sql(role: &str, password: &str) -> String {
    use postgres_protocol::escape::{escape_identifier, escape_literal};
    format!(
        "DO $fixture$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname={name}) THEN CREATE ROLE {role} LOGIN REPLICATION NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS PASSWORD {password}; END IF; END $fixture$;",
        name = escape_literal(role),
        role = escape_identifier(role),
        password = escape_literal(password)
    )
}

/// Arrange the same restricted platform identity when a fixture supplies PostgreSQL instead of CNPG.
pub async fn platform_migration_input(
    input: impl AsRef<str>,
    scope: crate::migrations::MigrationDatabase,
) -> anyhow::Result<String> {
    let input = input.as_ref();
    let options = wamn_postgres::connect_options(input).await?;
    let database = options
        .get_database()
        .ok_or_else(|| anyhow::anyhow!("fixture database is absent"))?;
    let role = scope.managed_role(database);
    let name = postgres_protocol::escape::escape_identifier(&role.name);
    let mut admin = wamn_postgres::connect(input).await?;
    match scope {
        crate::migrations::MigrationDatabase::System => {
            sqlx::raw_sql(SYSTEM_ROLE_SQL).execute(&mut admin).await?;
        }
        crate::migrations::MigrationDatabase::Project => {
            sqlx::raw_sql(sql::ensure_db_owner_role_sql())
                .execute(&mut admin)
                .await?;
        }
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname={}) THEN CREATE ROLE {name} LOGIN PASSWORD 'platform-migration-fixture' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$; GRANT {} TO {name} WITH INHERIT TRUE, SET FALSE; GRANT CONNECT ON DATABASE {} TO {name}",
        postgres_protocol::escape::escape_literal(&role.name), scope.owner_role(), postgres_protocol::escape::escape_identifier(database))))
        .execute(&mut admin).await?;
    let mut url = url::Url::parse(input)?;
    url.set_username(&role.name)
        .map_err(|()| anyhow::anyhow!("fixture login"))?;
    url.set_password(Some("platform-migration-fixture"))
        .map_err(|()| anyhow::anyhow!("fixture password"))?;
    Ok(url.into())
}
