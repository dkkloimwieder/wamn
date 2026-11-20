//! Test databases with the project catalog installed.

/// Create the roles that [`crate::CATALOG_SCHEMA_SQL`] grants to, with the
/// attributes that provisioning gives them, when they are absent.
fn project_roles_sql() -> String {
    format!(
        "DO $roles$ DECLARE role_name text; BEGIN \
       PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
       FOREACH role_name IN ARRAY ARRAY['{}', '{}', '{}'] LOOP \
         IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
           EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                           NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
         END IF; \
       END LOOP; \
     END $roles$;",
        wamn_role::APP_ROLE,
        wamn_role::QUERY_ROLE,
        wamn_role::PLATFORM_ROLE
    )
}

/// Install the project catalog on a supplied test database.
pub async fn project(connection: &mut sqlx::PgConnection) {
    {
        let batches: &[&str] = &[&project_roles_sql(), crate::CATALOG_SCHEMA_SQL];
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut *connection)
                .await
                .expect("install the project catalog minimum");
        }
    }
}
