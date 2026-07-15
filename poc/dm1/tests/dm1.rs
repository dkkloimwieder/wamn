//! POC-DM1 tests: the promoted artifacts stay in sync with the shipped tools
//! (drift-guard + pure compile checks), and a gated live-apply gate that builds
//! the whole data model on a throwaway Postgres — migrate the catalog, attach the
//! RLS, seed the reference data, seat the `app_system` personas — and asserts the
//! database enforces site-scoped RLS, the ERP receipts gate, the composite unique,
//! and exact-decimal specs. Mirrors the wamn-migrate / wamn-rls live-apply gates.

use std::path::{Path, PathBuf};

use wamn_dm1::{CATALOG_ID, catalog, policy, provisioning_sql, seed};

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

// --- drift guard: the promoted catalog stays == the wamn-catalog fixture -------

#[test]
fn promoted_catalog_matches_the_wamn_catalog_fixture() {
    let fixture = std::fs::read_to_string(repo(
        "crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json",
    ))
    .expect("read the wamn-catalog POC fixture");
    let fixture = wamn_catalog::Catalog::from_json(&fixture).expect("fixture parses");
    // The deploy/ artifact is a promotion of the fixture; keep them identical so
    // the same catalog the crate tests validate is the one DM1 migrates live.
    assert_eq!(
        catalog(),
        fixture,
        "deploy/poc-material-receiving.catalog.json drifted from the wamn-catalog fixture"
    );
    assert_eq!(catalog().catalog_id, CATALOG_ID);
}

// --- pure compile checks (no DB) -----------------------------------------------

#[test]
fn poc_policy_compiles_to_site_scoping_and_the_erp_gate() {
    let plan = wamn_rls::compile(&policy(), &catalog()).expect("RLS policy compiles");
    let sql = plan
        .sql(wamn_rls::Confirmation::None)
        .expect("additive, gate-free");
    // Inspector hold site-scoping: only the inspector role is constrained, keyed
    // on the (4.2-injected) app.site claim, with the safe NULLIF/uuid coercion.
    assert!(sql.contains("ON \"quality_holds\" AS RESTRICTIVE"));
    assert!(sql.contains(
        "COALESCE(current_setting('app.role', true), '') <> 'inspector' OR (site_id = NULLIF(current_setting('app.site', true), '')::uuid)"
    ));
    // ERP receipts-insert gate: only erp / quality-manager may INSERT receipts.
    assert!(sql.contains("ON \"receipts\" AS RESTRICTIVE"));
    assert!(sql.contains("FOR INSERT"));
    assert!(
        sql.contains(
            "COALESCE(current_setting('app.role', true), '') IN ('erp', 'quality-manager')"
        )
    );
}

#[test]
fn poc_seed_compiles_over_the_catalog() {
    let ds = seed();
    // Validates against the catalog (types, references, required, exact decimals).
    wamn_seed::validate(&ds, &catalog()).expect("seed validates");
    let plan = wamn_seed::compile(&ds, &catalog(), "t1").expect("seed compiles");
    let sql = plan
        .sql(wamn_seed::Confirmation::None)
        .expect("seed is additive");
    // Reference data + the inspector users (the extended system entity).
    for tbl in ["sites", "suppliers", "materials", "users"] {
        assert!(
            sql.contains(&format!("INSERT INTO \"{tbl}\"")),
            "seed should populate {tbl}"
        );
    }
    // Exact-decimal literals, never floats (the 3.1 no-float rule end to end).
    assert!(sql.contains("12.50") && sql.contains("0.050"));
    // Idempotent load.
    assert!(sql.contains("ON CONFLICT (id) DO NOTHING"));
}

