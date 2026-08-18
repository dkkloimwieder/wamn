//! Live-apply gate for the D24 registration-orphan guard (EVT-REG, wamn-rmxa).
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a throwaway
//! Postgres (recipe: docs/archive/build-and-test.md [EVT-REG/D24]); skipped cleanly when
//! unset. Drives the REAL `wamn-ctl` verbs (`publish_catalog::run` /
//! `migrate_catalog::run`) against the REAL storage SQL
//! (deploy/sql/catalog-schema.sql), proving both verbs REFUSE a catalog that
//! would remove an entity still referenced by an event registration — naming
//! every orphan across ALL tenants — while mutating nothing. Once the
//! registrations are deleted, the default command reaches its separate
//! additive-only refusal for the destructive target.
//!
//! wamn-0h0g.12.119 adds the fail-CLOSED half: the guard must REFUSE BY NAME a
//! registration set it cannot read, rather than reading zero rows and clearing a
//! destructive apply. `migrate-catalog` has no refusal in front of this guard, so
//! a silent pass here removes an entity that registrations still reference — the
//! exact outcome D24 exists to prevent — and reports the run clean.
//!
//! Hermetic: each scenario drops+recreates the `catalog` metadata schema, the
//! data schema, and the publish snapshot tables in its preamble, so a re-run
//! starts clean and teardown leaves nothing behind.

use std::sync::LazyLock;

use tokio_postgres::{Client, NoTls};

use wamn_ctl::{migrate_catalog, publish_catalog};

/// Every test in this binary rebuilds the FIXED `catalog` metadata schema in its
/// preamble, so they must not interleave — cargo runs a binary's tests in
/// parallel threads by default.
static SERIALIZE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

