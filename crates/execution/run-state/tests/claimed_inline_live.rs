//! PostgreSQL proof for exact claimed-run inline execution.

use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use wamn_run_state::queue::begin_claimed_run_sql;

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
    String::from_utf8(output.stdout).expect("psql output is UTF-8")
}

fn app_script(statement: &str, body: &str) -> String {
    format!(
        "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_run; \
         SET LOCAL app.tenant = 't1'; \
         PREPARE begin_claimed (text,text,bigint,bigint) AS {statement}; \
         {body} COMMIT;"
    )
}

#[test]
#[ignore = "requires WAMN_RUN_QUEUE_PG_URL and a throwaway PostgreSQL database"]
fn exact_claimed_run_live_faults_and_single_driver() {
    let url = std::env::var("WAMN_RUN_QUEUE_PG_URL")
        .expect("set WAMN_RUN_QUEUE_PG_URL to the throwaway superuser database");
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
             CREATE SCHEMA catalog; \
             CREATE TABLE catalog.release_manifests ( \
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
               PRIMARY KEY (tenant_id, catalog_id, catalog_version) \
             ); \
             CREATE TABLE catalog.execution_bundles ( \
               tenant_id text NOT NULL, execution_bundle_hash text NOT NULL, \
               PRIMARY KEY (tenant_id, execution_bundle_hash) \
             ); \
             INSERT INTO catalog.release_manifests VALUES \
               ('t1','claimed-inline-fixture',1), ('t2','claimed-inline-fixture',1); \
             INSERT INTO catalog.execution_bundles VALUES \
               ('t1','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'), \
               ('t2','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'); \
             {run_state} {run_queue} \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
                execution_bundle_hash,status,input_json) VALUES \
               ('t1','exact','flow-a',7,'claimed-inline-fixture',1,'test', \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
                'dispatched','{{\"v\":1}}'), \
               ('t1','stale','flow-a',7,'claimed-inline-fixture',1,'test', \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
                'dispatched','{{}}'), \
               ('t1','partitioned','flow-a',7,'claimed-inline-fixture',1,'test', \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
                'dispatched','{{}}'), \
               ('t1','terminal','flow-a',7,'claimed-inline-fixture',1,'test', \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
                'completed','{{}}'), \
               ('t2','other-tenant','flow-a',7,'claimed-inline-fixture',1,'test', \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
                'dispatched','{{}}'); \
             INSERT INTO wamn_run.run_queue \
               (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation,partition_key) \
             VALUES \
               ('t1','exact','inline-a',now()+interval '1 minute',1,NULL), \
               ('t1','stale','inline-a',now()+interval '1 minute',3,NULL), \
               ('t1','partitioned','inline-a',now()+interval '1 minute',1,'k'), \
               ('t1','terminal','inline-a',now()+interval '1 minute',1,NULL), \
               ('t2','other-tenant','inline-a',now()+interval '1 minute',1,NULL);"
        ),
    );

    let statement = begin_claimed_run_sql();
    let output = success(
        &url,
        &app_script(
            &statement,
            "CREATE TEMP TABLE first AS EXECUTE begin_claimed('exact','inline-a',1,30000); \
             DO $$ BEGIN \
               ASSERT (SELECT result_code FROM first) = 'claimed'; \
               ASSERT (SELECT flow_id FROM first) = 'flow-a'; \
               ASSERT (SELECT flow_version FROM first) = 7; \
               ASSERT (SELECT capture_mode FROM first) = 'off'; \
               ASSERT (SELECT status FROM runs WHERE run_id='exact') = 'running'; \
             END $$; \
             CREATE TEMP TABLE second AS EXECUTE begin_claimed('exact','inline-a',1,30000); \
             CREATE TEMP TABLE wrong_owner AS \
               EXECUTE begin_claimed('stale','inline-b',3,30000); \
             CREATE TEMP TABLE stale_generation AS \
               EXECUTE begin_claimed('stale','inline-a',2,30000); \
             CREATE TEMP TABLE keyed AS \
               EXECUTE begin_claimed('partitioned','inline-a',1,30000); \
             CREATE TEMP TABLE done AS \
               EXECUTE begin_claimed('terminal','inline-a',1,30000); \
             CREATE TEMP TABLE cross_tenant AS \
               EXECUTE begin_claimed('other-tenant','inline-a',1,30000); \
             DO $$ BEGIN \
               ASSERT (SELECT result_code FROM second) = 'already-driven'; \
               ASSERT (SELECT result_code FROM wrong_owner) = 'fence-lost'; \
               ASSERT (SELECT result_code FROM stale_generation) = 'fence-lost'; \
               ASSERT (SELECT result_code FROM keyed) = 'not-inline'; \
               ASSERT (SELECT result_code FROM done) = 'not-claimed'; \
               ASSERT (SELECT result_code FROM cross_tenant) = 'not-found'; \
             END $$;",
        ),
    );
    assert!(output.trim().is_empty());

    // Crash after arbitration: once the lease expires, the same identity and
    // generation may recover; no generic claim or generation bump occurs.
    success(
        &url,
        "UPDATE wamn_run.run_queue SET lease_expires_at=now()-interval '1 second' \
          WHERE run_id='exact';",
    );
    success(
        &url,
        &app_script(
            &statement,
            "CREATE TEMP TABLE recovered AS \
               EXECUTE begin_claimed('exact','inline-a',1,30000); \
             DO $$ BEGIN \
               ASSERT (SELECT result_code FROM recovered) = 'claimed'; \
               ASSERT (SELECT lease_generation FROM run_queue WHERE run_id='exact') = 1; \
             END $$;",
        ),
    );

    // Two fresh drivers race. The row locks serialize them; only one observes
    // `claimed`, while the waiter rechecks the committed running/live state.
    success(
        &url,
        "UPDATE wamn_run.runs SET status='dispatched' WHERE run_id='exact';",
    );
    let first_url = url.clone();
    let first_statement = statement.clone();
    let first = thread::spawn(move || {
        success(
            &first_url,
            &app_script(
                &first_statement,
                "CREATE TEMP TABLE raced AS \
                   EXECUTE begin_claimed('exact','inline-a',1,30000); \
                 SELECT result_code FROM raced; SELECT pg_sleep(0.5);",
            ),
        )
    });
    thread::sleep(Duration::from_millis(50));
    let second = success(
        &url,
        &app_script(
            &statement,
            "CREATE TEMP TABLE raced AS \
               EXECUTE begin_claimed('exact','inline-a',1,30000); \
             SELECT result_code FROM raced;",
        ),
    );
    let first = first.join().expect("first driver thread");
    assert!(first.lines().any(|line| line == "claimed"), "{first:?}");
    assert!(
        second.lines().any(|line| line == "already-driven"),
        "{second:?}"
    );

    success(&url, "DROP SCHEMA wamn_run CASCADE;");
}
