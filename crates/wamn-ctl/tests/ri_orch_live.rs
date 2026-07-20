//! Live-apply gate for the EVT-RI-ORCH operational caller (wamn-l5i9.61): the
//! `publish-catalog` / `migrate-catalog` verbs are the AUTOMATIC caller of the
//! REPLICA IDENTITY reconcile (wamn-l5i9.31), so a catalog apply never leaves an
//! entity that needs the old image on REPLICA IDENTITY DEFAULT.
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a throwaway
//! Postgres (recipe: docs/build-and-test.md [EVT-RI-ORCH]); skipped cleanly when
//! unset. Drives the REAL verbs (`publish_catalog::run` / `migrate_catalog::run`)
//! against the REAL storage SQL (deploy/sql/catalog-schema.sql) + the REAL floor
//! DDL, proving on the LIVE `pg_class.relreplident`:
//!
//! - **publish flips**: with an old-condition + a cross-tenant delete
//!   registration on entity `orders`, `publish-catalog --provision` provisions the
//!   floor AND flips `sales_orders` `'d' -> 'f'`, while the bystander `line_items`
//!   stays `'d'`; a re-publish is idempotent.
//! - **escape hatch**: once the registrations are deleted, `--skip-reconcile-
//!   replica-identity` leaves `sales_orders` at FULL (the reconcile did NOT run);
//!   a plain re-publish then resets it `'f' -> 'd'` (the reset direction).
//! - **migrate flips**: a first-materialization `migrate-catalog` creates the
//!   tables and, after the apply transaction commits, flips the needing entity to
//!   FULL while the bystander stays DEFAULT.
//!
//! The flip itself is wal_level-independent (it sets the `pg_class` flag), so this
//! gate needs no `wal_level=logical`; the non-retroactive WAL truth is proven by
//! the l5i9.31 gate (`replica_identity_live`). Hermetic: each scenario
//! drops+recreates the `catalog` metadata schema and the data schema in its
//! preamble, so a re-run starts clean and teardown leaves nothing behind.

use tokio_postgres::{Client, NoTls};

use wamn_ctl::{migrate_catalog, publish_catalog};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const DATA_SCHEMA: &str = "riorch_data";
const CATALOG_ID: &str = "riorch";

/// entity `orders` -> table `sales_orders` (the flip target); entity `lines` ->
/// `line_items` (the bystander that must stay DEFAULT). The entity id
/// deliberately differs from the table name so the flip proves the entity->table
/// map is consulted.
fn cat_json(version: u32) -> String {
    format!(
        r#"{{"schema-version":"0.1","catalog-id":"{CATALOG_ID}","version":{version},"entities":[
          {{"id":"orders","name":"sales_orders","fields":[{{"id":"status","name":"status","type":{{"kind":"text"}}}}]}},
          {{"id":"lines","name":"line_items","fields":[{{"id":"qty","name":"qty","type":{{"kind":"int"}}}}]}}
        ]}}"#
    )
}

fn write_tmp(name: &str, content: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&p, content).expect("write catalog fixture");
    p
}

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

/// A full `EventRegistration` document (the reconciler parses condition + ops).
fn reg_doc(id: &str, flow: &str, entity: &str, ops: &str, condition: &str) -> String {
    format!(
        r#"{{"schema-version":"0.1","registration-id":"{id}","catalog-id":"{CATALOG_ID}",
           "flow-id":"{flow}","entity":"{entity}","ops":[{ops}],"condition":{condition}}}"#
    )
}

async fn insert_reg(su: &Client, tenant: &str, id: &str, entity: &str, doc: &str) {
    su.execute(
        "INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ($1, $2, $3, 'f', $4, $5::text::jsonb)",
        &[&tenant, &CATALOG_ID, &id, &entity, &doc],
    )
    .await
    .expect("seed registration");
}

/// `pg_class.relreplident` for a table in the data schema ('d'/'f'/'n'/'i').
async fn relreplident(su: &Client, table: &str) -> String {
    su.query_one(
        "SELECT c.relreplident::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2",
        &[&DATA_SCHEMA, &table],
    )
    .await
    .expect("read relreplident")
    .get(0)
}

/// Hermetic reset: drop the `catalog` schema + the data schema, ensure the
/// `wamn_app` role, then apply the REAL storage SQL.
async fn reset(su: &Client) {
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; \
         DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app'; END IF; END $$;"
    ))
    .await
    .expect("reset schemas + ensure wamn_app role");
    su.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply deploy/sql/catalog-schema.sql (the storage target)");
}

fn publish_args(
    catalog: std::path::PathBuf,
    url: &str,
    provision: bool,
    skip_reconcile: bool,
) -> publish_catalog::PublishCatalogArgs {
    publish_catalog::PublishCatalogArgs {
        catalog,
        admin_database_url: Some(url.to_string()),
        tenant: "t1".to_string(),
        schema: DATA_SCHEMA.to_string(),
        provision,
        runstate: false,
        seed_dataset: None,
        flow: vec![],
        skip_reconcile_replica_identity: skip_reconcile,
    }
}

