//! Storage-schema tests for the T1 control-plane registry (wamn-q3n.3;
//! generalized in wamn-8df.3; org-scoped policies + templates in wamn-8df.4).
//!
//! Three layers, all pure/portable except the last:
//! - a **drift guard** tying `deploy/sql/system-schema.sql` to the `wamn-registry`
//!   model (table/column shape, the D18 placement CHECKs, the org-scoped
//!   `env_policies` keying with NO platform-global seed, `SCHEMA_VERSION`) —
//!   the `wamn-schema` / `state_literals_match_catalog_schema_sql` pattern;
//! - the **request-path-free** invariant (1): a static grep asserting no
//!   data-plane manifest references the T1 cluster / system DB;
//! - a **live-apply gate** (invariants 2/3 + placement/env FK integrity + the
//!   template stamp insert-if-absent semantics + the saga exactly-once/resume
//!   checkpoint), gated on `WAMN_REGISTRY_PG_URL` (a superuser URL — the harness
//!   provisions the `wamn_system` owner role) and skipped cleanly when unset
//!   (mirrors wamn-ddl / wamn-run-store).

use std::path::Path;

use wamn_registry::{SCHEMA_VERSION, Template};

fn deploy_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy")
}

fn system_schema_sql() -> String {
    std::fs::read_to_string(deploy_dir().join("sql/system-schema.sql"))
        .expect("read deploy/sql/system-schema.sql")
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

/// `deploy/sql/system-schema.sql` must mirror the `wamn-registry` model: the two
/// control-plane schemas, the registry tables and their distinctive columns (the
/// D18 placement shape + the 8df.4 org-scoped `env_policies` keying, with NO
/// platform-global policy seed), the storage-format `SCHEMA_VERSION`, and the
/// saga table.
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

    // Orgs carry the D18 placement (placement_kind + pool_cluster); the retired
    // tier / *_cluster columns are gone.
    assert!(sql.contains("CREATE TABLE registry.orgs"));
    assert!(sql.contains("placement_kind") && sql.contains("pool_cluster"));
    assert!(
        !sql.contains("prod_cluster") && !sql.contains("canary_cluster"),
        "the retired tier/canary cluster columns must be gone"
    );

    // Env policies: ORG-SCOPED rows (8df.4) — keyed (org, name), cascading with
    // their org, with the policy-value columns.
    assert!(sql.contains("CREATE TABLE registry.env_policies"));
    for col in [
        "recovery_domain",
        "promotion_rank",
        "instances",
        "backup_cadence",
        "wal_retention",
        "hibernation",
    ] {
        assert!(sql.contains(col), "env_policies missing column {col}");
    }
    let policies_block = sql
        .split("CREATE TABLE registry.env_policies")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("env_policies table body");
    assert!(
        policies_block.contains("PRIMARY KEY (org, name)"),
        "env_policies must be keyed per org (8df.4)"
    );
    // The org CASCADE is added AFTER project_envs (ALTER TABLE — the RI-trigger
    // ordering that lets a single-statement org DELETE cascade cleanly).
    let alter_pos = sql
        .find("ALTER TABLE registry.env_policies")
        .expect("env_policies org FK is added via ALTER TABLE");
    assert!(
        sql[alter_pos..].contains("REFERENCES registry.orgs (id) ON DELETE CASCADE"),
        "an org's policies must cascade with the org"
    );
    assert!(
        alter_pos > sql.find("CREATE TABLE registry.project_envs").unwrap(),
        "the env_policies org FK must be created AFTER project_envs (cascade ordering)"
    );
    // NO platform-global seed: policies are stamped per org from a Template.
    assert!(
        !sql.contains("INSERT INTO registry.env_policies"),
        "env_policies must not carry a platform-global seed — templates stamp per-org rows"
    );

    // Projects / project-envs, the latter FK'd to the ORG's env_policies (env
    // resolves a policy in its org's set — the D18+8df.4 referential-integrity
    // replacement for the env CHECK).
    assert!(sql.contains("CREATE TABLE registry.projects"));
    assert!(sql.contains("CREATE TABLE registry.project_envs"));
    assert!(sql.contains("secret_name") && sql.contains("secret_namespace"));
    assert!(
        sql.contains("FOREIGN KEY (org, env) REFERENCES registry.env_policies (org, name)"),
        "project_envs must FK (org, env) to the org's env_policies (8df.4)"
    );
    assert!(
        !sql.contains("REFERENCES registry.env_policies (name)"),
        "the single-column (platform-global) env FK must be retired"
    );
    assert!(
        !sql.contains("env IN ('dev', 'canary', 'prod')"),
        "the closed env CHECK must be retired"
    );

    // The storage-format version is recorded (singleton meta row).
    assert!(sql.contains(&format!("'{SCHEMA_VERSION}'")));

    // The saga table + its kind literals ('copy' = the wamn-8df.5 pipeline).
    assert!(sql.contains("CREATE TABLE provisioning.sagas"));
    assert!(sql.contains("'provision-org'") && sql.contains("'provision-project-env'"));
    assert!(
        sql.contains("'provision-project-env', 'copy'"),
        "the sagas kind CHECK must admit the copy pipeline (wamn-8df.5)"
    );
}