const E_SALES: &str = r#"{"id":"sales_orders","name":"orders","fields":[{"id":"status","name":"status","type":{"kind":"text"}}]}"#;
const E_SALES_ADDITIVE: &str = r#"{"id":"sales_orders","name":"orders","fields":[{"id":"status","name":"status","type":{"kind":"text"}},{"id":"note","name":"note","type":{"kind":"text"}}]}"#;
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
    su.batch_execute(wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .expect("ensure wamn_scenario_author role");
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

/// The full set of schema names present (the proof a dry run created no schema).
async fn schema_names(su: &Client) -> Vec<String> {
    su.query(
        "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name",
        &[],
    )
    .await
    .expect("list schemas")
    .iter()
    .map(|r| r.get::<_, String>(0))
    .collect()
}

async fn schema_present(su: &Client, name: &str) -> bool {
    su.query_one(
        "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = $1)",
        &[&name],
    )
    .await
    .expect("probe schema")
    .get(0)
}

fn publish_args(catalog: std::path::PathBuf, url: &str) -> publish_catalog::PublishCatalogArgs {
    publish_catalog::PublishCatalogArgs {
        catalog,
        admin_database_url: Some(url.to_string()),
        tenant: "t1".to_string(),
        project_config: None,
        schema: "public".to_string(),
        provision: false,
        runstate: false,
        seed_dataset: None,
        flow: vec![],
        exposure: None,
        // This gate exercises only the D24 orphan guard; the EVT-RI-ORCH
        // post-apply reconcile (l5i9.61) has its own gate (ri_orch_live).
        skip_reconcile_replica_identity: true,
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
        skip_reconcile_replica_identity: true,
    }
}

/// `migrate-catalog --dry-run` args (wamn-1bfe): the additive planner plus the
/// read-only D24 orphan probe, never an apply.
fn migrate_dry_run_args(
    target: std::path::PathBuf,
    url: &str,
) -> migrate_catalog::MigrateCatalogArgs {
    migrate_catalog::MigrateCatalogArgs {
        dry_run: true,
        ..migrate_args(target, url)
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
    let cat = wamn_schema_model::Catalog::from_json(&doc).expect("snapshot parses");
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
    let _serialized = SERIALIZE.lock().await;
    let su = connect(&url).await;
    publish_scenario(&su, &url).await;
    migrate_scenario(&su, &url).await;
    dry_run_scenario(&su, &url).await;
    dry_run_no_data_schema_scenario(&su, &url).await;
}

/// A caller that holds every privilege `migrate-catalog --dry-run` needs, and
/// simply does not BYPASS row-level security. That is the whole hazard: NOT a
/// under-privileged role (which errors loudly and harmlessly), but a
/// sufficiently-privileged one that reads the registration set as empty in
/// silence.
///
/// `wamn_app` cannot stand in here. `read_current_applied` runs
/// `SELECT ... FOR UPDATE` on `catalog.catalogs`, which needs UPDATE privilege,
/// and wamn-0h0g.12.126 deliberately removed exactly that from the guest role —
/// so wamn_app fails with a privilege error BEFORE reaching the guard, proving
/// nothing about the guard.
const RLS_ROLE: &str = "d24_rls_reader";

/// Create [`RLS_ROLE`] with precisely the dry-run path's privileges. Roles are
/// CLUSTER-wide, so this drops any leftover first and teardown drops it again.
async fn create_rls_reader(su: &Client) {
    su.batch_execute(&format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{RLS_ROLE}') THEN \
             EXECUTE 'DROP OWNED BY {RLS_ROLE}'; EXECUTE 'DROP ROLE {RLS_ROLE}'; \
           END IF; \
         END $$; \
         CREATE ROLE {RLS_ROLE} LOGIN PASSWORD '{RLS_ROLE}' NOSUPERUSER NOBYPASSRLS; \
         GRANT USAGE ON SCHEMA catalog TO {RLS_ROLE}; \
         GRANT SELECT, UPDATE ON catalog.catalogs TO {RLS_ROLE}; \
         GRANT SELECT ON catalog.event_registrations TO {RLS_ROLE};"
    ))
    .await
    .expect("create the non-bypassing reader role");
}

async fn drop_rls_reader(su: &Client) {
    su.batch_execute(&format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{RLS_ROLE}') THEN \
             EXECUTE 'DROP OWNED BY {RLS_ROLE}'; EXECUTE 'DROP ROLE {RLS_ROLE}'; \
           END IF; \
         END $$;"
    ))
    .await
    .expect("drop the non-bypassing reader role");
}

/// Derive a `role` URL for the same database as `url`. `migrate-catalog` opens
/// its OWN connection from a URL string, so handing it a non-bypassing URL is the
/// only way to drive the REAL verb as a non-bypassing identity (`SET ROLE` on
/// this test's connection would not reach it). `None` for a non-TCP url.
fn role_url(url: &str, role: &str) -> Option<String> {
    let config: tokio_postgres::Config = url.parse().ok()?;
    let host = match config.get_hosts().first()? {
        tokio_postgres::config::Host::Tcp(host) => host.clone(),
        _ => return None,
    };
    let port = config.get_ports().first().copied().unwrap_or(5432);
    let dbname = config.get_dbname()?;
    Some(format!("postgres://{role}:{role}@{host}:{port}/{dbname}"))
}

/// Registrations visible to `role` on this connection — the measurement that
/// makes the RLS silence observable rather than asserted.
async fn registrations_visible_as(su: &Client, role: &str) -> i64 {
    su.batch_execute(&format!("SET ROLE {role}"))
        .await
        .expect("assume the non-bypassing identity");
    let visible = reg_count(su).await;
    su.batch_execute("RESET ROLE")
        .await
        .expect("drop back to the superuser");
    visible
}

/// The applied catalog version for `t1`/`shop`/`dev`, or `None` when nothing is
/// applied — the proof a refusal advanced nothing.
async fn applied_version(su: &Client) -> Option<i32> {
    su.query_opt(
        "SELECT version FROM catalog.catalogs \
         WHERE tenant_id='t1' AND catalog_id='shop' AND environment='dev' AND state='applied'",
        &[],
    )
    .await
    .expect("read applied version")
    .map(|row| row.get(0))
}

