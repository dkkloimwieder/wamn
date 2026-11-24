//! Test databases with the control store installed, and the logins that CNPG manages.

use postgres_protocol::escape::{escape_identifier, escape_literal};
use wamn_identity_protocol::workload_role::WorkloadRoleFamily;
use wamn_postgres::migration_scope::MigrationDatabase;
use wamn_principal::PlatformComponent;

/// Create the control owner role with the attributes of the production bootstrap.
const SYSTEM_ROLE_SQL: &str = "DO $system_role$ BEGIN \
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_system') THEN \
         CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS; \
       END IF; \
     END $system_role$;";

/// Install the full control store, owned by `wamn_system`, on the database of `url`.
pub async fn system(url: &str) {
    let mut connection = wamn_postgres::connect(url).await.unwrap();
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let owner = format!(
        "ALTER DATABASE {} OWNER TO wamn_system",
        escape_identifier(&name)
    );
    for batch in [
        SYSTEM_ROLE_SQL,
        wamn_postgres::project_setup::ensure_project_roles_sql(),
        &owner,
        "SET ROLE wamn_system",
        wamn_platform_install::SYSTEM_SCHEMA_SQL,
        wamn_platform_install::CONTROL_PORTABLE_STORE_SQL,
        "RESET ROLE",
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(batch))
            .execute(&mut connection)
            .await
            .expect("install the control store");
    }
}

/// Create a password login of `family` as CNPG would, with the grants that provisioning adds.
pub fn login_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &wamn_role::LoginName,
    password: &str,
) -> String {
    let login = escape_identifier(role.as_str());
    format!(
        "{} DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname={}) THEN CREATE ROLE {login} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$; \
         ALTER ROLE {login} PASSWORD {}; GRANT {} TO {login} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; {}",
        wamn_postgres::project_setup::ensure_workload_acl_role_sql(family),
        escape_literal(role.as_str()),
        escape_literal(password),
        escape_identifier(family.acl_role()),
        wamn_postgres::project_setup::grant_workload_login_sql(family, database, role)
    )
}

/// Create the platform migration login of `scope` as CNPG would, and return its URL.
pub async fn platform_migration_input(input: &str, scope: MigrationDatabase) -> String {
    let options = wamn_postgres::connect_options(input).await.unwrap();
    let database = options.get_database().unwrap();
    let login = scope.login(database);
    let name = escape_identifier(login.as_str());
    let mut admin = wamn_postgres::connect(input).await.unwrap();
    let roles = match scope {
        MigrationDatabase::System => SYSTEM_ROLE_SQL,
        MigrationDatabase::Project => wamn_postgres::project_setup::ensure_project_roles_sql(),
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(roles))
        .execute(&mut admin)
        .await
        .unwrap();
    let owner = if scope == MigrationDatabase::System {
        wamn_role::SYSTEM_ROLE
    } else {
        wamn_role::PLATFORM_ROLE
    };
    // CNPG grants this membership with SET permission, which ownership transfers require.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname={}) THEN CREATE ROLE {name} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $$; \
         ALTER ROLE {name} PASSWORD 'platform-migration-fixture'; \
         GRANT {} TO {name} WITH INHERIT TRUE, SET TRUE; GRANT CONNECT ON DATABASE {} TO {name}",
        escape_literal(login.as_str()),
        escape_identifier(owner),
        escape_identifier(database)
    )))
    .execute(&mut admin)
    .await
    .unwrap();
    if scope == MigrationDatabase::Project {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER DATABASE {} OWNER TO {name}",
            escape_identifier(database)
        )))
        .execute(&mut admin)
        .await
        .unwrap();
    }
    let identity_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('platform.login_principal') IS NOT NULL")
            .fetch_one(&mut admin)
            .await
            .unwrap();
    if identity_exists {
        sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES($1,$2) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
            .bind(login.as_str())
            .bind(sqlx::types::Uuid::from(PlatformComponent::ApplyPackage.principal_id()))
            .execute(&mut admin)
            .await
            .unwrap();
    }
    let mut url = url::Url::parse(input).unwrap();
    url.set_username(login.as_str()).unwrap();
    url.set_password(Some("platform-migration-fixture"))
        .unwrap();
    url.into()
}