/// The org-row builder (`wamn_registry::sql::upsert_org_sql`) must target exactly
/// the `registry.orgs` placement columns the storage DDL declares — a drift guard
/// tying the builder to `deploy/sql/system-schema.sql` (SR2: registry SQL lives with
/// the model, pinned to the schema it writes).
#[test]
fn upsert_org_sql_matches_the_placement_columns() {
    let sql = code_only(&system_schema_sql());
    let builder = wamn_registry::sql::upsert_org_sql();
    assert!(builder.contains("registry.orgs"));
    assert!(builder.contains("ON CONFLICT (id)"));
    for col in ["id", "placement_kind", "pool_cluster"] {
        assert!(
            sql.contains(col),
            "orgs table (system-schema.sql) missing {col}"
        );
        assert!(builder.contains(col), "upsert builder missing {col}");
    }
}

/// The project / project-env builders (`upsert_project_sql`,
/// `upsert_project_env_sql`, `select_org_placement_sql`) must target exactly the
/// `registry.projects` / `registry.project_envs` / `registry.orgs` columns the
/// storage DDL declares.
#[test]
fn upsert_project_and_project_env_sql_match_the_columns() {
    let sql = code_only(&system_schema_sql());

    let projects = wamn_registry::sql::upsert_project_sql();
    assert!(projects.contains("registry.projects"));
    assert!(projects.contains("ON CONFLICT (org, id) DO NOTHING"));

    let envs = wamn_registry::sql::upsert_project_env_sql();
    assert!(envs.contains("registry.project_envs"));
    assert!(envs.contains("ON CONFLICT (org, project, env) DO UPDATE"));
    assert!(sql.contains("CREATE TABLE registry.project_envs"));
    for col in ["org", "project", "env", "secret_name", "secret_namespace"] {
        assert!(sql.contains(col), "project_envs table missing {col}");
        assert!(
            envs.contains(col),
            "project_env upsert builder missing {col}"
        );
    }

    // The placement read targets the orgs placement columns (so provision-project-env
    // can derive the cluster per-env via cluster_of).
    let sel = wamn_registry::sql::select_org_placement_sql();
    assert!(sel.contains("registry.orgs"));
    assert!(sel.contains("placement_kind") && sel.contains("pool_cluster"));

    // The env-policy reads target env_policies keyed by ORG (8df.4: provision-org
    // sizes clusters from the org's own set; provision-project-env reads one of
    // the org's rows to derive the owner).
    for reader in [
        wamn_registry::sql::select_env_policies_sql(),
        wamn_registry::sql::select_env_policy_sql(),
    ] {
        assert!(reader.contains("FROM registry.env_policies"));
        assert!(reader.contains("recovery_domain::text"));
        assert!(reader.contains("WHERE org = $1"), "reads must be org-keyed");
    }
    assert!(wamn_registry::sql::select_env_policies_sql().contains("ORDER BY promotion_rank"));

    // The template stamp targets every env_policies column the DDL declares.
    let stamp = wamn_registry::sql::stamp_env_policy_sql();
    assert!(stamp.contains("registry.env_policies"));
    assert!(stamp.contains("ON CONFLICT (org, name) DO NOTHING"));
    for col in [
        "org",
        "name",
        "recovery_domain",
        "promotion_rank",
        "instances",
        "storage",
        "cpu",
        "memory",
        "image",
        "backup_cadence",
        "wal_retention",
        "hibernation",
    ] {
        assert!(sql.contains(col), "env_policies table missing {col}");
        assert!(stamp.contains(col), "stamp builder missing {col}");
    }
}

/// The saga builders must target the `provisioning.sagas` columns and use only
/// status literals the storage CHECK allows (the SR2 drift guard, unchanged by D18).
#[test]
fn saga_sql_builders_match_the_sagas_columns_and_status_check() {
    let sql = code_only(&system_schema_sql());
    assert!(sql.contains("CREATE TABLE provisioning.sagas"));

    let create = wamn_registry::sql::create_saga_sql();
    assert!(create.contains("provisioning.sagas"));
    assert!(create.contains("ON CONFLICT (saga_id) DO NOTHING"));
    for col in ["saga_id", "kind", "target", "total_steps"] {
        assert!(sql.contains(col), "sagas table missing {col}");
        assert!(create.contains(col), "create_saga builder missing {col}");
    }

    let advance = wamn_registry::sql::advance_saga_step_sql();
    assert!(advance.contains("provisioning.sagas") && advance.contains("step = step + 1"));

    for (builder, status) in [
        (advance, "running"),
        (wamn_registry::sql::complete_saga_sql(), "completed"),
        (wamn_registry::sql::fail_saga_sql(), "failed"),
    ] {
        assert!(
            builder.contains(&format!("'{status}'")),
            "builder is missing the {status:?} status literal"
        );
        assert!(
            sql.contains(&format!("'{status}'")),
            "sagas status CHECK (system-schema.sql) is missing {status:?}"
        );
    }
}