/// wamn-0h0g.12.119 — THE NAMED MUTANT TARGET. The D24 guard fails CLOSED on a
/// registration set it cannot read, on BOTH `migrate-catalog` call sites (the
/// `--dry-run` probe and the real apply). Restoring the `Ok(())` early return for
/// the absent probe fails this test.
///
/// Each unreadable state is driven against an **additive** target first, on
/// purpose: an additive migration is refused by NOTHING else, so a guard that
/// fails open shows up as a clean, successful apply rather than as some other
/// gate's refusal. That is what makes this a sharp mutant kill instead of a test
/// that passes on the wrong error.
#[tokio::test]
async fn an_unreadable_registration_set_refuses_the_migration_by_name() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-0h0g.12.119 refusal gate");
        return;
    };
    let _serialized = SERIALIZE.lock().await;
    let su = connect(&url).await;
    reset(&su).await;

    let v1 = write_tmp(
        "d24_unread_v1.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );
    let additive = write_tmp(
        "d24_unread_additive.json",
        &cat_json(2, &format!("{E_SALES_ADDITIVE},{E_LINES}")),
    );
    let destructive = write_tmp("d24_unread_destructive.json", &cat_json(2, E_LINES));

    // v1 materializes both entities against a provisioned, empty registration set.
    migrate_catalog::run(migrate_args(v1, &url))
        .await
        .expect("v1 materializes");
    assert!(table_present(&su, &format!("{DATA_SCHEMA}.orders")).await);
    assert_eq!(applied_version(&su).await, Some(1));

    // --- 1. ABSENT: the catalog schema was dropped and only partially rebuilt. ---
    su.batch_execute("DROP TABLE catalog.event_registrations")
        .await
        .expect("drop the registration table");

    // The DRY RUN refuses: the point is that no plan is ever cleared by a
    // registration set nobody could read, not merely that the ALTERs are skipped.
    let err = migrate_catalog::run(migrate_dry_run_args(additive.clone(), &url))
        .await
        .expect_err("an absent registration table must refuse the dry run");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("orphan-guard-registrations-absent"),
        "dry run refuses by name: {msg}"
    );
    assert!(
        msg.contains("dry-run"),
        "marked as a dry-run finding: {msg}"
    );

    // The APPLY refuses too — this is the live, destructive call site.
    let err = migrate_catalog::run(migrate_args(additive.clone(), &url))
        .await
        .expect_err("an absent registration table must refuse the apply");
    assert!(
        format!("{err:#}").contains("orphan-guard-registrations-absent"),
        "apply refuses by name: {err:#}"
    );
    // The refused apply really did not run: no column, no version advance.
    assert!(
        !column_present(&su, "orders", "note").await,
        "the refused additive apply added nothing"
    );
    assert_eq!(
        applied_version(&su).await,
        Some(1),
        "the refused apply advanced no version"
    );

    // NO ENTITY IS REMOVED as a side effect of an unreadable registration set:
    // the destructive target is refused by the guard, ahead of every other gate.
    let err = migrate_catalog::run(migrate_args(destructive, &url))
        .await
        .expect_err("a destructive target must refuse on an absent registration set");
    assert!(
        format!("{err:#}").contains("orphan-guard-registrations-absent"),
        "the destructive apply refuses by NAME, not by the additive-only gate: {err:#}"
    );
    assert!(
        table_present(&su, &format!("{DATA_SCHEMA}.orders")).await,
        "the entity table survives an unreadable registration set"
    );
    assert_eq!(applied_version(&su).await, Some(1));

    // --- 2. HIDDEN BY ROW-LEVEL SECURITY. Rebuild the registration storage from ---
    // the real artifact and seed it, then read it as a non-bypassing identity.
    su.batch_execute("DROP SCHEMA catalog CASCADE")
        .await
        .expect("drop catalog for the rebuild");
    su.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("rebuild deploy/sql/catalog-schema.sql");
    create_rls_reader(&su).await;
    insert_reg(&su, "t1", "reg-t1", "sales_orders").await;
    insert_reg(&su, "t2", "reg-t2", "sales_orders").await;

    // THE SILENCE, PROVEN LIVE: the superuser sees both registrations; the
    // non-bypassing reader sees ZERO ROWS and NO ERROR, because the table is
    // FORCE ROW LEVEL SECURITY and no `app.tenant` claim is injected. That zero is
    // what the guard used to consume as "nothing is referenced".
    assert_eq!(reg_count(&su).await, 2, "the superuser sees both");
    assert_eq!(
        registrations_visible_as(&su, RLS_ROLE).await,
        0,
        "FORCE RLS hides the registrations from a non-bypassing role with NO error — \
         the silent state this bead closes"
    );

    let Some(app_url) = role_url(&url, RLS_ROLE) else {
        eprintln!("WAMN_CTL_PG_URL is not a TCP url — skipping the RLS-filtered arm");
        drop_rls_reader(&su).await;
        return;
    };
    // The REAL verb, driven as the non-bypassing identity, refuses the silence
    // instead of clearing the migration from it.
    //
    // Only the `--dry-run` call site is reachable this way. The apply path first
    // runs `ensure_wamn_app_role` / `ensure_catalog_storage`, whose REVOKE and
    // GRANT statements require OWNERSHIP of the catalog tables, so reaching the
    // apply guard as a non-bypassing caller means making that caller the schema
    // owner — and `deploy/sql/catalog-schema.sql` hard-codes
    // `CREATE SCHEMA catalog AUTHORIZATION postgres`. The two call sites share
    // this one function, and the apply site is proven by the absent arm above.
    let err = migrate_catalog::run(migrate_dry_run_args(additive, &app_url))
        .await
        .expect_err("an RLS-hidden registration set must refuse, not clear the migration");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("orphan-guard-registrations-unreadable"),
        "refuses by name: {msg}"
    );

    // Nothing was touched by the refusal.
    assert!(
        table_present(&su, &format!("{DATA_SCHEMA}.orders")).await,
        "the entity table survives the RLS-hidden refusal"
    );
    assert_eq!(reg_count(&su).await, 2, "registrations untouched");

    drop_rls_reader(&su).await;
    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}

