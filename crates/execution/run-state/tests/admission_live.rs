//! Ignored live gate for the callable-flow admission transaction.

use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use wamn_run_state::admission::{admission_sql, registration_evidence};
use wamn_run_state::queue::claim_partition_head_sql;

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
    "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL app.tenant = 't1';"
}

fn prepare(sql: &str) -> String {
    format!(
        "PREPARE admit_stmt \
         (text,text,text,int,text,text,text,int,text,text,text,text, \
          timestamptz,timestamptz,text,text,text,timestamptz,text,bigint, \
          text,bigint,text,text,text,text,int,text,text) AS {sql};"
    )
}

fn ordering(partition_key: Option<&str>, partition_policy: &str) -> String {
    format!(
        "{},'{partition_policy}'",
        partition_key.map_or_else(|| "NULL".to_string(), |key| format!("'{key}'"))
    )
}

fn execute_http_ordered(
    run_id: &str,
    key: &str,
    fingerprint: &str,
    partition_key: Option<&str>,
    partition_policy: &str,
) -> String {
    format!(
        "EXECUTE admit_stmt(\
         'http','c1','dev',1,'http-a','sha256:http','flow-http',1,\
         '{run_id}','{{\"request\":1}}','{{\"request-id\":\"req-1\"}}','rev-test',\
         now()+interval '30 seconds',now()+interval '1 minute',\
         'principal','{key}','{fingerprint}',now()+interval '1 day',\
         'inline-1',30000,NULL,NULL,NULL,NULL,NULL,NULL,NULL,{})",
        ordering(partition_key, partition_policy)
    )
}

fn execute_http(run_id: &str, key: &str, fingerprint: &str) -> String {
    execute_http_ordered(run_id, key, fingerprint, None, "blocking")
}

