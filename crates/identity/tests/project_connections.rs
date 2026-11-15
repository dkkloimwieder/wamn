//! Host storage constraints and release record history on PostgreSQL.

#[sqlx::test(migrations = false)]
async fn host_storage_and_release_history(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_platform_identity::test_database::project_platform(database.as_str()).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!(
        "fixtures/connections.sql"
    )))
    .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
    .await
    .unwrap();
}
