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
    let readout = gather_impact(&su, &plan, Some(&v1), &v2)
        .await
        .expect("gather impact");
    // wamn-0h0g.12.120: the registration set WAS read here, so the report must
    // carry no caveat. Asserted on the happy path too — a caveat that always
    // prints is one an operator learns to skip.
    assert_eq!(readout.unevaluated_registrations, None);
    assert!(!readout.render().contains("NOT EVALUATED"));
    let report = &readout.report;

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

/// Throwaway password for the non-bypassing probe. Not a credential of record.
const PROBE_PASSWORD: &str = "impact-report-probe";

/// The tenant the probe login is minted for. Deliberately NOT [`TENANT`]: it
/// derives a real key that owns none of the seeded registrations, which is what
/// makes the silence measurable as an UNDER-report rather than as no report.
const OTHER_TENANT: &str = "t2";

/// Rewrite the superuser url onto the probe login, keeping the DATABASE:
/// `wamn_authority.tenant_key` bakes `current_database()` in, so a probe pointed
/// at another database derives a different key.
fn probe_login_url(admin_url: &str, role: &str) -> String {
    let after_userinfo = admin_url
        .rsplit_once('@')
        .expect("the admin url carries userinfo")
        .1;
    format!("postgres://{role}:{PROBE_PASSWORD}@{after_userinfo}")
}

/// Mint a guest generation login in the PRODUCTION shape — no privilege of its
/// own, INHERITing the stable `wamn_app` ACL role
/// (`crates/control/provision/src/sql.rs::prepare_workload_generation_sql`).
///
/// The name carries the SERVER's own `tenant_key`, not a constant recomputed
/// here. Dropped and re-minted rather than `IF NOT EXISTS`-guarded: roles are
/// cluster-global, so a leftover healthy one would mask a broken mint.
async fn mint_probe_generation_login(su: &Client, tenant: &str) -> String {
    let key: String = su
        .query_one("SELECT wamn_authority.tenant_key($1)", &[&tenant])
        .await
        .expect("derive the probe tenant key from the installed function")
        .get(0);
    let role = format!("wamn_app_{key}_a");
    su.batch_execute(&format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
             DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\"; \
           END IF; \
         END $$; \
         CREATE ROLE \"{role}\" LOGIN PASSWORD '{PROBE_PASSWORD}' \
           NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
         GRANT wamn_app TO \"{role}\" WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;"
    ))
    .await
    .expect("mint the probe generation login");
    role
}

