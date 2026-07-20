//! Live-apply gate for the D24 registration-orphan guard (EVT-REG, wamn-rmxa).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a throwaway
//! Postgres (recipe: docs/build-and-test.md [EVT-REG/D24]); skipped cleanly when
//! unset. Drives the REAL `wamn-ctl` verbs (`publish_catalog::run` /
//! `migrate_catalog::run`) against the REAL storage SQL
//! (deploy/sql/catalog-schema.sql), proving both verbs REFUSE a catalog that
//! would remove an entity still referenced by an event registration — naming
//! every orphan across ALL tenants — while mutating nothing, and PROCEED once the
//! registrations are deleted (and when the removed entity is unreferenced).
//!
//! Hermetic: each scenario drops+recreates the `catalog` metadata schema, the
//! data schema, and the publish snapshot tables in its preamble, so a re-run
//! starts clean and teardown leaves nothing behind.

use tokio_postgres::{Client, NoTls};

use wamn_ctl::{migrate_catalog, publish_catalog};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

const E_SALES: &str = r#"{"id":"sales_orders","name":"orders","fields":[{"id":"status","name":"status","type":{"kind":"text"}}]}"#;
const E_LINES: &str = r#"{"id":"line_items","name":"lines","fields":[{"id":"qty","name":"qty","type":{"kind":"int"}}]}"#;
const DATA_SCHEMA: &str = "rmxa_data";

fn cat_json(version: u32, entities: &str) -> String {
    format!(
        r#"{{"schema-version":"0.1","catalog-id":"shop","version":{version},"entities":[{entities}]}}"#
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

/// Hermetic reset: drop the `catalog` schema, the data schema, and the publish
/// snapshot tables, ensure the `wamn_app` role, then apply the REAL storage SQL.
async fn reset(su: &Client) {
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; \
         DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE; \
         DROP TABLE IF EXISTS public.wamn_catalog CASCADE; \
         DROP TABLE IF EXISTS public.wamn_entities CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app'; END IF; END $$;"
    ))
    .await
    .expect("reset schemas + ensure wamn_app role");
    su.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply deploy/sql/catalog-schema.sql (the storage target)");
}

/// Insert an event registration (superuser, explicit tenant — bypasses RLS) for
/// `tenant` referencing `entity_id` under catalog `shop`. The stored document is
/// irrelevant to the guard (it reads only the denormalized key columns).
async fn insert_reg(su: &Client, tenant: &str, reg_id: &str, entity_id: &str) {
    su.execute(
        "INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ($1, 'shop', $2, 'notify', $3, '{}'::jsonb)",
        &[&tenant, &reg_id, &entity_id],
    )
    .await
    .expect("seed registration");
}

async fn reg_count(su: &Client) -> i64 {
    su.query_one(
        "SELECT count(*) FROM catalog.event_registrations WHERE catalog_id = 'shop'",
        &[],
    )
    .await
    .expect("count registrations")
    .get(0)
}

async fn table_present(su: &Client, qualified: &str) -> bool {
    su.query_one(
        &format!("SELECT to_regclass('{qualified}') IS NOT NULL"),
        &[],
    )
    .await
    .expect("probe table")
    .get(0)
}

fn publish_args(catalog: std::path::PathBuf, url: &str) -> publish_catalog::PublishCatalogArgs {
    publish_catalog::PublishCatalogArgs {
        catalog,
        admin_database_url: Some(url.to_string()),
        tenant: "t1".to_string(),
        schema: "public".to_string(),
        provision: false,
        runstate: false,
        seed_dataset: None,
        flow: vec![],
        // This gate exercises only the D24 orphan guard; the EVT-RI-ORCH
        // post-apply reconcile (l5i9.61) has its own gate (ri_orch_live).
        skip_reconcile_replica_identity: true,
    }
}

fn migrate_args(
    target: std::path::PathBuf,
    url: &str,
    confirm: bool,
) -> migrate_catalog::MigrateCatalogArgs {
    migrate_catalog::MigrateCatalogArgs {
        admin_database_url: url.to_string(),
        tenant: "t1".to_string(),
        environment: "dev".to_string(),
        schema: DATA_SCHEMA.to_string(),
        target,
        base: None,
        dry_run: false,
        confirm_with_backup: confirm,
        skip_reconcile_replica_identity: true,
    }
}

/// `migrate-catalog --dry-run` args (wamn-1bfe): the plan/report path plus the
/// read-only D24 orphan probe, never an apply. Confirmation is irrelevant to a
/// dry run (it does not gate on it), so reuse `migrate_args`'s defaults.
fn migrate_dry_run_args(
    target: std::path::PathBuf,
    url: &str,
) -> migrate_catalog::MigrateCatalogArgs {
    migrate_catalog::MigrateCatalogArgs {
        dry_run: true,
        ..migrate_args(target, url, false)
    }
}

