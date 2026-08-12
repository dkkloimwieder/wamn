//! Ignored PostgreSQL gate for occurrence-keyed child state transitions.

use std::process::{Command, Output};

use wamn_run_state::child::{create_or_recover_child_sql, release_child_sql};

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

fn prepare_create(sql: &str) -> String {
    format!(
        "PREPARE create_child_stmt \
         (text,text,text,bigint,text,int,text,text,text,text,text,text,int,bigint,text,text) \
         AS {sql};"
    )
}

fn prepare_release(sql: &str) -> String {
    format!(
        "PREPARE release_child_stmt \
         (text,text,text,bigint,text,text,int,text,text,text,text,int,bigint) AS {sql};"
    )
}

fn execute_create(parent: &str, owner: &str, generation: i64, child: &str) -> String {
    format!(
        "EXECUTE create_child_stmt(\
         '{parent}','{parent}','{owner}',{generation},'invoke',0,'{child}',\
         'child-internal','child-flow','service',\
         '{{\"decision\":\"approve\"}}','rev-child',8,64,NULL,'blocking')"
    )
}

fn seed_parent(url: &str, run_id: &str, owner: &str, generation: i64) {
    success(
        url,
        &format!(
            "INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment,status) \
             VALUES ('t1','{run_id}','parent-flow',1,'cat',4,'poc','running'); \
             INSERT INTO wamn_run.run_queue \
               (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
             VALUES ('t1','{run_id}','{owner}',now()+interval '1 minute',{generation});"
        ),
    );
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn child_live() {
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
             {run_state} {run_queue} \
             CREATE TABLE catalog.release_attachments (\
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL,\
               attachment_id text NOT NULL, attachment_kind text NOT NULL, flow_id text NOT NULL,\
               source_id text NOT NULL, definition_hash text NOT NULL, definition_json jsonb NOT NULL);\
             CREATE TABLE catalog.release_sources (\
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL,\
               source_id text NOT NULL, source_kind text NOT NULL, definition_json jsonb NOT NULL);\
             CREATE TABLE catalog.release_flows (\
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL,\
               flow_id text NOT NULL, flow_version int NOT NULL);\
             CREATE TABLE catalog.flow_artifacts (\
               tenant_id text NOT NULL, flow_id text NOT NULL, flow_version int NOT NULL,\
               artifact_hash text NOT NULL);\
             CREATE TABLE catalog.attachment_activation (\
               tenant_id text NOT NULL, catalog_id text NOT NULL, environment text NOT NULL,\
               attachment_id text NOT NULL, confirmed_definition_hash text NOT NULL,\
               enabled boolean NOT NULL);\
             GRANT USAGE ON SCHEMA catalog TO wamn_app;\
             GRANT SELECT ON ALL TABLES IN SCHEMA catalog TO wamn_app;\
             INSERT INTO catalog.release_sources VALUES \
               ('t1','cat',4,'child-callers','caller-policy',\
                '{{\"allowed-callers\":[\"parent-flow\"]}}');\
             INSERT INTO catalog.release_attachments VALUES \
               ('t1','cat',4,'child-internal','internal','child-flow','child-callers','sha256:def',\
                '{{\"run-deadline-ms\":60000,\"response-deadline-ms\":30000}}');\
             INSERT INTO catalog.release_flows VALUES \
               ('t1','cat',4,'child-flow',2);\
             INSERT INTO catalog.flow_artifacts VALUES \
               ('t1','child-flow',2,'sha256:artifact');\
             INSERT INTO catalog.attachment_activation VALUES \
               ('t1','cat','poc','child-internal','sha256:def',true);"
        ),
    );

    let create = create_or_recover_child_sql();
    let release = release_child_sql();

    // Positive: one statement inserts and pins the child, enqueues it, records
    // the occurrence wait, and releases the parent's queue lease.
    seed_parent(&url, "parent-create", "parent-worker", 7);
    let created = format!(
        "{} {} \
         CREATE TEMP TABLE created AS {}; \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM created) = 'created', \
             'child created: ' || (SELECT result_code FROM created); \
           ASSERT (SELECT child_run_id FROM created) = 'child-created', 'returned child'; \
           ASSERT (SELECT wait_generation FROM created) = 7, 'wait generation pinned'; \
           ASSERT EXISTS (SELECT FROM runs WHERE run_id='child-created' \
             AND parent_run_id='parent-create' AND parent_node_id='invoke' \
             AND parent_occurrence=0 AND flow_id='child-flow' AND flow_version=2 \
             AND catalog_id='cat' AND catalog_version=4 AND environment='poc' \
             AND attachment_id='child-internal' \
             AND input_json='{{\"decision\":\"approve\"}}'::jsonb \
             AND invocation_context->>'version'='1' \
             AND invocation_context->'principal'->>'run-id'='child-created' \
             AND invocation_context->'principal'->>'artifact-digest'='sha256:artifact' \
             AND invocation_context->'source'->'actor'->>'subject'='service:cat:poc:child-flow' \
             AND invocation_context->'source'->'caller'->>'flow-id'='parent-flow'), 'child identity pinned'; \
           ASSERT EXISTS (SELECT FROM run_queue WHERE run_id='child-created'), 'child enqueued'; \
           ASSERT EXISTS (SELECT FROM runs WHERE run_id='parent-create' \
             AND waiting_child_run_id='child-created' AND waiting_child_occurrence=0 \
             AND wait_generation=7), 'parent wait recorded'; \
           ASSERT EXISTS (SELECT FROM run_queue WHERE run_id='parent-create' \
             AND available_at='infinity'::timestamptz AND lease_owner IS NULL \
             AND lease_expires_at IS NULL), 'parent parked'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create("parent-create", "parent-worker", 7, "child-created")
    );
    success(&url, &created);

    // The seam discriminator: if an occurrence child already exists but its
    // parent wait was not recorded, replay recovers that exact child and parks
    // the parent. A different proposed run id is ignored.
    seed_parent(&url, "parent-recover", "recover-worker", 8);
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,attachment_id,\
            environment,status,trigger_source,input_json,platform_revision,parent_run_id,parent_node_id,\
            parent_occurrence,invoke_depth,invoke_root_run_id) \
         VALUES ('t1','child-existing','child-flow',2,'cat',4,'child-internal','poc',\
                 'dispatched','internal','{\"decision\":\"approve\"}','rev-child',\
                 'parent-recover','invoke',0,1,'parent-recover');",
    );
    let recovered = format!(
        "{} {} \
         CREATE TEMP TABLE recovered AS {}; \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM recovered) = 'recovered', 'child recovered'; \
           ASSERT (SELECT child_run_id FROM recovered) = 'child-existing', 'same child returned'; \
           ASSERT (SELECT count(*) FROM runs WHERE parent_run_id='parent-recover' \
             AND parent_node_id='invoke' AND parent_occurrence=0) = 1, 'exactly one child'; \
           ASSERT EXISTS (SELECT FROM run_queue WHERE run_id='child-existing'), 'recovered child queued'; \
           ASSERT EXISTS (SELECT FROM runs WHERE run_id='parent-recover' \
             AND waiting_child_run_id='child-existing' AND wait_generation=8), 'recovered wait'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create(
            "parent-recover",
            "recover-worker",
            8,
            "different-proposed-id"
        )
    );
    success(&url, &recovered);

    // A conflicting retry for the same occurrence cannot rewrite callee/input
    // identity or park the parent on the conflicting row.
    seed_parent(&url, "parent-conflict", "conflict-worker", 9);
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,attachment_id,\
            environment,status,trigger_source,input_json,platform_revision,parent_run_id,parent_node_id,\
            parent_occurrence,invoke_depth,invoke_root_run_id) \
         VALUES ('t1','child-conflict','child-flow',2,'cat',4,'child-internal','poc',\
                 'dispatched','internal','{\"decision\":\"deny\"}','rev-child',\
                 'parent-conflict','invoke',0,1,'parent-conflict');",
    );
    let conflict = format!(
        "{} {} \
         CREATE TEMP TABLE refused AS {}; \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused) = 'occurrence-conflict', \
             'conflicting occurrence refused'; \
           ASSERT (SELECT waiting_child_run_id FROM runs WHERE run_id='parent-conflict') IS NULL, \
             'conflict does not park parent'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='parent-conflict') \
             = 'conflict-worker', 'conflict preserves fence'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create("parent-conflict", "conflict-worker", 9, "child-other")
    );
    success(&url, &conflict);

    // Named creation fault: abort after the composed statement. No partial
    // child/wait/queue state survives; replay creates exactly one child.
    seed_parent(&url, "parent-fault", "fault-worker", 10);
    let create_fault = format!(
        "{} {} {}; SELECT 1/0; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create("parent-fault", "fault-worker", 10, "child-fault")
    );
    let fault = psql(&url, &create_fault);
    assert!(!fault.status.success(), "injected create fault must abort");
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id='child-fault'), \
             'fault rolls back child'; \
           ASSERT (SELECT waiting_child_run_id FROM wamn_run.runs \
             WHERE run_id='parent-fault') IS NULL, 'fault rolls back wait'; \
           ASSERT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id='parent-fault' \
             AND lease_owner='fault-worker' AND lease_generation=10), \
             'fault preserves parent fence'; \
         END $$;",
    );
    let create_after_fault = format!(
        "{} {} \
         CREATE TEMP TABLE replayed AS {}; \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM replayed) = 'created', 'fault replay creates'; \
           ASSERT (SELECT count(*) FROM runs WHERE parent_run_id='parent-fault' \
             AND parent_node_id='invoke' AND parent_occurrence=0) = 1, 'one child after replay'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create("parent-fault", "fault-worker", 10, "child-fault")
    );
    success(&url, &create_after_fault);

    // Claim the child so release is protected by its own queue generation.
    success(
        &url,
        "UPDATE wamn_run.run_queue SET lease_owner='child-worker', \
           lease_expires_at=now()+interval '1 minute', lease_generation=3 \
         WHERE tenant_id='t1' AND run_id='child-created';",
    );

    // Cross-parent access and a stale wait generation are typed and mutate
    // neither side.
    let cross_parent = format!(
        "{} {} \
         CREATE TEMP TABLE crossed AS \
           EXECUTE release_child_stmt('child-created','child-created','child-worker',3,\
             'responded','{{\"ok\":true}}',200,'respond','sha256:child',\
             'parent-recover','invoke',0,7); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM crossed) = 'cross-parent-access', \
             'cross-parent access refused'; \
           ASSERT (SELECT caller_released_at FROM runs WHERE run_id='child-created') IS NULL, \
             'cross-parent refusal does not release child'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_release(&release)
    );
    success(&url, &cross_parent);
    let stale = format!(
        "{} {} \
         CREATE TEMP TABLE stale AS \
           EXECUTE release_child_stmt('child-created','child-created','child-worker',3,\
             'responded','{{\"ok\":true}}',200,'respond','sha256:child',\
             'parent-create','invoke',0,6); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM stale) = 'stale-wait-generation', \
             'stale wait generation refused'; \
           ASSERT (SELECT caller_released_at FROM runs WHERE run_id='child-created') IS NULL, \
             'stale wake does not release child'; \
           ASSERT (SELECT wait_generation FROM runs WHERE run_id='parent-create') = 7, \
             'stale wake does not clear parent'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_release(&release)
    );
    success(&url, &stale);

    // Named wake fault: release and wait-clear execute, then the surrounding
    // transaction aborts. Both sides roll back together.
    let release_fault = format!(
        "{} {} \
         EXECUTE release_child_stmt('child-created','child-created','child-worker',3,\
           'responded','{{\"ok\":true}}',200,'respond','sha256:child',\
           'parent-create','invoke',0,7); \
         SELECT 1/0; COMMIT;",
        app_preamble(),
        prepare_release(&release)
    );
    let fault = psql(&url, &release_fault);
    assert!(!fault.status.success(), "injected release fault must abort");
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT caller_released_at FROM wamn_run.runs \
             WHERE run_id='child-created') IS NULL, 'release fault rolls back child'; \
           ASSERT EXISTS (SELECT FROM wamn_run.runs WHERE run_id='parent-create' \
             AND waiting_child_run_id='child-created' AND wait_generation=7), \
             'release fault rolls back parent clear'; \
           ASSERT (SELECT available_at FROM wamn_run.run_queue \
             WHERE run_id='parent-create') = 'infinity'::timestamptz, \
             'release fault rolls back wake'; \
         END $$;",
    );

    // Successful release has no observable half-state: child release, parent
    // wait clear, and queue wake all commit in the same statement.
    let released = format!(
        "{} {} \
         CREATE TEMP TABLE released AS \
           EXECUTE release_child_stmt('child-created','child-created','child-worker',3,\
             'responded','{{\"ok\":true}}',200,'respond','sha256:child',\
             'parent-create','invoke',0,7); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM released) = 'released', 'child released'; \
           ASSERT (SELECT caller_released_at FROM runs WHERE run_id='child-created') IS NOT NULL, \
             'child release committed'; \
           ASSERT EXISTS (SELECT FROM runs WHERE run_id='parent-create' \
             AND waiting_child_run_id IS NULL AND waiting_child_occurrence IS NULL \
             AND wait_generation IS NULL), 'parent wait atomically cleared'; \
           ASSERT (SELECT available_at FROM run_queue WHERE run_id='parent-create') <= now(), \
             'parent atomically woken'; \
         END $$; \
         CREATE TEMP TABLE replay AS \
           EXECUTE release_child_stmt('child-created','child-created','child-worker',3,\
             'responded','{{\"ok\":true}}',200,'respond','sha256:child',\
             'parent-create','invoke',0,7); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM replay) = 'already-released', \
             'release replay returns stored outcome'; \
           ASSERT (SELECT outcome_json::jsonb FROM replay) = '{{\"ok\":true}}'::jsonb, \
             'stored outcome returned'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_release(&release)
    );
    success(&url, &released);

    // Revocation gates creation only. Once release wakes the parent, a fresh
    // claim recovers the exact stored outcome without re-authorizing or
    // inheriting any child-authored context.
    success(
        &url,
        "UPDATE catalog.attachment_activation SET enabled=false \
          WHERE attachment_id='child-internal'; \
         UPDATE wamn_run.run_queue SET lease_owner='parent-resume', \
           lease_expires_at=now()+interval '1 minute', lease_generation=8 \
          WHERE run_id='parent-create';",
    );
    let resumed = format!(
        "{} {} \
         CREATE TEMP TABLE resumed AS {}; \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM resumed) = 'released', \
             'released child bypasses live reauthorization'; \
           ASSERT (SELECT outcome_json::jsonb FROM resumed) = '{{\"ok\":true}}'::jsonb, \
             'stored child outcome is byte-stable input to parent resume'; \
           ASSERT (SELECT waiting_child_run_id FROM runs WHERE run_id='parent-create') IS NULL, \
             'released recovery does not repark parent'; \
           ASSERT (SELECT lease_owner FROM run_queue WHERE run_id='parent-create')='parent-resume', \
             'released recovery preserves the new parent fence'; \
         END $$; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create("parent-create", "parent-resume", 8, "ignored-child-id")
    );
    success(&url, &resumed);

    // A new caller is checked against the typed policy and current activation.
    // Restore activation so the refusal discriminator is caller policy, not
    // revocation.
    success(
        &url,
        "UPDATE catalog.attachment_activation SET enabled=true \
          WHERE attachment_id='child-internal';",
    );
    seed_parent(&url, "parent-refused", "refused-worker", 11);
    success(
        &url,
        "UPDATE wamn_run.runs SET flow_id='not-allowed' WHERE run_id='parent-refused';",
    );
    let refused = format!(
        "{} {} CREATE TEMP TABLE refused_caller AS {}; \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM refused_caller)='caller-refused'; \
           ASSERT NOT EXISTS (SELECT FROM runs WHERE parent_run_id='parent-refused'); \
         END $$; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create("parent-refused", "refused-worker", 11, "child-refused")
    );
    success(&url, &refused);

    seed_parent(&url, "parent-depth", "depth-worker", 12);
    success(
        &url,
        "UPDATE wamn_run.runs SET invoke_depth=8 WHERE run_id='parent-depth';",
    );
    let depth = format!(
        "{} {} CREATE TEMP TABLE depth_refused AS {}; \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM depth_refused)='depth-exceeded'; \
           ASSERT NOT EXISTS (SELECT FROM runs WHERE parent_run_id='parent-depth'); \
         END $$; COMMIT;",
        app_preamble(),
        prepare_create(&create),
        execute_create("parent-depth", "depth-worker", 12, "child-depth")
    );
    success(&url, &depth);
}