#[test]
fn poc_catalog_migrates_to_the_full_data_model() {
    // A first materialization plans a fresh CREATE for the whole catalog.
    let cat = catalog();
    let req = wamn_migrate::MigrationRequest {
        tenant: "t1",
        environment: wamn_migrate::Env::Dev,
        current: None,
        target: &cat,
        expected_base: None,
        confirm: wamn_migrate::Confirmation::None,
    };
    let plan = wamn_migrate::plan_migration(&req).expect("first materialization plans");
    assert!(!plan.destructive);
    assert_eq!(plan.from_version, None);
    assert_eq!(plan.to_version, 1);
    // The DDL statement (params-free) carries the load-bearing 3.1/3.2 shapes.
    let ddl = &plan.statements[0].sql;
    // The is-system users entity migrates to a data-schema table + its extension.
    assert!(ddl.contains("CREATE TABLE \"users\""));
    assert!(ddl.contains("\"cert_level\""));
    // Composite uniqueness (receipt_no, supplier_id), tenant-scoped.
    assert!(ddl.contains("UNIQUE (tenant_id, \"receipt_no\", \"supplier_id\")"));
    // Exact-decimal + unit specs (no float).
    assert!(ddl.contains("numeric(5,2)") && ddl.contains("'unit: pct'"));
    // The tenant floor is present on the generated tables.
    assert!(ddl.contains("current_setting('app.tenant', true)"));
}

#[test]
fn provisioning_sql_composes_the_three_tools() {
    let sql = provisioning_sql("t1").expect("composes");
    // migrate (2.5): the DDL + the lifecycle write.
    assert!(sql.contains("CREATE TABLE \"quality_holds\""));
    assert!(sql.contains("INSERT INTO catalog.catalogs"));
    assert!(sql.contains("INSERT INTO catalog.schema_migrations"));
    // RLS (3.5) then seed (3.6).
    assert!(sql.contains("-- RLS policies (3.5)"));
    assert!(sql.contains("CREATE POLICY \"quality_holds_inspector_site\""));
    assert!(sql.contains("-- seed data (3.6)"));
    assert!(sql.contains("INSERT INTO \"sites\""));
}

#[test]
fn supplier_pricing_is_flagged_sensitive_for_the_4_3_field_mask() {
    // The field-level mask is deferred to 4.3; the catalog carries the flag DM1
    // migrates, so 4.3 can act on it.
    let cat = catalog();
    let suppliers = cat
        .entities
        .iter()
        .find(|e| e.id == "suppliers")
        .expect("suppliers entity");
    let cost = suppliers
        .fields
        .iter()
        .find(|f| f.id == "standard_cost")
        .expect("standard_cost field");
    assert!(
        cost.sensitive,
        "supplier pricing is flagged sensitive (4.3)"
    );
}

// --- live-apply gate (gated on WAMN_DM1_PG_URL) --------------------------------

/// The `app_system` (2.4) personas + the ERP api-key seed. Seated directly (roles
/// / api-keys are not catalog entities); proves the auth substrate is populated.
const APP_SYSTEM_SEED: &str = "\
INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES
  ('t1','inspector',false),('t1','quality-manager',false),('t1','erp',false);
INSERT INTO app_system.users (tenant_id, id, email, status) VALUES
  ('t1', gen_random_uuid(), 'erp@svc.example', 'active');
INSERT INTO app_system.api_keys (tenant_id, id, user_id, name, key_hash, prefix)
  SELECT 't1', gen_random_uuid(), u.id, 'erp-key', 'deadbeefhash', 'erp_'
  FROM app_system.users u WHERE u.email = 'erp@svc.example';
";

/// The transactional fixtures — receipts / lines / holds — the RLS assertions read.
/// Seeded as the superuser (RLS bypassed), referencing the wamn-seed reference rows
/// by natural key: two holds at `hq`, one at `west`.
const FIXTURES: &str = "\
INSERT INTO poc_dm1.receipts (tenant_id, id, receipt_no, supplier_id, site_id, received_at)
  SELECT 't1', gen_random_uuid(), 'R-100', s.id, si.id, now()
  FROM poc_dm1.suppliers s, poc_dm1.sites si WHERE s.name='acme' AND si.code='hq';