fn migrate_args(target: std::path::PathBuf, url: &str) -> migrate_catalog::MigrateCatalogArgs {
    migrate_catalog::MigrateCatalogArgs {
        admin_database_url: url.to_string(),
        tenant: "t1".to_string(),
        environment: "dev".to_string(),
        schema: DATA_SCHEMA.to_string(),
        target,
        base: None,
        dry_run: false,
        confirm_with_backup: false,
        skip_reconcile_replica_identity: false,
    }
}

/// Both scenarios share the fixed `catalog` metadata schema, so they run
/// SEQUENTIALLY under one test entry (parallel `#[tokio::test]`s would clobber
/// each other's hermetic reset).
#[tokio::test]
async fn publish_and_migrate_reconcile_replica_identity() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the EVT-RI-ORCH gate");
        return;
    };
    let su = connect(&url).await;
    publish_scenario(&su, &url).await;
    migrate_scenario(&su, &url).await;
}

async fn publish_scenario(su: &Client, url: &str) {
    reset(su).await;
    let v1 = write_tmp("riorch_pub.json", &cat_json(1));

    // orders needs FULL from the cross-TENANT union: an old-condition (t1) AND a
    // delete subscription (t2). lines is an insert-only bystander (t1).
    insert_reg(
        su,
        "t1",
        "r-cond",
        "orders",
        &reg_doc(
            "r-cond",
            "notify",
            "orders",
            "\"update\"",
            "\"new.status != old.status\"",
        ),
    )
    .await;
    insert_reg(
        su,
        "t2",
        "r-del",
        "orders",
        &reg_doc("r-del", "purge", "orders", "\"delete\"", "null"),
    )
    .await;
    insert_reg(
        su,
        "t1",
        "r-line",
        "lines",
        &reg_doc("r-line", "ins", "lines", "\"insert\"", "null"),
    )
    .await;

    // publish --provision: creates the floor AND reconciles RI as the last step.
    publish_catalog::run(publish_args(v1.clone(), url, true, false))
        .await
        .expect("publish --provision reconciles RI");
    assert_eq!(
        relreplident(su, "sales_orders").await,
        "f",
        "publish flips the needing entity's table to FULL"
    );
    assert_eq!(
        relreplident(su, "line_items").await,
        "d",
        "the insert-only bystander stays DEFAULT"
    );

    // Re-publish (no provision) is idempotent — RI unchanged.
    publish_catalog::run(publish_args(v1.clone(), url, false, false))
        .await
        .expect("re-publish is a reconcile no-op");
    assert_eq!(relreplident(su, "sales_orders").await, "f");
    assert_eq!(relreplident(su, "line_items").await, "d");

    // Delete every registration: orders no longer needs FULL. With the escape
    // hatch, the reconcile does NOT run, so sales_orders STAYS at FULL — proving
    // --skip-reconcile-replica-identity actually suppresses the pass.
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = $1",
        &[&CATALOG_ID],
    )
    .await
    .expect("delete registrations");
    publish_catalog::run(publish_args(v1.clone(), url, false, true))
        .await
        .expect("publish --skip-reconcile-replica-identity");
    assert_eq!(
        relreplident(su, "sales_orders").await,
        "f",
        "skip suppresses the reconcile — RI is left as-is"
    );

    // A plain re-publish now resets it: no registration needs FULL, so
    // sales_orders flips back 'f' -> 'd' (the reset direction).
    publish_catalog::run(publish_args(v1, url, false, false))
        .await
        .expect("re-publish resets the no-longer-needed FULL");
    assert_eq!(
        relreplident(su, "sales_orders").await,
        "d",
        "reconcile resets an entity that no longer needs the old image"
    );

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}

async fn migrate_scenario(su: &Client, url: &str) {
    reset(su).await;
    let v1 = write_tmp("riorch_mig.json", &cat_json(1));

    // orders carries an old-condition registration; lines is a bystander.
    insert_reg(
        su,
        "t1",
        "r-cond",
        "orders",
        &reg_doc(
            "r-cond",
            "notify",
            "orders",
            "\"update\"",
            "\"old.status == 'draft'\"",
        ),
    )
    .await;

    // First-materialization migrate: creates both tables in one transaction, then
    // reconciles AFTER the commit (reads the post-migration table set).
    migrate_catalog::run(migrate_args(v1, url))
        .await
        .expect("migrate-catalog v1 reconciles RI after commit");
    assert_eq!(
        relreplident(su, "sales_orders").await,
        "f",
        "migrate flips the needing entity's table to FULL after the apply tx"
    );
    assert_eq!(
        relreplident(su, "line_items").await,
        "d",
        "the bystander entity stays DEFAULT"
    );

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}
