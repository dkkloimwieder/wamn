//! Each environment reference prevents deletion before authority changes.

use wamn_platform_identity::connections::references::refuse_environment_references;

async fn refusal(relation: &str, expected: &str) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (mut client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(driver);
    client.batch_execute("CREATE SCHEMA platform; CREATE SCHEMA catalog; \
        CREATE TABLE platform.external_system(id uuid PRIMARY KEY); \
        CREATE TABLE platform.record_source(external_system_id uuid REFERENCES platform.external_system(id)); \
        CREATE TABLE platform.file(connection_id uuid); \
        CREATE TABLE catalog.connection_binding(connection_id uuid); \
        CREATE TABLE catalog.release_connection(connection_id uuid); \
        INSERT INTO platform.external_system VALUES ('00000000-0000-0000-0000-000000000001')").await.unwrap();
    client
        .batch_execute(&format!(
            "INSERT INTO {relation} VALUES ('00000000-0000-0000-0000-000000000001')"
        ))
        .await
        .unwrap();
    let tx = client.transaction().await.unwrap();
    let group = if expected == "record" {
        "external_system"
    } else {
        "connection"
    };
    let error =
        refuse_environment_references(&tx, group, "00000000-0000-0000-0000-000000000001", "erp")
            .await
            .unwrap_err();
    assert_eq!(error.code, "referenced");
    assert_eq!(error.field, expected);
}

#[tokio::test]
async fn binding_refuses_connection_deletion() {
    refusal("catalog.connection_binding", "binding").await;
}

#[tokio::test]
async fn release_version_refuses_connection_deletion() {
    refusal("catalog.release_connection", "release_connection").await;
}

#[tokio::test]
async fn file_refuses_connection_deletion() {
    refusal("platform.file", "file").await;
}

#[tokio::test]
async fn record_refuses_external_system_deletion() {
    refusal("platform.record_source", "record").await;
}
