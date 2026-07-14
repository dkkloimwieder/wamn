//! Storage-schema tests for the T1 control-plane registry (wamn-q3n.3).
//!
//! Three layers, all pure/portable except the last:
//! - a **drift guard** tying `deploy/system-schema.sql` to the `wamn-registry`
//!   model (table/column shape, the tier/env CHECK literals, `SCHEMA_VERSION`,
//!   the dev≠prod CHECK expression) — the `wamn-schema` /
//!   `state_literals_match_catalog_schema_sql` pattern;
//! - the **request-path-free** invariant (1): a static grep asserting no
//!   data-plane manifest references the T1 cluster / system DB;
//! - a **live-apply gate** (invariants 2/3/4 + FK integrity + the saga
//!   exactly-once/resume checkpoint), gated on `WAMN_REGISTRY_PG_URL` (a
//!   superuser URL — the harness provisions the `wamn_system` owner role) and
//!   skipped cleanly when unset (mirrors wamn-ddl / wamn-run-store).

use std::path::Path;

use wamn_registry::{Env, SCHEMA_VERSION, Tier};

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy")
}

fn system_schema_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("system-schema.sql"))
        .expect("read deploy/system-schema.sql")
}

/// The SQL with `--` line comments stripped, so text assertions test the actual
/// DDL and not the explanatory prose (the header deliberately *names* the tenant
/// RLS floor and credential columns to say it carries none). No `--` appears
/// inside a string literal in this file, so a per-line truncate is exact.
fn code_only(sql: &str) -> String {
    sql.lines()
        .map(|l| l.find("--").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n")
}

// --- drift guard: DDL ↔ model ----------------------------------------------

/// `deploy/system-schema.sql` must mirror the `wamn-registry` model: the two
/// control-plane schemas, the three registry tables and their distinctive
/// columns, the tier/env CHECK literals (from the model's `as_str()`), the
/// storage-format `SCHEMA_VERSION`, and the saga table.
#[test]
fn system_schema_sql_mirrors_the_model() {
    let sql = code_only(&system_schema_sql());

    // Platform-global, NOT tenant-scoped: none of the tenant-DB RLS floor.
    assert!(
        !sql.contains("app.tenant") && !sql.contains("ROW LEVEL SECURITY"),
        "the system DB is platform-global — it must carry no tenant RLS floor"
    );

    // Schemas.
    assert!(sql.contains("CREATE SCHEMA registry"));
    assert!(sql.contains("CREATE SCHEMA provisioning"));

    // Registry tables + the distinctive columns of the model
    // (Org.prod_cluster/dev_cluster, ProjectEnv.secret_name/secret_namespace).
    assert!(sql.contains("CREATE TABLE registry.orgs"));
    assert!(sql.contains("prod_cluster") && sql.contains("dev_cluster"));
    assert!(sql.contains("CREATE TABLE registry.projects"));
    assert!(sql.contains("CREATE TABLE registry.project_envs"));
    assert!(sql.contains("secret_name") && sql.contains("secret_namespace"));

    // Tier + Env CHECK literals come from the model (drift-guarded like State).
    for t in Tier::ALL {
        assert!(
            sql.contains(&format!("'{}'", t.as_str())),
            "system-schema.sql is missing tier literal {:?}",
            t.as_str()
        );
    }
    for e in Env::ALL {
        assert!(
            sql.contains(&format!("'{}'", e.as_str())),
            "system-schema.sql is missing env literal {:?}",
            e.as_str()
        );
    }

    // The storage-format version is recorded (singleton meta row).
    assert!(sql.contains(&format!("'{SCHEMA_VERSION}'")));

    // The saga table + its kind literals.
    assert!(sql.contains("CREATE TABLE provisioning.sagas"));
    assert!(sql.contains("'provision-org'") && sql.contains("'provision-project-env'"));
}

/// Invariant 4 (dev ≠ prod recovery domain) is a CHECK whose *expression* is
/// pinned, not just its name — the drift-guard lesson: a name-only assertion
/// lets a weakened predicate slip through.
#[test]
fn dev_ne_prod_recovery_domain_check_is_present() {
    let sql = code_only(&system_schema_sql());
    assert!(
        sql.contains("tier = 'trials' OR prod_cluster <> dev_cluster"),
        "the dev≠prod recovery-domain CHECK expression must be present verbatim"
    );
}

/// Invariant 2 (no credentials, R8b): the schema stores Secret *references* and
/// must not introduce a credential column (a text-level backstop; the live-apply
/// gate asserts the actual column set).
#[test]
fn schema_holds_no_credential_column() {
    let sql = code_only(&system_schema_sql()).to_lowercase();
    for bad in [
        "password",
        "secret_value",
        "credential",
        " dsn ",
        "connection_string",
    ] {
        assert!(
            !sql.contains(bad),
            "the system DB must hold NO credential material (found {bad:?}) — references only (R8b)"
        );
    }
}

// --- invariant 1: request-path-free ----------------------------------------

/// Invariant 1 (system cluster absent from ALL request paths): a static grep of
/// the deploy manifests. Only the T1 cluster definition itself
/// (`wamn-sysdb.yaml`) may reference the system cluster / DB; NO data-plane
/// workload (gateway / runner / dispatcher / webhook) may. When control-plane
/// provisioning tooling that legitimately connects to `wamn_system` lands
/// (`.6`/`.7`), add its manifest to the allowlist — a conscious edit
/// (drift-guard-over-ban).
#[test]
fn no_data_plane_manifest_references_the_system_cluster() {
    // The only manifests permitted to name the T1 cluster / system DB.
    const ALLOWLIST: &[&str] = &["wamn-sysdb.yaml"];

    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(deploy_dir()).expect("read deploy/") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        if ALLOWLIST.contains(&name.as_str()) {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read manifest");
        if body.contains("wamn-sysdb") || body.contains("wamn_system") {
            offenders.push(name);
        }
    }
    assert!(
        offenders.is_empty(),
        "these deploy manifests reference the T1 system cluster/DB (request-path-free \
         invariant 1) — add to the allowlist only if they are control-plane tooling: {offenders:?}"
    );
}

// --- live-apply gate: invariants 2/3/4 + FK + saga --------------------------

/// Apply `deploy/system-schema.sql` to a throwaway Postgres and assert the live,
/// DB-enforced invariants. Set `WAMN_REGISTRY_PG_URL` to a superuser URL (the
/// harness provisions the `wamn_system` owner role); skipped when unset.
///
/// The DDL + assertions run as `wamn_system` (`SET ROLE`), the way production
/// applies it (the owner owns the DB): this proves the registry is owned by — and
/// usable by — the control-plane owner role (what `.6` provision-org needs), not
/// just applyable by a superuser.
#[test]
fn system_schema_applies_and_enforces_invariants_on_postgres() {
    let Ok(url) = std::env::var("WAMN_REGISTRY_PG_URL") else {
        eprintln!(
            "skipping system_schema_applies_and_enforces_invariants_on_postgres \
             (set WAMN_REGISTRY_PG_URL to run)"
        );
        return;
    };

    let ddl = system_schema_sql();
    let mut script = String::new();
    // Provision the wamn_system owner role (the T1 cluster bootstraps it), a fresh
    // pair of schemas, and grant it CREATE so it can own the schema (in-cluster it
    // owns the DB); then apply + assert AS wamn_system.
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
         CREATE ROLE wamn_system LOGIN PASSWORD 'wamn_system' NOSUPERUSER; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS registry CASCADE;\n\
         DROP SCHEMA IF EXISTS provisioning CASCADE;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(&ddl);
    script.push('\n');
    script.push_str(ASSERTIONS);
    script.push_str("RESET ROLE;\n");

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

/// The live assertions (kept out of the Rust string plumbing for readability).
const ASSERTIONS: &str = r#"
-- FK integrity: an org + its project + two provisioned envs (references only).
INSERT INTO registry.orgs (id, tier, prod_cluster, dev_cluster)
  VALUES ('acme','standard','acme-prod','acme-dev'),
         ('try','trials','wamn-pg','wamn-pg');
INSERT INTO registry.projects (org, id) VALUES ('acme','billing'),('try','demo');
INSERT INTO registry.project_envs (org, project, env, secret_name)
  VALUES ('acme','billing','prod','wamn-db-acme-prod'),
         ('acme','billing','dev','wamn-db-acme-dev');

-- A project under an unregistered org is rejected (FK).
DO $$ BEGIN BEGIN
  INSERT INTO registry.projects (org, id) VALUES ('ghost','x');
  ASSERT false, 'a project under an unknown org must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- A project-env under an unregistered project is rejected (FK).
DO $$ BEGIN BEGIN
  INSERT INTO registry.project_envs (org, project, env, secret_name)
    VALUES ('acme','ghost','prod','s');
  ASSERT false, 'a project-env under an unknown project must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- Invariant 4: a paying org must place prod and dev on DIFFERENT clusters.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, tier, prod_cluster, dev_cluster)
    VALUES ('badstd','standard','same','same');
  ASSERT false, 'a standard org with prod=dev must violate the recovery-domain CHECK';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, tier, prod_cluster, dev_cluster)
    VALUES ('badded','dedicated','c','c');
  ASSERT false, 'a dedicated org with prod=dev must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- ...but the T3 trials pool deliberately collapses both onto the shared cluster.
DO $$ BEGIN ASSERT (SELECT count(*) FROM registry.orgs WHERE id='try')=1,
  'a trials org collapses both sides onto the shared pool'; END $$;

-- The tier / env CHECKs reject unknown values.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, tier, prod_cluster, dev_cluster)
    VALUES ('bt','platinum','p','d');
  ASSERT false, 'an unknown tier must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.project_envs (org, project, env, secret_name)
    VALUES ('acme','billing','staging','s');
  ASSERT false, 'an unknown env must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;

