//! One restricted migration login per project database.

use crate::{WorkloadRoleFamily, WorkloadRoleIdentity};
use anyhow::ensure;

pub const OWNER_ROLE: &str = "wamn_app_owner";
pub const LOGIN_PREFIX: &str = "wamn_migration";

/// Derive the single migration identity from the database and environment.
pub fn login(project_env_key: &str, database: &str) -> anyhow::Result<String> {
    Ok(crate::workload_login(
        WorkloadRoleFamily::Migration,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database,
        },
    )?)
}

/// Public CNPG configuration for the migration role, with no borrowed owner password.
/// Explicit operator connections must supply this role's own credential separately.
pub fn configuration(
    input: &str,
    org: &str,
    project: &str,
    environment: &str,
) -> anyhow::Result<String> {
    let input = wamn_postgres::configuration(input)?;
    if !input.trim_start().starts_with('{') {
        // Preserve the authenticated identity, password, and native TLS parameters.
        return Ok(input.into_owned());
    }
    let mut config: wamn_postgres::ConnectionConfig = serde_json::from_str(&input)?;
    let key = wamn_registry::project_env_key(org, project, environment);
    config.username = login(&key, &config.database)?;
    config.credentials = wamn_postgres::SecretRef {
        namespace: org.into(),
        name: crate::workload_secret_name(WorkloadRoleFamily::Migration, org, project, environment),
    };
    config.to_json()
}

/// Check the authenticated session before any package-controlled SQL runs.
pub async fn require_login(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
) -> anyhow::Result<String> {
    let (session, current, database, restricted): (String, String, String, bool) = sqlx::query_as(
        "SELECT session_user::text, current_user::text, current_database()::text, \
         NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls) \
         FROM pg_roles WHERE rolname = session_user",
    )
    .fetch_one(&mut *connection)
    .await?;
    let expected = login(project_env_key, &database)?;
    ensure!(
        session == expected && current == session && restricted,
        "package SQL requires the direct restricted migration login"
    );
    let protected: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_event_trigger WHERE evtname='wamn_migration_privileges' AND evtevent='ddl_command_start' AND evtenabled IN ('O','A') AND evttags @> ARRAY['GRANT','REVOKE','ALTER DEFAULT PRIVILEGES']::text[])")
        .fetch_one(connection).await?;
    ensure!(
        protected,
        "project migration privilege protection is absent"
    );
    Ok(session)
}

/// The history table name includes both package and environment identity.
pub fn history_table(project_env_key: &str, package: &str) -> String {
    let digest = wamn_digest::canonical_json_sha256(&serde_json::json!([project_env_key, package]));
    format!("wamn_history.migrations_{}", &digest[7..47])
}

/// Trusted grants required by compiler-generated history and proper foreign keys.
pub fn grants_sql(login: &str) -> String {
    let role = postgres_protocol::escape::escape_identifier(login);
    format!(
        "GRANT USAGE, CREATE ON SCHEMA wamn_history TO {role}; \
        GRANT USAGE ON SCHEMA platform TO {role}; \
        GRANT REFERENCES ON platform.file, platform.external_system TO {role}; \
        GRANT EXECUTE ON FUNCTION wamn_history.stamp_row(), \
          wamn_history.create_history_table(text,text,boolean), \
          wamn_history.row_image(record), wamn_history.timestamptz_image(timestamptz), \
          wamn_history.log_row_change() TO {role}; \
        ALTER DEFAULT PRIVILEGES FOR ROLE {role} REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;"
    )
}

/// Create SQLx's history with platform-owned audit and immutable migration identity.
/// Only SQLx's elapsed-time update remains writable after insertion.
pub async fn prepare_history(
    connection: &mut sqlx::PgConnection,
    table: &str,
    login: &str,
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
    let role = postgres_protocol::escape::escape_identifier(login);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE (execution_time) ON {table} TO {role}"
    )))
    .execute(connection)
    .await?;
    Ok(())
}
