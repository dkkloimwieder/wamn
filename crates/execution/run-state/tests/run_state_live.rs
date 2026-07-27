//! Ignored live gate for the fenced run-state transitions.

use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use wamn_run_state::transitions::{complete_sql, park_sql, release_caller_sql, terminalize_sql};

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
             DROP SCHEMA IF EXISTS wamn_run CASCADE; {run_state} {run_queue}"
        ),
    );

    let release = release_caller_sql();
    let terminalize = terminalize_sql();
    let park = park_sql();
    let complete = complete_sql();

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
         INSERT INTO wamn_run.node_runs \
           (tenant_id,run_id,node_id,occurrence,seq,status,recovery_class, \
            attempt_started_at,attempt_deadline_at,attempt_input_ref,attempt_key) \
         VALUES ('t1','attempt-1','effect',0,1,'started','never-replay', \
                 now(),now()+interval '30 seconds','sha256:input','attempt-key');",
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
