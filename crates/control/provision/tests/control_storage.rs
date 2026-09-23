//! Storage-schema tests for the T1 control-plane registry (wamn-q3n.3;
//! generalized in wamn-8df.3; org-scoped policies + templates in wamn-8df.4).
//!
//! A live-apply gate (invariants 2/3 + placement/env FK integrity + the template
//! stamp insert-if-absent semantics + the real registry reads and saga builders),
//! on a test database of the test PostgreSQL server (a superuser URL — the
//! harness provisions the `wamn_system` owner role). Invariant 1 (no data-plane
//! manifest names the system cluster) is a repo-policy lint.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command as Proc, Stdio};

use wamn_control_registry::Template;

// --- live-apply gate: invariants 2/3 + placement/env FK + seed + saga --------

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
    script.push_str(ASSERTIONS);
    // Exercise the REAL org-row builder via PREPARE/EXECUTE: two upserts of the
    // same id must collapse to ONE row (the second refreshing the placement),
    // checking `ON CONFLICT (id) DO UPDATE`.
    writeln!(
        script,
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
         DEALLOCATE up;",
        upsert = wamn_control_registry::sql::upsert_org_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL template stamp (8df.4) against the 'demo' org: stamp the
    // trials policy set with the model's values, customize one row, re-stamp, and
    // assert the customization SURVIVES (insert-if-absent — a DO UPDATE mutant
    // would clobber it back to template values and fail here).
    writeln!(
        script,
        "PREPARE stamp (text,text,text,int,int,text,text,text,text,text,text,text,text) AS {stamp};",
        stamp = wamn_control_registry::sql::stamp_env_policy_sql(),
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
         PREPARE upe (text,text,text,text,text,text,boolean) AS {up_env};\n\
         EXECUTE upp('demo','app');\n\
         EXECUTE upp('demo','app');\n\
         EXECUTE upe('demo','app','dev','wamn-db-demo--app--dev-OLD', NULL, 'k3m9x2p7', true);\n\
         EXECUTE upe('demo','app','dev','wamn-db-demo--app--dev', NULL, 'r4n8c6v2', false);\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.projects WHERE org='demo' AND id='app')=1,\n\
             'upsert_project_sql is idempotent — one project row after two upserts';\n\
           ASSERT (SELECT secret_name FROM registry.project_envs\n\
                     WHERE org='demo' AND project='app' AND env='dev')='wamn-db-demo--app--dev',\n\
             'the second project-env upsert refreshed the Secret reference (ON CONFLICT DO UPDATE)';\n\
           ASSERT (SELECT instance_suffix FROM registry.project_envs\n\
                     WHERE org='demo' AND project='app' AND env='dev')='k3m9x2p7',\n\
             'the second project-env upsert preserved the originally minted instance suffix';\n\
           ASSERT NOT (SELECT disposable FROM registry.project_envs\n\
                     WHERE org='demo' AND project='app' AND env='dev'),\n\
             'the disposable marker follows THIS provisioning, unlike the minted suffix';\n\
           -- Ask the SERVER what an unstated marker means, not the DDL text.\n\
           ASSERT (SELECT attnotnull FROM pg_attribute\n\
                     WHERE attrelid='registry.project_envs'::regclass\n\
                       AND attname='disposable'),\n\
             'the disposable marker must be NOT NULL';\n\
           ASSERT (SELECT pg_get_expr(default_value.adbin, default_value.adrelid)\n\
                     FROM pg_attribute AS column_meta\n\
                     JOIN pg_attrdef AS default_value\n\
                       ON default_value.adrelid = column_meta.attrelid\n\
                      AND default_value.adnum = column_meta.attnum\n\
                    WHERE column_meta.attrelid='registry.project_envs'::regclass\n\
                      AND column_meta.attname='disposable') = 'false',\n\
             'an environment nobody marked disposable must default to durable';\n\
         END $$;\n\
         DEALLOCATE upp; DEALLOCATE upe;",
        up_project = wamn_control_registry::sql::upsert_project_sql(),
        up_env = wamn_control_registry::sql::upsert_project_env_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL env-policy read via `CREATE TABLE AS EXECUTE` — provision-
    // project-env reads one of the ORG's policies to derive the cluster owner; a
    // different org's key returns nothing (org-scoped, never cross-org).
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
        get = wamn_control_registry::sql::select_env_policy_sql(),
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
         PREPARE envs (text) AS {org_envs};\n\
         PREPARE one_env (text,text,text) AS {one_env};\n\
         PREPARE retired (text,text,text) AS {retired};\n\
         CREATE TEMP TABLE place_probe AS EXECUTE place('try');\n\
         CREATE TEMP TABLE place_other AS EXECUTE place('ghost');\n\
         CREATE TEMP TABLE pols_probe AS EXECUTE pols('demo');\n\
         CREATE TEMP TABLE pols_other AS EXECUTE pols('try');\n\
         CREATE TEMP TABLE envs_probe AS EXECUTE envs('demo');\n\
         CREATE TEMP TABLE envs_other AS EXECUTE envs('try');\n\
         CREATE TEMP TABLE one_env_probe AS EXECUTE one_env('demo','app','dev');\n\
         CREATE TEMP TABLE retired_probe AS EXECUTE retired('acme','billing','prod');\n\
         CREATE TEMP TABLE retired_other AS EXECUTE retired('demo','app','dev');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT placement_kind || '/' || pool_cluster FROM place_probe)='pooled/wamn-pg',\n\
             'select_org_placement_sql reads the org placement';\n\
           ASSERT (SELECT count(*) FROM place_other)=0,\n\
             'select_org_placement_sql returns no row for an unknown org';\n\
           ASSERT (SELECT string_agg(name, ',') FROM pols_probe)='early,dev,prod',\n\
             'select_env_policies_sql returns the org set in promotion order';\n\
           ASSERT (SELECT count(*) FROM pols_other)=0,\n\
             'select_env_policies_sql never returns another org''s policies';\n\
           ASSERT (SELECT project || '/' || env || '/' || instance_suffix FROM envs_probe)='app/dev/k3m9x2p7',\n\
             'select_org_project_envs_sql lists the org project-envs';\n\
           ASSERT (SELECT count(*) FROM envs_other)=0,\n\
             'select_org_project_envs_sql never returns another org''s project-envs';\n\
           ASSERT (SELECT secret_name || '/' || instance_suffix FROM one_env_probe)\n\
               ='wamn-db-demo--app--dev/k3m9x2p7',\n\
             'select_project_env_sql reads one project-env by its triple';\n\
           ASSERT (SELECT string_agg(instance_suffix, ',') FROM retired_probe)='k3m9x2p7',\n\
             'select_retired_project_envs_sql reads the retired instance of the triple';\n\
           ASSERT (SELECT count(*) FROM retired_other)=0,\n\
             'select_retired_project_envs_sql never returns a live instance';\n\
         END $$;\n\
         DROP TABLE place_probe, place_other, pols_probe, pols_other, envs_probe, envs_other,\n\
           one_env_probe, retired_probe, retired_other;\n\
         DEALLOCATE place; DEALLOCATE pols; DEALLOCATE envs; DEALLOCATE one_env; DEALLOCATE retired;",
        placement = wamn_control_registry::sql::select_org_placement_sql(),
        policies = wamn_control_registry::sql::select_env_policies_sql(),
        org_envs = wamn_control_registry::sql::select_org_project_envs_sql(),
        one_env = wamn_control_registry::sql::select_project_env_sql(),
        retired = wamn_control_registry::sql::select_retired_project_envs_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL saga builders: a repeated create changes nothing, a step
    // moves the checkpoint forward, and complete and fail set their status.
    writeln!(
        script,
        "PREPARE saga_create (text,text,text,int) AS {create};\n\
         PREPARE saga_advance (text) AS {advance};\n\
         PREPARE saga_complete (text) AS {complete};\n\
         PREPARE saga_fail (text,text) AS {fail};\n\
         PREPARE saga_select (text) AS {select};\n\
         EXECUTE saga_create('saga-a','provision-org','demo',3);\n\
         EXECUTE saga_create('saga-a','provision-project-env','other',9);\n\
         EXECUTE saga_advance('saga-a');\n\
         EXECUTE saga_advance('saga-a');\n\
         CREATE TEMP TABLE saga_running AS EXECUTE saga_select('saga-a');\n\
         EXECUTE saga_complete('saga-a');\n\
         CREATE TEMP TABLE saga_done AS EXECUTE saga_select('saga-a');\n\
         EXECUTE saga_create('saga-b','provision-org','demo',2);\n\
         EXECUTE saga_fail('saga-b','boom');\n\
         CREATE TEMP TABLE saga_failed AS EXECUTE saga_select('saga-b');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT kind || '/' || target FROM provisioning.sagas WHERE saga_id='saga-a')\n\
               ='provision-org/demo',\n\
             'a repeated create_saga_sql changes nothing';\n\
           ASSERT (SELECT status || '/' || step || '/' || total_steps FROM saga_running)='running/2/3',\n\
             'advance_saga_step_sql moves the checkpoint forward';\n\
           ASSERT (SELECT status || '/' || step FROM saga_done)='completed/2',\n\
             'complete_saga_sql completes the saga at its checkpoint';\n\
           ASSERT (SELECT status FROM saga_failed)='failed'\n\
              AND (SELECT last_error FROM provisioning.sagas WHERE saga_id='saga-b')='boom',\n\
             'fail_saga_sql fails the saga and keeps the diagnostic';\n\
         END $$;\n\
         DROP TABLE saga_running, saga_done, saga_failed;\n\
         DEALLOCATE saga_create; DEALLOCATE saga_advance; DEALLOCATE saga_complete;\n\
         DEALLOCATE saga_fail; DEALLOCATE saga_select;",
        create = wamn_control_provision::saga::create_saga_sql(),
        advance = wamn_control_provision::saga::advance_saga_step_sql(),
        complete = wamn_control_provision::saga::complete_saga_sql(),
        fail = wamn_control_provision::saga::fail_saga_sql(),
        select = wamn_control_provision::saga::select_saga_sql(),
    )
    .expect("writing to a String cannot fail");
    // Exercise the REAL CDC reader-registration builders (wamn-l5i9.9) against
    // the demo/app/dev project-env provisioned above: upsert twice (the second
    // refreshes slot/enabled — ON CONFLICT DO UPDATE), read it back via the real
    // select, reject a registration for an UNPROVISIONED env (the project-env
    // FK — enable-cdc is an overlay on an already-provisioned env), and check that
    // the whole-org cascade drops the registration.
    writeln!(
        script,
        "PREPARE uper (text,text,text,text,text,text,text,text,boolean) AS {upsert};\n\
         PREPARE geter (text,text,text) AS {select};\n\
         EXECUTE uper('demo','app','dev','wamn_cdc_demo__app__dev','wamn_cdc_demo__app__dev',\
                      'EVT_4_demo_3_app_3_dev','wamn-cdc-demo--app--dev',NULL,true);\n\
         EXECUTE uper('demo','app','dev','wamn_cdc_demo__app__dev','wamn_cdc_demo__app__dev_v2',\
                      'EVT_4_demo_3_app_3_dev','wamn-cdc-demo--app--dev',NULL,false);\n\
         CREATE TEMP TABLE reader_probe AS EXECUTE geter('demo','app','dev');\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.event_readers\n\
                     WHERE org='demo' AND project='app' AND env='dev')=1,\n\
             'upsert_event_reader_sql is idempotent — one row after two upserts';\n\
           ASSERT (SELECT slot FROM reader_probe)='wamn_cdc_demo__app__dev_v2'\n\
              AND (SELECT enabled FROM reader_probe)=false,\n\
             'the second upsert refreshed slot + enabled (ON CONFLICT DO UPDATE)';\n\
           ASSERT (SELECT stream FROM reader_probe)='EVT_4_demo_3_app_3_dev'\n\
              AND (SELECT replication_secret_name FROM reader_probe)='wamn-cdc-demo--app--dev',\n\
             'select_event_reader_sql returns the stream + replication-Secret reference';\n\
         END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO registry.event_readers\n\
               (org, project, env, publication, slot, stream, replication_secret_name)\n\
             VALUES ('demo','app','prod','p','s','EVT_4_demo_3_app_4_prod','sec');\n\
           ASSERT false, 'a registration for an unprovisioned project-env must be rejected (FK)';\n\
         EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;\n\
         DROP TABLE reader_probe;\n\
         DELETE FROM registry.orgs WHERE id='demo';\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT count(*) FROM registry.event_readers WHERE org='demo')=0,\n\
             'deleting an org cascades its CDC registrations (through project_envs)';\n\
         END $$;\n\
         DEALLOCATE uper; DEALLOCATE geter;",
        upsert = wamn_control_registry::sql::upsert_event_reader_sql(),
        select = wamn_control_registry::sql::select_event_reader_sql(),
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
/// MODEL's values as literals — so the live gate stamps exactly what
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
             '{storage}','{cpu}','{memory}','{image}','{backup}','{wal}','{hib}','{durability}');",
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
            durability = p.durability_class.as_sql(),
        )
        .expect("writing to a String cannot fail");
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
DO $$ BEGIN
  ASSERT (SELECT durability_class FROM registry.env_policies
           WHERE org='acme' AND name='dev')='standard',
    'an omitted durability class takes the standard floor';
  BEGIN
    UPDATE registry.env_policies SET durability_class='premium'
      WHERE org='acme' AND name='dev';
    ASSERT false, 'an unknown durability class must be rejected';
  EXCEPTION WHEN check_violation THEN NULL; END;
