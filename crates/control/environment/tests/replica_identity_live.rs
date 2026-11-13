//! Server-answer test for package-model replica-identity reconciliation.

use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_environment_control::reconcile_replica_identity::reconcile;
use wamn_schema_control::ManagedModel;
use wamn_test_infrastructure::locked_database;

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to disposable PostgreSQL");

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

async fn identity(client: &mut Client, table: &str) -> String {
    sqlx::query(
        "SELECT class.relreplident::text \
               FROM pg_catalog.pg_class AS class \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace \
              WHERE namespace.nspname = 'ri_data' AND class.relname = $1",
    )
    .bind(&table)
    .fetch_one(&mut *client)
    .await
    .expect("read server replica identity")
    .get(0)
}

async fn install(client: &mut Client) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE SCHEMA ri_data; \
             CREATE TABLE ri_data.orders (id bigint PRIMARY KEY, project_env_key text NOT NULL); \
             CREATE TABLE ri_data.lines (id bigint PRIMARY KEY, project_env_key text NOT NULL);")
            .to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("install RI application tables");
}

#[tokio::test]
async fn captured_models_stay_full_without_registration_storage() {
    let url = locked_database::database(wamn_catalog::test_database::project);
    let mut client = connect(&url).await;
    install(&mut client).await;
    sqlx::raw_sql("DROP TABLE IF EXISTS catalog.event_registrations")
        .execute(&mut client)
        .await
        .map(drop)
        .expect("remove retired registration storage");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::create_publication_sql("captured", "ri_data")).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    reconcile(&mut client, &models(), true)
        .await
        .expect("capture requires FULL without registrations");
    assert_eq!(identity(&mut client, "orders").await, "f");
    assert_eq!(identity(&mut client, "lines").await, "f");
    assert!(
        reconcile(&mut client, &models(), true)
            .await
            .unwrap()
            .is_noop()
    );

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA IF EXISTS ri_data CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("clean RI schemas");
}
