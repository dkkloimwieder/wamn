//! Test databases with the project catalog installed.

/// Create the roles that [`crate::CATALOG_SCHEMA_SQL`] grants to, with the
/// attributes that provisioning gives them, when they are absent.
const PROJECT_ROLES_SQL: &str = "DO $roles$ DECLARE role_name text; BEGIN \
       PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
       FOREACH role_name IN ARRAY ARRAY['wamn_app'] LOOP \
         IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
           EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                           NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
         END IF; \
       END LOOP; \
     END $roles$;";

/// Install the project catalog on a supplied test database.
pub async fn project(database: &str) {
    let mut connection = wamn_postgres::connect(database).await.unwrap();
    {
        let batches: &[&str] = &[PROJECT_ROLES_SQL, crate::CATALOG_SCHEMA_SQL];
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut connection)
                .await
                .expect("install the project catalog minimum");
        }
    }
}