END $$;
INSERT INTO registry.projects (org, id) VALUES ('acme','billing'),('try','demo');
INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix)
  VALUES ('acme','billing','prod','wamn-db-acme-prod','k3m9x2p7'),
         ('acme','billing','dev','wamn-db-acme-dev','q80zdw41');

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
  INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix)
    VALUES ('try','demo','dev','s','aaaaaaaa');
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
  INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix)
    VALUES ('acme','ghost','prod','s','bbbbbbbb');
  ASSERT false, 'a project-env under an unknown project must be rejected';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;

-- D18: an env that names no policy in ITS org's set is rejected (the composite
-- env FK — the retired env CHECK's replacement). 'staging' is not stamped.
DO $$ BEGIN BEGIN
  INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix)
    VALUES ('acme','billing','staging','s','cccccccc');
  ASSERT false, 'an env naming no policy must be rejected (env FK)';
EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;
-- ...but adding the ORG's policy first lets it in (env is data, not a closed CHECK).
INSERT INTO registry.env_policies (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
  VALUES ('acme', 'staging', '"own"'::jsonb, 20, 1, '2Gi', '200m', '256Mi', 'ghcr.io/cloudnative-pg/postgresql:18');
INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix)
  VALUES ('acme','billing','staging','wamn-db-acme-staging','dddddddd');
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
-- one applies. (The PRIMARY guard is crates/control/registry validate(); this DB
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

-- wamn-R27: an interior `--` run inside an org/project id or env slug is rejected
-- by the tightened component regex — `--` is the wamn-db-<org>--<project>--<env>
-- (and wamn_cdc_<org>__<project>__<env>) separator, so a `--` run would collide
-- two distinct triples onto ONE derived database / CDC role name.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('a--x','dedicated',NULL);
  ASSERT false, 'a consecutive-hyphen org id must be rejected (wamn-R27)';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.projects (org, id) VALUES ('try','x--p');
  ASSERT false, 'a consecutive-hyphen project id must be rejected (wamn-R27)';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.env_policies
      (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
    VALUES ('acme','d--v','"own"'::jsonb,10,1,'2Gi','200m','256Mi','x');
  ASSERT false, 'a consecutive-hyphen env policy name must be rejected (wamn-R27)';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- Length and reserved-word boundaries that the cases above do not reach.
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('wamn','dedicated',NULL);
  ASSERT false, 'the bare reserved org id wamn must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('longpool','pooled',repeat('a',64));
  ASSERT false, 'a pool cluster over 63 characters must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.projects (org, id) VALUES ('try',repeat('a',41));
  ASSERT false, 'a project id over 40 characters must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  INSERT INTO registry.env_policies
      (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
    VALUES ('acme',repeat('a',41),'"own"'::jsonb,10,1,'2Gi','200m','256Mi','x');
  ASSERT false, 'an env policy name over 40 characters must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- A SINGLE interior hyphen stays valid on a component (org id here); then cleaned up.
INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('a-b-c','dedicated',NULL);
DO $$ BEGIN ASSERT (SELECT count(*) FROM registry.orgs WHERE id='a-b-c')=1,
  'a single interior hyphen is a valid component id'; END $$;
DELETE FROM registry.orgs WHERE id='a-b-c';
-- pool_cluster is a DERIVED name and may carry `--` (the ban is for identity
-- slugs only): a `--` pool cluster on a well-formed pooled org applies.
INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('poolorg','pooled','wamn--pg--x');
DO $$ BEGIN ASSERT (SELECT count(*) FROM registry.orgs WHERE id='poolorg')=1,
  'a derived pool_cluster name may carry consecutive hyphens (wamn-R27 bans slugs, not names)'; END $$;
DELETE FROM registry.orgs WHERE id='poolorg';

-- Registry and provisioning store references, not tenant database credentials.
-- Identity owns authentication hashes and tokens separately.
DO $$ BEGIN
  ASSERT NOT EXISTS (
    SELECT FROM information_schema.columns
    WHERE table_schema IN ('registry', 'provisioning')
      AND column_name ~ '(password|secret_value|credential|(^|_)dsn($|_)|connection_string)'
  ), 'registry and provisioning must hold no tenant DB credential columns';
END $$;

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
-- control-plane registry, provisioning, and first-party identity set.
DO $$ DECLARE tbls text; BEGIN
  SELECT string_agg(table_schema||'.'||table_name, ',' ORDER BY table_schema, table_name)
    INTO tbls FROM information_schema.tables
    WHERE table_schema IN ('registry','provisioning','identity') AND table_type='BASE TABLE';
  ASSERT tbls = 'identity.password_attempts,identity.password_credentials,identity.password_logins,identity.password_tokens,identity.pats,identity.principals,identity.project_env_memberships,identity.project_roles,identity.renewal_credentials,identity.session_keys,identity.session_signing_state,provisioning.sagas,registry.env_policies,registry.event_readers,registry.meta,registry.orgs,registry.project_envs,registry.projects,registry.retired_project_envs',
    format('unexpected control-plane table set (invariant 3): %s', tbls);
END $$;

-- Saga: creation is exactly-once via the saga_id PK; the kind/status CHECKs hold.
INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s1','provision-org','acme')
  ON CONFLICT (saga_id) DO NOTHING;
DO $$ BEGIN BEGIN
  INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s2','provision-everything','x');
  ASSERT false, 'an unknown saga kind must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
-- Copy sagas belong to the operations artifact, never to the core relation.
DO $$ BEGIN BEGIN
  INSERT INTO provisioning.sagas (saga_id, kind, target) VALUES ('s3','copy','x');
  ASSERT false, 'a copy saga must be rejected by the core relation';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;
DO $$ BEGIN BEGIN
  UPDATE provisioning.sagas SET status='bogus' WHERE saga_id='s1';
  ASSERT false, 'an unknown saga status must be rejected';
EXCEPTION WHEN check_violation THEN NULL; END; END $$;

-- Deleting an org cascades its projects, project-envs, and policy rows — the
-- whole-org delete succeeds despite the in-use-policy
-- NO ACTION FK (checked at statement end, after the project-env cascade).
DELETE FROM registry.orgs WHERE id='acme';
DO $$ BEGIN
  ASSERT (SELECT count(*) FROM registry.projects WHERE org='acme')=0, 'projects cascade';
  ASSERT (SELECT count(*) FROM registry.project_envs WHERE org='acme')=0, 'project-envs cascade';
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