/// wamn-0h0g.12.119 — THE PRECISION CONTROL, and the test that must stay GREEN
/// under the mutant. A correctly-provisioned project-env whose registration table
/// holds GENUINELY ZERO rows is a legitimate state, not an unreadable one: both
/// `migrate-catalog` call sites must still pass. This is what keeps the refusal
/// precise rather than blunt — a guard that refused every empty read would be
/// just as wrong, and would break every unregistered project.
#[tokio::test]
async fn a_provisioned_but_empty_registration_set_still_passes_the_guard() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the wamn-0h0g.12.119 precision control");
        return;
    };
    let _serialized = SERIALIZE.lock().await;
    let su = connect(&url).await;
    reset(&su).await;

    let v1 = write_tmp(
        "d24_empty_v1.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );
    let additive = write_tmp(
        "d24_empty_additive.json",
        &cat_json(2, &format!("{E_SALES_ADDITIVE},{E_LINES}")),
    );

    // Provisioned and genuinely empty — the state the refusal must NOT catch.
    assert!(table_present(&su, "catalog.event_registrations").await);
    assert_eq!(reg_count(&su).await, 0);

    migrate_catalog::run(migrate_args(v1, &url))
        .await
        .expect("an empty-but-provisioned registration set still applies v1");
    assert!(table_present(&su, &format!("{DATA_SCHEMA}.orders")).await);

    migrate_catalog::run(migrate_dry_run_args(additive.clone(), &url))
        .await
        .expect("an empty-but-provisioned registration set still passes the dry-run guard");
    migrate_catalog::run(migrate_args(additive, &url))
        .await
        .expect("an empty-but-provisioned registration set still passes the apply guard");
    assert!(
        column_present(&su, "orders", "note").await,
        "the additive apply proceeded"
    );
    assert_eq!(applied_version(&su).await, Some(2));

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}

