//! Ignored live gate for the fenced run-state transitions.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use wamn_run_state::transitions::{
    complete_sql, release_caller_sql, reserved_checkpoint_sql, terminalize_sql,
};

fn psql(url: &str, script: &str) -> Output {
    let mut child = Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", "-1", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start psql");
    let mut stdin = child.stdin.take().expect("open psql stdin");
    if let Err(error) = stdin.write_all(script.as_bytes()) {
        eprintln!("psql closed stdin before the full script was written: {error}");
    }
    drop(stdin);
    child.wait_with_output().expect("run psql")
}

fn success(url: &str, script: &str) -> String {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "psql failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("psql stdout is utf-8")
}

fn app_preamble() -> &'static str {
    "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; \
     SET LOCAL app.tenant = 't1';"
}

fn wait_for_pg_sleep(url: &str, application_name: &str) {
    for _ in 0..100 {
        let active = success(
            url,
            &format!(
                "SELECT EXISTS ( \
                   SELECT 1 FROM pg_stat_activity \
                   WHERE application_name = '{application_name}' \
                     AND wait_event = 'PgSleep' \
                 );"
            ),
        );
        if active.trim() == "t" {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "timed out waiting for PostgreSQL session {application_name} to hold its resolution map"
    );
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn run_state_live() {
    let url = std::env::var("WAMN_RUN_STORE_PG_URL")
        .expect("set WAMN_RUN_STORE_PG_URL to the throwaway superuser database");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");

    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             {catalog} {run_state} {run_queue} \
             INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('t1','cat',1,'prod','0.1','draft'); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ('t1', \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
               '0.1',decode('7b7d','hex'),2); \
             INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) \
             VALUES ('t1','cat',1,'[]');"
        ),
    );

    let release = release_caller_sql();
    let terminalize = terminalize_sql();
    let reserved_checkpoint = reserved_checkpoint_sql();
    let complete = complete_sql();
    let materialize_resolutions = wamn_run_state::sql::materialize_run_flow_resolutions_sql();
    let select_resolution_plans = wamn_run_state::sql::select_release_resolution_plans_sql();

    // Resolution persistence stays behind the run-plane execution role. The
    // scenario-author role is read-only, and the function is not PUBLIC.
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT has_table_privilege('wamn_app', \
                    'wamn_run.run_flow_resolutions', 'SELECT'), \
                  'execution role reads resolution evidence'; \
           ASSERT has_table_privilege('wamn_app', \
                    'wamn_run.run_flow_resolutions', 'INSERT'), \
                  'execution role inserts resolution evidence'; \
           ASSERT NOT has_table_privilege('wamn_app', \
                    'wamn_run.run_flow_resolutions', 'UPDATE'), \
                  'execution role cannot rewrite resolution evidence'; \
           ASSERT NOT has_table_privilege('wamn_app', \
                    'wamn_run.run_flow_resolutions', 'DELETE'), \
                  'execution role cannot delete resolution evidence'; \
           ASSERT NOT has_table_privilege('wamn_app', \
                    'wamn_run.run_flow_resolutions', 'TRUNCATE'), \
                  'execution role cannot truncate resolution evidence'; \
           ASSERT has_function_privilege('wamn_app', \
                    'wamn_run.materialize_run_flow_resolutions(text,jsonb)', 'EXECUTE'), \
                  'execution role invokes resolution materialization'; \
           ASSERT ( \
             SELECT array_agg(grantee.rolname ORDER BY grantee.rolname) \
             FROM pg_class AS relation \
             CROSS JOIN LATERAL aclexplode(relation.relacl) AS privilege \
             JOIN pg_roles AS grantee ON grantee.oid = privilege.grantee \
             WHERE relation.oid = 'wamn_run.run_flow_resolutions'::regclass \
               AND privilege.grantee <> relation.relowner \
               AND privilege.privilege_type = 'INSERT' \
           ) = ARRAY['wamn_app']::name[], \
                  'execution role is the only non-owner insert grantee'; \
           ASSERT ( \
             SELECT array_agg(grantee.rolname ORDER BY grantee.rolname) \
             FROM pg_proc AS proc \
             CROSS JOIN LATERAL aclexplode(proc.proacl) AS privilege \
             JOIN pg_roles AS grantee ON grantee.oid = privilege.grantee \
             WHERE proc.oid = \
                     'wamn_run.materialize_run_flow_resolutions(text,jsonb)'::regprocedure \
               AND privilege.grantee <> proc.proowner \
               AND privilege.privilege_type = 'EXECUTE' \
           ) = ARRAY['wamn_app']::name[], \
                  'execution role is the only non-owner execute grantee'; \
           ASSERT has_table_privilege('wamn_scenario_author', \
                    'wamn_run.run_flow_resolutions', 'SELECT'), \
                  'scenario author reads resolution evidence'; \
           ASSERT NOT has_table_privilege('wamn_scenario_author', \
                    'wamn_run.run_flow_resolutions', 'INSERT'), \
                  'scenario author cannot insert resolution evidence'; \
           ASSERT NOT has_table_privilege('wamn_scenario_author', \
                    'wamn_run.run_flow_resolutions', 'UPDATE'), \
                  'scenario author cannot rewrite resolution evidence'; \
           ASSERT NOT has_table_privilege('wamn_scenario_author', \
                    'wamn_run.run_flow_resolutions', 'DELETE'), \
                  'scenario author cannot delete resolution evidence'; \
           ASSERT NOT has_table_privilege('wamn_scenario_author', \
                    'wamn_run.run_flow_resolutions', 'TRUNCATE'), \
                  'scenario author cannot truncate resolution evidence'; \
           ASSERT NOT has_function_privilege('wamn_scenario_author', \
                    'wamn_run.materialize_run_flow_resolutions(text,jsonb)', 'EXECUTE'), \
                  'scenario author cannot invoke resolution materialization'; \
           ASSERT NOT EXISTS ( \
             SELECT 1 \
             FROM pg_proc AS proc \
             CROSS JOIN LATERAL aclexplode(proc.proacl) AS privilege \
             WHERE proc.oid = \
                     'wamn_run.materialize_run_flow_resolutions(text,jsonb)'::regprocedure \
               AND privilege.grantee = 0 \
               AND privilege.privilege_type = 'EXECUTE' \
           ), 'resolution materialization is not PUBLIC'; \
         END $$;",
    );

    // The release-bound resolution map is immutable once materialized. A retry
    // with the identical complete map succeeds; incomplete or mixed proposals
    // and pre-existing partial/mixed maps refuse without dropping/recomputing
    // rows; RLS hides other tenants; triggers reject direct mutation.
    success(
        &url,
        "INSERT INTO catalog.execution_bundles \
           (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
         VALUES ('t1', \
           'sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b', \
           '0.1',convert_to('{\"other\":true}','UTF8'),14); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) \
         VALUES ('t1','resolution-1','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running'), \
          ('t1','resolution-forged','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running'), \
          ('t1','resolution-preexisting-incomplete','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running'), \
          ('t1','resolution-preexisting-mixed','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running');",
    );
    let resolution_script = format!(
        "{} PREPARE resolution_stmt (text,text) AS {}; \
         CREATE TEMP TABLE first_resolution AS \
           EXECUTE resolution_stmt('resolution-1', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}},\
               {{\"flow-id\":\"g\",\
                 \"execution-bundle-hash\":\"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b\",\
                 \"source-artifact-hash\":\"sha256:artifact-g\"}}]'); \
         CREATE TEMP TABLE retry_resolution AS \
           EXECUTE resolution_stmt('resolution-1', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}},\
               {{\"flow-id\":\"g\",\
                 \"execution-bundle-hash\":\"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b\",\
                 \"source-artifact-hash\":\"sha256:artifact-g\"}}]'); \
         CREATE TEMP TABLE incomplete_resolution AS \
           EXECUTE resolution_stmt('resolution-1', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}}]'); \
         CREATE TEMP TABLE mixed_resolution AS \
           EXECUTE resolution_stmt('resolution-1', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}},\
               {{\"flow-id\":\"g\",\
                 \"execution-bundle-hash\":\"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b\",\
                 \"source-artifact-hash\":\"sha256:artifact-g\"}}]'); \
         INSERT INTO run_flow_resolutions \
             (tenant_id,run_id,flow_id,execution_bundle_hash,source_artifact_hash) \
         VALUES (current_setting('app.tenant', true),'resolution-forged','f',\
             'sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b',\
             'sha256:forged-artifact'), \
            (current_setting('app.tenant', true),'resolution-preexisting-incomplete','f',\
             'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',\
             'sha256:artifact-root'), \
            (current_setting('app.tenant', true),'resolution-preexisting-mixed','f',\
             'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',\
             'sha256:artifact-root'), \
            (current_setting('app.tenant', true),'resolution-preexisting-mixed','g',\
             'sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b',\
             'sha256:mixed-artifact'); \
         CREATE TEMP TABLE forged_resolution AS \
           EXECUTE resolution_stmt('resolution-forged', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}}]'); \
         CREATE TEMP TABLE preexisting_incomplete AS \
           EXECUTE resolution_stmt('resolution-preexisting-incomplete', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}},\
               {{\"flow-id\":\"g\",\
                 \"execution-bundle-hash\":\"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b\",\
                 \"source-artifact-hash\":\"sha256:artifact-g\"}}]'); \
         CREATE TEMP TABLE preexisting_mixed AS \
           EXECUTE resolution_stmt('resolution-preexisting-mixed', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}},\
               {{\"flow-id\":\"g\",\
                 \"execution-bundle-hash\":\"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b\",\
                 \"source-artifact-hash\":\"sha256:artifact-g\"}}]'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM first_resolution) = 'resolved', \
                  'first resolution inserts'; \
           ASSERT (SELECT result_code FROM retry_resolution) = 'resolved', \
                  'identical retry verifies'; \
           ASSERT (SELECT fail_kind FROM incomplete_resolution) = 'foreign-revision', \
                  'incomplete existing map refuses'; \
           ASSERT (SELECT fail_kind FROM mixed_resolution) = 'foreign-revision', \
                  'mixed existing map refuses'; \
           ASSERT (SELECT fail_kind FROM forged_resolution) = 'foreign-revision', \
                  'forged preexisting map refuses'; \
           ASSERT (SELECT fail_kind FROM preexisting_incomplete) = 'foreign-revision', \
                  'preexisting one-row map refuses expected two-row retry'; \
           ASSERT (SELECT fail_kind FROM preexisting_mixed) = 'foreign-revision', \
                  'preexisting mixed map refuses expected retry'; \
           ASSERT (SELECT count(*) FROM run_flow_resolutions WHERE run_id='resolution-1') = 2, \
                  'refusals do not recompute or drop rows'; \
         END $$; COMMIT;",
        app_preamble(),
        materialize_resolutions
    );
    success(&url, &resolution_script);

    // Two callers can reach first materialization before either has committed.
    // An identical retry resolves after the winner commits. A competing map
    // that omits one winning flow and proposes another refuses, and its
    // speculative non-conflicting row is rolled back instead of forming a
    // union with the winning map.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) \
         VALUES ('t1','resolution-concurrent-identical','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running'), \
          ('t1','resolution-concurrent-different','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running');",
    );
    let complete_resolution_map = r#"[{"flow-id":"f","execution-bundle-hash":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","source-artifact-hash":"sha256:artifact-root"},{"flow-id":"g","execution-bundle-hash":"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b","source-artifact-hash":"sha256:artifact-g"}]"#;
    let different_resolution_map = r#"[{"flow-id":"f","execution-bundle-hash":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","source-artifact-hash":"sha256:artifact-root"},{"flow-id":"h","execution-bundle-hash":"sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b","source-artifact-hash":"sha256:artifact-h"}]"#;

    let identical_winner_url = url.clone();
    let identical_winner_script = format!(
        "{} PREPARE resolution_stmt (text,text) AS {}; \
         CREATE TEMP TABLE winner_resolution AS \
           EXECUTE resolution_stmt('resolution-concurrent-identical', '{}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM winner_resolution) = 'resolved', \
                  'concurrent identical winner resolves'; \
         END $$; \
         SET LOCAL application_name = 'resolution-identical-winner'; \
         SELECT pg_sleep(2); COMMIT;",
        app_preamble(),
        materialize_resolutions,
        complete_resolution_map
    );
    let identical_winner =
        thread::spawn(move || success(&identical_winner_url, &identical_winner_script));
    wait_for_pg_sleep(&url, "resolution-identical-winner");
    let identical_retry_script = format!(
        "{} PREPARE resolution_stmt (text,text) AS {}; \
         CREATE TEMP TABLE retry_resolution AS \
           EXECUTE resolution_stmt('resolution-concurrent-identical', '{}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM retry_resolution) = 'resolved', \
                  'concurrent identical retry resolves'; \
           ASSERT (SELECT count(*) FROM run_flow_resolutions \
                    WHERE run_id = 'resolution-concurrent-identical') = 2, \
                  'identical retry persists one complete map'; \
         END $$; COMMIT;",
        app_preamble(),
        materialize_resolutions,
        complete_resolution_map
    );
    success(&url, &identical_retry_script);
    identical_winner.join().expect("identical winner thread");

    let different_winner_url = url.clone();
    let different_winner_script = format!(
        "{} PREPARE resolution_stmt (text,text) AS {}; \
         CREATE TEMP TABLE winner_resolution AS \
           EXECUTE resolution_stmt('resolution-concurrent-different', '{}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM winner_resolution) = 'resolved', \
                  'concurrent complete-map winner resolves'; \
         END $$; \
         SET LOCAL application_name = 'resolution-different-winner'; \
         SELECT pg_sleep(2); COMMIT;",
        app_preamble(),
        materialize_resolutions,
        complete_resolution_map
    );
    let different_winner =
        thread::spawn(move || success(&different_winner_url, &different_winner_script));
    wait_for_pg_sleep(&url, "resolution-different-winner");
    let different_retry_script = format!(
        "{} PREPARE resolution_stmt (text,text) AS {}; \
         CREATE TEMP TABLE retry_resolution AS \
           EXECUTE resolution_stmt('resolution-concurrent-different', '{}'); \
         DO $$ BEGIN \
           ASSERT (SELECT fail_kind FROM retry_resolution) = 'foreign-revision', \
                  'concurrent different retry refuses'; \
           ASSERT (SELECT count(*) FROM run_flow_resolutions \
                    WHERE run_id = 'resolution-concurrent-different') = 2, \
                  'different retry leaves one complete map'; \
           ASSERT EXISTS ( \
             SELECT 1 FROM run_flow_resolutions \
             WHERE run_id = 'resolution-concurrent-different' \
               AND flow_id = 'f' \
               AND execution_bundle_hash = \
                   'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a' \
               AND source_artifact_hash = 'sha256:artifact-root' \
           ), 'winning root resolution remains exact'; \
           ASSERT EXISTS ( \
             SELECT 1 FROM run_flow_resolutions \
             WHERE run_id = 'resolution-concurrent-different' \
               AND flow_id = 'g' \
               AND execution_bundle_hash = \
                   'sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b' \
               AND source_artifact_hash = 'sha256:artifact-g' \
           ), 'winning reachable resolution remains exact'; \
           ASSERT NOT EXISTS ( \
             SELECT 1 FROM run_flow_resolutions \
             WHERE run_id = 'resolution-concurrent-different' AND flow_id = 'h' \
           ), 'different retry rolls back its non-conflicting row'; \
         END $$; COMMIT;",
        app_preamble(),
        materialize_resolutions,
        different_resolution_map
    );
    success(&url, &different_retry_script);
    different_winner.join().expect("different winner thread");

    let hidden_script = format!(
        "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; \
         SET LOCAL app.tenant = 't2'; \
         PREPARE resolution_stmt (text,text) AS {}; \
         CREATE TEMP TABLE hidden AS \
           EXECUTE resolution_stmt('resolution-1', \
             '[{{\"flow-id\":\"f\",\
                 \"execution-bundle-hash\":\"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\",\
                 \"source-artifact-hash\":\"sha256:artifact-root\"}}]'); \
         DO $$ BEGIN \
           ASSERT (SELECT fail_kind FROM hidden) = 'unresolvable-name', \
                  'other tenant cannot see the run or map'; \
         END $$; COMMIT;",
        materialize_resolutions
    );
    success(&url, &hidden_script);

    // A run pinned to catalog version 1 keeps reading version 1 release members
    // after the catalog head moves to version 2.
    success(
        &url,
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ('t1','cat-republish',1,'prod','0.1','superseded'), \
                ('t1','cat-republish',2,'prod','0.1','applied'); \
         INSERT INTO catalog.flow_artifacts \
           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) \
         VALUES ('t1','f',1,'0.1','{}','graph-v1','artifact-v1'), \
                ('t1','f',2,'0.1','{}','graph-v2','artifact-v2'); \
         INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json) \
         VALUES ('t1','cat-republish',1, \
                 '[{\"flow-id\":\"f\",\"flow-version\":1,\"artifact-hash\":\"artifact-v1\"}]'), \
                ('t1','cat-republish',2, \
                 '[{\"flow-id\":\"f\",\"flow-version\":2,\"artifact-hash\":\"artifact-v2\"}]'); \
         INSERT INTO catalog.release_flows \
           (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
         VALUES ('t1','cat-republish',1,'f',1, \
                 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'), \
                ('t1','cat-republish',2,'f',2, \
                 'sha256:dbcbb05208bbb6cc1181867c1498e0e60dcbe6e4097bbada64fe1408114fa81b'); \
         INSERT INTO catalog.catalog_heads \
           (tenant_id,catalog_id,environment,applied_catalog_version) \
         VALUES ('t1','cat-republish','prod',1); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) \
         VALUES ('t1','resolution-republish','f',1,'cat-republish',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running'); \
         UPDATE catalog.catalog_heads \
            SET applied_catalog_version = 2 \
          WHERE tenant_id='t1' AND catalog_id='cat-republish' AND environment='prod';",
    );
    let republish_script = format!(
        "{} PREPARE plan_stmt (text) AS {}; \
         CREATE TEMP TABLE pinned_plan AS EXECUTE plan_stmt('resolution-republish'); \
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM pinned_plan) = 1, \
                  'one pinned release member remains visible'; \
           ASSERT (SELECT execution_bundle_hash FROM pinned_plan) = \
                  'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
                  'head movement does not change the run pinned release map'; \
           ASSERT (SELECT artifact_hash FROM pinned_plan) = 'artifact-v1', \
                  'source artifact stays pinned to version one'; \
         END $$; COMMIT;",
        app_preamble(),
        select_resolution_plans
    );
    success(&url, &republish_script);

    success(
        &url,
        "DO $$ BEGIN \
           BEGIN \
             UPDATE wamn_run.run_flow_resolutions \
                SET source_artifact_hash = 'sha256:changed' \
              WHERE tenant_id='t1' AND run_id='resolution-1' AND flow_id='f'; \
             RAISE EXCEPTION 'resolution update unexpectedly succeeded'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             ASSERT SQLERRM = 'run-flow-resolution-immutable', 'update immutable message'; \
           END; \
           BEGIN \
             DELETE FROM wamn_run.run_flow_resolutions \
              WHERE tenant_id='t1' AND run_id='resolution-1' AND flow_id='f'; \
             RAISE EXCEPTION 'resolution delete unexpectedly succeeded'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             ASSERT SQLERRM = 'run-flow-resolution-immutable', 'delete immutable message'; \
           END; \
         END $$;",
    );

    // A stale entry boundary cannot insert its synthetic node checkpoint. The
    // same statement succeeds under the current generation.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) \
         VALUES ('t1','entry-1','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','entry-1','worker-entry',now()+interval '1 minute',8);",
    );
    let entry_script = format!(
        "{} PREPARE entry_stmt \
           (text,text,text,bigint,text,int,int,text,text,text,text,bigint,text,text,boolean,bigint) \
           AS {}; \
         CREATE TEMP TABLE stale_entry AS \
           EXECUTE entry_stmt('entry-1','entry-1','worker-entry',7, \
                              'in',0,0,'main','{{\"tick\":1}}','{{\"tick\":1}}', \
                              NULL,NULL,NULL,'full',false,30000); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM stale_entry) = 'fence-lost', \
                  'stale entry generation loses'; \
           ASSERT NOT EXISTS (SELECT FROM node_runs WHERE run_id='entry-1'), \
                  'stale generation writes no entry checkpoint'; \
         END $$; \
         CREATE TEMP TABLE current_entry AS \
           EXECUTE entry_stmt('entry-1','entry-1','worker-entry',8, \
                              'in',0,0,'main','{{\"tick\":1}}','{{\"tick\":1}}', \
                              NULL,NULL,NULL,'full',false,30000); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM current_entry) = 'recorded', \
                  'current entry generation records'; \
           ASSERT EXISTS (SELECT FROM node_runs WHERE run_id='entry-1' AND local_node_id='in'), \
                  'current generation writes entry checkpoint'; \
         END $$; COMMIT;",
        app_preamble(),
        reserved_checkpoint
    );
    success(&url, &entry_script);

    // Positive caller release, duplicate replay, then terminalization. A
    // transition after terminal state returns its typed refusal.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment, \
            execution_bundle_hash, attachment_id, status) \
         VALUES ('t1', 'release-1', 'f', 1, 'cat', 1, 'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'http-a', 'running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id, run_id, lease_owner, lease_expires_at, lease_generation) \
         VALUES ('t1', 'release-1', 'worker-a', now() + interval '1 minute', 1);",
    );
    let release_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE released AS \
           EXECUTE release_stmt('release-1','release-1','worker-a',1, \
                                'responded','{{\"ok\":true}}',200,'respond','sha256:one'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM released) = 'released', 'caller released'; \
           ASSERT (SELECT caller_outcome_kind FROM runs WHERE run_id='release-1') = 'responded', \
                  'caller outcome persisted'; \
         END $$; COMMIT;",
        app_preamble(),
        release
    );
    success(&url, &release_script);

    let replay_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE replayed AS \
           EXECUTE release_stmt('release-1','release-1','worker-a',1, \
                                'responded','{{\"ok\":true}}',200,'respond','sha256:one'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM replayed) = 'already-released', 'duplicate is replay'; \
           ASSERT (SELECT outcome_kind FROM replayed) = 'responded', 'stored kind returned'; \
         END $$; COMMIT;",
        app_preamble(),
        release
    );
    success(&url, &replay_script);

    let terminal_script = format!(
        "{} PREPARE terminal_stmt \
           (text,text,text,bigint,text,text,text) AS {}; \
         CREATE TEMP TABLE terminal AS \
           EXECUTE terminal_stmt('release-1','release-1','worker-a',1, \
                                 'completed','frontier-exhausted','{{\"done\":true}}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM terminal) = 'terminalized', 'run terminalized'; \
           ASSERT (SELECT status FROM runs WHERE run_id='release-1') = 'completed', \
                  'terminal status persisted'; \
           ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='release-1'), \
                  'queue row removed atomically'; \
         END $$; COMMIT;",
        app_preamble(),
        terminalize
    );
    success(&url, &terminal_script);

    // An attachment identifies admission provenance, not necessarily a waiting
    // caller. Cron and event runs terminalize naturally; request sources must
    // release their caller first.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,attachment_id,status,trigger_source) VALUES \
           ('t1','terminal-cron','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'cron-a','running','cron'), \
           ('t1','terminal-event','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'event-a','running','event'), \
           ('t1','terminal-http-open','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'http-open','running','http'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,attachment_id,status,trigger_source, \
            caller_outcome_kind,caller_outcome_json,caller_http_status,caller_release_node_id, \
            caller_outcome_hash,caller_released_at) VALUES \
           ('t1','terminal-http-released','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'http-released','running','http', \
            'responded','{}',200,'respond','sha256:released',now()); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
           ('t1','terminal-cron','worker-source',now()+interval '1 minute',1), \
           ('t1','terminal-event','worker-source',now()+interval '1 minute',1), \
           ('t1','terminal-http-open','worker-source',now()+interval '1 minute',1), \
           ('t1','terminal-http-released','worker-source',now()+interval '1 minute',1);",
    );
    let source_terminal_script = format!(
        "{} PREPARE terminal_stmt \
           (text,text,text,bigint,text,text,text) AS {}; \
         CREATE TEMP TABLE cron_terminal AS \
           EXECUTE terminal_stmt('terminal-cron','terminal-cron','worker-source',1, \
                                 'completed','frontier-exhausted','{{}}'); \
         CREATE TEMP TABLE event_terminal AS \
           EXECUTE terminal_stmt('terminal-event','terminal-event','worker-source',1, \
                                 'completed','frontier-exhausted','{{}}'); \
         CREATE TEMP TABLE http_open_terminal AS \
           EXECUTE terminal_stmt('terminal-http-open','terminal-http-open','worker-source',1, \
                                 'completed','frontier-exhausted','{{}}'); \
         CREATE TEMP TABLE http_released_terminal AS \
           EXECUTE terminal_stmt('terminal-http-released','terminal-http-released', \
                                 'worker-source',1,'completed','frontier-exhausted','{{}}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM cron_terminal) = 'terminalized', \
                  'attached cron has no caller to release'; \
           ASSERT (SELECT result_code FROM event_terminal) = 'terminalized', \
                  'attached event has no caller to release'; \
           ASSERT (SELECT result_code FROM http_open_terminal) = 'caller-unreleased', \
                  'HTTP request must release its caller'; \
           ASSERT (SELECT status FROM runs WHERE run_id='terminal-http-open') = 'running', \
                  'caller refusal leaves the request running'; \
           ASSERT EXISTS (SELECT FROM run_queue WHERE run_id='terminal-http-open'), \
                  'caller refusal leaves the request queued'; \
           ASSERT (SELECT result_code FROM http_released_terminal) = 'terminalized', \
                  'released HTTP request terminalizes'; \
         END $$; COMMIT;",
        app_preamble(),
        terminalize
    );
    success(&url, &source_terminal_script);

    let post_terminal_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE refused AS \
           EXECUTE release_stmt('release-1','release-1','worker-a',1, \
                                'failed','{{\"error\":{{}}}}',500,NULL,'sha256:two'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused) = 'run-terminal', \
                  'post-terminal transition is typed'; \
         END $$; COMMIT;",
        app_preamble(),
        release
    );
    success(&url, &post_terminal_script);

    // Actual lock race: the new claimant increments generation and holds the
    // queue row while the stale worker enters release_caller. The stale statement
    // resumes after commit and must return fence-lost without caller mutation.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,attachment_id,status) \
         VALUES ('t1','race-1','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'http-race','running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','race-1','stale-worker',now()+interval '1 minute',7);",
    );
    let race_url = url.clone();
    let winner = thread::spawn(move || {
        success(
            &race_url,
            "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; \
             SET LOCAL app.tenant='t1'; \
             UPDATE run_queue SET lease_owner='winner', lease_generation=lease_generation+1, \
                    lease_expires_at=now()+interval '1 minute' \
              WHERE run_id='race-1'; \
             SELECT pg_sleep(1); COMMIT;",
        )
    });
    thread::sleep(Duration::from_millis(200));
    let stale_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         CREATE TEMP TABLE stale AS \
           EXECUTE release_stmt('race-1','race-1','stale-worker',7, \
                                'responded','{{\"bad\":true}}',200,'respond','sha256:stale'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM stale) = 'fence-lost', 'stale generation loses'; \
           ASSERT (SELECT caller_released_at FROM runs WHERE run_id='race-1') IS NULL, \
                  'FenceLost writes no caller state'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='race-1') = 'winner', \
                  'FenceLost writes no queue state'; \
         END $$; COMMIT;",
        app_preamble(),
        release
    );
    success(&url, &stale_script);
    winner.join().expect("winner thread");

    // Attempt completion writes the attempt and checkpoint together, and a
    // duplicate completion receives a typed refusal.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status,state_json) \
         VALUES ('t1','attempt-1','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'running','{\"step\":0}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','attempt-1','worker-c',now()+interval '1 minute',4); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,occurrence,seq,status) \
         VALUES ('t1','attempt-1',0, \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'effect',0,1,'started');",
    );
    let complete_script = format!(
        "{} PREPARE complete_stmt \
           (text,text,text,bigint,text,int,text,text,text) AS {}; \
         CREATE TEMP TABLE completed AS \
           EXECUTE complete_stmt('attempt-1','attempt-1','worker-c',4, \
                                 'effect',0,'main','{{\"value\":1}}','{{\"step\":1}}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM completed) = 'completed', 'attempt completed'; \
           ASSERT (SELECT status FROM node_runs WHERE run_id='attempt-1') = 'success', \
                  'attempt output persisted'; \
           ASSERT (SELECT state_json FROM runs WHERE run_id='attempt-1') = '{{\"step\":1}}', \
                  'checkpoint persisted'; \
         END $$; COMMIT;",
        app_preamble(),
        complete
    );
    success(&url, &complete_script);

    // The named unique constraint is the stable input to the public
    // InvocationAdmissionRefusal mapping.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status) VALUES \
           ('t1','admit-1','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'running'), \
           ('t1','admit-2','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'running'); \
         INSERT INTO wamn_run.invocation_admissions \
           (tenant_id,catalog_id,environment,attachment_id,definition_hash, \
            principal_digest,client_key_digest,client_request_fingerprint, \
            admitted_catalog_version,admitted_flow_version,run_id,expires_at) \
         VALUES ('t1','cat','prod','http-a','def-1','principal','client','fp-1', \
                 1,1,'admit-1',now()+interval '1 day'); \
         DO $$ DECLARE constraint_name text; BEGIN \
           BEGIN \
             INSERT INTO wamn_run.invocation_admissions \
               (tenant_id,catalog_id,environment,attachment_id,definition_hash, \
                principal_digest,client_key_digest,client_request_fingerprint, \
                admitted_catalog_version,admitted_flow_version,run_id,expires_at) \
             VALUES ('t1','cat','prod','http-a','def-1','principal','client','fp-2', \
                     1,1,'admit-2',now()+interval '1 day'); \
             ASSERT false, 'duplicate admission unexpectedly inserted'; \
           EXCEPTION WHEN unique_violation THEN \
             GET STACKED DIAGNOSTICS constraint_name = CONSTRAINT_NAME; \
             ASSERT constraint_name = 'invocation_admissions_identity', \
                    'stable typed duplicate constraint'; \
           END; \
         END $$;",
    );

    // Named fault: caller release + terminal run + queue deletion all execute,
    // then the transaction aborts. None may survive.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,attachment_id,status) \
         VALUES ('t1','fault-1','f',1,'cat',1,'prod', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'http-fault','running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','fault-1','worker-f',now()+interval '1 minute',9);",
    );
    let fault_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         PREPARE terminal_stmt \
           (text,text,text,bigint,text,text,text) AS {}; \
         EXECUTE release_stmt('fault-1','fault-1','worker-f',9, \
                              'failed','{{\"error\":{{\"code\":\"boom\"}}}}',500,NULL,'sha256:fault'); \
         EXECUTE terminal_stmt('fault-1','fault-1','worker-f',9, \
                               'failed','node-failed','null'); \
         SELECT 1/0; COMMIT;",
        app_preamble(),
        release,
        terminalize
    );
    let fault = psql(&url, &fault_script);
    assert!(
        !fault.status.success(),
        "injected transaction fault must fail"
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT status FROM wamn_run.runs WHERE run_id='fault-1') = 'running', \
                  'fault rolled back run terminal state'; \
           ASSERT (SELECT caller_released_at FROM wamn_run.runs WHERE run_id='fault-1') IS NULL, \
                  'fault rolled back caller state'; \
           ASSERT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id='fault-1' \
                          AND lease_owner='worker-f' AND lease_generation=9), \
                  'fault rolled back queue deletion'; \
         END $$;",
    );
}
