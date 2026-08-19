//! Ignored live gate for the fenced run-state transitions.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use wamn_run_state::queue::grant_production_claim_sql;
use wamn_run_state::transitions::{release_caller_sql, terminalize_sql};

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

    // `wamn_app` is re-stated rather than only created: the claim-time legs below
    // prove the DDL's column grants by executing under this role, and a leftover
    // role from an earlier database carrying SUPERUSER or BYPASSRLS would pass them
    // with no grants at all (wamn-0h0g.15.23).
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               ELSE \
                 ALTER ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS \
                 (SELECT FROM pg_roles WHERE rolname = 'wamn_run_projection_writer') THEN \
                 CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
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
               (tenant_id,catalog_id,catalog_version) \
             VALUES ('t1','cat',1);"
        ),
    );

    let release = release_caller_sql();
    let terminalize = terminalize_sql();

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
            admitted_catalog_version,admitted_flow_version,run_id) \
         VALUES ('t1','cat','prod','http-a','def-1','principal','client','fp-1', \
                 1,1,'admit-1'); \
         DO $$ DECLARE constraint_name text; BEGIN \
           BEGIN \
             INSERT INTO wamn_run.invocation_admissions \
               (tenant_id,catalog_id,environment,attachment_id,definition_hash, \
                principal_digest,client_key_digest,client_request_fingerprint, \
                admitted_catalog_version,admitted_flow_version,run_id) \
             VALUES ('t1','cat','prod','http-a','def-1','principal','client','fp-2', \
                     1,1,'admit-2'); \
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

    // The claim-time release record (wamn-0h0g.15.23). `(release_version,
    // manifest_digest)` is minted on the EXISTING claim write from the claiming
    // pod's own release identity, so the installed grants must admit the pair, the
    // named paired CHECK must refuse half of it, and
    // `guard_run_admission_pins_immutable` must refuse a rewrite. It is not blanket
    // write-once: NULL -> value is the claim, value -> NULL is how a runnable,
    // effect-free run reopens its claimability (the queue park, wamn-0h0g.15.82),
    // and value -> value' is refused on every path. Covered here against the
    // INSTALLED DDL, which is the guard the composed statements actually meet.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status,durability_class) VALUES \
           ('t1','record-claim','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'dispatched','standard'), \
           ('t1','record-unpaired','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'running','standard'), \
           ('t1','record-effect','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'dispatched','durable'); \
         INSERT INTO wamn_run.run_queue (tenant_id,run_id) VALUES \
           ('t1','record-claim'),('t1','record-effect');",
    );

    let claim = grant_production_claim_sql();
    let record_script = format!(
        "{} PREPARE claim_stmt (text,text,bigint,int,text) AS {}; \
         EXECUTE claim_stmt('record-claim','worker-record',30000,4, \
           'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'); \
         EXECUTE claim_stmt('record-effect','worker-effect',30000,4, \
           'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'); \
         DO $$ BEGIN \
           ASSERT (SELECT release_version FROM runs WHERE run_id='record-claim') = 4, \
                  'the claim records the claiming release version'; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-claim') \
                  = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
                  'the claim records the claiming manifest digest'; \
           ASSERT (SELECT status FROM runs WHERE run_id='record-claim') = 'running', \
                  'the record rides the claim write, not a second statement'; \
           ASSERT (SELECT lease_generation FROM run_queue WHERE run_id='record-claim') = 1, \
                  'the recording claim is the one that took the lease'; \
           ASSERT (SELECT release_version FROM runs WHERE run_id='record-effect') = 4, \
                  'each claim records its own pair'; \
         END $$; COMMIT;",
        app_preamble(),
        claim
    );
    success(&url, &record_script);

    // A recorded pair is rewritten by neither half.
    let refusal_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET release_version = 5 WHERE run_id = 'record-claim'; \
             ASSERT false, 'a recorded release version was rewritten in place'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-release-record-immutable', refusal; \
           END; \
           BEGIN \
             UPDATE runs SET manifest_digest = \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a' \
              WHERE run_id = 'record-claim'; \
             ASSERT false, 'a recorded manifest digest was rewritten in place'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-release-record-immutable', refusal; \
           END; \
           ASSERT (SELECT release_version FROM runs WHERE run_id='record-claim') = 4, \
                  'the refused rewrites left the recorded version exactly as claimed'; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-claim') \
                  = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', \
                  'the refused rewrites left the recorded digest exactly as claimed'; \
         END $$; COMMIT;",
        app_preamble()
    );
    success(&url, &refusal_script);

    // Erasure is the park/wake arm, and it is conditional: a runnable, effect-free
    // run may reopen its claimability, but a terminal run keeps the audit link to
    // the plan hashes it executed.
    let erasure_script = format!(
        "{} \
         UPDATE runs SET release_version = NULL, manifest_digest = NULL \
          WHERE run_id = 'record-claim'; \
         DO $$ BEGIN \
           ASSERT (SELECT release_version FROM runs WHERE run_id='record-claim') IS NULL, \
                  'a runnable, effect-free run may reopen its claimability'; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-claim') IS NULL, \
                  'a runnable, effect-free run may reopen its claimability'; \
         END $$; \
         UPDATE runs SET release_version = 6, \
                manifest_digest = \
                  'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a' \
          WHERE run_id = 'record-claim'; \
         UPDATE runs SET status = 'completed' WHERE run_id = 'record-claim'; \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET release_version = NULL, manifest_digest = NULL \
              WHERE run_id = 'record-claim'; \
             ASSERT false, 'a terminal run erased the release it executed under'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-release-record-immutable', refusal; \
           END; \
           ASSERT (SELECT release_version FROM runs WHERE run_id='record-claim') = 6, \
                  'the re-recorded pair survives the refused erasure'; \
         END $$; COMMIT;",
        app_preamble()
    );
    success(&url, &erasure_script);

    // The other erasure precondition: an attributed effect names the release that
    // fired it, and that link is never rewritten out from under it. `record-effect`
    // is still `running`, so only the effect evidence can refuse here.
    //
    // THIS LEG IS A PREMIUM-TIER PROOF (wamn-0h0g.20.2). The guard's
    // effect-attempt arm is class-gated, so `record-effect` is admitted
    // `durable` above; on the default `standard` class the same run erases its
    // record freely, which is what the leg below asserts and what keeps the
    // queue park from ever aborting on the guard. The split into
    // surviving-spine and shelved-floor suites is wamn-0h0g.20.4's.
    success(
        &url,
        "INSERT INTO wamn_run.effect_attempts \
           (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id,local_node_id, \
            source_artifact_hash,requirement_name,occurrence,seq,generation_fact_kind, \
            attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','record-effect', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',0, \
           'effect-node', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'manager',0,1,'not-required','2099-01-01T00:00:00Z','record-effect-input');",
    );
    let effect_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET release_version = NULL, manifest_digest = NULL \
              WHERE run_id = 'record-effect'; \
             ASSERT false, 'an attributed effect lost the release that fired it'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-release-record-immutable', refusal; \
           END; \
           ASSERT (SELECT release_version FROM runs WHERE run_id='record-effect') = 4, \
                  'the release the effect fired under is intact'; \
         END $$; COMMIT;",
        app_preamble()
    );
    success(&url, &effect_script);

    // THE COMPLEMENT, AND THE HALF THE CLASS GATE MAKES LOAD-BEARING
    // (wamn-0h0g.20.2). The identical run on the DEFAULT class erases its
    // record freely even while carrying an attributed effect. If this leg ever
    // reds, `park_sql` — which carries the same class predicate on the same
    // `EXISTS` — aborts on this guard for every standard run that ever reached
    // the effect ledger, and the run plane loses the arm that reopens
    // claimability (wamn-0h0g.15.82).
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status,durability_class) VALUES \
           ('t1','record-standard-effect','f',1,'cat',1,'prod', \
            'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
            'running','standard'); \
         UPDATE wamn_run.runs SET release_version = 4, manifest_digest = \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a' \
          WHERE run_id = 'record-standard-effect'; \
         INSERT INTO wamn_run.effect_attempts \
           (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id,local_node_id, \
            source_artifact_hash,requirement_name,occurrence,seq,generation_fact_kind, \
            attempt_deadline_at,attempt_input_ref) \
         VALUES ('t1','record-standard-effect', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',0, \
           'effect-node', \
           'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
           'manager',0,1,'not-required','2099-01-01T00:00:00Z','standard-effect-input');",
    );
    let standard_class_script = format!(
        "{} \
         UPDATE runs SET release_version = NULL, manifest_digest = NULL \
          WHERE run_id = 'record-standard-effect'; \
         DO $$ BEGIN \
           ASSERT (SELECT release_version FROM runs \
                    WHERE run_id='record-standard-effect') IS NULL, \
                  'the default class could not clear a record the park must clear'; \
         END $$; COMMIT;",
        app_preamble()
    );
    success(&url, &standard_class_script);

    // The class is defended TWICE, and the two guards are independent.
    //
    // First, by the column grant: `wamn_app` holds neither INSERT nor UPDATE on
    // `durability_class`, so the guest-visible role cannot buy the premium tier
    // at all — the refusal is `insufficient_privilege`, before any trigger runs.
    let class_grant_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET durability_class = 'durable' \
              WHERE run_id = 'record-standard-effect'; \
             ASSERT false, 'the app role holds write authority over the class'; \
           EXCEPTION WHEN insufficient_privilege THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'permission denied for table runs', refusal; \
           END; \
         END $$; COMMIT;",
        app_preamble()
    );
    success(&url, &class_grant_script);

    // Second, by the column-scoped trigger, which is what defends the class
    // against a role the grant does not stop. RIDER 1 of wamn-0h0g.20.1: a
    // column the trigger does not NAME never fires its transition arm, so this
    // leg is the only thing that can tell a named column from an unnamed one.
    success(
        &url,
        "DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE wamn_run.runs SET durability_class = 'durable' \
              WHERE run_id = 'record-standard-effect'; \
             ASSERT false, 'an admitted run changed its durability class'; \
           EXCEPTION WHEN object_not_in_prerequisite_state THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'run-admission-pin-immutable', refusal; \
           END; \
           ASSERT (SELECT durability_class FROM wamn_run.runs \
                    WHERE run_id='record-standard-effect') = 'standard', \
                  'the refused class change leaked through'; \
         END $$;",
    );

    // The ruled literal set and the fail-open default, against the INSTALLED
    // DDL rather than the file: `standard` is what an admission that names no
    // class takes, and nothing outside the pair is storable. The unruled literal
    // is tried on an INSERT deliberately — a BEFORE UPDATE trigger runs ahead of
    // constraint checking, so an UPDATE would prove the trigger again, not the
    // CHECK.
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT durability_class FROM wamn_run.runs \
                    WHERE run_id='record-claim') = 'standard', \
                  'the absent-policy default is not the cheap tier'; \
           BEGIN \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,execution_bundle_hash,status,durability_class) \
             VALUES ('t1','record-unruled-class','f',1,'cat',1,'prod', \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
               'running','premium'); \
             ASSERT false, 'the class CHECK admitted an unruled literal'; \
           EXCEPTION WHEN check_violation THEN NULL; \
           END; \
         END $$;",
    );

    // Half a record never reaches the guard — its record arms do not fire while
    // both OLD values are NULL — so `runs_release_record_check` is the only thing
    // that can refuse it, under either column.
    //
    // The two half-pair legs are why the CHECK needs its `IS NOT NULL` conjuncts:
    // with only `release_version > 0 AND manifest_digest ~ '…'`, the disjunct
    // evaluates to NULL — not false — when exactly one half is present and well
    // formed, and a CHECK whose expression is NULL is SATISFIED. The pair is then
    // not actually paired: `(7, NULL)` and `(NULL, '<digest>')` are both admitted,
    // on a table that is author-reachable. These legs assert the contract, so they
    // are red against a CHECK that has lost those conjuncts, and they are last so
    // every leg above still proves out before this one can abort the suite.
    let paired_script = format!(
        "{} \
         DO $$ DECLARE refusal text; BEGIN \
           BEGIN \
             UPDATE runs SET release_version = 0, \
                    manifest_digest = \
                      'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855' \
              WHERE run_id = 'record-unpaired'; \
             ASSERT false, 'runs_release_record_check admitted release version 0'; \
           EXCEPTION WHEN check_violation THEN \
             GET STACKED DIAGNOSTICS refusal = CONSTRAINT_NAME; \
             ASSERT refusal = 'runs_release_record_check', refusal; \
           END; \
           BEGIN \
             UPDATE runs SET release_version = 7 WHERE run_id = 'record-unpaired'; \
             ASSERT false, \
                    'runs_release_record_check admitted a version with no digest'; \
           EXCEPTION WHEN check_violation THEN \
             GET STACKED DIAGNOSTICS refusal = CONSTRAINT_NAME; \
             ASSERT refusal = 'runs_release_record_check', refusal; \
           END; \
           BEGIN \
             UPDATE runs SET manifest_digest = \
               'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855' \
              WHERE run_id = 'record-unpaired'; \
             ASSERT false, \
                    'runs_release_record_check admitted a digest with no version'; \
           EXCEPTION WHEN check_violation THEN \
             GET STACKED DIAGNOSTICS refusal = CONSTRAINT_NAME; \
             ASSERT refusal = 'runs_release_record_check', refusal; \
           END; \
           ASSERT (SELECT release_version FROM runs WHERE run_id='record-unpaired') IS NULL, \
                  'the unclaimed run carries no release record'; \
           ASSERT (SELECT manifest_digest FROM runs WHERE run_id='record-unpaired') IS NULL, \
                  'the unclaimed run carries no release record'; \
         END $$; COMMIT;",
        app_preamble()
    );
    success(&url, &paired_script);
}