async fn publish_scenario(su: &Client, url: &str) {
    reset(su).await;

    let ab = write_tmp(
        "d24_pub_ab.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );
    let a_only = write_tmp("d24_pub_a.json", &cat_json(2, E_SALES));
    let b_only = write_tmp("d24_pub_b.json", &cat_json(3, E_LINES));

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
    migrate_catalog::run(migrate_args(ab, url))
        .await
        .expect("first materialization applies");
    assert!(table_present(su, &format!("{DATA_SCHEMA}.orders")).await);
    assert!(table_present(su, &format!("{DATA_SCHEMA}.lines")).await);

    insert_reg(su, "t1", "reg-t1", "sales_orders").await;
    insert_reg(su, "t2", "reg-t2", "sales_orders").await;

    // v2 removes `sales_orders` — still referenced. REFUSED before the apply tx,
    // independently of the default command's additive-only gate.
    let err = migrate_catalog::run(migrate_args(b_v2.clone(), url))
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

    // Delete the registrations. The orphan guard now clears, exposing the
    // separate additive-only refusal; the default command still mutates nothing.
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = 'shop'",
        &[],
    )
    .await
    .expect("owner deletes the registrations");
    let err = migrate_catalog::run(migrate_args(b_v2, url))
        .await
        .expect_err("default migrate refuses the unreferenced destructive target");
    let msg = err.to_string();
    assert!(msg.contains("destructive"), "refusal is destructive: {msg}");
    assert!(
        msg.contains("reprovision"),
        "refusal names the supported replacement path: {msg}"
    );
    for orphan in ["reg-t1", "reg-t2"] {
        assert!(
            !msg.contains(orphan),
            "cleared orphan {orphan:?} is absent from the additive-only refusal: {msg}"
        );
    }
    assert!(
        table_present(su, &format!("{DATA_SCHEMA}.orders")).await,
        "the additive-only refusal leaves the unreferenced entity table intact"
    );
    let applied: i32 = su
        .query_one(
            "SELECT version FROM catalog.catalogs \
             WHERE tenant_id='t1' AND catalog_id='shop' AND environment='dev' AND state='applied'",
            &[],
        )
        .await
        .expect("read applied version after additive-only refusal")
        .get(0);
    assert_eq!(applied, 1, "the additive-only refusal preserved v1");

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}

