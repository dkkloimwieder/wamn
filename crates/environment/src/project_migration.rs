//! One restricted migration login per project database.

/// The history table name includes both package and environment identity.
pub fn history_table(
    project_env_key: &wamn_registry::ProjectEnvKey,
    package: &wamn_package::PackageName,
) -> String {
    let digest = wamn_digest::canonical_json_sha256(&serde_json::json!([project_env_key, package]));
    format!("wamn_history.migration_{}", &digest.hex()[..40])
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
        principal = wamn_identity_protocol::PlatformComponent::ApplyPackage.principal_id(),
    )
}
