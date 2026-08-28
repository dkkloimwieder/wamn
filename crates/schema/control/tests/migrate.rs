//! Tests for the migration engine (2.5).
//!
//! Three layers (the wamn-schema-compiler / wamn-project-state precedent):
//! - **unit** — the guards (forward-only, catalog-id, stale-base), the
//!   additive-only public boundary, and a metadata-only version bump;
//! - a **drift guard** tying `deploy/sql/catalog-schema.sql` to the engine (the new
//!   `document` column, the `schema_migrations` table + columns, and the
//!   environment / lifecycle-state literals the SQL builders use);
//! - a **live-apply gate** proving the DB-enforced behavior end-to-end — a first
//!   materialization, a forward migration (document round-trip, single-applied
//!   advance, history), and destructive refusal — over a real Postgres
//!   (`WAMN_MIGRATE_PG_URL`, a superuser URL; skipped when unset).

use std::path::Path;
use std::sync::Mutex;

use wamn_schema_control::{
    Env, MigrationError, MigrationRequest, SqlStatement, Value, plan_migration,
};
use wamn_schema_model::{Catalog, Entity, Field, FieldType};

static LIVE_DATABASE: Mutex<()> = Mutex::new(());

// --- fixtures ---------------------------------------------------------------

fn field(id: &str, ty: FieldType, nullable: bool) -> Field {
    Field {
        id: id.into(),
        name: id.into(),
        field_type: ty,
        nullable,
        default: None,
        sensitive: false,
        is_system: false,
        label: None,
        description: None,
    }
}

/// A single-entity `widgets` catalog. `with_note` adds a nullable `note` column,
/// so v1(sku) -> v2(sku,note) is additive and v2 -> v3(sku) is destructive.
fn widget_catalog(version: u32, with_note: bool) -> Catalog {
    let mut fields = vec![field("sku", FieldType::Text { max_len: None }, false)];
    if with_note {
        fields.push(field("note", FieldType::Text { max_len: None }, true));
    }
    Catalog {
        schema_version: "0.1".into(),
        catalog_id: "widgets".into(),
        version,
        name: None,
        entities: vec![Entity {
            id: "widget".into(),
            name: "widget".into(),
            is_system: false,
            label: None,
            description: None,
            fields,
            indexes: vec![],
            constraints: vec![],
        }],
        relations: vec![],
    }
}

fn req<'a>(
    current: Option<&'a Catalog>,
    target: &'a Catalog,
    expected_base: Option<u32>,
) -> MigrationRequest<'a> {
    MigrationRequest {
        tenant: "t1",
        environment: Env::new("dev"),
        current,
        target,
        expected_base,
    }
}

fn has_stmt_with(plan: &[SqlStatement], needle: &str) -> bool {
    plan.iter().any(|s| s.sql.contains(needle))
}

// --- unit -------------------------------------------------------------------

#[test]
fn first_materialization_plans_a_create() {
    let v1 = widget_catalog(1, false);
    let plan = plan_migration(&req(None, &v1, None)).unwrap();

    assert_eq!(plan.from_version, None);
    assert_eq!(plan.to_version, 1);
    assert!(!plan.destructive);
    assert!(plan.warnings.is_empty());
    // DDL (CREATE) + demote + upsert-applied + history.
    assert!(has_stmt_with(&plan.statements, "CREATE TABLE"));
    assert!(has_stmt_with(&plan.statements, "catalog.catalogs"));
    // The immutable history row is always recorded (a load-bearing statement).
    assert!(
        has_stmt_with(&plan.statements, "catalog.schema_migrations"),
        "every apply records a schema_migrations row"
    );
    // The applied version stores the catalog document (the diff source).
    let upsert = plan
        .statements
        .iter()
        .find(|s| s.sql.contains("INSERT INTO catalog.catalogs"))
        .expect("an upsert-applied statement");
    assert!(
        upsert
            .params
            .iter()
            .any(|p| matches!(p, Value::Text(t) if t.contains("\"catalog-id\""))),
        "the upsert binds the catalog document"
    );
}

#[test]
fn forward_migration_plans_an_additive_diff() {
    let v1 = widget_catalog(1, false);
    let v2 = widget_catalog(2, true);
    let plan = plan_migration(&req(Some(&v1), &v2, None)).unwrap();

    assert_eq!(plan.from_version, Some(1));
    assert_eq!(plan.to_version, 2);
    assert!(!plan.destructive);
    assert!(has_stmt_with(&plan.statements, "ADD COLUMN"));
    // The history row records from -> to.
    let history = plan
        .statements
        .iter()
        .find(|s| s.sql.contains("catalog.schema_migrations"))
        .unwrap();
    assert!(history.params.contains(&Value::NullableInt(Some(1)))); // from_version
    assert!(history.params.contains(&Value::Int(2))); // to_version
}

