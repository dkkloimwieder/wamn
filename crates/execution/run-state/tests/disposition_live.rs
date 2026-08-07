//! Ignored PostgreSQL proof for the effect-disposition continuation protocol.

use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use wamn_run_state::disposition::{
    park_effect_uncertain_sql, platform_break_glass_bulk_sql, project_bulk_sql, project_single_sql,
    select_current_resolution_sql, select_run_dispositions_sql,
};
use wamn_run_state::transitions::{begin_attempt_sql, complete_attempt_success_sql};

fn psql(url: &str, script: &str) -> Output {
    Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", url, "-c", script])
        .output()
        .expect("run psql")
}

fn spawn_psql(url: &str, script: &str) -> Child {
    Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", url, "-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql")
}

fn wait_for_advisory_lock(url: &str, key: i64) {
    let probe = format!(
        "WITH acquired AS (SELECT pg_try_advisory_lock({key}) AS ok) \
         SELECT CASE WHEN ok THEN NOT pg_advisory_unlock({key}) ELSE true END \
           FROM acquired"
    );
    for _ in 0..100 {
        if success(url, &probe).trim() == "t" {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("concurrent disposition never reached the held-lock barrier");
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

fn failure(url: &str, script: &str, expected: &str) {
    let output = psql(url, script);
    assert!(
        !output.status.success(),
        "psql unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "expected {expected:?}\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn app_preamble() -> &'static str {
    "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; \
     SET LOCAL app.tenant = 't1';"
}

fn admin_preamble() -> &'static str {
    "BEGIN ISOLATION LEVEL SERIALIZABLE; SET LOCAL search_path TO wamn_run; \
     SET LOCAL app.tenant = 't1';"
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn disposition_live() {
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
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
                    NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             {catalog} {run_state} {run_queue}"
        ),
    );

    // Start from a sent `never-replay` attempt whose completion is unknown.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,state_json) \
         VALUES ('t1','disp-1','f',1,'running', \
                 '{\"cursor\":{\"x\":1},\"context\":{\"old\":true}}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,available_at,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','disp-1',now(),'worker-1',now()+interval '1 minute',1); \
         INSERT INTO wamn_run.effect_attempts \
           (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
            selected_recovery_class,recovery_class,generation_fact_kind, \
            verified_author_principal,verified_publisher_principal, \
            attempt_started_at,attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','00000000-0000-0000-0000-000000000042','disp-1','effect',0,0,0, \
                 'never-replay','never-replay','not-required','principal:author', \
                 'principal:publisher',now(),now()+interval '1 minute','sha256:input'); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status) \
         VALUES ('t1','00000000-0000-0000-0000-000000000042','disp-1','effect',0,0, \
                 'started'); \
         INSERT INTO wamn_run.effect_attempt_dispatches (tenant_id,attempt_id) \
         VALUES ('t1','00000000-0000-0000-0000-000000000042');",
    );

    let park = park_effect_uncertain_sql();

    // The app role cannot invoke automatic park without the host-injected
    // runner identity, even when it guesses a live lease owner correctly.
    let unauthenticated_park = format!(
        "{} PREPARE park_stmt (text,text,text,bigint,text,int,text) AS {}; \
         CREATE TEMP TABLE refused AS EXECUTE park_stmt( \
           'disp-1','disp-1','worker-1',1,'effect',0,'{{}}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused) = 'executor-auth-required'; \
           ASSERT NOT EXISTS (SELECT FROM effect_disposition_requests); \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='disp-1') = 'worker-1'; \
         END $$; \
         PREPARE view_stmt(text) AS {}; \
         CREATE TEMP TABLE pending_view AS EXECUTE view_stmt('disp-1'); \
         DO $$ BEGIN \
           ASSERT (SELECT disposition_state FROM pending_view) = 'pending'; \
         END $$; COMMIT;",
        app_preamble(),
        park,
        select_run_dispositions_sql()
    );
    success(&url, &unauthenticated_park);

    // Even if a stale deployment accidentally retained INSERT grants, an
    // ordinary database session cannot bypass the trusted append boundary.
    success(
        &url,
        "GRANT INSERT ON wamn_run.effect_disposition_requests, \
                         wamn_run.effect_dispositions TO wamn_app;",
    );
    failure(
        &url,
        "SET SESSION AUTHORIZATION wamn_app; \
         SET search_path TO wamn_run; SET app.tenant = 't1'; \
         CREATE TEMP TABLE pg_roles (rolname name, rolsuper boolean); \
         INSERT INTO pg_roles VALUES (CURRENT_USER,true); \
         INSERT INTO effect_disposition_requests \
           (tenant_id,action,selection_kind,principal,effective_role,correlation_id) \
         VALUES ('t1','park','single','forged','project-admin','forged');",
        "effect-disposition-append-requires-trusted-adapter",
    );

    // A caller cannot turn the platform statement into a project credential:
    // the SQL derives SESSION_USER and refuses a non-privileged login before
    // any append, even while the stale INSERT grants are present.
    let unprivileged_platform = format!(
        "SET SESSION AUTHORIZATION wamn_app; BEGIN; \
         SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant = 't1'; \
         PREPARE platform_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text,text) AS {}; \
         CREATE TEMP TABLE refused AS EXECUTE platform_stmt( \
           '00000000-0000-0000-0000-000000000042','park',NULL,NULL,NULL,NULL, \
           'forged-platform',NULL,NULL,NULL,NULL,NULL,NULL,'claimed reason'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused) = 'platform-privilege-required'; \
           ASSERT NOT EXISTS (SELECT FROM effect_disposition_requests); \
         END $$; COMMIT;",
        platform_break_glass_single_sql()
    );
    success(&url, &unprivileged_platform);
    success(
        &url,
        "REVOKE INSERT ON wamn_run.effect_disposition_requests, \
                          wamn_run.effect_dispositions FROM wamn_app;",
    );

    // Project/store adapters must opt into the serializable retry contract;
    // weaker isolation is a typed refusal before any append.
    let weak_isolation = format!(
        "BEGIN; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant='t1'; \
         PREPARE project_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text) AS {}; \
         CREATE TEMP TABLE refused AS EXECUTE project_stmt( \
           '00000000-0000-0000-0000-000000000042','park','principal:deployer', \
           'project-deployer',NULL,NULL,'weak-isolation',NULL,NULL,NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused) = 'serializable-required'; \
           ASSERT NOT EXISTS (SELECT FROM effect_disposition_requests); \
         END $$; COMMIT;",
        project_single_sql()
    );
    success(&url, &weak_isolation);

    let first_park = format!(
        "{} SET LOCAL app.runner='worker-1'; \
         PREPARE park_stmt (text,text,text,bigint,text,int,text) AS {}; \
         CREATE TEMP TABLE parked AS \
           EXECUTE park_stmt('disp-1','disp-1','worker-1',1,'effect',0, \
             '{{\"cursor\":{{\"x\":1}},\"context\":{{\"old\":true}}}}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM parked) = 'parked'; \
           ASSERT (SELECT status FROM runs WHERE run_id='disp-1') = 'running'; \
           ASSERT (SELECT available_at = 'infinity'::timestamptz \
                     FROM run_queue WHERE run_id='disp-1'); \
           ASSERT (SELECT lease_owner IS NULL FROM run_queue WHERE run_id='disp-1'); \
           ASSERT (SELECT count(*) FROM effect_attempts WHERE run_id='disp-1') = 1; \
           ASSERT NOT EXISTS (SELECT FROM effect_attempt_outcomes); \
         END $$; \
         PREPARE view_stmt(text) AS {}; \
         CREATE TEMP TABLE parked_view AS EXECUTE view_stmt('disp-1'); \
         DO $$ BEGIN \
           ASSERT (SELECT disposition_state FROM parked_view) = 'parked'; \
         END $$; COMMIT;",
        app_preamble(),
        park,
        select_run_dispositions_sql()
    );
    success(&url, &first_park);

    // Malformed JSON and NULL failure kinds remain typed refusals. They never
    // escape as cast errors or fall through SQL three-valued logic.
    let malformed_resolution = format!(
        "{} PREPARE project_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text) AS {}; \
         CREATE TEMP TABLE bad_json AS EXECUTE project_stmt( \
           '00000000-0000-0000-0000-000000000042','resolve','principal:admin', \
           'project-admin','external-evidence','case:bad-json','bad-json','succeeded', \
           '{{not-json','accepted',NULL,NULL,NULL); \
         CREATE TEMP TABLE null_kind AS EXECUTE project_stmt( \
           '00000000-0000-0000-0000-000000000042','resolve','principal:admin', \
           'project-admin','external-evidence','case:null-kind','null-kind','failed', \
           NULL,NULL,NULL,NULL,'{{\"message\":\"failed\"}}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM bad_json) = 'invalid-success-emission'; \
           ASSERT (SELECT result_code FROM null_kind) = 'invalid-failure-outcome'; \
           ASSERT NOT EXISTS (SELECT FROM effect_disposition_requests \
                               WHERE correlation_id IN ('bad-json','null-kind')); \
         END $$; COMMIT;",
        admin_preamble(),
        project_single_sql()
    );
    success(&url, &malformed_resolution);
    failure(
        &url,
        "BEGIN; SET LOCAL search_path TO wamn_run; SET LOCAL app.tenant='t1'; \
         INSERT INTO effect_disposition_requests \
           (tenant_id,request_id,action,selection_kind,principal,effective_role,basis, \
            evidence_ref,correlation_id) \
         VALUES ('t1','bad00000-0000-0000-0000-000000000001','resolve','single', \
                 'principal:admin','project-admin','external-evidence','case:null','null-hole'); \
         INSERT INTO effect_dispositions \
           (tenant_id,request_id,attempt_id,action,resolution_status,failure_detail) \
         VALUES ('t1','bad00000-0000-0000-0000-000000000001', \
                 '00000000-0000-0000-0000-000000000042','resolve','failed','{}');",
        "effect_dispositions_outcome_check",
    );

    // Release wakes the same run but creates no dispatch permission. Reclaiming
    // it classifies the same immutable attempt uncertain and parks it again.
    let release = project_single_sql();
    let release_then_repark = format!(
        "{} PREPARE release_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text) AS {}; \
         CREATE TEMP TABLE released AS EXECUTE release_stmt( \
           '00000000-0000-0000-0000-000000000042','release','principal:deployer', \
           'project-deployer',NULL,NULL,'release:42',NULL,NULL,NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM released) = 'applied'; \
           ASSERT (SELECT available_at <= now() FROM run_queue WHERE run_id='disp-1'); \
         END $$; \
         PREPARE view_stmt(text) AS {}; \
         CREATE TEMP TABLE released_view AS EXECUTE view_stmt('disp-1'); \
         DO $$ BEGIN \
           ASSERT (SELECT disposition_state FROM released_view) = 'released'; \
         END $$; \
         SET LOCAL ROLE wamn_app; \
         UPDATE run_queue SET lease_owner='worker-2', \
              lease_expires_at=now()+interval '1 minute',lease_generation=2 \
          WHERE run_id='disp-1'; \
         PREPARE resolution_stmt (text,text,int) AS {}; \
         CREATE TEMP TABLE unresolved AS EXECUTE resolution_stmt('disp-1','effect',0); \
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM unresolved) = 0, \
                  'release is not a resolution'; \
         END $$; \
         PREPARE begin_stmt \
           (text,text,text,bigint,text,int,int,text,text,text,text,text,text,text,bigint,text) \
           AS {}; \
         CREATE TEMP TABLE classified AS EXECUTE begin_stmt( \
           'disp-1','disp-1','worker-2',2,'effect',0,0, \
           'never-replay','never-replay','not-required',NULL,NULL, \
           'sha256:input',NULL,30000,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM classified) = 'effect-uncertain'; \
         END $$; \
         SET LOCAL app.runner='worker-2'; \
         PREPARE repark_stmt (text,text,text,bigint,text,int,text) AS {}; \
         CREATE TEMP TABLE reparked AS EXECUTE repark_stmt( \
           'disp-1','disp-1','worker-2',2,'effect',0, \
           '{{\"cursor\":{{\"x\":1}},\"context\":{{\"old\":true}}}}'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM reparked) = 'parked'; \
           ASSERT (SELECT count(*) FROM effect_attempts WHERE run_id='disp-1') = 1, \
                  'release never creates a successor'; \
           ASSERT (SELECT count(*) FROM effect_dispositions \
                    WHERE attempt_id='00000000-0000-0000-0000-000000000042') = 3; \
         END $$; COMMIT;",
        admin_preamble(),
        release,
        select_run_dispositions_sql(),
        select_current_resolution_sql(),
        begin_attempt_sql(),
        park
    );
    success(&url, &release_then_repark);

    // Append a complete asserted success, wake/claim the run, read it through
    // the exact-current-attempt query, then use the ordinary atomic completion
    // transition. The disposition remains immutable audit; the normal outcome
    // fact and node checkpoint become reconstruction authority.
    success(
        &url,
        "INSERT INTO wamn_run.effect_disposition_requests \
           (tenant_id,request_id,action,selection_kind,principal,effective_role,basis, \
            evidence_ref,correlation_id) \
         VALUES ('t1','10000000-0000-0000-0000-000000000042','resolve','single', \
                 'principal:admin','project-admin','external-evidence','case:42','resolve:42'); \
         INSERT INTO wamn_run.effect_dispositions \
           (tenant_id,request_id,attempt_id,action,resolution_status,success_payload, \
            success_port,success_context) \
         VALUES ('t1','10000000-0000-0000-0000-000000000042', \
                 '00000000-0000-0000-0000-000000000042','resolve','succeeded', \
                 '{\"accepted\":true}','accepted','{\"resolved\":true}'); \
         UPDATE wamn_run.run_queue SET available_at=now(),lease_owner='worker-3', \
                lease_expires_at=now()+interval '1 minute',lease_generation=3 \
          WHERE tenant_id='t1' AND run_id='disp-1';",
    );
    let consume = format!(
        "{} PREPARE view_stmt(text) AS {}; \
         CREATE TEMP TABLE resolved_view AS EXECUTE view_stmt('disp-1'); \
         DO $$ BEGIN \
           ASSERT (SELECT disposition_state FROM resolved_view) = 'resolved'; \
         END $$; \
         PREPARE resolution_stmt (text,text,int) AS {}; \
         CREATE TEMP TABLE resolved AS EXECUTE resolution_stmt('disp-1','effect',0); \
         DO $$ BEGIN \
           ASSERT (SELECT resolution_status FROM resolved) = 'succeeded'; \
           ASSERT (SELECT success_port FROM resolved) = 'accepted'; \
           ASSERT (SELECT success_payload::jsonb FROM resolved) = '{{\"accepted\":true}}'; \
         END $$; \
         PREPARE complete_stmt \
           (text,text,text,bigint,text,int,text,text,text,text,bigint,text,text,boolean,text,bigint) \
           AS {}; \
         CREATE TEMP TABLE completed AS EXECUTE complete_stmt( \
           'disp-1','disp-1','worker-3',3,'effect',0,'accepted', \
           '{{\"accepted\":true}}','{{\"request\":1}}',NULL,NULL,NULL,'full',false, \
           '{{\"resolved\":true}}',30000); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM completed) = 'completed'; \
           ASSERT (SELECT status FROM node_runs WHERE run_id='disp-1') = 'success'; \
           ASSERT (SELECT count(*) FROM effect_attempt_outcomes \
                    WHERE attempt_id='00000000-0000-0000-0000-000000000042') = 1; \
           ASSERT (SELECT state_json #> '{{context}}' FROM runs WHERE run_id='disp-1') \
                    = '{{\"resolved\":true}}'; \
           ASSERT (SELECT state_json #> '{{cursor}}' FROM runs WHERE run_id='disp-1') \
                    = '{{\"x\":1}}', 'checkpoint preserves co-resident state'; \
         END $$; COMMIT;",
        app_preamble(),
        select_run_dispositions_sql(),
        select_current_resolution_sql(),
        complete_attempt_success_sql()
    );
    success(&url, &consume);

    // Two bounded groups share one pinned flow/interface. Generation 7 proves
    // project bulk park/release/resolve and deterministic ordinals. Generation
    // 8 proves all-or-nothing separation refusal, then explicit platform
    // break-glass override of that exact same materialized set.
    success(
        &url,
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ('t1','c-bulk',1,'dev','0.1','applied'); \
         INSERT INTO catalog.flow_artifacts \
           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash, \
            interface_bundle_json,interface_bundle_hash,component_digests, \
            verified_author_principal) \
         VALUES ('t1','bulk-flow',1,'0.1', \
                 '{\"nodes\":[{\"id\":\"effect\",\"type\":\"http-request\"}]}', \
                 'graph-bulk','artifact-bulk', \
                 '[{\"interface\":{\"node-type\":\"http-request\", \
                    \"output-ports\":[\"accepted\"]}}]', \
                 'interfaces-bulk','[]','principal:artifact-author'); \
         INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json,verified_publisher_principal) \
         VALUES ('t1','c-bulk',1, \
                 '[{\"flow-id\":\"bulk-flow\",\"flow-version\":1, \
                    \"artifact-hash\":\"artifact-bulk\"}]','principal:release-publisher'); \
         INSERT INTO catalog.release_flows \
           (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
         VALUES ('t1','c-bulk',1,'bulk-flow',1); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            status,state_json,invocation_context) VALUES \
           ('t1','bulk-7a','bulk-flow',1,'c-bulk',1,'dev','running','{}', \
            '{\"principal\":{\"artifact-digest\":\"artifact-bulk\"}}'), \
           ('t1','bulk-7b','bulk-flow',1,'c-bulk',1,'dev','running','{}', \
            '{\"principal\":{\"artifact-digest\":\"artifact-bulk\"}}'), \
           ('t1','bulk-8a','bulk-flow',1,'c-bulk',1,'dev','running','{}', \
            '{\"principal\":{\"artifact-digest\":\"artifact-bulk\"}}'), \
           ('t1','bulk-8b','bulk-flow',1,'c-bulk',1,'dev','running','{}', \
            '{\"principal\":{\"artifact-digest\":\"artifact-bulk\"}}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,available_at,lease_generation) VALUES \
           ('t1','bulk-7a','infinity',1),('t1','bulk-7b','infinity',1), \
           ('t1','bulk-8a','infinity',1),('t1','bulk-8b','infinity',1); \
         INSERT INTO wamn_run.effect_attempts \
           (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
            selected_recovery_class,recovery_class,generation_fact_kind,connection_name, \
            connection_generation,credential_generation,verified_author_principal, \
            verified_publisher_principal,attempt_started_at,attempt_deadline_at, \
            attempt_input_ref) VALUES \
           ('t1','70000000-0000-0000-0000-000000000001','bulk-7a','effect',0,0,0, \
            'never-replay','never-replay','attested','erp','definition:7','credential:7', \
            'principal:author-a','principal:publisher-a','2026-08-07 10:10Z', \
            '2026-08-07 11:10Z','sha256:7a'), \
           ('t1','70000000-0000-0000-0000-000000000002','bulk-7b','effect',0,0,0, \
            'never-replay','never-replay','attested','erp','definition:7','credential:7', \
            'principal:author-b','principal:publisher-b','2026-08-07 10:20Z', \
            '2026-08-07 11:20Z','sha256:7b'), \
           ('t1','80000000-0000-0000-0000-000000000001','bulk-8a','effect',0,0,0, \
            'never-replay','never-replay','attested','erp','definition:8','credential:8', \
            'principal:admin','principal:publisher-a','2026-08-07 10:30Z', \
            '2026-08-07 11:30Z','sha256:8a'), \
           ('t1','80000000-0000-0000-0000-000000000002','bulk-8b','effect',0,0,0, \
            'never-replay','never-replay','attested','erp','definition:8','credential:8', \
            'principal:author-b','principal:publisher-b','2026-08-07 10:40Z', \
            '2026-08-07 11:40Z','sha256:8b'); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status) VALUES \
           ('t1','70000000-0000-0000-0000-000000000001','bulk-7a','effect',0,0,'started'), \
           ('t1','70000000-0000-0000-0000-000000000002','bulk-7b','effect',0,0,'started'), \
           ('t1','80000000-0000-0000-0000-000000000001','bulk-8a','effect',0,0,'started'), \
           ('t1','80000000-0000-0000-0000-000000000002','bulk-8b','effect',0,0,'started'); \
         INSERT INTO wamn_run.effect_attempt_dispatches (tenant_id,attempt_id) VALUES \
           ('t1','70000000-0000-0000-0000-000000000001'), \
           ('t1','70000000-0000-0000-0000-000000000002'), \
           ('t1','80000000-0000-0000-0000-000000000001'), \
           ('t1','80000000-0000-0000-0000-000000000002');",
    );

    // Two serializable sessions overlap on one attempt. The stale park loses
    // with 40001; a fresh-snapshot retry returns the typed terminal
    // disposition refusal and cannot strand the committed resolution.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            status,state_json,invocation_context) \
         VALUES ('t1','race-resolve','bulk-flow',1,'c-bulk',1,'dev','running','{}', \
                 '{\"principal\":{\"artifact-digest\":\"artifact-bulk\"}}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,available_at,lease_generation) \
         VALUES ('t1','race-resolve','infinity',1); \
         INSERT INTO wamn_run.effect_attempts \
           (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
            selected_recovery_class,recovery_class,generation_fact_kind,connection_name, \
            connection_generation,credential_generation,verified_author_principal, \
            verified_publisher_principal,attempt_started_at,attempt_deadline_at, \
            attempt_input_ref) \
         VALUES ('t1','90000000-0000-0000-0000-000000000001','race-resolve','effect',0,0,0, \
                 'never-replay','never-replay','attested','erp','definition:9','credential:9', \
                 'principal:author','principal:publisher','2026-08-07 10:50Z', \
                 '2026-08-07 11:50Z','sha256:race'); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status) \
         VALUES ('t1','90000000-0000-0000-0000-000000000001', \
                 'race-resolve','effect',0,0,'started'); \
         INSERT INTO wamn_run.effect_attempt_dispatches (tenant_id,attempt_id) \
         VALUES ('t1','90000000-0000-0000-0000-000000000001');",
    );
    let race_key = 4_242_042_i64;
    let winner_script = format!(
        "{} PREPARE project_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text) AS {}; \
         CREATE TEMP TABLE winner AS EXECUTE project_stmt( \
           '90000000-0000-0000-0000-000000000001','resolve','principal:admin', \
           'project-admin','external-evidence','case:race','race-winner','succeeded', \
           '{{\"accepted\":true}}','accepted',NULL,NULL,NULL); \
         SELECT pg_advisory_xact_lock({race_key}); SELECT pg_sleep(2); COMMIT;",
        admin_preamble(),
        project_single_sql()
    );
    let winner = spawn_psql(&url, &winner_script);
    wait_for_advisory_lock(&url, race_key);
    let stale_park = format!(
        "{} PREPARE project_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text) AS {}; \
         EXECUTE project_stmt( \
           '90000000-0000-0000-0000-000000000001','park','principal:deployer', \
           'project-deployer',NULL,NULL,'race-stale-park',NULL,NULL,NULL,NULL,NULL,NULL); \
         COMMIT;",
        admin_preamble(),
        project_single_sql()
    );
    failure(&url, &stale_park, "could not serialize access");
    let winner_output = winner.wait_with_output().expect("wait for winning resolve");
    assert!(
        winner_output.status.success(),
        "winning resolve failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&winner_output.stdout),
        String::from_utf8_lossy(&winner_output.stderr)
    );
    let fresh_retry = format!(
        "{} PREPARE project_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text) AS {}; \
         CREATE TEMP TABLE retried AS EXECUTE project_stmt( \
           '90000000-0000-0000-0000-000000000001','park','principal:deployer', \
           'project-deployer',NULL,NULL,'race-retry',NULL,NULL,NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM retried) = 'already-resolved'; \
           ASSERT NOT EXISTS (SELECT FROM effect_disposition_requests \
                               WHERE correlation_id IN ('race-stale-park','race-retry')); \
           ASSERT (SELECT available_at <= now() FROM run_queue \
                    WHERE run_id='race-resolve'); \
         END $$; COMMIT;",
        admin_preamble(),
        project_single_sql()
    );
    success(&url, &fresh_retry);

    let project_bulk = project_bulk_sql();
    let bulk_park = format!(
        "{} PREPARE bulk_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text) \
           AS {}; \
         CREATE TEMP TABLE applied AS EXECUTE bulk_stmt( \
           'erp','definition:7','2026-08-07 10:00Z','2026-08-07 11:00Z', \
           'bulk-flow','park','principal:deployer','project-deployer',NULL,NULL, \
           'bulk-park:7',NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM applied) = 'applied'; \
           ASSERT (SELECT selection_count FROM applied) = 2; \
           ASSERT (SELECT array_agg(d.selection_ordinal ORDER BY d.selection_ordinal) \
                     FROM effect_dispositions d JOIN effect_disposition_requests q \
                       USING (tenant_id,request_id) \
                    WHERE q.correlation_id='bulk-park:7') = ARRAY[0,1]; \
           ASSERT NOT EXISTS (SELECT FROM run_queue \
                    WHERE run_id IN ('bulk-7a','bulk-7b') \
                      AND available_at <> 'infinity'::timestamptz); \
         END $$; COMMIT;",
        admin_preamble(),
        project_bulk
    );
    success(&url, &bulk_park);

    let bulk_release = format!(
        "{} PREPARE bulk_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text) \
           AS {}; \
         CREATE TEMP TABLE applied AS EXECUTE bulk_stmt( \
           'erp','definition:7','2026-08-07 10:00Z','2026-08-07 11:00Z', \
           'bulk-flow','release','principal:deployer','project-deployer',NULL,NULL, \
           'bulk-release:7',NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM applied) = 'applied'; \
           ASSERT (SELECT selection_count FROM applied) = 2; \
           ASSERT NOT EXISTS (SELECT FROM run_queue \
                    WHERE run_id IN ('bulk-7a','bulk-7b') AND available_at > now()); \
         END $$; COMMIT;",
        admin_preamble(),
        project_bulk
    );
    success(&url, &bulk_release);

    let bulk_resolve = format!(
        "{} PREPARE bulk_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text) \
           AS {}; \
         CREATE TEMP TABLE applied AS EXECUTE bulk_stmt( \
           'erp','definition:7','2026-08-07 10:00Z','2026-08-07 11:00Z', \
           'bulk-flow','resolve','principal:admin','project-admin','external-evidence', \
           'case:7','bulk-resolve:7','succeeded','{{\"accepted\":true}}','accepted', \
           NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM applied) = 'applied'; \
           ASSERT (SELECT selection_count FROM applied) = 2; \
           ASSERT (SELECT count(*) FROM effect_dispositions d \
                    JOIN effect_disposition_requests q USING (tenant_id,request_id) \
                   WHERE q.correlation_id='bulk-resolve:7' \
                     AND d.action='resolve' AND d.success_port='accepted') = 2; \
         END $$; COMMIT;",
        admin_preamble(),
        project_bulk
    );
    success(&url, &bulk_resolve);

    let park_8 = format!(
        "{} PREPARE bulk_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text) \
           AS {}; \
         CREATE TEMP TABLE applied AS EXECUTE bulk_stmt( \
           'erp','definition:8','2026-08-07 10:00Z','2026-08-07 11:00Z', \
           'bulk-flow','park','principal:deployer','project-deployer',NULL,NULL, \
           'bulk-park:8',NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
         DO $$ BEGIN ASSERT (SELECT result_code FROM applied) = 'applied'; END $$; \
         COMMIT;",
        admin_preamble(),
        project_bulk
    );
    success(&url, &park_8);

    let project_refusal = format!(
        "{} PREPARE bulk_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text) \
           AS {}; \
         CREATE TEMP TABLE refused AS EXECUTE bulk_stmt( \
           'erp','definition:8','2026-08-07 10:00Z','2026-08-07 11:00Z', \
           'bulk-flow','resolve','principal:admin','project-admin','external-evidence', \
           'case:8','bulk-resolve:8-refused','succeeded','{{\"accepted\":true}}','accepted', \
           NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused) = 'self-resolution'; \
           ASSERT (SELECT selection_count FROM refused) = 2; \
           ASSERT NOT EXISTS (SELECT FROM effect_disposition_requests \
                               WHERE correlation_id='bulk-resolve:8-refused'); \
           ASSERT NOT EXISTS (SELECT FROM effect_dispositions \
                               WHERE attempt_id::text LIKE '80000000-%' AND action='resolve'); \
         END $$; COMMIT;",
        admin_preamble(),
        project_bulk
    );
    success(&url, &project_refusal);

    let platform_bulk = platform_break_glass_bulk_sql();
    let platform_override = format!(
        "{} PREPARE bulk_stmt \
           (text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text,text) \
           AS {}; \
         CREATE TEMP TABLE applied AS EXECUTE bulk_stmt( \
           'erp','definition:8','2026-08-07 10:00Z','2026-08-07 11:00Z', \
           'bulk-flow','resolve',NULL,NULL,'operator-judgment','case:8', \
           'bulk-resolve:8-platform','succeeded','{{\"accepted\":true}}','accepted', \
           NULL,NULL,NULL,'incident commander approved'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM applied) = 'applied'; \
           ASSERT (SELECT selection_count FROM applied) = 2; \
           ASSERT (SELECT count(*) FROM effect_disposition_requests \
                    WHERE correlation_id='bulk-resolve:8-platform' \
                      AND effective_role='platform-admin-break-glass' \
                      AND break_glass_reason='incident commander approved') = 1; \
           ASSERT (SELECT count(*) FROM effect_dispositions \
                    WHERE attempt_id::text LIKE '80000000-%' AND action='resolve') = 2; \
         END $$; COMMIT;",
        admin_preamble(),
        platform_bulk
    );
    success(&url, &platform_override);
}