/// The dump-record builder + the dump-catalog reads must target the
/// `provisioning.dumps` columns the storage DDL declares (unchanged by D18).
#[test]
fn dumps_table_and_record_builder_match_the_columns() {
    let sql = code_only(&system_schema_sql());

    assert!(sql.contains("CREATE TABLE provisioning.dumps"));
    assert!(sql.contains("REFERENCES registry.project_envs (org, project, env)"));
    assert!(sql.contains("dumps_format_check") && sql.contains("format IN ('directory')"));

    let builder = wamn_registry::sql::record_dump_sql();
    assert!(builder.contains("provisioning.dumps"));
    assert!(builder.contains("ON CONFLICT (org, project, env, object_key) DO UPDATE"));
    for col in ["org", "project", "env", "object_key", "format", "byte_size"] {
        assert!(sql.contains(col), "dumps table missing {col}");
        assert!(builder.contains(col), "record_dump builder missing {col}");
    }

    for reader in [
        wamn_registry::sql::select_latest_dump_sql(),
        wamn_registry::sql::select_dumps_sql(),
    ] {
        assert!(reader.contains("FROM provisioning.dumps"));
        assert!(reader.contains("object_key"));
        assert!(reader.contains("ORDER BY taken_at DESC, object_key DESC"));
    }
}

/// The CDC reader-registration builders (`upsert_event_reader_sql` /
/// `select_event_reader_sql`) must target the `registry.event_readers` columns
/// the storage DDL declares (wamn-l5i9.9, D19 v3) — and the table must carry the
/// Secret REFERENCE shape (invariant 2), the triple key, and the project-env
/// cascade FK (the provisioning.dumps precedent).
#[test]
fn event_readers_table_and_builders_match_the_columns() {
    let sql = code_only(&system_schema_sql());

    assert!(sql.contains("CREATE TABLE registry.event_readers"));
    let block = sql
        .split("CREATE TABLE registry.event_readers")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("event_readers table body");
    assert!(
        block.contains("PRIMARY KEY (org, project, env)"),
        "event_readers is keyed by the identity triple"
    );
    assert!(
        block.contains("REFERENCES registry.project_envs (org, project, env) ON DELETE CASCADE"),
        "a de-provisioned project-env must drop its CDC registration"
    );

    let upsert = wamn_registry::sql::upsert_event_reader_sql();
    let select = wamn_registry::sql::select_event_reader_sql();
    for col in [
        "publication",
        "slot",
        "stream",
        "replication_secret_name",
        "replication_secret_namespace",
        "enabled",
    ] {
        assert!(block.contains(col), "event_readers table missing {col}");
        assert!(upsert.contains(col), "upsert builder missing {col}");
        assert!(select.contains(col), "select builder missing {col}");
    }
    // Invariant 2: a REFERENCE, never material — no url/password column.
    for bad in ["url", "password", "dsn"] {
        assert!(
            !block.contains(bad),
            "event_readers must hold NO credential column (found {bad:?})"
        );
    }
}

/// The D18 placement structural CHECK is pinned by *expression*, not just its
/// name (the drift-guard lesson: a name-only assertion lets a weakened predicate
/// slip through). The retired tier/canary CHECKs must be gone.
#[test]
fn placement_check_is_present_and_tier_checks_are_gone() {
    let sql = code_only(&system_schema_sql());
    assert!(
        sql.contains("(placement_kind = 'pooled') = (pool_cluster IS NOT NULL)"),
        "the pooled ⟺ pool_cluster CHECK expression must be present verbatim"
    );
    assert!(sql.contains("placement_kind IN ('pooled', 'dedicated')"));
    // The old tier/recovery-domain/canary CHECKs are retired (D18).
    for gone in [
        "orgs_tier_check",
        "orgs_recovery_domain_check",
        "orgs_canary_dedicated_check",
        "prod_cluster <> dev_cluster",
    ] {
        assert!(
            !sql.contains(gone),
            "retired constraint still present: {gone}"
        );
    }
}

