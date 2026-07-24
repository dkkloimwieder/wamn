//! Live-apply gate for the per-entity REPLICA IDENTITY FULL reconciler
//! (EVT-REPLICA-IDENT, wamn-l5i9.31).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a throwaway
//! `wal_level=logical` Postgres (recipe: docs/build-and-test.md
//! [EVT-REPLICA-IDENT]); skipped cleanly when unset. Drives the REAL reconcile
//! path (`reconcile_replica_identity::reconcile`) against the REAL floor DDL
//! (`wamn_ddl::Migration::create`) + the REAL registration storage
//! (deploy/sql/catalog-schema.sql), proving:
//!
//! 1. **relreplident transitions** — an entity with an old-image / delete
//!    registration flips `'d' -> 'f'`; an unrelated entity stays `'d'`; removing
//!    the registrations flips it back `'f' -> 'd'`; a reconcile at target is a
//!    no-op. The FULL requirement is the cross-TENANT union (RI is per-table).
//! 2. **WAL truth (non-retroactive)** — a `test_decoding` slot created BEFORE any
//!    writes captures the same table before and after the flip: under DEFAULT an
//!    UPDATE carries no old image and a DELETE's old image is the pkey only (no
//!    `tenant_id` — a delete cannot even be tenant-scoped); after the flip to
//!    FULL an UPDATE carries the old image and a DELETE's old image carries
//!    `tenant_id`. The flip enriches only WAL written AFTER it.
//!
//! Hermetic: drops+recreates the `catalog` metadata schema, the data schema, and
//! the test slot in its preamble, so a re-run starts clean and teardown leaves
//! nothing behind.

use tokio_postgres::{Client, NoTls};

use wamn_ctl::reconcile_replica_identity::reconcile;
use wamn_ddl::{Confirmation, Migration};
use wamn_migrate::ReplicaIdentity;

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const DATA_SCHEMA: &str = "ri_data";
const SLOT: &str = "ri_td_slot";
const CATALOG_ID: &str = "ritest";

/// entity `orders` -> table `sales_orders` (the flip target); entity `lines` ->
/// `line_items` (the bystander that must stay DEFAULT). The entity id
/// deliberately differs from the table name so the flip proves the entity->table
/// map is consulted.
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1", "catalog-id": "ritest", "version": 1,
  "entities": [
    { "id": "orders", "name": "sales_orders", "fields": [
      { "id": "status", "name": "status", "type": { "kind": "text" } } ] },
    { "id": "lines", "name": "line_items", "fields": [
      { "id": "qty", "name": "qty", "type": { "kind": "int" } } ] }
  ]
}"#;

fn catalog() -> wamn_catalog::Catalog {
    wamn_catalog::Catalog::from_json(CATALOG_JSON).expect("catalog parses")
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
    let row: String = su
        .query_one(
            "SELECT c.relreplident::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
            &[&DATA_SCHEMA, &table],
        )
        .await
        .expect("read relreplident")
        .get(0);
    row
}

/// Drain the test_decoding slot; return the concatenated change lines for
/// `table` only (BEGIN/COMMIT and other tables filtered out).
async fn drain_changes(su: &Client, table: &str) -> Vec<String> {
    let rows = su
        .query(
            "SELECT data FROM pg_logical_slot_get_changes($1, NULL, NULL)",
            &[&SLOT],
        )
        .await
        .expect("drain test_decoding");
    let needle = format!("table {DATA_SCHEMA}.{table}:");
    rows.iter()
        .map(|r| r.get::<_, String>(0))
        .filter(|d| d.contains(&needle))
        .collect()
}

async fn reset(su: &Client) {
    // Drop a leftover slot first (a live slot pins WAL).
    let _ = su
        .execute(
            "SELECT pg_drop_replication_slot($1) FROM pg_replication_slots WHERE slot_name = $1",
            &[&SLOT],
        )
        .await;
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
        .expect("apply deploy/sql/catalog-schema.sql");
    // The real 3.2 floor for the two entities in the data schema.
    let floor = Migration::create(&catalog())
        .expect("floor compile")
        .sql(Confirmation::None)
        .expect("floor sql");
    su.batch_execute(&format!(
        "CREATE SCHEMA {DATA_SCHEMA}; SET search_path TO {DATA_SCHEMA}; {floor}"
    ))
    .await
    .expect("apply the 3.2 floor");
    su.batch_execute("SET search_path TO public")
        .await
        .expect("reset search_path");
}

