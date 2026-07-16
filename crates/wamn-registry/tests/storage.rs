//! Storage-schema tests for the T1 control-plane registry (wamn-q3n.3;
//! generalized in wamn-8df.3).
//!
//! Three layers, all pure/portable except the last:
//! - a **drift guard** tying `deploy/system-schema.sql` to the `wamn-registry`
//!   model (table/column shape, the D18 placement CHECKs, the seeded `env_policies`
//!   matching `EnvPolicy::defaults()`, `SCHEMA_VERSION`) — the `wamn-schema` /
//!   `state_literals_match_catalog_schema_sql` pattern;
//! - the **request-path-free** invariant (1): a static grep asserting no
//!   data-plane manifest references the T1 cluster / system DB;
//! - a **live-apply gate** (invariants 2/3 + placement/env FK integrity + the
//!   seeded policies + the saga exactly-once/resume checkpoint), gated on
//!   `WAMN_REGISTRY_PG_URL` (a superuser URL — the harness provisions the
//!   `wamn_system` owner role) and skipped cleanly when unset (mirrors wamn-ddl /
//!   wamn-run-store).

use std::path::Path;

use wamn_registry::{EnvPolicy, SCHEMA_VERSION};

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
/// control-plane schemas, the registry tables and their distinctive columns (the
/// D18 placement + env-policy shape), the seeded `env_policies` matching
/// `EnvPolicy::defaults()`, the storage-format `SCHEMA_VERSION`, and the saga table.
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

    // Env policies: the named-policy table + its distinctive columns.
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

    // Projects / project-envs, the latter FK'd to env_policies (env resolves a
    // policy — the D18 referential-integrity replacement for the env CHECK).
    assert!(sql.contains("CREATE TABLE registry.projects"));
    assert!(sql.contains("CREATE TABLE registry.project_envs"));
    assert!(sql.contains("secret_name") && sql.contains("secret_namespace"));
    assert!(
        sql.contains("REFERENCES registry.env_policies (name)"),
        "project_envs.env must FK to env_policies (the retired env CHECK's replacement)"
    );
    assert!(
        !sql.contains("env IN ('dev', 'canary', 'prod')"),
        "the closed env CHECK must be retired"
    );

    // The seed must carry the two default policies with the model's values — a
    // drift guard tying the SQL seed to EnvPolicy::dev() / prod().
    for p in EnvPolicy::defaults() {
        assert!(
            sql.contains(&format!("'{}'", p.name)),
            "seed is missing the {:?} policy",
            p.name
        );
        for lit in [&p.image, &p.hibernation] {
            assert!(
                sql.contains(&format!("'{lit}'")),
                "seed missing literal {lit:?}"
            );
        }
    }
    // `own` recovery domain and prod's cadence/retention are seeded literally.
    assert!(sql.contains("'\"own\"'::jsonb"));
    assert!(sql.contains("'0 0 */6 * * *'") && sql.contains("'14d'"));

    // The storage-format version is recorded (singleton meta row).
    assert!(sql.contains(&format!("'{SCHEMA_VERSION}'")));

    // The saga table + its kind literals.
    assert!(sql.contains("CREATE TABLE provisioning.sagas"));
    assert!(sql.contains("'provision-org'") && sql.contains("'provision-project-env'"));
}

/// The org-row builder (`wamn_registry::sql::upsert_org_sql`) must target exactly
/// the `registry.orgs` placement columns the storage DDL declares — a drift guard
/// tying the builder to `deploy/system-schema.sql` (SR2: registry SQL lives with
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

    // The env-policy reads target env_policies (provision-org sizes clusters from
    // the full policy set; provision-project-env reads one to derive the owner).
    for reader in [
        wamn_registry::sql::select_env_policies_sql(),
        wamn_registry::sql::select_env_policy_sql(),
    ] {
        assert!(reader.contains("FROM registry.env_policies"));
        assert!(reader.contains("recovery_domain::text"));
    }
    assert!(wamn_registry::sql::select_env_policies_sql().contains("ORDER BY promotion_rank"));
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

// --- live-apply gate: invariants 2/3 + placement/env FK + seed + saga --------

