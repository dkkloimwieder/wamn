//! Ignored live gate for the callable-flow admission transaction.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use wamn_run_state::admission::{
    AdmissionTransition, ManagementProducerKey, RunStateSchema, admission_transaction,
    registration_evidence,
};

fn psql(url: &str, script: &str) -> Output {
    let mut child = Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run psql");
    let _ = child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes());
    child.wait_with_output().expect("wait for psql")
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
    "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL app.tenant = 't1';"
}

/// The private management authority `wamn-0h0g.7.5` ratified.
fn management_preamble() -> &'static str {
    "BEGIN; SET LOCAL ROLE wamn_management_admitter; SET LOCAL app.tenant = 't1';"
}

fn prepare(sql: &str) -> String {
    format!(
        "PREPARE admit_stmt \
         (text,text,text,int,text,text,text,int,text,text,text,text, \
          timestamptz,timestamptz,text,text,text, \
          text,bigint,text,text,text,text,int) AS {sql};"
    )
}

fn prepare_management(sql: &str) -> String {
    format!(
        "PREPARE admit_management \
         (text,text,text,int,text,int,text,text,text,text, \
          timestamptz,text,text,int) AS {sql};"
    )
}

fn execute_draft(run_id: &str, command_id: &str, input: &str) -> String {
    format!(
        "EXECUTE admit_management(\
         'draft-run','c1','dev',1,'flow-http',1,\
         '{run_id}','{input}','{{\"request-id\":\"draft-1\"}}','rev-test',\
         now()+interval '1 minute','{command_id}',NULL,NULL)"
    )
}

fn execute_draft_at(run_id: &str, command_id: &str, catalog_version: i32) -> String {
    format!(
        "EXECUTE admit_management(\
         'draft-run','c1','dev',{catalog_version},'flow-http',1,\
         '{run_id}','{{\"draft\":1}}','{{\"request-id\":\"draft-1\"}}','rev-test',\
         now()+interval '1 minute','{command_id}',NULL,NULL)"
    )
}

fn execute_case(run_id: &str, report_id: &str, ordinal: i32, context: &str) -> String {
    format!(
        "EXECUTE admit_management(\
         'test-case','c1','dev',1,'flow-http',1,\
         '{run_id}','{{\"case\":{ordinal}}}','{context}','rev-test',\
         now()+interval '1 minute',NULL,'{report_id}',{ordinal})"
    )
}

fn execute_http(run_id: &str, key: &str, fingerprint: &str) -> String {
    format!(
        "EXECUTE admit_stmt(\
         'http','c1','dev',1,'http-a','sha256:http','flow-http',1,\
         '{run_id}','{{\"request\":1}}','{{\"request-id\":\"req-1\"}}','rev-test',\
         now()+interval '30 seconds',now()+interval '1 minute',\
         'principal','{key}','{fingerprint}',\
         NULL,NULL,NULL,NULL,NULL,NULL,NULL)"
    )
}

fn execute_http_without_key(run_id: &str) -> String {
    format!(
        "EXECUTE admit_stmt(\
         'http','c1','dev',1,'http-a','sha256:http','flow-http',1,\
         '{run_id}','{{\"request\":1}}','{{\"request-id\":\"req-1\"}}','rev-test',\
         now()+interval '30 seconds',now()+interval '1 minute',\
         'principal',NULL,'fingerprint-without-key',\
         NULL,NULL,NULL,NULL,NULL,NULL,NULL)"
    )
}

fn execute_event_lineage(
    run_id: &str,
    document: &str,
    hash: &str,
    seq: i64,
    source_run_id: &str,
    root_run_id: &str,
    depth: i32,
) -> String {
    format!(
        "EXECUTE admit_stmt(\
         'event','c1','dev',1,NULL,NULL,'flow-event',1,\
         '{run_id}','{{\"event\":{seq}}}','{{\"event-seq\":{seq}}}','rev-test',\
         NULL,now()+interval '1 minute',NULL,NULL,NULL,\
         'reg-a',{seq},'{document}','{hash}',\
         '{source_run_id}','{root_run_id}',{depth})"
    )
}