-- Invariant 2 (no credentials, R8b): project_envs carries the Secret REFERENCE
-- and NO credential column.
DO $$ DECLARE bad int; BEGIN
  SELECT count(*) INTO bad FROM information_schema.columns
    WHERE table_schema='registry' AND table_name='project_envs'
      AND column_name IN ('password','secret','secret_value','url','dsn',
                          'credential','credentials','connection_string');
  ASSERT bad=0, 'project_envs must hold NO credential column (R8b) — references only';
  ASSERT (SELECT count(*) FROM information_schema.columns
    WHERE table_schema='registry' AND table_name='project_envs'
      AND column_name IN ('secret_name','secret_namespace'))=2,
    'project_envs must carry the Secret reference (name + optional namespace)';
END $$;

-- Invariant 3 (no tenant data): the ONLY tables in the system DB are the
-- control-plane set.
DO $$ DECLARE tbls text; BEGIN
  SELECT string_agg(table_schema||'.'||table_name, ',' ORDER BY table_schema, table_name)
    INTO tbls FROM information_schema.tables
    WHERE table_schema IN ('registry','provisioning') AND table_type='BASE TABLE';
  ASSERT tbls = 'provisioning.sagas,registry.meta,registry.orgs,registry.project_envs,registry.projects',
    format('unexpected control-plane table set (invariant 3): %s', tbls);
