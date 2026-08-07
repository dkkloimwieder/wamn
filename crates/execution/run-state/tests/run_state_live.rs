//! Ignored live gate for the fenced run-state transitions.

use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use wamn_run_state::transitions::{
    begin_attempt_sql, complete_sql, mark_attempt_dispatched_sql, park_sql, release_caller_sql,
    reserved_checkpoint_sql, terminalize_sql,
};

fn psql(url: &str, script: &str) -> Output {
    Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", url, "-c", script])
        .output()
        .expect("run psql")
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
             END $$; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             {catalog} {run_state} {run_queue}"
        ),
    );

    let release = release_caller_sql();
    let terminalize = terminalize_sql();
    let reserved_checkpoint = reserved_checkpoint_sql();
    let park = park_sql();
    let complete = complete_sql();
    let begin_attempt = begin_attempt_sql();
    let mark_attempt = mark_attempt_dispatched_sql();

    // A stale entry boundary cannot insert its synthetic node checkpoint. The
    // same statement succeeds under the current generation.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status) \
         VALUES ('t1','entry-1','f',1,'running'); \
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
           ASSERT EXISTS (SELECT FROM node_runs WHERE run_id='entry-1' AND node_id='in'), \
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
           (tenant_id, run_id, flow_id, flow_version, attachment_id, status) \
         VALUES ('t1', 'release-1', 'f', 1, 'http-a', 'running'); \
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
           (text,text,text,bigint,text,text,text,text) AS {}; \
         CREATE TEMP TABLE terminal AS \
           EXECUTE terminal_stmt('release-1','release-1','worker-a',1, \
                                 'completed','frontier-exhausted',NULL,'{{\"done\":true}}'); \
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
           (tenant_id,run_id,flow_id,flow_version,attachment_id,status,trigger_source) VALUES \
           ('t1','terminal-cron','f',1,'cron-a','running','cron'), \
           ('t1','terminal-event','f',1,'event-a','running','event'), \
           ('t1','terminal-http-open','f',1,'http-open','running','http'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,attachment_id,status,trigger_source, \
            caller_outcome_kind,caller_outcome_json,caller_http_status,caller_release_node_id, \
            caller_outcome_hash,caller_released_at) VALUES \
           ('t1','terminal-http-released','f',1,'http-released','running','http', \
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
           (text,text,text,bigint,text,text,text,text) AS {}; \
         CREATE TEMP TABLE cron_terminal AS \
           EXECUTE terminal_stmt('terminal-cron','terminal-cron','worker-source',1, \
                                 'completed','frontier-exhausted',NULL,'{{}}'); \
         CREATE TEMP TABLE event_terminal AS \
           EXECUTE terminal_stmt('terminal-event','terminal-event','worker-source',1, \
                                 'completed','frontier-exhausted',NULL,'{{}}'); \
         CREATE TEMP TABLE http_open_terminal AS \
           EXECUTE terminal_stmt('terminal-http-open','terminal-http-open','worker-source',1, \
                                 'completed','frontier-exhausted',NULL,'{{}}'); \
         CREATE TEMP TABLE http_released_terminal AS \
           EXECUTE terminal_stmt('terminal-http-released','terminal-http-released', \
                                 'worker-source',1,'completed','frontier-exhausted',NULL,'{{}}'); \
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

    // Cross-run authority is rejected before either run or queue state changes.
    success(
        &url,
        "INSERT INTO wamn_run.runs (tenant_id,run_id,flow_id,flow_version,status,state_json) VALUES \
           ('t1','cross-a','f',1,'running','{\"before\":true}'), \
           ('t1','cross-b','f',1,'running','{\"before\":true}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
           ('t1','cross-a','same-worker',now()+interval '1 minute',3), \
           ('t1','cross-b','same-worker',now()+interval '1 minute',3);",
    );
    let cross_script = format!(
        "{} PREPARE park_stmt (text,text,text,bigint,text,timestamptz) AS {}; \
         CREATE TEMP TABLE crossed AS \
           EXECUTE park_stmt('cross-a','cross-b','same-worker',3, \
                             '{{\"after\":true}}',now()+interval '1 hour'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM crossed) = 'cross-run-authority', \
                  'cross-run authority is typed'; \
           ASSERT (SELECT state_json FROM runs WHERE run_id='cross-a') = '{{\"before\":true}}', \
                  'cross-run refusal does not write the run'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='cross-a') = 'same-worker', \
                  'cross-run refusal does not release the queue'; \
         END $$; COMMIT;",
        app_preamble(),
        park
    );
    success(&url, &cross_script);

    // Actual lock race: the new claimant increments generation and holds the
    // queue row while the stale worker enters release_caller. The stale statement
    // resumes after commit and must return fence-lost without caller mutation.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,attachment_id,status) \
         VALUES ('t1','race-1','f',1,'http-race','running'); \
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
           (tenant_id,run_id,flow_id,flow_version,status,state_json) \
         VALUES ('t1','attempt-1','f',1,'running','{\"step\":0}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','attempt-1','worker-c',now()+interval '1 minute',4); \
         INSERT INTO wamn_run.effect_attempts \
           (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
            selected_recovery_class,recovery_class,generation_fact_kind, \
            attempt_started_at,attempt_deadline_at,attempt_input_ref,attempt_key) \
         VALUES ('t1','10000000-0000-0000-0000-000000000001','attempt-1','effect',0,1,0, \
                 'never-replay','never-replay','not-required', \
                 now(),now()+interval '30 seconds','sha256:input','attempt-key'); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status) \
         VALUES ('t1','10000000-0000-0000-0000-000000000001', \
                 'attempt-1','effect',0,1,'started'); \
         INSERT INTO wamn_run.effect_attempt_dispatches (tenant_id,attempt_id) \
         VALUES ('t1','10000000-0000-0000-0000-000000000001');",
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
           ASSERT (SELECT count(*) FROM effect_attempt_outcomes AS o \
                     JOIN effect_attempts AS e \
                       ON e.tenant_id=o.tenant_id AND e.attempt_id=o.attempt_id \
                    WHERE e.run_id='attempt-1' AND o.outcome_status='success') = 1, \
                  'attempt completion appends one immutable outcome'; \
           ASSERT (SELECT state_json FROM runs WHERE run_id='attempt-1') = '{{\"step\":1}}', \
                  'checkpoint persisted'; \
         END $$; COMMIT;",
        app_preamble(),
        complete
    );
    success(&url, &complete_script);

    // T-NR: fault each durable seam independently.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status) VALUES \
           ('t1','nr-rollback','f',1,'running'), \
           ('t1','nr-before-send','f',1,'running'), \
           ('t1','nr-never','f',1,'running'), \
           ('t1','nr-replay','f',1,'running'), \
           ('t1','nr-keyed','f',1,'running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
           ('t1','nr-rollback','worker-nr',now()+interval '1 minute',1), \
           ('t1','nr-before-send','worker-nr',now()+interval '1 minute',1), \
           ('t1','nr-never','worker-nr',now()+interval '1 minute',1), \
           ('t1','nr-replay','worker-nr',now()+interval '1 minute',1), \
           ('t1','nr-keyed','worker-nr',now()+interval '1 minute',1);",
    );
    let rollback_script = format!(
        "{} PREPARE begin_stmt \
           (text,text,text,bigint,text,int,int,text,text,text,text,text,text,text,bigint,text) AS {}; \
         EXECUTE begin_stmt('nr-rollback','nr-rollback','worker-nr',1, \
                            'effect',0,1,'never-replay','never-replay','not-required', \
                            NULL,NULL,'sha256:input',NULL,30000,NULL); \
         ROLLBACK;",
        app_preamble(),
        begin_attempt
    );
    success(&url, &rollback_script);
    let nr_script = format!(
        "{} PREPARE begin_stmt \
           (text,text,text,bigint,text,int,int,text,text,text,text,text,text,text,bigint,text) AS {}; \
         PREPARE mark_stmt (text,text,text,bigint,text,int,bigint) AS {}; \
         CREATE TEMP TABLE rollback_recovery AS \
           EXECUTE begin_stmt('nr-rollback','nr-rollback','worker-nr',1, \
                              'effect',0,1,'never-replay','never-replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE before_first AS \
           EXECUTE begin_stmt('nr-before-send','nr-before-send','worker-nr',1, \
                              'effect',0,1,'never-replay','never-replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE before_retarget AS \
           EXECUTE begin_stmt('nr-before-send','nr-before-send','worker-nr',1, \
                              'effect',0,1,'replay','replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE before_recovery AS \
           EXECUTE begin_stmt('nr-before-send','nr-before-send','worker-nr',1, \
                              'effect',0,1,'never-replay','never-replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE before_marked AS \
           EXECUTE mark_stmt('nr-before-send','nr-before-send','worker-nr',1, \
                             'effect',0,30000); \
         CREATE TEMP TABLE nr_first AS \
           EXECUTE begin_stmt('nr-never','nr-never','worker-nr',1, \
                              'effect',0,1,'never-replay','never-replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE nr_marked AS \
           EXECUTE mark_stmt('nr-never','nr-never','worker-nr',1,'effect',0,30000); \
         CREATE TEMP TABLE nr_recovery AS \
           EXECUTE begin_stmt('nr-never','nr-never','worker-nr',1, \
                              'effect',0,1,'never-replay','never-replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE pure_first AS \
           EXECUTE begin_stmt('nr-replay','nr-replay','worker-nr',1, \
                              'pure',0,1,'replay','replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE pure_marked AS \
           EXECUTE mark_stmt('nr-replay','nr-replay','worker-nr',1,'pure',0,30000); \
         CREATE TEMP TABLE pure_recovery AS \
           EXECUTE begin_stmt('nr-replay','nr-replay','worker-nr',1, \
                              'pure',0,1,'replay','replay','not-required', \
                              NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE keyed_first AS \
           EXECUTE begin_stmt('nr-keyed','nr-keyed','worker-nr',1, \
                              'keyed',0,1,'idempotent-with-key','idempotent-with-key', \
                              'not-required',NULL,NULL,'sha256:input','key-1',30000,NULL); \
         CREATE TEMP TABLE keyed_prepared_missing AS \
           EXECUTE begin_stmt('nr-keyed','nr-keyed','worker-nr',1, \
                              'keyed',0,1,'idempotent-with-key','idempotent-with-key', \
                              'not-required',NULL,NULL,'sha256:input',NULL,30000,NULL); \
         CREATE TEMP TABLE keyed_marked AS \
           EXECUTE mark_stmt('nr-keyed','nr-keyed','worker-nr',1,'keyed',0,30000); \
         CREATE TEMP TABLE keyed_missing AS \
           EXECUTE begin_stmt('nr-keyed','nr-keyed','worker-nr',1, \
                              'keyed',0,1,'idempotent-with-key','idempotent-with-key', \
                              'not-required',NULL,NULL,'sha256:input',NULL,30000,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM rollback_recovery) = 'started', \
                  'crash before intent commit leaves the occurrence resumable'; \
           ASSERT (SELECT result_code FROM before_recovery) = 'started', \
                  'committed intent before send remains resumable'; \
           ASSERT (SELECT result_code FROM before_retarget) = 'effect-uncertain', \
                  'recovery cannot retarget the pinned admission facts'; \
           ASSERT (SELECT result_code FROM before_marked) = 'marked', \
                  'resumed occurrence crosses the durable send boundary once'; \
           ASSERT (SELECT result_code FROM nr_first) = 'started', 'intent commits before send'; \
           ASSERT (SELECT result_code FROM nr_marked) = 'marked', \
                  'send boundary commits immediately before dispatch'; \
           ASSERT (SELECT result_code FROM nr_recovery) = 'effect-uncertain', \
                  'sent-but-unrecorded never-replay does not redispatch'; \
           ASSERT (SELECT result_code FROM pure_recovery) = 'redispatch', \
                  'pure recovery redispatches'; \
           ASSERT (SELECT e.attempt_index FROM node_runs AS n \
                     JOIN effect_attempts AS e \
                       ON e.tenant_id=n.tenant_id \
                      AND e.attempt_id=n.current_effect_attempt_id \
                    WHERE n.run_id='nr-replay') = 1, \
                  'authorized redispatch increments the durable attempt'; \
           ASSERT (SELECT e.attempt_index FROM node_runs AS n \
                     JOIN effect_attempts AS e \
                       ON e.tenant_id=n.tenant_id \
                      AND e.attempt_id=n.current_effect_attempt_id \
                    WHERE n.run_id='nr-never') = 0, \
                  'effect-uncertain performs no second attempt'; \
           ASSERT (SELECT e.selected_recovery_class = 'never-replay' \
                          AND e.recovery_class = 'never-replay' \
                          AND e.generation_fact_kind = 'not-required' \
                          AND e.connection_generation IS NULL \
                          AND e.credential_generation IS NULL \
                     FROM node_runs AS n \
                     JOIN effect_attempts AS e \
                       ON e.tenant_id=n.tenant_id \
                      AND e.attempt_id=n.current_effect_attempt_id \
                    WHERE n.run_id='nr-never'), \
                  'attempt ledger records selected/effective class and explicit no-generation facts'; \
           ASSERT (SELECT result_code FROM keyed_prepared_missing) = 'missing-attempt-key', \
                  'prepared keyed attempt still requires the original key'; \
           ASSERT (SELECT result_code FROM keyed_missing) = 'missing-attempt-key', \
                  'keyed recovery refuses without the original key'; \
           ASSERT (SELECT count(*) FROM node_runs WHERE run_id LIKE 'nr-%') = 5, \
                  'recovery does not create a second occurrence'; \
         END $$; COMMIT;",
        app_preamble(),
        begin_attempt,
        mark_attempt
    );
    success(&url, &nr_script);

    // Portable HTTP admission resolves and records generation facts in the
    // same INSERT that creates the write-ahead attempt intent. The caller
    // supplies only the requirement name; generation parameters remain NULL.
    success(
        &url,
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ('t1','c-http',1,'dev','0.1','applied'); \
         INSERT INTO catalog.flow_artifacts \
           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash, \
            interface_bundle_json,interface_bundle_hash,component_digests) VALUES \
           ('t1','http-flow',1,'0.1', \
            '{\"nodes\":[{\"id\":\"notify\",\"type\":\"http-request\",\"connection\":\"manager\"}]}', \
            'graph-http','artifact-http', \
            '[{\"interface\":{\"node-type\":\"http-request\",\"connection-requirements\":[{\"requirement-type\":\"http\",\"contract\":\"wamn:connection/http@0.1.0\"}]}}]', \
            'interfaces-http','[]'); \
         INSERT INTO catalog.release_manifests \
           (tenant_id,catalog_id,catalog_version,members_json) VALUES \
           ('t1','c-http',1,'[{\"flow-id\":\"http-flow\",\"flow-version\":1,\"artifact-hash\":\"artifact-http\"}]'); \
         INSERT INTO catalog.release_flows \
           (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
         VALUES ('t1','c-http',1,'http-flow',1); \
         INSERT INTO catalog.connection_requirements \
           (tenant_id,artifact_hash,requirement_name,requirement_json,requirement_hash) VALUES \
           ('t1','artifact-http','manager', \
            '{\"descriptor\":{\"requirement-type\":\"http\",\"contract\":\"wamn:connection/http@0.1.0\"}}', \
            'requirement-http'); \
         INSERT INTO catalog.connection_instances \
           (tenant_id,environment,instance_id,requirement_type,contract) VALUES \
           ('t1','dev','manager-dev','http','wamn:connection/http@0.1.0'); \
         INSERT INTO catalog.connection_generations \
           (tenant_id,environment,instance_id,generation,definition_json,definition_hash, \
            credential_set_handle) VALUES \
           ('t1','dev','manager-dev',1,'{}','definition-http','manager-credential-v1'); \
         UPDATE catalog.connection_instances \
            SET active_generation=1,revision=1,updated_at=now()+interval '1 microsecond' \
          WHERE tenant_id='t1' AND environment='dev' AND instance_id='manager-dev'; \
         INSERT INTO catalog.connection_bindings \
           (tenant_id,catalog_id,catalog_version,artifact_hash,requirement_name,environment, \
            instance_id,binding_status,validation_status,validation_hash) VALUES \
           ('t1','c-http',1,'artifact-http','manager','dev','manager-dev','active','valid', \
            'binding-http'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment,status, \
            invocation_context) VALUES \
           ('t1','http-intent','http-flow',1,'c-http',1,'dev','running', \
            '{\"principal\":{\"artifact-digest\":\"artifact-http\"}}'), \
           ('t1','http-wrong-node','http-flow',1,'c-http',1,'dev','running', \
            '{\"principal\":{\"artifact-digest\":\"artifact-http\"}}'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
           ('t1','http-intent','worker-http',now()+interval '1 minute',1), \
           ('t1','http-wrong-node','worker-http',now()+interval '1 minute',1);",
    );
    let connection_intent_script = format!(
        "{} PREPARE begin_http \
           (text,text,text,bigint,text,int,int,text,text,text,text,text,text,text,bigint,text) AS {}; \
         PREPARE mark_http (text,text,text,bigint,text,int,bigint) AS {}; \
         CREATE TEMP TABLE http_intent AS \
           EXECUTE begin_http('http-intent','http-intent','worker-http',1, \
                              'notify',0,7,'never-replay','never-replay','attested', \
                              NULL,NULL,'sha256:operation','http-intent:notify:0',30000,'manager'); \
         CREATE TEMP TABLE wrong_node AS \
           EXECUTE begin_http('http-wrong-node','http-wrong-node','worker-http',1, \
                              'other',0,7,'never-replay','never-replay','attested', \
                              NULL,NULL,'sha256:operation','http-wrong-node:other:0',30000,'manager'); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM http_intent) = 'started', \
                  'portable HTTP intent inserts before send'; \
           ASSERT (SELECT e.generation_fact_kind = 'attested' \
                          AND e.connection_generation = 'manager-dev:1' \
                          AND e.credential_generation = 'manager-credential-v1' \
                          AND e.attempt_input_ref = 'sha256:operation' \
                          AND e.attempt_key = 'http-intent:notify:0' \
                          AND d.attempt_id IS NULL \
                     FROM node_runs AS n \
                     JOIN effect_attempts AS e \
                       ON e.tenant_id=n.tenant_id \
                      AND e.attempt_id=n.current_effect_attempt_id \
                     LEFT JOIN effect_attempt_dispatches AS d \
                       ON d.tenant_id=e.tenant_id AND d.attempt_id=e.attempt_id \
                    WHERE n.run_id='http-intent' AND n.node_id='notify'), \
                  'one intent insert records generation, fingerprint, and stable key'; \
           ASSERT (SELECT result_code FROM wrong_node) = 'node-not-permitted', \
                  'node without the admitted connection cannot create intent'; \
           ASSERT NOT EXISTS (SELECT FROM node_runs WHERE run_id='http-wrong-node'), \
                  'refused connection reaches no durable send intent'; \
         END $$; \
         CREATE TEMP TABLE http_marked AS \
           EXECUTE mark_http('http-intent','http-intent','worker-http',1,'notify',0,30000); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM http_marked) = 'marked', \
                  'durable intent crosses the send boundary only after insertion'; \
         END $$; COMMIT;",
        app_preamble(),
        begin_attempt,
        mark_attempt
    );
    success(&url, &connection_intent_script);

    // The final host-side send marker refuses both expired authority windows
    // and performs no dispatch-state write.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,run_deadline_at) VALUES \
           ('t1','deadline-attempt','f',1,'running',now()+interval '1 minute'), \
           ('t1','deadline-run','f',1,'running',now()-interval '1 second'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
           ('t1','deadline-attempt','worker-deadline',now()+interval '1 minute',1), \
           ('t1','deadline-run','worker-deadline',now()+interval '1 minute',1); \
         INSERT INTO wamn_run.effect_attempts \
           (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index, \
            selected_recovery_class,recovery_class,generation_fact_kind, \
            attempt_started_at,attempt_deadline_at,attempt_input_ref) VALUES \
           ('t1','10000000-0000-0000-0000-000000000002', \
            'deadline-attempt','effect',0,1,0,'replay','replay','not-required', \
            now()-interval '2 seconds',now()-interval '1 second','sha256:expired'), \
           ('t1','10000000-0000-0000-0000-000000000003', \
            'deadline-run','effect',0,1,0,'replay','replay','not-required', \
            now()-interval '2 seconds',now()+interval '1 minute','sha256:run-expired'); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,current_effect_attempt_id,run_id,node_id,occurrence,seq,status) VALUES \
           ('t1','10000000-0000-0000-0000-000000000002', \
            'deadline-attempt','effect',0,1,'started'), \
           ('t1','10000000-0000-0000-0000-000000000003', \
            'deadline-run','effect',0,1,'started');",
    );
    let deadline_script = format!(
        "{} PREPARE mark_stmt (text,text,text,bigint,text,int,bigint) AS {}; \
         CREATE TEMP TABLE attempt_expired AS \
           EXECUTE mark_stmt('deadline-attempt','deadline-attempt','worker-deadline',1, \
                             'effect',0,30000); \
         CREATE TEMP TABLE run_expired AS \
           EXECUTE mark_stmt('deadline-run','deadline-run','worker-deadline',1, \
                             'effect',0,30000); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM attempt_expired) = 'attempt-deadline-expired', \
                  'expired attempt is refused at the send boundary'; \
           ASSERT (SELECT result_code FROM run_expired) = 'run-deadline-expired', \
                  'expired invocation budget is refused at the send boundary'; \
           ASSERT NOT EXISTS (SELECT FROM effect_attempt_dispatches AS d \
                               JOIN effect_attempts AS e \
                                 ON e.tenant_id=d.tenant_id \
                                AND e.attempt_id=d.attempt_id \
                              WHERE e.run_id LIKE 'deadline-%'), \
                  'expired authority performs no dispatch marker write'; \
         END $$; COMMIT;",
        app_preamble(),
        mark_attempt
    );
    success(&url, &deadline_script);

    // The named unique constraint is the stable input to the public
    // InvocationAdmissionRefusal mapping.
    success(
        &url,
        "INSERT INTO wamn_run.runs (tenant_id,run_id,flow_id,flow_version,status) \
         VALUES ('t1','admit-1','f',1,'running'),('t1','admit-2','f',1,'running'); \
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
           (tenant_id,run_id,flow_id,flow_version,attachment_id,status) \
         VALUES ('t1','fault-1','f',1,'http-fault','running'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','fault-1','worker-f',now()+interval '1 minute',9);",
    );
    let fault_script = format!(
        "{} PREPARE release_stmt \
           (text,text,text,bigint,text,text,int,text,text) AS {}; \
         PREPARE terminal_stmt \
           (text,text,text,bigint,text,text,text,text) AS {}; \
         EXECUTE release_stmt('fault-1','fault-1','worker-f',9, \
                              'failed','{{\"error\":{{\"code\":\"boom\"}}}}',500,NULL,'sha256:fault'); \
         EXECUTE terminal_stmt('fault-1','fault-1','worker-f',9, \
                               'failed','node-failed',NULL,'null'); \
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
