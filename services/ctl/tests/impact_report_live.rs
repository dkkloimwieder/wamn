#![cfg(feature = "ops")]

//! Live gate for operations-only schema-change impact analysis (11.8, wamn-wvb).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url of a throwaway Postgres (recipe:
//! docs/operations/build-and-test.md [11.8]); skipped cleanly when unset. Drives the REAL
//! machinery against the REAL storage SQL (deploy/sql/catalog-schema.sql):
//!
//!   1. materialize a v1 catalog with `E_touched` (`orders`) + `E_untouched`
//!      (`audit`) through `migrate-catalog`;
//!   2. seed a dependent flow per entity through an event registration (id-keyed);
//!   3. stage v2 = destructive on `E_touched` (drop a column) + additive on
//!      `E_untouched` (add a column), and assert `wamn_schema_control::impact::analyze` (through
//!      the shell's [`gather_impact`]) names EXACTLY `E_touched`'s flow/api
//!      resource on the destructive entity — never `E_untouched`'s (the untouched
//!      partition) — while retaining the destructive classification;
//!
//! Hermetic: drops+recreates the `catalog` metadata schema + the data schema.

mod support;

use tokio_postgres::{Client, NoTls};

use wamn_ctl::impact_report::{compile_plan, gather_impact};
use wamn_ctl::migrate_catalog;
use wamn_ctl::publish_catalog::ensure_runstate;
use wamn_schema_control::BareSchemaName;

const DATA_SCHEMA: &str = "wvb_data";
const TENANT: &str = "t1";
const CATALOG_ID: &str = "shop";

/// A catalog document: `entities` is the raw entity-array JSON.
fn cat_json(version: u32, entities: &str) -> String {
    format!(
        r#"{{"schema-version":"0.1","catalog-id":"{CATALOG_ID}","version":{version},"entities":[{entities}]}}"#
    )
}

fn field(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"{id}","type":{{"kind":"text"}}}}"#)
}

/// E_touched `orders`: v1 has fields `status` + `note`; v2 drops `note` (destructive).
fn touched(fields: &[&str]) -> String {
    let fs: Vec<String> = fields.iter().map(|f| field(f)).collect();
    format!(
        r#"{{"id":"touched","name":"orders","fields":[{}]}}"#,
        fs.join(",")
    )
}

/// E_untouched `audit`: v1 has `kind`; v2 adds `ts` (additive).
fn untouched(fields: &[&str]) -> String {
    let fs: Vec<String> = fields.iter().map(|f| field(f)).collect();
    format!(
        r#"{{"id":"untouched","name":"audit","fields":[{}]}}"#,
        fs.join(",")
    )
}

fn v1_json() -> String {
    cat_json(
        1,
        &format!("{},{}", touched(&["status", "note"]), untouched(&["kind"])),
    )
}
fn v2_json() -> String {
    // orders drops `note` (DESTRUCTIVE); audit adds `ts` (additive).
    cat_json(
        2,
        &format!("{},{}", touched(&["status"]), untouched(&["kind", "ts"])),
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

fn migrate_args(target: std::path::PathBuf, url: &str) -> migrate_catalog::MigrateCatalogArgs {
    migrate_catalog::MigrateCatalogArgs {
        admin_database_url: url.to_string(),
        tenant: TENANT.to_string(),
        environment: "dev".to_string(),
        schema: DATA_SCHEMA.to_string(),
        target,
        base: None,
        dry_run: false,
        skip_reconcile_replica_identity: true,
    }
}

/// Reset schemas + role, apply the catalog metadata schema, and provision the
/// run-plane into the data schema through the production `ensure_*`
/// path used by `publish-catalog --runstate`.
async fn reset(su: &Client) {
    let schema = BareSchemaName::new(DATA_SCHEMA).expect("live-test schema is valid");
    let catalog_schema = include_str!("../../../deploy/sql/catalog-schema.sql");
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; \
         DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         END IF; END $$;"
    ))
    .await
    .expect("reset schemas + ensure wamn_app role");
    su.batch_execute(wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .expect("ensure wamn_scenario_author role");
    su.batch_execute(&wamn_control_provision::sql::ensure_effect_writer_acl_role_sql())
        .await
        .expect("ensure effect-writer ACL roles");
    su.batch_execute(catalog_schema)
        .await
        .expect("apply deploy/sql/catalog-schema.sql");
    ensure_runstate(su, &schema)
        .await
        .expect("ensure run-state");
}

async fn insert_reg(su: &Client, reg_id: &str, flow_id: &str, entity_id: &str) {
    su.execute(
        "INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ($1, $2, $3, $4, $5, '{}'::jsonb)",
        &[&TENANT, &CATALOG_ID, &reg_id, &flow_id, &entity_id],
    )
    .await
    .expect("seed registration");
}

async fn column_present(su: &Client, table: &str, column: &str) -> bool {
    su.query_one(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = $2 AND column_name = $3)",
        &[&DATA_SCHEMA, &table, &column],
    )
    .await
    .expect("probe column")
    .get(0)
}