#[tokio::test]
async fn reconcile_flips_relreplident_and_the_flip_enriches_wal_non_retroactively() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the l5i9.31 replica-identity gate");
        return;
    };
    let su = connect(&url).await;
    // The throwaway container runs synchronous_commit=off; logical decoding only
    // returns changes up to the FLUSHED WAL LSN, so force this session's commits
    // to flush or pg_logical_slot_get_changes sees nothing.
    su.batch_execute("SET synchronous_commit = on")
        .await
        .expect("synchronous_commit on");
    reset(&su).await;
    let cat = catalog();

    // The slot BEFORE any writes: it captures the DEFAULT-era and FULL-era WAL of
    // the SAME table, which is what makes the non-retroactive boundary observable.
    su.query_one(
        "SELECT 1 FROM pg_create_logical_replication_slot($1, 'test_decoding')",
        &[&SLOT],
    )
    .await
    .expect("create test_decoding slot");

    // --- 1a. No registrations yet → everything reconciles to DEFAULT (no-op). ---
    let plan = reconcile(&su, &cat, DATA_SCHEMA, true)
        .await
        .expect("reconcile");
    assert!(plan.is_noop(), "fresh floor + no registrations → no flips");
    assert_eq!(relreplident(&su, "sales_orders").await, "d");
    assert_eq!(relreplident(&su, "line_items").await, "d");

    // Seed a row per table, then observe the DEFAULT-era WAL truth: an UPDATE
    // carries no old image; a DELETE's old image is the pkey only (no tenant_id).
    let oid: String = su
        .query_one(
            &format!("INSERT INTO {DATA_SCHEMA}.sales_orders (tenant_id, status) VALUES ('t1','draft') RETURNING id::text"),
            &[],
        )
        .await
        .expect("insert order")
        .get(0);
    su.batch_execute(&format!(
        "UPDATE {DATA_SCHEMA}.sales_orders SET status = 'shipped' WHERE id = '{oid}'::uuid; \
         DELETE FROM {DATA_SCHEMA}.sales_orders WHERE id = '{oid}'::uuid;"
    ))
    .await
    .expect("default-era update+delete");
    let before = drain_changes(&su, "sales_orders").await;
    let upd_before = before
        .iter()
        .find(|l| l.contains("UPDATE:"))
        .expect("an update line");
    let del_before = before
        .iter()
        .find(|l| l.contains("DELETE:"))
        .expect("a delete line");
    assert!(
        !upd_before.contains("old-key"),
        "DEFAULT: an UPDATE carries no old image: {upd_before}"
    );
    assert!(
        !del_before.contains("tenant_id"),
        "DEFAULT: a DELETE's old image is the pkey only — no tenant_id (cannot be tenant-scoped): {del_before}"
    );

    // --- 1b. Register: an old-condition on orders (tenant t1) + a delete on ---
    // orders (tenant t2 — the cross-tenant union) + an insert-only bystander on
    // lines. Then reconcile: sales_orders flips 'd' -> 'f', line_items stays 'd'.
    insert_reg(
        &su,
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
        &su,
        "t2",
        "r-del",
        "orders",
        &reg_doc("r-del", "purge", "orders", "\"delete\"", "null"),
    )
    .await;
    insert_reg(
        &su,
        "t1",
        "r-line",
        "lines",
        &reg_doc("r-line", "ins", "lines", "\"insert\"", "null"),
    )
    .await;

    let plan = reconcile(&su, &cat, DATA_SCHEMA, true)
        .await
        .expect("reconcile");
    let flip = plan
        .flips
        .iter()
        .find(|f| f.table == "sales_orders")
        .expect("sales_orders flips");
    assert_eq!(flip.from, ReplicaIdentity::Default);
    assert_eq!(flip.to, ReplicaIdentity::Full);
    assert_eq!(
        plan.flips.len(),
        1,
        "only sales_orders flips (the union of t1+t2 regs)"
    );
    assert_eq!(
        relreplident(&su, "sales_orders").await,
        "f",
        "flipped to FULL live"
    );
    assert_eq!(
        relreplident(&su, "line_items").await,
        "d",
        "bystander stays DEFAULT"
    );

    // --- 2. WAL truth AFTER the flip: an UPDATE carries the old image; a DELETE's
    // old image carries tenant_id (so the delete is now tenant-scopable). ---
    let oid2: String = su
        .query_one(
            &format!("INSERT INTO {DATA_SCHEMA}.sales_orders (tenant_id, status) VALUES ('t1','draft') RETURNING id::text"),
            &[],
        )
        .await
        .expect("insert order 2")
        .get(0);
    su.batch_execute(&format!(
        "UPDATE {DATA_SCHEMA}.sales_orders SET status = 'shipped' WHERE id = '{oid2}'::uuid; \
         DELETE FROM {DATA_SCHEMA}.sales_orders WHERE id = '{oid2}'::uuid;"
    ))
    .await
    .expect("full-era update+delete");
    let after = drain_changes(&su, "sales_orders").await;
    let upd_after = after
        .iter()
        .find(|l| l.contains("UPDATE:"))
        .expect("an update line");
    let del_after = after
        .iter()
        .find(|l| l.contains("DELETE:"))
        .expect("a delete line");
    assert!(
        upd_after.contains("old-key"),
        "FULL: an UPDATE carries the old image: {upd_after}"
    );
    assert!(
        del_after.contains("tenant_id"),
        "FULL: a DELETE's old image carries tenant_id (now tenant-scopable): {del_after}"
    );

    // --- 3. Remove the registrations → sales_orders flips back 'f' -> 'd'. ---
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = $1",
        &[&CATALOG_ID],
    )
    .await
    .expect("delete registrations");
    let plan = reconcile(&su, &cat, DATA_SCHEMA, true)
        .await
        .expect("reconcile back");
    let back = plan
        .flips
        .iter()
        .find(|f| f.table == "sales_orders")
        .expect("sales_orders flips back");
    assert_eq!(back.from, ReplicaIdentity::Full);
    assert_eq!(back.to, ReplicaIdentity::Default);
    assert_eq!(
        relreplident(&su, "sales_orders").await,
        "d",
        "flipped back to DEFAULT"
    );

    // --- 4. A reconcile at the target state is a no-op (idempotent). ---
    let plan = reconcile(&su, &cat, DATA_SCHEMA, true)
        .await
        .expect("reconcile idempotent");
    assert!(plan.is_noop(), "reconcile at target flips nothing");

    // teardown: drop the slot (releases pinned WAL), then the schemas.
    let _ = su
        .execute(
            "SELECT pg_drop_replication_slot($1) FROM pg_replication_slots WHERE slot_name = $1",
            &[&SLOT],
        )
        .await;
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}
