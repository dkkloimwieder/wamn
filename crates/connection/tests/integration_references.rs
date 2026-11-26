//! Each environment reference prevents deletion before authority changes.

use sqlx::Connection as _;
use wamn_connection::project_connections::Administration;

async fn refusal(relation: &str, expected: &str) {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let mut client = wamn_postgres::connect(postgres.admin_url()).await.unwrap();

    sqlx::raw_sql("CREATE SCHEMA platform; CREATE SCHEMA catalog; \
        CREATE TABLE platform.external_system(id uuid PRIMARY KEY); \
        CREATE TABLE platform.connection(id uuid PRIMARY KEY); \
        CREATE TABLE platform.record_source(external_system_id uuid REFERENCES platform.external_system(id)); \
        CREATE TABLE platform.file(connection_id uuid); \
        INSERT INTO platform.external_system VALUES ('00000000-0000-0000-0000-000000000001')").execute(&mut client).await.map(drop).unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {relation} VALUES ('00000000-0000-0000-0000-000000000001')"
    )))
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
    let mut system = wamn_postgres::connect(postgres.admin_url()).await.unwrap();
    let mut database = Administration::new(&mut tx, &mut system, None, None);
    let error = database
        .refuse_env_references(group, "00000000-0000-0000-0000-000000000001")
        .await
        .unwrap_err();
    assert_eq!(error.code, "referenced");
    assert_eq!(error.field, expected);
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn file_refuses_connection_deletion() {
    refusal("platform.file", "file").await;
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn record_refuses_external_system_deletion() {
    refusal("platform.record_source", "record").await;
}