INSERT INTO poc_dm1.receipts (tenant_id, id, receipt_no, supplier_id, site_id, received_at)
  SELECT 't1', gen_random_uuid(), 'R-101', s.id, si.id, now()
  FROM poc_dm1.suppliers s, poc_dm1.sites si WHERE s.name='globex' AND si.code='west';
INSERT INTO poc_dm1.receipt_lines (tenant_id, id, receipt_id, material_id, quantity)
  SELECT 't1', gen_random_uuid(), r.id, m.id, '100.000'
  FROM poc_dm1.receipts r, poc_dm1.materials m WHERE r.receipt_no='R-100' AND m.name='resin-a';
INSERT INTO poc_dm1.receipt_lines (tenant_id, id, receipt_id, material_id, quantity)
  SELECT 't1', gen_random_uuid(), r.id, m.id, '42.500'
  FROM poc_dm1.receipts r, poc_dm1.materials m WHERE r.receipt_no='R-101' AND m.name='solvent-b';
INSERT INTO poc_dm1.quality_holds (tenant_id, id, line_id, site_id, status, opened_at)
  SELECT 't1', gen_random_uuid(), l.id, (SELECT id FROM poc_dm1.sites WHERE code='hq'), v.status, now()
  FROM poc_dm1.receipt_lines l JOIN poc_dm1.receipts r ON r.id=l.receipt_id
  CROSS JOIN (VALUES ('open'::text),('escalated'::text)) v(status)
  WHERE r.receipt_no='R-100';
INSERT INTO poc_dm1.quality_holds (tenant_id, id, line_id, site_id, status, opened_at)
  SELECT 't1', gen_random_uuid(), l.id, (SELECT id FROM poc_dm1.sites WHERE code='west'), 'open', now()
  FROM poc_dm1.receipt_lines l JOIN poc_dm1.receipts r ON r.id=l.receipt_id
  WHERE r.receipt_no='R-101';
";

/// Superuser assertions (RLS bypassed): the migrate landed, the seed landed, the
/// composite unique fires, and exact-decimal + unit specs survived.
const ASSERT_SUPERUSER: &str = "\
DO $$ BEGIN
  ASSERT to_regclass('poc_dm1.quality_holds') IS NOT NULL, 'quality_holds table created';
  ASSERT to_regclass('poc_dm1.users') IS NOT NULL, 'system users entity migrated to a data table';
  ASSERT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='poc_dm1' AND table_name='users' AND column_name='cert_level'),
         'users.cert_level extension landed';
  ASSERT (SELECT count(*) FROM catalog.catalogs
          WHERE catalog_id='poc-material-receiving' AND state='applied')=1, 'one applied version';
  ASSERT (SELECT document->>'catalog-id' FROM catalog.catalogs
          WHERE catalog_id='poc-material-receiving' AND state='applied')='poc-material-receiving',
         'applied document is the catalog';
  ASSERT (SELECT count(*) FROM catalog.schema_migrations
          WHERE catalog_id='poc-material-receiving' AND to_version=1)=1, 'history row recorded';
  -- seed landed
  ASSERT (SELECT count(*) FROM poc_dm1.sites)=2, 'seeded sites';
  ASSERT (SELECT count(*) FROM poc_dm1.materials)=3, 'seeded materials';
  ASSERT (SELECT count(*) FROM poc_dm1.users)=2, 'seeded inspector users';
  ASSERT (SELECT cert_level FROM poc_dm1.users WHERE email='hq-inspector@plant.example')='L1',
         'cert_level seeded on the extended system entity';
  -- exact-decimal + unit specs (no float)
  ASSERT (SELECT moisture_max_pct::text FROM poc_dm1.materials WHERE name='resin-a')='12.50',
         'exact-decimal moisture spec';
  ASSERT (SELECT weight_tolerance_kg::text FROM poc_dm1.materials WHERE name='resin-a')='0.050',
         'exact-decimal weight spec';
  ASSERT col_description('poc_dm1.materials'::regclass,
           (SELECT attnum FROM pg_attribute
            WHERE attrelid='poc_dm1.materials'::regclass AND attname='moisture_max_pct'))='unit: pct',
         'unit comment survived';
  -- composite uniqueness (receipt_no, supplier_id) fires
  DECLARE acme uuid := (SELECT id FROM poc_dm1.suppliers WHERE name='acme');
          hq   uuid := (SELECT id FROM poc_dm1.sites WHERE code='hq');
  BEGIN
    INSERT INTO poc_dm1.receipts (tenant_id, id, receipt_no, supplier_id, site_id, received_at)
      VALUES ('t1', gen_random_uuid(), 'R-DUP', acme, hq, now());
    BEGIN
      INSERT INTO poc_dm1.receipts (tenant_id, id, receipt_no, supplier_id, site_id, received_at)
        VALUES ('t1', gen_random_uuid(), 'R-DUP', acme, hq, now());
      RAISE EXCEPTION 'composite unique (receipt_no, supplier_id) did not fire';
    EXCEPTION WHEN unique_violation THEN NULL; -- expected
    END;
  END;
  -- app_system personas + ERP api-key seated (2.4 substrate)
  ASSERT (SELECT count(*) FROM app_system.roles
          WHERE name IN ('inspector','quality-manager','erp'))=3, 'persona roles seated';
  ASSERT EXISTS (SELECT 1 FROM app_system.api_keys ak
                 JOIN app_system.users u ON u.tenant_id=ak.tenant_id AND u.id=ak.user_id
                 WHERE u.email='erp@svc.example'), 'ERP api-key seated';
