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

fn failure(url: &str, script: &str, expected: &str) {
    let output = psql(url, script);
    assert!(
        !output.status.success(),
        "psql unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "expected failure {expected:?}\nstdout:\n{}\nstderr:\n{}",
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

    // Response expiry is the one cancellation class stored as 504. It wins
    // when response/run deadlines are equal, while durable operator requests
    // and run-only deadlines retain 499. A released response deadline is inert,
    // and a live attempt still defers the sweep.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,response_deadline_at,run_deadline_at) \
         VALUES \
           ('t1','response-only','f',1,'running', \
            TIMESTAMPTZ '2000-01-01 00:00:00Z',now()+interval '1 hour'), \
           ('t1','deadline-equal','f',1,'running', \
            TIMESTAMPTZ '2000-01-02 00:00:00Z',TIMESTAMPTZ '2000-01-02 00:00:00Z'), \
           ('t1','run-only','f',1,'running',NULL,TIMESTAMPTZ '2000-01-03 00:00:00Z'), \
           ('t1','operator-request','f',1,'running', \
            TIMESTAMPTZ '2000-01-04 00:00:00Z',TIMESTAMPTZ '2000-01-04 00:00:00Z'), \
           ('t1','response-live-attempt','f',1,'running', \
            TIMESTAMPTZ '2000-01-05 00:00:00Z',now()+interval '1 hour'); \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id,lease_generation) VALUES \
           ('t1','response-only',11),('t1','deadline-equal',12),('t1','run-only',13), \
           ('t1','operator-request',14),('t1','response-live-attempt',15); \
         INSERT INTO wamn_run.node_runs \
           (tenant_id,run_id,node_id,occurrence,seq,status,recovery_class, \
            attempt_started_at,attempt_dispatched_at,attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','response-live-attempt','effect',0,1,'started','replay', \
                 now(),now(),now()+interval '1 minute','sha256:response-live'); \
         INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,caller_outcome_kind, \
            caller_outcome_json,caller_http_status,caller_release_node_id, \
            caller_outcome_hash,caller_released_at,response_deadline_at,run_deadline_at) \
         VALUES ('t1','released-response','f',1,'running','responded','{}',202,'respond', \
                 'sha256:released-response',now(), \
                 TIMESTAMPTZ '2000-01-06 00:00:00Z',now()+interval '1 hour'); \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id,lease_generation) \
         VALUES ('t1','released-response',16);",
    );
    let response_hash = canonical_json_sha256(&serde_json::json!({
        "error": {
            "code": "response-deadline",
            "flow-id": "f",
            "flow-version": 1,
            "run-id": "response-only"
        }
    }));
    success(
        &url,
        &format!(
            "{} PREPARE request_stmt (text,text,bigint) AS {}; \
             CREATE TEMP TABLE operator_requested AS \
               EXECUTE request_stmt('operator-request','operator',14); \
             PREPARE sweep_stmt (bigint) AS {}; \
             CREATE TEMP TABLE boundary_sweep AS EXECUTE sweep_stmt(16); \
             DO $$ BEGIN \
               ASSERT (SELECT result_code FROM operator_requested) = 'requested', \
                      'operator request persisted before the deadline sweep'; \
               ASSERT (SELECT count(*) FROM boundary_sweep) = 4, \
                      'exactly the four eligible boundary rows terminalized'; \
               ASSERT (SELECT min(notification_count) FROM boundary_sweep) = 4 \
                  AND (SELECT max(notification_count) FROM boundary_sweep) = 4, \
                      'four terminalizations emit exactly four waiter notifications'; \
               ASSERT (SELECT caller_outcome_kind FROM runs \
                        WHERE run_id='response-only') = 'cancelled' \
                  AND (SELECT cancel_kind FROM runs WHERE run_id='response-only') \
                        = 'response-deadline' \
                  AND (SELECT terminal_reason FROM runs WHERE run_id='response-only') \
                        = 'response-deadline' \
                  AND (SELECT caller_http_status FROM runs WHERE run_id='response-only') = 504, \
                      'pre-release response deadline stores cancelled/504 and exact code'; \
               ASSERT (SELECT caller_outcome_json FROM runs \
                        WHERE run_id='response-only') = \
                        '{{\"error\":{{\"code\":\"response-deadline\", \
                         \"flow-id\":\"f\",\"flow-version\":1, \
                         \"run-id\":\"response-only\"}}}}'::jsonb, \
                      'response deadline stores the canonical failure envelope'; \
               ASSERT (SELECT caller_outcome_hash FROM runs \
                        WHERE run_id='response-only') = '{}', \
                      'response deadline hash matches the Rust RFC8785 canonicalizer'; \
               ASSERT (SELECT seized_generation FROM boundary_sweep \
                        WHERE run_id='response-only') = 12, \
                      'response terminalization seizes the next generation'; \
               ASSERT (SELECT cancel_kind FROM runs WHERE run_id='deadline-equal') \
                        = 'response-deadline' \
                  AND (SELECT caller_http_status FROM runs WHERE run_id='deadline-equal') = 504, \
                      'response deadline wins the equal run-deadline boundary'; \
               ASSERT (SELECT cancel_kind FROM runs WHERE run_id='run-only') = 'run-deadline' \
                  AND (SELECT caller_http_status FROM runs WHERE run_id='run-only') = 499, \
                      'run-only deadline retains 499'; \
               ASSERT (SELECT cancel_kind FROM runs WHERE run_id='operator-request') = 'operator' \
                  AND (SELECT caller_http_status FROM runs \
                       WHERE run_id='operator-request') = 499, \
                      'durable operator request wins elapsed deadlines and retains 499'; \
               ASSERT (SELECT status FROM runs WHERE run_id='released-response') = 'running' \
                  AND (SELECT caller_outcome_kind FROM runs \
                       WHERE run_id='released-response') = 'responded' \
                  AND (SELECT caller_http_status FROM runs \
                       WHERE run_id='released-response') = 202 \
                  AND (SELECT caller_outcome_hash FROM runs \
                       WHERE run_id='released-response') = 'sha256:released-response' \
                  AND EXISTS (SELECT FROM run_queue WHERE run_id='released-response'), \
                      'released response deadline is inert and outcome remains untouched'; \
               ASSERT (SELECT status FROM runs WHERE run_id='response-live-attempt') = 'running' \
                  AND EXISTS (SELECT FROM run_queue \
                              WHERE run_id='response-live-attempt' \
                                AND lease_generation=15), \
                      'live attempt defers response-deadline seizure'; \
             END $$; COMMIT;",
            app_preamble(),
            request,
            sweep,
            response_hash
        ),
    );

    // A fault immediately before terminalization rolls the queue seizure back.
    // Retrying stores one outcome and reports one transactional notification.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,response_deadline_at,run_deadline_at) \
         VALUES ('t1','fault-before','f',1,'running', \
                 TIMESTAMPTZ '2000-02-01 00:00:00Z',now()+interval '1 hour'); \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id,lease_generation) \
         VALUES ('t1','fault-before',21); \
         CREATE FUNCTION wamn_run.reject_before_terminalization() RETURNS trigger \
           LANGUAGE plpgsql AS $$ BEGIN \
             IF NEW.run_id='fault-before' AND NEW.status='cancelled' THEN \
               RAISE EXCEPTION 'fault-before-terminalization'; \
             END IF; \
             RETURN NEW; \
           END $$; \
         CREATE TRIGGER reject_before_terminalization \
           BEFORE UPDATE ON wamn_run.runs FOR EACH ROW \
           EXECUTE FUNCTION wamn_run.reject_before_terminalization();",
    );
    failure(
        &url,
        &format!(
            "{} PREPARE sweep_stmt (bigint) AS {}; EXECUTE sweep_stmt(16); COMMIT;",
            app_preamble(),
            sweep
        ),
        "fault-before-terminalization",
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT status FROM wamn_run.runs WHERE run_id='fault-before') = 'running' \
              AND (SELECT caller_released_at FROM wamn_run.runs \
                   WHERE run_id='fault-before') IS NULL, \
                  'pre-terminal fault stores no outcome'; \
           ASSERT (SELECT lease_generation FROM wamn_run.run_queue \
                   WHERE run_id='fault-before') = 21, \
                  'pre-terminal fault rolls seizure back'; \
         END $$; \
         DROP TRIGGER reject_before_terminalization ON wamn_run.runs; \
         DROP FUNCTION wamn_run.reject_before_terminalization();",
    );
    success(
        &url,
        &format!(
            "{} PREPARE sweep_stmt (bigint) AS {}; \
             CREATE TEMP TABLE retried AS EXECUTE sweep_stmt(16); \
             DO $$ BEGIN \
               ASSERT (SELECT count(*) FROM retried WHERE run_id='fault-before') = 1 \
                  AND (SELECT notification_count FROM retried \
                       WHERE run_id='fault-before') = 1 \
                  AND (SELECT seized_generation FROM retried \
                       WHERE run_id='fault-before') = 22, \
                      'pre-terminal fault retry stores and notifies exactly once'; \
               ASSERT (SELECT caller_http_status FROM runs \
                        WHERE run_id='fault-before') = 504, \
                      'pre-terminal retry preserves response 504'; \
             END $$; COMMIT;",
            app_preamble(),
            sweep
        ),
    );

    // An AFTER trigger is the immediately-after-terminalization fault seam.
    // PostgreSQL must roll back the terminal row, seizure, and notification
    // together; the clean retry is again the sole committed outcome.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,status,response_deadline_at,run_deadline_at) \
         VALUES ('t1','fault-after','f',1,'running', \
                 TIMESTAMPTZ '2000-02-02 00:00:00Z',now()+interval '1 hour'); \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id,lease_generation) \
         VALUES ('t1','fault-after',31); \
         CREATE FUNCTION wamn_run.reject_after_terminalization() RETURNS trigger \
           LANGUAGE plpgsql AS $$ BEGIN \
             IF NEW.run_id='fault-after' AND NEW.status='cancelled' THEN \
               RAISE EXCEPTION 'fault-after-terminalization'; \
             END IF; \
             RETURN NEW; \
           END $$; \
         CREATE TRIGGER reject_after_terminalization \
           AFTER UPDATE ON wamn_run.runs FOR EACH ROW \
           EXECUTE FUNCTION wamn_run.reject_after_terminalization();",
    );
    failure(
        &url,
        &format!(
            "{} PREPARE sweep_stmt (bigint) AS {}; EXECUTE sweep_stmt(16); COMMIT;",
            app_preamble(),
            sweep
        ),
        "fault-after-terminalization",
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT status FROM wamn_run.runs WHERE run_id='fault-after') = 'running' \
              AND (SELECT caller_released_at FROM wamn_run.runs \
                   WHERE run_id='fault-after') IS NULL, \
                  'post-terminal fault rolls the stored outcome back'; \
           ASSERT (SELECT lease_generation FROM wamn_run.run_queue \
                   WHERE run_id='fault-after') = 31, \
                  'post-terminal fault rolls seizure back'; \
         END $$; \
         DROP TRIGGER reject_after_terminalization ON wamn_run.runs; \
         DROP FUNCTION wamn_run.reject_after_terminalization();",
    );
    success(
        &url,
        &format!(
            "{} PREPARE sweep_stmt (bigint) AS {}; \
             CREATE TEMP TABLE retried AS EXECUTE sweep_stmt(16); \
             CREATE TEMP TABLE no_second_outcome AS EXECUTE sweep_stmt(16); \
             DO $$ BEGIN \
               ASSERT (SELECT count(*) FROM retried WHERE run_id='fault-after') = 1 \
                  AND (SELECT notification_count FROM retried \
                       WHERE run_id='fault-after') = 1 \
                  AND (SELECT seized_generation FROM retried \
                       WHERE run_id='fault-after') = 32, \
                      'post-terminal fault retry stores and notifies exactly once'; \
               ASSERT NOT EXISTS (SELECT FROM no_second_outcome \
                                  WHERE run_id='fault-after'), \
                      'post-terminal retry cannot store or notify twice'; \
             END $$; COMMIT;",
            app_preamble(),
            sweep
        ),
    );
}
