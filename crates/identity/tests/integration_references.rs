//! Each environment reference prevents deletion before authority changes.

use sqlx::Connection as _;
use wamn_platform_identity::connections::references::refuse_environment_references;

async fn refusal(pool: &sqlx::PgPool, relation: &str, expected: &str) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(pool);
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();

    sqlx::raw_sql("CREATE SCHEMA platform; CREATE SCHEMA catalog; \
        CREATE TABLE platform.external_system(id uuid PRIMARY KEY); \
        CREATE TABLE platform.connection(id uuid PRIMARY KEY); \
        CREATE TABLE platform.record_source(external_system_id uuid REFERENCES platform.external_system(id)); \
        CREATE TABLE platform.file(connection_id uuid); \
        CREATE TABLE catalog.connection_binding(connection_id uuid); \
        CREATE TABLE catalog.release_connection(connection_id uuid); \
        INSERT INTO platform.external_system VALUES ('00000000-0000-0000-0000-000000000001')").execute(&mut client).await.map(drop).unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!("INSERT INTO {relation} VALUES ('00000000-0000-0000-0000-000000000001')"))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let mut tx = client.begin().await.unwrap();
    let group = if expected == "record" {
        "external_system"
    } else {
        "connection"
    };
    let error =
        refuse_environment_references(&mut tx, group, "00000000-0000-0000-0000-000000000001")
            .await
            .unwrap_err();
    assert_eq!(error.code, "referenced");
    assert_eq!(error.field, expected);
}

#[sqlx::test(migrations = false)]
async fn binding_refuses_connection_deletion(pool: sqlx::PgPool) {
    refusal(&pool, "catalog.connection_binding", "binding").await;
}

#[sqlx::test(migrations = false)]
async fn release_version_refuses_connection_deletion(pool: sqlx::PgPool) {
    refusal(&pool, "catalog.release_connection", "release_connection").await;
}

#[sqlx::test(migrations = false)]
async fn file_refuses_connection_deletion(pool: sqlx::PgPool) {
    refusal(&pool, "platform.file", "file").await;
}

#[sqlx::test(migrations = false)]
async fn record_refuses_external_system_deletion(pool: sqlx::PgPool) {
    refusal(&pool, "platform.record_source", "record").await;
}