/// Apply `deploy/system-schema.sql` to a throwaway Postgres and assert the live,
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
    // The seeded env policies match the model's EnvPolicy::defaults() exactly — a
    // live drift guard: a divergence between the SQL seed and the Rust defaults
    // fails an ASSERT here (a name-only text guard cannot catch a value drift).
    // Run BEFORE ASSERTIONS, which adds a 'staging' policy to prove env is data.
    script.push_str(&env_policy_seed_assertions());
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
    // Exercise the REAL project / project-env builders against the 'demo' org just
    // upserted. env 'dev' resolves the seeded policy (the FK holds).
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
    // project-env reads one policy by name to derive the cluster owner.
    script.push_str(&format!(
        "PREPARE getpol (text) AS {get};\n\
         CREATE TEMP TABLE policy_probe AS EXECUTE getpol('dev');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM policy_probe)=1,\n\
             'select_env_policy_sql returns exactly one row';\n\
           ASSERT (SELECT name FROM policy_probe)='dev',\n\
             'select_env_policy_sql returns the named policy';\n\
         END $$;\n\
         DROP TABLE policy_probe; DEALLOCATE getpol;\n",
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

/// Build the live env-policy seed assertions from the model, so a divergence
/// between `deploy/system-schema.sql`'s seed and `EnvPolicy::dev()` / `prod()`
/// fails the live gate. Both defaults are `own`, so the recovery-domain assertion
/// is the literal `"own"`.
fn env_policy_seed_assertions() -> String {
    let mut s = String::from(
        "DO $$ BEGIN ASSERT (SELECT count(*) FROM registry.env_policies)=2,\n\
           'exactly the two default policies are seeded'; END $$;\n",
    );
    for p in EnvPolicy::defaults() {
        s.push_str(&format!(
            "DO $$ BEGIN\n\
               ASSERT (SELECT promotion_rank FROM registry.env_policies WHERE name='{name}')={rank},\n\
                 'seed {name}.promotion_rank matches the model';\n\
               ASSERT (SELECT instances FROM registry.env_policies WHERE name='{name}')={inst},\n\
                 'seed {name}.instances matches the model';\n\
               ASSERT (SELECT storage FROM registry.env_policies WHERE name='{name}')='{storage}',\n\
                 'seed {name}.storage matches the model';\n\
               ASSERT (SELECT image FROM registry.env_policies WHERE name='{name}')='{image}',\n\
                 'seed {name}.image matches the model';\n\
               ASSERT (SELECT backup_cadence FROM registry.env_policies WHERE name='{name}')='{backup}',\n\
                 'seed {name}.backup_cadence matches the model';\n\
               ASSERT (SELECT wal_retention FROM registry.env_policies WHERE name='{name}')='{wal}',\n\
                 'seed {name}.wal_retention matches the model';\n\
               ASSERT (SELECT hibernation FROM registry.env_policies WHERE name='{name}')='{hib}',\n\
                 'seed {name}.hibernation matches the model';\n\
               ASSERT (SELECT recovery_domain FROM registry.env_policies WHERE name='{name}')='\"own\"'::jsonb,\n\
                 'seed {name}.recovery_domain is own';\n\
             END $$;\n",
            name = p.name,
            rank = p.promotion_rank,
            inst = p.instances,
            storage = p.storage,
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
-- FK integrity: an org + its project + two provisioned envs (references only).
-- env 'prod' / 'dev' resolve the seeded env_policies (the env FK holds).
INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
  VALUES ('acme','dedicated',NULL),
         ('try','pooled','wamn-pg');
INSERT INTO registry.projects (org, id) VALUES ('acme','billing'),('try','demo');
INSERT INTO registry.project_envs (org, project, env, secret_name)
  VALUES ('acme','billing','prod','wamn-db-acme-prod'),
         ('acme','billing','dev','wamn-db-acme-dev');

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

-- D18: an env that names no seeded policy is rejected (the env FK — the retired
-- env CHECK's replacement). 'staging' is not a seeded policy.
DO $$ BEGIN BEGIN
  INSERT INTO registry.project_envs (org, project, env, secret_name)
    VALUES ('acme','billing','staging','s');
  ASSERT false, 'an env naming no policy must be rejected (env FK)';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;
-- ...but adding the policy first lets it in (env is data, not a closed CHECK).
INSERT INTO registry.env_policies (name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
  VALUES ('staging', '"own"'::jsonb, 20, 1, '2Gi', '200m', '256Mi', 'ghcr.io/cloudnative-pg/postgresql:18');
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
-- control-plane set (now including env_policies).
DO $$ DECLARE tbls text; BEGIN
  SELECT string_agg(table_schema||'.'||table_name, ',' ORDER BY table_schema, table_name)
    INTO tbls FROM information_schema.tables
    WHERE table_schema IN ('registry','provisioning') AND table_type='BASE TABLE';
  ASSERT tbls = 'provisioning.dumps,provisioning.sagas,registry.env_policies,registry.meta,registry.orgs,registry.project_envs,registry.projects',
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

-- Deleting an org cascades its projects, project-envs, and their dump records.
DELETE FROM registry.orgs WHERE id='acme';
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM registry.projects WHERE org='acme')=0, 'projects cascade';
  ASSERT (SELECT count(*) FROM registry.project_envs WHERE org='acme')=0, 'project-envs cascade';
  ASSERT (SELECT count(*) FROM provisioning.dumps WHERE org='acme')=0, 'dumps cascade';
END $$;
"#;