END $$;

-- Saga: creation is exactly-once via the saga_id PK; step is a durable resume
-- checkpoint; the kind/status CHECKs hold.
INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s1','provision-org','acme')
  ON CONFLICT (saga_id) DO NOTHING;
INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s1','provision-org','acme')
  ON CONFLICT (saga_id) DO NOTHING;
DO $$ BEGIN ASSERT (SELECT count(*) FROM provisioning.sagas WHERE saga_id='s1')=1,
  'saga creation is exactly-once via the saga_id PK'; END $$;
UPDATE provisioning.sagas SET step=step+1, status='running' WHERE saga_id='s1';
UPDATE provisioning.sagas SET step=step+1 WHERE saga_id='s1';
DO $$ BEGIN ASSERT (SELECT step FROM provisioning.sagas WHERE saga_id='s1')=2,
  'saga step is a durable resume checkpoint'; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s2','provision-everything','x');
  ASSERT false, 'an unknown saga kind must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  UPDATE provisioning.sagas SET status='bogus' WHERE saga_id='s1';
  ASSERT false, 'an unknown saga status must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;

-- Deleting an org cascades its projects and project-envs.
DELETE FROM registry.orgs WHERE id='acme';
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM registry.projects WHERE org='acme')=0, 'projects cascade';
  ASSERT (SELECT count(*) FROM registry.project_envs WHERE org='acme')=0, 'project-envs cascade';
END $$;

DROP SCHEMA registry CASCADE;
DROP SCHEMA provisioning CASCADE;
"#;
