//! Ignored PostgreSQL gate for cancellation/deadline races.

use std::process::{Command, Output};
use std::thread;

use wamn_flow::canonical_json_sha256;
use wamn_run_state::cancellation::{cancellation_sweep_sql, request_cancellation_sql};
use wamn_run_state::transitions::complete_attempt_success_sql;

fn psql(url: &str, script: &str) -> Output {
    Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", url, "-c", script])
        .output()
        .expect("run psql")
}

fn success(url: &str, script: &str) {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "psql failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn app_preamble() -> &'static str {
    "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; \
     SET LOCAL app.tenant = 't1';"
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn cancellation_live() {
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
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
                   NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; {run_state} {run_queue}"
        ),
    );

    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,attachment_id,status,run_deadline_at) \
         VALUES ('t1','live-attempt','f',1,'http-a','running',now()+interval '5 minutes'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,parent_run_id,parent_node_id, \
            parent_occurrence,run_deadline_at) \
         VALUES ('t1','live-child','f',1,'running','live-attempt','invoke',0, \
                 now()+interval '5 minutes'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','live-attempt','worker-a',now()+interval '1 minute',4); \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id,lease_generation) \
         VALUES ('t1','live-child',1); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,run_id,node_id,occurrence,seq,status,recovery_class, \
            attempt_started_at,attempt_dispatched_at,attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','live-attempt','effect',0,1,'started','replay', \
                 now(),now(),now()+interval '1 minute','sha256:input');",
    );

    let request = request_cancellation_sql();
    let sweep = cancellation_sweep_sql();
    let complete = complete_attempt_success_sql();
    let completion_hash = canonical_json_sha256(&serde_json::json!({
        "error": {
            "code": "caller-disconnect",
            "flow-id": "f",
            "flow-version": 1,
            "run-id": "live-attempt"
        }
    }));
    success(
        &url,
        &format!(
            "{} \
             PREPARE request_stmt (text,text,bigint) AS {}; \
             CREATE TEMP TABLE stale AS \
               EXECUTE request_stmt('live-attempt','caller-disconnect',3); \
             CREATE TEMP TABLE requested AS \
               EXECUTE request_stmt('live-attempt','caller-disconnect',4); \
             PREPARE sweep_stmt (bigint) AS {}; \
             CREATE TEMP TABLE deferred AS EXECUTE sweep_stmt(8); \
             DO $$ BEGIN \
               ASSERT (SELECT result_code FROM stale) = 'stale-generation', \
                      'stale generation refused'; \
               ASSERT (SELECT result_code FROM requested) = 'requested', \
                      'request persisted'; \
               ASSERT NOT EXISTS (SELECT FROM deferred), \
                      'live attempt defers seizure'; \
               ASSERT (SELECT lease_generation FROM run_queue \
                        WHERE run_id='live-attempt') = 4, \
                      'deferred request does not seize'; \
               ASSERT (SELECT status FROM runs WHERE run_id='live-attempt') = 'running', \
                      'deferred run stays live'; \
             END $$; \
             PREPARE complete_stmt \
               (text,text,text,bigint,text,int,text,text,text,text,bigint,text,text,boolean,text,bigint) \
               AS {}; \
             CREATE TEMP TABLE completed AS \
               EXECUTE complete_stmt('live-attempt','live-attempt','worker-a',4, \
                 'effect',0,'main','{{\"ok\":true}}','{{\"in\":true}}',NULL,NULL,NULL, \
                 'full',false,'{{\"cancel-check\":true}}',30000); \
             CREATE TEMP TABLE propagated_child AS EXECUTE sweep_stmt(8); \
             DO $$ BEGIN \
               ASSERT (SELECT result_code FROM completed) = 'cancelled', \
                      'attempt completion applies request'; \
               ASSERT (SELECT status FROM runs WHERE run_id='live-attempt') = 'cancelled', \
                      'run cancelled at attempt completion'; \
               ASSERT (SELECT status FROM node_runs WHERE run_id='live-attempt') = 'success', \
                      'the in-flight effect records exactly one completion'; \
               ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='live-attempt'), \
                      'terminal cancellation dequeues'; \
               ASSERT (SELECT caller_outcome_kind FROM runs \
                        WHERE run_id='live-attempt') = 'cancelled', \
                      'unreleased waiter gets durable cancellation'; \
               ASSERT (SELECT caller_outcome_json FROM runs \
                        WHERE run_id='live-attempt') = \
                        '{{\"error\":{{\"code\":\"caller-disconnect\", \
                         \"flow-id\":\"f\",\"flow-version\":1, \
                         \"run-id\":\"live-attempt\"}}}}'::jsonb, \
                      'completion persists the exact durable failure envelope'; \
               ASSERT (SELECT caller_http_status FROM runs \
                        WHERE run_id='live-attempt') = 499, \
                      'completion persists cancellation HTTP status'; \
               ASSERT (SELECT caller_outcome_hash FROM runs \
                        WHERE run_id='live-attempt') = '{}', \
                      'completion hash matches the Rust RFC8785 canonicalizer'; \
               ASSERT (SELECT notification_count FROM completed) = 1, \
                      'one completion terminalization drives one notification'; \
               ASSERT (SELECT status FROM runs WHERE run_id='live-child') = 'cancelled', \
                      'completion-time cancellation propagates to its child'; \
               ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='live-child'), \
                      'propagated child terminalizes through the sweep'; \
             END $$; COMMIT;",
            app_preamble(),
            request,
            sweep,
            complete,
            completion_hash
        ),
    );

    // Cancellation and attempt completion take the run lock in the same order.
    // Whichever commits first, the effect completion is recorded once and the
    // durable request is eventually the sole terminal verdict.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,attachment_id,status,run_deadline_at) \
         VALUES ('t1','completion-race','f',1,'http-a','running',now()+interval '5 minutes'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
         VALUES ('t1','completion-race','worker-race',now()+interval '1 minute',7); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,run_id,node_id,occurrence,seq,status,recovery_class, \
            attempt_started_at,attempt_dispatched_at,attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','completion-race','effect',0,1,'started','replay', \
                 now(),now(),now()+interval '1 minute','sha256:race');",
    );
    let request_script = format!(
        "{} PREPARE request_stmt (text,text,bigint) AS {}; \
         EXECUTE request_stmt('completion-race','caller-disconnect',7); \
         COMMIT;",
        app_preamble(),
        request
    );
    let complete_script = format!(
        "{} PREPARE complete_stmt \
           (text,text,text,bigint,text,int,text,text,text,text,bigint,text,text,boolean,text,bigint) \
           AS {}; \
         EXECUTE complete_stmt('completion-race','completion-race','worker-race',7, \
           'effect',0,'main','{{\"ok\":true}}','{{\"in\":true}}',NULL,NULL,NULL, \
           'full',false,'{{\"race\":true}}',30000); COMMIT;",
        app_preamble(),
        complete
    );
    let request_url = url.clone();
    let request_thread = thread::spawn(move || success(&request_url, &request_script));
    let complete_url = url.clone();
    let complete_thread = thread::spawn(move || success(&complete_url, &complete_script));
    request_thread.join().expect("cancellation request thread");
    complete_thread.join().expect("attempt completion thread");
    success(
        &url,
        &format!(
            "{} PREPARE sweep_stmt (bigint) AS {}; \
             CREATE TEMP TABLE race_sweep AS EXECUTE sweep_stmt(8); \
             CREATE TEMP TABLE race_sweep_again AS EXECUTE sweep_stmt(8); \
             DO $$ BEGIN \
               ASSERT (SELECT status FROM runs WHERE run_id='completion-race') = 'cancelled', \
                      'first durable cancellation eventually wins the race'; \
               ASSERT (SELECT status FROM node_runs WHERE run_id='completion-race') = 'success', \
                      'racing completion records exactly once'; \
               ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='completion-race'), \
                      'race terminalization dequeues once'; \
               ASSERT NOT EXISTS (SELECT FROM race_sweep_again WHERE run_id='completion-race'), \
                      'terminal race emits no second sweep outcome'; \
             END $$; COMMIT;",
            app_preamble(),
            sweep
        ),
    );

    // The bounded sweep terminalizes only one root in the first batch, then
    // reaches the propagated child on the next pass. A released caller cannot
    // retroactively request cancellation.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,run_deadline_at) VALUES \
           ('t1','deadline-parent','f',1,'running',now()-interval '1 second'), \
           ('t1','other-expired','f',1,'running',now()-interval '1 second'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,parent_run_id,parent_node_id, \
            parent_occurrence,run_deadline_at) \
         VALUES ('t1','deadline-child','f',1,'running','deadline-parent','invoke',0, \
                 now()+interval '1 hour'); \
         INSERT INTO wamn_run.run_queue \
           (tenant_id,run_id,lease_generation) VALUES \
           ('t1','deadline-parent',1),('t1','deadline-child',1),('t1','other-expired',1); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,caller_outcome_kind, \
            caller_outcome_json,caller_http_status,caller_release_node_id, \
            caller_outcome_hash,caller_released_at,run_deadline_at) \
         VALUES ('t1','released','f',1,'running','responded','{}',201,'respond', \
                 'sha256:released',now(),now()-interval '1 second'); \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id,lease_generation) \
         VALUES ('t1','released',9);",
    );
    let sweep_hash = canonical_json_sha256(&serde_json::json!({
        "error": {
            "code": "run-deadline",
            "flow-id": "f",
            "flow-version": 1,
            "run-id": "deadline-parent"
        }
    }));
    success(
        &url,
        &format!(
            "{} PREPARE sweep_stmt (bigint) AS {}; \
             PREPARE request_stmt (text,text,bigint) AS {}; \
             CREATE TEMP TABLE first_batch AS EXECUTE sweep_stmt(1); \
             CREATE TEMP TABLE released_refusal AS \
               EXECUTE request_stmt('released','caller-disconnect',9); \
             DO $$ BEGIN \
               ASSERT (SELECT count(*) FROM first_batch) = 1, \
                      'sweep obeys the batch bound'; \
               ASSERT (SELECT notification_count FROM first_batch) = 1, \
                      'one swept terminalization drives one notification'; \
               ASSERT (SELECT caller_outcome_json FROM runs \
                        WHERE run_id='deadline-parent') = \
                        '{{\"error\":{{\"code\":\"run-deadline\", \
                         \"flow-id\":\"f\",\"flow-version\":1, \
                         \"run-id\":\"deadline-parent\"}}}}'::jsonb, \
                      'sweep persists the exact durable failure envelope'; \
               ASSERT (SELECT caller_http_status FROM runs \
                        WHERE run_id='deadline-parent') = 499, \
                      'sweep persists cancellation HTTP status'; \
               ASSERT (SELECT caller_outcome_hash FROM runs \
                        WHERE run_id='deadline-parent') = '{}', \
                      'sweep hash matches the Rust RFC8785 canonicalizer'; \
               ASSERT (SELECT result_code FROM released_refusal) = 'caller-released', \
                      'released boundary refuses cancellation'; \
             END $$; \
             CREATE TEMP TABLE second_batch AS EXECUTE sweep_stmt(8); \
             CREATE TEMP TABLE third_batch AS EXECUTE sweep_stmt(8); \
             DO $$ BEGIN \
               ASSERT (SELECT count(*) FROM runs \
                        WHERE run_id IN ('deadline-parent','deadline-child','other-expired') \
                          AND status='cancelled') = 3, \
                      'deadline cancellation propagates and makes bounded progress'; \
               ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='deadline-parent'), \
                      'terminal parent dequeues once'; \
               ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='deadline-child'), \
                      'terminal child dequeues once'; \
               ASSERT NOT EXISTS (SELECT FROM run_queue WHERE run_id='other-expired'), \
                      'other terminal run dequeues once'; \
               ASSERT (SELECT status FROM runs WHERE run_id='released') = 'cancelled', \
                      'elapsed run deadline still terminalizes released work'; \
               ASSERT (SELECT caller_outcome_kind FROM runs WHERE run_id='released') \
                        = 'responded' \
                  AND (SELECT caller_outcome_json FROM runs WHERE run_id='released') = '{{}}' \
                  AND (SELECT caller_http_status FROM runs WHERE run_id='released') = 201 \
                  AND (SELECT caller_outcome_hash FROM runs WHERE run_id='released') \
                        = 'sha256:released', \
                      'post-release caller state stays byte-for-byte untouched'; \
             END $$; COMMIT;",
            app_preamble(),
            sweep,
            request,
            sweep_hash
        ),
    );
}