#[tokio::test]
async fn impact_report_names_the_affected_change() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the 11.8 impact-analysis gate");
        return;
    };
    let su = connect(&url).await;
    reset(&su).await;

    // v1: materialize orders + audit.
    let v1_file = write_tmp("wvb_v1.json", &v1_json());
    migrate_catalog::run(migrate_args(v1_file, &url))
        .await
        .expect("first materialization applies");
    assert!(column_present(&su, "orders", "note").await);

    // Seed a dependent flow per entity through its registration (id-keyed).
    // flow-t depends on E_touched; flow-u on E_untouched (the decoy that must
    // never be attributed to E_touched).
    insert_reg(&su, "reg-touched", "flow-t", "touched").await;
    insert_reg(&su, "reg-untouched", "flow-u", "untouched").await;

    // --- the typed analysis, through the shell's live reads -----------------
    let v1 = wamn_schema_model::Catalog::from_json(&v1_json()).unwrap();
    let v2 = wamn_schema_model::Catalog::from_json(&v2_json()).unwrap();
    let plan = compile_plan(Some(&v1), &v2).expect("compile plan");
    let report = gather_impact(&su, &plan, Some(&v1), &v2)
        .await
        .expect("gather impact");

    let touched = report
        .entities
        .iter()
        .find(|e| e.entity_id == "touched")
        .expect("touched entity is in the report");
    assert!(
        touched.destructive,
        "the dropped-column entity is destructive"
    );
    assert_eq!(touched.entity_name, "orders");
    // registration edge: EXACTLY flow-t / reg-touched — never the decoy.
    assert_eq!(touched.flows_via_registration.len(), 1);
    assert_eq!(touched.flows_via_registration[0].flow_id, "flow-t");
    assert_eq!(
        touched.flows_via_registration[0].registration_id,
        "reg-touched"
    );
    // api edge: the entity's own resource.
    assert!(
        touched
            .api_resources
            .contains(&"/api/rest/orders".to_string())
    );
    // The untouched partition: none of E_untouched's dependents leak onto E_touched.
    let rendered = report.render();
    let touched_block = rendered
        .split("entity \"audit\"")
        .next()
        .expect("report has a touched block before the audit block");
    for decoy in ["flow-u", "reg-untouched"] {
        assert!(
            !touched_block.contains(decoy),
            "the untouched entity's {decoy:?} must not appear under E_touched:\n{touched_block}"
        );
    }

    // E_untouched is present, additive, and carries ITS OWN dependents.
    let audit = report
        .entities
        .iter()
        .find(|e| e.entity_id == "untouched")
        .expect("untouched entity is in the report");
    assert!(!audit.destructive, "the added-column entity is additive");
    assert_eq!(audit.flows_via_registration[0].flow_id, "flow-u");

    // Operations callers retain both facts without a default-command
    // acknowledgement carrier.
    assert!(touched.destructive);
    assert!(touched.has_downstream_impact());

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}