/// The cjv.20 charset/length backstop: each stored slug/name column carries a
/// named CHECK mirroring `wamn-registry` validate() (`check_id` / `check_env` /
/// `check_name`). Pinned by constraint name + the anchored slug regex + the
/// length bound + the reserved-`wamn` clause, so a weakened predicate can't slip
/// through (the drift-guard-expression lesson). Defends a direct control-plane
/// writer (wamn-2ib) that skips both provision-org and Registry::validate().
#[test]
fn charset_length_checks_backstop_the_stored_slug_names() {
    let sql = code_only(&system_schema_sql());

    // The anchored slug regex mirrors wamn-registry `is_slug`, once per stored
    // slug/name column: orgs.id, orgs.pool_cluster, projects.id, env_policies.name.
    let slug_re = "'^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'";
    assert!(
        sql.matches(slug_re).count() >= 4,
        "each of the 4 stored slug/name columns must carry the anchored slug regex"
    );

    // Every column carries a named CHECK constraint.
    for name in [
        "orgs_id_charset_check",
        "orgs_pool_cluster_charset_check",
        "projects_id_charset_check",
        "env_policies_name_charset_check",
    ] {
        assert!(
            sql.contains(name),
            "missing charset CHECK constraint {name}"
        );
    }

    // Length bounds: ids/env slugs ≤ 40 (MAX_ID_LEN), cluster names ≤ 63
    // (MAX_NAME_LEN, DNS-1123 label).
    assert!(sql.contains("char_length(id) <= 40"), "id length bound");
    assert!(
        sql.contains("char_length(name) <= 40"),
        "env name length bound"
    );
    assert!(
        sql.contains("char_length(pool_cluster) <= 63"),
        "cluster name length bound"
    );

    // The id columns (orgs.id, projects.id) mirror the reserved-`wamn` rule
    // (check_id); the pool_cluster / env name columns deliberately do NOT (they
    // may carry the `wamn` prefix / are arbitrary env slugs).
    assert!(
        sql.contains("id <> 'wamn'") && sql.contains("id NOT LIKE 'wamn-%'"),
        "the id charset CHECK must reject the reserved `wamn` prefix"
    );
    assert!(
        !sql.contains("pool_cluster NOT LIKE") && !sql.contains("name NOT LIKE"),
        "pool_cluster / env name must NOT carry a reserved-prefix rule"
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
/// workload (gateway / runner / dispatcher / webhook) may.
#[test]
fn no_data_plane_manifest_references_the_system_cluster() {
    const ALLOWLIST: &[&str] = &["wamn-sysdb.yaml"];

    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    let mut stack = vec![deploy_dir()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read deploy/") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            scanned += 1;
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if ALLOWLIST.contains(&name.as_str()) {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("read manifest");
            if body.contains("wamn-sysdb") || body.contains("wamn_system") {
                offenders.push(name);
            }
        }
    }
    // The tiered deploy/ layout ships ~50 manifests; a low count means the walk
    // went vacuous (the pre-tiering flat read_dir bug class).
    assert!(scanned >= 10, "deploy/ manifest walk saw only {scanned} yaml files");
    assert!(
        offenders.is_empty(),
        "these deploy manifests reference the T1 system cluster/DB (request-path-free \
         invariant 1) — add to the allowlist only if they are control-plane tooling: {offenders:?}"
    );
}

// --- live-apply gate: invariants 2/3 + placement/env FK + seed + saga --------

/// Apply `deploy/sql/system-schema.sql` to a throwaway Postgres and assert the live,
/// DB-enforced invariants. Set `WAMN_REGISTRY_PG_URL` to a superuser URL (the
/// harness provisions the `wamn_system` owner role); skipped when unset.
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
    // Exercise the REAL org-row builder via PREPARE/EXECUTE: two upserts of the
    // same id must collapse to ONE row (the second refreshing the placement),
    // proving `ON CONFLICT (id) DO UPDATE`.
    script.push_str(&format!(
        "PREPARE up (text,text,text) AS {upsert};\n\
         EXECUTE up('demo','pooled','wamn-pg');\n\
         EXECUTE up('demo','dedicated',NULL);\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.orgs WHERE id='demo')=1,\n\
             'upsert_org_sql is idempotent — one row after two upserts';\n\
           ASSERT (SELECT placement_kind FROM registry.orgs WHERE id='demo')='dedicated',\n\
             'the second upsert refreshed the placement (ON CONFLICT DO UPDATE)';\n\
           ASSERT (SELECT pool_cluster FROM registry.orgs WHERE id='demo') IS NULL,\n\
             'the second (dedicated) upsert cleared the pool cluster';\n\
         END $$;\n\
         DEALLOCATE up;\n",
        upsert = wamn_registry::sql::upsert_org_sql(),
    ));
    // Exercise the REAL template stamp (8df.4) against the 'demo' org: stamp the
    // trials policy set with the model's values, customize one row, re-stamp, and
    // assert the customization SURVIVES (insert-if-absent — a DO UPDATE mutant
    // would clobber it back to template values and fail here).
    script.push_str(&format!(
        "PREPARE stamp (text,text,text,int,int,text,text,text,text,text,text,text) AS {stamp};\n",
        stamp = wamn_registry::sql::stamp_env_policy_sql(),
    ));
    script.push_str(&stamp_statements("demo", &Template::trials()));
    script.push_str(
        "UPDATE registry.env_policies SET storage='42Gi' WHERE org='demo' AND name='dev';\n",
    );
    script.push_str(&stamp_statements("demo", &Template::trials()));
    script.push_str(
        "DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.env_policies WHERE org='demo')=2,\n\
             'the trials template stamps dev + prod for the org (re-stamp adds nothing)';\n\
           ASSERT (SELECT storage FROM registry.env_policies WHERE org='demo' AND name='dev')='42Gi',\n\
             'a customized policy row SURVIVES a re-stamp (insert-if-absent, never clobbered)';\n\
           ASSERT (SELECT instances FROM registry.env_policies WHERE org='demo' AND name='prod')=3,\n\
             'the stamped prod policy carries the template values';\n\
         END $$;\n\
         DEALLOCATE stamp;\n",
    );
    // Exercise the REAL project / project-env builders against the 'demo' org just
    // stamped. env 'dev' resolves demo's own policy row (the composite FK holds).
    script.push_str(&format!(
        "PREPARE upp (text,text) AS {up_project};\n\
         PREPARE upe (text,text,text,text,text) AS {up_env};\n\
         EXECUTE upp('demo','app');\n\
         EXECUTE upp('demo','app');\n\
         EXECUTE upe('demo','app','dev','wamn-db-demo--app--dev-OLD', NULL);\n\
         EXECUTE upe('demo','app','dev','wamn-db-demo--app--dev', NULL);\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.projects WHERE org='demo' AND id='app')=1,\n\
             'upsert_project_sql is idempotent — one project row after two upserts';\n\
           ASSERT (SELECT secret_name FROM registry.project_envs\n\
                     WHERE org='demo' AND project='app' AND env='dev')='wamn-db-demo--app--dev',\n\
             'the second project-env upsert refreshed the Secret reference (ON CONFLICT DO UPDATE)';\n\
         END $$;\n\
         DEALLOCATE upp; DEALLOCATE upe;\n",
        up_project = wamn_registry::sql::upsert_project_sql(),
        up_env = wamn_registry::sql::upsert_project_env_sql(),
    ));
    // Exercise the REAL env-policy read via `CREATE TABLE AS EXECUTE` — provision-
    // project-env reads one of the ORG's policies to derive the cluster owner; a
    // different org's key returns nothing (org-scoped, never cross-org).
    script.push_str(&format!(
        "PREPARE getpol (text,text) AS {get};\n\
         CREATE TEMP TABLE policy_probe AS EXECUTE getpol('demo','dev');\n\
         CREATE TEMP TABLE policy_probe_other AS EXECUTE getpol('ghost','dev');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM policy_probe)=1,\n\
             'select_env_policy_sql returns exactly one row for the org';\n\
           ASSERT (SELECT name FROM policy_probe)='dev',\n\
             'select_env_policy_sql returns the named policy';\n\
           ASSERT (SELECT count(*) FROM policy_probe_other)=0,\n\
             'select_env_policy_sql never returns another org''s policy (org-keyed)';\n\
         END $$;\n\
         DROP TABLE policy_probe; DROP TABLE policy_probe_other; DEALLOCATE getpol;\n",
        get = wamn_registry::sql::select_env_policy_sql(),
    ));
    // Exercise the REAL dump-record builder (unchanged by D18).
    script.push_str(&format!(
        "PREPARE rdump (text,text,text,text,text,bigint) AS {record};\n\
         EXECUTE rdump('demo','app','dev','dumps/demo/app/dev/1000','directory', 100);\n\
         EXECUTE rdump('demo','app','dev','dumps/demo/app/dev/1000','directory', 200);\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT byte_size FROM provisioning.dumps\n\
                     WHERE object_key='dumps/demo/app/dev/1000')=200,\n\
             'the second record refreshed byte_size (ON CONFLICT DO UPDATE)';\n\
         END $$;\n\
         DEALLOCATE rdump;\n",
        record = wamn_registry::sql::record_dump_sql(),
    ));
    // Exercise the REAL saga builders (unchanged by D18): creation exactly-once,
    // step a durable checkpoint, complete/fail terminal.
    script.push_str(&format!(
        "PREPARE csaga (text,text,text,int) AS {create};\n\
         PREPARE asaga (text) AS {advance};\n\
         PREPARE dsaga (text) AS {complete};\n\
         PREPARE fsaga (text,text) AS {fail};\n\
         EXECUTE csaga('sg1','provision-org','demo', 3);\n\
         EXECUTE csaga('sg1','provision-org','demo', 3);\n\
         EXECUTE asaga('sg1');\n\
         EXECUTE asaga('sg1');\n\
         EXECUTE dsaga('sg1');\n\
         EXECUTE csaga('sg2','provision-project-env','demo/app/dev', NULL);\n\
         EXECUTE fsaga('sg2','boom');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM provisioning.sagas WHERE saga_id='sg1')=1,\n\
             'create_saga_sql is exactly-once via the saga_id PK';\n\
           ASSERT (SELECT step FROM provisioning.sagas WHERE saga_id='sg1')=2,\n\
             'advance_saga_step_sql advances the durable checkpoint (two advances → 2)';\n\
           ASSERT (SELECT status FROM provisioning.sagas WHERE saga_id='sg1')='completed',\n\
             'complete_saga_sql sets the terminal completed status';\n\
           ASSERT (SELECT status FROM provisioning.sagas WHERE saga_id='sg2')='failed'\n\
              AND (SELECT last_error FROM provisioning.sagas WHERE saga_id='sg2')='boom',\n\
             'fail_saga_sql records the terminal failed status + the error';\n\
         END $$;\n\
         DEALLOCATE csaga; DEALLOCATE asaga; DEALLOCATE dsaga; DEALLOCATE fsaga;\n",
        create = wamn_registry::sql::create_saga_sql(),
        advance = wamn_registry::sql::advance_saga_step_sql(),
        complete = wamn_registry::sql::complete_saga_sql(),
        fail = wamn_registry::sql::fail_saga_sql(),
    ));
    // The copy pipeline's saga kind (wamn-8df.5) is admitted by the kind CHECK,
    // and select_saga_sql reads back the durable state the cutover gate checks.
    script.push_str(&format!(
        "PREPARE csaga2 (text,text,text,int) AS {create};\n\
         PREPARE asaga2 (text) AS {advance};\n\
         PREPARE ssaga (text) AS {select};\n\
         EXECUTE csaga2('sg3','copy','acme/app/dev -> acme/app/prod', 5);\n\
         EXECUTE asaga2('sg3');\n\
         CREATE TEMP TABLE saga_probe AS EXECUTE ssaga('sg3');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM provisioning.sagas WHERE saga_id='sg3' AND kind='copy')=1,\n\
             'the sagas kind CHECK admits the copy pipeline (wamn-8df.5)';\n\
           ASSERT (SELECT status FROM saga_probe)='running'\n\
              AND (SELECT step FROM saga_probe)=1\n\
              AND (SELECT total_steps FROM saga_probe)=5,\n\
             'select_saga_sql reads the durable state the cutover gate checks';\n\
         END $$;\n\
         DROP TABLE saga_probe;\n\
         DEALLOCATE csaga2; DEALLOCATE asaga2; DEALLOCATE ssaga;\n",
        create = wamn_registry::sql::create_saga_sql(),
        advance = wamn_registry::sql::advance_saga_step_sql(),
        select = wamn_registry::sql::select_saga_sql(),
    ));
    // Exercise the REAL CDC reader-registration builders (wamn-l5i9.9) against
    // the demo/app/dev project-env provisioned above: upsert twice (the second
    // refreshes slot/enabled — ON CONFLICT DO UPDATE), read it back via the real
    // select, reject a registration for an UNPROVISIONED env (the project-env
    // FK — enable-cdc is an overlay on an already-provisioned env), and prove
    // the whole-org cascade drops the registration.
    script.push_str(&format!(
        "PREPARE uper (text,text,text,text,text,text,text,text,boolean) AS {upsert};\n\
         PREPARE geter (text,text,text) AS {select};\n\
         EXECUTE uper('demo','app','dev','wamn_cdc_demo__app__dev','wamn_cdc_demo__app__dev',\
                      'EVT_demo_dev','wamn-cdc-demo--app--dev',NULL,true);\n\
         EXECUTE uper('demo','app','dev','wamn_cdc_demo__app__dev','wamn_cdc_demo__app__dev_v2',\
                      'EVT_demo_dev','wamn-cdc-demo--app--dev',NULL,false);\n\
         CREATE TEMP TABLE reader_probe AS EXECUTE geter('demo','app','dev');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.event_readers\n\
                     WHERE org='demo' AND project='app' AND env='dev')=1,\n\
             'upsert_event_reader_sql is idempotent — one row after two upserts';\n\
           ASSERT (SELECT slot FROM reader_probe)='wamn_cdc_demo__app__dev_v2'\n\
              AND (SELECT enabled FROM reader_probe)=false,\n\
             'the second upsert refreshed slot + enabled (ON CONFLICT DO UPDATE)';\n\
           ASSERT (SELECT stream FROM reader_probe)='EVT_demo_dev'\n\
              AND (SELECT replication_secret_name FROM reader_probe)='wamn-cdc-demo--app--dev',\n\
             'select_event_reader_sql returns the stream + replication-Secret reference';\n\
         END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO registry.event_readers\n\
               (org, project, env, publication, slot, stream, replication_secret_name)\n\
             VALUES ('demo','app','prod','p','s','EVT_demo_prod','sec');\n\
           ASSERT false, 'a registration for an unprovisioned project-env must be rejected (FK)';\n\
         EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;\n\
         DROP TABLE reader_probe;\n\
         DELETE FROM registry.orgs WHERE id='demo';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.event_readers WHERE org='demo')=0,\n\
             'deleting an org cascades its CDC registrations (through project_envs)';\n\
         END $$;\n\
         DEALLOCATE uper; DEALLOCATE geter;\n",
        upsert = wamn_registry::sql::upsert_event_reader_sql(),
        select = wamn_registry::sql::select_event_reader_sql(),
    ));
    script.push_str("DROP SCHEMA registry CASCADE;\n");
    script.push_str("DROP SCHEMA provisioning CASCADE;\n");
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

/// Render `EXECUTE stamp(...)` lines for every policy in a template, with the
/// MODEL's values as literals — so the live gate stamps exactly what
/// `provision-org` would (the `Template` policies through the real
/// `stamp_env_policy_sql` builder). None of the shipped values contain a single
/// quote, so plain `'{}'` quoting is exact.
fn stamp_statements(org: &str, template: &Template) -> String {
    let mut s = String::new();
    for p in &template.policies {
        let recovery = serde_json::to_string(&p.recovery_domain).expect("recovery json");
        s.push_str(&format!(
            "EXECUTE stamp('{org}','{name}','{recovery}',{rank},{inst},\
             '{storage}','{cpu}','{memory}','{image}','{backup}','{wal}','{hib}');\n",
            name = p.name,
            rank = p.promotion_rank,
            inst = p.instances,
            storage = p.storage,
            cpu = p.cpu,
            memory = p.memory,
            image = p.image,
            backup = p.backup_cadence,
            wal = p.wal_retention,
            hib = p.hibernation,
        ));
    }
    s
}

/// The live assertions (kept out of the Rust string plumbing for readability).
const ASSERTIONS: &str = r#"
-- FK integrity: an org + its per-org policies (8df.4 fixtures — the REAL stamp
-- builder is exercised via PREPARE later) + its project + two provisioned envs
-- (references only). 'try' deliberately gets NO policy rows.
INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
  VALUES ('acme','dedicated',NULL),
         ('try','pooled','wamn-pg');
INSERT INTO registry.env_policies
    (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
  VALUES ('acme','dev','"own"'::jsonb,10,1,'2Gi','200m','256Mi','ghcr.io/cloudnative-pg/postgresql:18'),
         ('acme','prod','"own"'::jsonb,30,3,'2Gi','200m','256Mi','ghcr.io/cloudnative-pg/postgresql:18');
INSERT INTO registry.projects (org, id) VALUES ('acme','billing'),('try','demo');
INSERT INTO registry.project_envs (org, project, env, secret_name)
  VALUES ('acme','billing','prod','wamn-db-acme-prod'),
         ('acme','billing','dev','wamn-db-acme-dev');

-- A policy row under an unregistered org is rejected (FK to orgs).
DO $$ BEGIN BEGIN
  INSERT INTO registry.env_policies
      (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
    VALUES ('ghost','dev','"own"'::jsonb,10,1,'2Gi','200m','256Mi','x');
  ASSERT false, 'a policy under an unknown org must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- ORG-SCOPING (8df.4): another org's policy never satisfies this org's env FK.
-- 'try' has no policies, so a try project-env is rejected even though 'acme'
-- has a 'dev' policy — the composite (org, env) FK is what keeps a T2 and a T4
-- org's identically-named envs independent.
DO $$ BEGIN BEGIN
  INSERT INTO registry.project_envs (org, project, env, secret_name)
    VALUES ('try','demo','dev','s');
  ASSERT false, 'an env with no policy in ITS org must be rejected (composite FK)';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- A policy in use by a provisioned env cannot be dropped (the deliberate
-- NO ACTION FK), while a whole-org DELETE still cascades (asserted at the end).
DO $$ BEGIN BEGIN
  DELETE FROM registry.env_policies WHERE org='acme' AND name='prod';
  ASSERT false, 'a policy referenced by a provisioned env must not be droppable';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- A dump record for that project-env — cascades with the org below, and its FK
-- requires the project-env to exist.
INSERT INTO provisioning.dumps (org, project, env, object_key, byte_size)
  VALUES ('acme','billing','prod','dumps/acme/billing/prod/1', 42);
-- A dump under an unregistered project-env is rejected (FK to project_envs).
DO $$ BEGIN BEGIN
  INSERT INTO provisioning.dumps (org, project, env, object_key)
    VALUES ('acme','ghost','prod','dumps/acme/ghost/prod/1');
  ASSERT false, 'a dump under an unknown project-env must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

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

-- D18: an env that names no policy in ITS org's set is rejected (the composite
-- env FK — the retired env CHECK's replacement). 'staging' is not stamped.
DO $$ BEGIN BEGIN
  INSERT INTO registry.project_envs (org, project, env, secret_name)
    VALUES ('acme','billing','staging','s');
  ASSERT false, 'an env naming no policy must be rejected (env FK)';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;
-- ...but adding the ORG's policy first lets it in (env is data, not a closed CHECK).
INSERT INTO registry.env_policies (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
  VALUES ('acme', 'staging', '"own"'::jsonb, 20, 1, '2Gi', '200m', '256Mi', 'ghcr.io/cloudnative-pg/postgresql:18');
INSERT INTO registry.project_envs (org, project, env, secret_name)
  VALUES ('acme','billing','staging','wamn-db-acme-staging');
DO $$ BEGIN ASSERT (SELECT count(*) FROM registry.project_envs
    WHERE org='acme' AND project='billing' AND env='staging')=1,
  'a project-env in a newly-added env resolves (env is data)'; END $$;

-- D18 placement: the pooled ⟺ pool_cluster CHECK. A pooled org MUST name a pool.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
    VALUES ('badpool','pooled',NULL);
  ASSERT false, 'a pooled org with no pool cluster must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- A dedicated org MUST NOT carry a pool (its clusters are derived).
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
    VALUES ('baddedicated','dedicated','wamn-pg');
  ASSERT false, 'a dedicated org must not carry a pool cluster';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- An unknown placement_kind is rejected.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind) VALUES ('badkind','elastic');
  ASSERT false, 'an unknown placement_kind must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;

-- cjv.20: the charset/length backstop on the stored slug/name columns. Each
-- malformed value is rejected by its named CHECK (check_violation); a well-formed
-- one applies. (The PRIMARY guard is crates/wamn-registry validate(); this DB
-- CHECK backstops a writer that skips both provision-org AND validate().)
-- orgs.id — a check_id mirror (slug + <= 40 bytes + reserved-wamn).
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('Bad_Id','dedicated',NULL);
  ASSERT false, 'a non-slug org id must be rejected (orgs_id_charset_check)';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- Uppercase alone is rejected (the CHECK is case-SENSITIVE `~`, not `~*`).
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('BadCaps','dedicated',NULL);
  ASSERT false, 'an uppercase org id must be rejected (case-sensitive charset)';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('wamn-x','dedicated',NULL);
  ASSERT false, 'a reserved-wamn org id must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
    VALUES (repeat('a',41),'dedicated',NULL);
  ASSERT false, 'an over-length org id must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- orgs.pool_cluster — a check_name mirror (slug + <= 63; MAY carry the wamn prefix).
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('goodorg','pooled','Bad_Pool');
  ASSERT false, 'a non-slug pool cluster must be rejected (orgs_pool_cluster_charset_check)';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- A well-formed org id + a wamn-prefixed pool cluster applies (then cleaned up).
INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('goodorg','pooled','wamn-pg');
DO $$ BEGIN ASSERT (SELECT count(*) FROM registry.orgs WHERE id='goodorg')=1,
  'a well-formed org id + wamn-prefixed pool cluster applies'; END $$;
DELETE FROM registry.orgs WHERE id='goodorg';
-- projects.id — a check_id mirror (slug + reserved), under the existing 'try' org.
DO $$ BEGIN BEGIN
  INSERT INTO registry.projects (org, id) VALUES ('try','Bad_Proj');
  ASSERT false, 'a non-slug project id must be rejected (projects_id_charset_check)';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.projects (org, id) VALUES ('try','wamn-run');
  ASSERT false, 'a reserved-wamn project id must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- env_policies.name — a check_env mirror (slug + <= 40, NO reserved), under 'acme'.
DO $$ BEGIN BEGIN
  INSERT INTO registry.env_policies
      (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
    VALUES ('acme','Bad_Env','"own"'::jsonb,10,1,'2Gi','200m','256Mi','x');
  ASSERT false, 'a non-slug env policy name must be rejected (env_policies_name_charset_check)';
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

-- Invariant 2 also covers the CDC registrations (wamn-l5i9.9): event_readers
-- carries the replication-Secret REFERENCE and NO credential column — the
-- replication credential is its own tier and never lands in the registry.
DO $$ DECLARE bad int; BEGIN
  SELECT count(*) INTO bad FROM information_schema.columns
    WHERE table_schema='registry' AND table_name='event_readers'
      AND column_name IN ('password','secret','secret_value','url','dsn',
                          'credential','credentials','connection_string');
  ASSERT bad=0, 'event_readers must hold NO credential column (R8b) — references only';
  ASSERT (SELECT count(*) FROM information_schema.columns
    WHERE table_schema='registry' AND table_name='event_readers'
      AND column_name IN ('replication_secret_name','replication_secret_namespace'))=2,
    'event_readers must carry the replication-Secret reference (name + optional namespace)';
END $$;

-- Invariant 3 (no tenant data): the ONLY tables in the system DB are the
-- control-plane set (now including env_policies + event_readers).
DO $$ DECLARE tbls text; BEGIN
  SELECT string_agg(table_schema||'.'||table_name, ',' ORDER BY table_schema, table_name)
    INTO tbls FROM information_schema.tables
    WHERE table_schema IN ('registry','provisioning') AND table_type='BASE TABLE';
  ASSERT tbls = 'provisioning.dumps,provisioning.sagas,registry.env_policies,registry.event_readers,registry.meta,registry.orgs,registry.project_envs,registry.projects',
    format('unexpected control-plane table set (invariant 3): %s', tbls);
END $$;

-- Saga: creation is exactly-once via the saga_id PK; the kind/status CHECKs hold.
INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s1','provision-org','acme')
  ON CONFLICT (saga_id) DO NOTHING;
DO $$ BEGIN BEGIN
  INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s2','provision-everything','x');
  ASSERT false, 'an unknown saga kind must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  UPDATE provisioning.sagas SET status='bogus' WHERE saga_id='s1';
  ASSERT false, 'an unknown saga status must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;

-- Deleting an org cascades its projects, project-envs, their dump records, AND
-- its policy rows — the whole-org delete succeeds despite the in-use-policy
-- NO ACTION FK (checked at statement end, after the project-env cascade).
DELETE FROM registry.orgs WHERE id='acme';
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM registry.projects WHERE org='acme')=0, 'projects cascade';
  ASSERT (SELECT count(*) FROM registry.project_envs WHERE org='acme')=0, 'project-envs cascade';
  ASSERT (SELECT count(*) FROM provisioning.dumps WHERE org='acme')=0, 'dumps cascade';
  ASSERT (SELECT count(*) FROM registry.env_policies WHERE org='acme')=0, 'env-policies cascade';
END $$;
"#;