fn execute_event(run_id: &str, document: &str, hash: &str, seq: i64) -> String {
    execute_event_lineage(run_id, document, hash, seq, run_id, run_id, 0)
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn admission_live() {
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
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles \
                               WHERE rolname = 'wamn_run_projection_writer') THEN \
                 CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles \
                               WHERE rolname = 'wamn_management_admitter') THEN \
                 CREATE ROLE wamn_management_admitter NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             {catalog} {run_state} {run_queue}"
        ),
    );

    let registration = json!({
        "schema-version": "0.1",
        "registration-id": "reg-a",
        "catalog-id": "c1",
        "flow-id": "flow-event",
        "entity": "hold",
        "ops": ["update"]
    });
    let (registration_document, registration_digest) = registration_evidence(&registration);

    success(
        &url,
        &format!(
            "BEGIN; \
             INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('t1','c1',1,'dev','0.1','applied'), \
                    ('t2','c1',1,'dev','0.1','applied'); \
             INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) VALUES \
               ('t1','flow-http',1,'0.1','{{}}','gh-http','ah-http'), \
               ('t1','flow-event',1,'0.1','{{}}','gh-event','ah-event'); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             SELECT tenant_id, 'sha256:' || encode(sha256(convert_to('{{}}','UTF8')), 'hex'), \
                    '0.1', convert_to('{{}}','UTF8'), 2 \
               FROM (VALUES ('t1'), ('t2')) AS tenants(tenant_id); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             SELECT 't2', 'sha256:' || encode(sha256(convert_to('{{\"tenant\":\"t2\"}}','UTF8')), 'hex'), \
                    '0.1', convert_to('{{\"tenant\":\"t2\"}}','UTF8'), \
                    octet_length(convert_to('{{\"tenant\":\"t2\"}}','UTF8')); \
             INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version) VALUES \
               ('t1','c1',1), \
               ('t2','c1',1); \
             INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) VALUES \
               ('t1','c1',1,'flow-http',1,'sha256:' || encode(sha256(convert_to('{{}}','UTF8')), 'hex')), \
               ('t1','c1',1,'flow-event',1,'sha256:' || encode(sha256(convert_to('{{}}','UTF8')), 'hex')); \
             INSERT INTO catalog.release_exposure_manifests \
               (tenant_id,catalog_id,catalog_version,definitions_json) \
             VALUES ('t1','c1',1,'{{}}'); \
             INSERT INTO catalog.release_sources \
               (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) VALUES \
               ('t1','c1',1,'auth-a','auth','{{}}','source-http'); \
             INSERT INTO catalog.release_attachments \
               (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id,source_id, \
                definition_hash,definition_json,route_host,route_path,route_template,route_method) VALUES \
               ('t1','c1',1,'http-a','http','flow-http','auth-a','sha256:http','{{}}',\
                'example.test','/echo','/echo','POST'); \
             INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) VALUES ('t1','c1','dev',1); \
             INSERT INTO catalog.attachment_activation \
               (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) VALUES \
               ('t1','c1','dev','http-a','sha256:http',true); \
             INSERT INTO catalog.event_registrations \
               (tenant_id,catalog_id,registration_id,flow_id,entity_id,registration) VALUES \
               ('t1','c1','reg-a','flow-event','hold','{registration_document}'); \
             COMMIT;"
        ),
    );

    let schema = RunStateSchema::default();
    let recipe = admission_transaction(AdmissionTransition::CallableFlow { schema: &schema });
    let admit = recipe.admit().to_string();
    let prepared = prepare(&admit);

    success(
        &url,
        "INSERT INTO wamn_run.environment_policies \
           (tenant_id,expected_environment,durability_class) \
         VALUES ('t1','dev','standard'), \
                ('t2','dev','standard');",
    );

    // Both producer variants use the same statement and enter the ordinary
    // unleased queue. Event admission additionally carries its stream position.
    for (execution, expected) in [
        (execute_http("http-1", "key-1", "fp-1"), "admitted|http-1"),
        (
            execute_event("event-1", &registration_document, &registration_digest, 41),
            "admitted|event-1",
        ),
    ] {
        let result = success(
            &url,
            &format!("{} {} {}; COMMIT;", app_preamble(), prepared, execution),
        );
        assert_eq!(result.trim(), expected);
    }
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               ASSERT (SELECT status FROM wamn_run.runs WHERE run_id='http-1') = 'dispatched'; \
               ASSERT (SELECT available_at <= now() FROM wamn_run.run_queue WHERE run_id='http-1'); \
               ASSERT (SELECT lease_owner FROM wamn_run.run_queue WHERE run_id='http-1') IS NULL; \
               ASSERT (SELECT lease_expires_at FROM wamn_run.run_queue WHERE run_id='http-1') IS NULL; \
               ASSERT (SELECT lease_generation FROM wamn_run.run_queue WHERE run_id='http-1') = 0; \
               ASSERT (SELECT count(*) FROM wamn_run.invocation_admissions WHERE run_id='http-1') = 1; \
               ASSERT (SELECT lease_owner FROM wamn_run.run_queue WHERE run_id='event-1') IS NULL; \
               ASSERT (SELECT stream_seq FROM wamn_run.run_queue WHERE run_id='event-1') = 41; \
               ASSERT (SELECT capture_mode FROM wamn_run.runs WHERE run_id='http-1') = 'off'; \
               ASSERT (SELECT capture_mode FROM wamn_run.runs WHERE run_id='event-1') = 'off'; \
               ASSERT (SELECT invocation_context->'source'->>'registration-hash' FROM wamn_run.runs \
                        WHERE run_id='event-1') = '{}'; \
               ASSERT (SELECT invocation_context->>'version' FROM wamn_run.runs \
                        WHERE run_id='event-1') = '0.1'; \
               ASSERT (SELECT invocation_context->'principal'->>'artifact-digest' \
                        FROM wamn_run.runs WHERE run_id='event-1') = 'ah-event'; \
               ASSERT (SELECT invocation_context->'principal'->>'run-id' \
                        FROM wamn_run.runs WHERE run_id='event-1') = 'event-1'; \
               ASSERT (SELECT execution_bundle_hash FROM wamn_run.runs WHERE run_id='http-1') \
                        = (SELECT execution_bundle_hash FROM catalog.release_flows \
                            WHERE tenant_id='t1' AND catalog_id='c1' AND flow_id='flow-http'); \
               ASSERT (SELECT execution_bundle_hash FROM wamn_run.runs WHERE run_id='event-1') \
                        = (SELECT execution_bundle_hash FROM catalog.release_flows \
                            WHERE tenant_id='t1' AND catalog_id='c1' AND flow_id='flow-event'); \
             END $$;",
            registration_digest
        ),
    );

    // Admission reads the project-local policy projection and freezes it on
    // each run. A policy change affects only later admissions; neither the
    // canonical statement nor its caller carries the class as a parameter.
    success(
        &url,
        "UPDATE wamn_run.environment_policies SET durability_class='durable' \
          WHERE tenant_id='t1';",
    );

    // Named COALESCE/mismatch mutants: a direct guest insert cannot obtain the
    // cheap tier by omitting convergence, and cannot name another environment
    // in this one-environment physical database.
    success(
        &url,
        "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL app.tenant = 't1'; \
         DO $policy_refusals$ DECLARE refusal text; BEGIN \
           BEGIN \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,execution_bundle_hash) \
             VALUES ('t1','policy-environment-mismatch','flow-http',1,'c1',1, \
                     'alternate', \
                     'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'); \
             ASSERT false, 'alternate environment admission succeeded'; \
           EXCEPTION WHEN SQLSTATE '55000' THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'environment-policy-environment-mismatch', refusal; \
           END; \
           PERFORM set_config('app.tenant', 'unprojected', true); \
           BEGIN \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,execution_bundle_hash) \
             VALUES ('unprojected','policy-not-converged','flow-http',1,'c1',1, \
                     'dev', \
                     'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'); \
             ASSERT false, 'unprojected tenant admission succeeded'; \
           EXCEPTION WHEN SQLSTATE '55000' THEN \
             GET STACKED DIAGNOSTICS refusal = MESSAGE_TEXT; \
             ASSERT refusal = 'environment-policy-not-converged', refusal; \
           END; \
         END $policy_refusals$; COMMIT;",
    );
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
            environment,execution_bundle_hash) \
         VALUES ('t1','policy-scope-probe','flow-http',1,'c1',1,'dev', \
                 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'); \
         DO $$ BEGIN \
           ASSERT (SELECT durability_class FROM wamn_run.runs \
                    WHERE tenant_id='t1' AND run_id='policy-scope-probe')='durable', \
                  'the trigger did not use the tenant-qualified policy key'; \
         END $$; \
         UPDATE wamn_run.runs SET status='completed' \
          WHERE tenant_id='t1' AND run_id='policy-scope-probe'; \
         DELETE FROM wamn_run.runs \
          WHERE tenant_id='t1' AND run_id='policy-scope-probe';",
    );
    let durable = success(
        &url,
        &format!(
            "{} {} {}; COMMIT;",
            app_preamble(),
            prepared,
            execute_http(
                "http-policy-durable",
                "key-policy-durable",
                "fp-policy-durable"
            )
        ),
    );
    assert_eq!(durable.trim(), "admitted|http-policy-durable");
    success(
        &url,
        "UPDATE wamn_run.environment_policies SET durability_class='standard' \
          WHERE tenant_id='t1';",
    );
    let standard = success(
        &url,
        &format!(
            "{} {} {}; COMMIT;",
            app_preamble(),
            prepared,
            execute_http(
                "http-policy-standard",
                "key-policy-standard",
                "fp-policy-standard"
            )
        ),
    );
    assert_eq!(standard.trim(), "admitted|http-policy-standard");
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT durability_class FROM wamn_run.runs \
                    WHERE run_id='http-policy-durable') = 'durable', \
                  'the durable policy was not frozen at admission'; \
           ASSERT (SELECT durability_class FROM wamn_run.runs \
                    WHERE run_id='http-policy-standard') = 'standard', \
                  'the changed policy did not govern the later admission'; \
         END $$;",
    );

    // Pure call-free routes may omit the key. NULL is deliberately not a
    // reusable identity: each invocation admits a distinct run and ledger row.
    for run_id in ["http-no-key-a", "http-no-key-b"] {
        let result = success(
            &url,
            &format!(
                "{} {} {}; COMMIT;",
                app_preamble(),
                prepared,
                execute_http_without_key(run_id)
            ),
        );
        assert_eq!(result.trim(), format!("admitted|{run_id}"));
    }
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM wamn_run.invocation_admissions \
                     WHERE run_id IN ('http-no-key-a','http-no-key-b') \
                       AND client_key_digest IS NULL) = 2; \
           ASSERT (SELECT count(*) FROM wamn_run.runs \
                     WHERE run_id IN ('http-no-key-a','http-no-key-b')) = 2; \
           ASSERT (SELECT count(*) FROM wamn_run.run_queue \
                     WHERE run_id IN ('http-no-key-a','http-no-key-b')) = 2; \
         END $$;",
    );

    // Simulate a legacy/corrupt release member whose scalar hash names a plan
    // that exists only in another tenant. Admission must resolve through the
    // tenant-scoped bundle join and refuse before any of its three writes.
    let missing_plan = success(
        &url,
        &format!(
            "CREATE TEMP TABLE missing_plan_counts AS \
               SELECT (SELECT count(*) FROM wamn_run.runs) AS runs, \
                      (SELECT count(*) FROM wamn_run.run_queue) AS queue, \
                      (SELECT count(*) FROM wamn_run.invocation_admissions) AS admissions; \
             CREATE TEMP TABLE original_release_bundle AS \
               SELECT execution_bundle_hash FROM catalog.release_flows \
                WHERE tenant_id='t1' AND catalog_id='c1' AND catalog_version=1 \
                  AND flow_id='flow-http'; \
             ALTER TABLE catalog.release_flows \
               DROP CONSTRAINT release_flows_execution_bundle_fk; \
             DROP TRIGGER release_flows_immutable ON catalog.release_flows; \
             UPDATE catalog.release_flows \
                SET execution_bundle_hash = ( \
                  SELECT execution_bundle_hash FROM catalog.execution_bundles \
                   WHERE tenant_id='t2' \
                     AND exact_bytes = convert_to('{{\"tenant\":\"t2\"}}','UTF8')) \
              WHERE tenant_id='t1' AND catalog_id='c1' AND catalog_version=1 \
                AND flow_id='flow-http'; \
             {} {} \
             CREATE TEMP TABLE missing_plan_result AS {}; COMMIT; \
             DO $$ BEGIN \
               ASSERT (SELECT count(*) FROM catalog.execution_bundles AS bundle \
                         JOIN catalog.release_flows AS rf \
                           ON bundle.execution_bundle_hash = rf.execution_bundle_hash \
                        WHERE rf.tenant_id='t1' AND rf.catalog_id='c1' \
                          AND rf.catalog_version=1 AND rf.flow_id='flow-http' \
                          AND bundle.tenant_id='t2') = 1; \
               ASSERT NOT EXISTS (SELECT FROM catalog.execution_bundles AS bundle \
                         JOIN catalog.release_flows AS rf \
                           ON bundle.tenant_id = rf.tenant_id \
                          AND bundle.execution_bundle_hash = rf.execution_bundle_hash \
                        WHERE rf.tenant_id='t1' AND rf.catalog_id='c1' \
                          AND rf.catalog_version=1 AND rf.flow_id='flow-http'); \
               ASSERT (SELECT result_code FROM missing_plan_result) = 'missing-root-plan'; \
               ASSERT (SELECT run_id FROM missing_plan_result) IS NULL; \
               ASSERT (SELECT count(*) FROM wamn_run.runs) \
                    = (SELECT runs FROM missing_plan_counts); \
               ASSERT (SELECT count(*) FROM wamn_run.run_queue) \
                    = (SELECT queue FROM missing_plan_counts); \
               ASSERT (SELECT count(*) FROM wamn_run.invocation_admissions) \
                    = (SELECT admissions FROM missing_plan_counts); \
               ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id='missing-plan-run'); \
               ASSERT NOT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id='missing-plan-run'); \
               ASSERT NOT EXISTS (SELECT FROM wamn_run.invocation_admissions \
                                   WHERE run_id='missing-plan-run'); \
             END $$; \
             UPDATE catalog.release_flows AS rf \
                SET execution_bundle_hash = original.execution_bundle_hash \
               FROM original_release_bundle AS original \
              WHERE rf.tenant_id='t1' AND rf.catalog_id='c1' AND rf.catalog_version=1 \
                AND rf.flow_id='flow-http'; \
             ALTER TABLE catalog.release_flows \
               ADD CONSTRAINT release_flows_execution_bundle_fk \
               FOREIGN KEY (tenant_id, execution_bundle_hash) \
               REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash); \
             CREATE TRIGGER release_flows_immutable \
               BEFORE UPDATE ON catalog.release_flows FOR EACH ROW \
               EXECUTE FUNCTION catalog.reject_immutable_row_change(); \
             SELECT result_code || '|' || COALESCE(run_id, '') FROM missing_plan_result;",
            app_preamble(),
            prepared,
            execute_http("missing-plan-run", "key-missing-plan", "fp-missing-plan")
        ),
    );
    assert_eq!(missing_plan.trim(), "missing-root-plan|");

    // Trusted causal ancestry is stored outside both the business input and
    // invocation context. A non-event source roots at itself; subsequent event
    // runs carry the immediate parent while retaining the original root.
    for (execution, expected) in [
        (
            execute_event_lineage(
                "event-child",
                &registration_document,
                &registration_digest,
                46,
                "http-1",
                "http-1",
                1,
            ),
            "admitted|event-child",
        ),
        (
            execute_event_lineage(
                "event-grandchild",
                &registration_document,
                &registration_digest,
                47,
                "event-child",
                "http-1",
                2,
            ),
            "admitted|event-grandchild",
        ),
    ] {
        let result = success(
            &url,
            &format!("{} {} {}; COMMIT;", app_preamble(), prepared, execution),
        );
        assert_eq!(result.trim(), expected);
    }
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT event_source_run_id='event-1' AND event_root_run_id='event-1' \
                     AND event_depth=0 FROM wamn_run.runs WHERE run_id='event-1'); \
           ASSERT (SELECT event_source_run_id='http-1' AND event_root_run_id='http-1' \
                     AND event_depth=1 FROM wamn_run.runs WHERE run_id='event-child'); \
           ASSERT (SELECT event_source_run_id='event-child' AND event_root_run_id='http-1' \
                     AND event_depth=2 FROM wamn_run.runs WHERE run_id='event-grandchild'); \
           ASSERT NOT (SELECT input_json ? 'causation' FROM wamn_run.runs \
                        WHERE run_id='event-grandchild'); \
           ASSERT NOT (SELECT invocation_context->'source' ? 'causation' FROM wamn_run.runs \
                        WHERE run_id='event-grandchild'); \
         END $$;",
    );

    // Definition/head drift, conflicting HTTP reuse, and stale/bad event
    // registration evidence are typed refusals and create no run.
    let negatives = format!(
        "{} {} \
         CREATE TEMP TABLE reused AS {}; \
         CREATE TEMP TABLE bad_hash AS {}; \
         CREATE TEMP TABLE stale_head AS EXECUTE admit_stmt(\
           'http','c1','dev',99,'http-a','sha256:http','flow-http',1,\
           'http-stale','{{}}','{{}}','rev-test',now()+interval '30 seconds',\
           now()+interval '1 minute','principal-stale','key-stale','fp-stale',\
           NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
         CREATE TEMP TABLE inactive AS EXECUTE admit_stmt(\
           'http','c1','dev',1,'missing','sha256:http','flow-http',1,\
           'http-inactive','{{}}','{{}}','rev-test',now()+interval '30 seconds',\
           now()+interval '1 minute','principal-inactive','key-inactive','fp-inactive',\
           NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM reused) = 'idempotency-key-reused'; \
           ASSERT (SELECT result_code FROM bad_hash) = 'invalid-registration-hash'; \
           ASSERT (SELECT result_code FROM stale_head) = 'head-drift'; \
           ASSERT (SELECT result_code FROM inactive) = 'inactive-definition'; \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
             WHERE run_id IN ('http-reused','event-bad','http-stale','http-inactive')); \
         END $$; COMMIT;",
        app_preamble(),
        prepared,
        execute_http("http-reused", "key-1", "different-fingerprint"),
        execute_event("event-bad", &registration_document, "sha256:bad", 42),
    );
    success(&url, &negatives);

    // The refusal above uses an attachment id that was never released, so it
    // only proves an absent route refuses. Enablement is the separate,
    // environment-owned fact: one UPDATE against catalog.attachment_activation
    // takes a released, hash-current, tombstone-free attachment out of
    // catalog.active_attachments, and the byte-identical prepared admission
    // statement refuses at the very next execution.
    success(
        &url,
        "UPDATE catalog.attachment_activation SET enabled = false \
          WHERE tenant_id='t1' AND catalog_id='c1' AND environment='dev' \
            AND attachment_id='http-a'; \
         DO $$ BEGIN \
           ASSERT EXISTS (SELECT FROM catalog.release_attachments \
             WHERE tenant_id='t1' AND catalog_id='c1' AND catalog_version=1 \
               AND attachment_id='http-a' AND definition_hash='sha256:http'); \
           ASSERT NOT EXISTS (SELECT FROM catalog.attachment_tombstones \
             WHERE tenant_id='t1' AND attachment_id='http-a'); \
           ASSERT NOT EXISTS (SELECT FROM catalog.active_attachments \
             WHERE tenant_id='t1' AND attachment_id='http-a'); \
         END $$;",
    );
    let switched_off = success(
        &url,
        &format!(
            "{} {} {}; COMMIT;",
            app_preamble(),
            prepared,
            execute_http("emergency-off", "key-emergency", "fp-emergency")
        ),
    );
    assert_eq!(switched_off.trim(), "inactive-definition|");
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id='emergency-off'); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id='emergency-off'); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.invocation_admissions \
                               WHERE client_key_digest='key-emergency'); \
         END $$;",
    );

    // Switching back on restores admission for the byte-identical request:
    // the refusal reserved no identity, so nothing has to be undone.
    success(
        &url,
        "UPDATE catalog.attachment_activation SET enabled = true \
          WHERE tenant_id='t1' AND catalog_id='c1' AND environment='dev' \
            AND attachment_id='http-a';",
    );
    let switched_on = success(
        &url,
        &format!(
            "{} {} {}; COMMIT;",
            app_preamble(),
            prepared,
            execute_http("emergency-off", "key-emergency", "fp-emergency")
        ),
    );
    assert_eq!(switched_on.trim(), "admitted|emergency-off");

    let invalid_inputs = format!(
        "{} {} \
         CREATE TEMP TABLE bad_http AS EXECUTE admit_stmt(\
           'http','c1','dev',1,'http-a','sha256:http','flow-http',1,\
           'bad-http','{{}}','{{}}','rev-test',NULL,NULL,'p','k','',\
           NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
         CREATE TEMP TABLE bad_event AS EXECUTE admit_stmt(\
           'event','c1','dev',1,NULL,NULL,'flow-event',1,\
           'bad-event','{{}}','{{}}','rev-test',NULL,NULL,NULL,NULL,NULL,\
           'reg-a',43,'{}',NULL,'bad-event','bad-event',0); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM bad_http) = 'invalid-input'; \
           ASSERT (SELECT result_code FROM bad_event) = 'invalid-input'; \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
             WHERE run_id IN ('bad-http','bad-event')); \
         END $$; COMMIT;",
        app_preamble(),
        prepared,
        registration_document,
    );
    success(&url, &invalid_inputs);

    // Lineage is checked against the immediate source under tenant RLS. A
    // changed root/depth, a foreign-tenant source, an over-budget chain, or a
    // payload/context attempt to forge causation cannot create a run.
    success(
        &url,
        "INSERT INTO wamn_run.runs \
           (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version,environment, \
            execution_bundle_hash,status,trigger_source) \
         VALUES ('t2','foreign-source','flow-http',1,'c1',1,'dev', \
           'sha256:' || encode(sha256(convert_to('{}','UTF8')), 'hex'),'completed','http');",
    );
    let forged_input = execute_event(
        "event-forged",
        &registration_document,
        &registration_digest,
        52,
    )
    .replace(
        "{\"event\":52}",
        "{\"event\":52,\"causation\":{\"run\":\"forged\"}}",
    );
    for (execution, expected) in [
        (
            execute_event_lineage(
                "event-bad-root",
                &registration_document,
                &registration_digest,
                48,
                "event-child",
                "wrong-root",
                2,
            ),
            "invalid-event-lineage|",
        ),
        (
            execute_event_lineage(
                "event-bad-depth",
                &registration_document,
                &registration_digest,
                49,
                "event-child",
                "http-1",
                3,
            ),
            "invalid-event-lineage|",
        ),
        (
            execute_event_lineage(
                "event-foreign",
                &registration_document,
                &registration_digest,
                50,
                "foreign-source",
                "foreign-source",
                1,
            ),
            "invalid-event-lineage|",
        ),
        (
            execute_event_lineage(
                "event-over-depth",
                &registration_document,
                &registration_digest,
                51,
                "event-child",
                "http-1",
                17,
            ),
            "invalid-input|",
        ),
        (forged_input, "invalid-event-lineage|"),
    ] {
        let result = success(
            &url,
            &format!("{} {} {}; COMMIT;", app_preamble(), prepared, execution),
        );
        assert_eq!(result.trim(), expected, "{execution}");
    }
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id IN (\
             'event-bad-root','event-bad-depth','event-foreign',\
             'event-over-depth','event-forged')); \
         END $$;",
    );

    // Retry can recover the exact immutable record, but cannot alter its
    // lineage. Direct post-admission mutation is rejected by the DDL guard.
    let duplicate = success(
        &url,
        &format!(
            "{} {} {}; COMMIT;",
            app_preamble(),
            prepared,
            execute_event_lineage(
                "event-child-retry",
                &registration_document,
                &registration_digest,
                46,
                "http-1",
                "http-1",
                1,
            )
        ),
    );
    assert_eq!(duplicate.trim(), "duplicate|event-child");
    let changed = success(
        &url,
        &format!(
            "{} {} {}; COMMIT;",
            app_preamble(),
            prepared,
            execute_event_lineage(
                "event-child-retry",
                &registration_document,
                &registration_digest,
                46,
                "event-1",
                "event-1",
                1,
            )
        ),
    );
    assert_eq!(changed.trim(), "conflicting-run-identity|event-child");
    let mutation = psql(
        &url,
        &format!(
            "{} UPDATE wamn_run.runs SET event_depth=9 WHERE run_id='event-child'; COMMIT;",
            app_preamble()
        ),
    );
    assert!(!mutation.status.success(), "lineage mutation must fail");

    // A failure at every run, queue, and HTTP-ledger seam rolls back every
    // preceding CTE. The queue fault is HTTP-specific so it proves that both
    // its earlier ledger reservation and run insert are atomic with enqueue.
    for (name, target, execution, run_id) in [
        (
            "run",
            "wamn_run.runs",
            execute_event(
                "event-run-fault",
                &registration_document,
                &registration_digest,
                54,
            ),
            "event-run-fault",
        ),
        (
            "queue",
            "wamn_run.run_queue",
            execute_http("http-queue-fault", "key-queue-fault", "fp-queue-fault"),
            "http-queue-fault",
        ),
        (
            "ledger",
            "wamn_run.invocation_admissions",
            execute_http("http-fault", "key-fault", "fp-fault"),
            "http-fault",
        ),
    ] {
        let trigger = format!(
            "CREATE FUNCTION pg_temp.reject_admission() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RAISE EXCEPTION 'injected-{name}-fault'; END $$; \
             CREATE TRIGGER reject_admission BEFORE INSERT ON {target} \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_admission();"
        );
        let output = psql(
            &url,
            &format!(
                "BEGIN; {} {} SET LOCAL ROLE wamn_app; \
                 SET LOCAL app.tenant = 't1'; {}; COMMIT;",
                trigger, prepared, execution
            ),
        );
        assert!(
            !output.status.success(),
            "{name} fault must abort admission"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&format!("injected-{name}-fault")),
            "{name} fault must reach the injected admission trigger; stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        success(
            &url,
            &format!(
                "DO $$ BEGIN \
                   ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id='{}'); \
                   ASSERT NOT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id='{}'); \
                   ASSERT NOT EXISTS (SELECT FROM wamn_run.invocation_admissions WHERE run_id='{}'); \
                 END $$;",
                run_id, run_id, run_id,
            ),
        );
    }

    // Two absent HTTP identities race. The unique ledger reservation chooses
    // the winner before run creation, so the loser cannot leave an orphan run.
    let mut racers = Vec::new();
    for run_id in ["race-a", "race-b"] {
        let race_url = url.clone();
        let race_sql = admit.clone();
        racers.push(thread::spawn(move || {
            success(
                &race_url,
                &format!(
                    "{} {} {}; COMMIT;",
                    app_preamble(),
                    prepare(&race_sql),
                    execute_http(run_id, "race-key", "race-fingerprint")
                ),
            )
        }));
    }
    let results: Vec<String> = racers
        .into_iter()
        .map(|racer| racer.join().expect("admission racer"))
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result.contains("admitted"))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.contains("duplicate"))
            .count(),
        1
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM wamn_run.invocation_admissions \
                   WHERE client_key_digest='race-key') = 1, 'one race ledger'; \
           ASSERT (SELECT count(*) FROM wamn_run.runs \
                   WHERE run_id IN ('race-a','race-b')) = 1, 'one race run'; \
           ASSERT (SELECT count(*) FROM wamn_run.run_queue \
                   WHERE run_id IN ('race-a','race-b')) = 1, 'one race queue'; \
         END $$;",
    );

    // -----------------------------------------------------------------------
    // Private management admission (wamn-0h0g.8.20). A draft run and a test
    // case cross a durable CONTROL reservation into this SEPARATE project
    // transaction, so a crash between the two commits must never mint a second
    // run. The grants below are the exact narrow authority the private
    // `wamn_management_admitter` role needs; issuing them from provisioning is
    // deferred, and deploy/sql/run-state.sql deliberately still names no
    // management role.
    success(
        &url,
        "GRANT USAGE ON SCHEMA wamn_run TO wamn_management_admitter; \
         GRANT USAGE ON SCHEMA catalog TO wamn_management_admitter; \
         GRANT EXECUTE ON FUNCTION wamn_run.lock_catalog_head(text, text, text) \
           TO wamn_management_admitter; \
         GRANT SELECT ON catalog.catalog_heads, catalog.release_flows, \
           catalog.flow_artifacts, catalog.execution_bundles TO wamn_management_admitter; \
         GRANT SELECT ON wamn_run.environment_policies TO wamn_management_admitter; \
         GRANT SELECT (tenant_id, run_id, idempotency_key, trigger_source, capture_mode, \
           flow_id, flow_version, catalog_id, catalog_version, environment, \
           execution_bundle_hash, input_json) ON wamn_run.runs TO wamn_management_admitter; \
         GRANT INSERT (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
           environment, execution_bundle_hash, status, trigger_source, capture_mode, input_json, \
           invocation_context, admission_context_version, platform_revision, idempotency_key, \
           run_deadline_at) ON wamn_run.runs TO wamn_management_admitter; \
         GRANT SELECT (tenant_id, run_id), INSERT (tenant_id, run_id, available_at, stream_seq) \
           ON wamn_run.run_queue TO wamn_management_admitter;",
    );

    let management = admission_transaction(AdmissionTransition::ManagementRun { schema: &schema });
    let management_admit = management.admit().to_string();
    let prepared_management = prepare_management(&management_admit);
    let draft_coordinate = ManagementProducerKey::DraftRun {
        command_id: "cmd-1",
    };
    let case_coordinate = ManagementProducerKey::TestCase {
        report_id: "report-a",
        ordinal: 0,
    };
    let draft_key = draft_coordinate.idempotency_key();
    let case_key = case_coordinate.idempotency_key();

    // A first admission under each coordinate creates exactly one ordinary run
    // and one unleased queue row, with the capture value the producer receives
    // rather than presents.
    for (execution, expected) in [
        (
            execute_draft("draft-run-1", "cmd-1", "{\"draft\":1}"),
            "admitted|draft-run-1",
        ),
        (
            execute_case("case-run-0", "report-a", 0, "{\"case-id\":\"first\"}"),
            "admitted|case-run-0",
        ),
    ] {
        let result = success(
            &url,
            &format!(
                "{} {} {}; COMMIT;",
                management_preamble(),
                prepared_management,
                execution
            ),
        );
        assert_eq!(result.trim(), expected);
    }
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               ASSERT (SELECT trigger_source FROM wamn_run.runs WHERE run_id='draft-run-1') \
                        = 'scenario-draft'; \
               ASSERT (SELECT capture_mode FROM wamn_run.runs WHERE run_id='draft-run-1') \
                        = 'full'; \
               ASSERT (SELECT idempotency_key FROM wamn_run.runs WHERE run_id='draft-run-1') \
                        = '{draft_key}'; \
               ASSERT (SELECT invocation_context #>> '{{source,producer}}' FROM wamn_run.runs \
                        WHERE run_id='draft-run-1') = 'draft-scenario'; \
               ASSERT (SELECT status FROM wamn_run.runs WHERE run_id='draft-run-1') \
                        = 'dispatched'; \
               ASSERT (SELECT run_deadline_at IS NOT NULL FROM wamn_run.runs \
                        WHERE run_id='draft-run-1'); \
               ASSERT (SELECT execution_bundle_hash FROM wamn_run.runs \
                        WHERE run_id='draft-run-1') \
                        = (SELECT execution_bundle_hash FROM catalog.release_flows \
                            WHERE tenant_id='t1' AND catalog_id='c1' AND flow_id='flow-http'); \
               ASSERT (SELECT lease_owner FROM wamn_run.run_queue WHERE run_id='draft-run-1') \
                        IS NULL; \
               ASSERT (SELECT stream_seq FROM wamn_run.run_queue WHERE run_id='draft-run-1') = 0; \
               ASSERT (SELECT trigger_source FROM wamn_run.runs WHERE run_id='case-run-0') \
                        = 'test-case'; \
               ASSERT (SELECT capture_mode FROM wamn_run.runs WHERE run_id='case-run-0') = 'off'; \
               ASSERT (SELECT idempotency_key FROM wamn_run.runs WHERE run_id='case-run-0') \
                        = '{case_key}'; \
               ASSERT (SELECT invocation_context #>> '{{source,producer}}' FROM wamn_run.runs \
                        WHERE run_id='case-run-0') IS NULL; \
               ASSERT (SELECT count(*) FROM wamn_run.invocation_admissions \
                        WHERE run_id IN ('draft-run-1','case-run-0')) = 0; \
             END $$;"
        ),
    );

    // The crash-recovery leg: the control reservation records the run id BEFORE
    // this transaction, so a crash after the project commit re-drives the
    // identical admission. It returns the SAME run without a second row, and
    // the coordinate alone recovers the run id the producer never recorded.
    for (execution, expected) in [
        (
            execute_draft("draft-run-1", "cmd-1", "{\"draft\":1}"),
            "duplicate|draft-run-1",
        ),
        (
            execute_case("case-run-0", "report-a", 0, "{\"case-id\":\"first\"}"),
            "duplicate|case-run-0",
        ),
    ] {
        let result = success(
            &url,
            &format!(
                "{} {} {}; COMMIT;",
                management_preamble(),
                prepared_management,
                execution
            ),
        );
        assert_eq!(result.trim(), expected);
    }
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               ASSERT (SELECT count(*) FROM wamn_run.runs \
                        WHERE run_id IN ('draft-run-1','case-run-0')) = 2; \
               ASSERT (SELECT count(*) FROM wamn_run.run_queue \
                        WHERE run_id IN ('draft-run-1','case-run-0')) = 2; \
               ASSERT (SELECT run_id FROM wamn_run.runs \
                        WHERE tenant_id='t1' AND idempotency_key='{draft_key}') = 'draft-run-1'; \
               ASSERT (SELECT run_id FROM wamn_run.runs \
                        WHERE tenant_id='t1' AND idempotency_key='{case_key}') = 'case-run-0'; \
             END $$;"
        ),
    );

    // The same coordinate with any different admitted fact refuses ATOMICALLY
    // and names the run already holding it. Coordinate and run id move
    // together, so pointing either half somewhere else is also a divergence.
    for (execution, expected) in [
        (
            execute_draft("draft-run-1", "cmd-1", "{\"draft\":2}"),
            "conflicting-run-identity|draft-run-1",
        ),
        (
            execute_draft("draft-run-2", "cmd-1", "{\"draft\":1}"),
            "conflicting-run-identity|draft-run-1",
        ),
        (
            execute_case("case-run-9", "report-a", 0, "{\"case-id\":\"first\"}"),
            "conflicting-run-identity|case-run-0",
        ),
        (
            execute_case("case-run-0", "report-a", 1, "{\"case-id\":\"first\"}"),
            "conflicting-run-identity|case-run-0",
        ),
    ] {
        let result = success(
            &url,
            &format!(
                "{} {} {}; COMMIT;",
                management_preamble(),
                prepared_management,
                execution
            ),
        );
        assert_eq!(result.trim(), expected, "{execution}");
    }
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
             WHERE run_id IN ('draft-run-2','case-run-9')); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.run_queue \
             WHERE run_id IN ('draft-run-2','case-run-9')); \
           ASSERT (SELECT input_json FROM wamn_run.runs WHERE run_id='draft-run-1') \
                    = '{\"draft\":1}'::jsonb; \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
             WHERE idempotency_key = 'case:report-a:1'); \
         END $$;",
    );

    // Every non-draft management admission stays capture-off: the draft marker
    // cannot be presented, a callable producer cannot enter here, an absent
    // coordinate refuses, and a stale head refuses.
    let management_negatives = format!(
        "{} {} \
         CREATE TEMP TABLE forged_marker AS {}; \
         CREATE TEMP TABLE wrong_producer AS EXECUTE admit_management(\
           'http','c1','dev',1,'flow-http',1,'mgmt-http','{{}}','{{}}','rev-test',\
           now()+interval '1 minute',NULL,NULL,NULL); \
         CREATE TEMP TABLE missing_coordinate AS EXECUTE admit_management(\
           'test-case','c1','dev',1,'flow-http',1,'mgmt-nokey','{{}}','{{}}','rev-test',\
           now()+interval '1 minute',NULL,NULL,NULL); \
         CREATE TEMP TABLE stale_head AS EXECUTE admit_management(\
           'draft-run','c1','dev',99,'flow-http',1,'mgmt-stale','{{}}','{{}}','rev-test',\
           now()+interval '1 minute','cmd-stale',NULL,NULL); \
         CREATE TEMP TABLE no_deadline AS EXECUTE admit_management(\
           'draft-run','c1','dev',1,'flow-http',1,'mgmt-forever','{{}}','{{}}','rev-test',\
           NULL,'cmd-forever',NULL,NULL); \
         DO $$ BEGIN \
           ASSERT (SELECT result_code FROM forged_marker) = 'invalid-input'; \
           ASSERT (SELECT result_code FROM wrong_producer) = 'invalid-producer'; \
           ASSERT (SELECT result_code FROM missing_coordinate) = 'invalid-input'; \
           ASSERT (SELECT result_code FROM stale_head) = 'head-drift'; \
           ASSERT (SELECT result_code FROM no_deadline) = 'invalid-input'; \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id IN (\
             'case-forged','mgmt-http','mgmt-nokey','mgmt-stale','mgmt-forever')); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE capture_mode='full' \
             AND trigger_source <> 'scenario-draft'); \
         END $$; COMMIT;",
        management_preamble(),
        prepared_management,
        execute_case(
            "case-forged",
            "report-b",
            0,
            "{\"producer\":\"draft-scenario\"}"
        ),
    );
    success(&url, &management_negatives);

    // `wamn_app` drives run state but must never author the admission-owned
    // capture carrier. The management statement NAMES `capture_mode`, and
    // PostgreSQL checks column privileges per named column, so the
    // guest-visible role is refused before any row is considered.
    let denied = psql(
        &url,
        &format!(
            "{} {} {}; COMMIT;",
            app_preamble(),
            prepared_management,
            execute_case("app-denied", "report-c", 0, "{}")
        ),
    );
    assert!(
        !denied.status.success(),
        "wamn_app must not admit a management run"
    );
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("permission denied"),
        "the refusal must be a privilege refusal; stderr:\n{}",
        String::from_utf8_lossy(&denied.stderr)
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id='app-denied'); \
         END $$;",
    );

    // A queue fault rolls the run insert back with it: a management admission
    // is one atomic run-plus-queue fact or none, and the coordinate stays free.
    let management_fault = psql(
        &url,
        &format!(
            "BEGIN; \
             CREATE FUNCTION pg_temp.reject_management() RETURNS trigger LANGUAGE plpgsql AS \
             $fault$ BEGIN RAISE EXCEPTION 'injected-management-fault'; END $fault$; \
             CREATE TRIGGER reject_management BEFORE INSERT ON wamn_run.run_queue \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_management(); \
             {} SET LOCAL ROLE wamn_management_admitter; SET LOCAL app.tenant = 't1'; \
             {}; COMMIT;",
            prepared_management,
            execute_draft("draft-fault", "cmd-fault", "{\"draft\":3}")
        ),
    );
    assert!(
        !management_fault.status.success(),
        "a queue fault must abort management admission"
    );
    assert!(
        String::from_utf8_lossy(&management_fault.stderr).contains("injected-management-fault"),
        "the fault must reach the injected trigger; stderr:\n{}",
        String::from_utf8_lossy(&management_fault.stderr)
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id='draft-fault'); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id='draft-fault'); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
                               WHERE idempotency_key='draft:cmd-fault'); \
         END $$;",
    );

    // Two identical management admissions race with no row lock between them.
    // The run key and the producer key's partial unique index choose the
    // winner; the loser conflicts out of the insert and reports `duplicate`
    // with the committed run id when it observed the row and with none when it
    // raced. Either way one coordinate names exactly one run and queue row.
    let mut management_racers = Vec::new();
    for _ in 0..2 {
        let race_url = url.clone();
        let race_sql = management_admit.clone();
        management_racers.push(thread::spawn(move || {
            success(
                &race_url,
                &format!(
                    "{} {} {}; COMMIT;",
                    management_preamble(),
                    prepare_management(&race_sql),
                    execute_case("case-race", "report-race", 0, "{\"case-id\":\"race\"}")
                ),
            )
        }));
    }
    let management_results: Vec<String> = management_racers
        .into_iter()
        .map(|racer| racer.join().expect("management admission racer"))
        .collect();
    assert_eq!(
        management_results
            .iter()
            .filter(|result| result.trim() == "admitted|case-race")
            .count(),
        1,
        "{management_results:?}"
    );
    assert_eq!(
        management_results
            .iter()
            .filter(|result| result.trim().starts_with("duplicate|"))
            .count(),
        1,
        "{management_results:?}"
    );
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM wamn_run.runs WHERE run_id='case-race') = 1; \
           ASSERT (SELECT count(*) FROM wamn_run.run_queue WHERE run_id='case-race') = 1; \
           ASSERT (SELECT count(*) FROM wamn_run.runs \
                    WHERE idempotency_key='case:report-race:0') = 1; \
         END $$;",
    );

    // Publication takes FOR UPDATE on the same stable head. Admission blocks
    // behind it, then rechecks the committed version and refuses the stale
    // candidate without a partial run.
    success(
        &url,
        "INSERT INTO catalog.catalogs \
           (tenant_id,catalog_id,version,environment,schema_version,state) \
         VALUES ('t1','c1',2,'dev','0.1','staged');",
    );
    let publisher_url = url.clone();
    let publisher = thread::spawn(move || {
        success(
            &publisher_url,
            "BEGIN; SET LOCAL application_name='cf-admit-publisher'; \
             SELECT applied_catalog_version FROM catalog.catalog_heads \
              WHERE tenant_id='t1' AND catalog_id='c1' AND environment='dev' FOR UPDATE; \
             UPDATE catalog.catalog_heads SET applied_catalog_version=2 \
              WHERE tenant_id='t1' AND catalog_id='c1' AND environment='dev'; \
             SELECT pg_sleep(1); COMMIT;",
        )
    });
    let mut publisher_holds_head = false;
    for _ in 0..40 {
        let observed = success(
            &url,
            "SELECT count(*) FROM pg_stat_activity \
              WHERE pid <> pg_backend_pid() AND wait_event = 'PgSleep' \
                AND application_name = 'cf-admit-publisher'",
        );
        if observed.trim() == "1" {
            publisher_holds_head = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(publisher_holds_head, "publisher did not reach locked sleep");
    let started = Instant::now();
    let drifted = success(
        &url,
        &format!(
            "{} PREPARE lock_head (text,text) AS {}; \
             CREATE TEMP TABLE locked_head AS EXECUTE lock_head('c1','dev'); \
             TABLE locked_head; {} {}; COMMIT;",
            app_preamble(),
            recipe.lock_head(),
            prepared,
            execute_http("promotion-race", "key-promotion", "fp-promotion")
        ),
    );
    assert_eq!(drifted.trim(), "2\nhead-drift|");
    assert!(started.elapsed() >= Duration::from_millis(700));
    publisher.join().expect("publisher");
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
             WHERE run_id='promotion-race'), 'promotion wrote no run'; \
           ASSERT (SELECT execution_bundle_hash FROM wamn_run.runs WHERE run_id='http-1') \
             = 'sha256:' || encode(sha256(convert_to('{}','UTF8')), 'hex'), \
             'head movement changed the admitted run bundle'; \
         END $$;",
    );

    // The publication above left the head on a version this environment has no
    // release for, so every management drift arm is now armed. A management
    // producer replays the parameters its control reservation froze, so this is
    // the state a crashed producer wakes into (wamn-0h0g.15.160): its exact
    // retry still returns the run it already has, and its coordinate alone
    // still recovers the run id it never recorded. New work is unaffected —
    // the drift arms still refuse it, from behind the identity arms.
    for (execution, expected) in [
        (
            execute_draft("draft-run-1", "cmd-1", "{\"draft\":1}"),
            "duplicate|draft-run-1",
        ),
        (
            execute_draft("draft-run-3", "cmd-1", "{\"draft\":1}"),
            "conflicting-run-identity|draft-run-1",
        ),
        (
            execute_draft_at("draft-after-publish", "cmd-after-publish", 1),
            "head-drift|",
        ),
        (
            execute_draft_at("draft-unreleased", "cmd-unreleased", 2),
            "definition-drift|",
        ),
    ] {
        let result = success(
            &url,
            &format!(
                "{} {} {}; COMMIT;",
                management_preamble(),
                prepared_management,
                execution
            ),
        );
        assert_eq!(result.trim(), expected, "{execution}");
    }
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT applied_catalog_version FROM catalog.catalog_heads \
                    WHERE tenant_id='t1' AND catalog_id='c1' AND environment='dev') = 2, \
                  'the retries above ran against a moved head'; \
           ASSERT NOT EXISTS (SELECT FROM catalog.release_flows \
                    WHERE tenant_id='t1' AND catalog_id='c1' AND catalog_version=2); \
           ASSERT (SELECT count(*) FROM wamn_run.runs \
                    WHERE tenant_id='t1' AND idempotency_key='draft:cmd-1') = 1; \
           ASSERT (SELECT catalog_version FROM wamn_run.runs WHERE run_id='draft-run-1') = 1; \
           ASSERT (SELECT input_json FROM wamn_run.runs WHERE run_id='draft-run-1') \
                    = '{\"draft\":1}'::jsonb; \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id IN (\
             'draft-run-3','draft-after-publish','draft-unreleased')); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id IN (\
             'draft-run-3','draft-after-publish','draft-unreleased')); \
         END $$;",
    );
}
