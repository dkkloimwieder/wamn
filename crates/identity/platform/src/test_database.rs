//! Test databases with the project system schema installed.

const APP_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/app-schema.sql");

/// Create a test database with the project+platform base grants: the project catalog
/// of [`wamn_catalog::test_database::project`], then `deploy/sql/app-schema.sql`.
///
/// # Panics
///
/// Panics when the test server cannot start or a schema does not install.
pub fn project_platform() -> wamn_test_postgres::Database {
    let database = wamn_catalog::test_database::project();
    database
        .execute(&[APP_SCHEMA_SQL])
        .expect("install the platform base grants");
    database
}
