//! Tests for the migration engine (2.5).
//!
//! Three layers (the wamn-ddl / wamn-sysschema precedent):
//! - **unit** — the guards (forward-only, catalog-id, stale-base), the 3.2
//!   destructive gate (reused verbatim), dry-run vs apply, the generated
//!   rollback, and a metadata-only version bump;
//! - a **drift guard** tying `deploy/catalog-schema.sql` to the engine (the new
//!   `document` column, the `schema_migrations` table + columns, and the
//!   confirmation / environment / lifecycle-state literals the SQL builders use);
//! - a **live-apply gate** proving the DB-enforced behavior end-to-end — a first
//!   materialization, a forward migration (document round-trip, single-applied
//!   advance, history), and a gated destructive migration — over a real
//!   Postgres (`WAMN_MIGRATE_PG_URL`, a superuser URL; skipped when unset).

use std::path::Path;

use wamn_catalog::{Catalog, Entity, Field, FieldType};
use wamn_migrate::{
    Confirmation, Env, MigrationError, MigrationRequest, SqlStatement, Value, dry_run,
    plan_migration, rollback_plan,
};

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
    confirm: Confirmation,
) -> MigrationRequest<'a> {
    MigrationRequest {
        tenant: "t1",
        environment: Env::Dev,
        current,
        target,
        expected_base,
        confirm,
    }
}

fn has_stmt_with(plan: &[SqlStatement], needle: &str) -> bool {
    plan.iter().any(|s| s.sql.contains(needle))
}

// --- unit -------------------------------------------------------------------

