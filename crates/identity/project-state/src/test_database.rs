//! Test databases with the project system schema installed.

const APP_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/app-schema.sql");

/// Create a test database with the tenant+platform floor: the tenant catalog
/// of [`wamn_catalog::test_database::tenant`], then `deploy/sql/app-schema.sql`.
///
/// # Panics
///
/// Panics when the test server cannot start or a schema does not install.
pub fn tenant_platform() -> wamn_test_postgres::Database {
    let database = wamn_catalog::test_database::tenant();
    database
        .execute(&[APP_SCHEMA_SQL])
        .expect("install the platform floor");
    database
}

#[cfg(test)]
mod tests {
    #[test]
    fn tenant_platform_floor_installs_both_schemas() {
        let database = super::tenant_platform();
        assert_eq!(
            database
                .execute(&["SELECT to_regclass('catalog.packages') IS NOT NULL \
                       AND to_regclass('platform.\"user\"') IS NOT NULL",])
                .unwrap()
                .trim(),
            "t"
        );
    }
}