END $$;
";

#[test]
fn dm1_data_model_applies_and_enforces_policies_on_postgres() {
    let Ok(url) = std::env::var("WAMN_DM1_PG_URL") else {
        eprintln!(
            "skipping dm1_data_model_applies_and_enforces_policies_on_postgres (set WAMN_DM1_PG_URL to run)"
        );
        return;
    };

    let catalog_schema = std::fs::read_to_string(repo("deploy/catalog-schema.sql"))
        .expect("read catalog-schema.sql");
    let app_schema =
        std::fs::read_to_string(repo("deploy/app-schema.sql")).expect("read app-schema.sql");
    let provisioning = provisioning_sql("t1").expect("compose provisioning SQL");

    let mut script = String::new();
    // Provision wamn_app (as in production) + fresh catalog / app_system / data schemas.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS poc_dm1 CASCADE;\n\
         DROP SCHEMA IF EXISTS app_system CASCADE;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n",
    );
    script.push_str(&catalog_schema);
    script.push('\n');
    script.push_str(&app_schema);
    script.push('\n');
    script.push_str(
        "CREATE SCHEMA poc_dm1 AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA poc_dm1 TO wamn_app;\n\
         SET search_path = poc_dm1, catalog;\n",
    );

    // Compose: migrate (2.5) -> RLS (3.5) -> seed (3.6).
    script.push_str(&provisioning);
    // The 2.4 personas + ERP key.
    script.push_str(APP_SYSTEM_SEED);
    // Transactional fixtures the RLS reads.
    script.push_str(FIXTURES);
    // Superuser assertions: migrate/seed landed, composite unique, exact decimals.
    script.push_str(ASSERT_SUPERUSER);

    // --- RLS as wamn_app under session claims (the 4.2-injected claims, set by hand) ---
    // Inspector at HQ sees only HQ holds (2).
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path = poc_dm1;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         SELECT set_config('app.site', (SELECT id::text FROM sites WHERE code='hq'), true);\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM quality_holds)=2, 'inspector at hq sees 2 holds'; END $$;\n\
         COMMIT;\n",
    );
    // Inspector at WEST sees only WEST holds (1).
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path = poc_dm1;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         SELECT set_config('app.site', (SELECT id::text FROM sites WHERE code='west'), true);\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM quality_holds)=1, 'inspector at west sees 1 hold'; END $$;\n\
         COMMIT;\n",
    );
    // A quality-manager is unrestricted (sees all 3).
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path = poc_dm1;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'quality-manager';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM quality_holds)=3, 'manager sees all holds'; END $$;\n\
         COMMIT;\n",
    );
    // An inspector with no site claim is fail-closed (0) — NULLIF coercion.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path = poc_dm1;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         SET LOCAL app.site = '';\n\
         DO $$ BEGIN ASSERT (SELECT count(*) FROM quality_holds)=0, 'no site claim denies all'; END $$;\n\
         COMMIT;\n",
    );
    // Write-scoping (WITH CHECK): an inspector may open a hold at their site, not elsewhere. Rolled back.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path = poc_dm1;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         SELECT set_config('app.site', (SELECT id::text FROM sites WHERE code='hq'), true);\n\
         DO $$ BEGIN\n\
           INSERT INTO quality_holds (tenant_id, id, line_id, site_id, status, opened_at)\n\
             VALUES ('t1', gen_random_uuid(), (SELECT id FROM receipt_lines LIMIT 1),\n\
                     (SELECT id FROM sites WHERE code='hq'), 'open', now());\n\
           BEGIN\n\
             INSERT INTO quality_holds (tenant_id, id, line_id, site_id, status, opened_at)\n\
               VALUES ('t1', gen_random_uuid(), (SELECT id FROM receipt_lines LIMIT 1),\n\
                       (SELECT id FROM sites WHERE code='west'), 'open', now());\n\
             RAISE EXCEPTION 'inspector wrote a hold for another site';\n\
           EXCEPTION WHEN insufficient_privilege THEN NULL; -- expected: RLS WITH CHECK\n\
           END;\n\
         END $$;\n\
         ROLLBACK;\n",
    );
    // ERP gate: the ERP role may INSERT a receipt. Rolled back.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path = poc_dm1;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'erp';\n\
         INSERT INTO receipts (tenant_id, id, receipt_no, supplier_id, site_id, received_at)\n\
           VALUES ('t1', gen_random_uuid(), 'R-ERP', (SELECT id FROM suppliers WHERE name='acme'),\n\
                   (SELECT id FROM sites WHERE code='hq'), now());\n\
         ROLLBACK;\n",
    );
    // ERP gate: an inspector may NOT INSERT a receipt (WITH CHECK denies it). Rolled back.
    script.push_str(
        "BEGIN;\n\
         SET LOCAL ROLE wamn_app;\n\
         SET LOCAL search_path = poc_dm1;\n\
         SET LOCAL app.tenant = 't1';\n\
         SET LOCAL app.role = 'inspector';\n\
         DO $$ BEGIN\n\
           BEGIN\n\
             INSERT INTO receipts (tenant_id, id, receipt_no, supplier_id, site_id, received_at)\n\
               VALUES ('t1', gen_random_uuid(), 'R-NO', (SELECT id FROM suppliers WHERE name='acme'),\n\
                       (SELECT id FROM sites WHERE code='hq'), now());\n\
             RAISE EXCEPTION 'inspector inserted a receipt (ERP gate did not fire)';\n\
           EXCEPTION WHEN insufficient_privilege THEN NULL; -- expected\n\
           END;\n\
         END $$;\n\
         ROLLBACK;\n",
    );

    script.push_str("DROP SCHEMA poc_dm1 CASCADE;\nDROP SCHEMA app_system CASCADE;\nDROP SCHEMA catalog CASCADE;\n");

    use std::io::Write;
    use std::process::{Command as Proc, Stdio};
    let mut child = Proc::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}
