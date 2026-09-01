//! Server-answer proof for package-model replica-identity reconciliation.

mod support;

use tokio_postgres::{Client, NoTls};
use wamn_ctl::reconcile_replica_identity::reconcile;
use wamn_schema_control::ManagedModel;

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const PACKAGE_ID: &str = "ri_package";
const OWNER_PACKAGE_ID: &str = "client_overlay";

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

fn registration() -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "registration-id": "orders-delete",
        "package-id": OWNER_PACKAGE_ID,
        "source-package-id": PACKAGE_ID,
        "entity": "orders",
        "ops": ["delete"],
        "condition": null
    })
    .to_string()
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
            "DROP SCHEMA IF EXISTS ri_data CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DO $roles$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_ri_probe') THEN \
                 CREATE ROLE wamn_ri_probe NOLOGIN NOSUPERUSER NOBYPASSRLS; \
               END IF; \
             END $roles$;",
        )
        .await
        .expect("reset RI schemas and roles");
    client
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install production package catalog schema");
    client
        .batch_execute(
            "CREATE SCHEMA ri_data; \
             CREATE TABLE ri_data.orders (id bigint PRIMARY KEY, tenant_id text NOT NULL); \
             CREATE TABLE ri_data.lines (id bigint PRIMARY KEY, tenant_id text NOT NULL); \
             GRANT USAGE ON SCHEMA catalog, ri_data TO wamn_ri_probe; \
             GRANT SELECT ON catalog.event_registrations TO wamn_ri_probe;",
        )
        .await
        .expect("install RI application tables and probe surface");
}

#[tokio::test]
async fn package_registration_union_flips_exact_tables_and_unreadable_state_refuses() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping RI live proof; WAMN_CTL_PG_URL is unset");
        return;
    };
    let client = connect(&url).await;
    install(&client).await;
    client
        .execute(
            "INSERT INTO catalog.event_registrations \
             (tenant_id, package_id, registration_id, entity_id, registration) \
             VALUES ('t1', $1, 'orders-delete', 'orders', $2::text::jsonb)",
            &[&OWNER_PACKAGE_ID, &registration()],
        )
        .await
        .expect("store overlay-owned registration on a base package entity");

    reconcile(&client, PACKAGE_ID, &models(), true)
        .await
        .expect("reconcile package registrations");
    assert_eq!(identity(&client, "orders").await, "f");
    assert_eq!(identity(&client, "lines").await, "d");

    client
        .batch_execute("SET ROLE wamn_ri_probe")
        .await
        .expect("enter non-bypass probe role");
    let unreadable = reconcile(&client, PACKAGE_ID, &models(), false)
        .await
        .expect_err("forced-RLS silence must refuse as unreadable");
    client
        .batch_execute("RESET ROLE; RESET row_security")
        .await
        .expect("leave probe role");
    assert!(format!("{unreadable:#}").contains("replica-identity-registrations-unreadable"));
    assert_eq!(identity(&client, "orders").await, "f");

    client
        .execute(
            "DELETE FROM catalog.event_registrations \
             WHERE registration ->> 'source-package-id' = $1",
            &[&PACKAGE_ID],
        )
        .await
        .expect("remove the only FULL demand");
    reconcile(&client, PACKAGE_ID, &models(), true)
        .await
        .expect("genuinely empty registration set reconciles");
    assert_eq!(identity(&client, "orders").await, "d");

    client
        .batch_execute("DROP TABLE catalog.event_registrations")
        .await
        .expect("remove registration owner relation");
    let absent = reconcile(&client, PACKAGE_ID, &models(), false)
        .await
        .expect_err("absent registration storage must refuse");
    assert!(format!("{absent:#}").contains("replica-identity-registrations-absent"));

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS ri_data CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE;",
        )
        .await
        .expect("clean RI schemas");
}
