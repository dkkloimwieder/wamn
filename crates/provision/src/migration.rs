//! One restricted migration login per project database.

use anyhow::{Context as _, ensure};

/// The fixed owner login in a project Cluster.
pub fn login() -> wamn_role::LoginName {
    wamn_role::MIGRATION_ROLE
        .parse()
        .expect("fixed project owner")
}

/// Check the authenticated session before any package-controlled SQL runs.
pub async fn require_login(
    connection: &mut sqlx::PgConnection,
) -> anyhow::Result<wamn_role::LoginName> {
    let (session, current, restricted): (String, String, bool) = sqlx::query_as(
        "SELECT session_user::text, current_user::text, \
         NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls) \
         FROM pg_roles WHERE rolname = session_user",
    )
    .fetch_one(&mut *connection)
    .await?;
    let session = wamn_role::LoginName::try_from(session).context("decode the session login")?;
    let current = wamn_role::LoginName::try_from(current).context("decode the current login")?;
    let expected = login();
    ensure!(
        session == expected && current == session && restricted,
        "package SQL requires the direct restricted migration login"
    );
    let owns_database: bool = sqlx::query_scalar(
        "SELECT datdba = (SELECT oid FROM pg_roles WHERE rolname = session_user) FROM pg_database WHERE datname = current_database()",
    ).fetch_one(&mut *connection).await?;
    ensure!(owns_database, "migration login must own its database");
    Ok(session)
}

/// The history table name includes both package and environment identity.
pub fn history_table(
    project_env_key: &wamn_registry::ProjectEnvKey,
    package: &wamn_package::PackageName,
) -> String {
    let digest = wamn_digest::canonical_json_sha256(&serde_json::json!([project_env_key, package]));
    format!("wamn_history.migrations_{}", &digest.hex()[..40])
}

/// Grant runtime use and default access on one application model schema.
pub async fn grant_model_schema_access(
    connection: &mut sqlx::PgConnection,
    login: &wamn_role::LoginName,
    schema: &str,
) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT USAGE ON SCHEMA {schema} TO {app}, {query}, {platform}; \
         ALTER DEFAULT PRIVILEGES FOR ROLE {login} IN SCHEMA {schema} \
           GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {app}; \
         ALTER DEFAULT PRIVILEGES FOR ROLE {login} IN SCHEMA {schema} \
           GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO {app}; \
         ALTER DEFAULT PRIVILEGES FOR ROLE {login} IN SCHEMA {schema} \
           GRANT SELECT ON TABLES TO {query}; \
         ALTER DEFAULT PRIVILEGES FOR ROLE {login} REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;",
        schema = postgres_protocol::escape::escape_identifier(schema),
        login = postgres_protocol::escape::escape_identifier(login.as_str()),
        app = postgres_protocol::escape::escape_identifier(wamn_role::APP_ROLE),
        query = postgres_protocol::escape::escape_identifier(wamn_role::QUERY_ROLE),
        platform = postgres_protocol::escape::escape_identifier(wamn_role::PLATFORM_ROLE),
    )))
    .execute(connection)
    .await?;
    Ok(())
}

/// Register the actor for package migration sessions.
pub fn grants_sql(login: &wamn_role::LoginName) -> String {
    format!(
        "INSERT INTO platform.login_principal(login,principal_id) VALUES ({login}, '{principal}') ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id;",
        login = postgres_protocol::escape::escape_literal(login.as_str()),
        principal = crate::PlatformComponent::ApplyPackage.principal_id(),
    )
}

/// Create SQLx's history with platform-owned audit and immutable migration identity.
/// Only SQLx's elapsed-time update remains writable after insertion.
pub async fn prepare_history(
    connection: &mut sqlx::PgConnection,
    table: &str,
    login: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(&mut *connection)
        .await?;
    if !exists {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TABLE {table} (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
            success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL,
            created_by UUID NOT NULL,
            updated_at TIMESTAMPTZ NOT NULL,
            updated_by UUID NOT NULL
        );
        CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON {table}
            FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at','created_by','updated_at','updated_by');
        CREATE TRIGGER migration_immutable BEFORE UPDATE OR DELETE ON {table}
            FOR EACH ROW EXECUTE FUNCTION wamn_history.guard_sqlx_migration();")))
            .execute(&mut *connection).await?;
    }
    let role = postgres_protocol::escape::escape_identifier(login.as_str());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE (execution_time) ON {table} TO {role}"
    )))
    .execute(connection)
    .await?;
    Ok(())
}
