//! Live proofs for the two current REPLICA IDENTITY repair callers.
//!
//! Run against a fresh PostgreSQL 18 database through `WAMN_CTL_PG_URL` with
//! `--ignored --test-threads=1`. The tests drive the real `migrate-catalog`
//! post-commit hook and the real one-shot `reconcile-replica-identity` command.
//! The retired periodic CronJob and refused `publish-catalog` path are not test
//! subjects (wamn-0h0g.12.70).

mod support;

use tokio_postgres::{Client, NoTls};

use wamn_ctl::{migrate_catalog, reconcile_replica_identity};
use wamn_schema_compiler::Migration;

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const DATA_SCHEMA: &str = "riorch_data";
const CATALOG_ID: &str = "riorch";

fn catalog_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","catalog-id":"{CATALOG_ID}","version":1,"entities":[
          {{"id":"orders","name":"sales_orders","fields":[{{"id":"status","name":"status","type":{{"kind":"text"}}}}]}},
          {{"id":"lines","name":"line_items","fields":[{{"id":"qty","name":"qty","type":{{"kind":"int"}}}}]}}
        ]}}"#
    )
}

fn catalog() -> wamn_schema_model::Catalog {
    wamn_schema_model::Catalog::from_json(&catalog_json()).expect("catalog parses")
}

fn write_catalog(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, catalog_json()).expect("write catalog fixture");
    path
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn reset(client: &Client) {
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
               THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app'; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') \
               THEN CREATE ROLE wamn_scenario_author NOLOGIN; END IF; END $$;"
        ))
        .await
        .expect("reset schemas and ensure wamn_app");
    client
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply catalog storage");
}

async fn teardown(client: &Client) {
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
        ))
        .await
        .expect("teardown");
}

fn registration_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","registration-id":"delete-orders","catalog-id":"{CATALOG_ID}",
           "flow-id":"purge","entity":"orders","ops":["delete"],"condition":null}}"#
    )
}

async fn seed_delete_registration(client: &Client) {
    let registration = registration_json();
    client
        .execute(
            "INSERT INTO catalog.event_registrations \
               (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
             VALUES ('tenant-a', $1, 'delete-orders', 'purge', 'orders', $2::text::jsonb)",
            &[&CATALOG_ID, &registration],
        )
        .await
        .expect("seed delete registration");
}

async fn relreplident(client: &Client, table: &str) -> String {
    client
        .query_one(
            "SELECT c.relreplident::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
            &[&DATA_SCHEMA, &table],
        )
        .await
        .expect("read relreplident")
        .get(0)
}

fn migrate_args(target: std::path::PathBuf, url: &str) -> migrate_catalog::MigrateCatalogArgs {
    migrate_catalog::MigrateCatalogArgs {
        admin_database_url: url.to_string(),
        tenant: "tenant-a".to_string(),
        environment: "dev".to_string(),
        schema: DATA_SCHEMA.to_string(),
        target,
        base: None,
        dry_run: false,
        skip_reconcile_replica_identity: false,
    }
}

fn repair_args(
    catalog: std::path::PathBuf,
    url: &str,
    dry_run: bool,
) -> reconcile_replica_identity::ReconcileReplicaIdentityArgs {
    reconcile_replica_identity::ReconcileReplicaIdentityArgs {
        admin_database_url: url.to_string(),
        catalog,
        schema: DATA_SCHEMA.to_string(),
        dry_run,
    }
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL via WAMN_CTL_PG_URL"]
async fn migrate_catalog_reconciles_replica_identity_after_commit() {
    let url = support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PG18");
    let client = connect(&url).await;
    reset(&client).await;
    seed_delete_registration(&client).await;

    migrate_catalog::run(migrate_args(write_catalog("riorch-migrate.json"), &url))
        .await
        .expect("migrate-catalog applies the floor and reconciles after commit");

    assert_eq!(
        relreplident(&client, "sales_orders").await,
        "f",
        "the entity needing an old image must be FULL when migrate returns"
    );
    assert_eq!(
        relreplident(&client, "line_items").await,
        "d",
        "the bystander entity must remain DEFAULT"
    );
    teardown(&client).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL via WAMN_CTL_PG_URL"]
async fn operator_repair_is_dry_runnable_scoped_and_idempotent() {
    let url = support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PG18");
    let client = connect(&url).await;
    reset(&client).await;

    let floor = Migration::create(&catalog())
        .expect("compile floor")
        .sql()
        .expect("render floor");
    client
        .batch_execute(&format!(
            "CREATE SCHEMA {DATA_SCHEMA}; \
             SET search_path TO {DATA_SCHEMA}; \
             {floor} \
             RESET search_path; \
             CREATE TABLE {DATA_SCHEMA}.unrelated_guard \
               (id integer PRIMARY KEY, payload text NOT NULL); \
             ALTER TABLE {DATA_SCHEMA}.unrelated_guard REPLICA IDENTITY FULL; \
             INSERT INTO {DATA_SCHEMA}.unrelated_guard VALUES (7, 'unchanged');"
        ))
        .await
        .expect("apply entity floor and unrelated witness");
    seed_delete_registration(&client).await;

    let catalog_path = write_catalog("riorch-operator.json");
    reconcile_replica_identity::run(repair_args(catalog_path.clone(), &url, true))
        .await
        .expect("operator dry-run detects drift");
    assert_eq!(
        relreplident(&client, "sales_orders").await,
        "d",
        "dry-run must not apply the pending flip"
    );

    reconcile_replica_identity::run(repair_args(catalog_path.clone(), &url, false))
        .await
        .expect("operator repair applies drift correction");
    assert_eq!(
        relreplident(&client, "sales_orders").await,
        "f",
        "operator repair flips the catalog entity to FULL"
    );
    assert_eq!(
        relreplident(&client, "line_items").await,
        "d",
        "operator repair leaves the bystander catalog entity at DEFAULT"
    );
    assert_eq!(
        relreplident(&client, "unrelated_guard").await,
        "f",
        "operator repair must not alter a relation outside the catalog"
    );
    let witness = client
        .query_one(
            &format!("SELECT id, payload FROM {DATA_SCHEMA}.unrelated_guard"),
            &[],
        )
        .await
        .expect("read unrelated witness");
    assert_eq!(witness.get::<_, i32>(0), 7);
    assert_eq!(witness.get::<_, String>(1), "unchanged");

    reconcile_replica_identity::run(repair_args(catalog_path, &url, false))
        .await
        .expect("a second operator repair is a no-op");
    assert_eq!(relreplident(&client, "sales_orders").await, "f");
    assert_eq!(relreplident(&client, "unrelated_guard").await, "f");
    teardown(&client).await;
}
