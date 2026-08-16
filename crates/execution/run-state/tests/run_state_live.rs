//! Ignored live gate for the fenced run-state transitions.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

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
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
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
}
