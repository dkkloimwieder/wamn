//! Project system schema setup for tests.

const APP_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/app-schema.sql");

/// Install the project catalog and platform schema on a supplied database.
pub async fn project_platform(database: &str) {
    let mut connection = wamn_postgres::connect(database).await.unwrap();
    wamn_catalog::test_database::project(&mut connection).await;
    sqlx::raw_sql(APP_SCHEMA_SQL)
        .execute(&mut connection)
        .await
        .expect("install the platform base grants");
}
