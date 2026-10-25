//! Server-answer test for package-model replica-identity reconciliation.

use tokio_postgres::{Client, NoTls};
use wamn_control::reconcile_replica_identity::reconcile;
use wamn_schema_control::ManagedModel;
use wamn_test_infrastructure::locked_database;

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

fn models() -> Vec<ManagedModel> {
    ["orders", "lines"]
        .map(|table| ManagedModel {
            model_id: table.to_owned(),
            schema: "ri_data".to_owned(),
            table: table.to_owned(),
        })
        .into_iter()
        .collect()
}

async fn identity(client: &Client, table: &str) -> String {
    client
        .query_one(
            "SELECT class.relreplident::text \
               FROM pg_catalog.pg_class AS class \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace \
              WHERE namespace.nspname = 'ri_data' AND class.relname = $1",
            &[&table],
        )
        .await
        .expect("read server replica identity")
        .get(0)
}

async fn install(client: &Client) {
    client
        .batch_execute(
            "CREATE SCHEMA ri_data; \
             CREATE TABLE ri_data.orders (id bigint PRIMARY KEY, tenant_id text NOT NULL); \
             CREATE TABLE ri_data.lines (id bigint PRIMARY KEY, tenant_id text NOT NULL);",
        )
        .await
        .expect("install RI application tables");
}

#[tokio::test]
async fn captured_models_stay_full_without_registration_storage() {
    let url = locked_database::database(wamn_catalog::test_database::tenant);
    let client = connect(&url).await;
    install(&client).await;
    client
        .batch_execute("DROP TABLE IF EXISTS catalog.event_registrations")
        .await
        .expect("remove retired registration storage");
    client
        .batch_execute(&wamn_control_provision::sql::create_publication_sql(
            "captured", "ri_data",
        ))
        .await
        .unwrap();
    reconcile(&client, &models(), true)
        .await
        .expect("capture requires FULL without registrations");
    assert_eq!(identity(&client, "orders").await, "f");
    assert_eq!(identity(&client, "lines").await, "f");
    assert!(reconcile(&client, &models(), true).await.unwrap().is_noop());

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS ri_data CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE;",
        )
        .await
        .expect("clean RI schemas");
}
