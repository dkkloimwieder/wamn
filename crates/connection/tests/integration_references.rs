//! Project records keep their external system references.

use serde_json::json;
use sqlx::Connection as _;
use wamn_connection::{administration::configure, project_connections::Administration};

#[tokio::test]
#[ignore = "requires: docker"]
async fn record_refuses_external_system_deletion() -> anyhow::Result<()> {
    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let mut client = wamn_postgres::connect(postgres.admin_url()).await?;

    sqlx::raw_sql(
        "CREATE SCHEMA platform;
         CREATE TABLE platform.external_system(id uuid PRIMARY KEY DEFAULT uuidv7(), name text UNIQUE);
         CREATE TABLE platform.record_source(external_system_id uuid REFERENCES platform.external_system(id));
         INSERT INTO platform.external_system (name) VALUES ('source');
         INSERT INTO platform.record_source SELECT id FROM platform.external_system WHERE name='source';",
    )
    .execute(&mut client)
    .await?;
    let mut tx = client.begin().await?;
    let mut system = wamn_postgres::connect(postgres.admin_url()).await?;
    let mut database = Administration::new(&mut tx, &mut system, None);
    let error = configure(
        &mut database,
        &"acme".parse()?,
        "external_system.delete",
        &json!({"project":"app", "env":"dev", "name":"source"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "referenced");
    assert_eq!(error.field, "record");
    Ok(())
}
