//! Storage-schema tests for the T1 control-plane registry (wamn-q3n.3;
//! generalized in wamn-8df.3; org-targeted policies + templates in wamn-8df.4).
//!
//! A live-apply acceptance test (invariants 2/3 + placement/env FK integrity + the template
//! stamp insert-if-absent semantics + the real registry reads and saga builders),
//! on a test database of the test PostgreSQL server (a superuser URL — the
//! harness provisions the `wamn_system` owner role). Invariant 1 (no data-plane
//! manifest names the system cluster) is a repo-policy lint.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command as Proc, Stdio};

use wamn_control_provision::Template;

// --- live-apply acceptance test: invariants 2/3 + placement/env FK + seed + saga --------

/// Apply `deploy/sql/system-schema.sql` to a test database and assert the live,
/// DB-enforced invariants. The test connects as the superuser (the harness
/// provisions the `wamn_system` owner role) and holds the process lock, because
/// it creates cluster-wide roles.
#[test]
fn system_schema_applies_and_enforces_invariants_on_postgres() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let mut script = String::new();
    script.push_str(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
         CREATE ROLE wamn_system LOGIN PASSWORD 'wamn_system' NOSUPERUSER; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS registry CASCADE;\n\
         DROP SCHEMA IF EXISTS provisioning CASCADE;\n\
         DROP SCHEMA IF EXISTS identity CASCADE;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;\n",
    );
    script.push_str(wamn_control_provision::sql::ensure_db_owner_role_sql());
    script.push_str("\nSET ROLE wamn_system;\n");
    script.push_str(wamn_control_provision::SYSTEM_SCHEMA_SQL);
    script.push('\n');
    script.push_str("SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b';\n");
    script.push_str(ASSERTIONS);
    script.push_str(&identifier_cases());
    // Exercise the REAL org-row builder via PREPARE/EXECUTE: two upserts of the
    // same id must collapse to ONE row (the second refreshing the placement),
    // testing `ON CONFLICT (id) DO UPDATE`.
    writeln!(
        script,
        "PREPARE up (text,text,text) AS {upsert};\n\
         EXECUTE up('demo','pooled','wamn-pg');\n\
         EXECUTE up('demo','dedicated',NULL);\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.orgs WHERE id='demo')=1,\n\
             'upsert_org_sql is idempotent — one row after two upserts';\n\
           ASSERT (SELECT placement_type FROM registry.orgs WHERE id='demo')='dedicated',\n\
             'the second upsert refreshed the placement (ON CONFLICT DO UPDATE)';\n\
           ASSERT (SELECT pool_cluster FROM registry.orgs WHERE id='demo') IS NULL,\n\
             'the second (dedicated) upsert cleared the pool cluster';\n\
         END $$;\n\
         DEALLOCATE up;",
        upsert = wamn_registry::sql::upsert_org_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL template stamp (8df.4) against the 'demo' org: stamp the
    // trials policy set with the model's values, customize one row, re-stamp, and
    // assert the customization SURVIVES (insert-if-absent — a DO UPDATE mutant
    // would clobber it back to template values and fail here).
    writeln!(
        script,
        "PREPARE stamp (text,text,text,int,int,text,text,text,text,text,text,text) AS {stamp};",
        stamp = wamn_registry::sql::stamp_env_policy_sql(),
    )
    .expect("writing to a String cannot fail");
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
    writeln!(
        script,
        "PREPARE upp (text,text) AS {up_project};\n\
         PREPARE upe (text,text,text,text,text,text,boolean,text,text) AS {up_env};\n\
         EXECUTE upp('demo','app');\n\
         EXECUTE upp('demo','app');\n\
         EXECUTE upe('demo','app','dev','wamn-db-demo--app--dev-OLD', NULL, 'k3m9x2p7', true, 'dev', NULL);\n\
         DO $$ DECLARE suffix text; marker boolean; BEGIN\n\
           EXECUTE $upe${up_env}$upe$ INTO suffix, marker\n\
             USING 'demo', 'app', 'dev', 'wamn-db-demo--dev--app', NULL::text, 'r4n8c6v2', false, 'dev', NULL;\n\
           ASSERT suffix = 'k3m9x2p7' AND NOT marker,\n\
             'a re-provision gets back the stored instance suffix and marker (RETURNING)';\n\
         END $$;\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.projects WHERE org='demo' AND id='app')=1,\n\
             'upsert_project_sql is idempotent — one project row after two upserts';\n\
           ASSERT (SELECT secret_name FROM registry.environment_projects\n\
                     WHERE org='demo' AND project='app' AND env='dev')='wamn-db-demo--dev--app',\n\
             'the second project-env upsert refreshed the Secret reference (ON CONFLICT DO UPDATE)';\n\
           ASSERT (SELECT instance_suffix FROM registry.environment_projects\n\
                     WHERE org='demo' AND project='app' AND env='dev')='k3m9x2p7',\n\
             'the second project-env upsert preserved the originally minted instance suffix';\n\
           ASSERT NOT (SELECT disposable FROM registry.environment_projects JOIN registry.environments USING (org,env)\n\
                     WHERE org='demo' AND project='app' AND env='dev'),\n\
             'the disposable marker follows THIS provisioning, unlike the minted suffix';\n\
           -- Ask the SERVER what an unstated marker means, not the DDL text.\n\
           ASSERT (SELECT attnotnull FROM pg_attribute\n\
                     WHERE attrelid='registry.environments'::regclass\n\
                       AND attname='disposable'),\n\
             'the disposable marker must be NOT NULL';\n\
           ASSERT (SELECT pg_get_expr(default_value.adbin, default_value.adrelid)\n\
                     FROM pg_attribute AS column_meta\n\
                     JOIN pg_attrdef AS default_value\n\
                       ON default_value.adrelid = column_meta.attrelid\n\
                      AND default_value.adnum = column_meta.attnum\n\
                    WHERE column_meta.attrelid='registry.environments'::regclass\n\
                      AND column_meta.attname='disposable') = 'false',\n\
             'an environment nobody marked disposable must default to durable';\n\
         END $$;\n\
         DEALLOCATE upp; DEALLOCATE upe;",
        up_project = wamn_registry::sql::upsert_project_sql(),
        up_env = wamn_registry::sql::upsert_project_env_with_policy_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL env-policy read via `CREATE TABLE AS EXECUTE` — provision-
    // project-env reads one of the ORG's policies to derive the cluster owner; a
    // different org's key returns nothing (org-targeted, never cross-org).
    writeln!(
        script,
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
         DROP TABLE policy_probe; DROP TABLE policy_probe_other; DEALLOCATE getpol;",
        get = wamn_registry::sql::select_env_policy_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL registry reads. Each returns only its own org's rows,
    // and the policy set comes back in promotion order, not insertion order:
    // 'early' is inserted last with the lowest rank.
    script.push_str(
        "INSERT INTO registry.env_policies\n\
           (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)\n\
           VALUES ('demo','early','\"own\"'::jsonb,1,1,'2Gi','200m','256Mi','x');\n",
    );
    writeln!(
        script,
        "PREPARE place (text) AS {placement};\n\
         PREPARE pols (text) AS {policies};\n\
         PREPARE one_env (text,text,text) AS {one_env};\n\
         CREATE TEMP TABLE place_probe AS EXECUTE place('try');\n\
         CREATE TEMP TABLE place_other AS EXECUTE place('ghost');\n\
         CREATE TEMP TABLE pols_probe AS EXECUTE pols('demo');\n\
         CREATE TEMP TABLE pols_other AS EXECUTE pols('try');\n\
         CREATE TEMP TABLE one_env_probe AS EXECUTE one_env('demo','app','dev');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT placement_type || '/' || pool_cluster FROM place_probe)='pooled/wamn-pg',\n\
             'select_org_placement_sql reads the org placement';\n\
           ASSERT (SELECT count(*) FROM place_other)=0,\n\
             'select_org_placement_sql returns no row for an unknown org';\n\
           ASSERT (SELECT string_agg(name, ',') FROM pols_probe)='early,dev,prod',\n\
             'select_env_policies_sql returns the org set in promotion order';\n\
           ASSERT (SELECT count(*) FROM pols_other)=0,\n\
             'select_env_policies_sql never returns another org''s policies';\n\
           ASSERT (SELECT secret_name || '/' || instance_suffix FROM one_env_probe)\n\
               ='wamn-db-demo--dev--app/k3m9x2p7',\n\
             'select_project_env_sql reads one project-env by its triple';\n\
         END $$;\n\
         DROP TABLE place_probe, place_other, pols_probe, pols_other,\n\
           one_env_probe;\n\
         DEALLOCATE place; DEALLOCATE pols; DEALLOCATE one_env;",
        placement = wamn_registry::sql::select_org_placement_sql(),
        policies = wamn_registry::sql::select_env_policies_sql(),
        one_env = wamn_registry::sql::select_project_env_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL CDC reader-registration builders (wamn-l5i9.9) against
    // the demo/app/dev project-env provisioned above: upsert twice (the second
    // refreshes slot/enabled), observe the stored state, and reject a registration
    // for an UNPROVISIONED env (the project-env
    // FK — enable-cdc is an overlay on an already-provisioned env), and make sure that
    // the whole-org cascade drops the registration.
    writeln!(
        script,
        "PREPARE uper (text,text,text,text,text,text,text,text,boolean,text[]) AS {upsert};\n\
         EXECUTE uper('demo','app','dev','wamn_cdc_demo__app__dev','wamn_cdc_demo__app__dev',\
                      'EVT_4_demo_3_dev_3_app','wamn-cdc-demo--dev--app',NULL,true,ARRAY['app']);\n\
         EXECUTE uper('demo','app','dev','wamn_cdc_demo__app__dev','wamn_cdc_demo__app__dev_v2',\
                      'EVT_4_demo_3_dev_3_app','wamn-cdc-demo--dev--app',NULL,false,ARRAY['billing']);\n\
         CREATE TEMP TABLE reader_probe AS SELECT * FROM registry.event_readers \
             WHERE org='demo' AND project='app' AND env='dev';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.event_readers\n\
                     WHERE org='demo' AND project='app' AND env='dev')=1,\n\
             'upsert_event_reader_sql is idempotent — one row after two upserts';\n\
           ASSERT (SELECT slot FROM reader_probe)='wamn_cdc_demo__app__dev_v2'\n\
              AND (SELECT enabled FROM reader_probe)=false\n\
              AND (SELECT schema FROM reader_probe)=ARRAY['billing'],\n\
             'the second upsert refreshed slot + enabled + schema (ON CONFLICT DO UPDATE)';\n\
           ASSERT (SELECT stream FROM reader_probe)='EVT_4_demo_3_dev_3_app'\n\
              AND (SELECT replication_secret_name FROM reader_probe)='wamn-cdc-demo--dev--app',\n\
             'registration retains the stream and credential reference';\n\
         END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO registry.event_readers\n\
               (org, project, env, publication, slot, stream, replication_secret_name)\n\
             VALUES ('demo','app','prod','p','s','EVT_4_demo_4_prod_3_app','sec');\n\
           ASSERT false, 'a registration for an unprovisioned project-env must be rejected (FK)';\n\
         EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;\n\
         DROP TABLE reader_probe;\n\
         DELETE FROM registry.orgs WHERE id='demo';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.event_readers WHERE org='demo')=0,\n\
             'deleting an org cascades its CDC registrations (through environment_projects)';\n\
         END $$;\n\
         DEALLOCATE uper;",
        upsert = wamn_registry::sql::upsert_event_reader_sql(),
    )
    .expect("writing to a String cannot fail");
    script.push_str("DROP SCHEMA registry CASCADE;\n");
    script.push_str("DROP SCHEMA provisioning CASCADE;\n");
    script.push_str("RESET ROLE;\n");

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
/// MODEL's values as literals — so the live acceptance test stamps exactly what
/// `provision-org` would (the `Template` policies through the real
/// `stamp_env_policy_sql` builder). None of the shipped values contain a single
/// quote, so plain `'{}'` quoting is exact.
fn stamp_statements(org: &str, template: &Template) -> String {
    let mut s = String::new();
    for p in &template.policies {
        let recovery = serde_json::to_string(&p.recovery_domain).expect("recovery json");
        writeln!(
            s,
            "EXECUTE stamp('{org}','{name}','{recovery}',{rank},{inst},\
             '{storage}','{cpu}','{memory}','{image}','{backup}','{wal}','{hib}');",
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
        )
        .expect("writing to a String cannot fail");
    }
    s
}

/// The identifier cases that the stored slug and name CHECKs must decide as the
/// registry validators do (cjv.20, wamn-R27). Each input is inserted under its
/// column, and the validator's verdict on the same input is the expected
/// answer: an accepted input applies, and a refused one raises `check_violation`.
fn identifier_cases() -> String {
    let long = |length: usize| "a".repeat(length);
    let org_ids = [
        "a-b-c".to_owned(),
        long(40),
        String::new(),
        "Bad_Id".to_owned(),
        "BadCaps".to_owned(),
        "wamn".to_owned(),
        "wamn-x".to_owned(),
        long(41),
        "a--x".to_owned(),
        "-a".to_owned(),
    ];
    let pools = [
        "wamn-pg".to_owned(),
        "wamn--pg--x".to_owned(),
        long(63),
        String::new(),
        "Bad_Pool".to_owned(),
        long(64),
        "pool-".to_owned(),
    ];
    let project_ids = [
        "billing-2".to_owned(),
        long(40),
        "Bad_Proj".to_owned(),
        "wamn-run".to_owned(),
        "x--p".to_owned(),
        long(41),
    ];
    let envs = [
        "canary".to_owned(),
        "wamn-env".to_owned(),
        long(40),
        "Bad_Env".to_owned(),
        "d--v".to_owned(),
        long(41),
    ];

    let mut s = String::from(
        "INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
         VALUES ('probe','dedicated',NULL);\n",
    );
    let mut case = |accepted: bool, insert: String, remove: String, input: &str| {
        if accepted {
            writeln!(s, "{insert};\n{remove};").expect("writing to a String cannot fail");
        } else {
            writeln!(
                s,
                "DO $$ BEGIN BEGIN\n  {insert};\n  \
                 ASSERT false, 'the validators refuse {input:?}, so its CHECK must too';\n\
                 EXCEPTION WHEN check_violation THEN NULL; END; END $$;"
            )
            .expect("writing to a String cannot fail");
        }
    };
    for id in &org_ids {
        case(
            wamn_registry::identifiers::valid_project(id),
            format!(
                "INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
                 VALUES ('{id}','dedicated',NULL)"
            ),
            format!("DELETE FROM registry.orgs WHERE id='{id}'"),
            id,
        );
    }
    for pool in &pools {
        case(
            !pool.is_empty()
                && pool.len() <= 63
                && pool
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && pool
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && pool
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric),
            format!(
                "INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
                 VALUES ('pooled','pooled','{pool}')"
            ),
            "DELETE FROM registry.orgs WHERE id='pooled'".to_owned(),
            pool,
        );
    }
    for id in &project_ids {
        case(
            wamn_registry::identifiers::valid_project(id),
            format!("INSERT INTO registry.projects (org, id) VALUES ('probe','{id}')"),
            format!("DELETE FROM registry.projects WHERE org='probe' AND id='{id}'"),
            id,
        );
    }
    for env in &envs {
        case(
            wamn_registry::identifiers::project_id_reason(env).is_none(),
            format!(
                "INSERT INTO registry.env_policies \
                 (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
                 VALUES ('probe','{env}','\"own\"'::jsonb,10,1,'2Gi','200m','256Mi','x')"
            ),
            format!("DELETE FROM registry.env_policies WHERE org='probe' AND name='{env}'"),
            env,
        );
    }
    s.push_str("DELETE FROM registry.orgs WHERE id='probe';\n");
    s
}

/// The live assertions (kept out of the Rust string plumbing for readability).
const ASSERTIONS: &str = r#"
-- FK integrity: an org + its per-org policies (8df.4 fixtures — the REAL stamp
-- builder is exercised via PREPARE later) + its project + two provisioned envs
-- (references only). 'try' deliberately gets NO policy rows.
INSERT INTO registry.orgs (id, placement_type, pool_cluster)
  VALUES ('acme','dedicated',NULL),
         ('try','pooled','wamn-pg');
INSERT INTO registry.env_policies
    (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
  VALUES ('acme','dev','"own"'::jsonb,10,1,'2Gi','200m','256Mi','ghcr.io/cloudnative-pg/postgresql:18'),
         ('acme','prod','"own"'::jsonb,30,3,'2Gi','200m','256Mi','ghcr.io/cloudnative-pg/postgresql:18');
INSERT INTO registry.projects (org, id) VALUES ('acme','billing'),('try','demo');
WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('acme','prod','prod'),('acme','dev','dev') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('acme','billing','prod','wamn-db-acme-prod','k3m9x2p7'),('acme','billing','dev','wamn-db-acme-dev','q80zdw41');

-- A policy row under an unregistered org is rejected (FK to orgs).
DO $$ BEGIN BEGIN
  INSERT INTO registry.env_policies
      (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
    VALUES ('ghost','dev','"own"'::jsonb,10,1,'2Gi','200m','256Mi','x');
  ASSERT false, 'a policy under an unknown org must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- ORG-SCOPING (8df.4): another org's policy never satisfies this org's policy FK.
-- 'try' has no policies, so a try project-env is rejected even though 'acme'
-- has a 'dev' policy — the composite (org, policy_name) FK is what keeps a T2 and a T4
-- org's identically-named envs independent.
DO $$ BEGIN BEGIN
  WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('try','dev','dev') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('try','demo','dev','s','aaaaaaaa');
  ASSERT false, 'an env with no policy in ITS org must be rejected (composite FK)';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- A policy in use by a provisioned env cannot be dropped (the deliberate
-- NO ACTION FK), while a whole-org DELETE still cascades (asserted at the end).
DO $$ BEGIN BEGIN
  DELETE FROM registry.env_policies WHERE org='acme' AND name='prod';
  ASSERT false, 'a policy referenced by a provisioned env must not be droppable';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- A project under an unregistered org is rejected (FK).
DO $$ BEGIN BEGIN
  INSERT INTO registry.projects (org, id) VALUES ('ghost','x');
  ASSERT false, 'a project under an unknown org must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- A project-env under an unregistered project is rejected (FK).
DO $$ BEGIN BEGIN
  WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('acme','prod','prod') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('acme','ghost','prod','s','bbbbbbbb');
  ASSERT false, 'a project-env under an unknown project must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- D18: an env that names no policy in ITS org's set is rejected (the composite
-- policy_name FK — the retired env CHECK's replacement). 'staging' is not stamped.
DO $$ BEGIN BEGIN
  WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('acme','staging','staging') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('acme','billing','staging','s','cccccccc');
  ASSERT false, 'an env naming no policy must be rejected (policy_name FK)';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;
-- ...but adding the ORG's policy first lets it in (env is data, not a closed CHECK).
INSERT INTO registry.env_policies (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
  VALUES ('acme', 'staging', '"own"'::jsonb, 20, 1, '2Gi', '200m', '256Mi', 'ghcr.io/cloudnative-pg/postgresql:18');
WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('acme','staging','staging') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('acme','billing','staging','wamn-db-acme-staging','dddddddd');
DO $$ BEGIN ASSERT (SELECT count(*) FROM registry.environment_projects
    WHERE org='acme' AND project='billing' AND env='staging')=1,
  'a project-env in a newly-added env resolves (env is data)'; END $$;

-- D18 placement: the pooled ⟺ pool_cluster CHECK. A pooled org MUST name a pool.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_type, pool_cluster)
    VALUES ('badpool','pooled',NULL);
  ASSERT false, 'a pooled org with no pool cluster must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- A dedicated org MUST NOT carry a pool (its clusters are derived).
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_type, pool_cluster)
    VALUES ('baddedicated','dedicated','wamn-pg');
  ASSERT false, 'a dedicated org must not carry a pool cluster';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- An unknown placement_type is rejected.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_type) VALUES ('badtype','elastic');
  ASSERT false, 'an unknown placement_type must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;

-- Registry and provisioning store references, not project database credentials.
-- Identity owns authentication hashes and tokens separately.
DO $$ BEGIN
  ASSERT NOT EXISTS (
    SELECT FROM information_schema.columns
    WHERE table_schema IN ('registry', 'provisioning')
      AND column_name ~ '(password|secret_value|credential|(^|_)dsn($|_)|connection_string)'
      -- A generation references its identity.credential row by id, and an
      -- auth type names the fields of a credential.
      AND (table_name, column_name) NOT IN
        (('connection_generation', 'credential_id'), ('auth_type', 'credential_schema'))
  ),'registry and provisioning must hold no project DB credential columns';
END $$;

-- Deleting an org cascades its projects, project-envs, and policy rows — the
-- whole-org delete succeeds despite the in-use-policy
-- NO ACTION FK (validated at statement end, after the project-env cascade).
DELETE FROM registry.orgs WHERE id='acme';
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM registry.projects WHERE org='acme')=0, 'projects cascade';
  ASSERT (SELECT count(*) FROM registry.environment_projects WHERE org='acme')=0, 'project-envs cascade';
  ASSERT (SELECT count(*) FROM registry.env_policies WHERE org='acme')=0, 'env-policies cascade';
  -- wamn-0h0g.15.90: the CASCADE that erases the live rows is exactly the path
  -- that loses the instance identity, so the retention trigger must have caught
  -- all three before they went. Drop the trigger and this is 0.
  ASSERT (SELECT count(*) FROM registry.retired_project_envs WHERE org='acme')=3,
    'a cascaded-away project-env leaves a retired-instance handle';
  ASSERT (SELECT instance_suffix FROM registry.retired_project_envs
            WHERE org='acme' AND project='billing' AND env='prod')='k3m9x2p7',
    'the retained handle is the dead instance suffix, verbatim';
END $$;
"#;
