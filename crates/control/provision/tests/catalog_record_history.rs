//! Catalog stamps use the same actor contract as application records.

#[sqlx::test(migrations = false)]
async fn catalog_stamps_require_an_actor_and_preserve_creation(pool: sqlx::PgPool) {
    let database = wamn_test_postgres::url(&pool);
    wamn_catalog::test_database::project(database.as_str()).await;
    let principals = format!(
        "BEGIN; {} COMMIT;",
        wamn_control_provision::platform_principals_sql("example.invalid").unwrap()
    );
    let mut batches = vec![
        include_str!("../../../../deploy/sql/app-schema.sql"),
        &principals,
    ];
    batches.extend(
        include_str!("fixtures/catalog_record_history.sql").split("-- transaction boundary"),
    );
    let mut connection = wamn_postgres::connect(database.as_str()).await.unwrap();
    for batch in batches {
        sqlx::raw_sql(sqlx::AssertSqlSafe(batch.to_owned()))
            .execute(&mut connection)
            .await
            .unwrap();
    }
}