/// The snapshot entity ids currently published for tenant `t1` (parsed from the
/// stored `wamn_catalog` document) — the proof the guard mutated nothing.
async fn snapshot_entity_ids(su: &Client) -> Vec<String> {
    let doc: String = su
        .query_one(
            "SELECT document::text FROM public.wamn_catalog WHERE tenant_id = 't1'",
            &[],
        )
        .await
        .expect("read published snapshot")
        .get(0);
    let cat = wamn_catalog::Catalog::from_json(&doc).expect("snapshot parses");
    cat.entities
        .iter()
        .map(|e| e.id.as_str().to_string())
        .collect()
}

/// All scenarios share the fixed `catalog` metadata schema, so they run
/// SEQUENTIALLY under one test entry (parallel `#[tokio::test]`s would clobber
/// each other's hermetic reset).
#[tokio::test]
async fn orphan_guard_refuses_then_proceeds() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the D24 orphan-guard gate");
        return;
    };
    let su = connect(&url).await;
    publish_scenario(&su, &url).await;
    migrate_scenario(&su, &url).await;
    dry_run_scenario(&su, &url).await;
}

async fn publish_scenario(su: &Client, url: &str) {
    reset(su).await;

    let ab = write_tmp(
        "d24_pub_ab.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );
    let a_only = write_tmp("d24_pub_a.json", &cat_json(1, E_SALES));
    let b_only = write_tmp("d24_pub_b.json", &cat_json(1, E_LINES));

    // Seed: publish the full catalog {sales_orders, line_items} for tenant t1.
    publish_catalog::run(publish_args(ab, url))
        .await
        .expect("initial publish of the full catalog");

    // Two tenants register against entity `sales_orders`.
    insert_reg(su, "t1", "reg-t1", "sales_orders").await;
    insert_reg(su, "t2", "reg-t2", "sales_orders").await;

    // Removing the UNREFERENCED entity `line_items` proceeds (keeps sales_orders).
    publish_catalog::run(publish_args(a_only, url))
        .await
        .expect("publish removing an unreferenced entity proceeds");
    assert_eq!(
        snapshot_entity_ids(su).await,
        vec!["sales_orders".to_string()]
    );

    // Removing `sales_orders` — still referenced by BOTH tenants — is REFUSED.
    let err = publish_catalog::run(publish_args(b_only.clone(), url))
        .await
        .expect_err("orphaning publish must be refused");
    let msg = err.to_string();
    for needle in ["reg-t1", "reg-t2", "t1", "t2", "sales_orders"] {
        assert!(msg.contains(needle), "refusal names {needle:?}: {msg}");
    }

    // NOTHING mutated: snapshot still {sales_orders}, both registrations intact.
    assert_eq!(
        snapshot_entity_ids(su).await,
        vec!["sales_orders".to_string()]
    );
    assert_eq!(
        reg_count(su).await,
        2,
        "registrations untouched by the refusal"
    );

    // Delete the registrations via the storage surface, then the same publish
    // proceeds (sales_orders now unreferenced).
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = 'shop'",
        &[],
    )
    .await
    .expect("owner deletes the registrations");
    publish_catalog::run(publish_args(b_only, url))
        .await
        .expect("re-publish proceeds once the registrations are gone");
    assert_eq!(
        snapshot_entity_ids(su).await,
        vec!["line_items".to_string()]
    );

    su.batch_execute("DROP SCHEMA IF EXISTS catalog CASCADE; DROP TABLE IF EXISTS public.wamn_catalog CASCADE; DROP TABLE IF EXISTS public.wamn_entities CASCADE")
        .await
        .expect("teardown");
}

