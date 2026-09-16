//! Test databases with the project system schema installed.

const APP_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/app-schema.sql");

/// Create a test database with the tenant+app_system floor: the tenant catalog
/// of [`wamn_catalog::test_database::tenant`], then `deploy/sql/app-schema.sql`.
///
/// # Panics
///
/// Panics when the test server cannot start or a schema does not install.
pub fn tenant_app_system() -> wamn_test_postgres::Database {
    let database = wamn_catalog::test_database::tenant();
    database
        .execute(&[APP_SCHEMA_SQL])
        .expect("install the app_system floor");
    database
}

#[cfg(test)]
mod tests {
    #[test]
    fn tenant_app_system_floor_installs_both_schemas() {
        let database = super::tenant_app_system();
        assert_eq!(
            database
                .execute(&["SELECT to_regclass('catalog.packages') IS NOT NULL \
                       AND to_regclass('app_system.users') IS NOT NULL",])
                .unwrap()
                .trim(),
            "t"
        );
    }
}