#[test]
fn first_materialization_plans_a_create() {
    let v1 = widget_catalog(1, false);
    let plan = plan_migration(&req(None, &v1, None, Confirmation::None)).unwrap();

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
    let plan = plan_migration(&req(Some(&v1), &v2, None, Confirmation::None)).unwrap();

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
    match plan_migration(&req(Some(&v1), &v1, None, Confirmation::None)) {
        Err(MigrationError::AlreadyApplied { version: 1 }) => {}
        other => panic!("expected AlreadyApplied, got {other:?}"),
    }
    // older version -> not forward (current is v2, target is v1)
    match plan_migration(&req(Some(&v2), &v1, None, Confirmation::None)) {
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
    match plan_migration(&req(Some(&other), &v2, None, Confirmation::None)) {
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
    match plan_migration(&req(
        Some(&v2),
        &v3,
        Some(1),
        Confirmation::ConfirmedWithBackup,
    )) {
        Err(MigrationError::StaleBase {
            expected_base: Some(1),
            current_applied: Some(2),
        }) => {}
        other => panic!("expected StaleBase, got {other:?}"),
    }
    // With the correct base (2, the current applied), it plans fine.
    assert!(
        plan_migration(&req(
            Some(&v2),
            &v3,
            Some(2),
            Confirmation::ConfirmedWithBackup
        ))
        .is_ok()
    );
}

#[test]
fn destructive_migration_requires_confirmation() {
    // v2 -> v3 drops the `note` column: destructive.
    let v2 = widget_catalog(2, true);
    let v3 = widget_catalog(3, false);
    // Without a confirmed backup, the 3.2 gate refuses.
    match plan_migration(&req(Some(&v2), &v3, None, Confirmation::None)) {
        Err(MigrationError::RequiresConfirmation(_)) => {}
        other => panic!("expected RequiresConfirmation, got {other:?}"),
    }
    // With it, the plan is built and the DDL carries the backup-checkpoint marker.
    let plan = plan_migration(&req(
        Some(&v2),
        &v3,
        None,
        Confirmation::ConfirmedWithBackup,
    ))
    .unwrap();
    assert!(plan.destructive);
    assert!(has_stmt_with(
        &plan.statements,
        "BACKUP CHECKPOINT REQUIRED"
    ));
    assert!(has_stmt_with(&plan.statements, "DROP COLUMN"));
    let history = plan
        .statements
        .iter()
        .find(|s| s.sql.contains("catalog.schema_migrations"))
        .unwrap();
    assert!(history.params.contains(&Value::Bool(true))); // destructive flag
    assert!(
        history
            .params
            .contains(&Value::Text("confirmed-with-backup".into()))
    );
}

#[test]
fn dry_run_reports_a_destructive_migration_without_gating() {
    let v2 = widget_catalog(2, true);
    let v3 = widget_catalog(3, false);
    // dry_run does NOT gate — it reports the destructiveness (unlike plan_migration).
    let report = dry_run(&req(Some(&v2), &v3, None, Confirmation::None)).unwrap();
    assert!(report.destructive);
    assert_eq!(report.from_version, Some(2));
    assert_eq!(report.to_version, 3);
    assert!(report.ddl_report.contains("DESTRUCTIVE"));
    // The generated rollback goes back to v2 (re-adds the dropped column).
    assert!(!report.rollback.is_empty());
    assert!(report.rollback.report().contains("add column"));
    // The rendered dry-run mentions the environment + versions.
    assert!(report.render().contains("2 -> 3"));
}

#[test]
fn rollback_of_a_forward_migration_is_the_inverse() {
    let v1 = widget_catalog(1, false);
    let v2 = widget_catalog(2, true);
    // Rolling back v1 -> v2 drops the added `note` column: destructive inverse.
    let rb = rollback_plan(&req(Some(&v1), &v2, None, Confirmation::None)).unwrap();
    assert!(!rb.is_empty());
    assert!(rb.report().contains("drop column"));
    // The inverse is gated (destructive).
    assert!(matches!(
        rb.sql(Confirmation::None),
        Err(wamn_migrate::RequiresConfirmation { .. })
    ));
    // A first materialization has no prior version: rollback points at drop / restore.
    let rb0 = rollback_plan(&req(None, &v1, None, Confirmation::None)).unwrap();
    assert!(rb0.is_empty());
    assert!(rb0.note.contains("restore-to-last-dump"));
}

#[test]
fn a_metadata_only_version_bump_still_advances_the_lifecycle() {
    // Same content, a newer version: empty DDL, but the lifecycle + history advance.
    let v1 = widget_catalog(1, false);
    let mut v2 = widget_catalog(2, false);
    v2.name = Some("renamed".into()); // header-only change, no structural diff
    let plan = plan_migration(&req(Some(&v1), &v2, None, Confirmation::None)).unwrap();

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
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/catalog-schema.sql");
    std::fs::read_to_string(p).expect("read deploy/catalog-schema.sql")
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
        "confirmation",
        "statement_count",
        "destructive",
        "checksum",
    ] {
        assert!(
            sql.contains(col),
            "schema_migrations is missing pinned column {col:?}"
        );
    }

    // The confirmation CHECK literals must equal the engine's mapping.
    assert!(sql.contains("schema_migrations_confirmation_check"));
    assert!(sql.contains(&format!(
        "'{}'",
        wamn_migrate::sql::confirmation_sql(Confirmation::None)
    )));
    assert!(sql.contains(&format!(
        "'{}'",
        wamn_migrate::sql::confirmation_sql(Confirmation::ConfirmedWithBackup)
    )));

    // The environment CHECK literals must equal the closed Env set.
    assert!(sql.contains("schema_migrations_environment_check"));
    for env in Env::ALL {
        assert!(
            sql.contains(&format!("'{}'", env.as_str())),
            "the environment CHECK must list {:?}",
            env.as_str()
        );
    }

    // The lifecycle state literals the builders write must exist in the DDL CHECK.
    let demote = wamn_migrate::sql::demote_current_applied_sql();
    let upsert = wamn_migrate::sql::upsert_applied_version_sql();
    assert!(demote.contains("'superseded'") && sql.contains("'superseded'"));
    assert!(upsert.contains("'applied'") && sql.contains("'applied'"));
    // The builders target the fixed `catalog` metadata schema.
    assert!(demote.contains("catalog.catalogs"));
    assert!(wamn_migrate::sql::record_migration_sql().contains("catalog.schema_migrations"));
}

// --- live-apply gate --------------------------------------------------------

/// Substitute the positional `$n` params into a statement's SQL as literals, so
/// the engine's real builder strings run under `psql` (the driver binds them with
/// `$n` — this proves the same SQL shape). Highest-to-lowest so `$1` never
/// matches inside `$10`+ (there are at most 9 params).
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

fn apply_block(plan: &wamn_migrate::ApplyPlan) -> String {
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
fn migration_engine_applies_forward_and_gates_destructive_on_postgres() {
    let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
        eprintln!(
            "skipping migration_engine_applies_forward_and_gates_destructive_on_postgres \
             (set WAMN_MIGRATE_PG_URL to run)"
        );
        return;
    };

    let v1 = widget_catalog(1, false);
    let v2 = widget_catalog(2, true);
    let v3 = widget_catalog(3, false);

    // Plans built by the REAL engine.
    let plan_a = plan_migration(&req(None, &v1, None, Confirmation::None)).unwrap();
    let plan_b = plan_migration(&req(Some(&v1), &v2, None, Confirmation::None)).unwrap();
    // The destructive migration is refused without a confirmed backup (pure gate).
    assert!(matches!(
        plan_migration(&req(Some(&v2), &v3, None, Confirmation::None)),
        Err(MigrationError::RequiresConfirmation(_))
    ));
    let plan_c = plan_migration(&req(
        Some(&v2),
        &v3,
        None,
        Confirmation::ConfirmedWithBackup,
    ))
    .unwrap();
    assert!(plan_c.destructive);

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

    // Scenario C — gated destructive migration (drops the note column).
    script.push_str(&apply_block(&plan_c));
    script.push_str(
        "DO $$ BEGIN\n\
           ASSERT (SELECT version FROM catalog.catalogs WHERE state='applied')=3, 'C: v3 applied';\n\
           ASSERT (SELECT state FROM catalog.catalogs WHERE version=2)='superseded', 'C: v2 superseded';\n\
           ASSERT NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='wamn_migrate_data' AND table_name='widget' AND column_name='note'), 'C: note dropped';\n\
           ASSERT (SELECT count(*) FROM catalog.schema_migrations WHERE to_version=3 AND destructive=true AND confirmation='confirmed-with-backup')=1, 'C: destructive history';\n\
         END $$;\n",
    );

    run_psql(&url, &script);

    // The stored document round-trips through Catalog::from_json — the diff source
    // a subsequent migration reads. v3 is the current applied version.
    let doc = query_psql(
        &url,
        "SELECT document::text FROM catalog.catalogs WHERE state='applied' AND catalog_id='widgets'",
    );
    let readback = Catalog::from_json(doc.trim()).expect("stored document parses as a Catalog");
    assert_eq!(readback.catalog_id, "widgets");
    assert_eq!(readback.version, 3);

    // Teardown (leave nothing behind).
    run_psql(
        &url,
        "DROP SCHEMA IF EXISTS catalog CASCADE; DROP SCHEMA IF EXISTS wamn_migrate_data CASCADE;",
    );
}

fn run_psql(url: &str, script: &str) {
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
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
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