async fn migrate_scenario(su: &Client, url: &str) {
    reset(su).await;

    let ab = write_tmp(
        "d24_mig_ab.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );
    let b_v2 = write_tmp("d24_mig_b_v2.json", &cat_json(2, E_LINES));

    // v1: materialize {sales_orders -> orders, line_items -> lines}.
    migrate_catalog::run(migrate_args(ab, url, false))
        .await
        .expect("first materialization applies");
    assert!(table_present(su, &format!("{DATA_SCHEMA}.orders")).await);
    assert!(table_present(su, &format!("{DATA_SCHEMA}.lines")).await);

    insert_reg(su, "t1", "reg-t1", "sales_orders").await;
    insert_reg(su, "t2", "reg-t2", "sales_orders").await;

    // v2 removes `sales_orders` — still referenced. REFUSED before the apply tx,
    // independent of the destructive-backup gate (no --confirm-with-backup here).
    let err = migrate_catalog::run(migrate_args(b_v2.clone(), url, false))
        .await
        .expect_err("orphaning migration must be refused");
    let msg = err.to_string();
    for needle in ["reg-t1", "reg-t2", "t1", "t2", "sales_orders"] {
        assert!(msg.contains(needle), "refusal names {needle:?}: {msg}");
    }

    // NOTHING mutated: the `orders` table survives, v1 is still applied, regs stay.
    assert!(
        table_present(su, &format!("{DATA_SCHEMA}.orders")).await,
        "the dropped-entity table survives the refusal"
    );
    let applied: i32 = su
        .query_one(
            "SELECT version FROM catalog.catalogs \
             WHERE tenant_id='t1' AND catalog_id='shop' AND environment='dev' AND state='applied'",
            &[],
        )
        .await
        .expect("read applied version")
        .get(0);
    assert_eq!(applied, 1, "the applied catalog version is unchanged");
    assert_eq!(
        reg_count(su).await,
        2,
        "registrations untouched by the refusal"
    );

    // Delete the registrations, then the destructive migration proceeds (with the
    // backup confirmation the drop requires); `orders` is dropped, v2 applied.
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = 'shop'",
        &[],
    )
    .await
    .expect("owner deletes the registrations");
    migrate_catalog::run(migrate_args(b_v2, url, true))
        .await
        .expect("re-migrate proceeds once the registrations are gone");
    assert!(
        !table_present(su, &format!("{DATA_SCHEMA}.orders")).await,
        "the unreferenced entity's table is now dropped"
    );

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}

/// wamn-1bfe: `migrate-catalog --dry-run` must SURFACE the D24 refusal, so an
/// operator cannot dry-run clean and then fail the real run. A dry run whose
/// target removes a still-referenced entity fails with the marked verdict naming
/// every orphan across ALL tenants — while mutating NOTHING — and turns clean
/// again once the registrations are deleted (proving the probe is not vacuous).
async fn dry_run_scenario(su: &Client, url: &str) {
    reset(su).await;

    let ab = write_tmp(
        "d24_dry_ab.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );
    let b_v2 = write_tmp("d24_dry_b_v2.json", &cat_json(2, E_LINES));

    // v1: materialize {sales_orders -> orders, line_items -> lines}.
    migrate_catalog::run(migrate_args(ab, url, false))
        .await
        .expect("first materialization applies");
    assert!(table_present(su, &format!("{DATA_SCHEMA}.orders")).await);

    insert_reg(su, "t1", "reg-t1", "sales_orders").await;
    insert_reg(su, "t2", "reg-t2", "sales_orders").await;

    // A dry run of v2 (removes the still-referenced `sales_orders`) REFUSES with a
    // marked dry-run finding naming both tenants — NOT a clean report.
    let err = migrate_catalog::run(migrate_dry_run_args(b_v2.clone(), url))
        .await
        .expect_err("orphaning dry-run must surface the refusal");
    let msg = err.to_string();
    assert!(
        msg.contains("dry-run"),
        "the verdict is marked as a dry-run finding: {msg}"
    );
    for needle in ["reg-t1", "reg-t2", "t1", "t2", "sales_orders"] {
        assert!(
            msg.contains(needle),
            "dry-run refusal names {needle:?}: {msg}"
        );
    }

    // NOTHING mutated by the dry run: v1 still applied, `orders` survives, regs stay.
    assert!(
        table_present(su, &format!("{DATA_SCHEMA}.orders")).await,
        "dry-run mutated nothing (the dropped-entity table survives)"
    );
    let applied: i32 = su
        .query_one(
            "SELECT version FROM catalog.catalogs \
             WHERE tenant_id='t1' AND catalog_id='shop' AND environment='dev' AND state='applied'",
            &[],
        )
        .await
        .expect("read applied version")
        .get(0);
    assert_eq!(applied, 1, "dry-run left the applied version unchanged");
    assert_eq!(
        reg_count(su).await,
        2,
        "dry-run left the registrations intact"
    );

    // Delete the registrations — the SAME dry run now reports clean (Ok), proving
    // the probe refuses only a real orphan, not vacuously.
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = 'shop'",
        &[],
    )
    .await
    .expect("owner deletes the registrations");
    migrate_catalog::run(migrate_dry_run_args(b_v2, url))
        .await
        .expect("dry-run proceeds once the registrations are gone");

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}