#[test]
fn forward_only_rejects_older_and_equal() {
    let v1 = widget_catalog(1, false);
    let v2 = widget_catalog(2, true);
    // equal version -> already applied
    match plan_migration(&req(Some(&v1), &v1, None)) {
        Err(MigrationError::AlreadyApplied { version: 1 }) => {}
        other => panic!("expected AlreadyApplied, got {other:?}"),
    }
    // older version -> not forward (current is v2, target is v1)
    match plan_migration(&req(Some(&v2), &v1, None)) {
        Err(MigrationError::NotForward {
            target: 1,
            current: 2,
        }) => {}
        other => panic!("expected NotForward, got {other:?}"),
    }
}

#[test]
fn catalog_id_mismatch_is_rejected() {
    let mut other = widget_catalog(1, false);
    other.catalog_id = "other".into();
    let v2 = widget_catalog(2, true);
    match plan_migration(&req(Some(&other), &v2, None)) {
        Err(MigrationError::CatalogIdMismatch { current, target }) => {
            assert_eq!(current, "other");
            assert_eq!(target, "widgets");
        }
        other => panic!("expected CatalogIdMismatch, got {other:?}"),
    }
}

#[test]
fn stale_base_is_rejected() {
    // current applied is v2; target v3 claims it was branched from v1 -> stale.
    let v2 = widget_catalog(2, true);
    let v3 = widget_catalog(3, false);
    match plan_migration(&req(Some(&v2), &v3, Some(1))) {
        Err(MigrationError::StaleBase {
            expected_base: Some(1),
            current_applied: Some(2),
        }) => {}
        other => panic!("expected StaleBase, got {other:?}"),
    }
    // With the correct base, lifecycle validation succeeds and the additive-only
    // boundary becomes the refusal.
    assert!(matches!(
        plan_migration(&req(Some(&v2), &v3, Some(2))),
        Err(MigrationError::Destructive(_))
    ));
}

#[test]
fn destructive_migration_is_not_a_public_capability() {
    // v2 -> v3 drops the `note` column: destructive.
    let v2 = widget_catalog(2, true);
    let v3 = widget_catalog(3, false);
    match plan_migration(&req(Some(&v2), &v3, None)) {
        Err(MigrationError::Destructive(error)) => {
            assert!(error.operations.iter().any(|op| op.contains("drop column")));
        }
        other => panic!("expected Destructive, got {other:?}"),
    }
}

#[test]
fn a_metadata_only_version_bump_still_advances_the_lifecycle() {
    // Same content, a newer version: empty DDL, but the lifecycle + history advance.
    let v1 = widget_catalog(1, false);
    let mut v2 = widget_catalog(2, false);
    v2.name = Some("renamed".into()); // header-only change, no structural diff
    let plan = plan_migration(&req(Some(&v1), &v2, None)).unwrap();

    assert!(!has_stmt_with(&plan.statements, "ALTER TABLE"));
    assert!(!has_stmt_with(&plan.statements, "CREATE TABLE"));
    // demote + upsert-applied + history (no DDL statement).
    assert_eq!(plan.statements.len(), 3);
    assert!(has_stmt_with(&plan.statements, "catalog.schema_migrations"));
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("no structural changes"))
    );
}

// --- drift guard ------------------------------------------------------------

fn catalog_schema_sql() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/sql/catalog-schema.sql");
    std::fs::read_to_string(p).expect("read deploy/sql/catalog-schema.sql")
}