/// wamn-0h0g.12.120: an impact report whose registration edge class could not be
/// evaluated SAYS SO, and says it before the lines it invalidates.
///
/// The silence this closes is not a hypothetical. `ImpactReport::render` prints
/// `(no dependent flows)` under every entity with an empty edge list, so a
/// project-env that is not registration-provisioned — or a credential that
/// cannot see the set — used to render as one where NOTHING depends on the
/// destructive change. An operator reads this report to decide whether a
/// destructive migrate is safe.
///
/// It REPORTS rather than refuses because the verb is advisory and mutates
/// nothing; its two siblings on the same mechanism (wamn-0h0g.12.103's REPLICA
/// IDENTITY reconcile, wamn-0h0g.12.119's D24 orphan guard) refuse because their
/// empty reading drives an EFFECT. Detection is identical in all three: the
/// absent/unreadable pair and the `row_security = off` read.
#[tokio::test]
async fn impact_report_says_when_the_registration_edge_class_is_unevaluated() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the 12.120 unevaluated-registrations gate");
        return;
    };
    let su = connect(&url).await;
    reset(&su).await;

    let v1_file = write_tmp("wvb_unevaluated_v1.json", &v1_json());
    migrate_catalog::run(migrate_args(v1_file, &url))
        .await
        .expect("first materialization applies");
    insert_reg(&su, "reg-touched", "flow-t", "touched").await;
    insert_reg(&su, "reg-untouched", "flow-u", "untouched").await;

    let v1 = wamn_schema_model::Catalog::from_json(&v1_json()).unwrap();
    let v2 = wamn_schema_model::Catalog::from_json(&v2_json()).unwrap();
    let plan = compile_plan(Some(&v1), &v2).expect("compile plan");

    // ---- ARM 1: the RLS silence, MEASURED before it is closed --------------
    //
    // The probe is a minted generation login, not `SET ROLE wamn_app`: catalog
    // tenancy derives from `current_user`, so the bare ACL role yields a NULL key
    // and matches nothing, and every arm below would pass for that reason
    // instead. This login derives a REAL key — asserted against the server's own
    // `tenant_key` — that simply owns none of the seeded registrations.
    let probe_role = mint_probe_generation_login(&su, OTHER_TENANT).await;
    let probe = connect(&probe_login_url(&url, &probe_role)).await;
    let authority = probe
        .query_one(
            "SELECT current_user::text, \
                    (SELECT rolsuper FROM pg_roles WHERE rolname = current_user), \
                    (SELECT rolbypassrls FROM pg_roles WHERE rolname = current_user), \
                    wamn_authority.current_tenant_key() \
                      IS NOT DISTINCT FROM wamn_authority.tenant_key($1)",
            &[&OTHER_TENANT],
        )
        .await
        .expect("read the probe's own authority from the server");
    assert_eq!(authority.get::<_, String>(0), probe_role);
    assert!(!authority.get::<_, bool>(1), "the probe is a superuser");
    assert!(
        !authority.get::<_, bool>(2),
        "the probe bypasses row security, so it could never be silenced"
    );
    assert!(
        authority.get::<_, bool>(3),
        "the probe derives no tenant key, so its empty read proves nothing"
    );

    let cross_tenant_sql =
        wamn_schema_control::sql::select_registration_flow_refs_for_catalog_sql();
    let truth = su
        .query(&cross_tenant_sql, &[&CATALOG_ID])
        .await
        .expect("the superuser reads the cross-tenant registration set");
    assert_eq!(truth.len(), 2, "the fixture seeded two registration edges");
    let silent = probe
        .query(&cross_tenant_sql, &[&CATALOG_ID])
        .await
        .expect("THE SILENCE: the same read returns successfully for the probe");
    assert!(
        silent.is_empty(),
        "the probe saw {} of {} rows; the demonstration needs it to see none \
         with no error at all",
        silent.len(),
        truth.len()
    );

    // ---- ARM 2: the shell turns that silence into a named caveat -----------
    let readout = gather_impact(&probe, &plan, Some(&v1), &v2)
        .await
        .expect("an advisory verb reports rather than refusing");
    let unevaluated = readout
        .unevaluated_registrations
        .as_ref()
        .expect("a silenced read must not be folded in as an empty edge set");
    assert_eq!(
        unevaluated.kind.name(),
        "impact-report-registrations-unreadable"
    );
    assert_eq!(unevaluated.catalog_id, CATALOG_ID);
    // The SERVER's own SQLSTATE and message, carried through to the operator.
    // `tokio_postgres::Error`'s own Display is the literal string "db error".
    let detail = unevaluated
        .detail
        .as_deref()
        .expect("the database's own words reach the operator");
    assert!(detail.contains("SQLSTATE 42501"), "{detail}");
    assert!(detail.contains("row-level security"), "{detail}");
    assert_rendered_caveat_precedes_the_conclusion(&readout.render());
    // The report is still DELIVERED: refusing would take away the operator's
    // only view of the destructive change.
    assert!(
        readout.report.any_destructive(),
        "the destructive classification survives the caveat"
    );

    // ---- ARM 3: the ABSENT state, the one the shipped verb reaches ---------
    //
    // `impact-report` connects on `--admin-database-url`, a superuser, so ARM 1's
    // silence is unreachable through the CLI today. This arm is not.
    su.batch_execute("DROP TABLE catalog.event_registrations CASCADE")
        .await
        .expect("un-provision the registration set");
    let readout = gather_impact(&su, &plan, Some(&v1), &v2)
        .await
        .expect("an absent registration set is reported, not refused");
    let unevaluated = readout
        .unevaluated_registrations
        .as_ref()
        .expect("an absent registration set must not render as an empty one");
    assert_eq!(
        unevaluated.kind.name(),
        "impact-report-registrations-absent"
    );
    assert_eq!(unevaluated.catalog_id, CATALOG_ID);
    assert_rendered_caveat_precedes_the_conclusion(&readout.render());
    assert!(
        readout.report.any_destructive(),
        "the destructive classification survives the caveat"
    );

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE; \
         DROP ROLE IF EXISTS \"{probe_role}\""
    ))
    .await
    .expect("teardown");
}

/// The caveat is worthless printed after the sentence it corrects.
///
/// `(no dependent flows)` is the report's own words for an empty edge list, and
/// with the set unread it is exactly the false conclusion. Assert BOTH strings
/// are present and in that order — an assertion that only checked for the caveat
/// could not tell a report that still lies below it from one that does not.
fn assert_rendered_caveat_precedes_the_conclusion(rendered: &str) {
    let caveat = rendered
        .find("NOT EVALUATED")
        .unwrap_or_else(|| panic!("the render never says the edges were unevaluated:\n{rendered}"));
    let conclusion = rendered
        .find("(no dependent flows)")
        .unwrap_or_else(|| panic!("the render no longer states an empty edge list:\n{rendered}"));
    assert!(
        caveat < conclusion,
        "the caveat trails the conclusion it invalidates:\n{rendered}"
    );
}