/// wamn-1bfe: `migrate-catalog --dry-run` must SURFACE the D24 refusal, so an
/// operator cannot dry-run clean and then fail the real run. A dry run whose
/// target removes a still-referenced entity fails with the marked verdict naming
/// every orphan across ALL tenants — while mutating NOTHING. Once the
/// registrations are deleted, the distinct additive-only refusal proves the
/// orphan probe was not vacuous.
async fn dry_run_scenario(su: &Client, url: &str) {
    reset(su).await;

    let ab = write_tmp(
        "d24_dry_ab.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );
    let b_v2 = write_tmp("d24_dry_b_v2.json", &cat_json(2, E_LINES));

    // v1: materialize {sales_orders -> orders, line_items -> lines}.
    migrate_catalog::run(migrate_args(ab, url))
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

    // Delete the registrations. The orphan guard now clears and the same dry run
    // reaches the separate additive-only refusal, proving the probe was not the
    // source of the remaining error.
    su.execute(
        "DELETE FROM catalog.event_registrations WHERE catalog_id = 'shop'",
        &[],
    )
    .await
    .expect("owner deletes the registrations");
    let err = migrate_catalog::run(migrate_dry_run_args(b_v2, url))
        .await
        .expect_err("destructive dry-run refuses after the orphan is cleared");
    let msg = err.to_string();
    assert!(msg.contains("destructive"), "refusal is destructive: {msg}");
    for orphan in ["reg-t1", "reg-t2"] {
        assert!(
            !msg.contains(orphan),
            "cleared orphan {orphan:?} is absent from the additive-only refusal: {msg}"
        );
    }
    assert!(
        table_present(su, &format!("{DATA_SCHEMA}.orders")).await,
        "the destructive dry-run leaves the entity table intact"
    );

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}

/// wamn-nr61: `migrate-catalog --dry-run` must be STRICTLY read-only (the 1wdq
/// reconcile-run-plane standard) — it must NOT run `ensure_data_schema`'s
/// CREATE SCHEMA. Against an env whose data schema does not exist yet, the dry
/// run still plans coherently and exits cleanly (the pure planner never touches
/// the live data schema; the read is catalog-qualified; `SET search_path` skips
/// the absent schema), while the schema set stays byte-identical — then a real
/// run provisions it.
async fn dry_run_no_data_schema_scenario(su: &Client, url: &str) {
    reset(su).await; // drops DATA_SCHEMA; recreates the catalog metadata schema

    // Precondition: the data schema does not exist.
    assert!(
        !schema_present(su, DATA_SCHEMA).await,
        "precondition: the data schema is absent before the dry run"
    );
    let before = schema_names(su).await;

    let ab = write_tmp(
        "nr61_dry_ab.json",
        &cat_json(1, &format!("{E_SALES},{E_LINES}")),
    );

    // A dry run against the not-yet-existing data schema plans cleanly (no
    // registrations → no orphan) — the planner tolerates the absent schema.
    migrate_catalog::run(migrate_dry_run_args(ab.clone(), url))
        .await
        .expect("dry-run against an absent data schema plans cleanly");

    // THE NAMED ASSERT (nr61 mutant target): the dry run created NOTHING — the
    // schema set is unchanged and the data schema is still absent. Reinstating
    // `ensure_data_schema` on the dry-run path CREATEs DATA_SCHEMA and fails here.
    assert_eq!(
        schema_names(su).await,
        before,
        "dry-run must not create the data schema (schema set unchanged)"
    );
    assert!(
        !schema_present(su, DATA_SCHEMA).await,
        "dry-run left the data schema absent"
    );

    // A real (non-dry) run then provisions the schema + materializes the tables.
    migrate_catalog::run(migrate_args(ab, url))
        .await
        .expect("the real run provisions the data schema");
    assert!(
        schema_present(su, DATA_SCHEMA).await,
        "the real run created the data schema"
    );
    assert!(table_present(su, &format!("{DATA_SCHEMA}.orders")).await);
    assert!(table_present(su, &format!("{DATA_SCHEMA}.lines")).await);

    // A forward additive change is accepted by both paths. Dry-run reports it
    // without adding the column; apply adds it and advances the catalog.
    let additive = write_tmp(
        "nr61_additive.json",
        &cat_json(2, &format!("{E_SALES_ADDITIVE},{E_LINES}")),
    );
    migrate_catalog::run(migrate_dry_run_args(additive.clone(), url))
        .await
        .expect("additive dry-run succeeds");
    assert!(
        !column_present(su, "orders", "note").await,
        "additive dry-run does not add the column"
    );
    migrate_catalog::run(migrate_args(additive, url))
        .await
        .expect("additive apply succeeds");
    assert!(
        column_present(su, "orders", "note").await,
        "additive apply adds the column"
    );
    let applied: i32 = su
        .query_one(
            "SELECT version FROM catalog.catalogs \
             WHERE tenant_id='t1' AND catalog_id='shop' AND environment='dev' AND state='applied'",
            &[],
        )
        .await
        .expect("read applied additive version")
        .get(0);
    assert_eq!(applied, 2, "additive apply advances the catalog");

    su.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS {DATA_SCHEMA} CASCADE"
    ))
    .await
    .expect("teardown");
}