/// The SQL with `--` line comments stripped (no `--` appears inside a string
/// literal in this file, so a per-line truncate is exact).
fn code_only(sql: &str) -> String {
    sql.lines()
        .map(|l| l.find("--").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn catalog_schema_sql_mirrors_the_engine() {
    let sql = code_only(&catalog_schema_sql());

    // The applied-catalog `document` column the engine writes + diffs against.
    assert!(
        sql.lines()
            .any(|l| l.contains("document") && l.contains("jsonb")),
        "catalog.catalogs must carry a `document jsonb` column"
    );

    // The migration-history table + its columns (as the SQL builders reference them).
    assert!(sql.contains("CREATE TABLE catalog.schema_migrations"));
    for col in [
        "from_version",
        "to_version",
        "statement_count",
        "destructive",
        "checksum",
    ] {
        assert!(
            sql.contains(col),
            "schema_migrations is missing pinned column {col:?}"
        );
    }

    // `environment` is an open slug (D18): the closed CHECK is retired.
    assert!(
        !sql.contains("schema_migrations_environment_check"),
        "the closed environment CHECK must be retired (env is an open slug)"
    );

    // The lifecycle state literals the builders write must exist in the DDL CHECK.
    let demote = wamn_schema_control::sql::demote_current_applied_sql();
    let upsert = wamn_schema_control::sql::upsert_applied_version_sql();
    assert!(demote.contains("'superseded'") && sql.contains("'superseded'"));
    assert!(upsert.contains("'applied'") && sql.contains("'applied'"));
    // The builders target the fixed `catalog` metadata schema.
    assert!(demote.contains("catalog.catalogs"));
    assert!(wamn_schema_control::sql::record_migration_sql().contains("catalog.schema_migrations"));
}

#[test]
fn select_applied_catalogs_enumerates_an_envs_applied_set() {
    // This test is the builder's only caller: the copy definition pass that
    // motivated it was deleted (`5bb69f0d`). What is asserted is the shape the
    // builder still guarantees — (tenant, environment)-scoped,
    // applied-state-only, and deterministic.
    let sql = wamn_schema_control::sql::select_applied_catalogs_sql();
    assert!(sql.contains("FROM catalog.catalogs"));
    for col in ["catalog_id", "version", "document::text"] {
        assert!(sql.contains(col), "missing column {col}");
    }
    assert!(sql.contains("tenant_id = $1 AND environment = $2"));
    assert!(sql.contains("state = 'applied'"), "applied versions only");
    assert!(sql.contains("ORDER BY catalog_id"), "deterministic order");
}

// --- live-apply gate --------------------------------------------------------

/// Substitute the positional `$n` params into a statement's SQL as literals, so
/// the engine's real builder strings run under `psql` (the driver binds them with
/// `$n` — this proves the same SQL shape). Highest-to-lowest so `$1` never
/// matches inside `$10`+ (there are at most 8 params).
fn render(stmt: &SqlStatement) -> String {
    let mut sql = stmt.sql.clone();
    for (i, v) in stmt.params.iter().enumerate().rev() {
        let ph = format!("${}", i + 1);
        sql = sql.replace(&ph, &lit(v));
    }
    sql
}

fn lit(v: &Value) -> String {
    match v {
        Value::Text(s) | Value::NullableText(Some(s)) => format!("'{}'", s.replace('\'', "''")),
        Value::NullableText(None) | Value::NullableInt(None) => "NULL".into(),
        Value::Int(i) | Value::NullableInt(Some(i)) => i.to_string(),
        Value::Bool(b) => b.to_string(),
    }
}

fn apply_block(plan: &wamn_schema_control::ApplyPlan) -> String {
    let mut out = String::from("BEGIN;\n");
    for s in &plan.statements {
        let r = render(s);
        let r = r.trim_end();
        out.push_str(r);
        if !r.ends_with(';') {
            out.push(';');
        }
        out.push('\n');
    }
    out.push_str("COMMIT;\n");
    out
}

#[test]
fn catalog_schema_from_zero_is_complete_and_transactional_on_postgres() {
    let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
        eprintln!(
            "skipping catalog_schema_from_zero_is_complete_and_transactional_on_postgres \
             (set WAMN_MIGRATE_PG_URL to run)"
        );
        return;
    };
    let _live_database = LIVE_DATABASE
        .lock()
        .expect("live database test lock is not poisoned");

    run_psql(
        &url,
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
         END IF; END $$;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;",
    );

    let schema = catalog_schema_sql();
    let faulted = schema.replacen(
        "CREATE TABLE catalog.schema_migrations (",
        "SELECT 1 / 0;\nCREATE TABLE catalog.schema_migrations (",
        1,
    );
    assert_ne!(faulted, schema, "fault seam must exist in canonical DDL");
    run_psql_expect_failure(&url, &format!("BEGIN;\n{faulted}\nCOMMIT;"));
    assert_eq!(
        query_psql(&url, "SELECT to_regnamespace('catalog') IS NULL").trim(),
        "t",
        "the injected failure must roll back every earlier catalog object"
    );

    run_psql(
        &url,
        &format!(
            "BEGIN;\n{schema}\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT count(*) FROM pg_tables WHERE schemaname='catalog')=20,\n\
                 'complete catalog table set';\n\
               ASSERT to_regclass('catalog.event_registrations') IS NOT NULL,\n\
                 'event registrations table';\n\
               ASSERT EXISTS (\n\
                 SELECT 1 FROM pg_policies\n\
                 WHERE schemaname='catalog' AND tablename='event_registrations'\n\
                   AND policyname='event_registrations_tenant'\n\
               ), 'event registrations tenant policy';\n\
               ASSERT has_table_privilege(\n\
                 'wamn_app', 'catalog.event_registrations',\n\
                 'SELECT, INSERT, UPDATE, DELETE'\n\
               ), 'event registrations grant';\n\
               ASSERT to_regclass('catalog.event_registrations_by_entity') IS NOT NULL,\n\
                 'event registrations entity index';\n\
             END $$;\n\
             COMMIT;"
        ),
    );

    run_psql(&url, "DROP SCHEMA catalog CASCADE;");
}

#[test]
fn migration_engine_applies_forward_and_refuses_destructive_on_postgres() {
    let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
        eprintln!(
            "skipping migration_engine_applies_forward_and_refuses_destructive_on_postgres \
             (set WAMN_MIGRATE_PG_URL to run)"
        );
        return;
    };
    let _live_database = LIVE_DATABASE
        .lock()
        .expect("live database test lock is not poisoned");

    let v1 = widget_catalog(1, false);
    let v2 = widget_catalog(2, true);
    let v3 = widget_catalog(3, false);

    // Plans built by the REAL engine.
    let plan_a = plan_migration(&req(None, &v1, None)).unwrap();
    let plan_b = plan_migration(&req(Some(&v1), &v2, None)).unwrap();
    // The default boundary cannot construct the destructive plan.
    assert!(matches!(
        plan_migration(&req(Some(&v2), &v3, None)),
        Err(MigrationError::Destructive(_))
    ));
    let mut script = String::new();
    // Provision wamn_app (as in production) and a fresh catalog + data schema.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_migrate_data CASCADE;\n",
    );
    script.push_str(&catalog_schema_sql());
    script.push('\n');
    script.push_str(
        "CREATE SCHEMA wamn_migrate_data AUTHORIZATION CURRENT_USER;\n\
         GRANT USAGE ON SCHEMA wamn_migrate_data TO wamn_app;\n\
         SET search_path = wamn_migrate_data, catalog;\n",
    );

    // Scenario A — first materialization.
    script.push_str(&apply_block(&plan_a));
    script.push_str(
        "DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM catalog.catalogs WHERE state='applied')=1, 'A: one applied';\n\
           ASSERT (SELECT version FROM catalog.catalogs WHERE state='applied')=1, 'A: v1 applied';\n\
           ASSERT (SELECT document IS NOT NULL FROM catalog.catalogs WHERE version=1), 'A: document stored';\n\
           ASSERT (SELECT document->>'catalog-id' FROM catalog.catalogs WHERE version=1)='widgets', 'A: document is the catalog';\n\
           ASSERT to_regclass('wamn_migrate_data.widget') IS NOT NULL, 'A: widget table created';\n\
           ASSERT (SELECT count(*) FROM catalog.schema_migrations WHERE to_version=1 AND from_version IS NULL AND destructive=false)=1, 'A: history row';\n\
         END $$;\n",
    );

    // Scenario B — forward additive migration; the prior applied is demoted.
    script.push_str(&apply_block(&plan_b));
    script.push_str(
        "DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM catalog.catalogs WHERE state='applied')=1, 'B: still one applied';\n\
           ASSERT (SELECT version FROM catalog.catalogs WHERE state='applied')=2, 'B: v2 applied';\n\
           ASSERT (SELECT state FROM catalog.catalogs WHERE version=1)='superseded', 'B: v1 superseded';\n\
           ASSERT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='wamn_migrate_data' AND table_name='widget' AND column_name='note'), 'B: note column added';\n\
           ASSERT (SELECT count(*) FROM catalog.schema_migrations WHERE to_version=2 AND from_version=1)=1, 'B: history v1->v2';\n\
         END $$;\n",
    );

    run_psql(&url, &script);

    // The stored document round-trips through Catalog::from_json — the diff source
    // a subsequent migration reads.
    let doc = query_psql(
        &url,
        "SELECT document::text FROM catalog.catalogs WHERE state='applied' AND catalog_id='widgets'",
    );
    let readback = Catalog::from_json(doc.trim()).expect("stored document parses as a Catalog");
    assert_eq!(readback.catalog_id, "widgets");
    assert_eq!(readback.version, 2);

    // Teardown (leave nothing behind).
    run_psql(
        &url,
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS wamn_migrate_data CASCADE;",
    );
}

fn run_psql(url: &str, script: &str) {
    let out = psql_output(url, script);
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn psql_output(url: &str, script: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::{Command as Proc, Stdio};
    let mut child = Proc::new("psql")
        .arg(url)
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
    child.wait_with_output().unwrap()
}

fn run_psql_expect_failure(url: &str, script: &str) {
    let out = psql_output(url, script);
    assert!(
        !out.status.success(),
        "faulted catalog apply unexpectedly succeeded:\n{script}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("division by zero"),
        "faulted catalog apply failed at the wrong seam: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn query_psql(url: &str, sql: &str) -> String {
    use std::process::Command as Proc;
    let out = Proc::new("psql")
        .arg(url)
        .args(["-tAqc", sql])
        .output()
        .expect("spawn psql");
    assert!(
        out.status.success(),
        "psql query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
