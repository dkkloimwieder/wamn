//! Live-apply test for the operations persistence extension.
//!
//! The core control schema remains independently installable. The ops artifact
//! is applied afterwards, owns exactly two operations relations, and may
//! reference only the core project-environment identity.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

/// Apply core once and the ops extension twice to a test database, then
/// exercise the real builders. The test holds the process lock, because it
/// creates cluster-wide roles.
#[test]
fn ops_schema_applies_idempotently_after_core_on_postgres() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let mut script = String::new();
    script.push_str(wamn_control_provision::state::ensure_ops_role_sql());
    script.push('\n');
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
    // The ops artifact extends an installed core and creates no core object.
    script.push_str(CORE_OBJECTS_BEFORE);
    script.push_str(wamn_control_provision::OPS_SCHEMA_SQL);
    script.push('\n');
    script.push_str(wamn_control_provision::OPS_SCHEMA_SQL);
    script.push('\n');
    script.push_str(CORE_OBJECTS_UNCHANGED);
    script.push_str(
        "INSERT INTO registry.orgs (id, placement_kind) VALUES ('acme','dedicated');\n\
         INSERT INTO registry.env_policies \
             (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) \
           VALUES ('acme','dev','\"own\"'::jsonb,10,1,'2Gi','200m','256Mi','postgres:18');\n\
         INSERT INTO registry.projects (org,id) VALUES ('acme','app');\n\
         INSERT INTO registry.project_envs (org,project,env,secret_name,instance_suffix) \
           VALUES ('acme','app','dev','wamn-db-acme--app--dev','k3m9x2p7');\n\
         RESET ROLE;\n\
         SET ROLE wamn_ops;\n",
    );
    writeln!(
        script,
        "PREPARE dump (text,text,text,text,text,bigint) AS {record_dump};\n\
         PREPARE dumps (text,text,text) AS {select_dumps};\n\
         EXECUTE dump('acme','app','dev','dumps/acme/app/dev/2','directory',5);\n\
         EXECUTE dump('acme','app','dev','dumps/acme/app/dev/1','directory',10);\n\
         EXECUTE dump('acme','app','dev','dumps/acme/app/dev/1','directory',20);\n\
         CREATE TEMP TABLE dumps_probe AS EXECUTE dumps('acme','app','dev');\n\
         PREPARE copy (text,text,text,int) AS {create_copy};\n\
         PREPARE advance (text) AS {advance_copy};\n\
         PREPARE complete (text) AS {complete_copy};\n\
         PREPARE fail (text,text) AS {fail_copy};\n\
         PREPARE checkpoint (text) AS {select_copy};\n\
         EXECUTE copy('copy-1','copy','acme/app/dev -> acme/app/prod',5);\n\
         EXECUTE copy('copy-1','copy','acme/app/dev -> acme/app/test',9);\n\
         EXECUTE advance('copy-1');\n\
         CREATE TEMP TABLE copy_running AS EXECUTE checkpoint('copy-1');\n\
         EXECUTE complete('copy-1');\n\
         CREATE TEMP TABLE copy_done AS EXECUTE checkpoint('copy-1');\n\
         EXECUTE copy('copy-2','copy','acme/app/dev -> acme/app/prod',5);\n\
         EXECUTE fail('copy-2','restore refused');\n\
         CREATE TEMP TABLE copy_failed AS EXECUTE checkpoint('copy-2');",
        record_dump = wamn_control_provision::state::record_dump_sql(),
        select_dumps = wamn_control_provision::state::select_dumps_sql(),
        create_copy = wamn_control_provision::state::create_saga_sql(),
        advance_copy = wamn_control_provision::state::advance_saga_step_sql(),
        complete_copy = wamn_control_provision::state::complete_saga_sql(),
        fail_copy = wamn_control_provision::state::fail_saga_sql(),
        select_copy = wamn_control_provision::state::select_saga_sql(),
    )
    .expect("writing to a String cannot fail");
    script.push_str(
        "DO $$ BEGIN\n\
           ASSERT (SELECT byte_size FROM provisioning.dumps WHERE object_key='dumps/acme/app/dev/1')=20, \
             'dump upsert updates metadata';\n\
           ASSERT (SELECT string_agg(object_key, ',') FROM dumps_probe) \
                  = 'dumps/acme/app/dev/1,dumps/acme/app/dev/2', \
             'dumps list newest first';\n\
           ASSERT (SELECT target FROM provisioning.copy_sagas WHERE saga_id='copy-1') \
                  = 'acme/app/dev -> acme/app/prod', \
             'a repeated copy create changes nothing';\n\
           ASSERT (SELECT status || '/' || step || '/' || total_steps FROM copy_running) = 'running/1/5', \
             'copy checkpoint advances';\n\
           ASSERT (SELECT status || '/' || step FROM copy_done) = 'completed/1', \
             'copy completes at its checkpoint';\n\
           ASSERT (SELECT status FROM copy_failed) = 'failed' \
              AND (SELECT last_error FROM provisioning.copy_sagas WHERE saga_id='copy-2') = 'restore refused', \
             'a failed copy keeps its diagnostic';\n\
           ASSERT has_schema_privilege('wamn_ops','provisioning','USAGE'), \
             'wamn_ops needs provisioning usage';\n\
           ASSERT has_table_privilege('wamn_ops','provisioning.dumps','SELECT') \
              AND has_table_privilege('wamn_ops','provisioning.dumps','INSERT') \
              AND has_table_privilege('wamn_ops','provisioning.dumps','UPDATE'), \
             'dump ACL drifted';\n\
           ASSERT has_table_privilege('wamn_ops','provisioning.copy_sagas','SELECT') \
              AND has_table_privilege('wamn_ops','provisioning.copy_sagas','INSERT') \
              AND has_table_privilege('wamn_ops','provisioning.copy_sagas','UPDATE'), \
             'copy ACL drifted';\n\
           ASSERT NOT has_schema_privilege('wamn_ops','provisioning','CREATE'), \
             'wamn_ops must not create provisioning objects';\n\
           ASSERT (SELECT NOT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                            AND NOT rolcreaterole AND NOT rolinherit \
                            AND NOT rolreplication AND NOT rolbypassrls \
                     FROM pg_roles WHERE rolname='wamn_ops'), \
             'wamn_ops security attributes drifted';\n\
         END $$;\n\
         DO $$ BEGIN BEGIN\n\
           INSERT INTO provisioning.dumps (org,project,env,object_key) \
             VALUES ('acme','missing','dev','dumps/acme/missing/dev/1');\n\
           ASSERT false, 'ops-to-core identity FK must reject an unknown project-env';\n\
         EXCEPTION WHEN foreign_key_violation THEN NULL; END; END $$;\n\
         DROP TABLE dumps_probe, copy_running, copy_done, copy_failed;\n\
         DEALLOCATE dump; DEALLOCATE dumps; DEALLOCATE copy; DEALLOCATE advance;\n\
         DEALLOCATE complete; DEALLOCATE fail; DEALLOCATE checkpoint;\n\
         RESET ROLE;\n\
         DO $$ BEGIN\n\
           ASSERT (SELECT string_agg(table_name,',' ORDER BY table_name) \
                     FROM information_schema.tables \
                     WHERE table_schema='provisioning' AND table_type='BASE TABLE') = \
                  'copy_sagas,dumps,sagas', \
             'core plus ops provisioning relation set';\n\
           ASSERT (SELECT string_agg(DISTINCT confrelid::regclass::text, ',') \
                     FROM pg_catalog.pg_constraint \
                     WHERE contype = 'f' \
                       AND conrelid IN ('provisioning.dumps'::regclass, \
                                        'provisioning.copy_sagas'::regclass)) = 'registry.project_envs', \
             'the only foreign key out of the ops relations targets the project-environment identity';\n\
         END $$;\n\
         DROP SCHEMA registry CASCADE;\n\
         DROP SCHEMA provisioning CASCADE;\n\
",
    );

    let mut child = Command::new("psql")
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
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    let output = child.wait_with_output().expect("wait for psql");
    assert!(
        output.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The core schemas and their relations, recorded before the ops artifact runs.
const CORE_OBJECTS_BEFORE: &str = "CREATE TEMP TABLE core_objects AS \
     SELECT namespace.nspname, relation.relname, relation.relkind \
       FROM pg_catalog.pg_namespace AS namespace \
       LEFT JOIN pg_catalog.pg_class AS relation ON relation.relnamespace = namespace.oid \
      WHERE namespace.nspname NOT LIKE 'pg\\_%' AND namespace.nspname <> 'information_schema' \
        AND (relation.oid IS NULL OR namespace.nspname <> 'provisioning');\n";

/// After two ops applies, every schema and every relation outside
/// `provisioning` is exactly the set recorded before.
const CORE_OBJECTS_UNCHANGED: &str = "DO $$ BEGIN\n\
       ASSERT NOT EXISTS ( \
         (SELECT namespace.nspname, relation.relname, relation.relkind \
            FROM pg_catalog.pg_namespace AS namespace \
            LEFT JOIN pg_catalog.pg_class AS relation ON relation.relnamespace = namespace.oid \
           WHERE namespace.nspname NOT LIKE 'pg\\_%' AND namespace.nspname <> 'information_schema' \
             AND (relation.oid IS NULL OR namespace.nspname <> 'provisioning')) \
         EXCEPT SELECT * FROM core_objects), \
         'the ops artifact must not create a schema or a core relation';\n\
     END $$;\n\
     DROP TABLE core_objects;\n";