fn execute_event_ordered(
    run_id: &str,
    document: &str,
    hash: &str,
    seq: i64,
    partition_key: Option<&str>,
    partition_policy: &str,
) -> String {
    execute_event_lineage_ordered(
        run_id,
        document,
        hash,
        seq,
        run_id,
        run_id,
        0,
        partition_key,
        partition_policy,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_event_lineage_ordered(
    run_id: &str,
    document: &str,
    hash: &str,
    seq: i64,
    source_run_id: &str,
    root_run_id: &str,
    depth: i32,
    partition_key: Option<&str>,
    partition_policy: &str,
) -> String {
    format!(
        "EXECUTE admit_stmt(\
         'event','c1','dev',1,NULL,NULL,'flow-event',1,\
         '{run_id}','{{\"event\":{seq}}}','{{\"event-seq\":{seq}}}','rev-test',\
         NULL,now()+interval '1 minute',NULL,NULL,NULL,NULL,NULL,NULL,\
         'reg-a',{seq},'{document}','{hash}',\
         '{source_run_id}','{root_run_id}',{depth},{})",
        ordering(partition_key, partition_policy)
    )
}

fn execute_event(run_id: &str, document: &str, hash: &str, seq: i64) -> String {
    execute_event_ordered(run_id, document, hash, seq, None, "blocking")
}

#[test]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
fn admission_live() {
    let url = std::env::var("WAMN_RUN_STORE_PG_URL")
        .expect("set WAMN_RUN_STORE_PG_URL to the throwaway superuser database");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let mut catalog = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .expect("read catalog DDL");
    // The deadbc8 baseline ends mid-policy at `current_setting('app.t`.
    // Complete only that already-started DDL in this throwaway fixture; the
    // tracked canonical-file repair is outside this admission bead.
    if catalog.trim_end().ends_with("current_setting('app.t") {
        let trimmed_len = catalog.trim_end().len();
        catalog.truncate(trimmed_len);
        catalog.push_str(
            "enant', true), '')) \
             WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), '')); \
             GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.event_registrations TO wamn_app; \
             CREATE INDEX event_registrations_by_entity ON catalog.event_registrations \
               (tenant_id, catalog_id, entity_id);",
        );
    }
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
             VALUES ('t1','c1',1,'dev','0.1','applied'); \
             INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests) VALUES \
               ('t1','flow-http',1,'0.1','{{}}','gh-http','ah-http','[]','ih-http','[]'), \
               ('t1','flow-event',1,'0.1','{{}}','gh-event','ah-event','[]','ih-event','[]'); \
             INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) VALUES \
               ('t1','c1',1,'[\
                 {{\"flow-id\":\"flow-http\",\"flow-version\":1,\"artifact-hash\":\"ah-http\"}},\
                 {{\"flow-id\":\"flow-event\",\"flow-version\":1,\"artifact-hash\":\"ah-event\"}}\
               ]'); \
             INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version) VALUES \
               ('t1','c1',1,'flow-http',1),('t1','c1',1,'flow-event',1); \
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

    let recipe = admission_sql();
    let admit = recipe.admit().to_string();
    let prepared = prepare(&admit);

    // Both producer variants use the same statement, while their initial queue
    // state stays producer-shaped.
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
               ASSERT (SELECT lease_owner FROM wamn_run.run_queue WHERE run_id='http-1') = 'inline-1'; \
               ASSERT (SELECT lease_generation FROM wamn_run.run_queue WHERE run_id='http-1') = 1; \
               ASSERT (SELECT count(*) FROM wamn_run.invocation_admissions WHERE run_id='http-1') = 1; \
               ASSERT (SELECT lease_owner FROM wamn_run.run_queue WHERE run_id='event-1') IS NULL; \
               ASSERT (SELECT stream_seq FROM wamn_run.run_queue WHERE run_id='event-1') = 41; \
               ASSERT (SELECT invocation_context->'source'->>'registration-hash' FROM wamn_run.runs \
                        WHERE run_id='event-1') = '{}'; \
               ASSERT (SELECT invocation_context->>'version' FROM wamn_run.runs \
                        WHERE run_id='event-1') = '0.1'; \
               ASSERT (SELECT invocation_context->'principal'->>'artifact-digest' \
                        FROM wamn_run.runs WHERE run_id='event-1') = 'ah-event'; \
               ASSERT (SELECT invocation_context->'principal'->>'run-id' \
                        FROM wamn_run.runs WHERE run_id='event-1') = 'event-1'; \
             END $$;",
            registration_digest
        ),
    );

    // Trusted causal ancestry is stored outside both the business input and
    // invocation context. A non-event source roots at itself; subsequent event
    // runs carry the immediate parent while retaining the original root.
    for (execution, expected) in [
        (
            execute_event_lineage_ordered(
                "event-child",
                &registration_document,
                &registration_digest,
                46,
                "http-1",
                "http-1",
                1,
                None,
                "blocking",
            ),
            "admitted|event-child",
        ),
        (
            execute_event_lineage_ordered(
                "event-grandchild",
                &registration_document,
                &registration_digest,
                47,
                "event-child",
                "http-1",
                2,
                None,
                "blocking",
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

    // The centralized insert carries ordering for every producer. Unordered
    // work is explicitly null+blocking; keyed work preserves either policy.
    for (execution, expected) in [
        (
            execute_http_ordered(
                "http-ordered",
                "key-ordered",
                "fp-ordered",
                Some("account-7"),
                "blocking",
            ),
            "admitted|http-ordered",
        ),
        (
            execute_http_ordered(
                "http-blocking-head",
                "key-blocking-head",
                "fp-blocking-head",
                Some("site-strict"),
                "blocking",
            ),
            "admitted|http-blocking-head",
        ),
        (
            execute_event_ordered(
                "event-ordered",
                &registration_document,
                &registration_digest,
                44,
                Some("site-leap"),
                "leapfrog",
            ),
            "admitted|event-ordered",
        ),
        (
            execute_http_ordered(
                "http-blocking-next",
                "key-blocking-next",
                "fp-blocking-next",
                Some("site-strict"),
                "blocking",
            ),
            "admitted|http-blocking-next",
        ),
        (
            execute_event_ordered(
                "event-ordered-next",
                &registration_document,
                &registration_digest,
                45,
                Some("site-leap"),
                "leapfrog",
            ),
            "admitted|event-ordered-next",
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
           ASSERT (SELECT partition_key IS NULL AND partition_policy='blocking' \
                     FROM wamn_run.run_queue WHERE run_id='http-1'); \
           ASSERT (SELECT partition_key='account-7' AND partition_policy='blocking' \
                     FROM wamn_run.run_queue WHERE run_id='http-ordered'); \
           ASSERT (SELECT partition_key='site-strict' AND partition_policy='blocking' \
                     FROM wamn_run.run_queue WHERE run_id='http-blocking-head'); \
           ASSERT (SELECT partition_key='site-leap' AND partition_policy='leapfrog' \
                     FROM wamn_run.run_queue WHERE run_id='event-ordered'); \
         END $$;",
    );

    // The policies stamped by admission drive the real partition claim: a
    // backed-off blocking head holds its later sibling, while leapfrog yields.
    let claim = claim_partition_head_sql(10);
    success(
        &url,
        &format!(
            "{} SET LOCAL search_path=wamn_run,public; \
             UPDATE wamn_run.run_queue SET available_at=now()+interval '1 hour' \
              WHERE run_id IN ('http-blocking-head','event-ordered'); \
             INSERT INTO wamn_run.partition_owner \
               (tenant_id,partition_key,lease_owner,lease_expires_at) VALUES \
               ('t1','site-strict','ordering-probe',now()+interval '1 minute'),\
               ('t1','site-leap','ordering-probe',now()+interval '1 minute'); \
             PREPARE ordering_claim_stmt(text,bigint) AS {claim}; \
             EXECUTE ordering_claim_stmt('ordering-probe',30000); \
             DO $$ BEGIN \
               ASSERT (SELECT lease_owner FROM wamn_run.run_queue \
                 WHERE run_id='http-blocking-next') IS NULL, \
                 'blocking admission holds the later sibling'; \
               ASSERT (SELECT lease_owner FROM wamn_run.run_queue \
                 WHERE run_id='event-ordered-next') = 'ordering-probe', \
                 'leapfrog admission yields to the ready sibling'; \
             END $$; COMMIT;",
            app_preamble()
        ),
    );

    // A retry may recover the existing admission, but it cannot alter the
    // ordering already stamped on that queue row.
    for execution in [
        execute_http_ordered(
            "http-ordered-retry",
            "key-ordered",
            "fp-ordered",
            Some("changed"),
            "blocking",
        ),
        execute_http_ordered(
            "http-blocking-head-retry",
            "key-blocking-head",
            "fp-blocking-head",
            Some("changed"),
            "blocking",
        ),
        execute_event_ordered(
            "event-ordered-retry",
            &registration_document,
            &registration_digest,
            44,
            Some("changed"),
            "blocking",
        ),
    ] {
        let result = success(
            &url,
            &format!("{} {} {}; COMMIT;", app_preamble(), prepared, execution),
        );
        assert!(
            result.trim().starts_with("conflicting-run-identity|"),
            "{result}"
        );
    }
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT (SELECT partition_key='account-7' AND partition_policy='blocking' \
                     FROM wamn_run.run_queue WHERE run_id='http-ordered'); \
           ASSERT (SELECT partition_key='site-strict' AND partition_policy='blocking' \
                     FROM wamn_run.run_queue WHERE run_id='http-blocking-head'); \
           ASSERT (SELECT partition_key='site-leap' AND partition_policy='leapfrog' \
                     FROM wamn_run.run_queue WHERE run_id='event-ordered'); \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
                     WHERE run_id IN ('http-ordered-retry','http-blocking-head-retry',\
                                      'event-ordered-retry')); \
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
           now()+interval '1 day','inline-stale',30000,\
           NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'blocking'); \
         CREATE TEMP TABLE inactive AS EXECUTE admit_stmt(\
           'http','c1','dev',1,'missing','sha256:http','flow-http',1,\
           'http-inactive','{{}}','{{}}','rev-test',now()+interval '30 seconds',\
           now()+interval '1 minute','principal-inactive','key-inactive','fp-inactive',\
           now()+interval '1 day','inline-inactive',30000,\
           NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'blocking'); \
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

    let invalid_inputs = format!(
        "{} {} \
         CREATE TEMP TABLE bad_http AS EXECUTE admit_stmt(\
           'http','c1','dev',1,'http-a','sha256:http','flow-http',1,\
           'bad-http','{{}}','{{}}','rev-test',NULL,NULL,'p','k','f',now()+interval '1 day',\
           NULL,30000,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'blocking'); \
         CREATE TEMP TABLE bad_event AS EXECUTE admit_stmt(\
           'event','c1','dev',1,NULL,NULL,'flow-event',1,\
           'bad-event','{{}}','{{}}','rev-test',NULL,NULL,NULL,NULL,NULL,NULL,\
           'reg-a',43,'{}',NULL,'bad-event','bad-event',0,NULL,'blocking'); \
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
           (tenant_id,run_id,flow_id,flow_version,status,trigger_source) \
         VALUES ('t2','foreign-source','flow-http',1,'completed','http');",
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
            execute_event_lineage_ordered(
                "event-bad-root",
                &registration_document,
                &registration_digest,
                48,
                "event-child",
                "wrong-root",
                2,
                None,
                "blocking",
            ),
            "invalid-event-lineage|",
        ),
        (
            execute_event_lineage_ordered(
                "event-bad-depth",
                &registration_document,
                &registration_digest,
                49,
                "event-child",
                "http-1",
                3,
                None,
                "blocking",
            ),
            "invalid-event-lineage|",
        ),
        (
            execute_event_lineage_ordered(
                "event-foreign",
                &registration_document,
                &registration_digest,
                50,
                "foreign-source",
                "foreign-source",
                1,
                None,
                "blocking",
            ),
            "invalid-event-lineage|",
        ),
        (
            execute_event_lineage_ordered(
                "event-over-depth",
                &registration_document,
                &registration_digest,
                51,
                "event-child",
                "http-1",
                17,
                None,
                "blocking",
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
            execute_event_lineage_ordered(
                "event-child-retry",
                &registration_document,
                &registration_digest,
                46,
                "http-1",
                "http-1",
                1,
                None,
                "blocking",
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
            execute_event_lineage_ordered(
                "event-child-retry",
                &registration_document,
                &registration_digest,
                46,
                "event-1",
                "event-1",
                1,
                None,
                "blocking",
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

    for execution in [
        execute_http_ordered(
            "http-unknown-policy",
            "key-unknown-policy",
            "fp-unknown-policy",
            Some("site"),
            "unknown",
        ),
        execute_event_ordered(
            "event-unkeyed-leapfrog",
            &registration_document,
            &registration_digest,
            55,
            None,
            "leapfrog",
        ),
        execute_http_ordered(
            "http-empty-key",
            "key-empty",
            "fp-empty",
            Some(""),
            "blocking",
        ),
    ] {
        let result = success(
            &url,
            &format!("{} {} {}; COMMIT;", app_preamble(), prepared, execution),
        );
        assert_eq!(result.trim(), "invalid-input|");
    }
    success(
        &url,
        "DO $$ BEGIN \
           ASSERT NOT EXISTS (SELECT FROM wamn_run.runs WHERE run_id IN (\
             'http-unknown-policy','event-unkeyed-leapfrog','http-empty-key')); \
         END $$;",
    );

    // A failure at every run -> queue -> HTTP-ledger seam rolls back every
    // preceding CTE.
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
            execute_event_ordered(
                "event-fault",
                &registration_document,
                &registration_digest,
                53,
                Some("fault-key"),
                "leapfrog",
            ),
            "event-fault",
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
                "{} {} {} {}; COMMIT;",
                app_preamble(),
                prepared,
                trigger,
                execution
            ),
        );
        assert!(
            !output.status.success(),
            "{name} fault must abort admission"
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
                    execute_http_ordered(
                        run_id,
                        "race-key",
                        "race-fingerprint",
                        Some("race-partition"),
                        "leapfrog",
                    )
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
           ASSERT (SELECT bool_and(partition_key='race-partition' \
                     AND partition_policy='leapfrog') FROM wamn_run.run_queue \
                   WHERE run_id IN ('race-a','race-b')), 'race ordering is coherent'; \
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
        "DO $$ BEGIN ASSERT NOT EXISTS (SELECT FROM wamn_run.runs \
           WHERE run_id='promotion-race'), 'promotion wrote no run'; END $$;",
    );
}
